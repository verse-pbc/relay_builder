//! Cryptographic operations for events

use crate::error::{Error, Result};
use crate::subscription_coordinator::StoreCommand;
use futures_util::future::join_all;
use nostr_sdk::prelude::*;
use rayon::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info};

/// Handle for cryptographic operations on Nostr events
#[derive(Clone)]
pub struct CryptoHelper {
    /// Keys for signing events
    keys: Arc<Keys>,
    /// Verification request sender
    verify_sender: mpsc::Sender<VerifyRequest>,
    /// Signing request sender
    sign_sender: mpsc::Sender<StoreCommand>,
    /// Stats counter for verified events
    verified_count: Arc<AtomicUsize>,
    /// Stats counter for signed events
    signed_count: Arc<AtomicUsize>,
}

/// Request to verify an event
struct VerifyRequest {
    event: Event,
    response: oneshot::Sender<Result<()>>,
}

impl std::fmt::Debug for CryptoHelper {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CryptoHelper").finish()
    }
}

/// Statistics tracker for batch processing
#[allow(clippy::struct_field_names)] // batch_ prefix is intentional and clear
struct BatchStats {
    batch_count: AtomicUsize,
    batch_size_sum: AtomicUsize,
    batch_size_max: AtomicUsize,
}

impl BatchStats {
    fn new() -> Self {
        Self {
            batch_count: AtomicUsize::new(0),
            batch_size_sum: AtomicUsize::new(0),
            batch_size_max: AtomicUsize::new(0),
        }
    }

    fn record_batch(&self, batch_size: usize, total_processed: usize, operation_name: &str) {
        let batch_num = self.batch_count.fetch_add(1, Ordering::Relaxed);
        self.batch_size_sum.fetch_add(batch_size, Ordering::Relaxed);

        // Update max batch size
        let mut current_max = self.batch_size_max.load(Ordering::Relaxed);
        while current_max < batch_size {
            match self.batch_size_max.compare_exchange_weak(
                current_max,
                batch_size,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(x) => current_max = x,
            }
        }

        // Log every 100 batches
        #[allow(clippy::manual_is_multiple_of)]
        if batch_num > 0 && batch_num % 100 == 0 {
            let sum = self.batch_size_sum.load(Ordering::Relaxed);
            let max = self.batch_size_max.load(Ordering::Relaxed);
            let avg = sum / (batch_num + 1);

            info!(
                "Crypto {} batch stats: {} batches processed, avg size: {}, max size: {}, total processed: {}",
                operation_name, batch_num + 1, avg, max, total_processed
            );

            // Also log current batch size for comparison
            if batch_size > avg * 2 {
                info!(
                    "Current batch size {} is significantly above average",
                    batch_size
                );
            }
        }
    }
}

impl CryptoHelper {
    /// Create a new crypto helper with the given keys
    pub fn new(keys: Arc<Keys>) -> Self {
        // Create verification channel with reasonable capacity
        let (verify_sender, verify_receiver) = mpsc::channel::<VerifyRequest>(10000);
        let verified_count = Arc::new(AtomicUsize::new(0));

        // Create signing channel with reasonable capacity
        let (sign_sender, sign_receiver) = mpsc::channel::<StoreCommand>(10000);
        let signed_count = Arc::new(AtomicUsize::new(0));

        // Spawn the verification processor as a tokio task
        let verified_count_clone = Arc::clone(&verified_count);
        tokio::spawn(Self::run_verify_processor(
            verify_receiver,
            verified_count_clone,
        ));

        // Spawn the signing processor as a tokio task
        let signed_count_clone = Arc::clone(&signed_count);
        let keys_clone = Arc::clone(&keys);
        tokio::spawn(Self::run_sign_processor(
            sign_receiver,
            keys_clone,
            signed_count_clone,
        ));

        Self {
            keys,
            verify_sender,
            sign_sender,
            verified_count,
            signed_count,
        }
    }

    /// Run the verification processor that batches and parallelizes verification
    async fn run_verify_processor(
        mut receiver: mpsc::Receiver<VerifyRequest>,
        verified_count: Arc<AtomicUsize>,
    ) {
        use once_cell::sync::Lazy;
        static STATS: Lazy<BatchStats> = Lazy::new(BatchStats::new);

        info!("Crypto verification processor started");

        // Reusable batch buffer to avoid allocations
        let mut batch: Vec<VerifyRequest> = Vec::with_capacity(1000);

        loop {
            batch.clear();

            // Wait for at least one verification request
            let Some(first_request) = receiver.recv().await else {
                info!("Crypto verification processor shutting down - channel closed");
                break;
            };
            batch.push(first_request);

            // Eagerly drain any additional pending requests
            while let Ok(request) = receiver.try_recv() {
                batch.push(request);
            }

            let batch_size = batch.len();
            debug!("Processing verification batch of {} events", batch_size);

            // Extract events and senders for processing
            let requests = std::mem::take(&mut batch);

            // Process verification in spawn_blocking with rayon, return results
            let results = tokio::task::spawn_blocking(move || {
                requests
                    .into_par_iter()
                    .map(|request| {
                        let VerifyRequest { event, response } = request;

                        // Perform the actual verification
                        let result = event.verify().map_err(|e| {
                            debug!("Event verification failed: {:?}", e);
                            Error::protocol(format!("Invalid event signature: {e}"))
                        });

                        (response, result)
                    })
                    .collect::<Vec<_>>()
            })
            .await
            .expect("spawn_blocking panicked");

            // Send responses from tokio context (SAFE - no cross-thread wake)
            for (response, result) in results {
                let _ = response.send(result);
            }

            // Update stats
            verified_count.fetch_add(batch_size, Ordering::Relaxed);

            // Log batch statistics
            STATS.record_batch(
                batch_size,
                verified_count.load(Ordering::Relaxed),
                "verification",
            );
        }

        info!("Crypto verification processor stopped");
    }

