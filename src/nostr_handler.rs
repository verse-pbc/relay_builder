//! WebSocket handler implementation for Nostr relays
//!
//! This module provides the bridge between websocket_builder's simple text-based
//! WebSocket API and relay_builder's Nostr-specific middleware system.

use crate::config::ScopeConfig;
use crate::event_ingester::{EventIngester, IngesterError};
use crate::middleware_chain::NostrChainBuilder;
use crate::nostr_middleware::{InboundProcessor, OutboundProcessor};
use crate::state::NostrConnectionState;
use axum::extract::ws::{Message, WebSocket};
use futures_util::stream::{self, SplitSink, StreamExt};
use futures_util::SinkExt;
use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use rayon::prelude::*;
use std::net::SocketAddr;
use std::sync::Arc;
use websocket_builder::{DisconnectReason, HandlerFactory, Utf8Bytes, WebSocketHandler};

/// Factory for creating Nostr connection handlers
#[derive(Clone)]
pub struct NostrHandlerFactory<T, Chain> {
    /// The middleware chain to use for each connection
    chain: Chain,
    /// Event ingester for JSON parsing and signature verification
    event_ingester: EventIngester,
    /// Scope configuration for subdomain extraction
    scope_config: ScopeConfig,
    /// Size for the outbound message channel
    channel_size: usize,
    /// Phantom data for the custom state type
    _phantom: std::marker::PhantomData<T>,
}

