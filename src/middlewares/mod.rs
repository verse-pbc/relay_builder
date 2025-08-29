//! Protocol and utility middlewares for Nostr relays

mod error_handling;
mod logger;
mod metrics;
mod nip40_expiration;
mod nip42_auth;
mod nip70_protected;
mod rate_limit;

pub use error_handling::{ClientMessageId, ErrorHandlingMiddleware};
pub use logger::LoggerMiddleware;
pub use logger::LoggerMiddleware as NostrLoggerMiddleware; // Alias for backward compatibility
pub use metrics::{MetricsHandler, MetricsMiddleware};
pub use nip40_expiration::Nip40ExpirationMiddleware;
pub use nip42_auth::{AuthConfig, Nip42Middleware};
pub use nip70_protected::Nip70Middleware;
pub use rate_limit::RateLimitMiddleware;
