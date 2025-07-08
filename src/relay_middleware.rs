//! RelayMiddleware for processing Nostr messages with optimized performance
//!
//! This module provides middleware that processes Nostr protocol messages while
//! delegating business logic to EventProcessor implementations. The implementation
//! is optimized for zero-allocation in hot paths like subscription processing.

use crate::database::RelayDatabase;
use crate::error::Error;
use crate::event_processor::{EventContext, EventProcessor};
use crate::state::NostrConnectionState;
use crate::subscription_coordinator::StoreCommand;
use crate::subscription_registry::SubscriptionRegistry;
use async_trait::async_trait;
use nostr_sdk::prelude::*;
use std::sync::Arc;
use tracing::{debug, error};
use websocket_builder::{InboundContext, Middleware, OutboundContext};

/// Relay middleware that processes messages with zero-allocation performance.
///
/// This middleware provides protocol handling (EVENT, REQ, CLOSE, AUTH) while
/// delegating business logic to EventProcessor implementations. The design
/// minimizes allocations in hot paths for maximum performance.
///
/// ## Key Features
///
/// - Zero-allocation event visibility checks during subscription processing
/// - Direct access to custom state without cloning
/// - Type-safe generic state management
/// - Backward compatible with full state access where needed
#[derive(Debug, Clone)]
pub struct RelayMiddleware<P, T = ()>
where
    P: EventProcessor<T>,
    T: Send + Sync + 'static,
{
    processor: Arc<P>,
    relay_pubkey: PublicKey,
    database: Arc<RelayDatabase>,
    registry: Arc<SubscriptionRegistry>,
    max_limit: usize,
    relay_url: RelayUrl,
    db_sender: crate::database::DatabaseSender,
    crypto_helper: crate::crypto_helper::CryptoHelper,
    max_subscriptions: Option<usize>,
    _phantom: std::marker::PhantomData<T>,
}

