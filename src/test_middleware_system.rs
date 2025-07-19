//! Test the new middleware system to verify it works correctly
//!
//! This test creates a simple middleware chain and verifies the flow.

use crate::middleware_chain::chain;
use crate::middlewares::{ErrorHandlingMiddleware, NostrLoggerMiddleware};
use crate::nostr_middleware::NostrMessageSender;
use flume::{bounded, Receiver};
use nostr_sdk::prelude::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_middleware_chain_creation() {
        // Test that we can create a middleware chain with our converted middleware
        let error_middleware = ErrorHandlingMiddleware::new();
        let logger_middleware = NostrLoggerMiddleware::<()>::new();

        // Test individual middleware - specify the type parameter explicitly
        let _chain1 = chain::<()>().with(error_middleware.clone()).build();
        let _chain2 = chain::<()>().with(logger_middleware.clone()).build();

        // Test chained middleware
        let _combined_chain = chain::<()>()
            .with(error_middleware)
            .with(logger_middleware)
            .build();

        // If we get here, the middleware system compiled and can create chains
        // The fact that this compiles is the test
    }

    #[test]
    fn test_message_sender_creation() {
        let (tx, _rx): (_, Receiver<(RelayMessage, usize, Option<String>)>) = bounded(10);
        let sender = NostrMessageSender::new(tx, 0);

        // Test that we can create messages
        let notice = RelayMessage::notice("Test message");
        let event_id = EventId::all_zeros();
        let ok_msg = RelayMessage::ok(event_id, true, "success");

        // Test that the sender interface works (don't actually send)
        assert_eq!(sender.position(), 0);

        // Verify we can call the send methods (they would fail due to closed channel, but that's expected)
        let _ = sender.send(notice);
        let _ = sender.send_bypass(ok_msg);
    }

    #[test]
    fn test_middleware_traits() {
        // Test that our middleware implement the required traits
        let error_middleware = ErrorHandlingMiddleware::new();
        let logger_middleware = NostrLoggerMiddleware::<()>::new();

        // These should implement Clone
        let _cloned_error = error_middleware.clone();
        let _cloned_logger = logger_middleware.clone();

        // These should implement Debug
        let _debug_error = format!("{error_middleware:?}");
        let _debug_logger = format!("{logger_middleware:?}");

        // Test passes if all trait bounds are satisfied
        // The fact that this compiles is the test
    }
}
