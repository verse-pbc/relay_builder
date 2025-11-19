//! High-performance subscription registry using inverted indexes for efficient event distribution
//!
//! This module implements the strfry ActiveMonitors optimization to reduce event distribution
//! complexity from O(n*m) to O(log k + m) where:
//! - n = number of connections
//! - m = average filters per connection
//! - k = number of unique index keys

use crate::error::Error;
use crate::metrics::SubscriptionMetricsHandler;
use crate::nostr_middleware::MessageSender;
use crate::subscription_index::SubscriptionIndex;
use dashmap::DashMap;
use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, trace, warn};

/// Diagnostic information for a single scope
#[derive(Debug, Clone)]
pub struct ScopeDiagnostics {
    pub scope: Scope,
    pub connection_count: usize,
    pub subscription_count: usize,
    pub filter_count: usize,
    pub tracked_events: usize,
    pub authors_indexed: usize,
    pub kinds_indexed: usize,
    pub event_ids_indexed: usize,
    pub tags_indexed: usize,
}

/// Overall registry diagnostics
#[derive(Debug, Clone)]
pub struct RegistryDiagnostics {
    pub total_connections: usize,
    pub scope_diagnostics: Vec<ScopeDiagnostics>,
    pub empty_scopes: Vec<Scope>,
}

/// Trait for distributing events to subscribers
pub trait EventDistributor: Send + Sync {
    /// Distribute an event to all matching subscriptions within the given scope
    fn distribute_event(
        &self,
        event: Arc<Event>,
        scope: &Scope,
    ) -> impl std::future::Future<Output = ()> + Send;
}

/// Registry for managing all active subscriptions across connections
#[derive(Clone)]
pub struct SubscriptionRegistry {
    /// Map of connection_id to their subscription data
    connections: Arc<DashMap<String, Arc<ConnectionSubscriptions>>>,
    /// Subscription indexes per scope for efficient event distribution
    indexes: Arc<DashMap<Scope, Arc<SubscriptionIndex>>>,
    /// Optional metrics handler
    metrics_handler: Option<Arc<dyn SubscriptionMetricsHandler>>,
}

impl std::fmt::Debug for SubscriptionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubscriptionRegistry")
            .field("connections_count", &self.connections.len())
            .field("scopes_with_indexes", &self.indexes.len())
            .field("has_metrics_handler", &self.metrics_handler.is_some())
            .finish()
    }
}

/// Subscription data for a single connection
pub struct ConnectionSubscriptions {
    /// Map of subscription_id to filters - RwLock since writes are rare
    subscriptions: RwLock<HashMap<SubscriptionId, Vec<Filter>>>,
    /// Channel to send events to this connection
    sender: MessageSender,
    /// Authenticated public key if any
    auth_pubkey: Option<PublicKey>,
    /// Subdomain/scope for this connection (Arc for cheap clones)
    subdomain: Arc<Scope>,
}

/// Handle for a connection that ensures cleanup on drop
pub struct ConnectionHandle {
    /// Connection ID
    pub id: String,
    /// Weak reference to the registry for cleanup
    registry: std::sync::Weak<SubscriptionRegistry>,
}

impl Drop for ConnectionHandle {
    fn drop(&mut self) {
        // Try to upgrade weak reference to Arc
        let Some(registry) = self.registry.upgrade() else {
            // Registry is gone, nothing we can do
            return;
        };

        // Check if the connection still exists in the registry
        // If it does, this means on_disconnect didn't run (unexpected path)
        if registry.connections.contains_key(&self.id) {
            warn!(
                "ConnectionHandle::drop performing fallback cleanup for connection {} - \
                 on_disconnect may not have executed properly",
                self.id
            );
            let connection_id = self.id.clone();
            tokio::spawn(async move {
                registry.cleanup_connection(&connection_id).await;
            });
        } else {
            // Connection already cleaned up - this is the expected path
            trace!(
                "ConnectionHandle::drop for connection {} - already cleaned up (expected)",
                self.id
            );
        }
    }
}

