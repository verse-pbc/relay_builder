//! Integration tests for NIP-42 authentication

use nostr_sdk::prelude::*;
use relay_builder::{
    EventContext, EventProcessor, RelayBuilder, RelayConfig, Result as RelayResult, StoreCommand,
};
use std::sync::Arc;

type AuthEvent = (Event, Option<PublicKey>);

/// Test processor that tracks authentication state
#[derive(Debug, Clone)]
struct AuthTestProcessor {
    auth_events: Arc<parking_lot::Mutex<Vec<AuthEvent>>>,
}

impl AuthTestProcessor {
    fn new() -> Self {
        Self {
            auth_events: Arc::new(parking_lot::Mutex::new(Vec::new())),
        }
    }
}

impl<T> EventProcessor<T> for AuthTestProcessor
where
    T: Clone + Send + Sync + std::fmt::Debug + Default + 'static,
{
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<parking_lot::RwLock<T>>,
        context: &EventContext,
    ) -> RelayResult<Vec<StoreCommand>> {
        // Track events with their auth state
        self.auth_events
            .lock()
            .push((event.clone(), context.authed_pubkey));

        // Accept all events
        Ok(vec![(event, (*context.subdomain).clone()).into()])
    }
}

#[tokio::test]
async fn test_relay_builder_with_auth_enabled() {
    // Create temporary database
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test_db");

    // Create relay configuration with auth enabled
    let relay_keys = Keys::generate();
    let mut config = RelayConfig::new(
        "ws://localhost:8080",
        db_path.to_string_lossy().to_string(),
        relay_keys.clone(),
    );
    config.enable_auth = true;

    // Build the relay - should automatically include auth middleware
    let processor = AuthTestProcessor::new();
    let _handler_factory = RelayBuilder::<()>::new(config)
        .event_processor(processor.clone())
        .build()
        .await
        .expect("Should build relay with auth middleware when enable_auth is true");

    // The actual auth middleware execution with AUTH challenges
    // is tested through the Either middleware tests and example code
}

#[tokio::test]
async fn test_relay_builder_with_auth_disabled() {
    // Create temporary database
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test_db");

    // Create relay configuration with auth disabled (default)
    let relay_keys = Keys::generate();
    let config = RelayConfig::new(
        "ws://localhost:8080",
        db_path.to_string_lossy().to_string(),
        relay_keys.clone(),
    );
    assert!(!config.enable_auth, "Auth should be disabled by default");

    // Build the relay - should not include auth middleware
    let processor = AuthTestProcessor::new();
    let _handler_factory = RelayBuilder::<()>::new(config)
        .event_processor(processor.clone())
        .build()
        .await
        .expect("Should build relay without auth middleware when enable_auth is false");
}

/// Test processor that requires auth for posting
#[derive(Debug, Clone)]
struct RequireAuthProcessor;

impl<T> EventProcessor<T> for RequireAuthProcessor
where
    T: Clone + Send + Sync + std::fmt::Debug + Default + 'static,
{
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<parking_lot::RwLock<T>>,
        context: &EventContext,
    ) -> RelayResult<Vec<StoreCommand>> {
        if context.authed_pubkey.is_none() {
            return Err(relay_builder::Error::restricted(
                "authentication required: please AUTH first",
            ));
        }
        Ok(vec![(event, (*context.subdomain).clone()).into()])
    }
}

#[tokio::test]
async fn test_relay_builder_with_auth_required_processor() {
    // Create temporary database
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test_db");

    // Create relay configuration with auth enabled
    let relay_keys = Keys::generate();
    let mut config = RelayConfig::new(
        "ws://localhost:8080",
        db_path.to_string_lossy().to_string(),
        relay_keys.clone(),
    );
    config.enable_auth = true;

    // Build the relay with a processor that requires auth
    let _handler_factory = RelayBuilder::<()>::new(config)
        .event_processor(RequireAuthProcessor)
        .build()
        .await
        .expect("Should build relay with auth-required processor");

    // In a real scenario, events would be rejected if no AUTH is performed
}

#[tokio::test]
async fn test_relay_builder_auth_with_custom_middleware() {
    use relay_builder::util::{Either, IdentityMiddleware};

    // Create temporary database
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir.path().join("test_db");

    // Create relay configuration with auth enabled
    let relay_keys = Keys::generate();
    let mut config = RelayConfig::new(
        "ws://localhost:8080",
        db_path.to_string_lossy().to_string(),
        relay_keys.clone(),
    );
    config.enable_auth = true;

    // Custom middleware that tracks calls
    #[derive(Debug, Clone)]
    struct TrackingMiddleware {
        calls: Arc<parking_lot::Mutex<Vec<String>>>,
    }

    impl<T> relay_builder::NostrMiddleware<T> for TrackingMiddleware
    where
        T: Send + Sync + Clone + 'static,
    {
        async fn process_inbound<Next>(
            &self,
            ctx: relay_builder::nostr_middleware::InboundContext<'_, T, Next>,
        ) -> Result<(), anyhow::Error>
        where
            Next: relay_builder::nostr_middleware::InboundProcessor<T>,
        {
            self.calls.lock().push("process_inbound".to_string());
            ctx.next().await
        }
    }

    let tracking = TrackingMiddleware {
        calls: Arc::new(parking_lot::Mutex::new(Vec::new())),
    };

    // Build with custom middleware - auth should be added automatically before custom middleware
    let _handler_factory = RelayBuilder::<()>::new(config)
        .event_processor(AuthTestProcessor::new())
        .build_with(|chain| {
            chain.with(Either::<TrackingMiddleware, IdentityMiddleware>::Left(
                tracking,
            ))
        })
        .await
        .expect("Should build relay with auth and custom middleware");

    // The middleware chain order should be:
    // Logger -> ErrorHandling -> Auth -> Custom -> Relay
}
