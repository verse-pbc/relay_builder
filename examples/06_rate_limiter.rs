//! Rate limiter relay - demonstrates custom middleware
//!
//! This example shows how to create custom middleware for cross-cutting concerns.
//! Here we implement a simple rate limiter that counts messages per connection.
//!
//! Run with: cargo run --example 06_rate_limiter --features axum

use anyhow::Result;
use async_trait::async_trait;
use axum::{routing::get, Router};
use nostr_sdk::prelude::*;
use relay_builder::{NostrConnectionState, RelayBuilder, RelayConfig, RelayInfo};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;
use websocket_builder::{InboundContext, Middleware, SendMessage};

/// Track rate limit info per connection
#[derive(Debug, Clone)]
struct ConnectionRateInfo {
    events_received: u32,
    window_start: Instant,
}

/// Simple rate limiting middleware
#[derive(Debug, Clone)]
struct RateLimitMiddleware {
    max_events_per_minute: u32,
    connections: Arc<Mutex<HashMap<String, ConnectionRateInfo>>>,
}

#[async_trait]
impl Middleware for RateLimitMiddleware {
    type State = NostrConnectionState<()>;
    type IncomingMessage = ClientMessage<'static>;
    type OutgoingMessage = RelayMessage<'static>;

    async fn process_inbound(
        &self,
        ctx: &mut InboundContext<Self::State, Self::IncomingMessage, Self::OutgoingMessage>,
    ) -> Result<(), anyhow::Error> {
        // Only count EVENT messages
        if let Some(ClientMessage::Event(_)) = &ctx.message {
            let mut connections = self.connections.lock().await;
            let now = Instant::now();

            let info = connections
                .entry(ctx.connection_id.clone())
                .or_insert_with(|| ConnectionRateInfo {
                    events_received: 0,
                    window_start: now,
                });

            // Reset counter if minute has passed
            if now.duration_since(info.window_start) > Duration::from_secs(60) {
                info.events_received = 0;
                info.window_start = now;
            }

            // Increment counter
            info.events_received += 1;

            // Check if over limit
            if info.events_received > self.max_events_per_minute {
                tracing::warn!(
                    "Rate limit exceeded for connection {}: {} events",
                    ctx.connection_id,
                    info.events_received
                );

                // Send notice to client
                ctx.send_message(RelayMessage::notice("rate limit exceeded - slow down!"))?;

                // Drop the message (don't call next)
                return Ok(());
            }
        }

        // Continue to next middleware
        ctx.next().await
    }

    async fn on_disconnect(
        &self,
        ctx: &mut websocket_builder::DisconnectContext<
            Self::State,
            Self::IncomingMessage,
            Self::OutgoingMessage,
        >,
    ) -> Result<(), anyhow::Error> {
        // Clean up connection tracking
        self.connections.lock().await.remove(&ctx.connection_id);
        ctx.next().await
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Create relay configuration
    let relay_url = "ws://localhost:8080";
    let keys = Keys::generate();
    let config = RelayConfig::new(relay_url, "./rate_limiter.db", keys);

    // Relay information for NIP-11
    let relay_info = RelayInfo {
        name: "Rate Limited Relay".to_string(),
        description: "Demonstrates custom rate limiting middleware".to_string(),
        pubkey: config.keys.public_key().to_string(),
        contact: "admin@example.com".to_string(),
        supported_nips: vec![1, 9, 50],
        software: "relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Build relay with rate limiting middleware
    let handler = RelayBuilder::new(config)
        // Add our custom middleware BEFORE the default event processor
        .with_middleware(RateLimitMiddleware {
            max_events_per_minute: 10, // Very low for demo purposes
            connections: Arc::new(Mutex::new(HashMap::new())),
        })
        .with_relay_info(relay_info)
        .build_axum()
        .await?;

    // Create HTTP server
    let app = Router::new().route("/", get(handler));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    println!("🚀 Rate Limited Relay running at:");
    println!("📡 WebSocket: ws://localhost:8080");
    println!("🌐 Browser: http://localhost:8080");
    println!("⚡ Rate limit: 10 events per connection");
    println!();
    println!("Try sending more than 10 events to see rate limiting in action!");
    println!();
    println!("Note: This is a simple demo. Production rate limiters should:");
    println!("  - Track time windows (e.g., sliding window)");
    println!("  - Consider different limits for different event kinds");
    println!("  - Possibly use per-IP or per-pubkey limits");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
