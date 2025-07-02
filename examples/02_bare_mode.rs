//! Example of using bare mode to manually configure all middlewares
//!
//! This example demonstrates how to use the bare() builder option to skip
//! automatic middleware and manually add them. This is useful when you need
//! fine-grained control over middleware ordering or want to implement custom
//! alternatives.
//!
//! WARNING: Bare mode skips critical security features like signature verification!

use async_trait::async_trait;
use axum::{routing::get, Router};
use nostr_relay_builder::{
    crypto_helper::CryptoHelper, middlewares::*, Error, EventContext, EventProcessor, RelayBuilder,
    RelayConfig, RelayInfo, StoreCommand,
};
use nostr_sdk::prelude::*;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio_util::task::TaskTracker;
use tracing::{info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

/// A simple event processor that accepts all events
#[derive(Debug)]
struct SimpleProcessor;

#[async_trait]
impl EventProcessor for SimpleProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<parking_lot::RwLock<()>>,
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>, Error> {
        info!(
            "Processing event {} of kind {} from {}",
            event.id, event.kind, event.pubkey
        );

        // Store all valid events
        Ok(vec![(event, context.subdomain.clone()).into()])
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "bare_relay=debug,nostr_relay_builder=debug".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Generate keys for the relay
    let keys = Keys::generate();
    info!("Relay public key: {}", keys.public_key());

    // Create relay configuration
    let config = RelayConfig::new("ws://localhost:8080", "./data/bare_relay", keys.clone())
        .with_max_subscriptions(100)
        .with_max_limit(5000);

    // Create a task tracker and crypto helper for event verification
    let task_tracker = TaskTracker::new();
    let crypto_helper = CryptoHelper::new(Arc::new(keys.clone()));

    // Build the relay in bare mode and manually add middlewares
    let builder = RelayBuilder::new(config)
        .bare() // Enable bare mode - no automatic middlewares
        .with_task_tracker(task_tracker)
        // Now manually add the middlewares that would normally be added automatically
        .with_middleware(LoggerMiddleware::new())
        .with_middleware(ErrorHandlingMiddleware::new())
        .with_middleware(EventVerifierMiddleware::new(crypto_helper));

    warn!("This relay is running in BARE MODE with manually configured middleware");
    warn!("The example shows how to replicate default behavior, but you can:");
    warn!("- Skip any of these middlewares");
    warn!("- Add them in a different order");
    warn!("- Replace them with custom implementations");

    // Create relay info for NIP-11
    let relay_info = RelayInfo {
        name: "Bare Mode Example Relay".to_string(),
        description: "Example relay demonstrating bare mode with manual middleware configuration"
            .to_string(),
        pubkey: keys.public_key().to_string(),
        contact: "admin@example.com".to_string(),
        supported_nips: vec![1, 2, 9, 11, 12, 15, 16, 20, 22],
        software: "https://github.com/verse-pbc/nostr_relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Build the relay handler using the new build_axum method
    let root_handler = builder
        .with_event_processor(SimpleProcessor)
        .with_relay_info(relay_info)
        .build_axum()
        .await?;

    // Create HTTP server
    let app = Router::new()
        .route("/", get(root_handler))
        .into_make_service_with_connect_info::<SocketAddr>();

    let addr = SocketAddr::from(([0, 0, 0, 0], 8080));
    info!("🚀 Bare relay listening on: {}", addr);
    info!("📡 WebSocket endpoint: ws://localhost:8080");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
