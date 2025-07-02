//! Connection state management

use crate::config::ScopeConfig;
use crate::database::{DatabaseSender, RelayDatabase};
use crate::error::Error;
use crate::subscription_coordinator::StoreCommand;
use crate::subscription_coordinator::SubscriptionCoordinator;
use crate::subscription_registry::SubscriptionRegistry;
use anyhow::Result;
use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};
use websocket_builder::{MessageSender, StateFactory};

const DEFAULT_RELAY_URL: &str = "wss://default.relay";

tokio::task_local! {
    /// Task-local storage for the current request host.
    /// This is set by the HTTP handler before processing WebSocket connections.
    pub static CURRENT_REQUEST_HOST: Option<String>;
}

/// Type alias for the default NostrConnectionState without custom state
pub type DefaultNostrConnectionState = NostrConnectionState<()>;

/// Connection state for a WebSocket client
#[derive(Debug, Clone)]
pub struct NostrConnectionState<T = ()> {
    /// The relay URL this connection is for
    pub relay_url: RelayUrl,
    /// Challenge for NIP-42 authentication
    pub challenge: Option<String>,
    /// Authenticated public key (if authenticated via NIP-42)
    pub authed_pubkey: Option<PublicKey>,
    /// Subscription coordinator for this connection (private - use methods below)
    subscription_coordinator: Option<SubscriptionCoordinator>,
    /// Database sender for write operations
    pub(crate) db_sender: Option<DatabaseSender>,
    /// Cancellation token for this connection
    pub connection_token: CancellationToken,
    /// Subdomain/scope for this connection
    pub subdomain: Scope,
    /// Custom state for application-specific data
    pub custom: Arc<parking_lot::RwLock<T>>,
    /// Active subscriptions for this connection
    pub(crate) active_subscriptions: std::collections::HashSet<SubscriptionId>,
    /// Maximum number of subscriptions allowed (None means unlimited)
    pub(crate) max_subscriptions: Option<usize>,
}

impl<T: Default> Default for NostrConnectionState<T> {
    fn default() -> Self {
        Self {
            relay_url: RelayUrl::parse(DEFAULT_RELAY_URL).expect("Invalid default relay URL"),
            challenge: None,
            authed_pubkey: None,
            subscription_coordinator: None,
            db_sender: None,
            connection_token: CancellationToken::new(),
            subdomain: Scope::Default,
            custom: Arc::new(parking_lot::RwLock::new(T::default())),
            active_subscriptions: std::collections::HashSet::new(),
            max_subscriptions: None,
        }
    }
}

impl<T: Default> NostrConnectionState<T> {
    /// Create a new connection state
    pub fn new(relay_url: String) -> Result<Self, Error> {
        let relay_url = RelayUrl::parse(&relay_url)
            .map_err(|e| Error::internal(format!("Invalid relay URL: {e}")))?;

        Ok(Self {
            relay_url,
            challenge: None,
            authed_pubkey: None,
            subscription_coordinator: None,
            db_sender: None,
            connection_token: CancellationToken::new(),
            subdomain: Scope::Default,
            custom: Arc::new(parking_lot::RwLock::new(T::default())),
            active_subscriptions: std::collections::HashSet::new(),
            max_subscriptions: None,
        })
    }
}

impl<T> NostrConnectionState<T> {
    /// Create a new connection state with custom data
    pub fn with_custom(relay_url: String, custom: T) -> Result<Self, Error> {
        let relay_url = RelayUrl::parse(&relay_url)
            .map_err(|e| Error::internal(format!("Invalid relay URL: {e}")))?;

        Ok(Self {
            relay_url,
            challenge: None,
            authed_pubkey: None,
            subscription_coordinator: None,
            db_sender: None,
            connection_token: CancellationToken::new(),
            subdomain: Scope::Default,
            custom: Arc::new(parking_lot::RwLock::new(custom)),
            active_subscriptions: std::collections::HashSet::new(),
            max_subscriptions: None,
        })
    }

    /// Check if the connection is authenticated
    pub fn is_authenticated(&self) -> bool {
        self.authed_pubkey.is_some()
    }

