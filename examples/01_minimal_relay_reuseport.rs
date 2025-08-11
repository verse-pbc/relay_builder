//! Minimal Nostr relay - SO_REUSEPORT version
//!
//! This uses SO_REUSEPORT with multiple listeners and CPU affinity
//!
//! Run with: cargo run --example 01_minimal_relay_reuseport --features axum

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
    let relay_url = "ws://example.local:8080";
    let db_path = "./minimal_relay_db";
    let relay_keys = Keys::generate();

    // Configure with CPU affinity for the database writer
    let cpu_count = num_cpus::get();
    let writer_cpu = cpu_count.saturating_sub(1);

    let config = RelayConfig::new(relay_url, db_path, relay_keys)
        .with_subdomains(2) // Enables subdomain isolation
        .with_ingester_cpu_affinity(writer_cpu) // Pin database writer to dedicated CPU
        .with_diagnostics(); // Enable health check logging

    // Create relay info for NIP-11
    let relay_info = common::create_relay_info(
        "Minimal Relay (SO_REUSEPORT)",
        "A minimal Nostr relay using SO_REUSEPORT and CPU affinity",
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

    // Use the OPTIMIZED server with SO_REUSEPORT and CPU affinity
    common::run_relay_server_optimized(
        app,
        addr,
        "Minimal relay (SO_REUSEPORT)",
        common::ServerConfig::default(),
    )
    .await
}