impl<T, Chain> NostrHandlerFactory<T, Chain> {
    pub fn new(
        chain: Chain,
        event_ingester: EventIngester,
        scope_config: ScopeConfig,
        channel_size: usize,
    ) -> Self {
        Self {
            chain,
            event_ingester,
            scope_config,
            channel_size,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T, Chain> HandlerFactory for NostrHandlerFactory<T, Chain>
where
    T: Clone + Send + Sync + std::fmt::Debug + Default + 'static,
    Chain: crate::middleware_chain::BuildConnected + Clone + Send + Sync + 'static,
    Chain::Output: InboundProcessor<T> + OutboundProcessor<T> + Clone + Send + Sync + 'static,
{
    type Handler = NostrConnection<T, Chain>;

    fn create(&self, headers: &axum::http::HeaderMap) -> Self::Handler {
        // Extract subdomain from headers based on scope_config
        let subdomain = if let Some(host) = headers.get("host").and_then(|h| h.to_str().ok()) {
            match &self.scope_config {
                ScopeConfig::Subdomain { base_domain_parts } => {
                    crate::subdomain::extract_subdomain(host, *base_domain_parts)
                        .and_then(|s| Scope::named(&s).ok())
                        .map(Arc::new)
                }
                _ => None,
            }
        } else {
            None
        };

        let (outbound_tx, outbound_rx) = flume::bounded(self.channel_size);

        NostrConnection {
            chain_blueprint: Some(self.chain.clone()),
            connected_chain: None,
            event_ingester: self.event_ingester.clone(),
            subdomain: subdomain.unwrap_or_else(|| Arc::new(Scope::Default)),
            state: None,
            addr: None,
            inbound_task: None,
            outbound_tx,
            outbound_rx,
            connection_span: None,
        }
    }
}

/// WebSocket connection handler for Nostr
pub struct NostrConnection<T, Chain>
where
    Chain: crate::middleware_chain::BuildConnected + Send + Sync,
    Chain::Output: Send + Sync,
{
    /// The middleware chain blueprint (will be transformed to connected chain)
    chain_blueprint: Option<Chain>,
    /// The connected chain (created from blueprint on connect)
    connected_chain: Option<Chain::Output>,
    /// Event ingester for JSON parsing and signature verification
    event_ingester: EventIngester,
    /// The subdomain scope for this connection
    subdomain: Arc<Scope>,
    /// Connection state
    state: Option<Arc<parking_lot::RwLock<NostrConnectionState<T>>>>,
    /// Remote address
    addr: Option<SocketAddr>,
    /// Handle to the inbound processing task
    inbound_task: Option<tokio::task::JoinHandle<()>>,
    /// Sender for outbound messages from middleware (with index tracking and optional pre-serialized JSON)
    outbound_tx: flume::Sender<(RelayMessage<'static>, usize, Option<String>)>,
    /// Receiver for outbound messages from middleware
    outbound_rx: flume::Receiver<(RelayMessage<'static>, usize, Option<String>)>,
    /// Span for tracing this connection
    connection_span: Option<tracing::Span>,
}

impl<T, Chain> WebSocketHandler for NostrConnection<T, Chain>
where
    T: Clone + Send + Sync + std::fmt::Debug + Default + 'static,
    Chain: crate::middleware_chain::BuildConnected + Clone + Send + Sync + 'static,
    Chain::Output: InboundProcessor<T> + OutboundProcessor<T> + Clone + Send + Sync + 'static,
{
    async fn on_connect(
        &mut self,
        addr: SocketAddr,
        sink: SplitSink<WebSocket, Message>,
    ) -> Result<()> {
        self.addr = Some(addr);

        // Create a connection ID for tracking
        let connection_id = uuid::Uuid::new_v4().to_string();

        // Create span for this connection
        let span = tracing::info_span!(
            "websocket_connection",
            connection_id = %connection_id,
            ip = %addr,
            subdomain = ?self.subdomain
        );
        let _enter = span.enter();

        // Store the span for use in other methods
        self.connection_span = Some(span.clone());

        let relay_url = RelayUrl::parse(&format!("ws://{addr}"))
            .unwrap_or_else(|_| RelayUrl::parse("ws://localhost").expect("Valid fallback URL"));
        let state = Arc::new(parking_lot::RwLock::new(
            NostrConnectionState::<T>::with_subdomain(relay_url, self.subdomain.clone())
                .expect("Valid state"),
        ));
        self.state = Some(state.clone());

        // Build the connected chain from the blueprint
        let blueprint = self
            .chain_blueprint
            .take()
            .expect("Chain blueprint already consumed");
        let connected_chain = blueprint.build_connected(self.outbound_tx.clone());
        self.connected_chain = Some(connected_chain.clone());

        // Clone what we need for the outbound task
        let chain = connected_chain.clone();
        let state_clone = state.clone();
        let addr_string = addr.to_string();
        let outbound_rx = self.outbound_rx.clone();

        // Spawn the outbound processing task
        let outbound_span = span.clone();
        tokio::spawn(async move {
            let _enter = outbound_span.enter();
            // Buffer size for ready chunks - adjust based on your needs
            let buffer_size = 10;
            let mut sink = sink;

            let mut message_stream = Box::pin(
                stream::unfold(outbound_rx, |rx| async move {
                    rx.recv_async()
                        .await
                        .ok()
                        .map(|(msg, idx, json)| ((msg, idx, json), rx))
                })
                // Collect messages into chunks for parallel processing
                .ready_chunks(buffer_size),
            );

            while let Some(message_batch) = message_stream.next().await {
                let mut processed_messages = Vec::new();

                for (message, from_index, pre_json) in message_batch {
                    // Capture original event ID for invalidation check (if it's an EVENT message)
                    let original_event_id = if let RelayMessage::Event { event, .. } = &message {
                        Some(event.id)
                    } else {
                        None
                    };

                    let mut message_opt = Some(message);
                    if let Err(e) = chain
                        .process_outbound_chain(
                            &addr_string,
                            &mut message_opt,
                            &state_clone,
                            from_index,
                        )
                        .await
                    {
                        tracing::error!("Outbound middleware processing error: {}", e);
                        continue;
                    }

                    // If message survived processing, add to batch
                    if let Some(msg) = message_opt {
                        // Middleware changed the event - invalidate pre-serialized JSON
                        let json_to_use =
                            if let (Some(original_id), RelayMessage::Event { event, .. }) =
                                (original_event_id, &msg)
                            {
                                if original_id != event.id {
                                    // Event was modified, invalidate pre-serialized JSON
                                    None
                                } else {
                                    // Event unchanged, can use pre-serialized JSON
                                    pre_json
                                }
                            } else {
                                // Not an event message or no original event, use pre-json as-is
                                pre_json
                            };

                        processed_messages.push((msg, json_to_use));
                    }
                }

                // Serialize messages in parallel using rayon (only if not pre-serialized)
                let json_strings: Vec<String> = processed_messages
                    .into_par_iter()
                    .map(|(message, pre_serialized)| {
                        // Use pre-serialized JSON if available, otherwise serialize
                        pre_serialized.unwrap_or_else(|| message.as_json())
                    })
                    .collect();

                for json in json_strings {
                    if let Err(e) = sink.send(Message::Text(json.into())).await {
                        // Use debug level for WebSocket errors after close/timeout
                        if e.to_string().contains("Sending after closing")
                            || e.to_string().contains("WebSocket protocol error")
                        {
                            tracing::debug!("WebSocket closed: {}", e);
                        } else {
                            tracing::error!("Failed to send message: {}", e);
                        }
                        return; // Exit on error
                    }
                }
            }
        });

        // Now that the outbound task is running, call on_connect on all middlewares
        // They can safely send messages now
        // Use the connected chain for on_connect
        let connected_chain = self
            .connected_chain
            .as_ref()
            .expect("Connected chain should be set");

        connected_chain
            .on_connect_chain(&addr.to_string(), &state)
            .await?;

        Ok(())
    }

    async fn on_message(&mut self, text: Utf8Bytes) -> Result<()> {
        // Enter the connection span for this message
        let _guard = self.connection_span.as_ref().map(|s| s.enter());

        let Some(state) = &self.state else {
            return Err(anyhow::anyhow!("No connection state"));
        };

        let Some(addr) = &self.addr else {
            return Err(anyhow::anyhow!("No remote address"));
        };

        // EventIngester handles JSON parsing + signature verification
        let message = match self
            .event_ingester
            .process_message(text.as_bytes().to_vec())
            .await
        {
            Ok(msg) => msg,
            Err(e) => {
                match e {
                    IngesterError::JsonParseError(parse_err) => {
                        let error_msg =
                            RelayMessage::notice(format!("Invalid message format: {parse_err}"));
                        let _ = self.outbound_tx.send((error_msg, 0, None));
                    }
                    IngesterError::SignatureVerificationFailed(event_id) => {
                        let ok_msg = RelayMessage::ok(
                            event_id,
                            false,
                            "invalid: event signature verification failed",
                        );
                        let _ = self.outbound_tx.send((ok_msg, 0, None));
                    }
                    IngesterError::InvalidUtf8 => {
                        let error_msg = RelayMessage::notice("Invalid UTF-8 in message");
                        let _ = self.outbound_tx.send((error_msg, 0, None));
                    }
                    IngesterError::MessageTooLarge => {
                        let error_msg = RelayMessage::notice("Message size exceeds limit");
                        let _ = self.outbound_tx.send((error_msg, 0, None));
                    }
                    IngesterError::ChannelClosed => {
                        // Log and return error for channel issues
                        tracing::error!("EventIngester channel closed");
                        return Err(anyhow::anyhow!("EventIngester channel closed"));
                    }
                }
                return Ok(());
            }
        };

        let mut message_opt = Some(message);

        // Use the connected chain for processing
        let connected_chain = self
            .connected_chain
            .as_ref()
            .expect("Connected chain should be set");

        match connected_chain
            .process_inbound_chain(&addr.to_string(), &mut message_opt, state)
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => {
                // Log error but don't disconnect
                tracing::error!("Inbound processing error: {}", e);

                if let Some(nostr_error) = e.downcast_ref::<crate::error::Error>() {
                    let notice = RelayMessage::notice(nostr_error.to_string());
                    let _ = self.outbound_tx.send((notice, 0, None));
                }

                Ok(())
            }
        }
    }

    async fn on_disconnect(&mut self, reason: DisconnectReason) {
        // Enter the connection span for disconnect
        let _guard = self.connection_span.as_ref().map(|s| s.enter());

        tracing::debug!("Connection disconnected: {:?}", reason);

        if let (Some(state), Some(addr)) = (&self.state, &self.addr) {
            // Use the connected chain for on_disconnect
            if let Some(connected_chain) = &self.connected_chain {
                let _ = connected_chain
                    .on_disconnect_chain(&addr.to_string(), state)
                    .await;
            }
        }

        // Cancel inbound task if running
        if let Some(task) = self.inbound_task.take() {
            task.abort();
        }
    }
}

/// Extension trait for NostrChainBuilder to create a handler factory
pub trait IntoHandlerFactory<T, Chain> {
    /// Convert the chain into a handler factory with an event ingester
    fn into_handler_factory(
        self,
        event_ingester: EventIngester,
        scope_config: ScopeConfig,
        channel_size: usize,
    ) -> NostrHandlerFactory<T, Chain>;
}

impl<T, Chain> IntoHandlerFactory<T, Chain> for NostrChainBuilder<T, Chain>
where
    T: Clone + Send + Sync + std::fmt::Debug + Default + 'static,
    Chain: crate::middleware_chain::BuildConnected + Clone + Send + Sync + 'static,
    Chain::Output: InboundProcessor<T> + OutboundProcessor<T> + Clone + Send + Sync + 'static,
{
    fn into_handler_factory(
        self,
        event_ingester: EventIngester,
        scope_config: ScopeConfig,
        channel_size: usize,
    ) -> NostrHandlerFactory<T, Chain> {
        NostrHandlerFactory::new(self.build(), event_ingester, scope_config, channel_size)
    }
}

// Re-export for convenience
pub type Result<T> = std::result::Result<T, anyhow::Error>;
