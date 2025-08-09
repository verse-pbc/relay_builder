//! Example relay with diagnostic health checks enabled
//!
//! This example demonstrates how to enable periodic health checks that log
//! internal state metrics every 30 minutes to help identify memory issues.

use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use relay_builder::{EventContext, EventProcessor, RelayBuilder, RelayConfig, StoreCommand};
use std::sync::Arc;

/// Simple event processor that accepts all events
#[derive(Debug, Clone)]
struct AcceptAllProcessor;

impl EventProcessor for AcceptAllProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<RwLock<()>>,
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>, relay_builder::Error> {
        // Accept all events
        Ok(vec![StoreCommand::SaveSignedEvent(
            Box::new(event),
            context.subdomain.clone(),
            None, // No response handler
        )])
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("relay_builder=info".parse()?),
        )
        .init();

    // Generate relay keys
    let keys = Keys::generate();

    // Create configuration with diagnostics enabled
    let config = RelayConfig::new("ws://localhost:8080", "./data/relay_with_diagnostics", keys)
        .with_diagnostics(); // Enable health checks every 30 minutes

    // Build the relay
    let handler_factory = RelayBuilder::<()>::new(config)
        .event_processor(AcceptAllProcessor)
        .build()
        .await?;

    println!("Relay with diagnostics started!");
    println!("Health checks will be logged every 30 minutes at INFO level.");
    println!("Look for '=== Relay Health Check ===' in the logs.");
    println!();
    println!("To test immediately, modify diagnostics.rs to use a shorter interval.");

    // Create the WebSocket route
    let app = websocket_builder::websocket_route("/", handler_factory);

    // Start the server
    let listener = tokio::net::TcpListener::bind("127.0.0.1:8080").await?;
    println!("Server running on ws://localhost:8080");

    axum::serve(listener, app).await?;

    Ok(())
}
