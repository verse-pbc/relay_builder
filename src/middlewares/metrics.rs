//! Metrics middleware for tracking relay performance
//!
//! This middleware is responsible for tracking various metrics like:
//! - Active connections
//! - Event processing latency
//! - Inbound events processed
//!
//! It delegates to a pluggable metrics handler to avoid coupling to specific metrics implementations.

use crate::state::NostrConnectionState;
use anyhow::Result;
use async_trait::async_trait;
use nostr_sdk::prelude::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use websocket_builder::{
    ConnectionContext, DisconnectContext, InboundContext, Middleware, OutboundContext,
};

/// Trait for handling metrics in the relay
pub trait MetricsHandler: Send + Sync + std::fmt::Debug {
    /// Record event processing latency
    fn record_event_latency(&self, kind: u32, latency_ms: f64);

    /// Called when a connection is established
    fn increment_active_connections(&self);

    /// Called when a connection is closed
    fn decrement_active_connections(&self);

    /// Called when an inbound event is processed
    fn increment_inbound_events_processed(&self);

    /// Whether to track latency for this event (allows sampling)
    fn should_track_latency(&self) -> bool {
        true // Default to always track for backward compatibility
    }
}

/// Per-event tracking state
#[derive(Debug)]
struct EventTimingState {
    start_time: Instant,
    event_kind: u16,
}

/// Middleware that tracks metrics for relay operations
#[derive(Debug)]
pub struct MetricsMiddleware<T = ()> {
    handler: Option<Arc<dyn MetricsHandler>>,
    /// Track event timing by event ID
    event_timing: Arc<Mutex<HashMap<EventId, EventTimingState>>>,
    _phantom: std::marker::PhantomData<T>,
}

impl<T> Default for MetricsMiddleware<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> MetricsMiddleware<T> {
    /// Create a new metrics middleware without a handler (no-op)
    pub fn new() -> Self {
        Self {
            handler: None,
            event_timing: Arc::new(Mutex::new(HashMap::new())),
            _phantom: std::marker::PhantomData,
        }
    }

    /// Create a new metrics middleware with a handler
    pub fn with_handler(handler: Box<dyn MetricsHandler>) -> Self {
        Self {
            handler: Some(Arc::from(handler)),
            event_timing: Arc::new(Mutex::new(HashMap::new())),
            _phantom: std::marker::PhantomData,
        }
    }

    /// Create a new metrics middleware with an Arc handler
    pub fn with_arc_handler(handler: Arc<dyn MetricsHandler>) -> Self {
        Self {
            handler: Some(handler),
            event_timing: Arc::new(Mutex::new(HashMap::new())),
            _phantom: std::marker::PhantomData,
        }
    }
}

#[async_trait]
impl<T: Clone + Send + Sync + std::fmt::Debug + 'static> Middleware for MetricsMiddleware<T> {
    type State = NostrConnectionState<T>;
    type IncomingMessage = ClientMessage<'static>;
    type OutgoingMessage = RelayMessage<'static>;

    async fn process_inbound(
        &self,
        ctx: &mut InboundContext<Self::State, Self::IncomingMessage, Self::OutgoingMessage>,
    ) -> Result<(), anyhow::Error> {
        // Track event processing start time and increment counter
        if let Some(ClientMessage::Event(event)) = ctx.message.as_ref() {
            if let Some(handler) = &self.handler {
                // Only track timing if the handler wants it
                if handler.should_track_latency() {
                    let event_id = event.id;
                    let event_kind = event.as_ref().kind.as_u16();
                    let mut timing_map = self.event_timing.lock().unwrap();
                    timing_map.insert(
                        event_id,
                        EventTimingState {
                            start_time: Instant::now(),
                            event_kind,
                        },
                    );
                }
                handler.increment_inbound_events_processed();
            }
        }

        // Continue with the middleware chain
        ctx.next().await
    }

    async fn process_outbound(
        &self,
        ctx: &mut OutboundContext<Self::State, Self::IncomingMessage, Self::OutgoingMessage>,
    ) -> Result<(), anyhow::Error> {
        // Track event processing latency for OK responses
        if let Some(RelayMessage::Ok { event_id, .. }) = ctx.message.as_ref() {
            if let Some(handler) = &self.handler {
                let mut timing_map = self.event_timing.lock().unwrap();
                if let Some(timing_state) = timing_map.remove(event_id) {
                    let latency_ms = timing_state.start_time.elapsed().as_secs_f64() * 1000.0;
                    handler.record_event_latency(timing_state.event_kind as u32, latency_ms);
                }
            }
        }

        // Continue with the middleware chain
        ctx.next().await
    }

    async fn on_connect(
        &self,
        ctx: &mut ConnectionContext<Self::State, Self::IncomingMessage, Self::OutgoingMessage>,
    ) -> Result<(), anyhow::Error> {
        if let Some(handler) = &self.handler {
            handler.increment_active_connections();
        }

        // Continue with the middleware chain
        ctx.next().await
    }

    async fn on_disconnect(
        &self,
        ctx: &mut DisconnectContext<Self::State, Self::IncomingMessage, Self::OutgoingMessage>,
    ) -> Result<(), anyhow::Error> {
        if let Some(handler) = &self.handler {
            handler.decrement_active_connections();
        }

        // Clean up any remaining event timing entries for this connection
        // Note: In a production system, you might want to track which events
        // belong to which connection to clean up more precisely

        // Continue with the middleware chain
        ctx.next().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::RwLock;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    struct TestMetricsHandler {
        connections: Arc<Mutex<i32>>,
        events_processed: Arc<Mutex<u64>>,
        latencies: Arc<Mutex<Vec<(u32, f64)>>>,
    }

    impl MetricsHandler for TestMetricsHandler {
        fn record_event_latency(&self, kind: u32, latency_ms: f64) {
            self.latencies.lock().unwrap().push((kind, latency_ms));
        }

        fn increment_active_connections(&self) {
            *self.connections.lock().unwrap() += 1;
        }

        fn decrement_active_connections(&self) {
            *self.connections.lock().unwrap() -= 1;
        }

        fn increment_inbound_events_processed(&self) {
            *self.events_processed.lock().unwrap() += 1;
        }

        fn should_track_latency(&self) -> bool {
            true // Always track in tests
        }
    }

    fn create_test_state() -> NostrConnectionState<()> {
        NostrConnectionState::new("wss://test.relay".to_string()).expect("Valid URL")
    }

    #[tokio::test]
    async fn test_connection_tracking() {
        let handler = TestMetricsHandler::default();
        let connections = handler.connections.clone();

        let middleware = MetricsMiddleware::with_handler(Box::new(handler));
        let chain = vec![Arc::new(middleware)
            as Arc<
                dyn Middleware<
                    State = NostrConnectionState<()>,
                    IncomingMessage = ClientMessage<'static>,
                    OutgoingMessage = RelayMessage<'static>,
                >,
            >];

        let state = create_test_state();
        let state_arc = Arc::new(RwLock::new(state));
        let chain_arc = Arc::new(chain.clone());

        // Test connection
        let mut ctx = ConnectionContext::new(
            "test_connection".to_string(),
            None,
            state_arc.clone(),
            chain_arc.clone(),
            0,
        );

        chain[0].on_connect(&mut ctx).await.unwrap();
        assert_eq!(*connections.lock().unwrap(), 1);

        // Test disconnection
        let mut ctx =
            DisconnectContext::new("test_connection".to_string(), None, state_arc, chain_arc, 0);

        chain[0].on_disconnect(&mut ctx).await.unwrap();
        assert_eq!(*connections.lock().unwrap(), 0);
    }
}
