//! Connection state management

use crate::database::RelayDatabase;
use crate::error::Error;
use crate::nostr_middleware::MessageSender;
use crate::subscription_coordinator::StoreCommand;
use crate::subscription_coordinator::SubscriptionCoordinator;
use crate::subscription_registry::SubscriptionRegistry;
use anyhow::Result;
use negentropy::{Negentropy, NegentropyStorageVector};
use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

const DEFAULT_RELAY_URL: &str = "wss://default.relay";

/// Type alias for the default NostrConnectionState without custom state
pub type DefaultNostrConnectionState = NostrConnectionState<()>;

/// Connection state for a WebSocket client
#[derive(Debug)]
pub struct NostrConnectionState<T = ()> {
    /// The relay URL this connection is for
    pub relay_url: RelayUrl,
    /// Challenge for NIP-42 authentication
    pub challenge: Option<String>,
    /// Authenticated public key (if authenticated via NIP-42)
    pub authed_pubkey: Option<PublicKey>,
    /// Subscription coordinator for this connection (private - use methods below)
    subscription_coordinator: Option<SubscriptionCoordinator>,
    /// Maximum number of subscriptions allowed (set by the connection factory)
    pub(crate) max_subscriptions: Option<usize>,
    /// Track active subscriptions
    active_subscriptions: std::collections::HashSet<SubscriptionId>,
    /// Active negentropy subscriptions
    negentropy_subscriptions: HashMap<SubscriptionId, Negentropy<'static, NegentropyStorageVector>>,
    /// Connection cancellation token
    pub connection_token: CancellationToken,
    /// Registry for looking up subscriptions (used to set up subscription coordinator)
    pub(crate) registry: Option<Arc<SubscriptionRegistry>>,
    /// The subdomain scope for this connection
    pub subdomain: Arc<Scope>,
    /// Custom state that can be managed by middleware
    pub custom_state: T,
}

impl<T> Default for NostrConnectionState<T>
where
    T: Default,
{
    fn default() -> Self {
        Self {
            relay_url: RelayUrl::parse(DEFAULT_RELAY_URL).expect("Default URL should be valid"),
            challenge: None,
            authed_pubkey: None,
            subscription_coordinator: None,
            max_subscriptions: None,
            active_subscriptions: std::collections::HashSet::new(),
            negentropy_subscriptions: HashMap::new(),
            connection_token: CancellationToken::new(),
            registry: None,
            subdomain: Arc::new(Scope::Default),
            custom_state: T::default(),
        }
    }
}

impl<T> NostrConnectionState<T>
where
    T: Default,
{
    pub fn new(relay_url: RelayUrl) -> Result<Self> {
        Ok(Self {
            relay_url,
            challenge: None,
            authed_pubkey: None,
            subscription_coordinator: None,
            max_subscriptions: None,
            active_subscriptions: std::collections::HashSet::new(),
            negentropy_subscriptions: HashMap::new(),
            connection_token: CancellationToken::new(),
            registry: None,
            subdomain: Arc::new(Scope::Default),
            custom_state: T::default(),
        })
    }

    pub fn with_subdomain(relay_url: RelayUrl, subdomain: Arc<Scope>) -> Result<Self> {
        Ok(Self {
            relay_url,
            challenge: None,
            authed_pubkey: None,
            subscription_coordinator: None,
            max_subscriptions: None,
            active_subscriptions: std::collections::HashSet::new(),
            negentropy_subscriptions: HashMap::new(),
            connection_token: CancellationToken::new(),
            registry: None,
            subdomain,
            custom_state: T::default(),
        })
    }
}

impl<T> Clone for NostrConnectionState<T>
where
    T: Clone,
{
    fn clone(&self) -> Self {
        Self {
            relay_url: self.relay_url.clone(),
            challenge: self.challenge.clone(),
            authed_pubkey: self.authed_pubkey,
            subscription_coordinator: self.subscription_coordinator.clone(),
            max_subscriptions: self.max_subscriptions,
            active_subscriptions: self.active_subscriptions.clone(),
            negentropy_subscriptions: HashMap::new(), // Can't clone Negentropy, so start with empty
            connection_token: self.connection_token.clone(),
            registry: self.registry.clone(),
            subdomain: self.subdomain.clone(),
            custom_state: self.custom_state.clone(),
        }
    }
}

impl<T> NostrConnectionState<T> {
    pub fn with_custom(relay_url: &str, custom_state: T) -> Result<Self, Error> {
        let relay_url =
            RelayUrl::parse(relay_url).map_err(|e| Error::internal(format!("Invalid URL: {e}")))?;

        Ok(Self {
            relay_url,
            challenge: None,
            authed_pubkey: None,
            subscription_coordinator: None,
            max_subscriptions: None,
            active_subscriptions: std::collections::HashSet::new(),
            negentropy_subscriptions: HashMap::new(),
            connection_token: CancellationToken::new(),
            registry: None,
            subdomain: Arc::new(Scope::Default),
            custom_state,
        })
    }

