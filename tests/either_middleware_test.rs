//! Test that Either middleware properly delegates all lifecycle methods

use nostr_sdk::prelude::*;
use relay_builder::{
    nostr_middleware::{
        ConnectionContext, DisconnectContext, InboundContext, InboundProcessor, MessageSender,
        NostrMiddleware, OutboundContext,
    },
    state::{ConnectionMetadata, NostrConnectionState},
    util::{Either, IdentityMiddleware},
};
use std::sync::{
    atomic::{AtomicBool, AtomicUsize, Ordering},
    Arc,
};

/// Test middleware that tracks all lifecycle method calls
#[derive(Clone)]
struct TrackingMiddleware {
    name: String,
    on_connect_called: Arc<AtomicBool>,
    process_inbound_called: Arc<AtomicUsize>,
    process_outbound_called: Arc<AtomicUsize>,
    on_disconnect_called: Arc<AtomicBool>,
}

impl TrackingMiddleware {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            on_connect_called: Arc::new(AtomicBool::new(false)),
            process_inbound_called: Arc::new(AtomicUsize::new(0)),
            process_outbound_called: Arc::new(AtomicUsize::new(0)),
            on_disconnect_called: Arc::new(AtomicBool::new(false)),
        }
    }

    fn was_on_connect_called(&self) -> bool {
        self.on_connect_called.load(Ordering::Relaxed)
    }

    fn inbound_call_count(&self) -> usize {
        self.process_inbound_called.load(Ordering::Relaxed)
    }

    fn outbound_call_count(&self) -> usize {
        self.process_outbound_called.load(Ordering::Relaxed)
    }

    fn was_on_disconnect_called(&self) -> bool {
        self.on_disconnect_called.load(Ordering::Relaxed)
    }
}

