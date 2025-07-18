//! Configurable Nostr relay for testing
//!
//! This relay can be configured via environment variables:
//! - RELAY_PORT: Port to listen on (default: 8080)
//! - RELAY_DB_PATH: Database path (default: ./relay_db)
//! - RELAY_NAME: Relay name (default: Test Relay)

use anyhow::Result;
use axum::{routing::get, Router};
use nostr_sdk::prelude::*;
use relay_builder::{RelayBuilder, RelayConfig, RelayInfo};
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Get configuration from environment
    let port: u16 = std::env::var("RELAY_PORT")
        .unwrap_or_else(|_| "8080".to_string())
        .parse()?;
    let db_path = std::env::var("RELAY_DB_PATH").unwrap_or_else(|_| "./relay_db".to_string());
    let relay_name = std::env::var("RELAY_NAME").unwrap_or_else(|_| "Test Relay".to_string());

    // Create relay configuration
    let relay_url = format!("ws://localhost:{port}");
    let relay_keys = Keys::generate();
    let config = RelayConfig::new(relay_url.clone(), db_path.clone(), relay_keys);

    // Create relay info for NIP-11
    let relay_info = RelayInfo {
        name: relay_name.clone(),
        description: "A configurable test relay for Negentropy sync".to_string(),
        pubkey: config.keys.public_key().to_hex(),
        contact: "admin@test.relay".to_string(),
        supported_nips: vec![1, 9, 50, 77],
        software: "relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Build the relay
    let root_handler = RelayBuilder::<()>::new(config.clone())
        .with_relay_info(relay_info)
        .build_axum()
        .await?;

    // Create HTTP server
    let app = Router::new().route("/", get(root_handler));

    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    println!("🚀 {relay_name} listening on: {addr}");
    println!("📡 WebSocket endpoint: {relay_url}");
    println!("💾 Database path: {db_path}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
