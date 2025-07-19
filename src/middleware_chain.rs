//! Static middleware chain with zero-cost abstractions for Nostr relays
//!
//! This module provides a compile-time middleware chain using static dispatch to achieve
//! zero-cost abstractions while supporting index-based message routing.

use crate::nostr_middleware::{
    DisconnectContext, InboundContext, InboundProcessor, NostrMessageSender, NostrMiddleware,
    OutboundContext, OutboundProcessor,
};
use crate::state::NostrConnectionState;
// use flume::{Sender}; // Not needed in this module
use nostr_sdk::prelude::*;
use std::marker::PhantomData;
use std::sync::Arc;

/// End marker for the middleware chain
#[derive(Clone)]
pub struct End<T> {
    _phantom: PhantomData<T>,
}

impl<T> End<T> {
    pub fn new() -> Self {
        Self {
            _phantom: PhantomData,
        }
    }
}

impl<T> Default for End<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> InboundProcessor<T> for End<T>
where
    T: Send + Sync + 'static,
{
    async fn process_inbound(
        &self,
        _connection_id: &str,
        _message: &mut Option<ClientMessage<'static>>,
        _state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
        _sender: &NostrMessageSender,
    ) -> Result<(), anyhow::Error> {
        // End of chain - no more processing
        Ok(())
    }

    async fn on_connect(
        &self,
        _connection_id: &str,
        _state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
        _sender: &NostrMessageSender,
    ) -> Result<(), anyhow::Error> {
        // End of chain - no more processing
        Ok(())
    }
}

impl<T> OutboundProcessor<T> for End<T>
where
    T: Send + Sync + 'static,
{
    async fn process_outbound(
        &self,
        _connection_id: &str,
        _message: &mut Option<RelayMessage<'static>>,
        _state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
        _sender: &NostrMessageSender,
        _from_position: usize,
    ) -> Result<(), anyhow::Error> {
        // End of chain - no more processing
        Ok(())
    }

    async fn on_disconnect(
        &self,
        _connection_id: &str,
        _state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
    ) -> Result<(), anyhow::Error> {
        // End of chain - no more processing
        Ok(())
    }
}

impl<T> NostrMiddleware<T> for End<T>
where
    T: Clone + Send + Sync + 'static,
{
    // Uses default implementations which are no-ops
}

/// Static middleware chain with zero-cost abstractions
#[derive(Clone)]
pub struct Chain<M, Next> {
    pub middleware: M,
    pub next: Next,
    pub position: usize,
}

impl<M, Next, T> InboundProcessor<T> for Chain<M, Next>
where
    M: NostrMiddleware<T>,
    Next: InboundProcessor<T>,
    T: Send + Sync + Clone + 'static,
{
    async fn process_inbound(
        &self,
        connection_id: &str,
        message: &mut Option<ClientMessage<'static>>,
        state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
        sender: &NostrMessageSender,
    ) -> Result<(), anyhow::Error> {
        // Create a new sender with this middleware's position
        let positioned_sender = NostrMessageSender::new(sender.raw_sender().clone(), self.position);

        // Create context with next reference
        let ctx = InboundContext {
            connection_id,
            message,
            state,
            sender: positioned_sender,
            next: &self.next,
        };

        // Process through current middleware
        self.middleware.process_inbound(ctx).await
    }

    async fn on_connect(
        &self,
        connection_id: &str,
        state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
        sender: &NostrMessageSender,
    ) -> Result<(), anyhow::Error> {
        // on_connect flows like process_inbound: outer to inner
        // First call current middleware's on_connect
        let ctx = crate::nostr_middleware::ConnectionContext {
            connection_id,
            state,
            sender: sender.clone(),
        };
        self.middleware.on_connect(ctx).await?;

        // Then propagate to next (inner) middleware
        self.next.on_connect(connection_id, state, sender).await
    }
}

