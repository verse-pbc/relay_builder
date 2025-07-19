//! WebSocket handler implementation for Nostr relays
//!
//! This module provides the bridge between websocket_builder's simple text-based
//! WebSocket API and relay_builder's Nostr-specific middleware system.

use crate::config::ScopeConfig;
use crate::event_ingester::{EventIngester, IngesterError};
use crate::middleware_chain::NostrChainBuilder;
use crate::nostr_middleware::{
    InboundProcessor, NostrMessageSender, NostrMiddleware, OutboundProcessor,
};
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
    /// Phantom data for the custom state type
    _phantom: std::marker::PhantomData<T>,
}

impl<T, Chain> NostrHandlerFactory<T, Chain> {
    pub fn new(chain: Chain, event_ingester: EventIngester, scope_config: ScopeConfig) -> Self {
        Self {
            chain,
            event_ingester,
            scope_config,
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T, Chain> HandlerFactory for NostrHandlerFactory<T, Chain>
where
    T: Clone + Send + Sync + std::fmt::Debug + Default + 'static,
    Chain: NostrMiddleware<T> + InboundProcessor<T> + OutboundProcessor<T>,
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

        NostrConnection {
            chain: self.chain.clone(),
            event_ingester: self.event_ingester.clone(),
            subdomain: subdomain.unwrap_or_else(|| Arc::new(Scope::Default)),
            state: None,
            addr: None,
            inbound_task: None,
            outbound_tx: None,
            outbound_rx: None,
        }
    }
}

/// WebSocket connection handler for Nostr
pub struct NostrConnection<T, Chain> {
    /// The middleware chain
    chain: Chain,
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
    outbound_tx: Option<flume::Sender<(RelayMessage<'static>, usize, Option<String>)>>,
    /// Receiver for outbound messages from middleware
    outbound_rx: Option<flume::Receiver<(RelayMessage<'static>, usize, Option<String>)>>,
}

impl<T, Chain> WebSocketHandler for NostrConnection<T, Chain>
where
    T: Clone + Send + Sync + std::fmt::Debug + Default + 'static,
    Chain: NostrMiddleware<T> + InboundProcessor<T> + OutboundProcessor<T>,
{
    async fn on_connect(
        &mut self,
        addr: SocketAddr,
        sink: SplitSink<WebSocket, Message>,
    ) -> Result<()> {
        self.addr = Some(addr);

        let relay_url = RelayUrl::parse(&format!("ws://{addr}"))
            .unwrap_or_else(|_| RelayUrl::parse("ws://localhost").expect("Valid fallback URL"));
        let state = Arc::new(parking_lot::RwLock::new(
            NostrConnectionState::<T>::with_subdomain(relay_url, self.subdomain.clone())
                .expect("Valid state"),
        ));
        self.state = Some(state.clone());

        let (outbound_tx, outbound_rx) = flume::unbounded();
        self.outbound_tx = Some(outbound_tx.clone());
        self.outbound_rx = Some(outbound_rx.clone());

        // Clone what we need for the outbound task
        let chain = self.chain.clone();
        let state_clone = state.clone();
        let addr_string = addr.to_string();

        tokio::spawn(async move {
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

                    let positioned_sender = NostrMessageSender::new(outbound_tx.clone(), 0);

                    let mut message_opt = Some(message);
                    if let Err(e) = OutboundProcessor::process_outbound(
                        &chain,
                        &addr_string,
                        &mut message_opt,
                        &state_clone,
                        &positioned_sender,
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
                        tracing::error!("Failed to send message: {}", e);
                        return; // Exit on error
                    }
                }
            }
        });

        // Call middleware on_connect through InboundProcessor
        let Some(outbound_tx_for_connect) = &self.outbound_tx else {
            return Err(anyhow::anyhow!("No outbound sender"));
        };
        let positioned_sender =
            crate::nostr_middleware::NostrMessageSender::new(outbound_tx_for_connect.clone(), 0);

        InboundProcessor::on_connect(&self.chain, &addr.to_string(), &state, &positioned_sender)
            .await?;

        Ok(())
    }

    async fn on_message(&mut self, text: Utf8Bytes) -> Result<()> {
        let Some(state) = &self.state else {
            return Err(anyhow::anyhow!("No connection state"));
        };

        let Some(addr) = &self.addr else {
            return Err(anyhow::anyhow!("No remote address"));
        };

        let Some(outbound_tx) = &self.outbound_tx else {
            return Err(anyhow::anyhow!("No outbound sender"));
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
                        let _ = outbound_tx.send((error_msg, 0, None));
                    }
                    IngesterError::SignatureVerificationFailed(event_id) => {
                        let ok_msg = RelayMessage::ok(
                            event_id,
                            false,
                            "invalid: event signature verification failed",
                        );
                        let _ = outbound_tx.send((ok_msg, 0, None));
                    }
                    IngesterError::InvalidUtf8 => {
                        let error_msg = RelayMessage::notice("Invalid UTF-8 in message");
                        let _ = outbound_tx.send((error_msg, 0, None));
                    }
                    IngesterError::MessageTooLarge => {
                        let error_msg = RelayMessage::notice("Message size exceeds limit");
                        let _ = outbound_tx.send((error_msg, 0, None));
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

        let Some(outbound_tx) = &self.outbound_tx else {
            return Err(anyhow::anyhow!("No outbound sender"));
        };

        let mut message_opt = Some(message);
        let sender = crate::nostr_middleware::NostrMessageSender::new(outbound_tx.clone(), 0);

        match InboundProcessor::process_inbound(
            &self.chain,
            &addr.to_string(),
            &mut message_opt,
            state,
            &sender,
        )
        .await
        {
            Ok(_) => Ok(()),
            Err(e) => {
                // Log error but don't disconnect
                tracing::error!("Inbound processing error: {}", e);

                if let Some(nostr_error) = e.downcast_ref::<crate::error::Error>() {
                    let notice = RelayMessage::notice(nostr_error.to_string());
                    let _ = outbound_tx.send((notice, 0, None));
                }

                Ok(())
            }
        }
    }

    async fn on_disconnect(&mut self, reason: DisconnectReason) {
        tracing::debug!("Connection disconnected: {:?}", reason);

        if let (Some(state), Some(addr)) = (&self.state, &self.addr) {
            // Call on_disconnect through OutboundProcessor (no sender needed)
            let _ = OutboundProcessor::on_disconnect(&self.chain, &addr.to_string(), state).await;
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
    ) -> NostrHandlerFactory<T, Chain>;
}

impl<T, Chain> IntoHandlerFactory<T, Chain> for NostrChainBuilder<T, Chain>
where
    T: Clone + Send + Sync + std::fmt::Debug + Default + 'static,
    Chain: NostrMiddleware<T> + InboundProcessor<T> + OutboundProcessor<T>,
{
    fn into_handler_factory(
        self,
        event_ingester: EventIngester,
        scope_config: ScopeConfig,
    ) -> NostrHandlerFactory<T, Chain> {
        NostrHandlerFactory::new(self.build(), event_ingester, scope_config)
    }
}

// Re-export for convenience
pub type Result<T> = std::result::Result<T, anyhow::Error>;
