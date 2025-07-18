//! Spam filter relay - demonstrates custom EventProcessor for business logic
//!
//! This example shows how to use EventProcessor to implement custom rules
//! for accepting or rejecting events. Here we filter out spam messages.
//!
//! Run with: cargo run --example spam_filter_relay --features axum

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

/// Custom processor that filters spam
#[derive(Debug, Clone)]
struct SpamFilterProcessor {
    blocked_words: Vec<String>,
}

#[async_trait]
impl EventProcessor for SpamFilterProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<parking_lot::RwLock<()>>,
        context: EventContext<'_>,
    ) -> RelayResult<Vec<StoreCommand>> {
        let content_lower = event.content.to_lowercase();

        // Check for blocked words
        for word in &self.blocked_words {
            if content_lower.contains(word) {
                // Log the rejection
                tracing::info!("Rejected spam from {}: contains '{}'", event.pubkey, word);
                // Return empty vec to reject silently
                return Ok(vec![]);
            }
        }

        // Check for excessive mentions (potential spam)
        let mention_count = event
            .tags
            .iter()
            .filter(|tag| tag.kind() == TagKind::p())
            .count();

        if mention_count > 20 {
            tracing::info!(
                "Rejected spam from {}: too many mentions ({})",
                event.pubkey,
                mention_count
            );
            return Ok(vec![]);
        }

        // Event passed all checks - accept it
        tracing::debug!("Accepted event from {}", event.pubkey);

        Ok(vec![(event, context.subdomain.clone()).into()])
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Create relay configuration
    let relay_url = "ws://localhost:8080";
    let keys = Keys::generate();
    let config = RelayConfig::new(relay_url, "./spam_filter_relay.db", keys);

    // Relay information for NIP-11
    let relay_info = RelayInfo {
        name: "Spam Filter Relay".to_string(),
        description: "A relay that filters spam messages".to_string(),
        pubkey: config.keys.public_key().to_string(),
        contact: "admin@example.com".to_string(),
        supported_nips: vec![1, 9, 50],
        software: "relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Create spam filter with blocked words
    let spam_filter = SpamFilterProcessor {
        blocked_words: vec![
            "spam".to_string(),
            "viagra".to_string(),
            "casino".to_string(),
            "forex".to_string(),
        ],
    };

    // Build relay with our custom processor
    let handler = RelayBuilder::new(config)
        .with_event_processor(spam_filter)
        .with_relay_info(relay_info)
        .build_axum()
        .await?;

    // Create HTTP server with the relay handler
    let app = Router::new().route("/", get(handler));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;

    println!("🚀 Spam Filter Relay running at:");
    println!("📡 WebSocket: ws://localhost:8080");
    println!("🌐 Browser: http://localhost:8080");
    println!("🛡️  Filtering spam with custom EventProcessor");
    println!();
    println!("Try sending events with spam words to see them rejected!");

    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