impl SubscriptionRegistry {
    /// Create a new subscription registry
    pub fn new(metrics_handler: Option<Arc<dyn SubscriptionMetricsHandler>>) -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
            indexes: Arc::new(DashMap::new()),
            metrics_handler,
        }
    }

    /// Register a new connection and return a handle for cleanup
    pub fn register_connection(
        self: &Arc<Self>,
        connection_id: String,
        sender: MessageSender,
        auth_pubkey: Option<PublicKey>,
        subdomain: Arc<Scope>,
    ) -> ConnectionHandle {
        let connection_data = Arc::new(ConnectionSubscriptions {
            subscriptions: RwLock::new(HashMap::new()),
            sender,
            auth_pubkey,
            subdomain,
        });

        self.connections
            .insert(connection_id.clone(), connection_data);

        ConnectionHandle {
            id: connection_id,
            registry: Arc::downgrade(self),
        }
    }

    /// Add a subscription for a connection
    pub async fn add_subscription(
        &self,
        connection_id: &str,
        subscription_id: &SubscriptionId,
        filters: Vec<Filter>,
    ) -> Result<(), Error> {
        let connection = self
            .connections
            .get(connection_id)
            .ok_or_else(|| Error::internal("Connection not found"))?;

        // Add to connection's local subscriptions
        let mut subscriptions = connection.subscriptions.write().await;
        let is_new = subscriptions
            .insert(subscription_id.clone(), filters.clone())
            .is_none();

        // Get or create index for this connection's scope
        let scope = connection.subdomain.as_ref().clone();
        let index = self
            .indexes
            .entry(scope)
            .or_insert_with(|| Arc::new(SubscriptionIndex::new()))
            .clone();

        // Add to the index for efficient distribution
        index
            .add_subscription(connection_id, subscription_id, filters)
            .await;

        // Only increment if this was a new subscription, not a replacement
        if is_new {
            if let Some(handler) = &self.metrics_handler {
                handler.increment_active_subscriptions();
            }
        }

        debug!(
            "Added subscription {} for connection {}",
            subscription_id, connection_id
        );
        Ok(())
    }

    /// Remove a subscription for a connection
    pub async fn remove_subscription(
        &self,
        connection_id: &str,
        subscription_id: &SubscriptionId,
    ) -> Result<(), Error> {
        let connection = self
            .connections
            .get(connection_id)
            .ok_or_else(|| Error::internal("Connection not found"))?;

        let scope = connection.subdomain.as_ref().clone();
        let mut subscriptions = connection.subscriptions.write().await;
        if subscriptions.remove(subscription_id).is_some() {
            // Remove from the appropriate scope index
            if let Some(index_entry) = self.indexes.get(&scope) {
                index_entry
                    .value()
                    .remove_subscription(connection_id, subscription_id)
                    .await;
            }

            if let Some(handler) = &self.metrics_handler {
                handler.decrement_active_subscriptions(1);
            }
            debug!(
                "Removed subscription {} for connection {}",
                subscription_id, connection_id
            );
        }

        Ok(())
    }

    /// Get connection info for REQ processing
    pub fn get_connection_info(
        &self,
        connection_id: &str,
    ) -> Option<(Option<PublicKey>, Arc<Scope>)> {
        self.connections
            .get(connection_id)
            .map(|conn| (conn.auth_pubkey, Arc::clone(&conn.subdomain)))
    }

    /// Check if a connection exists (mainly for testing)
    pub fn has_connection(&self, connection_id: &str) -> bool {
        self.connections.contains_key(connection_id)
    }

    /// Clean up a connection and all its subscriptions
    /// This is used both by Drop handler and when removing dead connections
    pub async fn cleanup_connection(&self, connection_id: &str) {
        debug!("Cleaning up connection {}", connection_id);

        // Get subscription IDs and scope before removing the connection
        let cleanup_info = if let Some(connection) = self.connections.get(connection_id) {
            let scope = connection.subdomain.as_ref().clone();
            let subscriptions = connection.subscriptions.read().await;
            let ids: Vec<SubscriptionId> = subscriptions.keys().cloned().collect();
            let count = ids.len();
            drop(subscriptions);
            debug!(
                "Connection {} has {} subscriptions to clean up",
                connection_id, count
            );
            Some((ids, count, scope))
        } else {
            warn!(
                "Connection {} not found in registry during cleanup",
                connection_id
            );
            None
        };

        // Remove the connection
        self.connections.remove(connection_id);

        // Clean up from index and decrement metrics
        if let Some((ids, count, scope)) = cleanup_info {
            if let Some(index_entry) = self.indexes.get(&scope) {
                let index = index_entry.value();
                for sub_id in ids {
                    index.remove_subscription(connection_id, &sub_id).await;
                }
            }

            if let Some(handler) = &self.metrics_handler {
                if count > 0 {
                    handler.decrement_active_subscriptions(count);
                    debug!(
                        "Successfully decremented {} subscriptions for cleaned up connection {}",
                        count, connection_id
                    );
                }
            }
        }
    }
}

