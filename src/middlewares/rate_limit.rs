//! Rate limiting middleware using governor crate
//!
//! Provides per-connection and global rate limiting for Nostr relays

use crate::error::Error;
use crate::nostr_middleware::{InboundContext, InboundProcessor, NostrMiddleware};
use governor::{DefaultDirectRateLimiter, DefaultKeyedRateLimiter, Quota, RateLimiter};
use nostr_sdk::prelude::*;
use std::sync::Arc;

/// Rate limiting middleware for Nostr connections
#[derive(Clone)]
pub struct RateLimitMiddleware<T = ()> {
    /// Per-connection rate limiter (keyed by connection ID)
    per_connection_limiter: Arc<DefaultKeyedRateLimiter<String>>,
    /// Optional global rate limiter for the entire relay
    global_limiter: Option<Arc<DefaultDirectRateLimiter>>,
    _phantom: std::marker::PhantomData<T>,
}

impl<T> RateLimitMiddleware<T> {
    /// Create a new rate limiter with per-connection limits
    pub fn new(per_connection_quota: Quota) -> Self {
        Self {
            per_connection_limiter: Arc::new(RateLimiter::keyed(per_connection_quota)),
            global_limiter: None,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Create a new rate limiter with both per-connection and global limits
    pub fn with_global_limit(per_connection_quota: Quota, global_quota: Quota) -> Self {
        Self {
            per_connection_limiter: Arc::new(RateLimiter::keyed(per_connection_quota)),
            global_limiter: Some(Arc::new(RateLimiter::direct(global_quota))),
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T> NostrMiddleware<T> for RateLimitMiddleware<T>
where
    T: Send + Sync + Clone + 'static,
{
    async fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, T, Next>,
    ) -> Result<(), anyhow::Error>
    where
        Next: InboundProcessor<T>,
    {
        // Check global rate limit first (if configured)
        if let Some(global_limiter) = &self.global_limiter {
            if global_limiter.check().is_err() {
                return Err(Error::restricted("global rate limit exceeded").into());
            }
        }

        // Check per-connection rate limit
        let connection_id = ctx.connection_id.to_string();
        if self
            .per_connection_limiter
            .check_key(&connection_id)
            .is_err()
        {
            return Err(Error::restricted("connection rate limit exceeded").into());
        }

        // Both rate limits passed, continue to next middleware
        ctx.next().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::{create_test_event, create_test_inbound_context};
    use nonzero_ext::nonzero;

    #[tokio::test]
    async fn test_allows_requests_within_rate_limit() {
        // Create middleware allowing 10 requests per second
        let quota = Quota::per_second(nonzero!(10u32));
        let middleware = RateLimitMiddleware::<()>::new(quota);

        // Create test context
        let connection_id = "test_connection_1".to_string();

        // Create a valid test event
        let keys = Keys::generate();
        let test_event = create_test_event(&keys, 1, vec![]).await;
        let message = Some(ClientMessage::event(test_event));

        let (tx, _rx) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(10);
        let mut ctx = create_test_inbound_context(
            connection_id.clone(),
            message,
            Some(tx),
            Default::default(),
            vec![],
            0,
        );

        // First request should pass
        let result = middleware.process_inbound(ctx.as_context()).await;
        assert!(result.is_ok(), "First request should be allowed");
    }

    #[tokio::test]
    async fn test_rejects_requests_exceeding_rate_limit() {
        // Create middleware allowing only 1 request per second
        let quota = Quota::per_second(nonzero!(1u32));
        let middleware = RateLimitMiddleware::<()>::new(quota);

        let connection_id = "test_connection_2".to_string();

        // Create valid test events
        let keys = Keys::generate();
        let test_event1 = create_test_event(&keys, 1, vec![]).await;
        let message = Some(ClientMessage::event(test_event1));

        let (tx, _rx) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(10);
        let mut ctx = create_test_inbound_context(
            connection_id.clone(),
            message,
            Some(tx.clone()),
            Default::default(),
            vec![],
            0,
        );

        let result = middleware.process_inbound(ctx.as_context()).await;
        assert!(result.is_ok(), "First request should be allowed");

        // Second request should be rate limited
        let test_event2 = create_test_event(&keys, 1, vec![]).await;
        let message2 = Some(ClientMessage::event(test_event2));

        let mut ctx2 = create_test_inbound_context(
            connection_id.clone(),
            message2,
            Some(tx),
            Default::default(),
            vec![],
            0,
        );

        let result2 = middleware.process_inbound(ctx2.as_context()).await;
        assert!(result2.is_err(), "Rate limiting should return error");

        // Verify it's the correct type of error
        let error = result2.unwrap_err();
        let relay_error = error
            .downcast_ref::<Error>()
            .expect("Should be relay Error");
        match relay_error {
            Error::Restricted { message, .. } => {
                assert!(
                    message.contains("rate limit"),
                    "Error message should mention rate limit"
                );
            }
            _ => panic!("Expected Restricted error for rate limiting"),
        }
    }

    #[tokio::test]
    async fn test_different_connections_have_independent_limits() {
        // Create middleware allowing 2 requests per second per connection
        let quota = Quota::per_second(nonzero!(2u32));
        let middleware = RateLimitMiddleware::<()>::new(quota);

        let connection1 = "conn1".to_string();
        let connection2 = "conn2".to_string();

        // Create valid test event
        let keys = Keys::generate();
        let test_event = create_test_event(&keys, 1, vec![]).await;
        let message = Some(ClientMessage::event(test_event));

        // Both connections should be able to make 2 requests
        for _ in 0..2 {
            // Connection 1
            let (tx1, _rx1) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(10);
            let mut ctx1 = create_test_inbound_context(
                connection1.clone(),
                message.clone(),
                Some(tx1),
                Default::default(),
                vec![],
                0,
            );
            let result1 = middleware.process_inbound(ctx1.as_context()).await;
            assert!(result1.is_ok());

            // Connection 2
            let (tx2, _rx2) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(10);
            let mut ctx2 = create_test_inbound_context(
                connection2.clone(),
                message.clone(),
                Some(tx2),
                Default::default(),
                vec![],
                0,
            );
            let result2 = middleware.process_inbound(ctx2.as_context()).await;
            assert!(result2.is_ok());
        }
    }

    #[tokio::test]
    async fn test_global_rate_limit() {
        // Create middleware with both per-connection and global limits
        let per_conn_quota = Quota::per_second(nonzero!(10u32));
        let global_quota = Quota::per_second(nonzero!(2u32)); // Very restrictive global limit
        let middleware = RateLimitMiddleware::<()>::with_global_limit(per_conn_quota, global_quota);

        let connection1 = "conn1".to_string();
        let connection2 = "conn2".to_string();

        // Create valid test event
        let keys = Keys::generate();
        let test_event = create_test_event(&keys, 1, vec![]).await;
        let message = Some(ClientMessage::event(test_event));

        // First two requests should pass (using up global limit)
        let (tx1, _rx1) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(10);
        let mut ctx1 = create_test_inbound_context(
            connection1.clone(),
            message.clone(),
            Some(tx1),
            Default::default(),
            vec![],
            0,
        );
        assert!(middleware.process_inbound(ctx1.as_context()).await.is_ok());

        let (tx2, _rx2) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(10);
        let mut ctx2 = create_test_inbound_context(
            connection2.clone(),
            message.clone(),
            Some(tx2),
            Default::default(),
            vec![],
            0,
        );
        assert!(middleware.process_inbound(ctx2.as_context()).await.is_ok());

        // Third request should be blocked by global limit even though per-connection limit allows it
        let (tx3, _rx3) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(10);
        let mut ctx3 = create_test_inbound_context(
            connection1.clone(),
            message.clone(),
            Some(tx3),
            Default::default(),
            vec![],
            0,
        );
        let result3 = middleware.process_inbound(ctx3.as_context()).await;
        assert!(result3.is_err(), "Should be blocked by global limit");

        // Verify it's the correct type of error
        let error = result3.unwrap_err();
        let relay_error = error
            .downcast_ref::<Error>()
            .expect("Should be relay Error");
        match relay_error {
            Error::Restricted { message, .. } => {
                assert!(
                    message.contains("rate limit"),
                    "Error message should mention rate limit"
                );
            }
            _ => panic!("Expected Restricted error for rate limiting"),
        }
    }
}
