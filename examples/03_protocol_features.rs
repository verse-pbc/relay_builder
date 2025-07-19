//! Protocol features - demonstrates authentication and NIPs support
//!
//! This example shows multiple protocol features working together:
//! - NIP-42: Authentication (required for posting)
//! - NIP-40: Event expiration (built into EventProcessor)
//! - NIP-70: Protected events (auth required to read)
//!
//! Run with: cargo run --example 03_protocol_features

use anyhow::Result;
use axum::{
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use nostr_sdk::prelude::*;
use relay_builder::{
    Error, EventContext, EventProcessor, RelayBuilder, RelayConfig, RelayInfo,
    Result as RelayResult, StoreCommand,
};
use std::net::SocketAddr;
use std::sync::Arc;
use websocket_builder::{handle_upgrade, HandlerFactory, WebSocketUpgrade};

/// Protocol-aware event processor
#[derive(Debug, Clone)]
struct ProtocolProcessor {
    require_auth_to_post: bool,
}

impl EventProcessor for ProtocolProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<parking_lot::RwLock<()>>,
        context: EventContext<'_>,
    ) -> RelayResult<Vec<StoreCommand>> {
        // NIP-42: Check authentication for posting
        if self.require_auth_to_post && context.authed_pubkey.is_none() {
            tracing::info!("Rejected event from unauthenticated user");
            return Err(Error::restricted(
                "authentication required: please AUTH first",
            ));
        }

        // NIP-40: Check event expiration
        if let Some(expiration_tag) = event
            .tags
            .iter()
            .find(|tag| matches!(tag.kind(), TagKind::Custom(name) if name == "expiration"))
            .and_then(|tag| tag.content())
        {
            if let Ok(expiration) = expiration_tag.parse::<u64>() {
                let now = Timestamp::now().as_u64();
                if expiration < now {
                    tracing::info!(
                        "Rejected expired event {} (expired at {})",
                        event.id,
                        expiration
                    );
                    return Err(Error::restricted("event has expired"));
                }
                tracing::info!("Event {} expires at {}", event.id, expiration);
            }
        }

        // NIP-70: Note about protected events
        // Protected events are handled at the filter level, not here
        // The relay automatically requires auth to read events with "-" tags
        let is_protected = event
            .tags
            .iter()
            .any(|tag| matches!(tag.kind(), TagKind::Custom(name) if name == "-"));

        if is_protected {
            tracing::info!(
                "Storing protected event {} (requires auth to read)",
                event.id
            );
        }

        // Log who posted
        if let Some(pubkey) = &context.authed_pubkey {
            tracing::info!(
                "Accepted event {} from authenticated user: {}",
                event.id,
                pubkey
            );
        } else {
            tracing::info!("Accepted event {} from anonymous user", event.id);
        }

        // Store the event
        Ok(vec![(event, context.subdomain.clone()).into()])
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Create relay configuration with auth enabled
    let relay_url = "ws://localhost:8081";
    let keys = Keys::generate();
    let mut config = RelayConfig::new(relay_url, "./data/protocol_features", keys);

    // Enable authentication - this automatically adds Nip42Middleware
    config.enable_auth = true;

    // Relay information for NIP-11
    let relay_info = RelayInfo {
        name: "Protocol Features Relay".to_string(),
        description: "Demonstrates authentication, expiration, and protected events".to_string(),
        pubkey: config.keys.public_key().to_string(),
        contact: "admin@example.com".to_string(),
        supported_nips: vec![1, 9, 11, 40, 42, 50, 70], // All supported NIPs
        software: "relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Build relay with protocol processor
    let handler_factory = Arc::new(
        RelayBuilder::<()>::new(config)
            .event_processor(ProtocolProcessor {
                require_auth_to_post: true,
            })
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

    let addr = SocketAddr::from(([127, 0, 0, 1], 8081));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    println!("🚀 Protocol Features Relay running at:");
    println!("📡 WebSocket: ws://localhost:8081");
    println!("🌐 Browser: http://localhost:8081");
    println!();
    println!("Protocol features:");
    println!("🔐 NIP-42: Authentication required to post");
    println!("⏰ NIP-40: Event expiration support");
    println!("🔒 NIP-70: Protected events (auth required to read)");
    println!();
    println!("Try:");
    println!("1. Connect and authenticate (AUTH flow)");
    println!("2. Send event with expiration: [\"expiration\", \"<timestamp>\"]");
    println!("3. Send protected event with [\"-\"] tag");
    println!("4. Try reading protected events without auth");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
