//! High-performance subscription registry using DashMap for concurrent access
//!
//! This module replaces the broadcast channel + actor pattern with a more efficient
//! DashMap-based approach that allows true parallel event distribution.

use crate::error::Error;
use crate::metrics::SubscriptionMetricsHandler;
use dashmap::DashMap;
use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, trace, warn};
use websocket_builder::MessageSender;

/// Trait for distributing events to subscribers
#[async_trait::async_trait]
pub trait EventDistributor: Send + Sync {
    /// Distribute an event to all matching subscriptions
    async fn distribute_event(&self, event: Arc<Event>);
}

/// Registry for managing all active subscriptions across connections
#[derive(Clone)]
pub struct SubscriptionRegistry {
    /// Map of connection_id to their subscription data
    connections: Arc<DashMap<String, Arc<ConnectionSubscriptions>>>,
    /// Optional metrics handler
    metrics_handler: Option<Arc<dyn SubscriptionMetricsHandler>>,
}

impl std::fmt::Debug for SubscriptionRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SubscriptionRegistry")
            .field("connections_count", &self.connections.len())
            .field("has_metrics_handler", &self.metrics_handler.is_some())
            .finish()
    }
}

/// Subscription data for a single connection
pub struct ConnectionSubscriptions {
    /// Map of subscription_id to filters - RwLock since writes are rare
    subscriptions: RwLock<HashMap<SubscriptionId, Vec<Filter>>>,
    /// Channel to send events to this connection
    sender: MessageSender<RelayMessage<'static>>,
    /// Authenticated public key if any
    auth_pubkey: Option<PublicKey>,
    /// Subdomain/scope for this connection
    subdomain: Scope,
}

/// Handle for a connection that ensures cleanup on drop
pub struct ConnectionHandle {
    /// Connection ID
    pub id: String,
    /// Reference to the registry for cleanup
    registry: Arc<SubscriptionRegistry>,
}

impl Drop for ConnectionHandle {
    fn drop(&mut self) {
        debug!("Connection {} dropped, removing from registry", self.id);

        // Count subscriptions before removing the connection
        let subscription_count = if let Some(connection) = self.registry.connections.get(&self.id) {
            connection.subscriptions.read().len()
        } else {
            0
        };

        self.registry.connections.remove(&self.id);

        if let Some(handler) = &self.registry.metrics_handler {
            if subscription_count > 0 {
                handler.decrement_active_subscriptions(subscription_count);
                debug!(
                    "Decremented {} subscriptions for dropped connection {}",
                    subscription_count, self.id
                );
            }
        }
    }
}

impl SubscriptionRegistry {
    /// Create a new subscription registry
    pub fn new(metrics_handler: Option<Arc<dyn SubscriptionMetricsHandler>>) -> Self {
        Self {
            connections: Arc::new(DashMap::new()),
            metrics_handler,
        }
    }

