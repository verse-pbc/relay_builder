//! Connection state management

use crate::database::RelayDatabase;
use crate::error::Error;
use crate::event_processor::EventContext;
use crate::nostr_middleware::MessageSender;
use crate::subscription_coordinator::StoreCommand;
use crate::subscription_coordinator::SubscriptionCoordinator;
use crate::subscription_registry::SubscriptionRegistry;
use anyhow::Result;
use arc_swap::ArcSwap;
use negentropy::{Negentropy, NegentropyStorageVector};
use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, warn};

/// Immutable connection metadata that doesn't need lock protection
#[derive(Debug, Clone)]
pub struct ConnectionMetadata {
    /// The relay URL this connection is for
    pub relay_url: RelayUrl,
    /// The subdomain scope for this connection
    pub subdomain: Arc<Scope>,
    /// Connection cancellation token
    pub connection_token: CancellationToken,
    /// Pre-built event context for lock-free access
    pub event_context: Arc<ArcSwap<EventContext>>,
}

impl ConnectionMetadata {
    pub fn new(_relay_url: RelayUrl, _subdomain: Arc<Scope>) -> Self {
        panic!("ConnectionMetadata::new needs relay_pubkey - use new_with_context instead")
    }

    pub fn new_with_context(
        relay_url: RelayUrl,
        subdomain: Arc<Scope>,
        relay_pubkey: PublicKey,
    ) -> Self {
        let initial_context = EventContext {
            authed_pubkey: None,
            subdomain: Arc::clone(&subdomain),
            relay_pubkey,
        };

        Self {
            relay_url,
            subdomain,
            connection_token: CancellationToken::new(),
            event_context: Arc::new(ArcSwap::from_pointee(initial_context)),
        }
    }
}

/// Type alias for the default NostrConnectionState without custom state
pub type DefaultNostrConnectionState = NostrConnectionState<()>;

/// Connection state for a WebSocket client (mutable fields only)
#[derive(Debug)]
pub struct NostrConnectionState<T = ()> {
    /// Challenge for NIP-42 authentication
    pub challenge: Option<String>,
    /// Authenticated public key (if authenticated via NIP-42)
    pub authed_pubkey: Option<PublicKey>,
    /// Subscription coordinator for this connection (private - use methods below)
    subscription_coordinator: Option<SubscriptionCoordinator>,
    /// Maximum number of subscriptions allowed (set by the connection factory)
    pub(crate) max_subscriptions: Option<usize>,
    /// Track subscription IDs for quota management (not actual subscription data)
    subscription_quota_usage: std::collections::HashSet<SubscriptionId>,
    /// Active negentropy subscriptions
    negentropy_subscriptions: HashMap<SubscriptionId, Negentropy<'static, NegentropyStorageVector>>,
    /// Registry for looking up subscriptions (used to set up subscription coordinator)
    pub(crate) registry: Option<Arc<SubscriptionRegistry>>,
    /// Custom state that can be managed by middleware
    pub custom_state: Arc<parking_lot::RwLock<T>>,
}

impl<T> Default for NostrConnectionState<T>
where
    T: Default,
{
    fn default() -> Self {
        Self {
            challenge: None,
            authed_pubkey: None,
            subscription_coordinator: None,
            max_subscriptions: None,
            subscription_quota_usage: std::collections::HashSet::new(),
            negentropy_subscriptions: HashMap::new(),
            registry: None,
            custom_state: Arc::new(parking_lot::RwLock::new(T::default())),
        }
    }
}

impl<T> NostrConnectionState<T>
where
    T: Default,
{
    pub fn new() -> Result<Self> {
        Ok(Self {
            challenge: None,
            authed_pubkey: None,
            subscription_coordinator: None,
            max_subscriptions: None,
            subscription_quota_usage: std::collections::HashSet::new(),
            negentropy_subscriptions: HashMap::new(),
            registry: None,
            custom_state: Arc::new(parking_lot::RwLock::new(T::default())),
        })
    }
}

impl<T> Clone for NostrConnectionState<T>
where
    T: Clone,
{
    fn clone(&self) -> Self {
        Self {
            challenge: self.challenge.clone(),
            authed_pubkey: self.authed_pubkey,
            subscription_coordinator: self.subscription_coordinator.clone(),
            max_subscriptions: self.max_subscriptions,
            subscription_quota_usage: self.subscription_quota_usage.clone(),
            negentropy_subscriptions: HashMap::new(), // Can't clone Negentropy, so start with empty
            registry: self.registry.clone(),
            custom_state: self.custom_state.clone(),
        }
    }
}

impl<T> NostrConnectionState<T> {
    pub fn with_custom(custom_state: T) -> Result<Self, Error> {
        Ok(Self {
            challenge: None,
            authed_pubkey: None,
            subscription_coordinator: None,
            max_subscriptions: None,
            subscription_quota_usage: std::collections::HashSet::new(),
            negentropy_subscriptions: HashMap::new(),
            registry: None,
            custom_state: Arc::new(parking_lot::RwLock::new(custom_state)),
        })
    }

    pub fn set_authenticated(&mut self, pubkey: PublicKey, metadata: &ConnectionMetadata) {
        self.authed_pubkey = Some(pubkey);
        self.challenge = None; // Clear challenge after successful auth

        // Update the event context with the authenticated pubkey
        let current = metadata.event_context.load();
        let new_context = EventContext {
            authed_pubkey: Some(pubkey),
            subdomain: Arc::clone(&current.subdomain),
            relay_pubkey: current.relay_pubkey,
        };
        metadata.event_context.store(Arc::new(new_context));
    }