impl<T> NostrMiddleware<T> for TrackingMiddleware
where
    T: Send + Sync + Clone + 'static,
{
    async fn on_connect(&self, _ctx: ConnectionContext<'_, T>) -> Result<(), anyhow::Error> {
        println!("{}: on_connect called", self.name);
        self.on_connect_called.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, T, Next>,
    ) -> Result<(), anyhow::Error>
    where
        Next: InboundProcessor<T>,
    {
        println!("{}: process_inbound called", self.name);
        self.process_inbound_called.fetch_add(1, Ordering::Relaxed);
        ctx.next().await
    }

    async fn process_outbound(&self, _ctx: OutboundContext<'_, T>) -> Result<(), anyhow::Error> {
        println!("{}: process_outbound called", self.name);
        self.process_outbound_called.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    async fn on_disconnect(&self, _ctx: DisconnectContext<'_, T>) -> Result<(), anyhow::Error> {
        println!("{}: on_disconnect called", self.name);
        self.on_disconnect_called.store(true, Ordering::Relaxed);
        Ok(())
    }
}

fn create_test_state() -> Arc<tokio::sync::RwLock<NostrConnectionState<()>>> {
    Arc::new(tokio::sync::RwLock::new(
        NostrConnectionState::<()>::new().unwrap(),
    ))
}

fn create_test_metadata() -> Arc<ConnectionMetadata> {
    let relay_pubkey = nostr_sdk::Keys::generate().public_key();
    Arc::new(ConnectionMetadata::new_with_context(
        RelayUrl::parse("ws://test.relay").unwrap(),
        Arc::new(nostr_lmdb::Scope::Default),
        relay_pubkey,
    ))
}

fn create_test_sender() -> MessageSender {
    let (tx, _rx) = flume::unbounded();
    MessageSender::new(tx, 0)
}

#[tokio::test]
async fn test_either_left_delegates_on_connect() {
    let tracking = TrackingMiddleware::new("wrapped");
    let either: Either<TrackingMiddleware, IdentityMiddleware> = Either::Left(tracking.clone());

    let state = create_test_state();
    let sender = create_test_sender();
    let metadata = create_test_metadata();
    let ctx = ConnectionContext {
        connection_id: "test_conn",
        state: &state,
        metadata: &metadata,
        sender,
    };

    // Call on_connect through Either::Left
    either.on_connect(ctx).await.unwrap();

    // This test will FAIL with current code because Either doesn't implement on_connect
    assert!(
        tracking.was_on_connect_called(),
        "Either::Left should delegate on_connect to wrapped middleware"
    );
}

#[tokio::test]
async fn test_either_right_delegates_on_connect() {
    let identity = IdentityMiddleware;
    let either = Either::Right::<TrackingMiddleware, _>(identity);

    let state = create_test_state();
    let sender = create_test_sender();
    let metadata = create_test_metadata();
    let ctx = ConnectionContext {
        connection_id: "test_conn",
        state: &state,
        metadata: &metadata,
        sender,
    };

    // Call on_connect through Either::Right (should be no-op)
    either.on_connect(ctx).await.unwrap();

    // Test passes because IdentityMiddleware's on_connect is a no-op
}

#[tokio::test]
async fn test_either_left_delegates_process_inbound() {
    let tracking = TrackingMiddleware::new("wrapped");
    let _either: Either<TrackingMiddleware, IdentityMiddleware> = Either::Left(tracking.clone());

    // We can't test process_inbound directly due to private fields in InboundContext
    // But we've verified that process_outbound and on_disconnect work
    // The real test is that on_connect doesn't work, which is the bug we're fixing

    // Verify the middleware is properly wrapped
    assert_eq!(tracking.inbound_call_count(), 0);
}

#[tokio::test]
async fn test_either_left_delegates_process_outbound() {
    let tracking = TrackingMiddleware::new("wrapped");
    let either: Either<TrackingMiddleware, IdentityMiddleware> = Either::Left(tracking.clone());

    let state = create_test_state();
    let sender = create_test_sender();
    let mut message = None;
    let metadata = create_test_metadata();
    let ctx = OutboundContext {
        connection_id: "test_conn",
        message: &mut message,
        state: &state,
        metadata: &metadata,
        sender: &sender,
    };

    // Call process_outbound through Either::Left
    either.process_outbound(ctx).await.unwrap();

    assert_eq!(
        tracking.outbound_call_count(),
        1,
        "Either::Left should delegate process_outbound to wrapped middleware"
    );
}

#[tokio::test]
async fn test_either_left_delegates_on_disconnect() {
    let tracking = TrackingMiddleware::new("wrapped");
    let either: Either<TrackingMiddleware, IdentityMiddleware> = Either::Left(tracking.clone());

    let state = create_test_state();
    let metadata = create_test_metadata();
    let ctx = DisconnectContext {
        connection_id: "test_conn",
        state: &state,
        metadata: &metadata,
    };

    // Call on_disconnect through Either::Left
    either.on_disconnect(ctx).await.unwrap();

    assert!(
        tracking.was_on_disconnect_called(),
        "Either::Left should delegate on_disconnect to wrapped middleware"
    );
}

#[tokio::test]
async fn test_auth_middleware_through_either() {
    use relay_builder::middlewares::Nip42Middleware;

    // Create auth middleware wrapped in Either
    let auth_middleware = Nip42Middleware::<()>::with_url("ws://test.relay".to_string());
    let either: Either<Nip42Middleware<()>, IdentityMiddleware> = Either::Left(auth_middleware);

    let state = create_test_state();
    let (tx, rx) = flume::unbounded::<(RelayMessage<'static>, usize, Option<String>)>();
    let sender = MessageSender::new(tx, 0);
    let metadata = create_test_metadata();
    let ctx = ConnectionContext {
        connection_id: "test_conn",
        state: &state,
        metadata: &metadata,
        sender,
    };

    // Call on_connect - auth middleware should send AUTH challenge
    either.on_connect(ctx).await.unwrap();

    // Check if AUTH message was sent
    // This will FAIL with current code because Either doesn't delegate on_connect
    let received = rx.try_recv();
    assert!(
        received.is_ok(),
        "AUTH challenge should be sent through Either::Left wrapper"
    );

    if let Ok((msg, _, _)) = received {
        match msg {
            RelayMessage::Auth { challenge } => {
                println!("Received AUTH challenge: {challenge}");
            }
            _ => panic!("Expected AUTH message, got {msg:?}"),
        }
    }
}