impl<M, Next, T> OutboundProcessor<T> for Chain<M, Next>
where
    M: NostrMiddleware<T>,
    Next: OutboundProcessor<T>,
    T: Send + Sync + Clone + 'static,
{
    async fn process_outbound(
        &self,
        connection_id: &str,
        message: &mut Option<RelayMessage<'static>>,
        state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
        sender: &NostrMessageSender,
        from_position: usize,
    ) -> Result<(), anyhow::Error> {
        if from_position <= self.position {
            // Message from this position or inner positions
            // Process through next first (inner middlewares)
            self.next
                .process_outbound(connection_id, message, state, sender, from_position)
                .await?;

            // Then process through this middleware
            let ctx = OutboundContext {
                connection_id,
                message,
                state,
                sender,
            };
            self.middleware.process_outbound(ctx).await
        } else {
            // Message from outer middleware - skip this level
            self.next
                .process_outbound(connection_id, message, state, sender, from_position)
                .await
        }
    }

    async fn on_disconnect(
        &self,
        connection_id: &str,
        state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
    ) -> Result<(), anyhow::Error> {
        // Always process in reverse order: next first, then this
        self.next.on_disconnect(connection_id, state).await?;

        let ctx = DisconnectContext {
            connection_id,
            state,
        };
        self.middleware.on_disconnect(ctx).await
    }
}

impl<M, Next, T> NostrMiddleware<T> for Chain<M, Next>
where
    M: NostrMiddleware<T>,
    Next: NostrMiddleware<T>,
    T: Send + Sync + Clone + 'static,
{
    // Chain uses default implementations from NostrMiddleware
    // The actual chain handling is done through InboundProcessor and OutboundProcessor
}

/// Builder for creating static middleware chains with type inference
pub struct NostrChainBuilder<T, Current> {
    chain: Current,
    next_position: usize,
    _phantom: PhantomData<T>,
}

impl<T> NostrChainBuilder<T, End<T>>
where
    T: Send + Sync + 'static,
{
    /// Create a new chain builder
    pub fn new() -> Self {
        Self {
            chain: End::new(),
            next_position: 0,
            _phantom: PhantomData,
        }
    }
}

impl<T> Default for NostrChainBuilder<T, End<T>>
where
    T: Send + Sync + 'static,
{
    fn default() -> Self {
        Self::new()
    }
}

/// Type alias for the essential middleware chain created by with_essentials()
/// Note: Event verification is handled by EventIngester, not middleware
pub type EssentialsChain<T, Current> = Chain<
    crate::middlewares::NostrLoggerMiddleware<T>,
    Chain<crate::middlewares::ErrorHandlingMiddleware, Current>,
>;

impl<T, Current> NostrChainBuilder<T, Current>
where
    T: Send + Sync + Clone + std::fmt::Debug + 'static,
    Current: NostrMiddleware<T> + InboundProcessor<T>,
{
    /// Add a middleware to the chain
    pub fn with<M>(self, middleware: M) -> NostrChainBuilder<T, Chain<M, Current>>
    where
        M: NostrMiddleware<T>,
    {
        let position = self.next_position;
        NostrChainBuilder {
            chain: Chain {
                middleware,
                next: self.chain,
                position,
            },
            next_position: position + 1,
            _phantom: PhantomData,
        }
    }

    /// Add essential middleware (error handling, logger)
    ///
    /// This is a convenience method that adds the most commonly needed middleware
    /// in the proper order. Note: Event verification is handled automatically by EventIngester.
    /// This method is equivalent to:
    /// ```ignore
    /// chain
    ///     .with(ErrorHandlingMiddleware::new())
    ///     .with(NostrLoggerMiddleware::new())
    /// ```
    pub fn with_essentials(self) -> NostrChainBuilder<T, EssentialsChain<T, Current>> {
        self.with(crate::middlewares::ErrorHandlingMiddleware::new())
            .with(crate::middlewares::NostrLoggerMiddleware::new())
    }

    /// Build the final chain
    pub fn build(self) -> Current {
        self.chain
    }
}

/// Convenience function to start building a chain
pub fn chain<T>() -> NostrChainBuilder<T, End<T>>
where
    T: Send + Sync + 'static,
{
    NostrChainBuilder::new()
}
