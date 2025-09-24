//! Advanced state management - custom state and multi-tenant support
//!
//! This example demonstrates:
//! - Custom per-connection state with EventProcessor<T>
//! - Subdomain-based data isolation (multi-tenancy)
//! - Tracking session data per user
//! - Different behavior based on subdomain
//!
//! Run with: cargo run --example 04_advanced_state --features axum

mod common;

use anyhow::Result;
use axum::{
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use nostr_sdk::prelude::*;
use relay_builder::{handle_upgrade, HandlerFactory, WebSocketUpgrade};
use relay_builder::{
    EventContext, EventProcessor, RelayBuilder, RelayConfig, RelayInfo, Result as RelayResult,
    StoreCommand,
};
use std::net::SocketAddr;
use std::sync::Arc;

/// Custom state for each connection
#[derive(Debug, Clone, Default)]
struct UserSession {
    messages_sent: u32,
    first_event_time: Option<std::time::Instant>,
    subdomain_info: Option<String>,
}

/// Advanced processor that uses custom state and handles multi-tenancy
#[derive(Debug, Clone)]
struct AdvancedStateProcessor {
    premium_subdomains: Vec<String>,
}

impl EventProcessor<UserSession> for AdvancedStateProcessor {
    async fn handle_event(
        &self,
        event: Event,
        custom_state: Arc<parking_lot::RwLock<UserSession>>,
        context: &EventContext,
    ) -> RelayResult<Vec<StoreCommand>> {
        let mut state = custom_state.write();

        // Initialize session if first event
        if state.first_event_time.is_none() {
            state.first_event_time = Some(std::time::Instant::now());
            state.subdomain_info = match context.subdomain.as_ref() {
                nostr_lmdb::Scope::Named { name, .. } => Some(name.clone()),
                nostr_lmdb::Scope::Default => None,
            };

            tracing::info!(
                "New session - Pubkey: {}, Subdomain: {:?}",
                event.pubkey,
                context.subdomain
            );
        }

        // Increment message counter
        state.messages_sent += 1;

        // Different limits based on subdomain
        let message_limit = match context.subdomain.as_ref() {
            nostr_lmdb::Scope::Named { name, .. } if self.premium_subdomains.contains(name) => {
                // Premium subdomains get higher limits
                500
            }
            nostr_lmdb::Scope::Named { .. } => {
                // Regular subdomains
                100
            }
            nostr_lmdb::Scope::Default => {
                // No subdomain (root domain)
                50
            }
        };

        // Check message limit
        if state.messages_sent > message_limit {
            tracing::warn!(
                "User {} exceeded message limit ({}) on subdomain {:?}",
                event.pubkey,
                message_limit,
                context.subdomain
            );
            return Err(relay_builder::Error::restricted(format!(
                "message limit {message_limit} exceeded for this session"
            )));
        }

        // Log statistics every 10 messages
        #[allow(clippy::manual_is_multiple_of)]
        if state.messages_sent % 10 == 0 {
            let duration = state
                .first_event_time
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0);

            tracing::info!(
                "Session stats - Subdomain: {:?}, Pubkey: {}, Messages: {}/{}, Duration: {}s",
                context.subdomain,
                event.pubkey,
                state.messages_sent,
                message_limit,
                duration
            );
        }

        // Special handling for certain subdomains
        if let nostr_lmdb::Scope::Named {
            name: subdomain, ..
        } = context.subdomain.as_ref()
        {
            match subdomain.as_str() {
                "vip" | "premium" => {
                    tracing::info!("VIP event from {}", event.pubkey);
                    // Could add special processing for VIP subdomain
                }
                "test" => {
                    // Test subdomain might have different validation
                    if event.content.len() > 1000 {
                        return Err(relay_builder::Error::restricted(
                            "test subdomain has 1000 char limit",
                        ));
                    }
                }
                _ => {}
            }
        }

        // Accept the event - it will be automatically isolated by subdomain
        Ok(vec![(event, (*context.subdomain).clone()).into()])
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    common::init_logging();

    // Create relay configuration with subdomain support
    let relay_url = "ws://example.local:8080";
    let keys = Keys::generate();
    let config = RelayConfig::new(relay_url, "./data/advanced_state", keys)
        // Enable subdomain-based isolation
        .with_subdomains_from_url(relay_url)
        .with_max_connections(1000);

    // Relay information for NIP-11
    let relay_info = RelayInfo {
        name: "Advanced State Relay".to_string(),
        description: "Multi-tenant relay with session tracking".to_string(),
        pubkey: config.keys.public_key().to_string(),
        contact: "admin@example.local".to_string(),
        supported_nips: vec![1, 9, 11, 50],
        software: "relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Create processor with premium subdomain list
    let processor = AdvancedStateProcessor {
        premium_subdomains: vec!["vip".to_string(), "premium".to_string()],
    };

    // Build relay with custom state type
    let handler_factory = Arc::new(
        RelayBuilder::<UserSession>::new(config)
            // Enable custom state type
            .custom_state::<UserSession>()
            // Our processor that uses both custom state and subdomains
            .event_processor(processor)
            .build()
            .await?,
    );

    // Create a unified handler that supports both WebSocket and HTTP on the same route
    let root_handler = {
        let relay_info_clone = relay_info.clone();
        move |ws: Option<WebSocketUpgrade>,
              axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
              headers: axum::http::HeaderMap| {
            let handler_factory = handler_factory.clone();
            let relay_info = relay_info_clone.clone();

            async move {
                match ws {
                    Some(ws) => {
                        // Handle WebSocket upgrade
                        let handler = handler_factory.create(&headers);
                        handle_upgrade(ws, addr, handler).await
                    }
                    None => {
                        // Check for NIP-11 JSON request
                        if let Some(accept) = headers.get(axum::http::header::ACCEPT) {
                            if let Ok(value) = accept.to_str() {
                                if value == "application/nostr+json" {
                                    return axum::Json(&relay_info).into_response();
                                }
                            }
                        }

                        // Serve HTML info page
                        Html(relay_builder::handlers::default_relay_html(&relay_info))
                            .into_response()
                    }
                }
            }
        }
    };

    // Create HTTP server
    let app = Router::new().route("/", get(root_handler));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    println!("🚀 Advanced State Relay running at:");
    println!("📡 WebSocket: ws://localhost:8080");
    println!("🌐 Browser: http://localhost:8080");
    println!();
    println!("Features:");
    println!("📊 Per-connection session tracking");
    println!("🏢 Multi-tenant with subdomain isolation");
    println!("⚡ Different limits per subdomain:");
    println!("   - Root domain: 50 messages/session");
    println!("   - Regular subdomains: 100 messages/session");
    println!("   - Premium subdomains (vip, premium): 500 messages/session");
    println!();
    println!("Testing multi-tenancy locally:");
    println!("1. Add to /etc/hosts:");
    println!("   127.0.0.1 example.local");
    println!("   127.0.0.1 test.example.local");
    println!("   127.0.0.1 vip.example.local");
    println!("2. Connect to different subdomains:");
    println!("   - ws://example.local:8080 (root)");
    println!("   - ws://test.example.local:8080 (test subdomain)");
    println!("   - ws://vip.example.local:8080 (premium subdomain)");
    println!();
    println!("Watch the logs to see session tracking and isolation!");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
