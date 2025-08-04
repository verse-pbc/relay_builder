//! Event processing - demonstrates custom business logic with EventProcessor
//!
//! This example shows how to use EventProcessor to implement custom rules
//! for accepting or rejecting events. We demonstrate:
//! - Content filtering (spam detection)
//! - Rate limiting per public key
//! - Custom rejection messages
//!
//! Run with: cargo run --example 02_event_processing --features axum

mod common;

use anyhow::Result;
use axum::{routing::get, Router};
use nostr_sdk::prelude::*;
use relay_builder::{
    EventContext, EventProcessor, RelayBuilder, RelayConfig, Result as RelayResult, StoreCommand,
};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

/// Track rate limit info per public key
#[derive(Debug, Clone)]
struct RateLimitInfo {
    events_received: u32,
    window_start: Instant,
}

/// Event processor with spam filtering and rate limiting
#[derive(Debug, Clone)]
struct SmartEventProcessor {
    blocked_words: Vec<String>,
    max_events_per_minute: u32,
    rate_limits: Arc<Mutex<HashMap<String, RateLimitInfo>>>,
}

impl SmartEventProcessor {
    fn new(blocked_words: Vec<String>, max_events_per_minute: u32) -> Self {
        Self {
            blocked_words,
            max_events_per_minute,
            rate_limits: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl EventProcessor for SmartEventProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<parking_lot::RwLock<()>>,
        context: EventContext<'_>,
    ) -> RelayResult<Vec<StoreCommand>> {
        // First check rate limit
        let pubkey = event.pubkey.to_string();
        let mut rate_limits = self.rate_limits.lock().await;
        let now = Instant::now();

        let info = rate_limits
            .entry(pubkey.clone())
            .or_insert_with(|| RateLimitInfo {
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
                "Rate limit exceeded for pubkey {}: {} events in current window",
                event.pubkey,
                info.events_received
            );
            return Err(relay_builder::Error::restricted(
                "rate limit exceeded - slow down!",
            ));
        }

        // Save events_received for later use
        let events_received = info.events_received;

        // Release the lock before content filtering
        drop(rate_limits);

        // Content filtering
        let content_lower = event.content.to_lowercase();

        // Check for blocked words
        for word in &self.blocked_words {
            if content_lower.contains(word) {
                tracing::info!("Rejected spam from {}: contains '{}'", event.pubkey, word);
                return Err(relay_builder::Error::restricted(format!(
                    "content contains blocked word: '{word}'"
                )));
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
            return Err(relay_builder::Error::restricted(
                "too many mentions - potential spam",
            ));
        }

        // Event passed all checks - accept it
        tracing::debug!(
            "Accepted event from {} (rate: {}/min)",
            event.pubkey,
            events_received
        );

        Ok(vec![(event, context.subdomain.clone()).into()])
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    common::init_logging();

    // Create relay configuration
    let relay_url = "ws://localhost:8080";
    let keys = Keys::generate();
    let config =
        RelayConfig::new(relay_url, "./data/event_processing", keys).with_max_connections(100);

    // Relay information for NIP-11
    let relay_info = common::create_relay_info(
        "Smart Event Processing Relay",
        "A relay with spam filtering and rate limiting",
        config.keys.public_key(),
        vec![1, 9, 11, 50],
    );

    // Create processor with spam filter and rate limiter
    let processor = SmartEventProcessor::new(
        vec![
            "spam".to_string(),
            "viagra".to_string(),
            "casino".to_string(),
            "forex".to_string(),
        ],
        10, // 10 events per minute per pubkey
    );

    // Build relay with our custom processor
    let handler_factory = Arc::new(
        RelayBuilder::<()>::new(config)
            .event_processor(processor)
            .build()
            .await?,
    );

    // Create the root handler
    let root_handler = common::create_root_handler(handler_factory, relay_info);

    // Create HTTP server with the relay handler
    let app = Router::new().route("/", get(root_handler));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));

    println!("Features:");
    println!("🛡️  Spam filtering with blocked words");
    println!("⚡ Rate limiting: 10 events per minute per public key");
    println!("📝 Detailed rejection messages");
    println!();
    println!("Try sending events with spam words or too fast to see rejections!");

    common::run_relay_server(app, addr, "Smart Event Processing Relay").await
}