    /// Run the signing processor that batches and parallelizes signing
    async fn run_sign_processor(
        mut receiver: mpsc::Receiver<StoreCommand>,
        keys: Arc<Keys>,
        signed_count: Arc<AtomicUsize>,
    ) {
        use once_cell::sync::Lazy;
        static STATS: Lazy<BatchStats> = Lazy::new(BatchStats::new);

        info!("Crypto signing processor started");

        // Reusable batch buffer to avoid allocations
        let mut batch: Vec<StoreCommand> = Vec::with_capacity(1000);

        loop {
            batch.clear();

            // Wait for at least one signing request
            let Some(first_command) = receiver.recv().await else {
                info!("Crypto signing processor shutting down - channel closed");
                break;
            };
            batch.push(first_command);

            // Eagerly drain any additional pending requests
            while let Ok(command) = receiver.try_recv() {
                batch.push(command);
            }

            let batch_size = batch.len();
            debug!("Processing signing batch of {} events", batch_size);

            // Create futures for all signing operations
            let sign_futures: Vec<_> = batch
                .drain(..)
                .filter_map(|command| {
                    if let StoreCommand::SaveUnsignedEvent(event, scope, response_handler) = command
                    {
                        let keys = Arc::clone(&keys);
                        Some(async move {
                            let signed_result = keys
                                .sign_event(event)
                                .await
                                .map_err(|e| Error::internal(format!("Failed to sign event: {e}")));

                            let response = match signed_result {
                                Ok(signed_event) => Ok(Some(StoreCommand::SaveSignedEvent(
                                    Box::new(signed_event),
                                    scope,
                                    None,
                                ))),
                                Err(e) => Err(e),
                            };

                            (response_handler, response)
                        })
                    } else {
                        error!("Non-SaveUnsignedEvent command received in signing processor");
                        None
                    }
                })
                .collect();

            // Execute all signing operations concurrently
            let results = join_all(sign_futures).await;

            // Send responses from tokio context (SAFE - no cross-thread wake)
            for (response_handler, response) in results {
                if let Some(sender) = response_handler {
                    let _ = sender.send(response);
                }
            }

            // Update stats
            signed_count.fetch_add(batch_size, Ordering::Relaxed);

            // Log batch statistics
            STATS.record_batch(batch_size, signed_count.load(Ordering::Relaxed), "signing");
        }

        info!("Crypto signing processor stopped");
    }

    /// Sign an unsigned event with the configured keys
    ///
    /// # Errors
    ///
    /// Returns error if event signing fails.
    pub async fn sign_event(&self, event: UnsignedEvent) -> Result<Event> {
        self.keys.sign_event(event).await.map_err(|e| {
            error!("Failed to sign event: {:?}", e);
            Error::internal(format!("Failed to sign event: {e}"))
        })
    }

    /// Verify an event's signature
    ///
    /// # Errors
    ///
    /// Returns error if signature is invalid or verification fails.
    pub async fn verify_event(&self, event: Event) -> Result<()> {
        // Create oneshot channel for response
        let (tx, rx) = oneshot::channel();

        // Send verification request
        self.verify_sender
            .send(VerifyRequest {
                event,
                response: tx,
            })
            .await
            .map_err(|_| Error::internal("Verification processor unavailable"))?;

        // Await response
        rx.await
            .map_err(|_| Error::internal("Verification processor dropped response"))?
    }

    /// Get the number of events verified
    #[must_use]
    pub fn verified_count(&self) -> usize {
        self.verified_count.load(Ordering::Relaxed)
    }

    /// Sign a store command (converts `SaveUnsignedEvent` to `SaveSignedEvent`)
    ///
    /// # Errors
    ///
    /// Returns error if command is not `SaveUnsignedEvent` or signing fails.
    pub async fn sign_store_command(&self, command: StoreCommand) -> Result<()> {
        if let StoreCommand::SaveUnsignedEvent(..) = command {
            // Send to the signing processor for batched processing
            self.sign_sender
                .send(command)
                .await
                .map_err(|_| Error::internal("Signing processor unavailable"))?;
            Ok(())
        } else {
            error!("sign_store_command called with non-SaveUnsignedEvent command");
            Err(Error::internal("Expected SaveUnsignedEvent command"))
        }
    }

    /// Get the number of events signed
    #[must_use]
    pub fn signed_count(&self) -> usize {
        self.signed_count.load(Ordering::Relaxed)
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
        let signed_event = helper.sign_event(unsigned_event).await.unwrap();

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
        let result = helper.verify_event(signed_event.clone()).await;
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
        let result = helper.verify_event(event).await;
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

                helper_clone.sign_event(unsigned_event).await
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

                helper_clone.verify_event(signed_event).await
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

                helper_clone.sign_event(unsigned_event).await
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
