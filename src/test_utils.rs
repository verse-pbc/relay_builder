use crate::database::RelayDatabase;
use crate::state::NostrConnectionState;
use crate::subscription_coordinator::SubscriptionCoordinator;
use crate::subscription_registry::SubscriptionRegistry;
use flume;
use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use std::sync::Arc;
use std::time::Instant;
use tempfile::TempDir;
use websocket_builder::{
    ConnectionContext, InboundContext, MessageSender, Middleware, OutboundContext,
};

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

    let mut unsigned = UnsignedEvent::new(
        keys.public_key(),
        created_at,
        Kind::Custom(kind),
        tags.clone(),
        "",
    );

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
    flume::Receiver<(RelayMessage<'static>, usize)>,
) {
    let (tx, rx) = flume::bounded(10);
    let sender = MessageSender::new(tx, 0);

    // Create subscription registry
    let registry = Arc::new(SubscriptionRegistry::new(None));

    // Create cancellation token
    let cancellation_token = tokio_util::sync::CancellationToken::new();

    // Create crypto helper for tests
    let test_keys = Keys::generate();
    let crypto_helper = crate::crypto_helper::CryptoHelper::new(Arc::new(test_keys));

    let subscription_coordinator = SubscriptionCoordinator::new(
        database,
        crypto_helper,
        registry,
        "test_connection".to_string(),
        sender,
        pubkey,
        Arc::new(nostr_lmdb::Scope::Default),
        cancellation_token,
        None,
        500, // max_limit
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

/// Helper for creating test contexts that match the new websocket_builder API
pub fn create_test_inbound_context<T: Send + Sync + 'static>(
    connection_id: String,
    message: Option<ClientMessage<'static>>,
    sender: Option<flume::Sender<(RelayMessage<'static>, usize)>>,
    state: NostrConnectionState<T>,
    middlewares: Vec<
        Arc<
            dyn Middleware<
                State = NostrConnectionState<T>,
                IncomingMessage = ClientMessage<'static>,
                OutgoingMessage = RelayMessage<'static>,
            >,
        >,
    >,
    index: usize,
) -> InboundContext<NostrConnectionState<T>, ClientMessage<'static>, RelayMessage<'static>> {
    let state_arc = Arc::new(RwLock::new(state));
    let middlewares_arc = Arc::new(middlewares);

    InboundContext::new(
        connection_id,
        message,
        sender,
        state_arc,
        middlewares_arc,
        index,
    )
}

/// Helper for creating test contexts that match the new websocket_builder API  
pub fn create_test_outbound_context<T: Send + Sync + 'static>(
    connection_id: String,
    message: RelayMessage<'static>,
    sender: Option<flume::Sender<(RelayMessage<'static>, usize)>>,
    state: NostrConnectionState<T>,
    middlewares: Vec<
        Arc<
            dyn Middleware<
                State = NostrConnectionState<T>,
                IncomingMessage = ClientMessage<'static>,
                OutgoingMessage = RelayMessage<'static>,
            >,
        >,
    >,
    index: usize,
) -> OutboundContext<NostrConnectionState<T>, ClientMessage<'static>, RelayMessage<'static>> {
    let state_arc = Arc::new(RwLock::new(state));
    let middlewares_arc = Arc::new(middlewares);

    OutboundContext::new(
        connection_id,
        message,
        sender,
        state_arc,
        middlewares_arc,
        index,
    )
}

/// Helper for creating test contexts that match the new websocket_builder API
pub fn create_test_connection_context<T: Send + Sync + 'static>(
    connection_id: String,
    sender: Option<flume::Sender<(RelayMessage<'static>, usize)>>,
    state: NostrConnectionState<T>,
    middlewares: Vec<
        Arc<
            dyn Middleware<
                State = NostrConnectionState<T>,
                IncomingMessage = ClientMessage<'static>,
                OutgoingMessage = RelayMessage<'static>,
            >,
        >,
    >,
    index: usize,
) -> ConnectionContext<NostrConnectionState<T>, ClientMessage<'static>, RelayMessage<'static>> {
    let state_arc = Arc::new(RwLock::new(state));
    let middlewares_arc = Arc::new(middlewares);

    ConnectionContext::new(connection_id, sender, state_arc, middlewares_arc, index)
}
