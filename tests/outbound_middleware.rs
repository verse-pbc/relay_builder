//! Tests for outbound middleware processing
//!
//! These tests verify that the outbound middleware chain is working correctly
//! after fixing the broken implementation.

use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use relay_builder::{
    middleware_chain::{chain, BuildConnected},
    middlewares::*,
    nostr_middleware::{MessageSender, NostrMiddleware, OutboundContext, OutboundProcessor},
    state::NostrConnectionState,
};
use std::{
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

/// Test middleware that tracks when process_outbound is called
#[derive(Clone)]
struct TrackingMiddleware {
    name: String,
    process_outbound_called: Arc<AtomicBool>,
    call_count: Arc<AtomicUsize>,
}

impl TrackingMiddleware {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            process_outbound_called: Arc::new(AtomicBool::new(false)),
            call_count: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn was_called(&self) -> bool {
        self.process_outbound_called.load(Ordering::Relaxed)
    }

    #[allow(dead_code)]
    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::Relaxed)
    }
}

impl<T> NostrMiddleware<T> for TrackingMiddleware
where
    T: Send + Sync + Clone + 'static,
{
    async fn process_outbound(&self, _ctx: OutboundContext<'_, T>) -> Result<(), anyhow::Error> {
        println!("TrackingMiddleware[{}] process_outbound called", self.name);
        self.process_outbound_called.store(true, Ordering::Relaxed);
        self.call_count.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

/// Test middleware that modifies outbound messages
#[derive(Clone)]
struct ModifyingMiddleware {
    suffix: String,
}

impl ModifyingMiddleware {
    fn new(suffix: &str) -> Self {
        Self {
            suffix: suffix.to_string(),
        }
    }
}

impl<T> NostrMiddleware<T> for ModifyingMiddleware
where
    T: Send + Sync + Clone + 'static,
{
    async fn process_outbound(&self, ctx: OutboundContext<'_, T>) -> Result<(), anyhow::Error> {
        if let Some(RelayMessage::Notice(notice)) = ctx.message.as_ref() {
            // Create a new message with the suffix appended
            let new_msg = format!("{}{}", notice, self.suffix);
            *ctx.message = Some(RelayMessage::notice(new_msg));
        }
        Ok(())
    }
}

/// Test helper struct that holds the context data
struct TestContext<T> {
    connection_id: String,
    message: Option<RelayMessage<'static>>,
    state: Arc<RwLock<NostrConnectionState<T>>>,
    sender: MessageSender,
    _raw_sender: flume::Sender<(RelayMessage<'static>, usize, Option<String>)>,
}

impl<T> TestContext<T> {
    fn new(message: RelayMessage<'static>) -> Self
    where
        T: Default,
    {
        let (tx, _rx) = flume::bounded(10);
        let sender = MessageSender::new(tx.clone(), 0);
        Self {
            connection_id: "test_conn".to_string(),
            message: Some(message),
            state: Arc::new(RwLock::new(NostrConnectionState::<T>::default())),
            sender,
            _raw_sender: tx,
        }
    }

    fn as_context(&mut self) -> OutboundContext<'_, T> {
        OutboundContext {
            connection_id: &self.connection_id,
            message: &mut self.message,
            state: &self.state,
            sender: &self.sender,
        }
    }

    // Helper to call chain.process_outbound through OutboundProcessor trait
    async fn process_through_chain<C>(
        &mut self,
        chain: &C,
        from_position: usize,
    ) -> Result<(), anyhow::Error>
    where
        C: OutboundProcessor<T>,
    {
        chain
            .process_outbound_chain(
                &self.connection_id,
                &mut self.message,
                &self.state,
                &self.sender,
                from_position,
            )
            .await
    }
}

#[tokio::test]
async fn test_logger_middleware_outbound_processing() {
    // Create logger middleware
    let logger = NostrLoggerMiddleware::<()>::new();

    // Create a test message
    let message = RelayMessage::notice("Test notice");
    let mut test_ctx = TestContext::<()>::new(message);

    // Process the message - should log it
    let ctx = test_ctx.as_context();
    assert!(logger.process_outbound(ctx).await.is_ok());

    // The message should still be present (logger doesn't modify)
    assert!(test_ctx.message.is_some());
}

#[tokio::test]
async fn test_nip40_expiration_filters_expired_events() {
    let middleware = Nip40ExpirationMiddleware::new();

    // Create an expired event
    let keys = Keys::generate();
    let expired_event = EventBuilder::new(Kind::Custom(1234), "expired content")
        .tags([Tag::expiration(
            Timestamp::now() - Duration::from_secs(3600),
        )])
        .build(keys.public_key());
    let expired_event = keys.sign_event(expired_event).await.unwrap();

    let message = RelayMessage::Event {
        subscription_id: std::borrow::Cow::Owned(SubscriptionId::new("test_sub")),
        event: std::borrow::Cow::Owned(expired_event),
    };

    let mut test_ctx = TestContext::<()>::new(message);

    // Process the message
    let ctx = test_ctx.as_context();
    assert!(middleware.process_outbound(ctx).await.is_ok());

    // The message should be filtered out (set to None)
    assert!(test_ctx.message.is_none());
}

#[tokio::test]
async fn test_metrics_middleware_tracks_ok_responses() {
    // The metrics middleware only tracks latency for OK responses
    // if there was a corresponding inbound event that started the timing.
    // Since we're testing outbound processing in isolation,
    // let's verify the middleware processes OK messages without errors.

    let middleware = MetricsMiddleware::<()>::new();

    // Create an OK message
    let event_id =
        EventId::from_hex("70b10f70c1318967eddf12527799411b1a9780ad9c43858f5e5fcd45486a13a5")
            .unwrap();
    let message = RelayMessage::ok(event_id, true, "stored");

    let mut test_ctx = TestContext::<()>::new(message);

    // Process the message - should complete without errors
    let ctx = test_ctx.as_context();
    assert!(middleware.process_outbound(ctx).await.is_ok());

    // The message should still be present (metrics doesn't modify)
    assert!(test_ctx.message.is_some());
    if let Some(RelayMessage::Ok { .. }) = test_ctx.message {
        // OK message processed successfully
    } else {
        panic!("Expected OK message");
    }
}

#[tokio::test]
async fn test_tracking_middleware_direct() {
    // Test that TrackingMiddleware works when called directly
    let tracker = TrackingMiddleware::new("direct");

    let message = RelayMessage::notice("Test");
    let mut test_ctx = TestContext::<()>::new(message);

    let ctx = test_ctx.as_context();
    assert!(tracker.process_outbound(ctx).await.is_ok());
    assert!(tracker.was_called(), "Tracker should be called");
}

#[tokio::test]
async fn test_simple_chain_from_inner() {
    // Test a simple case where inner middleware sends a message
    let outer = TrackingMiddleware::new("outer");
    let inner = TrackingMiddleware::new("inner");

    // Build chain: outer (pos 1) -> inner (pos 0)
    let chain_blueprint = chain::<()>()
        .with(inner.clone()) // position 0
        .with(outer.clone()) // position 1
        .build();

    // Build connected chain from blueprint
    let (tx, _rx) = flume::unbounded();
    let chain = chain_blueprint.build_connected(tx);

    let message = RelayMessage::notice("Test from inner");
    let mut test_ctx = TestContext::<()>::new(message);

    // Process with from_position = 0 (inner middleware sends)
    println!("About to call chain.process_outbound with from_position=0");
    let result = test_ctx.process_through_chain(&chain, 0).await;
    println!("chain.process_outbound returned: {result:?}");
    assert!(result.is_ok());

    // Both should be called when inner sends
    assert!(inner.was_called(), "Inner should be called (originator)");
    assert!(
        outer.was_called(),
        "Outer should be called (from_index < position)"
    );
}

#[tokio::test]
async fn test_middleware_chain_ordering() {
    // Create tracking middlewares with different positions
    let outer = TrackingMiddleware::new("outer");
    let middle = TrackingMiddleware::new("middle");
    let inner = TrackingMiddleware::new("inner");

    // Build chain: outer (pos 2) -> middle (pos 1) -> inner (pos 0)
    let chain_blueprint = chain::<()>()
        .with(inner.clone()) // position 0
        .with(middle.clone()) // position 1
        .with(outer.clone()) // position 2
        .build();

    // Build connected chain from blueprint
    let (tx, _rx) = flume::unbounded();
    let chain = chain_blueprint.build_connected(tx);

    // Create a message sent from the middle middleware (position 1)
    let message = RelayMessage::notice("Test");
    let mut test_ctx = TestContext::<()>::new(message);

    // Process with from_position = 1 (middle middleware)
    // The chain structure is: outer -> middle -> inner -> End
    // When from_position = 1:
    // - outer (pos 2): from_position (1) < position (2), processes through next then itself
    // - middle (pos 1): from_position (1) <= position (1), processes through next then itself
    // - inner (pos 0): from_position (1) > position (0), skips
    assert!(test_ctx.process_through_chain(&chain, 1).await.is_ok());

    // Middle and outer should process
    assert!(
        outer.was_called(),
        "Outer should be called (from_index < position)"
    );
    assert!(middle.was_called(), "Middle should be called (originator)");
    assert!(
        !inner.was_called(),
        "Inner should not be called (from_index > position)"
    );

    // Test a message from inner (position 0)
    inner
        .process_outbound_called
        .store(false, Ordering::Relaxed);
    middle
        .process_outbound_called
        .store(false, Ordering::Relaxed);
    outer
        .process_outbound_called
        .store(false, Ordering::Relaxed);

    let message2 = RelayMessage::notice("Test2");
    let mut test_ctx2 = TestContext::<()>::new(message2);
    assert!(test_ctx2.process_through_chain(&chain, 0).await.is_ok());

    // When from_index = 0 (inner), both middle and outer should process
    assert!(inner.was_called(), "Inner should be called (originator)");
    assert!(
        middle.was_called(),
        "Middle should be called (from_index < position)"
    );
    assert!(
        outer.was_called(),
        "Outer should be called (from_index < position)"
    );
}

#[tokio::test]
async fn test_message_modification_chain() {
    // Create modifying middlewares
    let first = ModifyingMiddleware::new(" [first]");
    let second = ModifyingMiddleware::new(" [second]");

    // Build chain: first -> second
    let chain_blueprint = chain::<()>()
        .with(second.clone()) // position 0
        .with(first.clone()) // position 1
        .build();

    // Build connected chain from blueprint
    let (tx, _rx) = flume::unbounded();
    let chain = chain_blueprint.build_connected(tx);

    // Create a notice message
    let message = RelayMessage::notice("Original");
    let mut test_ctx = TestContext::<()>::new(message);

    // Process from innermost (position 0)
    // When from_position = 0:
    // - second (pos 0): from_position <= position, processes through next then itself
    // - first (pos 1): from_position <= position, processes through next then itself
    assert!(test_ctx.process_through_chain(&chain, 0).await.is_ok());

    // Check the modified message
    if let Some(RelayMessage::Notice(notice)) = &test_ctx.message {
        // When sent from position 0, it goes through:
        // 1. second adds " [second]"
        // 2. first adds " [first]"
        assert_eq!(notice.as_ref(), "Original [second] [first]");
    } else {
        panic!("Expected modified notice message");
    }
}

#[tokio::test]
async fn test_json_invalidation_on_event_modification() {
    // This test verifies that the pre-serialized JSON is invalidated
    // when an event is modified by middleware

    // Create a custom middleware that modifies events
    #[derive(Clone)]
    struct EventModifierMiddleware;

    impl<T> NostrMiddleware<T> for EventModifierMiddleware
    where
        T: Send + Sync + Clone + 'static,
    {
        async fn process_outbound(&self, ctx: OutboundContext<'_, T>) -> Result<(), anyhow::Error> {
            if let Some(RelayMessage::Event {
                event: _,
                subscription_id,
            }) = ctx.message.take()
            {
                // Create a new event with different content
                let keys = Keys::generate();
                let new_event = EventBuilder::new(Kind::Custom(9999), "modified content")
                    .build(keys.public_key());
                let new_event = keys.sign_event(new_event).await?;

                *ctx.message = Some(RelayMessage::Event {
                    subscription_id,
                    event: std::borrow::Cow::Owned(new_event),
                });
            }
            Ok(())
        }
    }

    // This would be tested in the actual nostr_handler, but we can verify
    // that our middleware can modify events
    let modifier = EventModifierMiddleware;

    let keys = Keys::generate();
    let original_event =
        EventBuilder::new(Kind::Custom(1234), "original content").build(keys.public_key());
    let original_event = keys.sign_event(original_event).await.unwrap();
    let original_id = original_event.id;

    let message = RelayMessage::Event {
        subscription_id: std::borrow::Cow::Owned(SubscriptionId::new("test")),
        event: std::borrow::Cow::Owned(original_event),
    };

    let mut test_ctx = TestContext::<()>::new(message);

    // Process the message
    let ctx = test_ctx.as_context();
    assert!(modifier.process_outbound(ctx).await.is_ok());

    // Verify the event was modified
    if let Some(RelayMessage::Event { event, .. }) = &test_ctx.message {
        assert_ne!(event.id, original_id, "Event should have been modified");
        assert_eq!(event.kind, Kind::Custom(9999));
    } else {
        panic!("Expected event message");
    }
}
