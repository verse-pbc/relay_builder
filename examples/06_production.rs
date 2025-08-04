//! Production relay - demonstrates monitoring and lifecycle features
//!
//! This example shows production-ready features:
//! - Graceful shutdown with TaskTracker
//! - Connection counting
//! - Performance metrics
//! - Health endpoints
//! - Configuration limits
//!
//! Run with: cargo run --example 06_production --features axum

mod common;

use anyhow::Result;
use axum::{
    extract::State,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use nostr_sdk::prelude::*;
use relay_builder::{RelayBuilder, RelayConfig, RelayInfo, WebSocketConfig};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use websocket_builder::{handle_upgrade, HandlerFactory, WebSocketUpgrade};

/// Health check endpoint
async fn health(State(counter): State<Arc<AtomicUsize>>) -> String {
    let connections = counter.load(Ordering::Relaxed);
    format!("{{\"status\":\"ok\",\"active_connections\":{connections}}}")
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    common::init_logging();

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
        supported_nips: vec![1, 9, 50],
        software: "relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Production components
    let shutdown_token = CancellationToken::new();
    let task_tracker = TaskTracker::new();
    let connection_counter = Arc::new(AtomicUsize::new(0));

    // Build relay with all production features
    let handler_factory = Arc::new(
        RelayBuilder::<()>::new(config)
            .task_tracker(task_tracker.clone())
            .cancellation_token(shutdown_token.clone())
            .connection_counter(connection_counter.clone())
            // In production, you might also add:
            // .metrics(your_metrics_handler)
            // .subscription_metrics(PrometheusSubscriptionMetricsHandler)
            .build()
            .await?,
    );

    // Create a handler that supports WebSocket, NIP-11, and health check
    let relay_info_clone = relay_info.clone();
    let connection_counter_clone = connection_counter.clone();
    let ws_handler = move |ws: Option<WebSocketUpgrade>,
                           axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<
        SocketAddr,
    >,
                           headers: axum::http::HeaderMap| {
        let handler_factory = handler_factory.clone();
        let relay_info = relay_info_clone.clone();

        async move {
            match ws {
                Some(ws) => {
                    // Handle WebSocket upgrade
                    let handler = handler_factory.create(&headers);
                    handle_upgrade(ws, addr, handler).await
                }
                None => {
                    // Check for NIP-11 JSON request
                    if let Some(accept) = headers.get(axum::http::header::ACCEPT) {
                        if let Ok(value) = accept.to_str() {
                            if value == "application/nostr+json" {
                                return axum::Json(&relay_info).into_response();
                            }
                        }
                    }

                    // Serve HTML info page
                    Html(relay_builder::handlers::default_relay_html(&relay_info)).into_response()
                }
            }
        }
    };

    // Create HTTP server with health endpoint
    let app = Router::new()
        .route("/", get(ws_handler))
        .route("/health", get(health))
        .with_state(connection_counter_clone);

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
    println!();

    // Set up graceful shutdown
    let shutdown_handle = shutdown_token.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install CTRL+C signal handler");
        println!("\n📛 Shutdown signal received, closing connections...");
        shutdown_handle.cancel();
    });

    // Run server with graceful shutdown
    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_token.cancelled().await;
    });

    server.await?;

    // Wait for all tasks to complete
    task_tracker.close();
    task_tracker.wait().await;

    println!("✅ All tasks completed. Goodbye!");
    Ok(())
}