    pub fn set_authenticated(&mut self, pubkey: PublicKey) {
        self.authed_pubkey = Some(pubkey);
        self.challenge = None; // Clear challenge after successful auth
    }

    pub fn is_authenticated(&self) -> bool {
        self.authed_pubkey.is_some()
    }

    /// Setup the connection with database and registry
    pub fn setup_connection(
        &mut self,
        database: Arc<RelayDatabase>,
        registry: Arc<SubscriptionRegistry>,
        connection_id: String,
        sender: MessageSender,
        crypto_helper: crate::crypto_helper::CryptoHelper,
        max_limit: Option<usize>,
        replaceable_event_queue: flume::Sender<(UnsignedEvent, Scope)>,
    ) -> Result<(), Error> {
        debug!("Setting up connection for {}", connection_id);

        let metrics_handler = crate::global_metrics::get_subscription_metrics_handler();

        let coordinator = SubscriptionCoordinator::new(
            database,
            crypto_helper,
            registry,
            connection_id,
            sender,
            self.authed_pubkey,
            self.subdomain.clone(),
            metrics_handler,
            max_limit.unwrap_or(1000), // Default to 1000 if not specified
            replaceable_event_queue,
        );
        self.subscription_coordinator = Some(coordinator);

        debug!("Connection setup complete");
        Ok(())
    }

    pub async fn save_events(&mut self, store_commands: Vec<StoreCommand>) -> Result<(), Error> {
        let Some(coordinator) = &self.subscription_coordinator else {
            return Err(Error::internal("No subscription coordinator available"));
        };

        for store_command in store_commands {
            if let Err(e) = coordinator.save_and_broadcast(store_command).await {
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

        coordinator.save_and_broadcast(command).await
    }

    /// Remove a subscription
    pub fn remove_subscription(&self, subscription_id: &SubscriptionId) -> Result<(), Error> {
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

        // Note: negentropy_subscriptions will be automatically cleaned up when the connection is dropped
        // since they are stored in the connection state
    }

    /// Check if we can accept more subscriptions
    pub fn can_accept_subscription(&self) -> bool {
        if let Some(max) = self.max_subscriptions {
            self.active_subscriptions.len() < max
        } else {
            true
        }
    }

    /// Get count of active subscriptions
    pub fn subscription_count(&self) -> usize {
        self.active_subscriptions.len()
    }

    /// Get remaining subscription capacity
    pub fn remaining_subscription_capacity(&self) -> Option<usize> {
        self.max_subscriptions
            .map(|max| max.saturating_sub(self.active_subscriptions.len()))
    }

    /// Try to add a subscription, returns error if limit exceeded
    pub fn try_add_subscription(&mut self, subscription_id: &SubscriptionId) -> Result<(), Error> {
        if !self.can_accept_subscription() {
            let count = self.active_subscriptions.len();
            let max = self.max_subscriptions.unwrap_or(0);
            return Err(Error::internal(format!(
                "Subscription limit exceeded: {count}/{max} active subscriptions"
            )));
        }

        self.add_subscription(subscription_id.clone());

        // Double-check we haven't exceeded the limit
        if let Some(max) = self.max_subscriptions {
            if self.active_subscriptions.len() > max {
                self.remove_tracked_subscription(&subscription_id);
                let count = self.active_subscriptions.len();
                return Err(Error::internal(format!(
                    "Subscription limit exceeded after adding: {count}/{max}"
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

    /// Add a negentropy subscription
    pub fn add_negentropy_subscription(
        &mut self,
        subscription_id: SubscriptionId,
        negentropy: Negentropy<'static, NegentropyStorageVector>,
    ) {
        self.negentropy_subscriptions
            .insert(subscription_id, negentropy);
    }

    /// Get a mutable reference to a negentropy subscription
    pub fn get_negentropy_subscription_mut(
        &mut self,
        subscription_id: &SubscriptionId,
    ) -> Option<&mut Negentropy<'static, NegentropyStorageVector>> {
        self.negentropy_subscriptions.get_mut(subscription_id)
    }

    /// Remove a negentropy subscription
    pub fn remove_negentropy_subscription(&mut self, subscription_id: &SubscriptionId) -> bool {
        self.negentropy_subscriptions
            .remove(subscription_id)
            .is_some()
    }

    /// Get count of active negentropy subscriptions
    pub fn negentropy_subscription_count(&self) -> usize {
        self.negentropy_subscriptions.len()
    }
}
