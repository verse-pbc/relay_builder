//! Protocol features relay - demonstrates NIPs 09, 40, and 70
//!
//! This example shows how to add protocol-level features using built-in middleware:
//! - NIP-09: Event deletion
//! - NIP-40: Event expiration
//! - NIP-70: Protected events (require auth to read)
//!
//! Run with: cargo run --example 05_protocol_features --features axum

use anyhow::Result;
use axum::{routing::get, Router};
use nostr_relay_builder::{
    Nip09Middleware, Nip40ExpirationMiddleware, Nip70Middleware, RelayBuilder, RelayConfig,
    RelayDatabase, RelayInfo,
};
use nostr_sdk::prelude::*;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio_util::task::TaskTracker;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Create relay configuration with shared database
    let relay_url = "ws://localhost:8080";
    let keys = Keys::generate();

    // Create database (required for NIP-09)
    let task_tracker = TaskTracker::new();
    let keys_arc = Arc::new(keys.clone());
    let (database, _db_sender) =
        RelayDatabase::with_task_tracker("./protocol_features.db", keys_arc, task_tracker.clone())?;
    let database = Arc::new(database);

    let config = RelayConfig::new(relay_url, "./protocol_features.db", keys);

    // Relay information for NIP-11
    let relay_info = RelayInfo {
        name: "Protocol Features Relay".to_string(),
        description: "Demonstrates NIPs 09, 40, and 70".to_string(),
        pubkey: config.keys.public_key().to_string(),
        contact: "admin@example.com".to_string(),
        supported_nips: vec![1, 9, 11, 40, 70], // List all supported NIPs
        software: "nostr_relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Build relay with protocol middleware
    // Note: Middleware order matters! They process in the order added
    let handler = RelayBuilder::new(config)
        .with_task_tracker(task_tracker)
        // NIP-09: Event deletion
        // Processes kind:5 deletion events and removes deleted events
        .with_middleware(Nip09Middleware::new(database.clone()))
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
    println!("  📝 NIP-09: Event deletion (kind:5 events)");
    println!("  ⏰ NIP-40: Event expiration (auto-removes expired)");
    println!("  🔒 NIP-70: Protected events (requires auth to read)");
    println!();
    println!("Try:");
    println!("  1. Send an event with expiration tag: [\"expiration\", \"<timestamp>\"]");
    println!("  2. Delete an event by sending kind:5 with e-tag");
    println!("  3. Send a protected event and try to read without auth");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
