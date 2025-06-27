//! Production relay - demonstrates monitoring and lifecycle features
//!
//! This example shows production-ready features:
//! - Graceful shutdown with TaskTracker
//! - Connection counting
//! - Performance metrics
//! - Health endpoints
//! - Configuration limits
//!
//! Run with: cargo run --example 09_production --features axum

use anyhow::Result;
use axum::{extract::State, routing::get, Router};
use nostr_relay_builder::{RelayBuilder, RelayConfig, RelayInfo, WebSocketConfig};
use nostr_sdk::prelude::*;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

/// Health check endpoint
async fn health(State(counter): State<Arc<AtomicUsize>>) -> String {
    let connections = counter.load(Ordering::Relaxed);
    format!("{{\"status\":\"ok\",\"active_connections\":{connections}}}")
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Production configuration
    let relay_url = "ws://localhost:8080";
    let keys = Keys::generate();
    let mut config = RelayConfig::new(relay_url, "./production.db", keys);

    // Configure limits (similar to groups_relay)
    config = config.with_subscription_limits(100, 1000); // max 100 subs, 1000 events per query

    // WebSocket configuration
    config = config.with_websocket_config(WebSocketConfig {
        max_connections: Some(1000),
        max_connection_time: Some(3600), // 1 hour
    });

    // Relay information
    let relay_info = RelayInfo {
        name: "Production Relay".to_string(),
        description: "Production-ready relay with monitoring".to_string(),
        pubkey: config.keys.public_key().to_string(),
        contact: "ops@example.com".to_string(),
        supported_nips: vec![1, 9, 11, 40],
        software: "nostr_relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Production components
    let shutdown_token = CancellationToken::new();
    let task_tracker = TaskTracker::new();
    let connection_counter = Arc::new(AtomicUsize::new(0));

    // Build relay with all production features
    let service = RelayBuilder::<()>::new(config)
        .with_task_tracker(task_tracker.clone())
        .with_cancellation_token(shutdown_token.clone())
        .with_connection_counter(connection_counter.clone())
        // In production, you might also add:
        // .with_metrics(your_metrics_handler)
        // .with_subscription_metrics(PrometheusSubscriptionMetricsHandler)
        .build_relay_service(relay_info)
        .await?;

    // Create HTTP server with health endpoint
    let app = Router::new()
        .route("/", get(service.axum_root_handler()))
        .route("/health", get(health))
        .with_state(connection_counter.clone());

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    println!("🚀 Production Relay running at:");
    println!("📡 WebSocket: ws://localhost:8080");
    println!("🌐 Browser: http://localhost:8080");
    println!("💚 Health: http://localhost:8080/health");
    println!();
    println!("Production features enabled:");
    println!("  ✅ Graceful shutdown (Ctrl+C)");
    println!("  ✅ Connection counting");
    println!("  ✅ Health endpoint");
    println!("  ✅ Subscription limits (100 subs, 1000 events/query)");
    println!("  ✅ Connection limits (1000 max, 1hr timeout)");
    println!();
    println!("Reference: See groups_relay for advanced patterns like:");
    println!("  - PrometheusSubscriptionMetricsHandler");
    println!("  - Custom ValidationMiddleware");
    println!("  - SampledMetricsHandler");

    // Close task tracker early - no new tasks will be spawned
    task_tracker.close();

    // Graceful shutdown handler
    let shutdown_signal = async move {
        tokio::signal::ctrl_c().await.unwrap();
        println!("\n⏹️  Shutting down gracefully...");
        shutdown_token.cancel();
    };

    // Run server
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal)
    .await?;

    // At this point, the app and all its handlers should be dropped
    // The service Arc was moved into the handler, so it will be dropped
    // when the server shuts down and the handler is dropped

    // Note: The database will show a warning about not calling shutdown()
    // This is a known limitation - the RelayService doesn't expose a shutdown method
    // In production, you might want to modify the library to add proper shutdown support

    println!("⏳ Waiting for background tasks to complete...");
    task_tracker.wait().await;

    println!("✅ Shutdown complete");

    Ok(())
}
