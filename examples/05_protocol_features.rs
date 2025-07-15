//! Protocol features relay - demonstrates NIPs 40 and 70
//!
//! This example shows how to add protocol-level features using built-in middleware:
//! - NIP-40: Event expiration
//! - NIP-70: Protected events (require auth to read)
//!
//! Run with: cargo run --example 05_protocol_features --features axum

use anyhow::Result;
use axum::{routing::get, Router};
use nostr_relay_builder::{
    Nip40ExpirationMiddleware, Nip70Middleware, RelayBuilder, RelayConfig, RelayInfo,
};
use nostr_sdk::prelude::*;
use std::net::SocketAddr;
use tokio_util::task::TaskTracker;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Create relay configuration
    let relay_url = "ws://localhost:8080";
    let keys = Keys::generate();

    let task_tracker = TaskTracker::new();

    let config = RelayConfig::new(relay_url, "./protocol_features.db", keys);

    // Relay information for NIP-11
    let relay_info = RelayInfo {
        name: "Protocol Features Relay".to_string(),
        description: "Demonstrates NIPs 40 and 70".to_string(),
        pubkey: config.keys.public_key().to_string(),
        contact: "admin@example.com".to_string(),
        supported_nips: vec![1, 9, 40, 70, 50], // List all supported NIPs
        software: "nostr_relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Build relay with protocol middleware
    // Note: Middleware order matters! They process in the order added
    let handler = RelayBuilder::new(config)
        .with_task_tracker(task_tracker)
        // NIP-40: Event expiration
        // Filters out events with expired `expiration` tags
        .with_middleware(Nip40ExpirationMiddleware::new())
        // NIP-70: Protected events
        // Requires authentication to read events marked as protected
        .with_middleware(Nip70Middleware)
        .with_relay_info(relay_info)
        .build_axum()
        .await?;

    // Create HTTP server
    let app = Router::new().route("/", get(handler));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    println!("🚀 Protocol Features Relay running at:");
    println!("📡 WebSocket: ws://localhost:8080");
    println!("🌐 Browser: http://localhost:8080");
    println!();
    println!("Supported protocol features:");
    println!("  ⏰ NIP-40: Event expiration (auto-removes expired)");
    println!("  🔒 NIP-70: Protected events (requires auth to read)");
    println!();
    println!("Try:");
    println!("  1. Send an event with expiration tag: [\"expiration\", \"<timestamp>\"]");
    println!("  2. Send a protected event and try to read without auth");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
