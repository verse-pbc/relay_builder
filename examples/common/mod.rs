//! Common utilities for examples to reduce code duplication

#![allow(dead_code)]

use anyhow::Result;
use axum::{
    response::{Html, IntoResponse, Response},
    Router,
};
use nostr_sdk::prelude::*;
use relay_builder::{RelayInfo, WebSocketConfig};
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use websocket_builder::{
    handle_upgrade_with_config, ConnectionConfig, HandlerFactory, WebSocketUpgrade,
};

/// Initialize tracing subscriber for examples
pub fn init_logging() {
    tracing_subscriber::fmt::init();
}

/// Create the root handler for a relay that supports both WebSocket and HTTP
pub fn create_root_handler<F>(
    handler_factory: Arc<F>,
    relay_info: RelayInfo,
) -> impl Fn(
    Option<WebSocketUpgrade>,
    axum::extract::ConnectInfo<SocketAddr>,
    axum::http::HeaderMap,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>>
       + Clone
       + Send
       + 'static
where
    F: HandlerFactory + Send + Sync + 'static,
{
    create_root_handler_with_config(handler_factory, relay_info, WebSocketConfig::default())
}

/// Create the root handler with WebSocket configuration
pub fn create_root_handler_with_config<F>(
    handler_factory: Arc<F>,
    relay_info: RelayInfo,
    ws_config: WebSocketConfig,
) -> impl Fn(
    Option<WebSocketUpgrade>,
    axum::extract::ConnectInfo<SocketAddr>,
    axum::http::HeaderMap,
) -> std::pin::Pin<Box<dyn std::future::Future<Output = Response> + Send>>
       + Clone
       + Send
       + 'static
where
    F: HandlerFactory + Send + Sync + 'static,
{
    // Convert WebSocketConfig to websocket_builder's ConnectionConfig
    let connection_config = ConnectionConfig {
        max_connections: ws_config.max_connections,
        max_connection_duration: ws_config.max_connection_duration.map(Duration::from_secs),
        idle_timeout: ws_config.idle_timeout.map(Duration::from_secs),
    };

    move |ws: Option<WebSocketUpgrade>,
          axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<SocketAddr>,
          headers: axum::http::HeaderMap| {
        let handler_factory = handler_factory.clone();
        let relay_info = relay_info.clone();
        let connection_config = connection_config.clone();

        Box::pin(async move {
            match ws {
                Some(ws) => {
                    // Handle WebSocket upgrade with config
                    let handler = handler_factory.create(&headers);
                    handle_upgrade_with_config(ws, addr, handler, connection_config)
                        .await
                        .into_response()
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
                    Html(relay_builder::handlers::default_relay_html(&relay_info)).into_response()
                }
            }
        })
    }
}

/// Run the relay server on the specified address
pub async fn run_relay_server(app: Router, addr: SocketAddr, name: &str) -> Result<()> {
    println!("🚀 {name} listening on: {addr}");
    println!("📡 WebSocket endpoint: ws://{addr}");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;

    Ok(())
}

/// Create a standard RelayInfo structure
pub fn create_relay_info(
    name: &str,
    description: &str,
    public_key: PublicKey,
    supported_nips: Vec<u16>,
) -> RelayInfo {
    RelayInfo {
        name: name.to_string(),
        description: description.to_string(),
        pubkey: public_key.to_hex(),
        contact: format!("admin@{}.relay", name.to_lowercase().replace(' ', "-")),
        supported_nips,
        software: "relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: None,
    }
}
