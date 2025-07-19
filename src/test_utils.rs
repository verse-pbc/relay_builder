use crate::database::RelayDatabase;
use crate::nostr_middleware::{
    ConnectionContext, InboundContext, NostrMessageSender, OutboundContext,
};
use crate::state::NostrConnectionState;
use crate::subscription_coordinator::{ReplaceableEventsBuffer, SubscriptionCoordinator};
use crate::subscription_registry::SubscriptionRegistry;
use flume::{self};
use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Instant;
use tempfile::TempDir;

pub async fn setup_test() -> (TempDir, Arc<RelayDatabase>, Keys) {
    let tmp_dir = TempDir::new().unwrap();
    let db_path = tmp_dir.path().join("test.db");
    let keys = Keys::generate();
    let database = RelayDatabase::new(db_path.to_str().unwrap()).unwrap();
    (tmp_dir, Arc::new(database), keys)
}

pub async fn setup_test_with_database() -> (TempDir, Arc<RelayDatabase>, Keys) {
    let tmp_dir = TempDir::new().unwrap();
    let db_path = tmp_dir.path().join("test.db");
    let keys = Keys::generate();
    let database = RelayDatabase::new(db_path.to_str().unwrap()).unwrap();
    (tmp_dir, Arc::new(database), keys)
}

pub async fn create_test_keys() -> (Keys, Keys, Keys) {
    (Keys::generate(), Keys::generate(), Keys::generate())
}

pub async fn create_test_event(keys: &Keys, kind: u16, tags: Vec<Tag>) -> nostr_sdk::Event {
    let created_at = Timestamp::now_with_supplier(&Instant::now());

    let mut unsigned =
        UnsignedEvent::new(keys.public_key(), created_at, Kind::Custom(kind), tags, "");

    unsigned.ensure_id();

    unsigned.sign_with_keys(keys).unwrap()
}

pub fn create_test_state(pubkey: Option<nostr_sdk::PublicKey>) -> NostrConnectionState {
    let mut state =
        NostrConnectionState::new(RelayUrl::parse("ws://test.relay").expect("Valid URL"))
            .expect("Failed to create test state");
    state.authed_pubkey = pubkey;
    state.max_subscriptions = Some(100); // Set a reasonable test limit
    state
}

pub async fn create_test_state_with_subscription_service(
    pubkey: Option<nostr_sdk::PublicKey>,
    database: Arc<RelayDatabase>,
) -> (
    NostrConnectionState,
    flume::Receiver<(RelayMessage<'static>, usize, Option<String>)>,
) {
    let (tx, rx) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(10);
    let sender = NostrMessageSender::new(tx, 0);

    // Create subscription registry
    let registry = Arc::new(SubscriptionRegistry::new(None));

    // Create cancellation token
    let cancellation_token = tokio_util::sync::CancellationToken::new();

    // Create crypto helper for tests
    let test_keys = Keys::generate();
    let crypto_helper = crate::crypto_helper::CryptoHelper::new(Arc::new(test_keys));

    // Create a test replaceable events buffer with registry
    let buffer = ReplaceableEventsBuffer::with_registry(registry.clone());
    let replaceable_event_queue = buffer.get_sender();
    buffer.start_with_sender(
        database.clone(),
        crypto_helper.clone(),
        cancellation_token,
        "test_replaceable_events_buffer".to_string(),
    );

    let subscription_coordinator = SubscriptionCoordinator::new(
        database,
        crypto_helper,
        registry,
        "test_connection".to_string(),
        sender,
        pubkey,
        Arc::new(nostr_lmdb::Scope::Default),
        None,
        500, // max_limit
        replaceable_event_queue,
    );

    let mut state =
        NostrConnectionState::new(RelayUrl::parse("ws://test.relay").expect("Valid URL"))
            .expect("Failed to create test state");
    state.authed_pubkey = pubkey;
    state.max_subscriptions = Some(100); // Set a reasonable test limit

    // Use the test method to add the subscription coordinator
    state.set_subscription_coordinator(subscription_coordinator);

    (state, rx)
}

/// Helper for creating test contexts for the new NostrMiddleware API
///
/// This creates a test context that can be used for unit testing individual middleware.
/// Since the new system uses static dispatch, we create a mock next processor.
pub fn create_test_inbound_context<T: Send + Sync + Clone + 'static>(
    connection_id: String,
    message: Option<ClientMessage<'static>>,
    sender: Option<flume::Sender<(RelayMessage<'static>, usize, Option<String>)>>,
    state: NostrConnectionState<T>,
    _middlewares: Vec<()>, // Unused in new system, kept for API compatibility
    index: usize,
) -> TestInboundContext<T> {
    let state_arc = Arc::new(RwLock::new(state));

    // Create sender or use a dummy one
    let sender = sender.unwrap_or_else(|| {
        let (tx, _rx) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(1);
        tx
    });
    let nostr_sender = NostrMessageSender::new(sender, index);

    TestInboundContext {
        connection_id,
        message,
        state: state_arc,
        sender: nostr_sender,
        next_processor: TestNextProcessor::new(),
    }
}

/// Test-specific InboundContext that can be used for unit testing middleware
pub struct TestInboundContext<T> {
    pub connection_id: String,
    pub message: Option<ClientMessage<'static>>,
    pub state: Arc<RwLock<NostrConnectionState<T>>>,
    pub sender: NostrMessageSender,
    next_processor: TestNextProcessor<T>,
}

impl<T> TestInboundContext<T> {
    /// Create an InboundContext reference for middleware testing
    /// This simulates what the middleware chain would normally provide
    pub fn as_context(&mut self) -> InboundContext<'_, T, TestNextProcessor<T>> {
        InboundContext {
            connection_id: &self.connection_id,
            message: &mut self.message,
            state: &self.state,
            sender: self.sender.clone(),
            next: &self.next_processor,
        }
    }
}

