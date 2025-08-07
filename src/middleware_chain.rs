//! Static middleware chain with zero-cost abstractions for Nostr relays
//!
//! This module provides a compile-time middleware chain using static dispatch to achieve
//! zero-cost abstractions while supporting index-based message routing.

use crate::nostr_middleware::{
    DisconnectContext, InboundContext, InboundProcessor, MessageSender, NostrMiddleware,
    OutboundContext, OutboundProcessor,
};
use crate::state::NostrConnectionState;
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
    async fn process_inbound_chain(
        &self,
        _connection_id: &str,
        _message: &mut Option<ClientMessage<'static>>,
        _state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
    ) -> Result<(), anyhow::Error> {
        // End of chain - no more processing
        Ok(())
    }

    async fn on_connect_chain(
        &self,
        _connection_id: &str,
        _state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
    ) -> Result<(), anyhow::Error> {
        // End of chain - no more processing
        Ok(())
    }
}

impl<T> OutboundProcessor<T> for End<T>
where
    T: Send + Sync + 'static,
{
    async fn process_outbound_chain(
        &self,
        _connection_id: &str,
        _message: &mut Option<RelayMessage<'static>>,
        _state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
        _from_position: usize,
    ) -> Result<(), anyhow::Error> {
        // End of chain - no more processing
        Ok(())
    }

    async fn on_disconnect_chain(
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

/// Blueprint for building connected chains - contains no MessageSender
#[derive(Clone)]
pub struct ChainBlueprint<M, Next> {
    pub middleware: Arc<M>,
    pub next: Next,
    pub position: usize,
}

/// Connected chain with MessageSender for each middleware
#[derive(Clone)]
pub struct ConnectedChain<M, Next> {
    pub middleware: Arc<M>,
    pub next: Next,
    pub position: usize,
    pub message_sender: MessageSender,
}

/// Trait for recursively building connected chains from blueprints
pub trait BuildConnected {
    type Output;
    fn build_connected(
        self,
        sender_base: flume::Sender<(RelayMessage<'static>, usize, Option<String>)>,
    ) -> Self::Output;
}

/// Implementation for ChainBlueprint to build ConnectedChain
impl<M, Next> BuildConnected for ChainBlueprint<M, Next>
where
    Next: BuildConnected,
{
    type Output = ConnectedChain<M, Next::Output>;

    fn build_connected(
        self,
        sender_base: flume::Sender<(RelayMessage<'static>, usize, Option<String>)>,
    ) -> Self::Output {
        ConnectedChain {
            middleware: self.middleware,
            next: self.next.build_connected(sender_base.clone()),
            position: self.position,
            message_sender: MessageSender::new(sender_base, self.position),
        }
    }
}

/// Base case: End doesn't change
impl<T> BuildConnected for End<T> {
    type Output = End<T>;

    fn build_connected(
        self,
        _sender_base: flume::Sender<(RelayMessage<'static>, usize, Option<String>)>,
    ) -> Self::Output {
        self
    }
}

/// Implementation for Either to support conditional middleware
impl<L, R> BuildConnected for crate::util::Either<L, R>
where
    L: BuildConnected,
    R: BuildConnected,
{
    type Output = crate::util::Either<L::Output, R::Output>;

    fn build_connected(
        self,
        sender_base: flume::Sender<(RelayMessage<'static>, usize, Option<String>)>,
    ) -> Self::Output {
        match self {
            crate::util::Either::Left(l) => {
                crate::util::Either::Left(l.build_connected(sender_base))
            }
            crate::util::Either::Right(r) => {
                crate::util::Either::Right(r.build_connected(sender_base))
            }
        }
    }
}

/// Implementation for IdentityMiddleware - it doesn't need transformation
impl BuildConnected for crate::util::IdentityMiddleware {
    type Output = crate::util::IdentityMiddleware;

    fn build_connected(
        self,
        _sender_base: flume::Sender<(RelayMessage<'static>, usize, Option<String>)>,
    ) -> Self::Output {
        self
    }
}

/// Implementation for Arc<M> - just passes through since middleware is already Arc'd
impl<M> BuildConnected for Arc<M>
where
    M: Send + Sync,
{
    type Output = Arc<M>;

    fn build_connected(
        self,
        _sender_base: flume::Sender<(RelayMessage<'static>, usize, Option<String>)>,
    ) -> Self::Output {
        self
    }
}

/// InboundProcessor implementation for ConnectedChain - uses pre-created MessageSender
impl<M, Next, T> InboundProcessor<T> for ConnectedChain<M, Next>
where
    M: NostrMiddleware<T>,
    Next: InboundProcessor<T>,
    T: Send + Sync + Clone + 'static,
{
    async fn process_inbound_chain(
        &self,
        connection_id: &str,
        message: &mut Option<ClientMessage<'static>>,
        state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
    ) -> Result<(), anyhow::Error> {
        // Use pre-created MessageSender - no allocation!
        let ctx = InboundContext {
            connection_id,
            message,
            state,
            sender: self.message_sender.clone(), // Just Arc clone
            next: &self.next,
        };

        // Process through current middleware
        self.middleware.process_inbound(ctx).await
    }

    async fn on_connect_chain(
        &self,
        connection_id: &str,
        state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
    ) -> Result<(), anyhow::Error> {
        let ctx = crate::nostr_middleware::ConnectionContext {
            connection_id,
            state,
            sender: self.message_sender.clone(),
        };
        self.middleware.on_connect(ctx).await?;
        self.next.on_connect_chain(connection_id, state).await
    }
}

/// OutboundProcessor implementation for ConnectedChain
impl<M, Next, T> OutboundProcessor<T> for ConnectedChain<M, Next>
where
    M: NostrMiddleware<T>,
    Next: OutboundProcessor<T>,
    T: Send + Sync + Clone + 'static,
{
    async fn process_outbound_chain(
        &self,
        connection_id: &str,
        message: &mut Option<RelayMessage<'static>>,
        state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
        from_position: usize,
    ) -> Result<(), anyhow::Error> {
        if from_position <= self.position {
            self.next
                .process_outbound_chain(connection_id, message, state, from_position)
                .await?;

            let ctx = OutboundContext {
                connection_id,
                message,
                state,
                sender: &self.message_sender,
            };
            self.middleware.process_outbound(ctx).await
        } else {
            self.next
                .process_outbound_chain(connection_id, message, state, from_position)
                .await
        }
    }

    async fn on_disconnect_chain(
        &self,
        connection_id: &str,
        state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
    ) -> Result<(), anyhow::Error> {
        // Same as Chain
        self.next.on_disconnect_chain(connection_id, state).await?;

        let ctx = DisconnectContext {
            connection_id,
            state,
        };
        self.middleware.on_disconnect(ctx).await
    }
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
pub type EssentialsChain<T, Current> = ChainBlueprint<
    crate::middlewares::NostrLoggerMiddleware<T>,
    ChainBlueprint<crate::middlewares::ErrorHandlingMiddleware, Current>,
>;

impl<T, Current> NostrChainBuilder<T, Current>
where
    T: Send + Sync + Clone + std::fmt::Debug + 'static,
{
    /// Add a middleware to the chain blueprint
    pub fn with<M>(self, middleware: M) -> NostrChainBuilder<T, ChainBlueprint<M, Current>>
    where
        M: NostrMiddleware<T>,
    {
        let position = self.next_position;
        NostrChainBuilder {
            chain: ChainBlueprint {
                middleware: Arc::new(middleware),
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