    /// Set up the connection with a database and message sender
    pub fn setup_connection(
        &mut self,
        connection_id: String,
        database: Arc<RelayDatabase>,
        registry: Arc<SubscriptionRegistry>,
        sender: MessageSender<RelayMessage<'static>>,
        max_limit: usize,
    ) -> Result<(), Error> {
        debug!("Setting up connection");

        // db_sender should already be set by the connection factory
        let db_sender = self
            .db_sender
            .as_ref()
            .ok_or_else(|| Error::internal("DatabaseSender not set by connection factory"))?
            .clone();

        let metrics_handler = crate::global_metrics::get_subscription_metrics_handler();

        let coordinator = SubscriptionCoordinator::new(
            database,
            db_sender,
            registry,
            connection_id,
            sender,
            self.authed_pubkey,
            self.subdomain.clone(),
            self.connection_token.clone(),
            metrics_handler,
            max_limit,
        );
        self.subscription_coordinator = Some(coordinator);

        debug!("Connection setup complete");
        Ok(())
    }

    /// Save events to the database
    pub async fn save_events(&mut self, events: Vec<StoreCommand>) -> Result<(), Error> {
        let Some(coordinator) = &self.subscription_coordinator else {
            return Err(Error::internal("No subscription coordinator available"));
        };

        for event in events {
            if let Err(e) = coordinator.save_and_broadcast(event, None).await {
                error!("Failed to save event: {}", e);
                return Err(e);
            }
        }

        Ok(())
    }

    /// Save and broadcast a single store command
    pub async fn save_and_broadcast(&self, command: StoreCommand) -> Result<(), Error> {
        let Some(coordinator) = &self.subscription_coordinator else {
            return Err(Error::internal("No subscription coordinator available"));
        };

        coordinator.save_and_broadcast(command, None).await
    }

    /// Remove a subscription
    pub fn remove_subscription(&self, subscription_id: SubscriptionId) -> Result<(), Error> {
        let Some(coordinator) = &self.subscription_coordinator else {
            return Err(Error::internal("No subscription coordinator available"));
        };

        coordinator.remove_subscription(subscription_id)
    }

    /// Get the subscription coordinator (only for internal use)
    pub(crate) fn subscription_coordinator(&self) -> Option<&SubscriptionCoordinator> {
        self.subscription_coordinator.as_ref()
    }

    /// Set the subscription coordinator (only for testing)
    #[cfg(test)]
    pub(crate) fn set_subscription_coordinator(&mut self, coordinator: SubscriptionCoordinator) {
        self.subscription_coordinator = Some(coordinator);
    }

    /// Get or create a challenge for NIP-42 authentication
    pub fn get_challenge_event(&mut self) -> RelayMessage<'static> {
        let challenge = match self.challenge.as_ref() {
            Some(challenge) => challenge.clone(),
            None => {
                let challenge = format!("{}", rand::random::<u64>());
                self.challenge = Some(challenge.clone());
                challenge
            }
        };
        RelayMessage::auth(challenge)
    }

    /// Clean up the connection
    pub fn cleanup(&self) {
        debug!("Cleaning up connection");

        if let Some(subscription_coordinator) = &self.subscription_coordinator {
            subscription_coordinator.cleanup();
        }

        self.connection_token.cancel();
    }

    /// Convert the Scope to an Option<&str> for backward compatibility
    pub fn subdomain_str(&self) -> Option<&str> {
        match &self.subdomain {
            Scope::Named { name, .. } => Some(name),
            Scope::Default => None,
        }
    }

    /// Get subdomain as owned string to avoid borrowing issues
    pub fn subdomain_string(&self) -> Option<String> {
        match &self.subdomain {
            Scope::Named { name, .. } => Some(name.clone()),
            Scope::Default => None,
        }
    }

    /// Get the subdomain scope
    pub fn subdomain(&self) -> &Scope {
        &self.subdomain
    }

    /// Check if adding a subscription would exceed the limit
    pub fn can_add_subscription(&self, subscription_id: &SubscriptionId) -> Result<(), Error> {
        // If already exists, allow replacing
        if self.active_subscriptions.contains(subscription_id) {
            return Ok(());
        }

        // Check limit
        if let Some(max) = self.max_subscriptions {
            if self.active_subscriptions.len() >= max {
                return Err(Error::restricted(format!(
                    "Maximum subscriptions limit reached: {max}"
                )));
            }
        }

        Ok(())
    }

    /// Track a new subscription
    pub fn add_subscription(&mut self, subscription_id: SubscriptionId) {
        self.active_subscriptions.insert(subscription_id);
    }

    /// Remove a subscription
    pub fn remove_tracked_subscription(&mut self, subscription_id: &SubscriptionId) -> bool {
        self.active_subscriptions.remove(subscription_id)
    }

    /// Set the maximum number of subscriptions (mainly for testing)
    pub fn set_max_subscriptions(&mut self, max: Option<usize>) {
        self.max_subscriptions = max;
    }
}

