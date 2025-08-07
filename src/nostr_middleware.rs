//! Nostr-specific middleware trait and types
//!
//! This module provides a middleware system specifically designed for Nostr relays,
//! using concrete types to avoid generic complexity while maintaining compile-time performance.
//! It follows the patterns from websocket_builder's API guide with index-based routing.

use crate::state::NostrConnectionState;
use flume::Sender;
use nostr_sdk::prelude::*;
use std::sync::Arc;

/// Message sender with position-based routing for proper outbound processing
#[derive(Clone, Debug)]
pub struct MessageSender {
    sender: Sender<(RelayMessage<'static>, usize, Option<String>)>, // Message + originating middleware position + optional pre-serialized JSON
    position: usize,                                                // This middleware's position
}

impl MessageSender {
    pub fn new(
        sender: Sender<(RelayMessage<'static>, usize, Option<String>)>,
        position: usize,
    ) -> Self {
        Self { sender, position }
    }

    /// Send a message that goes through this middleware's outbound processing
    pub fn send(&self, message: RelayMessage<'static>) -> Result<(), anyhow::Error> {
        // Tag message with our position so outbound processing knows where it came from
        self.sender
            .try_send((message, self.position, None))
            .map_err(|e| anyhow::anyhow!("Failed to send message: {:?}", e))
    }

    /// Send a message bypassing this middleware's outbound processing
    /// Useful when you've already processed the message (e.g., filtered it)
    pub fn send_bypass(&self, message: RelayMessage<'static>) -> Result<(), anyhow::Error> {
        // Use position-1 to skip our own outbound processing
        let bypass_position = if self.position > 0 {
            self.position - 1
        } else {
            0
        };
        self.sender
            .try_send((message, bypass_position, None))
            .map_err(|e| anyhow::anyhow!("Failed to send message: {:?}", e))
    }

    /// Send a message with pre-serialized JSON for RelayMessage::Event
    /// This avoids re-serialization when distributing events to multiple connections
    pub fn send_with_json(
        &self,
        message: RelayMessage<'static>,
        json: String,
    ) -> Result<(), anyhow::Error> {
        self.sender
            .try_send((message, self.position, Some(json)))
            .map_err(|e| anyhow::anyhow!("Failed to send message: {:?}", e))
    }

    /// Get the raw sender (for advanced use cases)
    pub fn raw_sender(&self) -> &Sender<(RelayMessage<'static>, usize, Option<String>)> {
        &self.sender
    }

    /// Get this middleware's position
    pub fn position(&self) -> usize {
        self.position
    }
}

/// Context for inbound message processing
pub struct InboundContext<'a, T, Next> {
    /// Connection ID (remote address)
    pub connection_id: &'a str,
    /// The incoming message (mutable reference for consumption)
    pub message: &'a mut Option<ClientMessage<'static>>,
    /// Connection state
    pub state: &'a Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
    /// Message sender for sending responses with index tracking
    pub sender: MessageSender,
    /// Reference to the next processor in the chain (static dispatch)
    pub(crate) next: &'a Next,
}

impl<'a, T, Next> InboundContext<'a, T, Next>
where
    Next: InboundProcessor<T>,
{
    /// Continue to the next middleware in the chain
    pub async fn next(self) -> Result<(), anyhow::Error> {
        self.next
            .process_inbound_chain(self.connection_id, self.message, self.state, &self.sender)
            .await
    }
}

// Helper methods for all InboundContext variants
impl<'a, T, Next> InboundContext<'a, T, Next> {
    /// Send a message to the client
    pub fn send_message(&self, msg: RelayMessage<'static>) -> Result<(), anyhow::Error> {
        self.sender.send(msg)
    }

    /// Send a notice to the client
    pub fn send_notice(&self, message: String) -> Result<(), anyhow::Error> {
        self.send_message(RelayMessage::notice(message))
    }

