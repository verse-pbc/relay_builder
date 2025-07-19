//! Custom middleware - demonstrates NostrMiddleware implementation
//!
//! This example shows:
//! - How to create custom middleware using NostrMiddleware trait
//! - Using build_with for custom middleware stacks
//! - without_defaults() mode for complete control
//! - on_connect handling for welcome messages
//! - Both default and without_defaults approaches
//!
//! Run with: cargo run --example 05_custom_middleware

use anyhow::Result;
use axum::{
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use nostr_sdk::prelude::*;
use relay_builder::{
    middlewares::{ErrorHandlingMiddleware, NostrLoggerMiddleware},
    nostr_middleware::{ConnectionContext, InboundContext, InboundProcessor, NostrMiddleware},
    EventContext, EventProcessor, RelayBuilder, RelayConfig, RelayInfo, StoreCommand,
};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use websocket_builder::{handle_upgrade, HandlerFactory, WebSocketUpgrade};

/// Custom middleware that logs request timing
#[derive(Debug, Clone)]
struct TimingMiddleware {
    name: String,
}

impl TimingMiddleware {
    fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

impl<T> NostrMiddleware<T> for TimingMiddleware
where
    T: Send + Sync + Clone + 'static,
{
    async fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, T, Next>,
    ) -> Result<(), anyhow::Error>
    where
        Next: InboundProcessor<T>,
    {
        let start = Instant::now();
        let msg_type = ctx.message.as_ref().map(|m| format!("{m:?}"));

        // Process the message
        let result = ctx.next().await;

        let duration = start.elapsed();
        tracing::info!(
            "[{}] Message {:?} processed in {:?}",
            self.name,
            msg_type,
            duration
        );

        result
    }
}

/// Welcome middleware that sends a message on connect
#[derive(Clone)]
struct WelcomeMiddleware {
    message: String,
}

impl<T> NostrMiddleware<T> for WelcomeMiddleware
where
    T: Send + Sync + Clone + 'static,
{
    async fn on_connect(&self, ctx: ConnectionContext<'_, T>) -> Result<(), anyhow::Error> {
        tracing::info!("🔌 New connection from: {}", ctx.connection_id);
        ctx.send_notice(self.message.clone())?;
        Ok(())
    }
}

/// Simple event processor
#[derive(Debug, Clone)]
struct SimpleProcessor;

impl EventProcessor for SimpleProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<parking_lot::RwLock<()>>,
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>, relay_builder::Error> {
        tracing::debug!("Processing event {} from {}", event.id, event.pubkey);
        Ok(vec![(event, context.subdomain.clone()).into()])
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Show both approaches
    println!("Choose middleware approach:");
    println!("1. Default (automatic Logger and ErrorHandling)");
    println!("2. Without defaults (full control)");
    println!();
    println!("Running with approach 1 (default)...");
    println!();

    // Create relay configuration
    let keys = Keys::generate();
    let config = RelayConfig::new("ws://localhost:8080", "./data/custom_middleware", keys);

    let relay_info = RelayInfo {
        name: "Custom Middleware Relay".to_string(),
        description: "Demonstrates custom middleware implementation".to_string(),
        pubkey: config.keys.public_key().to_hex(),
        contact: "admin@custom.relay".to_string(),
        supported_nips: vec![1, 9, 11],
        software: "relay_builder/custom_middleware".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Approach 1: Default - automatically includes Logger and ErrorHandling
    let handler_factory = Arc::new(
        RelayBuilder::<()>::new(config.clone())
            .event_processor(SimpleProcessor)
            .build_with(|chain| {
                chain
                    // Add your custom middleware
                    .with(WelcomeMiddleware {
                        message: "Welcome to the custom middleware relay!".to_string(),
                    })
                    .with(TimingMiddleware::new("request-timer"))
                // Logger and ErrorHandling are automatically added as outermost layers
            })
            .await?,
    );

    // For demonstration, here's Approach 2: Without defaults (full control)
    // Uncomment to use this approach instead:
    /*
    let handler_factory = Arc::new(
        RelayBuilder::<()>::new(config.clone())
            .without_defaults() // No automatic middleware
            .event_processor(SimpleProcessor)
            .build_with(|chain| {
                chain
                    // Manually add ALL middleware in exact order
                    .with(WelcomeMiddleware {
                        message: "Welcome to the relay without defaults!".to_string(),
                    })
                    .with(TimingMiddleware::new("request-timer"))
                    .with(NostrLoggerMiddleware::new())
                    .with(ErrorHandlingMiddleware::new())
                // Note: Event signature verification is handled by EventIngester
                // The relay middleware is still added automatically at the end
            })
            .await?,
    );
    */

    let _ = (
        NostrLoggerMiddleware::<()>::new(),
        ErrorHandlingMiddleware::new(),
    ); // Keep imports used

    // Create a unified handler that supports both WebSocket and HTTP on the same route
    let root_handler = {
        let relay_info_clone = relay_info.clone();
        move |ws: Option<WebSocketUpgrade>,
              axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
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
                        Html(relay_builder::handlers::default_relay_html(&relay_info))
                            .into_response()
                    }
                }
            }
        }
    };

    // Create HTTP server
    let app = Router::new().route("/", get(root_handler));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    println!("🚀 Custom middleware relay running at: {addr}");
    println!("📡 WebSocket endpoint: ws://localhost:8080");
    println!();
    println!("Middleware stack (in order):");
    println!("  1. NostrLoggerMiddleware - Logs all messages (automatic)");
    println!("  2. ErrorHandlingMiddleware - Catches errors (automatic)");
    println!("  3. WelcomeMiddleware - Sends welcome on connect");
    println!("  4. TimingMiddleware - Logs processing time");
    println!("  5. RelayMiddleware - Core relay logic (automatic)");
    println!();
    println!("Features demonstrated:");
    println!("  ✅ Custom middleware with NostrMiddleware trait");
    println!("  ✅ on_connect handling for welcome messages");
    println!("  ✅ Timing middleware for performance monitoring");
    println!("  ✅ build_with for custom middleware stacks");
    println!("  ✅ Automatic essentials unless without_defaults()");
    println!();
    println!("Note: Event signature verification is handled automatically by EventIngester");
    println!();
    println!("Connect with a Nostr client to see the welcome message!");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
