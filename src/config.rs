//! Configuration options for the relay builder

use crate::crypto_helper::CryptoHelper;
use crate::database::RelayDatabase;
use crate::error::Error;
use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use std::sync::Arc;

/// Configuration for scope/subdomain handling
#[derive(Debug, Clone)]
pub enum ScopeConfig {
    /// No scope/subdomain support - all data stored in default scope
    Disabled,
    /// Enable subdomain extraction with specified base domain parts
    Subdomain {
        /// Number of parts that constitute the base domain
        /// For example, with 2: "sub.example.com" -> base is "example.com"
        base_domain_parts: usize,
    },
    /// Use a fixed scope for all connections
    Fixed {
        /// The scope to use for all connections
        scope: Scope,
    },
}

impl Default for ScopeConfig {
    fn default() -> Self {
        Self::Disabled
    }
}

impl ScopeConfig {
    /// Create a subdomain-based scope configuration
    pub fn subdomain(base_domain_parts: usize) -> Self {
        Self::Subdomain { base_domain_parts }
    }

    /// Create a fixed scope configuration
    pub fn fixed(scope: Scope) -> Self {
        Self::Fixed { scope }
    }

    /// Resolve a scope from a host string
    pub fn resolve_scope(&self, host: Option<&str>) -> Scope {
        match self {
            Self::Disabled => Scope::Default,
            Self::Subdomain { base_domain_parts } => host
                .and_then(|h| crate::subdomain::extract_subdomain(h, *base_domain_parts))
                .and_then(|s| {
                    if !s.is_empty() {
                        Scope::named(&s).ok()
                    } else {
                        None
                    }
                })
                .unwrap_or(Scope::Default),
            Self::Fixed { scope } => scope.clone(),
        }
    }
}

/// WebSocket server configuration
#[derive(Debug, Clone, Default)]
pub struct WebSocketConfig {
    /// Maximum number of concurrent connections
    pub max_connections: Option<usize>,
    /// Maximum connection time in seconds
    pub max_connection_time: Option<u64>,
}

/// Database configuration - either a path or an existing database instance
#[derive(Debug, Clone)]
pub enum DatabaseConfig {
    /// Create a new database at the specified path
    Path(String),
    /// Use an existing database instance
    Instance(Arc<RelayDatabase>),
}

impl From<String> for DatabaseConfig {
    fn from(path: String) -> Self {
        DatabaseConfig::Path(path)
    }
}

impl From<&str> for DatabaseConfig {
    fn from(path: &str) -> Self {
        DatabaseConfig::Path(path.to_string())
    }
}

impl From<Arc<RelayDatabase>> for DatabaseConfig {
    fn from(db: Arc<RelayDatabase>) -> Self {
        DatabaseConfig::Instance(db)
    }
}

/// Main configuration for the relay
#[derive(Debug, Clone)]
pub struct RelayConfig {
    /// URL of the relay (used for NIP-42 auth and other purposes)
    pub relay_url: String,
    /// Database configuration
    pub database: Option<DatabaseConfig>,
    /// Relay keys
    pub keys: Keys,
    /// Scope configuration
    pub scope_config: ScopeConfig,
    /// Whether to enable NIP-42 authentication
    pub enable_auth: bool,
    /// Authentication configuration (if enabled) - temporarily disabled
    // pub auth_config: Option<crate::middlewares::AuthConfig>,
    /// WebSocket server configuration
    pub websocket_config: WebSocketConfig,
    /// Maximum number of active subscriptions per connection
    pub max_subscriptions: usize,
    /// Maximum limit value allowed in subscription filters
    pub max_limit: usize,
}

impl RelayConfig {
    /// Create a new relay configuration
    pub fn new<D: Into<DatabaseConfig>>(
        relay_url: impl Into<String>,
        database: D,
        keys: Keys,
    ) -> Self {
        Self {
            relay_url: relay_url.into(),
            database: Some(database.into()),
            keys,
            scope_config: ScopeConfig::default(),
            enable_auth: false,
            // auth_config: None, // Temporarily disabled
            websocket_config: WebSocketConfig::default(),
            max_subscriptions: 50,
            max_limit: 5000,
        }
    }

    /// Create database instance from configuration with provided keys
    pub fn create_database(
        &self,
        keys: Arc<Keys>,
    ) -> Result<(Arc<RelayDatabase>, CryptoHelper), Error> {
        let crypto_helper = CryptoHelper::new(keys);
        let database = self.create_database_with_tracker(None, None)?;
        Ok((database, crypto_helper))
    }

