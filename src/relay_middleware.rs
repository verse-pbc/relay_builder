//! `RelayMiddleware` for processing Nostr messages with optimized performance
//!
//! This module provides middleware that processes Nostr protocol messages while
//! delegating business logic to `EventProcessor` implementations. The implementation
//! is optimized for zero-allocation in hot paths like subscription processing.

use crate::database::RelayDatabase;
use crate::error::Error;
use crate::event_processor::EventProcessor;
use crate::nostr_middleware::{InboundContext, InboundProcessor, NostrMiddleware, OutboundContext};
use crate::state::NostrConnectionState;
use crate::subscription_coordinator::StoreCommand;
use crate::subscription_registry::SubscriptionRegistry;
use negentropy::{Id, Negentropy, NegentropyStorageVector};
use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use std::sync::Arc;
use tracing::{debug, error};

/// Relay middleware that processes messages with zero-allocation performance.
///
/// This middleware provides protocol handling (EVENT, REQ, CLOSE, AUTH) while
/// delegating business logic to `EventProcessor` implementations. The design
/// minimizes allocations in hot paths for maximum performance.
///
/// ## Key Features
///
/// - Zero-allocation event visibility checks during subscription processing
/// - Direct access to custom state without cloning
/// - Type-safe generic state management
/// - Backward compatible with full state access where needed
#[derive(Clone)]
pub struct RelayMiddleware<P, T = ()>
where
    P: EventProcessor<T>,
    T: Send + Sync + 'static,
{
    processor: Arc<P>,
    #[allow(dead_code)]
    relay_pubkey: PublicKey,
    database: Arc<RelayDatabase>,
    registry: Arc<SubscriptionRegistry>,
    max_limit: usize,
    crypto_helper: crate::crypto_helper::CryptoHelper,
    max_subscriptions: Option<usize>,
    replaceable_event_queue: flume::Sender<(UnsignedEvent, Scope)>,
    _phantom: std::marker::PhantomData<T>,
}