    /// Send an OK response
    pub fn send_ok(
        &self,
        event_id: EventId,
        accepted: bool,
        message: String,
    ) -> Result<(), anyhow::Error> {
        self.send_message(RelayMessage::ok(event_id, accepted, message))
    }
}

/// Context for outbound message processing
pub struct OutboundContext<'a, T> {
    /// Connection ID (remote address)
    pub connection_id: &'a str,
    /// The outgoing message (mutable reference for modification)
    pub message: &'a mut Option<RelayMessage<'static>>,
    /// Connection state
    pub state: &'a Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
    /// Message sender with position tracking
    pub sender: &'a MessageSender,
}

/// Context for disconnect events (no sender - connection is closed)
#[derive(Clone)]
pub struct DisconnectContext<'a, T>
where
    T: Clone,
{
    /// Connection ID (remote address)
    pub connection_id: &'a str,
    /// Connection state
    pub state: &'a Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
}

/// Context for connection events
#[derive(Clone)]
pub struct ConnectionContext<'a, T>
where
    T: Clone,
{
    /// Connection ID (remote address)
    pub connection_id: &'a str,
    /// Connection state
    pub state: &'a Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
    /// Message sender for sending responses with index tracking
    pub sender: MessageSender,
}

// Helper methods for all ConnectionContext variants
impl<'a, T> ConnectionContext<'a, T>
where
    T: Clone,
{
    /// Send a message to the client
    pub fn send_message(&self, msg: RelayMessage<'static>) -> Result<(), anyhow::Error> {
        self.sender.send(msg)
    }

    /// Send a notice to the client
    pub fn send_notice(&self, message: String) -> Result<(), anyhow::Error> {
        self.send_message(RelayMessage::notice(message))
    }

    /// Send an OK response
    pub fn send_ok(
        &self,
        event_id: EventId,
        accepted: bool,
        message: String,
    ) -> Result<(), anyhow::Error> {
        self.send_message(RelayMessage::ok(event_id, accepted, message))
    }
}

/// Trait for processing inbound messages (internal, static dispatch)
pub trait InboundProcessor<T>: Send + Sync {
    fn process_inbound_chain(
        &self,
        connection_id: &str,
        message: &mut Option<ClientMessage<'static>>,
        state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
        sender: &MessageSender,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send;

    fn on_connect_chain(
        &self,
        connection_id: &str,
        state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
        sender: &MessageSender,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send;
}

/// Trait for processing outbound messages (internal, static dispatch)
pub trait OutboundProcessor<T>: Send + Sync {
    fn process_outbound_chain(
        &self,
        connection_id: &str,
        message: &mut Option<RelayMessage<'static>>,
        state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
        sender: &MessageSender,
        from_position: usize,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send;

    fn on_disconnect_chain(
        &self,
        connection_id: &str,
        state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send;
}

/// Trait for Nostr-specific middleware
///
/// This trait uses concrete Nostr types and native async traits for zero-cost
/// abstractions while providing position-based routing for proper outbound processing.
pub trait NostrMiddleware<T>: Send + Sync + Clone + 'static
where
    T: Send + Sync + Clone + 'static,
{
    /// Called when connection is established
    fn on_connect(
        &self,
        _ctx: ConnectionContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            // Default implementation: no-op
            Ok(())
        }
    }

    /// Process inbound messages
    ///
    /// This method receives a context with a `next()` method that MUST be called
    /// to continue the middleware chain. You can run code before `next().await`
    /// (prepend logic) or after it (append logic).
    fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, T, Next>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send
    where
        Next: InboundProcessor<T>,
    {
        async move {
            // Default implementation: pass through
            ctx.next().await
        }
    }

    /// Process outbound messages
    ///
    /// This method processes messages flowing back to the client. Unlike inbound,
    /// there's no `next()` call - outbound processing is handled by the chain runner.
    fn process_outbound(
        &self,
        _ctx: OutboundContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            // Default implementation: pass through
            Ok(())
        }
    }

    /// Called when connection closes
    fn on_disconnect(
        &self,
        _ctx: DisconnectContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            // Default implementation: no-op
            Ok(())
        }
    }
}
