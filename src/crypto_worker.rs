//! Background crypto worker for CPU-intensive operations
//!
//! Offloads signature verification and signing to dedicated threads
//! to avoid blocking the async runtime.

use crate::error::{Error, Result};
use nostr_sdk::prelude::*;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::task::TaskTracker;
use tracing::{debug, error, info};

/// Operations that can be performed by the crypto worker
#[derive(Debug)]
enum CryptoOperation {
    Sign {
        event: UnsignedEvent,
        response: oneshot::Sender<Result<Event>>,
    },
    Verify {
        event: Event,
        response: oneshot::Sender<Result<()>>,
    },
}

/// Handle for sending crypto operations to worker pool
#[derive(Clone, Debug)]
pub struct CryptoSender {
    tx: flume::Sender<CryptoOperation>,
}

impl CryptoSender {
    pub async fn sign_event(&self, event: UnsignedEvent) -> Result<Event> {
        let (response_tx, response_rx) = oneshot::channel();

        self.tx
            .send_async(CryptoOperation::Sign {
                event,
                response: response_tx,
            })
            .await
            .map_err(|_| {
                error!("Crypto worker queue full, rejecting sign operation");
                Error::internal(
                    "Crypto worker unavailable - queue may be full or workers shut down",
                )
            })?;

        response_rx.await.map_err(|_| {
            Error::internal("Crypto worker dropped before completing signing operation")
        })?
    }

    pub async fn verify_event(&self, event: Event) -> Result<()> {
        let (response_tx, response_rx) = oneshot::channel();

        self.tx
            .send_async(CryptoOperation::Verify {
                event,
                response: response_tx,
            })
            .await
            .map_err(|_| {
                error!("Crypto worker queue full, rejecting verify operation");
                Error::internal(
                    "Crypto worker unavailable - queue may be full or workers shut down",
                )
            })?;

        response_rx.await.map_err(|_| {
            Error::internal("Crypto worker dropped before completing verification operation")
        })?
    }
}

/// Namespace for spawning crypto workers
///
/// These workers handle signature verification and signing operations.
/// Can be configured via CRYPTO_WORKER_THREADS environment variable.
#[derive(Debug)]
pub struct CryptoWorker;

