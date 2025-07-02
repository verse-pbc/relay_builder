//! Cryptographic operations for events

use crate::error::{Error, Result};
use nostr_sdk::prelude::*;
use std::sync::Arc;
use tracing::{debug, error};

/// Handle for cryptographic operations on Nostr events
#[derive(Clone)]
pub struct CryptoHelper {
    /// Keys for signing events
    keys: Arc<Keys>,
}

impl std::fmt::Debug for CryptoHelper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CryptoHelper").finish()
    }
}

impl CryptoHelper {
    /// Create a new crypto helper with the given keys
    pub fn new(keys: Arc<Keys>) -> Self {
        Self { keys }
    }

    /// Sign an unsigned event with the configured keys
    pub fn sign_event(&self, event: UnsignedEvent) -> Result<Event> {
        event.sign_with_keys(&self.keys).map_err(|e| {
            error!("Failed to sign event: {:?}", e);
            Error::internal(format!("Failed to sign event: {e}"))
        })
    }

    /// Verify an event's signature
    pub fn verify_event(&self, event: &Event) -> Result<()> {
        event.verify().map_err(|e| {
            debug!("Event verification failed: {:?}", e);
            Error::protocol(format!("Invalid event signature: {e}"))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_sign_event() {
        let keys = Arc::new(Keys::generate());
        let helper = CryptoHelper::new(Arc::clone(&keys));

        // Create test event
        let unsigned_event = UnsignedEvent::new(
            keys.public_key(),
            Timestamp::now(),
            Kind::TextNote,
            vec![],
            "Test message",
        );

        // Sign the event
        let signed_event = helper.sign_event(unsigned_event).unwrap();

        // Verify the signature is valid
        assert!(signed_event.verify().is_ok());
        assert_eq!(signed_event.pubkey, keys.public_key());
        assert_eq!(signed_event.content, "Test message");
    }

    #[tokio::test]
    async fn test_verify_event() {
        let keys = Arc::new(Keys::generate());
        let helper = CryptoHelper::new(Arc::clone(&keys));

        // Create and sign event
        let unsigned_event = UnsignedEvent::new(
            keys.public_key(),
            Timestamp::now(),
            Kind::TextNote,
            vec![],
            "Test verification",
        );
        let signed_event = keys.sign_event(unsigned_event).await.unwrap();

        // Verify the event through crypto helper
        let result = helper.verify_event(&signed_event);
        assert!(result.is_ok());

        // Also verify directly to ensure correctness
        assert!(signed_event.verify().is_ok());
    }

    #[tokio::test]
    async fn test_invalid_event_verification() {
        let keys = Arc::new(Keys::generate());
        let helper = CryptoHelper::new(Arc::clone(&keys));

        // Create an event with invalid signature by modifying content after signing
        let unsigned = UnsignedEvent::new(
            keys.public_key(),
            Timestamp::now(),
            Kind::TextNote,
            vec![],
            "Original content",
        );
        let mut event = keys.sign_event(unsigned).await.unwrap();

        // Tamper with the event
        event.content = "Modified content".to_string();

        // Verify should fail
        let result = helper.verify_event(&event);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid event signature"));
    }

    #[tokio::test]
    async fn test_concurrent_operations() {
        let keys = Arc::new(Keys::generate());
        let helper = CryptoHelper::new(Arc::clone(&keys));

        // Spawn multiple concurrent operations
        let mut sign_handles = vec![];
        let mut verify_handles = vec![];

        // Spawn signing tasks
        for i in 0..50 {
            let helper_clone = helper.clone();
            let keys_clone = Arc::clone(&keys);

            let handle = tokio::spawn(async move {
                let unsigned_event = UnsignedEvent::new(
                    keys_clone.public_key(),
                    Timestamp::now(),
                    Kind::TextNote,
                    vec![],
                    format!("Test message {i}"),
                );

                helper_clone.sign_event(unsigned_event)
            });

            sign_handles.push(handle);
        }

        // Spawn verification tasks
        for i in 0..50 {
            let helper_clone = helper.clone();
            let keys_clone = Arc::clone(&keys);

            let handle = tokio::spawn(async move {
                let unsigned_event = UnsignedEvent::new(
                    keys_clone.public_key(),
                    Timestamp::now(),
                    Kind::TextNote,
                    vec![],
                    format!("Verify message {i}"),
                );
                let signed_event = keys_clone.sign_event(unsigned_event).await.unwrap();

                helper_clone.verify_event(&signed_event)
            });

            verify_handles.push(handle);
        }

        // Wait for all operations to complete
        for handle in sign_handles {
            let result = handle.await.unwrap();
            assert!(result.is_ok());
            // Verify the signed event is valid
            assert!(result.unwrap().verify().is_ok());
        }

        for handle in verify_handles {
            let result = handle.await.unwrap();
            assert!(result.is_ok());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_graceful_shutdown() {
        let keys = Arc::new(Keys::generate());
        let helper = CryptoHelper::new(Arc::clone(&keys));

        // Queue some operations
        let mut handles = vec![];
        for i in 0..10 {
            let helper_clone = helper.clone();
            let keys_clone = Arc::clone(&keys);

            let handle = tokio::spawn(async move {
                let unsigned_event = UnsignedEvent::new(
                    keys_clone.public_key(),
                    Timestamp::now(),
                    Kind::TextNote,
                    vec![],
                    format!("Shutdown test {i}"),
                );

                helper_clone.sign_event(unsigned_event)
            });

            handles.push(handle);
        }

        // Give operations time to start
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Check results - all should complete since we use block_in_place
        for handle in handles {
            let result = handle.await.unwrap();
            assert!(result.is_ok());
        }
    }
}
