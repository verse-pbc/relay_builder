//! Authentication relay - demonstrates NIP-42 authentication
//!
//! This example shows how to enable authentication and check if users are authenticated
//! in your EventProcessor. NIP-42 allows relays to require authentication.
//!
//! Run with: cargo run --example 04_auth_relay --features axum

use anyhow::Result;
use async_trait::async_trait;
use axum::{routing::get, Router};
use nostr_relay_builder::{
    Error, EventContext, EventProcessor, RelayBuilder, RelayConfig, RelayInfo,
    Result as RelayResult, StoreCommand,
};
use nostr_sdk::prelude::*;
use std::net::SocketAddr;
use std::sync::Arc;

/// Simple processor that only accepts events from authenticated users
#[derive(Debug, Clone)]
struct AuthRequiredProcessor;

#[async_trait]
impl EventProcessor for AuthRequiredProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<parking_lot::RwLock<()>>,
        context: EventContext<'_>,
    ) -> RelayResult<Vec<StoreCommand>> {
        // Check if user is authenticated
        if let Some(pubkey) = &context.authed_pubkey {
            tracing::info!("Accepted event from authenticated user: {}", pubkey);
            Ok(vec![(event, context.subdomain.clone()).into()])
        } else {
            // Reject with auth-required message
            tracing::info!("Rejected event from unauthenticated user");
            Err(Error::restricted("authentication required"))
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Create relay configuration with auth enabled
    let relay_url = "ws://localhost:8080";
    let keys = Keys::generate();
    let mut config = RelayConfig::new(relay_url, "./auth_relay.db", keys);

    // Enable authentication - this automatically adds Nip42Middleware
    config.enable_auth = true;

    // Relay information for NIP-11
    let relay_info = RelayInfo {
        name: "Auth Required Relay".to_string(),
        description: "Only authenticated users can post".to_string(),
        pubkey: config.keys.public_key().to_string(),
        contact: "admin@example.com".to_string(),
        supported_nips: vec![1, 11, 42], // Including NIP-42
        software: "nostr_relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Build relay with auth processor
    let handler = RelayBuilder::new(config)
        .with_event_processor(AuthRequiredProcessor)
        .with_relay_info(relay_info)
        .build_axum()
        .await?;

    // Create HTTP server
    let app = Router::new().route("/", get(handler));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    println!("🚀 Auth Required Relay running at:");
    println!("📡 WebSocket: ws://localhost:8080");
    println!("🌐 Browser: http://localhost:8080");
    println!("🔐 Authentication: Required (NIP-42)");
    println!();
    println!("To post events:");
    println!("  1. Connect to the relay");
    println!("  2. Receive AUTH challenge");
    println!("  3. Send signed AUTH response");
    println!("  4. Now you can post events!");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
