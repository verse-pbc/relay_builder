//! Protocol features - demonstrates authentication and NIPs support
//!
//! This example shows multiple protocol features working together:
//! - NIP-42: Authentication (required for posting)
//! - NIP-40: Event expiration (built into EventProcessor)
//! - NIP-70: Protected events (auth required to read)
//!
//! Run with: cargo run --example 03_protocol_features --features axum

mod common;

use anyhow::Result;
use axum::{routing::get, Router};
use nostr_sdk::prelude::*;
use relay_builder::{
    Error, EventContext, EventProcessor, RelayBuilder, RelayConfig, Result as RelayResult,
    StoreCommand,
};
use std::net::SocketAddr;
use std::sync::Arc;

/// Protocol-aware event processor
#[derive(Debug, Clone)]
struct ProtocolProcessor {
    require_auth_to_post: bool,
}

impl EventProcessor for ProtocolProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<parking_lot::RwLock<()>>,
        context: &EventContext,
    ) -> RelayResult<Vec<StoreCommand>> {
        // NIP-42: Check authentication for posting
        if self.require_auth_to_post && context.authed_pubkey.is_none() {
            tracing::info!("Rejected event from unauthenticated user");
            return Err(Error::restricted(
                "authentication required: please AUTH first",
            ));
        }

        // NIP-40: Check event expiration
        if let Some(expiration_tag) = event
            .tags
            .iter()
            .find(|tag| matches!(tag.kind(), TagKind::Custom(name) if name == "expiration"))
            .and_then(|tag| tag.content())
        {
            if let Ok(expiration) = expiration_tag.parse::<u64>() {
                let now = Timestamp::now().as_u64();
                if expiration < now {
                    tracing::info!(
                        "Rejected expired event {} (expired at {})",
                        event.id,
                        expiration
                    );
                    return Err(Error::restricted("event has expired"));
                }
                tracing::info!("Event {} expires at {}", event.id, expiration);
            }
        }

        // NIP-70: Note about protected events
        // Protected events are handled at the filter level, not here
        // The relay automatically requires auth to read events with "-" tags
        let is_protected = event
            .tags
            .iter()
            .any(|tag| matches!(tag.kind(), TagKind::Custom(name) if name == "-"));

        if is_protected {
            tracing::info!(
                "Storing protected event {} (requires auth to read)",
                event.id
            );
        }

        // Log who posted
        if let Some(pubkey) = &context.authed_pubkey {
            tracing::info!(
                "Accepted event {} from authenticated user: {}",
                event.id,
                pubkey
            );
        } else {
            tracing::info!("Accepted event {} from anonymous user", event.id);
        }

        // Store the event
        Ok(vec![(event, (*context.subdomain).clone()).into()])
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    common::init_logging();

    // Create relay configuration with auth enabled
    let relay_url = "ws://localhost:8081";
    let keys = Keys::generate();
    let mut config = RelayConfig::new(relay_url, "./data/protocol_features", keys);

    // Enable authentication - this automatically adds Nip42Middleware
    config.enable_auth = true;

    // Relay information for NIP-11
    let relay_info = common::create_relay_info(
        "Protocol Features Relay",
        "Demonstrates authentication, expiration, and protected events",
        config.keys.public_key(),
        vec![1, 9, 11, 40, 42, 50, 70], // All supported NIPs
    );

    // Build relay with protocol processor
    let handler_factory = Arc::new(
        RelayBuilder::<()>::new(config)
            .event_processor(ProtocolProcessor {
                require_auth_to_post: true,
            })
            .build()
            .await?,
    );

    // Create the root handler
    let root_handler = common::create_root_handler(handler_factory, relay_info);

    // Create HTTP server
    let app = Router::new().route("/", get(root_handler));

    let addr = SocketAddr::from(([127, 0, 0, 1], 8081));

    println!("Protocol features:");
    println!("🔐 NIP-42: Authentication required to post");
    println!("⏰ NIP-40: Event expiration support");
    println!("🔒 NIP-70: Protected events (auth required to read)");
    println!();
    println!("Try:");
    println!("1. Connect and authenticate (AUTH flow)");
    println!("2. Send event with expiration: [\"expiration\", \"<timestamp>\"]");
    println!("3. Send protected event with [\"-\"] tag");
    println!("4. Try reading protected events without auth");

    common::run_relay_server(app, addr, "Protocol Features Relay").await
}
