//! Minimal Nostr relay - the simplest possible relay that accepts all events
//!
//! This example shows the absolute minimum code needed to run a functional Nostr relay.
//! It accepts all events without any filtering or authentication.
//!
//! Run with: cargo run --example 01_minimal_relay --features axum

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
    let relay_url = "ws://localhost:8080";
    let db_path = "./minimal_relay_db";
    let relay_keys = Keys::generate();
    let config = RelayConfig::new(relay_url, db_path, relay_keys);

    // Create relay info for NIP-11
    let relay_info = common::create_relay_info(
        "Minimal Relay",
        "A minimal Nostr relay that accepts all events",
        config.keys.public_key(),
        vec![1, 9, 50],
    );

    // Build the relay handler factory
    let handler_factory = Arc::new(RelayBuilder::<()>::new(config.clone()).build().await?);

    // Create the root handler
    let root_handler = common::create_root_handler(handler_factory, relay_info);

    // Create HTTP server
    let app = Router::new().route("/", get(root_handler));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    common::run_relay_server(app, addr, "Minimal relay").await
}