    pub fn is_authenticated(&self) -> bool {
        self.authed_pubkey.is_some()
    }

    /// Setup the connection with database and registry
    #[allow(clippy::too_many_arguments)]
    pub fn setup_connection(
        &mut self,
        database: Arc<RelayDatabase>,
        registry: Arc<SubscriptionRegistry>,
        connection_id: String,
        sender: MessageSender,
        crypto_helper: crate::crypto_helper::CryptoHelper,
        subdomain: Arc<Scope>,
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
            subdomain,
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
            self.subscription_quota_usage.len() < max
        } else {
            true
        }
    }

    /// Get count of tracked subscriptions for quota purposes
    pub fn subscription_count(&self) -> usize {
        self.subscription_quota_usage.len()
    }

    /// Get remaining subscription capacity
    pub fn remaining_subscription_capacity(&self) -> Option<usize> {
        self.max_subscriptions
            .map(|max| max.saturating_sub(self.subscription_quota_usage.len()))
    }

    /// Reserve a quota slot for a subscription (handles replacements gracefully)
    pub fn reserve_quota_slot(&mut self, subscription_id: &SubscriptionId) -> Result<(), Error> {
        // Check if this is a replacement (subscription ID already exists)
        if self.subscription_quota_usage.contains(subscription_id) {
            debug!(
                "Updating existing subscription {}, no quota impact",
                subscription_id
            );
            return Ok(());
        }

        // For new subscriptions, check the quota limit
        if let Some(max) = self.max_subscriptions {
            let count = self.subscription_quota_usage.len();
            if count >= max {
                warn!(
                    "Subscription quota exceeded for connection: {}/{} slots used",
                    count, max
                );
                return Err(Error::restricted(format!(
                    "Subscription quota exceeded: {count}/{max}"
                )));
            }
        }

        // Reserve the slot
        self.subscription_quota_usage
            .insert(subscription_id.clone());
        Ok(())
    }

    /// Track a new subscription (DEPRECATED - use reserve_quota_slot instead)
    #[deprecated(note = "Use reserve_quota_slot() instead")]
    pub fn add_subscription(&mut self, subscription_id: SubscriptionId) {
        self.subscription_quota_usage.insert(subscription_id);
    }

    /// Release a quota slot when subscription is closed
    pub fn release_quota_slot(&mut self, subscription_id: &SubscriptionId) -> bool {
        self.subscription_quota_usage.remove(subscription_id)
    }

    /// Remove a subscription (DEPRECATED - use release_quota_slot instead)
    #[deprecated(note = "Use release_quota_slot() instead")]
    pub fn remove_tracked_subscription(&mut self, subscription_id: &SubscriptionId) -> bool {
        self.release_quota_slot(subscription_id)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_subscription_quota_replacement() {
        let mut state = NostrConnectionState::<()>::new().unwrap();
        state.set_max_subscriptions(Some(2));

        let sub1 = SubscriptionId::new("sub1");
        let sub2 = SubscriptionId::new("sub2");

        // Add two subscriptions (reach limit)
        assert!(state.reserve_quota_slot(&sub1).is_ok());
        assert!(state.reserve_quota_slot(&sub2).is_ok());
        assert_eq!(state.subscription_count(), 2);

        // Replacing existing subscription should work (not count against limit)
        assert!(state.reserve_quota_slot(&sub1).is_ok());
        assert_eq!(state.subscription_count(), 2); // Still 2, not 3

        // Adding genuinely new subscription should fail (at limit)
        let sub3 = SubscriptionId::new("sub3");
        let result = state.reserve_quota_slot(&sub3);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("quota exceeded"));

        // After removing one, can add new one
        assert!(state.release_quota_slot(&sub1));
        assert_eq!(state.subscription_count(), 1);
        assert!(state.reserve_quota_slot(&sub3).is_ok());
        assert_eq!(state.subscription_count(), 2);
    }

    #[test]
    fn test_subscription_quota_no_limit() {
        let mut state = NostrConnectionState::<()>::new().unwrap();
        // No limit set (None)

        // Should be able to add many subscriptions without hitting a limit
        for i in 0..100 {
            let sub_id = SubscriptionId::new(format!("sub{i}"));
            assert!(state.reserve_quota_slot(&sub_id).is_ok());
        }
        assert_eq!(state.subscription_count(), 100);
    }

    #[test]
    fn test_subscription_quota_replacement_same_id() {
        let mut state = NostrConnectionState::<()>::new().unwrap();
        state.set_max_subscriptions(Some(1));

        let sub_id = SubscriptionId::new("nak"); // Common subscription ID used by nak client

        // First add should work
        assert!(state.reserve_quota_slot(&sub_id).is_ok());
        assert_eq!(state.subscription_count(), 1);

        // Replacing with same ID should work (not increment count)
        assert!(state.reserve_quota_slot(&sub_id).is_ok());
        assert_eq!(state.subscription_count(), 1);

        // Adding different subscription should fail (at limit)
        let sub2 = SubscriptionId::new("other");
        assert!(state.reserve_quota_slot(&sub2).is_err());
    }
}