    /// Register a new connection and return a handle for cleanup
    pub fn register_connection(
        &self,
        connection_id: String,
        sender: MessageSender<RelayMessage<'static>>,
        auth_pubkey: Option<PublicKey>,
        subdomain: Scope,
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
            registry: Arc::new(self.clone()),
        }
    }

    /// Add a subscription for a connection
    pub fn add_subscription(
        &self,
        connection_id: &str,
        subscription_id: SubscriptionId,
        filters: Vec<Filter>,
    ) -> Result<(), Error> {
        let connection = self
            .connections
            .get(connection_id)
            .ok_or_else(|| Error::internal("Connection not found"))?;

        let mut subscriptions = connection.subscriptions.write();
        subscriptions.insert(subscription_id.clone(), filters);

        if let Some(handler) = &self.metrics_handler {
            handler.increment_active_subscriptions();
        }

        debug!(
            "Added subscription {} for connection {}",
            subscription_id, connection_id
        );
        Ok(())
    }

    /// Remove a subscription for a connection
    pub fn remove_subscription(
        &self,
        connection_id: &str,
        subscription_id: &SubscriptionId,
    ) -> Result<(), Error> {
        let connection = self
            .connections
            .get(connection_id)
            .ok_or_else(|| Error::internal("Connection not found"))?;

        let mut subscriptions = connection.subscriptions.write();
        if subscriptions.remove(subscription_id).is_some() {
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
    pub fn get_connection_info(&self, connection_id: &str) -> Option<(Option<PublicKey>, Scope)> {
        self.connections
            .get(connection_id)
            .map(|conn| (conn.auth_pubkey, conn.subdomain.clone()))
    }

    /// Start the event distribution task
    pub fn start_distribution_task(self: Arc<Self>, event_receiver: flume::Receiver<Arc<Event>>) {
        tokio::spawn(async move {
            debug!("Subscription registry distribution task started");

            while let Ok(event) = event_receiver.recv_async().await {
                // Distribute events inline without spawn_blocking
                self.distribute_event_inline(event);
            }

            debug!("Subscription registry distribution task stopped");
        });
    }
}

impl SubscriptionRegistry {
    /// Inline event distribution without spawn_blocking
    fn distribute_event_inline(&self, event: Arc<Event>) {
        trace!("Distributing event {} to subscribers", event.id);

        let mut total_matches = 0;
        let mut dead_connections = Vec::new();

        // Synchronous iteration over connections
        for entry in self.connections.iter() {
            let conn_id = entry.key();
            let conn_data = entry.value();

            // Use blocking read - fast since writes are rare
            let subscriptions = conn_data.subscriptions.read();

            for (sub_id, filters) in subscriptions.iter() {
                if filters.iter().any(|filter| filter.match_event(&event)) {
                    total_matches += 1;

                    let message = RelayMessage::event(
                        sub_id.clone(),
                        (*event).clone(), // Clone the event data
                    );

                    // MessageSender.send() is synchronous and uses try_send internally
                    let mut sender = conn_data.sender.clone();
                    if let Err(e) = sender.send(message) {
                        // Connection is dead, mark for removal
                        warn!("Failed to send to connection {}: {:?}", conn_id, e);
                        dead_connections.push(conn_id.clone());
                        break;
                    } else {
                        trace!(
                            "Sent event to subscription {} on connection {}",
                            sub_id,
                            conn_id
                        );
                    }
                }
            }
        }

        // Clean up dead connections
        for conn_id in dead_connections {
            self.connections.remove(&conn_id);
        }

        if total_matches > 0 {
            trace!("Event {} matched {} subscriptions", event.id, total_matches);
        }
    }
}

#[async_trait::async_trait]
impl EventDistributor for SubscriptionRegistry {
    async fn distribute_event(&self, event: Arc<Event>) {
        // Distribute inline without spawn_blocking
        self.distribute_event_inline(event);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_connection_registration_and_cleanup() {
        let registry = Arc::new(SubscriptionRegistry::new(None));

        // Register a connection
        let (tx, _rx) = flume::bounded::<(RelayMessage<'static>, usize)>(100);
        let sender = MessageSender::new(tx, 0);

        {
            let _handle =
                registry.register_connection("conn1".to_string(), sender, None, Scope::Default);

            // Connection should exist
            assert!(registry.connections.contains_key("conn1"));

            // Handle will be dropped here
        }

        // After drop, connection should be removed
        assert!(!registry.connections.contains_key("conn1"));
    }

    #[tokio::test]
    async fn test_subscription_management() {
        let registry = Arc::new(SubscriptionRegistry::new(None));

        // Register a connection
        let (tx, _rx) = flume::bounded::<(RelayMessage<'static>, usize)>(100);
        let sender = MessageSender::new(tx, 0);
        let _handle =
            registry.register_connection("conn1".to_string(), sender, None, Scope::Default);

        // Add subscription
        let sub_id = SubscriptionId::new("sub1");
        let filters = vec![Filter::new()];

        registry
            .add_subscription("conn1", sub_id.clone(), filters)
            .unwrap();

        // Remove subscription
        registry.remove_subscription("conn1", &sub_id).unwrap();
    }
}
