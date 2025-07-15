//! Integration tests for RelayMiddleware

use async_trait::async_trait;
use nostr_relay_builder::{
    Error, EventContext, EventProcessor, NostrConnectionState, RelayDatabase, RelayMiddleware,
    StoreCommand,
};
use nostr_sdk::prelude::*;
use nostr_sdk::RelayUrl;
use std::sync::Arc;
use tokio_util::task::TaskTracker;

/// Test implementation of EventProcessor
#[derive(Debug, Clone)]
struct TestEventProcessor {
    allow_all: bool,
}

impl TestEventProcessor {
    fn new(allow_all: bool) -> Self {
        Self { allow_all }
    }
}

#[async_trait]
impl EventProcessor for TestEventProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<parking_lot::RwLock<()>>,
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>, Error> {
        if self.allow_all {
            Ok(vec![(event, context.subdomain.clone()).into()])
        } else {
            Err(Error::restricted("Events not allowed"))
        }
    }

    fn can_see_event(
        &self,
        _event: &Event,
        _custom_state: Arc<parking_lot::RwLock<()>>,
        _context: EventContext<'_>,
    ) -> Result<bool, Error> {
        Ok(self.allow_all)
    }
}

/// Restrictive processor that only allows authenticated users
#[derive(Debug, Clone)]
struct RestrictiveEventProcessor;

#[async_trait]
impl EventProcessor for RestrictiveEventProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<parking_lot::RwLock<()>>,
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>, Error> {
        if context.authed_pubkey.is_some() {
            Ok(vec![(event, context.subdomain.clone()).into()])
        } else {
            Err(Error::auth_required(
                "Authentication required to publish events",
            ))
        }
    }

    fn can_see_event(
        &self,
        _event: &Event,
        _custom_state: Arc<parking_lot::RwLock<()>>,
        context: EventContext<'_>,
    ) -> Result<bool, Error> {
        Ok(context.authed_pubkey.is_some())
    }

    fn verify_filters(
        &self,
        _filters: &[Filter],
        _custom_state: Arc<parking_lot::RwLock<()>>,
        context: EventContext<'_>,
    ) -> Result<(), Error> {
        if context.authed_pubkey.is_some() {
            Ok(())
        } else {
            Err(Error::auth_required("Authentication required to subscribe"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    async fn setup_test() -> (Arc<RelayDatabase>, Keys, TempDir) {
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path();
        let keys = Keys::generate();
        let _task_tracker = TaskTracker::new();
        let database = RelayDatabase::new(db_path).unwrap();
        (Arc::new(database), keys, tmp_dir)
    }

    #[tokio::test]
    async fn test_relay_middleware_creation() {
        let (database, keys, _tmp_dir) = setup_test().await;
        let processor = TestEventProcessor::new(true);
        let registry = Arc::new(nostr_relay_builder::SubscriptionRegistry::new(None));
        let crypto_helper = nostr_relay_builder::CryptoHelper::new(Arc::new(keys.clone()));
        let middleware = RelayMiddleware::new(
            processor,
            keys.public_key(),
            database,
            registry,
            500,
            RelayUrl::parse("ws://test").unwrap(),
            crypto_helper,
            None,
        );
        assert!(middleware.processor().allow_all);
    }

    #[tokio::test]
    async fn test_verify_filters() {
        let restrict_processor = RestrictiveEventProcessor;
        let relay_pubkey = Keys::generate().public_key();
        let state = NostrConnectionState::<()>::new(RelayUrl::parse("ws://test").unwrap()).unwrap();
        let filters = vec![Filter::new()];

        let context = EventContext {
            authed_pubkey: None,
            subdomain: &state.subdomain,
            relay_pubkey: &relay_pubkey,
        };

        // Test without authentication
        let verify_result = restrict_processor.verify_filters(
            &filters,
            Arc::new(parking_lot::RwLock::new(())),
            context,
        );
        assert!(verify_result.is_err());

        // Test with authentication
        let auth_pubkey = Keys::generate().public_key();
        let auth_context = EventContext {
            authed_pubkey: Some(&auth_pubkey),
            subdomain: &state.subdomain,
            relay_pubkey: &relay_pubkey,
        };

        let verify_result = restrict_processor.verify_filters(
            &filters,
            Arc::new(parking_lot::RwLock::new(())),
            auth_context,
        );
        assert!(verify_result.is_ok());
    }

    #[tokio::test]
    async fn test_event_visibility() {
        let allow_processor = TestEventProcessor::new(true);
        let deny_processor = TestEventProcessor::new(false);
        let restrict_processor = RestrictiveEventProcessor;

        let relay_pubkey = Keys::generate().public_key();
        let state = NostrConnectionState::<()>::new(RelayUrl::parse("ws://test").unwrap()).unwrap();
        let keys = Keys::generate();
        let event = keys
            .sign_event(EventBuilder::text_note("test").build(keys.public_key()))
            .await
            .unwrap();

        let context = EventContext {
            authed_pubkey: None,
            subdomain: &state.subdomain,
            relay_pubkey: &relay_pubkey,
        };

        // Test allow all processor
        assert!(allow_processor
            .can_see_event(&event, Arc::new(parking_lot::RwLock::new(())), context)
            .unwrap());

        // Test deny all processor
        assert!(!deny_processor
            .can_see_event(&event, Arc::new(parking_lot::RwLock::new(())), context)
            .unwrap());

        // Test restrictive processor without auth
        assert!(!restrict_processor
            .can_see_event(&event, Arc::new(parking_lot::RwLock::new(())), context)
            .unwrap());

        // Test restrictive processor with auth
        let auth_pubkey = keys.public_key();
        let auth_context = EventContext {
            authed_pubkey: Some(&auth_pubkey),
            subdomain: &state.subdomain,
            relay_pubkey: &relay_pubkey,
        };
        assert!(restrict_processor
            .can_see_event(&event, Arc::new(parking_lot::RwLock::new(())), auth_context)
            .unwrap());
    }

    #[tokio::test]
    async fn test_subscription_limiting() {
        let mut state =
            NostrConnectionState::<()>::new(RelayUrl::parse("ws://test").unwrap()).unwrap();
        state.set_max_subscriptions(Some(3));

        // Can add first subscription
        let sub1 = SubscriptionId::new("sub1");
        assert!(state.try_add_subscription(sub1.clone()).is_ok());

        // Can add second subscription
        let sub2 = SubscriptionId::new("sub2");
        assert!(state.try_add_subscription(sub2.clone()).is_ok());

        // Can add third subscription
        let sub3 = SubscriptionId::new("sub3");
        assert!(state.try_add_subscription(sub3.clone()).is_ok());

        // Cannot add fourth subscription
        let sub4 = SubscriptionId::new("sub4");
        let result = state.try_add_subscription(sub4.clone());
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Subscription limit exceeded"));

        // Can't add another subscription when at limit
        assert_eq!(state.subscription_count(), 3);

        // Remove a subscription and verify we can add a new one
        assert!(state.remove_tracked_subscription(&sub2));
        assert!(state.try_add_subscription(sub4).is_ok());

        // Verify count is still at limit
        let sub5 = SubscriptionId::new("sub5");
        assert!(state.try_add_subscription(sub5).is_err());
    }
}
