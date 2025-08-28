//! WebSocket handling functionality
//!
//! This module provides a minimal abstraction over WebSocket connections:
//! - Simple trait-based handler with just 3 methods
//! - Per-connection handler instances for natural state management
//! - Automatic handling of WebSocket protocol details
//! - Easy integration with Axum

mod axum_integration;
mod connection;
mod handler;

// Re-export the public API
pub use axum_integration::{
    handle_upgrade, handle_upgrade_with_config, websocket_route, websocket_route_with_config,
    WebSocketUpgrade,
};
pub use connection::{handle_socket, ConnectionConfig};
pub use handler::{DisconnectReason, HandlerFactory, Utf8Bytes, WebSocketHandler};
