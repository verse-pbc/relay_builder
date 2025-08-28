//! Common utilities for examples to reduce code duplication

#![allow(dead_code)]

use anyhow::Result;
use axum::{
    response::{Html, IntoResponse, Response},
    Router,
};
use nostr_sdk::prelude::*;
use relay_builder::{cpu_affinity, RelayInfo, WebSocketConfig};
use relay_builder::{
    handle_upgrade_with_config, ConnectionConfig, HandlerFactory, WebSocketUpgrade,
};
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::net::{TcpListener, TcpSocket};
use tokio::task::JoinSet;

/// Initialize tracing subscriber for examples
pub fn init_logging() {
    tracing_subscriber::fmt::init();
}

/// Create a SO_REUSEPORT listener for the given address
fn make_reuseport_listener(addr: SocketAddr) -> io::Result<TcpListener> {
    let socket = if addr.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };

    socket.set_reuseaddr(true)?;

    // Enable SO_REUSEPORT for kernel-level load balancing
    #[cfg(all(
        unix,
        not(target_os = "solaris"),
        not(target_os = "illumos"),
        not(target_os = "cygwin"),
    ))]
    socket.set_reuseport(true)?;

    socket.bind(addr)?;
    socket.listen(1024)
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

/// Run the relay server on the specified address (legacy single-listener version)
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

/// Configuration for high-performance relay server
pub struct ServerConfig {
    /// CPU to pin the database writer thread to (None = auto-select last CPU)
    pub writer_cpu: Option<usize>,
    /// Number of worker threads (None = auto-detect)
    pub worker_threads: Option<usize>,
    /// Enable SO_REUSEPORT for multiple listeners
    pub enable_reuseport: bool,
    /// Enable CPU affinity pinning
    pub enable_cpu_affinity: bool,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            writer_cpu: None,
            worker_threads: None,
            enable_reuseport: true,
            enable_cpu_affinity: true,
        }
    }
}

/// Run high-performance relay server with SO_REUSEPORT and CPU affinity
pub async fn run_relay_server_optimized(
    app: Router,
    addr: SocketAddr,
    name: &str,
    config: ServerConfig,
) -> Result<()> {
    let cpu_count = num_cpus::get();

    // Determine writer CPU (default to last CPU, avoiding CPU 0 which handles more IRQs)
    let writer_cpu = config
        .writer_cpu
        .unwrap_or_else(|| cpu_count.saturating_sub(1));

    // Determine worker threads (all CPUs except writer)
    let worker_threads = config.worker_threads.unwrap_or_else(|| {
        if config.enable_cpu_affinity {
            (cpu_count - 1).max(1)
        } else {
            cpu_count
        }
    });

    println!("🚀 {name} starting with optimized configuration");
    println!("📊 System Configuration:");
    println!("    Total CPUs: {cpu_count}");
    if config.enable_cpu_affinity {
        println!("    Database writer CPU: {writer_cpu}");
        println!("    Worker threads: {worker_threads}");
    }
    println!(
        "    SO_REUSEPORT: {}",
        if config.enable_reuseport {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!(
        "    CPU affinity: {}",
        if config.enable_cpu_affinity {
            "enabled"
        } else {
            "disabled"
        }
    );

    if config.enable_reuseport {
        // Use multiple SO_REUSEPORT listeners for better scalability
        let listener_count = worker_threads.max(1);
        println!("    Creating {listener_count} SO_REUSEPORT listeners on {addr}");

        let mut tasks = JoinSet::new();

        for i in 0..listener_count {
            let listener = make_reuseport_listener(addr)?;
            let app = app.clone();

            tasks.spawn(async move {
                println!("    Listener {i} ready on {addr}");
                axum::serve(
                    listener,
                    app.into_make_service_with_connect_info::<SocketAddr>(),
                )
                .await
                .map_err(|e| anyhow::anyhow!(e))
            });
        }

        println!("📡 WebSocket endpoint: ws://{addr}");
        println!("✨ All listeners ready!");

        // Wait for all listeners
        while let Some(res) = tasks.join_next().await {
            res??;
        }
    } else {
        // Fallback to single listener
        println!("    Using single listener on {addr}");
        println!("📡 WebSocket endpoint: ws://{addr}");

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await?;
    }

    Ok(())
}

/// Create and run a tokio runtime with CPU affinity configuration
pub fn run_with_optimized_runtime<F, Fut>(server_config: ServerConfig, f: F) -> Result<()>
where
    F: FnOnce() -> Fut + Send + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let cpu_count = num_cpus::get();
    let writer_cpu = server_config
        .writer_cpu
        .unwrap_or_else(|| cpu_count.saturating_sub(1));
    let worker_threads = server_config.worker_threads.unwrap_or_else(|| {
        if server_config.enable_cpu_affinity {
            (cpu_count - 1).max(1)
        } else {
            cpu_count
        }
    });

    let mut runtime_builder = tokio::runtime::Builder::new_multi_thread();
    runtime_builder
        .worker_threads(worker_threads)
        .enable_io()
        .enable_time();

    if server_config.enable_cpu_affinity {
        let excluded = vec![writer_cpu];
        runtime_builder.on_thread_start(move || {
            // Set affinity for each worker thread to exclude the writer CPU
            if let Err(e) = cpu_affinity::allow_all_but_current_thread(&excluded) {
                eprintln!("Failed to set worker affinity: {e}");
            }
        });
    }

    let rt = runtime_builder.build()?;
    rt.block_on(f())
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