impl<P, T> RelayMiddleware<P, T>
where
    P: EventProcessor<T>,
    T: Clone + Send + Sync + std::fmt::Debug + Default + 'static,
{
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        processor: P,
        relay_pubkey: PublicKey,
        database: Arc<RelayDatabase>,
        registry: Arc<SubscriptionRegistry>,
        max_limit: usize,
        _relay_url: RelayUrl, // Keep for API compatibility but unused
        crypto_helper: crate::crypto_helper::CryptoHelper,
        max_subscriptions: Option<usize>,
        replaceable_event_queue: flume::Sender<(UnsignedEvent, Scope)>,
    ) -> Self {
        Self {
            processor: Arc::new(processor),
            relay_pubkey,
            database,
            registry,
            max_limit,
            crypto_helper,
            max_subscriptions,
            replaceable_event_queue,
            _phantom: std::marker::PhantomData,
        }
    }

    #[must_use]
    pub fn processor(&self) -> &Arc<P> {
        &self.processor
    }

    /// Handle EVENT messages with optimized performance
    async fn handle_event(
        &self,
        event: Event,
        state: Arc<tokio::sync::RwLock<NostrConnectionState<T>>>,
        metadata: &crate::state::ConnectionMetadata,
        message_sender: Option<crate::nostr_middleware::MessageSender>,
    ) -> Result<(), Error> {
        // Extract custom state and load current context
        let custom_state = {
            let connection_state = state.read().await;
            Arc::clone(&connection_state.custom_state)
        };

        let context = metadata.event_context.load();

        let mut commands = self
            .processor
            .handle_event(event, custom_state, &context)
            .await?;

        let subscription_coordinator = {
            let state_guard = state.read().await;
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
            let mut event_command = commands.swap_remove(idx);
            event_command.set_message_sender(message_sender.unwrap())?;
            subscription_coordinator
                .save_and_broadcast(event_command)
                .await
                .map_err(|e| Error::database(e.to_string()))?;
        }

        // Process all remaining commands without message_sender
        for command in commands {
            subscription_coordinator
                .save_and_broadcast(command)
                .await
                .map_err(|e| Error::database(e.to_string()))?;
        }

        // Database layer will send OK after persistence
        Ok(())
    }

    /// Handle subscription with optimized event filtering
    async fn handle_subscription(
        &self,
        state: Arc<tokio::sync::RwLock<NostrConnectionState<T>>>,
        metadata: &crate::state::ConnectionMetadata,
        subscription_id: String,
        filters: Vec<Filter>,
    ) -> Result<(), Error> {
        let subscription_id_obj = SubscriptionId::new(subscription_id.clone());

        // Read phase - extract custom state and load context
        let custom_state = {
            let connection_state = state.read().await;
            Arc::clone(&connection_state.custom_state)
        };

        let context = metadata.event_context.load();

        // Verify filters WITHOUT holding any lock on connection state
        self.processor
            .verify_filters(&filters, Arc::clone(&custom_state), &context)?;

        // Write phase - minimal critical section for quota only
        {
            let mut connection_state = state.write().await;
            connection_state.reserve_quota_slot(&subscription_id_obj)?;
        }

        // Clone processor and metadata for the filter closure
        let processor = Arc::clone(&self.processor);
        let event_context = Arc::clone(&metadata.event_context);

        // Create filter function that loads latest context and reuses the Arc
        let filter_fn = move |event: &Event,
                              _scope: &nostr_lmdb::Scope,
                              _auth_pk: Option<&PublicKey>|
              -> bool {
            // Load the latest context (wait-free read via ArcSwap)
            let ctx = event_context.load();

            // Reuse the same Arc<RwLock<T>> - just clone the Arc (cheap)!
            processor
                .can_see_event(event, Arc::clone(&custom_state), &ctx)
                .unwrap_or(false)
        };

        // Get subscription coordinator and process
        let subscription_coordinator = {
            let connection_state = state.read().await;
            connection_state
                .subscription_coordinator()
                .ok_or_else(|| Error::internal("No subscription coordinator available"))?
                .clone()
        };

        subscription_coordinator
            .handle_req(
                SubscriptionId::new(subscription_id),
                filters,
                context.authed_pubkey,
                &metadata.subdomain,
                filter_fn,
            )
            .await?;

        // Subscription service sends messages directly
        Ok(())
    }

    /// Handle NEG-OPEN message for negentropy synchronization
    async fn handle_neg_open(
        &self,
        state: Arc<tokio::sync::RwLock<NostrConnectionState<T>>>,
        metadata: &crate::state::ConnectionMetadata,
        subscription_id: String,
        filter: Filter,
        initial_message: String,
        sender: Option<crate::nostr_middleware::MessageSender>,
    ) -> Result<(), Error> {
        debug!(
            "Handling NEG-OPEN for subscription {} with filter: {:?}",
            subscription_id, filter
        );

        // Query database for negentropy items
        let items = self
            .database
            .negentropy_items(filter, &metadata.subdomain)
            .await?
            .into_iter()
            .collect::<Vec<_>>();

        debug!(
            "Found {} items for negentropy reconciliation in subscription {}",
            items.len(),
            subscription_id
        );

        // Create negentropy storage vector
        let mut storage = NegentropyStorageVector::new();

        // Add items to storage
        for (id, timestamp) in items {
            let id_bytes = id.to_bytes();
            storage
                .insert(timestamp.as_secs(), Id::from_byte_array(id_bytes))
                .map_err(|e| Error::internal(format!("Failed to add item to storage: {e}")))?;
        }

        // Seal the storage
        storage
            .seal()
            .map_err(|e| Error::internal(format!("Failed to seal storage: {e}")))?;

        // Create negentropy instance with 60,000 byte frame limit
        let mut negentropy = Negentropy::owned(storage, 60_000)
            .map_err(|e| Error::internal(format!("Failed to create negentropy instance: {e}")))?;

        // Perform initial reconciliation
        let hex_bytes = hex::decode(&initial_message)
            .map_err(|e| Error::internal(format!("Failed to decode initial message: {e}")))?;
        let response_bytes = negentropy
            .reconcile(&hex_bytes)
            .map_err(|e| Error::internal(format!("Failed to reconcile negentropy: {e}")))?;

        // Send response
        if let Some(sender) = sender {
            let response_message = RelayMessage::NegMsg {
                subscription_id: std::borrow::Cow::Owned(SubscriptionId::new(
                    subscription_id.clone(),
                )),
                message: std::borrow::Cow::Owned(hex::encode(response_bytes)),
            };
            sender
                .send(response_message)
                .map_err(|e| Error::internal(format!("Failed to send negentropy response: {e}")))?;
        }

        // Store negentropy instance in connection state
        {
            let mut connection_state = state.write().await;
            connection_state
                .add_negentropy_subscription(SubscriptionId::new(subscription_id), negentropy);
        }

        Ok(())
    }

    /// Handle NEG-MSG message for negentropy synchronization
    async fn handle_neg_msg(
        &self,
        state: Arc<tokio::sync::RwLock<NostrConnectionState<T>>>,
        subscription_id: String,
        message: String,
        sender: Option<crate::nostr_middleware::MessageSender>,
    ) -> Result<(), Error> {
        let subscription_id_obj = SubscriptionId::new(subscription_id.clone());

        debug!("Handling NEG-MSG for subscription {}", subscription_id);

        // Get negentropy instance from connection state
        let response_bytes = {
            let mut connection_state = state.write().await;
            match connection_state.get_negentropy_subscription_mut(&subscription_id_obj) {
                Some(negentropy) => {
                    // Decode incoming message and reconcile
                    let hex_bytes = hex::decode(&message)
                        .map_err(|e| Error::internal(format!("Failed to decode message: {e}")))?;
                    negentropy.reconcile(&hex_bytes).map_err(|e| {
                        Error::internal(format!("Failed to reconcile negentropy: {e}"))
                    })?
                }
                None => {
                    return Err(Error::notice(format!(
                        "Negentropy subscription {subscription_id} not found"
                    )));
                }
            }
        };

        // Send response
        if let Some(sender) = sender {
            let response_message = RelayMessage::NegMsg {
                subscription_id: std::borrow::Cow::Owned(subscription_id_obj),
                message: std::borrow::Cow::Owned(hex::encode(response_bytes)),
            };
            sender
                .send(response_message)
                .map_err(|e| Error::internal(format!("Failed to send negentropy response: {e}")))?;
        }

        Ok(())
    }

    /// Handle NEG-CLOSE message for negentropy synchronization
    async fn handle_neg_close(
        &self,
        state: Arc<tokio::sync::RwLock<NostrConnectionState<T>>>,
        subscription_id: String,
    ) -> Result<(), Error> {
        let subscription_id_obj = SubscriptionId::new(subscription_id.clone());

        debug!("Handling NEG-CLOSE for subscription {}", subscription_id);

        // Remove negentropy subscription from connection state
        {
            let mut connection_state = state.write().await;
            connection_state.remove_negentropy_subscription(&subscription_id_obj);
        }

        Ok(())
    }
}

