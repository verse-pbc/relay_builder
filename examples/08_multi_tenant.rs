//! Multi-tenant relay - demonstrates subdomain isolation
//!
//! This example shows how subdomain-based data isolation works. Each subdomain
//! gets its own isolated event storage - perfect for SaaS or community hosting.
//!
//! Run with: cargo run --example 08_multi_tenant --features axum

use anyhow::Result;
use async_trait::async_trait;
use axum::{routing::get, Router};
use nostr_sdk::prelude::*;
use relay_builder::{
    EventContext, EventProcessor, RelayBuilder, RelayConfig, RelayInfo, Result as RelayResult,
    StoreCommand,
};
use std::net::SocketAddr;
use std::sync::Arc;

/// Simple processor that logs which subdomain received the event
#[derive(Debug, Clone)]
struct MultiTenantProcessor;

#[async_trait]
impl EventProcessor for MultiTenantProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<parking_lot::RwLock<()>>,
        context: EventContext<'_>,
    ) -> RelayResult<Vec<StoreCommand>> {
        // Log subdomain info
        tracing::info!(
            "Event received - subdomain: {:?}, pubkey: {}",
            context.subdomain,
            event.pubkey
        );

        // Events are automatically isolated by subdomain
        // No need to do anything special!
        Ok(vec![(event, context.subdomain.clone()).into()])
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Create relay configuration with subdomain support
    let relay_url = "ws://relay.example.com";
    let keys = Keys::generate();
    let config = RelayConfig::new(relay_url, "./multi_tenant.db", keys)
        // Enable subdomain-based isolation
        .with_subdomains_from_url(relay_url);

    // Relay information for NIP-11
    let relay_info = RelayInfo {
        name: "Multi-Tenant Relay".to_string(),
        description: "Isolated spaces for different communities".to_string(),
        pubkey: config.keys.public_key().to_string(),
        contact: "admin@example.com".to_string(),
        supported_nips: vec![1, 9, 50],
        software: "relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Build relay with multi-tenant support
    let handler = RelayBuilder::new(config)
        .with_event_processor(MultiTenantProcessor)
        .with_relay_info(relay_info)
        .build_axum()
        .await?;

    // Create HTTP server
    let app = Router::new().route("/", get(handler));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    println!("🚀 Multi-Tenant Relay running at:");
    println!("📡 WebSocket: ws://localhost:8080");
    println!("🌐 Browser: http://localhost:8080");
    println!("🏢 Multi-tenancy: Enabled via subdomains");
    println!();
    println!("How it works:");
    println!("  - Connect to alice.relay.example.com → Events stored in 'alice' scope");
    println!("  - Connect to bob.relay.example.com → Events stored in 'bob' scope");
    println!("  - Each subdomain is completely isolated");
    println!();
    println!("Testing locally:");
    println!("  1. Add to /etc/hosts:");
    println!("     127.0.0.1 relay.example.com");
    println!("     127.0.0.1 alice.relay.example.com");
    println!("     127.0.0.1 bob.relay.example.com");
    println!("  2. Connect to ws://alice.relay.example.com:8080");
    println!("  3. Events are automatically isolated!");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
