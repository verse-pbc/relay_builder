//! Minimal Nostr relay - the simplest possible relay that accepts all events
//!
//! This example shows the absolute minimum code needed to run a functional Nostr relay.
//! It accepts all events without any filtering or authentication.
//!
//! Run with: cargo run --example 01_minimal_relay --features axum
//!
//! For subdomain isolation testing, add to /etc/hosts:
//!   127.0.0.1 example.local
//!   127.0.0.1 test.example.local
//!   127.0.0.1 other.example.local
//!
//! Then connect with:
//!   nak req --stream ws://example.local:8080
//!   nak req --stream ws://test.example.local:8080
//! Events sent to one subdomain will only be visible to that subdomain's subscribers.

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
    // For subdomain support, use a domain like "ws://example.local:8080"
    // Then test.example.local and other.example.local will have isolated data
    let relay_url = "ws://example.local:8080";
    //let relay_url = "ws://localhost:8080"; // Use this for no subdomain support
    let db_path = "./minimal_relay_db";
    let relay_keys = Keys::generate();
    let config = RelayConfig::new(relay_url, db_path, relay_keys)
        // Enable subdomain-based isolation
        // Use 2 for *.example.local (2 parts in base domain)
        // Use 3 for *.relay.example.com (3 parts in base domain)
        .with_subdomains(2); // Enables subdomain isolation

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