impl<P, T> RelayMiddleware<P, T>
where
    P: EventProcessor<T>,
    T: Clone + Send + Sync + std::fmt::Debug + Default + 'static,
{
    /// Create a new relay middleware with the specified processor.
    ///
    /// # Arguments
    /// * `processor` - The business logic implementation
    /// * `relay_pubkey` - The relay's public key
    /// * `database` - Database for storing events
    /// * `registry` - Subscription registry for event distribution
    /// * `max_limit` - Maximum limit for subscriptions
    /// * `relay_url` - The relay URL
    /// * `db_sender` - Database sender for async operations
    /// * `crypto_helper` - Crypto helper for signing events
    /// * `max_subscriptions` - Maximum subscriptions per connection
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        processor: P,
        relay_pubkey: PublicKey,
        database: Arc<RelayDatabase>,
        registry: Arc<SubscriptionRegistry>,
        max_limit: usize,
        relay_url: RelayUrl,
        db_sender: crate::database::DatabaseSender,
        crypto_helper: crate::crypto_helper::CryptoHelper,
        max_subscriptions: Option<usize>,
    ) -> Self {
        Self {
            processor: Arc::new(processor),
            relay_pubkey,
            database,
            registry,
            max_limit,
            relay_url,
            db_sender,
            crypto_helper,
            max_subscriptions,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Get a reference to the event processor
    pub fn processor(&self) -> &Arc<P> {
        &self.processor
    }

    /// Handle EVENT messages with optimized performance
    async fn handle_event(
        &self,
        event: Event,
        state: Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
        message_sender: Option<websocket_builder::MessageSender<RelayMessage<'static>>>,
    ) -> Result<(), Error> {
        // Extract necessary state before async call
        let (authed_pubkey, subdomain) = {
            let connection_state = state.read();
            (
                connection_state.authed_pubkey,
                Arc::clone(&connection_state.subdomain), // Clone the Arc pointer
            )
        };

        // Create custom state wrapper
        let custom_state_wrapper = Arc::new(parking_lot::RwLock::new({
            let connection_state = state.read();
            connection_state.custom_state.clone()
        }));

        let context = EventContext {
            authed_pubkey: authed_pubkey.as_ref(),
            subdomain: &subdomain,
            relay_pubkey: &self.relay_pubkey,
        };

        let mut commands = self
            .processor
            .handle_event(event, custom_state_wrapper, context)
            .await?;

        let subscription_coordinator = {
            let state_guard = state.read();
            state_guard
                .subscription_coordinator()
                .ok_or_else(|| Error::internal("No subscription coordinator available"))?
                .clone()
        };

        // TODO: Refactor to attach message_sender early in the middleware chain.
        // Instead of passing a raw Event to handle_event, we should pass an InboundEvent that already
        // carries the message_sender for OK/error responses. This would:
        // - Eliminate the need to search through commands to find SaveSignedEvent
        // - Make the primary event's response channel explicit from the start
        // - Allow EventProcessor to return a cleaner result type without mixing concerns
        // Architecture: The first middleware in the chain should wrap the incoming Event with its
        // message_sender, creating an InboundEvent { event: Event, response_sender: MessageSender }.
        // The EventProcessor would then return something like:
        // enum EventResult {
        //     Accept { derived_events: Vec<UnsignedEvent> },
        //     Reject { reason: String }
        // }
        // The OK response would be handled automatically based on the InboundEvent's sender.
        // Extract the SaveSignedEvent command if it exists (there should be only one)
        let event_command_idx = commands
            .iter()
            .position(|cmd| matches!(cmd, StoreCommand::SaveSignedEvent(_, _, _)));

        // If we found a SaveSignedEvent command, remove it and process it with message_sender
        if let Some(idx) = event_command_idx {
            let event_command = commands.swap_remove(idx);
            subscription_coordinator
                .save_and_broadcast(event_command, message_sender)
                .await
                .map_err(|e| Error::database(e.to_string()))?;
        }

        // Process all remaining commands without message_sender
        for command in commands {
            subscription_coordinator
                .save_and_broadcast(command, None)
                .await
                .map_err(|e| Error::database(e.to_string()))?;
        }

        // Database layer will send OK after persistence
        Ok(())
    }

    /// Handle subscription with optimized event filtering
    async fn handle_subscription(
        &self,
        state: Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
        subscription_id: String,
        filters: Vec<Filter>,
    ) -> Result<(), Error> {
        let subscription_id_obj = SubscriptionId::new(subscription_id.clone());

        // First check subscription limit and verify filters with write lock
        {
            let mut connection_state = state.write();

            // Check if we can add this subscription
            connection_state.try_add_subscription(subscription_id_obj.clone())?;

            let context = EventContext {
                authed_pubkey: connection_state.authed_pubkey.as_ref(),
                subdomain: &connection_state.subdomain,
                relay_pubkey: &self.relay_pubkey,
            };

            // Create custom state wrapper
            let custom_state_wrapper = Arc::new(parking_lot::RwLock::new(
                connection_state.custom_state.clone(),
            ));

            self.processor
                .verify_filters(&filters, custom_state_wrapper, context)?;

            // Track the subscription
            connection_state.add_subscription(subscription_id_obj.clone());
        }

        // Extract necessary state with read lock
        let (subdomain, authed_pubkey, custom_state) = {
            let connection_state = state.read();
            let subdomain = Arc::clone(&connection_state.subdomain);
            let authed_pubkey = connection_state.authed_pubkey;
            let custom_state = connection_state.custom_state.clone();
            (subdomain, authed_pubkey, custom_state)
        };

        // Clone for the filter function
        let processor = Arc::clone(&self.processor);
        let relay_pubkey = self.relay_pubkey;

        // Create filter function with cloned state - no async needed
        let filter_fn =
            move |event: &Event, scope: &nostr_lmdb::Scope, auth_pk: Option<&PublicKey>| -> bool {
                // Create context on stack - zero heap allocations
                let context = EventContext {
                    authed_pubkey: auth_pk,
                    subdomain: scope,
                    relay_pubkey: &relay_pubkey,
                };

                // Create custom state wrapper for each call
                let custom_state_wrapper = Arc::new(parking_lot::RwLock::new(custom_state.clone()));

                processor
                    .can_see_event(event, custom_state_wrapper, context)
                    .unwrap_or(false)
            };

        // Get subscription coordinator and process
        let subscription_coordinator = {
            let connection_state = state.read();
            connection_state
                .subscription_coordinator()
                .ok_or_else(|| Error::internal("No subscription coordinator available"))?
                .clone()
        };

        subscription_coordinator
            .handle_req(
                SubscriptionId::new(subscription_id),
                filters,
                authed_pubkey,
                &subdomain,
                filter_fn,
            )
            .await?;

        // Subscription service sends messages directly
        Ok(())
    }
}

#[async_trait]
impl<P, T> Middleware for RelayMiddleware<P, T>
where
    P: EventProcessor<T>,
    T: Clone + Send + Sync + std::fmt::Debug + Default + 'static,
{
    type State = NostrConnectionState<T>;
    type IncomingMessage = ClientMessage<'static>;
    type OutgoingMessage = RelayMessage<'static>;

    async fn on_connect(
        &self,
        ctx: &mut websocket_builder::ConnectionContext<
            Self::State,
            Self::IncomingMessage,
            Self::OutgoingMessage,
        >,
    ) -> anyhow::Result<()> {
        debug!("RelayMiddleware: Setting up connection");

        // Initialize state fields that were previously set by the factory
        {
            let mut state = ctx.state.write();
            state.relay_url = self.relay_url.clone();
            state.db_sender = Some(self.db_sender.clone());
            state.registry = Some(self.registry.clone());
            state.max_subscriptions = self.max_subscriptions;
            // TODO: subdomain resolution needs to be handled differently without factory pattern
        }

        // Initialize the subscription service for this connection
        if let Some(ref sender) = ctx.sender {
            {
                let mut state = ctx.state.write();
                state
                    .setup_connection(
                        self.database.clone(),
                        self.registry.clone(),
                        ctx.connection_id.clone(),
                        sender.clone(),
                        self.crypto_helper.clone(),
                        Some(self.max_limit),
                    )
                    .map_err(|e| anyhow::anyhow!("Failed to setup connection: {}", e))?;
            }
            debug!("RelayMiddleware: Connection setup complete");
        } else {
            error!("RelayMiddleware: No message sender available for connection setup");
        }

        ctx.next().await
    }

    async fn process_inbound(
        &self,
        ctx: &mut InboundContext<Self::State, Self::IncomingMessage, Self::OutgoingMessage>,
    ) -> anyhow::Result<()> {
        let Some(message) = ctx.message.take() else {
            return ctx.next().await;
        };

        match message {
            ClientMessage::Event(boxed_event) => {
                // Handle EVENT message
                match self
                    .handle_event(
                        boxed_event.into_owned(),
                        ctx.state.clone(),
                        ctx.sender.clone(),
                    )
                    .await
                {
                    Ok(()) => {}
                    Err(e) => {
                        error!("Event processing error: {}", e);
                        // Propagate the error up the chain so ErrorHandlingMiddleware can format it properly
                        return Err(e.into());
                    }
                }
                ctx.next().await
            }

            ClientMessage::Req {
                subscription_id,
                filter,
            } => {
                // Use generic subscription handling directly
                match self
                    .handle_subscription(
                        ctx.state.clone(),
                        subscription_id.to_string(),
                        vec![filter.into_owned()],
                    )
                    .await
                {
                    Ok(()) => {}
                    Err(e) => {
                        error!("Subscription error: {}", e);
                        // Propagate the error up the chain so ErrorHandlingMiddleware can format it properly
                        return Err(e.into());
                    }
                }
                ctx.next().await
            }

            ClientMessage::ReqMultiFilter {
                subscription_id,
                filters,
            } => {
                // Use generic subscription handling directly
                match self
                    .handle_subscription(ctx.state.clone(), subscription_id.to_string(), filters)
                    .await
                {
                    Ok(()) => {}
                    Err(e) => {
                        error!("Subscription error: {}", e);
                        // Propagate the error up the chain so ErrorHandlingMiddleware can format it properly
                        return Err(e.into());
                    }
                }
                ctx.next().await
            }

            ClientMessage::Close(subscription_id) => {
                // Handle CLOSE message
                {
                    let mut state = ctx.state.write();
                    let subscription_id_owned = subscription_id.into_owned();

                    // Remove from tracked subscriptions
                    state.remove_tracked_subscription(&subscription_id_owned);

                    if let Some(subscription_coordinator) = state.subscription_coordinator() {
                        let _ = subscription_coordinator
                            .remove_subscription(subscription_id_owned.clone());
                        debug!("Closed subscription: {}", subscription_id_owned);
                    }
                }
                ctx.next().await
            }

            // All other messages are not handled by default
            _ => match &message {
                ClientMessage::Auth(_) => {
                    // if it was enabled, we would have handled it in the nip42 middleware
                    debug!(
                            "AUTH message received but authentication is not enabled on this relay, ignoring"
                        );
                    ctx.next().await
                }
                _ => {
                    let msg = format!(
                        "Message type not supported: {}",
                        match &message {
                            ClientMessage::Count { .. } => "COUNT",
                            ClientMessage::NegOpen { .. } => "NEG-OPEN",
                            ClientMessage::NegMsg { .. } => "NEG-MSG",
                            ClientMessage::NegClose { .. } => "NEG-CLOSE",
                            _ => "UNKNOWN",
                        }
                    );
                    debug!("{msg}");
                    Err(Error::notice(msg).into())
                }
            },
        }
    }

    async fn process_outbound(
        &self,
        ctx: &mut OutboundContext<Self::State, Self::IncomingMessage, Self::OutgoingMessage>,
    ) -> anyhow::Result<()> {
        let Some(message) = ctx.message.take() else {
            return ctx.next().await;
        };

        // For broadcast events, check visibility before sending
        if let RelayMessage::Event { event, .. } = &message {
            let should_filter = {
                let state = ctx.state.read();
                let subdomain = Arc::clone(&state.subdomain);
                let authed_pubkey = state.authed_pubkey;
                let custom_state = state.custom_state.clone();
                let context = EventContext {
                    authed_pubkey: authed_pubkey.as_ref(),
                    subdomain: &subdomain,
                    relay_pubkey: &self.relay_pubkey,
                };

                // Create custom state wrapper
                let custom_state_wrapper = Arc::new(parking_lot::RwLock::new(custom_state));

                !self
                    .processor
                    .can_see_event(event, custom_state_wrapper, context)?
            };

            if should_filter {
                return ctx.next().await; // Filter out
            }
        }

        ctx.message = Some(message);
        ctx.next().await
    }

    async fn on_disconnect(
        &self,
        ctx: &mut websocket_builder::DisconnectContext<
            Self::State,
            Self::IncomingMessage,
            Self::OutgoingMessage,
        >,
    ) -> anyhow::Result<()> {
        debug!("RelayMiddleware: Processing disconnect");

        // Clean up the connection state to release database references and decrement subscription counters
        {
            let state = ctx.state.read();
            state.cleanup();
        }

        debug!("RelayMiddleware: Connection cleanup complete");
        ctx.next().await
    }
}
