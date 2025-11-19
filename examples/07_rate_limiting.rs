//! Rate limiting example - demonstrates usage of RateLimitMiddleware
//!
//! This example shows:
//! - How to add rate limiting middleware to your relay
//! - Per-connection rate limiting
//! - Global rate limiting
//! - Different quota configurations
//!
//! Run with: cargo run --example 07_rate_limiting --features axum

mod common;

use anyhow::Result;
use axum::{
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use governor::Quota;
use nonzero_ext::nonzero;
use nostr_sdk::prelude::*;
use relay_builder::{handle_upgrade, HandlerFactory, WebSocketUpgrade};
use relay_builder::{
    middlewares::RateLimitMiddleware, EventContext, EventProcessor, RelayBuilder, RelayConfig,
    RelayInfo, StoreCommand,
};
use std::net::SocketAddr;
use std::sync::Arc;

/// Simple event processor that accepts all events
#[derive(Debug, Clone)]
struct SimpleProcessor;

impl EventProcessor for SimpleProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<tokio::sync::RwLock<()>>,
        context: &EventContext,
    ) -> Result<Vec<StoreCommand>, relay_builder::Error> {
        tracing::info!("Processing event {} from {}", event.id, event.pubkey);
        Ok(vec![(event, (*context.subdomain).clone()).into()])
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    common::init_logging();

    // Create relay configuration
    let keys = Keys::generate();
    let config = RelayConfig::new("ws://localhost:8080", "./data/rate_limiting", keys);

    let relay_info = RelayInfo {
        name: "Rate Limited Relay".to_string(),
        description: "A relay demonstrating rate limiting middleware".to_string(),
        pubkey: config.keys.public_key().to_hex(),
        contact: "admin@ratelimited.relay".to_string(),
        supported_nips: vec![1, 9, 11],
        software: "relay_builder/rate_limiting".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    tracing::info!("🚀 Building relay with rate limiting middleware...");

    // Example 1: Basic per-connection rate limiting
    // Allow 10 events per second per connection
    let per_connection_limiter = RateLimitMiddleware::<()>::new(
        Quota::per_second(nonzero!(10u32)), // 10 events per second per connection
    );

    // Example 2: More restrictive rate limiting
    // Allow only 2 events per second per connection
    let _strict_limiter = RateLimitMiddleware::<()>::new(
        Quota::per_second(nonzero!(2u32)), // 2 events per second per connection
    );

    // Example 3: Rate limiting with both per-connection and global limits
    // Per connection: 5 events/sec, Global: 50 events/sec for entire relay
    let _hybrid_limiter = RateLimitMiddleware::<()>::with_global_limit(
        Quota::per_second(nonzero!(5u32)),  // Per connection limit
        Quota::per_second(nonzero!(50u32)), // Global relay limit
    );

    // Example 4: Different time windows
    let _per_minute_limiter = RateLimitMiddleware::<()>::new(
        Quota::per_minute(nonzero!(60u32)), // 60 events per minute (1 per second average)
    );

    // Build relay with the rate limiting middleware
    // You can choose which one to use by commenting/uncommenting
    let handler_factory = Arc::new(
        RelayBuilder::<()>::new(config.clone())
            .event_processor(SimpleProcessor)
            .build_with(|chain| {
                chain.with(per_connection_limiter) // Basic rate limiting
                                                   // .with(strict_limiter)      // Strict rate limiting
                                                   // .with(hybrid_limiter)      // Hybrid rate limiting
                                                   // .with(per_minute_limiter)  // Per-minute rate limiting
            })
            .await?,
    );

    print_relay_info(&relay_info);

    // Create HTTP server with unified handler
    let root_handler = {
        let relay_info_clone = relay_info.clone();
        move |ws: Option<WebSocketUpgrade>,
              axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
              headers: axum::http::HeaderMap| {
            let handler_factory = handler_factory.clone();
            let _relay_info = relay_info_clone.clone();

            async move {
                match ws {
                    Some(ws) => {
                        // Handle WebSocket upgrade
                        let handler = handler_factory.create(&headers);
                        handle_upgrade(ws, addr, handler).await
                    }
                    None => {
                        // Return relay info page
                        home_handler().await.into_response()
                    }
                }
            }
        }
    };

    let app = Router::new().route("/", get(root_handler));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    tracing::info!("🌐 Server listening on http://{}", addr);

    // Start the server
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn home_handler() -> impl IntoResponse {
    Html(
        r#"
        <html>
        <head><title>Rate Limited Nostr Relay</title></head>
        <body>
            <h1>Rate Limited Nostr Relay</h1>
            <p>WebSocket endpoint: <code>ws://localhost:8080/ws</code></p>
            <h2>Rate Limiting Features:</h2>
            <ul>
                <li>Per-connection rate limiting (default: 10 events/second)</li>
                <li>Global rate limiting (optional)</li>
                <li>Configurable quotas (per second, minute, hour)</li>
                <li>Automatic rate limit error messages</li>
            </ul>
            <h2>Testing Rate Limits:</h2>
            <p>Connect with a Nostr client and try sending events rapidly to see rate limiting in action!</p>
            <p>You'll receive error messages when limits are exceeded.</p>
        </body>
        </html>
        "#,
    )
}

fn print_relay_info(info: &RelayInfo) {
    tracing::info!("📋 Relay Information:");
    tracing::info!("  Name: {}", info.name);
    tracing::info!("  Description: {}", info.description);
    tracing::info!("  Pubkey: {}", info.pubkey);
    tracing::info!("  Contact: {}", info.contact);
    tracing::info!("  Software: {}", info.software);
    tracing::info!("  Version: {}", info.version);
    tracing::info!("  Supported NIPs: {:?}", info.supported_nips);
    tracing::info!("🛡️  Rate limiting middleware is active!");
}
