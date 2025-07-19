//! Metrics middleware for tracking relay performance
//!
//! This middleware is responsible for tracking various metrics like:
//! - Active connections
//! - Event processing latency
//! - Inbound events processed
//!
//! It delegates to a pluggable metrics handler to avoid coupling to specific metrics implementations.

use crate::nostr_middleware::{
    DisconnectContext, InboundContext, InboundProcessor, NostrMiddleware, OutboundContext,
};
use anyhow::Result;
use nostr_sdk::prelude::*;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

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
#[derive(Debug, Clone)]
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

impl<T> NostrMiddleware<T> for MetricsMiddleware<T>
where
    T: Send + Sync + Clone + 'static,
{
    fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, T, Next>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send
    where
        Next: InboundProcessor<T>,
    {
        async move {
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
    }

    fn process_outbound(
        &self,
        ctx: OutboundContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
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

            // Outbound processing doesn't call next() - runs sequentially
            Ok(())
        }
    }

    fn on_disconnect(
        &self,
        _ctx: DisconnectContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            if let Some(handler) = &self.handler {
                handler.decrement_active_connections();
            }

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Debug, Default)]
    #[allow(dead_code)]
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

    // TODO: Update tests once test_utils are refactored for the new API
}
