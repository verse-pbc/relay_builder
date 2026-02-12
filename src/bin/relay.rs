//! Batteries-included Nostr relay binary
//!
//! A pre-built, configurable relay that can be run directly via Docker or cargo.
//! All configuration is done via environment variables.

#![recursion_limit = "256"]

#[cfg(all(not(target_env = "musl"), feature = "jemalloc"))]
use tikv_jemallocator::Jemalloc;

#[cfg(all(not(target_env = "musl"), feature = "jemalloc"))]
#[global_allocator]
static GLOBAL: Jemalloc = Jemalloc;

use anyhow::Result;
use axum::{
    extract::State,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use governor::Quota;
use nostr_sdk::prelude::*;
use relay_builder::middlewares::RateLimitMiddleware;
use relay_builder::{
    handle_upgrade_with_config, ConnectionConfig, HandlerFactory, Nip40ExpirationMiddleware,
    Nip70Middleware, RelayBuilder, RelayConfig, RelayInfo, WebSocketConfig, WebSocketUpgrade,
};
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

fn env_parse<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

async fn health(State(counter): State<Arc<AtomicUsize>>) -> impl IntoResponse {
    let connections = counter.load(Ordering::Relaxed);
    axum::Json(serde_json::json!({
        "status": "ok",
        "active_connections": connections,
    }))
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    // Read configuration from environment
    let port: u16 = env_parse("RELAY_PORT", 8080);
    let bind = env_or("RELAY_BIND", "0.0.0.0");
    let data_dir = env_or("RELAY_DATA_DIR", "./data");
    let relay_url = env_or("RELAY_URL", &format!("ws://localhost:{port}"));
    let relay_name = env_or("RELAY_NAME", "Nostr Relay");
    let relay_description = env_or("RELAY_DESCRIPTION", "A relay built with relay_builder");
    let relay_contact = env_or("RELAY_CONTACT", "");
    let relay_icon = std::env::var("RELAY_ICON").ok().filter(|s| !s.is_empty());

    let max_connections: usize = env_parse("MAX_CONNECTIONS", 1000);
    let max_subscriptions: usize = env_parse("MAX_SUBSCRIPTIONS", 100);
    let max_subscription_limit: usize = env_parse("MAX_SUBSCRIPTION_LIMIT", 5000);
    let max_connection_duration: u64 = env_parse("MAX_CONNECTION_DURATION", 3600);
    let idle_timeout: u64 = env_parse("IDLE_TIMEOUT", 300);
    let rate_limit_per_second: u32 = env_parse("RATE_LIMIT_PER_SECOND", 10);
    let rate_limit_global: u32 = env_parse("RATE_LIMIT_GLOBAL", 0);

    // Generate relay keys
    let keys = Keys::generate();

    // Build relay configuration
    let ws_config = WebSocketConfig {
        max_connections: Some(max_connections),
        max_connection_duration: if max_connection_duration > 0 {
            Some(max_connection_duration)
        } else {
            None
        },
        idle_timeout: if idle_timeout > 0 {
            Some(idle_timeout)
        } else {
            None
        },
    };

    let mut config = RelayConfig::new(&relay_url, data_dir.as_str(), keys.clone());
    config.enable_auth = true;
    config = config
        .with_websocket_config(ws_config.clone())
        .with_subscription_limits(max_subscriptions, max_subscription_limit);

    // Relay information for NIP-11
    let relay_info = RelayInfo {
        name: relay_name.clone(),
        description: relay_description.clone(),
        pubkey: keys.public_key().to_hex(),
        contact: relay_contact.clone(),
        supported_nips: vec![1, 9, 11, 40, 42, 70],
        software: "relay_builder".to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        icon: relay_icon,
    };

    // Production components
    let shutdown_token = CancellationToken::new();
    let task_tracker = TaskTracker::new();
    let connection_counter = Arc::new(AtomicUsize::new(0));

    // Build rate limiter (use u32::MAX/s to effectively disable when set to 0)
    let effective_rate = if rate_limit_per_second > 0 {
        rate_limit_per_second
    } else {
        u32::MAX
    };
    let per_conn_quota = Quota::per_second(NonZeroU32::new(effective_rate).expect("non-zero"));
    let rate_limiter = if rate_limit_global > 0 {
        let global_quota = Quota::per_second(NonZeroU32::new(rate_limit_global).expect("non-zero"));
        RateLimitMiddleware::<()>::with_global_limit(per_conn_quota, global_quota)
    } else {
        RateLimitMiddleware::<()>::new(per_conn_quota)
    };

    // Build the relay with all middleware
    let handler_factory = Arc::new(
        RelayBuilder::<()>::new(config)
            .task_tracker(task_tracker.clone())
            .cancellation_token(shutdown_token.clone())
            .connection_counter(connection_counter.clone())
            .build_with(|chain| {
                chain
                    .with(Nip40ExpirationMiddleware::new())
                    .with(Nip70Middleware)
                    .with(rate_limiter)
            })
            .await?,
    );

    // Convert WebSocketConfig to ConnectionConfig
    let connection_config = ConnectionConfig {
        max_connections: ws_config.max_connections,
        max_connection_duration: ws_config.max_connection_duration.map(Duration::from_secs),
        idle_timeout: ws_config.idle_timeout.map(Duration::from_secs),
    };

    // Create root handler: WebSocket + NIP-11 + HTML
    let relay_info_for_handler = relay_info.clone();
    let ws_handler = move |ws: Option<WebSocketUpgrade>,
                           axum::extract::ConnectInfo(addr): axum::extract::ConnectInfo<
        SocketAddr,
    >,
                           headers: axum::http::HeaderMap| {
        let handler_factory = handler_factory.clone();
        let relay_info = relay_info_for_handler.clone();
        let connection_config = connection_config.clone();

        async move {
            match ws {
                Some(ws) => {
                    let handler = handler_factory.create(&headers);
                    handle_upgrade_with_config(ws, addr, handler, connection_config).await
                }
                None => {
                    if let Some(accept) = headers.get(axum::http::header::ACCEPT) {
                        if let Ok(value) = accept.to_str() {
                            if value == "application/nostr+json" {
                                return axum::Json(&relay_info).into_response();
                            }
                        }
                    }
                    Html(relay_builder::handlers::default_relay_html(&relay_info)).into_response()
                }
            }
        }
    };

    // Create HTTP server with health endpoint
    let app = Router::new()
        .route("/", get(ws_handler))
        .route("/health", get(health))
        .with_state(connection_counter.clone());

    let addr: SocketAddr = format!("{bind}:{port}").parse()?;
    let listener = tokio::net::TcpListener::bind(addr).await?;

    tracing::info!("Relay '{}' listening on {}", relay_name, addr);
    tracing::info!("WebSocket: {}", relay_url);
    tracing::info!("Health: http://{}/health", addr);
    tracing::info!(
        "Max connections: {}, Max subscriptions: {}, Rate limit: {}/s",
        max_connections,
        max_subscriptions,
        if rate_limit_per_second > 0 {
            rate_limit_per_second.to_string()
        } else {
            "unlimited".to_string()
        }
    );

    // Graceful shutdown
    let shutdown_handle = shutdown_token.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c()
            .await
            .expect("Failed to install signal handler");
        tracing::info!("Shutdown signal received, closing connections...");
        shutdown_handle.cancel();
    });

    let server = axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        shutdown_token.cancelled().await;
    });

    server.await?;

    task_tracker.close();
    task_tracker.wait().await;

    tracing::info!("All tasks completed. Goodbye!");
    Ok(())
}
