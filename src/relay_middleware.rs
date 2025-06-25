//! RelayMiddleware for processing Nostr messages with optimized performance
//!
//! This module provides middleware that processes Nostr protocol messages while
//! delegating business logic to EventProcessor implementations. The implementation
//! is optimized for zero-allocation in hot paths like subscription processing.

use crate::database::RelayDatabase;
use crate::error::Error;
use crate::event_processor::{EventContext, EventProcessor};
use crate::state::NostrConnectionState;
use async_trait::async_trait;
use nostr_sdk::prelude::*;
use std::sync::Arc;
use tracing::{debug, error};
use websocket_builder::{InboundContext, Middleware, OutboundContext, SendMessage};

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
    pub fn new(processor: P, relay_pubkey: PublicKey, database: Arc<RelayDatabase>) -> Self {
        Self {
            processor: Arc::new(processor),
            relay_pubkey,
            database,
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
        state: Arc<tokio::sync::RwLock<NostrConnectionState<T>>>,
        message_sender: Option<websocket_builder::MessageSender<RelayMessage<'static>>>,
    ) -> Result<(), Error> {
        let (subdomain, authed_pubkey, custom_state) = {
            let connection_state = state.read().await;

            // Extract necessary state before calling handle_event
            let subdomain = connection_state.subdomain().clone();
            let authed_pubkey = connection_state.authed_pubkey.as_ref().cloned();
            let custom_state = connection_state.custom.clone();

            (subdomain, authed_pubkey, custom_state)
        };

        // Process with direct custom state access
        let context = EventContext {
            authed_pubkey: authed_pubkey.as_ref(),
            subdomain: &subdomain,
            relay_pubkey: &self.relay_pubkey,
        };

        let commands = self
            .processor
            .handle_event(event.clone(), custom_state, context)
            .await?;

        let subscription_service = state.read().await;

        if let Some(subscription_service) = subscription_service.subscription_service() {
            for command in commands {
                subscription_service
                    .save_and_broadcast(command, message_sender.clone())
                    .await
                    .map_err(|e| Error::database(e.to_string()))?;
            }
        }

        // Database layer will send OK after persistence
        Ok(())
    }

    /// Handle subscription with optimized event filtering
    async fn handle_subscription(
        &self,
        state: Arc<tokio::sync::RwLock<NostrConnectionState<T>>>,
        subscription_id: String,
        filters: Vec<Filter>,
    ) -> Result<(), Error> {
        // First verify filters are allowed with read lock
        {
            let connection_state = state.read().await;
            let context = EventContext {
                authed_pubkey: connection_state.authed_pubkey.as_ref(),
                subdomain: connection_state.subdomain(),
                relay_pubkey: &self.relay_pubkey,
            };

            self.processor
                .verify_filters(&filters, connection_state.custom.clone(), context)?;
        }

        // Extract necessary state with read lock
        let (subdomain, authed_pubkey, custom_state) = {
            let connection_state = state.read().await;
            let subdomain = connection_state.subdomain().clone();
            let authed_pubkey = connection_state.authed_pubkey;
            let custom_state = connection_state.custom.clone();
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

                processor
                    .can_see_event(event, custom_state.clone(), context)
                    .unwrap_or(false)
            };

        // Get subscription service and process
        let connection_state = state.read().await;
        let subscription_service = connection_state
            .subscription_service()
            .ok_or_else(|| Error::internal("No subscription service available"))?;

        subscription_service
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

        // Initialize the subscription service for this connection
        if let Some(ref sender) = ctx.sender {
            ctx.state
                .write()
                .await
                .setup_connection(self.database.clone(), sender.clone())
                .await
                .map_err(|e| anyhow::anyhow!("Failed to setup connection: {}", e))?;
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
                // First check if processor wants to handle it
                let mut state_guard = ctx.state.write().await;
                let processor_response = self
                    .processor
                    .handle_message(
                        ClientMessage::Req {
                            subscription_id: subscription_id.clone(),
                            filter: filter.clone(),
                        },
                        &mut *state_guard,
                    )
                    .await?;
                drop(state_guard);

                if !processor_response.is_empty() {
                    // Processor handled it
                    for msg in processor_response {
                        ctx.send_message(msg)?;
                    }
                } else {
                    // Use generic subscription handling
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
                }
                ctx.next().await
            }

            ClientMessage::Close(subscription_id) => {
                // Handle CLOSE message
                {
                    let state = ctx.state.read().await;
                    if let Some(subscription_service) = state.subscription_service() {
                        let subscription_id = subscription_id.into_owned();
                        let _ = subscription_service.remove_subscription(subscription_id.clone());
                        debug!("Closed subscription: {}", subscription_id);
                    }
                }
                ctx.next().await
            }

            // Delegate all other messages to processor
            message => {
                let mut state_guard = ctx.state.write().await;
                let responses = self
                    .processor
                    .handle_message(message, &mut *state_guard)
                    .await?;
                drop(state_guard);
                for response in responses {
                    ctx.send_message(response)?;
                }
                ctx.next().await
            }
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
                let state = ctx.state.read().await;
                let subdomain = state.subdomain().clone();
                let authed_pubkey = state.authed_pubkey;
                let context = EventContext {
                    authed_pubkey: authed_pubkey.as_ref(),
                    subdomain: &subdomain,
                    relay_pubkey: &self.relay_pubkey,
                };

                !self
                    .processor
                    .can_see_event(event, state.custom.clone(), context)?
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
            let state = ctx.state.read().await;
            state.cleanup();
        }

        debug!("RelayMiddleware: Connection cleanup complete");
        ctx.next().await
    }
}
