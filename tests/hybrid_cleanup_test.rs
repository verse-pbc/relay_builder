use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use relay_builder::metrics::SubscriptionMetricsHandler;
use relay_builder::nostr_middleware::MessageSender;
use relay_builder::subscription_registry::{EventDistributor, SubscriptionRegistry};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

/// Test metrics handler that tracks subscription counts
#[derive(Debug, Clone)]
struct TestMetricsHandler {
    active_subscriptions: Arc<AtomicUsize>,
}

impl TestMetricsHandler {
    fn new() -> Self {
        Self {
            active_subscriptions: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn get_count(&self) -> usize {
        self.active_subscriptions.load(Ordering::SeqCst)
    }
}

impl SubscriptionMetricsHandler for TestMetricsHandler {
    fn increment_active_subscriptions(&self) {
        self.active_subscriptions.fetch_add(1, Ordering::SeqCst);
    }

    fn decrement_active_subscriptions(&self, count: usize) {
        self.active_subscriptions.fetch_sub(count, Ordering::SeqCst);
    }
}

#[tokio::test]
async fn test_normal_cleanup_via_explicit_call() {
    let metrics = Arc::new(TestMetricsHandler::new());
    let registry = Arc::new(SubscriptionRegistry::new(Some(metrics.clone())));

    // Create a connection
    let (tx, _rx) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(100);
    let sender = MessageSender::new(tx, 0);

    let _handle = {
        let handle = registry.register_connection(
            "conn1".to_string(),
            sender,
            None,
            Arc::new(Scope::Default),
        );

        // Add some subscriptions
        registry
            .add_subscription("conn1", &SubscriptionId::new("sub1"), vec![Filter::new()])
            .unwrap();
        registry
            .add_subscription("conn1", &SubscriptionId::new("sub2"), vec![Filter::new()])
            .unwrap();

        assert_eq!(metrics.get_count(), 2, "Should have 2 subscriptions");

        // Explicitly cleanup (simulating on_disconnect)
        registry.cleanup_connection("conn1");

        assert_eq!(metrics.get_count(), 0, "Subscriptions should be cleaned up");
        assert!(
            !registry.has_connection("conn1"),
            "Connection should be removed"
        );

        handle
    };

    // When handle is dropped, it should see the connection is already gone
    // and not attempt cleanup again (no double decrement)
    drop(_handle);

    // Give async operations time to complete
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    assert_eq!(
        metrics.get_count(),
        0,
        "Count should still be 0 (no double cleanup)"
    );
}

#[tokio::test]
async fn test_fallback_cleanup_via_drop() {
    let metrics = Arc::new(TestMetricsHandler::new());
    let registry = Arc::new(SubscriptionRegistry::new(Some(metrics.clone())));

    // Create a connection
    let (tx, _rx) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(100);
    let sender = MessageSender::new(tx, 0);

    {
        let _handle = registry.register_connection(
            "conn2".to_string(),
            sender,
            None,
            Arc::new(Scope::Default),
        );

        // Add some subscriptions
        registry
            .add_subscription("conn2", &SubscriptionId::new("sub1"), vec![Filter::new()])
            .unwrap();
        registry
            .add_subscription("conn2", &SubscriptionId::new("sub2"), vec![Filter::new()])
            .unwrap();

        assert_eq!(metrics.get_count(), 2, "Should have 2 subscriptions");

        // DO NOT call explicit cleanup - simulate on_disconnect not being called
        // The Drop handler should catch this
    }

    // Give Drop time to run
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Drop should have cleaned up
    assert_eq!(
        metrics.get_count(),
        0,
        "Drop should have cleaned up subscriptions"
    );
    assert!(
        !registry.has_connection("conn2"),
        "Connection should be removed by Drop"
    );
}

#[tokio::test]
async fn test_subscription_replacement_no_increment() {
    let metrics = Arc::new(TestMetricsHandler::new());
    let registry = Arc::new(SubscriptionRegistry::new(Some(metrics.clone())));

    // Create a connection
    let (tx, _rx) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(100);
    let sender = MessageSender::new(tx, 0);
    let _handle =
        registry.register_connection("conn3".to_string(), sender, None, Arc::new(Scope::Default));

    // Add a subscription
    let sub_id = SubscriptionId::new("sub1");
    registry
        .add_subscription("conn3", &sub_id, vec![Filter::new()])
        .unwrap();
    assert_eq!(metrics.get_count(), 1, "Should have 1 subscription");

    // Replace the subscription (same ID, different filters)
    let new_filter = Filter::new().kind(Kind::TextNote);
    registry
        .add_subscription("conn3", &sub_id, vec![new_filter])
        .unwrap();

    // Count should still be 1 (not incremented on replacement)
    assert_eq!(
        metrics.get_count(),
        1,
        "Should still have 1 subscription after replacement"
    );

    // Cleanup
    registry.cleanup_connection("conn3");
    assert_eq!(
        metrics.get_count(),
        0,
        "Should have 0 subscriptions after cleanup"
    );
}

#[tokio::test]
async fn test_dead_connection_cleanup() {
    let metrics = Arc::new(TestMetricsHandler::new());
    let registry = Arc::new(SubscriptionRegistry::new(Some(metrics.clone())));

    // Create a connection with a small channel
    let (tx, rx) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(1);
    let sender = MessageSender::new(tx, 0);
    let _handle =
        registry.register_connection("conn4".to_string(), sender, None, Arc::new(Scope::Default));

    // Add a subscription
    registry
        .add_subscription("conn4", &SubscriptionId::new("sub1"), vec![Filter::new()])
        .unwrap();
    assert_eq!(metrics.get_count(), 1, "Should have 1 subscription");

    // Drop the receiver to simulate dead connection
    drop(rx);

    // Try to distribute an event - this should detect the dead connection
    use nostr_sdk::{EventBuilder, Keys};
    let keys = Keys::generate();
    let event = EventBuilder::text_note("test")
        .build_with_ctx(&std::time::Instant::now(), keys.public_key())
        .sign_with_keys(&keys)
        .unwrap();

    registry
        .distribute_event(Arc::new(event), &Scope::Default)
        .await;

    // Dead connection should have been cleaned up
    assert_eq!(
        metrics.get_count(),
        0,
        "Dead connection should be cleaned up"
    );
    assert!(
        !registry.has_connection("conn4"),
        "Dead connection should be removed"
    );
}
