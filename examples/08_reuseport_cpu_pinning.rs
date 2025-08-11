//! Proof of Concept: SO_REUSEPORT with CPU pinning for improved performance
//!
//! This example demonstrates:
//! - Multiple SO_REUSEPORT listeners in a single process
//! - Dedicated CPU for database writer thread
//! - Worker threads constrained to avoid the writer's CPU
//! - Integration with relay_builder's architecture
//!
//! Run with: cargo run --example 08_reuseport_cpu_pinning --features axum
//!
//! Test with high connection count:
//!   for i in {1..100}; do nak req --stream ws://localhost:8080 & done

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
use relay_builder::{cpu_affinity, RelayBuilder, RelayConfig};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpSocket};
use tokio::runtime::Builder;
use tokio::task::JoinSet;

/// Create a SO_REUSEPORT listener for the given address
fn make_reuseport_listener(addr: SocketAddr) -> io::Result<TcpListener> {
    let socket = if addr.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };

    socket.set_reuseaddr(true)?;

    // Enable SO_REUSEPORT for kernel-level load balancing
    #[cfg(all(
        unix,
        not(target_os = "solaris"),
        not(target_os = "illumos"),
        not(target_os = "cygwin"),
    ))]
    socket.set_reuseport(true)?;

    socket.bind(addr)?;
    socket.listen(1024)
}

fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    let cpu_count = num_cpus::get();
    println!("System has {cpu_count} CPUs");

    if cpu_count < 2 {
        eprintln!("Warning: This example works best with at least 2 CPUs for isolation");
    }

    // Reserve the last CPU for the database writer (avoiding CPU 0 which handles more IRQs)
    let writer_cpu = cpu_count.saturating_sub(1);
    let allowed_workers: Vec<usize> = (0..cpu_count).filter(|c| *c != writer_cpu).collect();

    println!("Configuration:");
    println!("  Database writer CPU: {writer_cpu}");
    println!("  Worker CPUs: {allowed_workers:?}");

    // Build the runtime with CPU affinity for workers
    let worker_threads = allowed_workers.len().max(1);
    let excluded = vec![writer_cpu];

    let rt = Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_io()
        .enable_time()
        .on_thread_start(move || {
            // Set affinity for each worker thread to exclude the writer CPU
            if let Err(e) = cpu_affinity::allow_all_but_current_thread(&excluded) {
                eprintln!("Failed to set worker affinity: {e}");
            } else {
                println!(
                    "Worker thread {:?} started with affinity mask excluding CPU {:?}",
                    std::thread::current().id(),
                    excluded
                );
            }
        })
        .build()?;

    // Run the relay server
    rt.block_on(async move {
        // Create relay configuration with CPU pinning for database writer
        let relay_url = "ws://localhost:8080";
        let db_path = "./reuseport_relay_db";
        let relay_keys = Keys::generate();

        let config = RelayConfig::new(relay_url, db_path, relay_keys)
            .with_ingester_cpu_affinity(writer_cpu) // Pin database writer to dedicated CPU
            .with_max_readers(worker_threads as u32 * 10) // Adjust readers for multi-threaded access
            .with_diagnostics(); // Enable health check logging

        println!("Database ingester will be pinned to CPU {writer_cpu}");

        // Create relay info for NIP-11
        let relay_info = common::create_relay_info(
            "SO_REUSEPORT Relay",
            "High-performance relay with CPU pinning and SO_REUSEPORT",
            config.keys.public_key(),
            vec![1, 9, 50],
        );

        // Build the relay handler factory
        let handler_factory = Arc::new(RelayBuilder::<()>::new(config.clone()).build().await?);

        // Create the root handler
        let root_handler = common::create_root_handler(handler_factory.clone(), relay_info);

        // Create HTTP app
        let app = Router::new().route("/", get(root_handler));

        let addr: SocketAddr = "0.0.0.0:8080".parse().unwrap();

        // Create multiple SO_REUSEPORT listeners (one per worker thread)
        let listener_count = worker_threads.max(1);
        println!(
            "Creating {listener_count} SO_REUSEPORT listeners on {addr}"
        );

        let mut tasks = JoinSet::new();

        for i in 0..listener_count {
            let listener = make_reuseport_listener(addr)?;
            let app = app.clone();

            tasks.spawn(async move {
                println!(
                    "Listener {i} ready on {addr} (writer CPU: {writer_cpu}, workers exclude it)"
                );

                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                .map_err(|e| anyhow::anyhow!(e))
            });
        }

        // Optional: Periodic status reporting
        let allowed_workers_status = allowed_workers.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(30)).await;
                println!(
                    "Status: Writer pinned to CPU {writer_cpu}, workers on CPUs {allowed_workers_status:?}"
                );
            }
        });

        println!("🚀 SO_REUSEPORT relay listening on: {addr}");
        println!("📡 WebSocket endpoint: ws://{addr}");
        println!("🔧 CPU Configuration:");
        println!("    Database writer: CPU {writer_cpu}");
        println!("    Workers: CPUs {allowed_workers:?}");
        println!("    Listeners: {listener_count}");

        // Wait for all listeners
        while let Some(res) = tasks.join_next().await {
            res??;
        }

        Ok::<_, anyhow::Error>(())
    })?;

    Ok(())
}