/// Mock processor for testing - always succeeds
pub struct TestNextProcessor<T> {
    _phantom: std::marker::PhantomData<T>,
}

impl<T> TestNextProcessor<T> {
    pub fn new() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T> Default for TestNextProcessor<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> crate::nostr_middleware::InboundProcessor<T> for TestNextProcessor<T>
where
    T: Send + Sync + 'static,
{
    fn process_inbound(
        &self,
        _connection_id: &str,
        _message: &mut Option<ClientMessage<'static>>,
        _state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
        _sender: &NostrMessageSender,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move { Ok(()) }
    }

    fn on_connect(
        &self,
        _connection_id: &str,
        _state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
        _sender: &NostrMessageSender,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move { Ok(()) }
    }
}

/// Helper for creating test outbound contexts for the new NostrMiddleware API
pub fn create_test_outbound_context<T: Send + Sync + Clone + 'static>(
    connection_id: String,
    message: RelayMessage<'static>,
    sender: Option<flume::Sender<(RelayMessage<'static>, usize, Option<String>)>>,
    state: NostrConnectionState<T>,
    _middlewares: Vec<()>, // Unused in new system, kept for API compatibility
    position: usize,
) -> TestOutboundContext<T> {
    let state_arc = Arc::new(RwLock::new(state));

    // Create sender or use a dummy one
    let raw_sender = sender.unwrap_or_else(|| {
        let (tx, _rx) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(1);
        tx
    });

    let nostr_sender = NostrMessageSender::new(raw_sender, position);

    TestOutboundContext {
        connection_id,
        message: Some(message),
        state: state_arc,
        sender: nostr_sender,
        from_position: position,
    }
}

/// Test-specific OutboundContext that can be used for unit testing middleware
pub struct TestOutboundContext<T> {
    pub connection_id: String,
    pub message: Option<RelayMessage<'static>>,
    pub state: Arc<RwLock<NostrConnectionState<T>>>,
    pub sender: NostrMessageSender,
    pub from_position: usize,
}

impl<T> TestOutboundContext<T> {
    /// Create an OutboundContext reference for middleware testing
    pub fn as_context(&mut self) -> OutboundContext<'_, T> {
        OutboundContext {
            connection_id: &self.connection_id,
            message: &mut self.message,
            state: &self.state,
            sender: &self.sender,
        }
    }

    /// Get the from_position for testing (no longer needed by middleware)
    pub fn get_from_position(&self) -> usize {
        self.from_position
    }
}

/// Helper for creating test connection contexts for the new NostrMiddleware API
pub fn create_test_connection_context<T: Send + Sync + Clone + 'static>(
    connection_id: String,
    sender: Option<flume::Sender<(RelayMessage<'static>, usize, Option<String>)>>,
    state: NostrConnectionState<T>,
    _middlewares: Vec<()>, // Unused in new system, kept for API compatibility
    index: usize,
) -> TestConnectionContext<T> {
    let state_arc = Arc::new(RwLock::new(state));

    // Create sender or use a dummy one
    let sender = sender.unwrap_or_else(|| {
        let (tx, _rx) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(1);
        tx
    });
    let nostr_sender = NostrMessageSender::new(sender, index);

    TestConnectionContext {
        connection_id,
        state: state_arc,
        sender: nostr_sender,
    }
}

/// Test-specific ConnectionContext that can be used for unit testing middleware
pub struct TestConnectionContext<T> {
    pub connection_id: String,
    pub state: Arc<RwLock<NostrConnectionState<T>>>,
    pub sender: NostrMessageSender,
}

impl<T> TestConnectionContext<T>
where
    T: Clone,
{
    /// Create a ConnectionContext reference for middleware testing
    pub fn as_context(&self) -> ConnectionContext<'_, T> {
        ConnectionContext {
            connection_id: &self.connection_id,
            state: &self.state,
            sender: self.sender.clone(),
        }
    }
}