impl SubscriptionRegistry {
    /// Collect diagnostic information about the registry
    pub async fn get_diagnostics(&self) -> RegistryDiagnostics {
        let mut scope_diagnostics = Vec::new();

        // Collect per-scope information
        for index_entry in self.indexes.iter() {
            let scope = index_entry.key().clone();
            let index = index_entry.value();
            let index_stats = index.stats().await;

            // Count connections for this scope
            let connection_count = self
                .connections
                .iter()
                .filter(|conn| conn.subdomain.as_ref() == &scope)
                .count();

            // Count total subscriptions for this scope
            let mut subscription_count = 0;
            for conn in self.connections.iter() {
                if conn.subdomain.as_ref() == &scope {
                    subscription_count += conn.subscriptions.read().await.len();
                }
            }

            scope_diagnostics.push(ScopeDiagnostics {
                scope: scope.clone(),
                connection_count,
                subscription_count,
                filter_count: index_stats.total_filters,
                tracked_events: index_stats.tracked_events,
                authors_indexed: index_stats.authors_indexed,
                kinds_indexed: index_stats.kinds_indexed,
                event_ids_indexed: index_stats.event_ids_indexed,
                tags_indexed: index_stats.tags_indexed,
            });
        }

        // Check for empty scopes (has index but no connections)
        let empty_scopes: Vec<Scope> = self
            .indexes
            .iter()
            .filter(|entry| {
                let scope = entry.key();
                !self
                    .connections
                    .iter()
                    .any(|conn| conn.subdomain.as_ref() == scope)
            })
            .map(|entry| entry.key().clone())
            .collect();

        RegistryDiagnostics {
            total_connections: self.connections.len(),
            scope_diagnostics,
            empty_scopes,
        }
    }