/// Factory for creating NostrConnectionState instances
#[derive(Clone)]
pub struct NostrConnectionFactory<T = ()> {
    relay_url: RelayUrl,
    #[allow(dead_code)]
    database: Arc<RelayDatabase>,
    db_sender: DatabaseSender,
    registry: Arc<SubscriptionRegistry>,
    scope_config: ScopeConfig,
    state_factory: Option<Arc<dyn Fn() -> T + Send + Sync>>,
    max_subscriptions: Option<usize>,
}

/// Type alias for the original non-generic factory
pub type DefaultNostrConnectionFactory = NostrConnectionFactory<()>;

impl<T> NostrConnectionFactory<T> {
    /// Create a new connection factory with a state factory function
    pub fn new_with_factory(
        relay_url: String,
        database: Arc<RelayDatabase>,
        db_sender: DatabaseSender,
        registry: Arc<SubscriptionRegistry>,
        scope_config: ScopeConfig,
        state_factory: Arc<dyn Fn() -> T + Send + Sync>,
        max_subscriptions: Option<usize>,
    ) -> Result<Self, Error> {
        let relay_url = RelayUrl::parse(&relay_url)
            .map_err(|e| Error::internal(format!("Invalid relay URL: {e}")))?;

        Ok(Self {
            relay_url,
            database,
            db_sender,
            registry,
            scope_config,
            state_factory: Some(state_factory),
            max_subscriptions,
        })
    }

    /// Create a new connection factory that uses Default::default() for state
    pub fn new(
        relay_url: String,
        database: Arc<RelayDatabase>,
        db_sender: DatabaseSender,
        registry: Arc<SubscriptionRegistry>,
        scope_config: ScopeConfig,
        max_subscriptions: Option<usize>,
    ) -> Result<Self, Error>
    where
        T: Default + 'static,
    {
        let relay_url = RelayUrl::parse(&relay_url)
            .map_err(|e| Error::internal(format!("Invalid relay URL: {e}")))?;

        Ok(Self {
            relay_url,
            database,
            db_sender,
            registry,
            scope_config,
            state_factory: None,
            max_subscriptions,
        })
    }

    /// Get the subscription registry
    pub fn registry(&self) -> Arc<SubscriptionRegistry> {
        self.registry.clone()
    }
}

impl<T: Send + Sync + 'static> StateFactory<NostrConnectionState<T>> for NostrConnectionFactory<T>
where
    T: Default,
{
    fn create_state(&self, token: CancellationToken) -> NostrConnectionState<T> {
        // Try to get host information from task-local storage
        let host_opt: Option<String> = CURRENT_REQUEST_HOST
            .try_with(|current_host_opt_ref| current_host_opt_ref.clone())
            .unwrap_or_else(|_| {
                warn!(
                    "CURRENT_REQUEST_HOST task_local not found when creating NostrConnectionState."
                );
                None
            });

        // Resolve scope based on configuration
        let subdomain_scope = self.scope_config.resolve_scope(host_opt.as_deref());

        let custom = if let Some(ref factory) = self.state_factory {
            factory()
        } else {
            T::default()
        };

        let mut state = NostrConnectionState::with_custom(self.relay_url.to_string(), custom)
            .unwrap_or_else(|_| panic!("Failed to create NostrConnectionState"));

        state.connection_token = token;
        state.subdomain = subdomain_scope;
        state.db_sender = Some(self.db_sender.clone());
        state.max_subscriptions = self.max_subscriptions;

        state
    }
}
