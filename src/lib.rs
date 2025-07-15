//! A framework for building custom Nostr relays with middleware support
//!
//! This crate provides the building blocks for creating Nostr relays with:
//! - Middleware-based message processing
//! - Pluggable business logic via the EventProcessor trait
//! - Built-in protocol support (NIPs 09, 40, 42, 70)
//! - WebSocket connection management
//! - Database abstraction

pub mod config;
pub mod crypto_helper;
pub mod database;
pub mod error;
pub mod event_processor;
pub mod global_metrics;
#[cfg(feature = "axum")]
pub mod handlers;
pub mod message_converter;
pub mod metrics;
pub mod middlewares;
pub mod relay_builder;
pub mod relay_middleware;
pub mod state;
pub mod subdomain;
pub mod subscription_coordinator;
pub mod subscription_registry;
#[cfg(test)]
pub mod test_utils;
pub mod utils;

pub use config::{RelayConfig, ScopeConfig, WebSocketConfig};
pub use crypto_helper::CryptoHelper;
pub use database::RelayDatabase;
pub use error::{Error, Result};
pub use event_processor::{DefaultRelayProcessor, EventContext, EventProcessor};
#[cfg(feature = "axum")]
pub use handlers::{RelayInfo, RelayService};

pub use message_converter::NostrMessageConverter;
#[cfg(feature = "axum")]
pub use relay_builder::HtmlOption;
pub use relay_builder::{DefaultRelayWebSocketHandler, RelayBuilder, RelayWebSocketHandler};
pub use relay_middleware::RelayMiddleware;
pub use state::{DefaultNostrConnectionState, NostrConnectionState};
pub use subscription_coordinator::{StoreCommand, SubscriptionCoordinator};
pub use subscription_registry::{EventDistributor, SubscriptionRegistry};

// Re-export commonly used middlewares
pub use middlewares::{
    AuthConfig, ClientMessageId, ErrorHandlingMiddleware, EventVerifierMiddleware,
    LoggerMiddleware, Nip40ExpirationMiddleware, Nip42Middleware, Nip70Middleware,
};

// Re-export websocket_builder types to avoid version conflicts
pub use websocket_builder::MessageSender;
#[cfg(feature = "axum")]
pub use websocket_builder::WebSocketUpgrade;