impl<P, T> NostrMiddleware<T> for RelayMiddleware<P, T>
where
    P: EventProcessor<T> + Clone,
    T: Clone + Send + Sync + std::fmt::Debug + Default + 'static,
{
    fn on_connect(
        &self,
        ctx: crate::nostr_middleware::ConnectionContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            debug!("RelayMiddleware: Setting up connection");

            // Initialize state fields
            {
                let mut state_guard = ctx.state.write().await;
                state_guard.registry = Some(self.registry.clone());
                state_guard.max_subscriptions = self.max_subscriptions;
            }

            // Initialize the subscription service for this connection
            {
                let mut state_guard = ctx.state.write().await;
                state_guard
                    .setup_connection(
                        self.database.clone(),
                        self.registry.clone(),
                        ctx.connection_id.to_string(),
                        ctx.sender.clone(),
                        self.crypto_helper.clone(),
                        ctx.metadata.subdomain.clone(),
                        Some(self.max_limit),
                        self.replaceable_event_queue.clone(),
                    )
                    .map_err(|e| anyhow::anyhow!("Failed to setup connection: {}", e))?;
            }
            debug!("RelayMiddleware: Connection setup complete");
            Ok(())
        }
    }

    #[allow(clippy::too_many_lines)] // Core relay message processing logic
    fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, T, Next>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send
    where
        Next: InboundProcessor<T>,
    {
        async move {
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
                            ctx.metadata,
                            Some(ctx.sender.clone()),
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
                    filters,
                } => {
                    // Convert Vec<Cow<Filter>> to Vec<Filter>
                    let owned_filters: Vec<Filter> =
                        filters.into_iter().map(|f| f.into_owned()).collect();
                    // Use generic subscription handling directly
                    match self
                        .handle_subscription(
                            ctx.state.clone(),
                            ctx.metadata,
                            subscription_id.to_string(),
                            owned_filters,
                        )
                        .await
                    {
                        Ok(()) => {}
                        Err(e) => {
                            // Don't log Restricted errors here since they're already logged in state.rs
                            if !matches!(e, Error::Restricted { .. }) {
                                error!("Subscription error: {}", e);
                            }
                            // Propagate the error up the chain so ErrorHandlingMiddleware can format it properly
                            return Err(e.into());
                        }
                    }
                    ctx.next().await
                }

                ClientMessage::Close(subscription_id) => {
                    // Handle CLOSE message
                    let subscription_id_owned = subscription_id.into_owned();
                    let coordinator = {
                        let mut state = ctx.state.write().await;

                        // Release quota slot for this subscription
                        state.release_quota_slot(&subscription_id_owned);

                        state.subscription_coordinator().cloned()
                    };

                    if let Some(subscription_coordinator) = coordinator {
                        let _ = subscription_coordinator
                            .remove_subscription(&subscription_id_owned)
                            .await;
                        debug!("Closed subscription: {}", subscription_id_owned);
                    }

                    ctx.next().await
                }

                ClientMessage::NegOpen {
                    subscription_id,
                    filter,
                    initial_message,
                    ..
                } => {
                    // Handle NEG-OPEN message
                    match self
                        .handle_neg_open(
                            ctx.state.clone(),
                            ctx.metadata,
                            subscription_id.to_string(),
                            filter.into_owned(),
                            initial_message.into_owned(),
                            Some(ctx.sender.clone()),
                        )
                        .await
                    {
                        Ok(()) => {}
                        Err(e) => {
                            error!("Negentropy open error: {}", e);
                            // Send NEG-ERR message instead of dropping connection
                            let error_message = RelayMessage::NegErr {
                                subscription_id: std::borrow::Cow::Owned(SubscriptionId::new(
                                    subscription_id.to_string(),
                                )),
                                message: std::borrow::Cow::Owned(format!("Error: {e}")),
                            };
                            let _ = ctx.sender.send(error_message);
                        }
                    }
                    ctx.next().await
                }

                ClientMessage::NegMsg {
                    subscription_id,
                    message,
                } => {
                    // Handle NEG-MSG message
                    match self
                        .handle_neg_msg(
                            ctx.state.clone(),
                            subscription_id.to_string(),
                            message.into_owned(),
                            Some(ctx.sender.clone()),
                        )
                        .await
                    {
                        Ok(()) => {}
                        Err(e) => {
                            error!("Negentropy message error: {}", e);
                            // Send NEG-ERR message instead of dropping connection
                            let error_message = RelayMessage::NegErr {
                                subscription_id: std::borrow::Cow::Owned(SubscriptionId::new(
                                    subscription_id.to_string(),
                                )),
                                message: std::borrow::Cow::Owned(format!("Error: {e}")),
                            };
                            let _ = ctx.sender.send(error_message);
                        }
                    }
                    ctx.next().await
                }

                ClientMessage::NegClose { subscription_id } => {
                    // Handle NEG-CLOSE message
                    match self
                        .handle_neg_close(ctx.state.clone(), subscription_id.to_string())
                        .await
                    {
                        Ok(()) => {}
                        Err(e) => {
                            error!("Negentropy close error: {}", e);
                            return Err(e.into());
                        }
                    }
                    ctx.next().await
                }

                // All other messages are not handled by default
                _ => {
                    if let ClientMessage::Auth(_) = &message {
                        // if it was enabled, we would have handled it in the
                        // nip42 middleware and this point would not be reached
                        debug!(
                        "AUTH message received but authentication is not enabled on this relay, ignoring"
                    );
                        ctx.next().await
                    } else {
                        let msg = format!(
                            "Message type not supported: {}",
                            match &message {
                                ClientMessage::Count { .. } => "COUNT",
                                _ => "UNKNOWN",
                            }
                        );
                        debug!("{msg}");
                        Err(Error::notice(msg).into())
                    }
                }
            }
        }
    }

    fn process_outbound(
        &self,
        ctx: OutboundContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            let Some(message) = ctx.message.take() else {
                return Ok(());
            };

            // For broadcast events, check visibility before sending
            if let RelayMessage::Event { event, .. } = &message {
                let should_filter = {
                    let custom_state = {
                        let state = ctx.state.read().await;
                        Arc::clone(&state.custom_state)
                    };

                    // Load the latest context (wait-free)
                    let context = ctx.metadata.event_context.load();

                    // Reuse the Arc - no new allocation!
                    !self
                        .processor
                        .can_see_event(event, custom_state, &context)?
                };

                if should_filter {
                    return Ok(()); // Filter out
                }
            }

            *ctx.message = Some(message);
            Ok(())
        }
    }

    fn on_disconnect(
        &self,
        ctx: crate::nostr_middleware::DisconnectContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            debug!("RelayMiddleware: on_disconnect for {}", ctx.connection_id);

            // Perform explicit cleanup immediately when connection closes
            // No need to read the lock - connection_id is already in the context
            self.registry.cleanup_connection(ctx.connection_id).await;
            debug!(
                "RelayMiddleware: Cleaned up connection {}",
                ctx.connection_id
            );

            Ok(())
        }
    }
}
