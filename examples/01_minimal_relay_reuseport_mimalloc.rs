//! Minimal Nostr relay - SO_REUSEPORT + mimalloc version
//!
//! This uses SO_REUSEPORT with multiple listeners, CPU affinity, and mimalloc allocator
//!
//! Run with: cargo run --example 01_minimal_relay_reuseport_mimalloc --features axum,mimalloc

// Use mimalloc for better multi-threaded performance
// Note: This will override jemalloc if both features are enabled
#[cfg(all(
    not(target_env = "musl"),
    feature = "mimalloc",
    not(feature = "jemalloc")
))]
use mimalloc::MiMalloc;

#[cfg(all(
    not(target_env = "musl"),
    feature = "mimalloc",
    not(feature = "jemalloc")
))]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

// Fallback to jemalloc if both are enabled (since it's the default)
#[cfg(all(not(target_env = "musl"), feature = "jemalloc", feature = "mimalloc"))]
use mimalloc::MiMalloc;

#[cfg(all(not(target_env = "musl"), feature = "jemalloc", feature = "mimalloc"))]
#[global_allocator]
static GLOBAL: MiMalloc = MiMalloc;

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
    let db_path =
        std::env::var("RELAY_DATA_DIR").unwrap_or_else(|_| "./minimal_relay_db".to_string());
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
        "Minimal Relay (SO_REUSEPORT + mimalloc)",
        "A minimal Nostr relay using SO_REUSEPORT, CPU affinity, and mimalloc",
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
        "Minimal relay (SO_REUSEPORT + mimalloc)",
        common::ServerConfig::default(),
    )
    .await
}