impl CryptoWorker {
    /// Spawn crypto workers and return sender
    /// The workers will be tracked by the provided TaskTracker
    pub fn spawn(keys: Arc<Keys>, task_tracker: &TaskTracker) -> CryptoSender {
        let cpu_count = std::env::var("CRYPTO_WORKER_THREADS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3);

        let queue_multiplier = std::env::var("CRYPTO_WORKER_QUEUE_MULTIPLIER")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(100); // Increased for better batching

        info!(
            "Starting {} crypto workers with queue size {}",
            cpu_count,
            cpu_count * queue_multiplier
        );

        // Use bounded channel for natural backpressure
        let (tx, rx) = flume::bounded(cpu_count * queue_multiplier);

        // Spawn long-lived workers using spawn_blocking
        for i in 0..cpu_count {
            let rx = rx.clone();
            let worker_keys = Arc::clone(&keys);

            task_tracker.spawn_blocking(move || {
                debug!("Crypto worker {} started", i);

                // Process operations in batches for better performance
                loop {
                    // Block until at least one operation is available
                    let first_op = match rx.recv() {
                        Ok(op) => op,
                        Err(_) => {
                            info!("Crypto worker {} shutting down - channel closed, all senders dropped", i);
                            break;
                        }
                    };

                    // Drain all currently available operations
                    let mut batch = vec![first_op];
                    batch.extend(rx.drain());

                    debug!(
                        "Crypto worker {} processing batch of {} operations",
                        i,
                        batch.len()
                    );

                    // Process the batch
                    for op in batch {
                        match op {
                            CryptoOperation::Sign { event, response } => {
                                // Sign synchronously using sign_with_keys
                                let result = event.sign_with_keys(&worker_keys).map_err(|e| {
                                    error!("Failed to sign event: {:?}", e);
                                    Error::internal(format!("Failed to sign event: {e}"))
                                });

                                if let Err(result) = response.send(result) {
                                    error!("Failed to send sign response: {:?}", result);
                                }
                            }
                            CryptoOperation::Verify { event, response } => {
                                // Verification is synchronous - no async needed
                                let result = event.verify().map_err(|e| {
                                    debug!("Event verification failed: {:?}", e);
                                    Error::protocol(format!("Invalid event signature: {e}"))
                                });

                                if let Err(result) = response.send(result) {
                                    error!("Failed to send verify response: {:?}", result);
                                }
                            }
                        }
                    }
                }

                debug!("Crypto worker {} stopped", i);
            });
        }

        CryptoSender { tx }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn test_sign_event() {
        let keys = Arc::new(Keys::generate());
        let task_tracker = TaskTracker::new();
        let sender = CryptoWorker::spawn(Arc::clone(&keys), &task_tracker);

        // Create test event
        let unsigned_event = UnsignedEvent::new(
            keys.public_key(),
            Timestamp::now(),
            Kind::TextNote,
            vec![],
            "Test message",
        );

        // Sign the event
        let signed_event = sender.sign_event(unsigned_event).await.unwrap();

        // Verify the signature is valid
        assert!(signed_event.verify().is_ok());
        assert_eq!(signed_event.pubkey, keys.public_key());
        assert_eq!(signed_event.content, "Test message");

        // Cleanup - drop sender and wait for workers
        drop(sender);
        task_tracker.close();
        task_tracker.wait().await;
    }

    #[tokio::test]
    async fn test_verify_event() {
        let keys = Arc::new(Keys::generate());
        let task_tracker = TaskTracker::new();
        let sender = CryptoWorker::spawn(Arc::clone(&keys), &task_tracker);

        // Create and sign event
        let unsigned_event = UnsignedEvent::new(
            keys.public_key(),
            Timestamp::now(),
            Kind::TextNote,
            vec![],
            "Test verification",
        );
        let signed_event = keys.sign_event(unsigned_event).await.unwrap();

        // Verify the event through crypto worker
        let result = sender.verify_event(signed_event.clone()).await;
        assert!(result.is_ok());

        // Also verify directly to ensure correctness
        assert!(signed_event.verify().is_ok());

        // Cleanup
        drop(sender);
        task_tracker.close();
        task_tracker.wait().await;
    }

    #[tokio::test]
    async fn test_invalid_event_verification() {
        let keys = Arc::new(Keys::generate());
        let task_tracker = TaskTracker::new();
        let sender = CryptoWorker::spawn(Arc::clone(&keys), &task_tracker);

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
        let result = sender.verify_event(event).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid event signature"));

        // Cleanup
        drop(sender);
        task_tracker.close();
        task_tracker.wait().await;
    }

    #[tokio::test]
    async fn test_concurrent_operations() {
        let keys = Arc::new(Keys::generate());
        let task_tracker = TaskTracker::new();
        let sender = CryptoWorker::spawn(Arc::clone(&keys), &task_tracker);

        // Spawn multiple concurrent operations
        let mut sign_handles = vec![];
        let mut verify_handles = vec![];

        // Spawn signing tasks
        for i in 0..50 {
            let sender_clone = sender.clone();
            let keys_clone = Arc::clone(&keys);

            let handle = tokio::spawn(async move {
                let unsigned_event = UnsignedEvent::new(
                    keys_clone.public_key(),
                    Timestamp::now(),
                    Kind::TextNote,
                    vec![],
                    format!("Test message {i}"),
                );

                sender_clone.sign_event(unsigned_event).await
            });

            sign_handles.push(handle);
        }

        // Spawn verification tasks
        for i in 0..50 {
            let sender_clone = sender.clone();
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

                sender_clone.verify_event(signed_event).await
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

        // Cleanup
        drop(sender);
        task_tracker.close();
        task_tracker.wait().await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_graceful_shutdown() {
        let keys = Arc::new(Keys::generate());
        let task_tracker = TaskTracker::new();
        let sender = CryptoWorker::spawn(Arc::clone(&keys), &task_tracker);

        // Queue some operations
        let mut handles = vec![];
        for i in 0..10 {
            let sender_clone = sender.clone();
            let keys_clone = Arc::clone(&keys);

            let handle = tokio::spawn(async move {
                let unsigned_event = UnsignedEvent::new(
                    keys_clone.public_key(),
                    Timestamp::now(),
                    Kind::TextNote,
                    vec![],
                    format!("Shutdown test {i}"),
                );

                sender_clone.sign_event(unsigned_event).await
            });

            handles.push(handle);
        }

        // Give operations time to start
        tokio::time::sleep(Duration::from_millis(10)).await;

        // Cancel and drop the sender
        drop(sender);
        task_tracker.close();
        task_tracker.wait().await;

        // Check results - some may have completed, some may have failed
        let mut successes = 0;
        let mut failures = 0;

        for handle in handles {
            match handle.await {
                Ok(Ok(_)) => successes += 1,
                Ok(Err(_)) => failures += 1,
                Err(_) => failures += 1, // JoinError
            }
        }

        // At least some operations should have completed
        assert!(successes > 0, "Some operations should have completed");
        assert_eq!(
            successes + failures,
            10,
            "All operations should be accounted for"
        );
    }

    #[tokio::test]
    async fn test_backpressure() {
        // Set up a small queue to test backpressure
        std::env::set_var("CRYPTO_WORKER_THREADS", "1");
        std::env::set_var("CRYPTO_WORKER_QUEUE_MULTIPLIER", "2");

        let keys = Arc::new(Keys::generate());
        let task_tracker = TaskTracker::new();
        let sender = CryptoWorker::spawn(Arc::clone(&keys), &task_tracker);

        // Try to overflow the queue
        let mut handles = vec![];
        for i in 0..10 {
            let sender_clone = sender.clone();
            let keys_clone = Arc::clone(&keys);

            let handle = tokio::spawn(async move {
                let unsigned_event = UnsignedEvent::new(
                    keys_clone.public_key(),
                    Timestamp::now(),
                    Kind::TextNote,
                    vec![],
                    format!("Backpressure test {i}"),
                );

                // Add small delay to simulate slow processing
                tokio::time::sleep(Duration::from_millis(10)).await;
                sender_clone.sign_event(unsigned_event).await
            });

            handles.push(handle);
        }

        // Wait for all operations
        let mut successes = 0;
        let mut _failures = 0;

        for handle in handles {
            match handle.await.unwrap() {
                Ok(_) => successes += 1,
                Err(_) => _failures += 1,
            }
        }

        // With a queue size of 2 and slow processing, some operations might fail
        // But we should have at least some successes
        assert!(successes > 0);

        // Cleanup
        drop(sender);
        task_tracker.close();
        task_tracker.wait().await;
        std::env::remove_var("CRYPTO_WORKER_THREADS");
        std::env::remove_var("CRYPTO_WORKER_QUEUE_MULTIPLIER");
    }
}
