//! Integration tests for error handling middleware

use nostr_sdk::prelude::*;
use relay_builder::{ErrorHandlingMiddleware, RelayBuilder, RelayConfig};

#[tokio::test]
async fn test_error_handling_middleware_in_relay() {
    // This test verifies that ErrorHandlingMiddleware is properly integrated
    // Create a simple relay with error handling middleware
    let temp_dir = tempfile::tempdir().unwrap();
    let db_path = temp_dir
        .path()
        .join("test_db")
        .to_string_lossy()
        .to_string();
    let keys = Keys::generate();
    let config = RelayConfig::new("ws://test", db_path, keys);

    // ErrorHandlingMiddleware is now added by default
    // Test that we can build a relay (which will include ErrorHandlingMiddleware automatically)
    let _handler_factory = RelayBuilder::<()>::new(config)
        .build()
        .await
        .expect("Should build relay with default error handling");

    // The actual middleware execution is tested in the error_handling.rs unit tests
    // and through the websocket_builder's integration test pattern
}

#[test]
fn test_error_handling_middleware_creation() {
    // Verify the middleware can be created
    let _middleware = ErrorHandlingMiddleware::new();
}

#[test]
fn test_error_handling_middleware_debug_trait() {
    let middleware = ErrorHandlingMiddleware::new();
    let debug_str = format!("{middleware:?}");
    assert!(debug_str.contains("ErrorHandlingMiddleware"));
}