    /// Create database instance from configuration with optional TaskTracker
    pub fn create_database_with_tracker(
        &self,
        _task_tracker: Option<tokio_util::task::TaskTracker>,
        _cancellation_token: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<Arc<RelayDatabase>, Error> {
        match &self.database {
            Some(DatabaseConfig::Path(path)) => {
                let database = RelayDatabase::new(path)?;
                Ok(Arc::new(database))
            }
            Some(DatabaseConfig::Instance(db)) => {
                // Return the existing database instance
                Ok(db.clone())
            }
            None => Err(Error::internal(
                "Database configuration is required".to_string(),
            )),
        }
    }

    /// Create database instance from a database config with optional TaskTracker
    pub fn create_database_from_config(
        database_config: DatabaseConfig,
        _websocket_config: &WebSocketConfig,
        _max_subscriptions: usize,
        _task_tracker: Option<tokio_util::task::TaskTracker>,
        _cancellation_token: Option<tokio_util::sync::CancellationToken>,
    ) -> Result<Arc<RelayDatabase>, Error> {
        match database_config {
            DatabaseConfig::Path(path) => {
                let database = RelayDatabase::new(&path)?;
                Ok(Arc::new(database))
            }
            DatabaseConfig::Instance(db) => {
                // Return the existing database instance
                Ok(db)
            }
        }
    }

    /// Set the scope configuration
    pub fn with_scope_config(mut self, scope_config: ScopeConfig) -> Self {
        self.scope_config = scope_config;
        self
    }

    /// Enable subdomain-based scopes
    pub fn with_subdomains(mut self, base_domain_parts: usize) -> Self {
        self.scope_config = ScopeConfig::subdomain(base_domain_parts);
        self
    }

    /// Enable subdomain-based scopes, automatically determining base domain parts from URL
    pub fn with_subdomains_from_url(mut self, relay_url: &str) -> Self {
        // Extract host from URL
        let host = url::Url::parse(relay_url)
            .ok()
            .and_then(|u| u.host_str().map(|s| s.to_string()))
            .unwrap_or_default();

        // Count the number of parts in the base domain
        let base_domain_parts =
            if host.is_empty() || host == "localhost" || host.parse::<std::net::IpAddr>().is_ok() {
                2 // Default for invalid hosts
            } else {
                host.split('.').count()
            };

        self.scope_config = ScopeConfig::subdomain(base_domain_parts);
        self
    }

    // Enable NIP-42 authentication - temporarily disabled
    // pub fn with_auth(mut self, auth_config: crate::middlewares::AuthConfig) -> Self {
    //     self.enable_auth = true;
    //     self.auth_config = Some(auth_config);
    //     self
    // }

    /// Configure WebSocket server settings
    pub fn with_websocket_config(mut self, config: WebSocketConfig) -> Self {
        self.websocket_config = config;
        self
    }

    /// Set the maximum number of concurrent connections
    pub fn with_max_connections(mut self, max: usize) -> Self {
        self.websocket_config.max_connections = Some(max);
        self
    }

    /// Set the maximum connection time in seconds
    pub fn with_max_connection_time(mut self, seconds: u64) -> Self {
        self.websocket_config.max_connection_time = Some(seconds);
        self
    }

    /// Set the maximum number of active subscriptions per connection
    pub fn with_max_subscriptions(mut self, max_subscriptions: usize) -> Self {
        self.max_subscriptions = max_subscriptions;
        self
    }

    /// Set the maximum limit value allowed in subscription filters
    pub fn with_max_limit(mut self, max_limit: usize) -> Self {
        self.max_limit = max_limit;
        self
    }

    /// Set max_subscriptions and max_limit
    pub fn with_subscription_limits(mut self, max_subscriptions: usize, max_limit: usize) -> Self {
        self.max_subscriptions = max_subscriptions;
        self.max_limit = max_limit;
        self
    }

    /// Calculate the WebSocket channel size based on configuration
    /// This is used for per-connection MessageSender channels
    pub fn calculate_channel_size(&self) -> usize {
        // We need to handle the worst case: a single subscription requesting max_limit events
        // Plus overhead for control messages (EOSE, notices, etc.)
        let overhead = 5; // Space for control messages
        let limit_based_size = self.max_limit + overhead;

        self.max_subscriptions * limit_based_size
    }
}

// No Default implementation as RelayConfig requires Keys to be explicitly provided
