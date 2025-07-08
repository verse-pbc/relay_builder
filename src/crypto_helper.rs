//! Cryptographic operations for events

use crate::error::{Error, Result};
use crate::subscription_coordinator::StoreCommand;
use nostr_sdk::prelude::*;
use rayon::prelude::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use tokio::sync::oneshot;
use tracing::{debug, error, info};

/// Handle for cryptographic operations on Nostr events
#[derive(Clone)]
pub struct CryptoHelper {
    /// Keys for signing events
    keys: Arc<Keys>,
    /// Verification request sender
    verify_sender: flume::Sender<VerifyRequest>,
    /// Stats counter for verified events
    verified_count: Arc<AtomicUsize>,
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

impl CryptoHelper {
    /// Create a new crypto helper with the given keys
    pub fn new(keys: Arc<Keys>) -> Self {
        // Create verification channel with reasonable capacity
        let (verify_sender, verify_receiver) = flume::bounded::<VerifyRequest>(10000);
        let verified_count = Arc::new(AtomicUsize::new(0));

        // Spawn the verification processor
        let verified_count_clone = Arc::clone(&verified_count);
        std::thread::spawn(move || {
            Self::run_verify_processor(verify_receiver, verified_count_clone);
        });

        Self {
            keys,
            verify_sender,
            verified_count,
        }
    }

    /// Run the verification processor that batches and parallelizes verification
    fn run_verify_processor(
        receiver: flume::Receiver<VerifyRequest>,
        verified_count: Arc<AtomicUsize>,
    ) {
        info!("Crypto verification processor started");

        // Initialize rayon thread pool for CPU-bound work
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_cpus::get())
            .thread_name(|idx| format!("crypto-verify-{idx}"))
            .build()
            .expect("Failed to create rayon thread pool");

        loop {
            // Wait for at least one verification request
            let first_request = match receiver.recv() {
                Ok(req) => req,
                Err(_) => {
                    info!("Crypto verification processor shutting down - channel closed");
                    break;
                }
            };

            // Collect a batch using the eager consumption pattern
            let batch: Vec<VerifyRequest> = std::iter::once(first_request)
                .chain(receiver.drain())
                .collect();

            let batch_size = batch.len();
            debug!("Processing verification batch of {} events", batch_size);

            // Process the batch in parallel using rayon
            pool.install(|| {
                batch.into_par_iter().for_each(|request| {
                    let VerifyRequest { event, response } = request;

                    // Perform the actual verification
                    let result = event.verify().map_err(|e| {
                        debug!("Event verification failed: {:?}", e);
                        Error::protocol(format!("Invalid event signature: {e}"))
                    });

                    // Send the result back (ignore send errors if receiver dropped)
                    let _ = response.send(result);
                });
            });

            // Update stats
            verified_count.fetch_add(batch_size, Ordering::Relaxed);

            // Log batch size periodically (every 100 batches)
            static BATCH_COUNT: AtomicUsize = AtomicUsize::new(0);
            static BATCH_SIZE_SUM: AtomicUsize = AtomicUsize::new(0);
            static BATCH_SIZE_MAX: AtomicUsize = AtomicUsize::new(0);

            let batch_num = BATCH_COUNT.fetch_add(1, Ordering::Relaxed);
            BATCH_SIZE_SUM.fetch_add(batch_size, Ordering::Relaxed);

            // Update max batch size
            let mut current_max = BATCH_SIZE_MAX.load(Ordering::Relaxed);
            while current_max < batch_size {
                match BATCH_SIZE_MAX.compare_exchange_weak(
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
            if batch_num > 0 && batch_num % 100 == 0 {
                let sum = BATCH_SIZE_SUM.load(Ordering::Relaxed);
                let max = BATCH_SIZE_MAX.load(Ordering::Relaxed);
                let avg = sum / (batch_num + 1);
                let total = verified_count.load(Ordering::Relaxed);

                info!(
                    "Crypto verification batch stats: {} batches processed, avg size: {}, max size: {}, total verified: {}",
                    batch_num + 1, avg, max, total
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

        info!("Crypto verification processor stopped");
    }

    /// Sign an unsigned event with the configured keys
    pub async fn sign_event(&self, event: UnsignedEvent) -> Result<Event> {
        self.keys.sign_event(event).await.map_err(|e| {
            error!("Failed to sign event: {:?}", e);
            Error::internal(format!("Failed to sign event: {e}"))
        })
    }

    /// Verify an event's signature
    pub async fn verify_event(&self, event: Event) -> Result<()> {
        // Create oneshot channel for response
        let (tx, rx) = oneshot::channel();

        // Send verification request
        self.verify_sender
            .send_async(VerifyRequest {
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
    pub fn verified_count(&self) -> usize {
        self.verified_count.load(Ordering::Relaxed)
    }

    /// Sign a store command (converts SaveUnsignedEvent to SaveSignedEvent)
    pub async fn sign_store_command(&self, command: StoreCommand) -> Result<StoreCommand> {
        match command {
            StoreCommand::SaveUnsignedEvent(event, scope, response_handler) => {
                // Sign the event
                let signed_event = self.sign_event(event).await?;

                // Return the signed command with all context preserved
                Ok(StoreCommand::SaveSignedEvent(
                    Box::new(signed_event),
                    scope,
                    response_handler,
                ))
            }
            _ => {
                error!("sign_store_command called with non-SaveUnsignedEvent command");
                Err(Error::internal("Expected SaveUnsignedEvent command"))
            }
        }
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
