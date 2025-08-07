//! A framework for building custom Nostr relays with middleware support
//!
//! This crate provides the building blocks for creating Nostr relays with:
//! - Middleware-based message processing
//! - Pluggable business logic via the `EventProcessor` trait
//! - Built-in protocol support (NIPs 09, 40, 42, 70)
//! - WebSocket connection management
//! - Database abstraction
#![allow(clippy::manual_async_fn)]
// Performance-focused clippy lints
#![warn(
    clippy::perf,
    clippy::redundant_clone,
    clippy::needless_pass_by_value,
    clippy::inefficient_to_string,
    clippy::clone_on_copy
)]
#![allow(clippy::enum_variant_names, clippy::needless_borrow)]

pub mod config;
pub mod crypto_helper;
pub mod database;
pub mod error;
pub mod event_ingester;
pub mod event_processor;
pub mod global_metrics;

pub mod handlers;
pub mod metrics;
pub mod middlewares;
pub mod relay_builder;
pub mod relay_middleware;
pub mod state;
pub mod subdomain;
pub mod subscription_coordinator;
pub mod subscription_index;
pub mod subscription_registry;
#[cfg(test)]
pub mod test_utils;
pub mod util;
pub mod utils;
// Temporarily disabled until updated to new middleware API
// pub mod relay_builder_v2;
// pub mod relay_builder_simple;
pub mod middleware_chain;
pub mod nostr_handler;
pub mod nostr_middleware;
#[cfg(test)]
pub mod test_middleware_system;
#[cfg(test)]
pub mod test_simple_middleware;

pub use config::{RelayConfig, ScopeConfig, WebSocketConfig};
pub use crypto_helper::CryptoHelper;
pub use database::RelayDatabase;
pub use error::{Error, Result};
pub use event_ingester::{EventIngester, IngesterError};
pub use event_processor::{DefaultRelayProcessor, EventContext, EventProcessor};

pub use handlers::{RelayInfo, RelayService};

pub use relay_builder::HtmlOption;
pub use relay_builder::RelayBuilder;
pub use relay_middleware::RelayMiddleware;
pub use state::{DefaultNostrConnectionState, NostrConnectionState};
pub use subscription_coordinator::{StoreCommand, SubscriptionCoordinator};
pub use subscription_registry::{EventDistributor, SubscriptionRegistry};

// Re-export commonly used middlewares
pub use middlewares::{
    AuthConfig, ClientMessageId, ErrorHandlingMiddleware, MetricsHandler, MetricsMiddleware,
    Nip40ExpirationMiddleware, Nip42Middleware, Nip70Middleware, NostrLoggerMiddleware,
};

// Re-export new nostr-specific middleware system
pub use middleware_chain::{chain, Chain};
pub use nostr_handler::{IntoHandlerFactory, NostrHandlerFactory};
pub use nostr_middleware::{MessageSender, NostrMiddleware};

// Re-export websocket_builder types to avoid version conflicts
// pub use websocket_builder::MessageSender;  // Disabled - using NostrMessageSender instead

pub use websocket_builder::WebSocketUpgrade;
