//! Minimal Nostr relay - the simplest possible relay that accepts all events
//!
//! This example shows the absolute minimum code needed to run a functional Nostr relay.
//! It accepts all events without any filtering or authentication.
//!
//! Run with: cargo run --example 01_minimal_relay

use anyhow::Result;
use axum::{
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use nostr_sdk::prelude::*;
use relay_builder::{RelayBuilder, RelayConfig, RelayInfo};
use std::net::SocketAddr;
use std::sync::Arc;
use websocket_builder::{handle_upgrade, HandlerFactory, WebSocketUpgrade};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logging
    tracing_subscriber::fmt::init();

    // Create relay configuration
    let relay_url = "ws://localhost:8080";
    let db_path = "./minimal_relay_db";
    let relay_keys = Keys::generate();
    let config = RelayConfig::new(relay_url, db_path, relay_keys);

    // Create relay info for NIP-11
    let relay_info = RelayInfo {
        name: "Minimal Relay".to_string(),
        description: "A minimal Nostr relay that accepts all events".to_string(),
        pubkey: config.keys.public_key().to_hex(),
        contact: "admin@minimal.relay".to_string(),
        supported_nips: vec![1, 9, 50],
        software: "relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    };

    // Build the relay handler factory
    let handler_factory = Arc::new(RelayBuilder::<()>::new(config.clone()).build().await?);

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
    println!("🚀 Minimal relay listening on: {addr}");
    println!("📡 WebSocket endpoint: ws://localhost:8080");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}
