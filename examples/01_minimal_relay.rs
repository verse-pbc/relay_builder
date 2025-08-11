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

fn main() -> Result<()> {
    // Use the optimized runtime with CPU affinity
    common::run_with_optimized_runtime(common::ServerConfig::default(), run_server)
}

async fn run_server() -> Result<()> {
    common::init_logging();

    // Create relay configuration
    // For subdomain support, use a domain like "ws://example.local:8080"
    // Then test.example.local and other.example.local will have isolated data
    let relay_url = "ws://example.local:8080";
    //let relay_url = "ws://localhost:8080"; // Use this for no subdomain support
    let db_path = "./minimal_relay_db";
    let relay_keys = Keys::generate();

    // Configure with CPU affinity for the database writer
    let cpu_count = num_cpus::get();
    let writer_cpu = cpu_count.saturating_sub(1);

    let config = RelayConfig::new(relay_url, db_path, relay_keys)
        // Enable subdomain-based isolation
        // Use 2 for *.example.local (2 parts in base domain)
        // Use 3 for *.relay.example.com (3 parts in base domain)
        .with_subdomains(2) // Enables subdomain isolation
        .with_ingester_cpu_affinity(writer_cpu) // Pin database writer to dedicated CPU
        .with_diagnostics(); // Enable health check logging

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

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));

    // Use the optimized server with SO_REUSEPORT and CPU affinity
    common::run_relay_server_optimized(app, addr, "Minimal relay", common::ServerConfig::default())
        .await
}
