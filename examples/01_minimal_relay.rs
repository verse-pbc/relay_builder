//! Minimal Nostr relay
//!
//! The simplest possible Nostr relay implementation
//!
//! Run with: cargo run --example 01_minimal_relay --features axum

// Use jemalloc for better performance (when available)
#[cfg(all(not(target_env = "musl"), feature = "jemalloc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(all(not(target_env = "musl"), feature = "jemalloc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

mod common;

use anyhow::Result;
use axum::{routing::get, Router};
use nostr_sdk::prelude::*;
use relay_builder::{RelayBuilder, RelayConfig};
use std::net::SocketAddr;
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<()> {
    common::init_logging();

    // Create relay configuration
    let relay_url = "ws://example.local:8080";
    let db_path =
        std::env::var("RELAY_DATA_DIR").unwrap_or_else(|_| "./minimal_relay_db".to_string());
    let relay_keys = Keys::generate();
    let config = RelayConfig::new(relay_url, db_path, relay_keys)
        .with_subdomains(2) // Enables subdomain isolation
        .with_diagnostics(); // Enable health check logging

    // Create relay info for NIP-11
    let relay_info = common::create_relay_info(
        "Minimal Relay",
        "A minimal Nostr relay implementation",
        config.keys.public_key(),
        vec![1, 9, 50],
    );

    // Build the relay handler factory
    let handler_factory = Arc::new(RelayBuilder::<()>::new(config.clone()).build().await?);

    // Create the root handler
    let root_handler = common::create_root_handler(handler_factory, relay_info);

    // Create HTTP server
    let app = Router::new().route("/", get(root_handler));

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));

    // Run the relay server
    common::run_relay_server(app, addr, "Minimal relay").await
}