    /// Indexed event distribution using strfry optimization
    async fn distribute_event_indexed(&self, event: &Arc<Event>, scope: &Scope) {
        trace!(
            "Distributing event {} to subscribers using index in scope {:?}",
            event.id,
            scope
        );

        let mut total_matches = 0;
        let mut dead_connections = Vec::new();

        // Get the index for this scope
        let index = match self.indexes.get(scope) {
            Some(index_entry) => index_entry.value().clone(),
            None => {
                // No subscriptions for this scope
                trace!("No index found for scope {:?}", scope);
                return;
            }
        };

        // Get matching subscriptions from scope's index
        let matching_subscriptions = index.distribute_event(event).await;

        trace!(
            "Index found {} potential matches for event {} in scope {:?}",
            matching_subscriptions.len(),
            event.id,
            scope
        );

        // Pre-serialize the event once if we have matches
        let event_json = if !matching_subscriptions.is_empty() {
            Some(event.as_json())
        } else {
            None
        };

        for (conn_id, sub_id) in matching_subscriptions {
            // Get connection data
            let conn_data = match self.connections.get(&conn_id) {
                Some(conn) => conn,
                None => {
                    // Connection was removed after index returned it
                    trace!("Connection {} no longer exists", conn_id);
                    continue;
                }
            };

            // Double-check scope matches (should always match since we're using per-scope indexes)
            debug_assert_eq!(conn_data.subdomain.as_ref(), scope);

            total_matches += 1;

            let message = RelayMessage::event(
                sub_id.clone(),
                (**event).clone(), // Clone the event data
            );

            // Use pre-serialized JSON if available
            let sender = conn_data.sender.clone();
            let send_result = if let Some(ref json) = event_json {
                // Build the complete EVENT message JSON. We don't use the
                // message.as_json() so we can reuse the event json we already
                // have.
                let json_with_sub = format!(r#"["EVENT","{sub_id}",{json}]"#);
                sender.send_with_json(message, json_with_sub)
            } else {
                sender.send(message)
            };

            if let Err(e) = send_result {
                // Check the type of error to log appropriately
                let error_str = e.to_string();
                if error_str.contains("Channel full") {
                    warn!(
                        "Channel full for connection {} - client too slow, marking for removal",
                        conn_id
                    );
                } else if error_str.contains("Channel disconnected") {
                    debug!(
                        "Channel disconnected for connection {} - connection already closed",
                        conn_id
                    );
                } else {
                    warn!("Failed to send to connection {}: {}", conn_id, e);
                }
                dead_connections.push(conn_id);
            } else {
                trace!("Sent event to subscription on connection {}", conn_id);
            }
        }

        // Clean up dead connections
        for conn_id in dead_connections {
            // Use the shared cleanup method to ensure metrics are decremented
            self.cleanup_connection(&conn_id).await;
        }

        if total_matches > 0 {
            trace!("Event {} sent to {} subscriptions", event.id, total_matches);
        }
    }
}

impl EventDistributor for SubscriptionRegistry {
    async fn distribute_event(&self, event: Arc<Event>, scope: &Scope) {
        // Use indexed distribution for O(log k + m) performance
        self.distribute_event_indexed(&event, scope).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_connection_registration_and_cleanup() {
        let registry = Arc::new(SubscriptionRegistry::new(None));

        // Register a connection
        let (tx, _rx) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(100);
        let sender = MessageSender::new(tx, 0);

        {
            let _handle = registry.register_connection(
                "conn1".to_string(),
                sender,
                None,
                Arc::new(Scope::Default),
            );

            // Connection should exist
            assert!(registry.connections.contains_key("conn1"));

            // Handle will be dropped here
        }

        // Give the spawned cleanup task a chance to run
        tokio::task::yield_now().await;

        // After drop, connection should be removed
        assert!(!registry.connections.contains_key("conn1"));
    }

    #[tokio::test]
    async fn test_subscription_management() {
        let registry = Arc::new(SubscriptionRegistry::new(None));

        // Register a connection
        let (tx, _rx) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(100);
        let sender = MessageSender::new(tx, 0);
        let _handle = registry.register_connection(
            "conn1".to_string(),
            sender,
            None,
            Arc::new(Scope::Default),
        );

        // Add subscription
        let sub_id = SubscriptionId::new("sub1");
        let filters = vec![Filter::new()];

        registry
            .add_subscription("conn1", &sub_id, filters)
            .await
            .unwrap();

        // Remove subscription
        registry
            .remove_subscription("conn1", &sub_id)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_scope_aware_distribution() {
        use nostr_sdk::{EventBuilder, Keys};
        use std::time::Instant;

        let registry = Arc::new(SubscriptionRegistry::new(None));

        // Create two connections with different scopes
        let (tx1, rx1) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(100);
        let sender1 = MessageSender::new(tx1, 0);
        let _handle1 = registry.register_connection(
            "conn_default".to_string(),
            sender1,
            None,
            Arc::new(Scope::Default),
        );

        let (tx2, rx2) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(100);
        let sender2 = MessageSender::new(tx2, 0);
        let _handle2 = registry.register_connection(
            "conn_tenant1".to_string(),
            sender2,
            None,
            Arc::new(Scope::named("tenant1").unwrap()),
        );

        // Add subscriptions to both connections (matching all events)
        let sub_id1 = SubscriptionId::new("sub_default");
        let sub_id2 = SubscriptionId::new("sub_tenant1");
        let filters = vec![Filter::new()];

        registry
            .add_subscription("conn_default", &sub_id1, filters.clone())
            .await
            .unwrap();
        registry
            .add_subscription("conn_tenant1", &sub_id2, filters)
            .await
            .unwrap();

        // Create a test event
        let keys = Keys::generate();
        let event = EventBuilder::text_note("test message")
            .build_with_ctx(&Instant::now(), keys.public_key())
            .sign_with_keys(&keys)
            .unwrap();

        // Distribute event for Default scope
        registry
            .distribute_event(Arc::new(event.clone()), &Scope::Default)
            .await;

        // Check that only the Default connection received the event
        let msg1 = rx1.try_recv();
        let msg2 = rx2.try_recv();

        assert!(
            msg1.is_ok(),
            "Default scope connection should receive the event"
        );
        assert!(
            msg2.is_err(),
            "Named scope connection should NOT receive the event"
        );

        // Verify the correct event was received
        if let Ok((
            RelayMessage::Event {
                event: received_event,
                ..
            },
            _,
            _,
        )) = msg1
        {
            assert_eq!(received_event.id, event.id);
        } else {
            panic!("Expected Event message");
        }

        // Now test the other way - distribute to named scope
        let event2 = EventBuilder::text_note("test message 2")
            .build_with_ctx(&Instant::now(), keys.public_key())
            .sign_with_keys(&keys)
            .unwrap();

        registry
            .distribute_event(Arc::new(event2.clone()), &Scope::named("tenant1").unwrap())
            .await;

        // Check that only the tenant1 connection received the event
        let msg1 = rx1.try_recv();
        let msg2 = rx2.try_recv();

        assert!(
            msg1.is_err(),
            "Default scope connection should NOT receive the tenant1 event"
        );
        assert!(
            msg2.is_ok(),
            "Named scope connection should receive the event"
        );

        // Verify the correct event was received
        if let Ok((
            RelayMessage::Event {
                event: received_event,
                ..
            },
            _,
            _,
        )) = msg2
        {
            assert_eq!(received_event.id, event2.id);
        } else {
            panic!("Expected Event message");
        }
    }

    #[tokio::test]
    async fn test_multiple_named_scopes_isolation() {
        use nostr_sdk::{EventBuilder, Keys};
        use std::time::Instant;

        let registry = Arc::new(SubscriptionRegistry::new(None));

        // Create three connections with different named scopes
        let (tx1, rx1) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(100);
        let sender1 = MessageSender::new(tx1, 0);
        let _handle1 = registry.register_connection(
            "conn_tenant1".to_string(),
            sender1,
            None,
            Arc::new(Scope::named("tenant1").unwrap()),
        );

        let (tx2, rx2) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(100);
        let sender2 = MessageSender::new(tx2, 0);
        let _handle2 = registry.register_connection(
            "conn_tenant2".to_string(),
            sender2,
            None,
            Arc::new(Scope::named("tenant2").unwrap()),
        );

        let (tx3, rx3) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(100);
        let sender3 = MessageSender::new(tx3, 0);
        let _handle3 = registry.register_connection(
            "conn_tenant3".to_string(),
            sender3,
            None,
            Arc::new(Scope::named("tenant3").unwrap()),
        );

        // Add subscriptions to all connections
        let filters = vec![Filter::new()];
        registry
            .add_subscription(
                "conn_tenant1",
                &SubscriptionId::new("sub1"),
                filters.clone(),
            )
            .await
            .unwrap();
        registry
            .add_subscription(
                "conn_tenant2",
                &SubscriptionId::new("sub2"),
                filters.clone(),
            )
            .await
            .unwrap();
        registry
            .add_subscription("conn_tenant3", &SubscriptionId::new("sub3"), filters)
            .await
            .unwrap();

        // Create and distribute event to tenant2 only
        let keys = Keys::generate();
        let event = EventBuilder::text_note("message for tenant2")
            .build_with_ctx(&Instant::now(), keys.public_key())
            .sign_with_keys(&keys)
            .unwrap();

        registry
            .distribute_event(Arc::new(event.clone()), &Scope::named("tenant2").unwrap())
            .await;

        // Check that only tenant2 connection received the event
        let msg1 = rx1.try_recv();
        let msg2 = rx2.try_recv();
        let msg3 = rx3.try_recv();

        assert!(msg1.is_err(), "tenant1 should NOT receive tenant2's event");
        assert!(msg2.is_ok(), "tenant2 should receive its own event");
        assert!(msg3.is_err(), "tenant3 should NOT receive tenant2's event");

        // Verify the correct event was received
        if let Ok((
            RelayMessage::Event {
                event: received_event,
                ..
            },
            _,
            _,
        )) = msg2
        {
            assert_eq!(received_event.id, event.id);
            assert_eq!(received_event.content, "message for tenant2");
        } else {
            panic!("Expected Event message for tenant2");
        }
    }
}
