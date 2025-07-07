//! User sessions relay - demonstrates per-connection state
//!
//! This example shows how to use custom state with EventProcessor<T> to track
//! data for each connected client. Here we track message counts per user.
//!
//! Run with: cargo run --example 07_user_sessions --features axum

use anyhow::Result;
use async_trait::async_trait;
use axum::{routing::get, Router};
use nostr_relay_builder::{
    EventContext, EventProcessor, RelayBuilder, RelayConfig, RelayInfo, Result as RelayResult,
    StoreCommand,
};
use nostr_sdk::prelude::*;
use std::net::SocketAddr;
use std::sync::Arc;

/// Custom state for each connection
#[derive(Debug, Clone, Default)]
struct UserSession {
    messages_sent: u32,
    first_event_time: Option<std::time::Instant>,
}

/// Processor that tracks user activity
#[derive(Debug, Clone)]
struct SessionTrackingProcessor;

#[async_trait]
impl EventProcessor<UserSession> for SessionTrackingProcessor {
    async fn handle_event(
        &self,
        event: Event,
        custom_state: Arc<parking_lot::RwLock<UserSession>>,
        context: EventContext<'_>,
    ) -> RelayResult<Vec<StoreCommand>> {
        // Get a write lock since we need to modify the session state
        let mut state = custom_state.write();

        // Track first event time
        if state.first_event_time.is_none() {
            state.first_event_time = Some(std::time::Instant::now());
            tracing::info!("New session started for pubkey: {}", event.pubkey);
        }

        // Increment message counter
        state.messages_sent += 1;

        // Log session statistics
        if state.messages_sent % 5 == 0 {
            let duration = state
                .first_event_time
                .map(|t| t.elapsed().as_secs())
                .unwrap_or(0);

            tracing::info!(
                "Session stats - Pubkey: {}, Messages: {}, Duration: {}s",
                event.pubkey,
                state.messages_sent,
                duration
            );
        }

        // Simple spam prevention based on message count
        if state.messages_sent > 100 {
            tracing::warn!("User {} exceeded 100 messages in session", event.pubkey);
            return Err(nostr_relay_builder::Error::restricted(
                "too many messages in this session",
            ));
        }

        // Accept the event
        Ok(vec![(event, context.subdomain.clone()).into()])
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Create relay configuration
    let relay_url = "ws://localhost:8080";
    let keys = Keys::generate();
    let config = RelayConfig::new(relay_url, "./user_sessions.db", keys);

    // Relay information for NIP-11
    let relay_info = RelayInfo {
        name: "Session Tracking Relay".to_string(),
        description: "Tracks per-connection statistics".to_string(),
        pubkey: config.keys.public_key().to_string(),
        contact: "admin@example.com".to_string(),
        supported_nips: vec![1, 11],
        software: "nostr_relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Build relay with custom state
    let handler = RelayBuilder::<UserSession>::new(config)
        // Enable custom state type
        .with_custom_state::<UserSession>()
        // Our processor that uses the custom state
        .with_event_processor(SessionTrackingProcessor)
        .with_relay_info(relay_info)
        .build_axum()
        .await?;

    // Create HTTP server
    let app = Router::new().route("/", get(handler));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    println!("🚀 Session Tracking Relay running at:");
    println!("📡 WebSocket: ws://localhost:8080");
    println!("🌐 Browser: http://localhost:8080");
    println!("📊 Tracking: Messages per session");
    println!();
    println!("Features demonstrated:");
    println!("  - Custom state type (UserSession)");
    println!("  - Mutable state in async contexts");
    println!("  - Per-connection data tracking");
    println!("  - Session-based spam prevention");
    println!();
    println!("Watch the logs to see session statistics!");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
