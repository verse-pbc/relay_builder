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
    /// Signing request sender
    sign_sender: flume::Sender<StoreCommand>,
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
        let (verify_sender, verify_receiver) = flume::bounded::<VerifyRequest>(10000);
        let verified_count = Arc::new(AtomicUsize::new(0));

        // Create signing channel with reasonable capacity
        let (sign_sender, sign_receiver) = flume::bounded::<StoreCommand>(10000);
        let signed_count = Arc::new(AtomicUsize::new(0));

        // Spawn the verification processor
        let verified_count_clone = Arc::clone(&verified_count);
        std::thread::spawn(move || {
            Self::run_verify_processor(&verify_receiver, &verified_count_clone);
        });

        // Spawn the signing processor
        let signed_count_clone = Arc::clone(&signed_count);
        let keys_clone = Arc::clone(&keys);
        std::thread::spawn(move || {
            Self::run_sign_processor(&sign_receiver, &keys_clone, &signed_count_clone);
        });

        Self {
            keys,
            verify_sender,
            sign_sender,
            verified_count,
            signed_count,
        }
    }

    /// Run the verification processor that batches and parallelizes verification
    fn run_verify_processor(
        receiver: &flume::Receiver<VerifyRequest>,
        verified_count: &Arc<AtomicUsize>,
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

            // Log batch statistics
            use once_cell::sync::Lazy;
            static STATS: Lazy<BatchStats> = Lazy::new(BatchStats::new);
            STATS.record_batch(
                batch_size,
                verified_count.load(Ordering::Relaxed),
                "verification",
            );
        }

        info!("Crypto verification processor stopped");
    }

    /// Run the signing processor that batches and parallelizes signing
    fn run_sign_processor(
        receiver: &flume::Receiver<StoreCommand>,
        keys: &Arc<Keys>,
        signed_count: &Arc<AtomicUsize>,
    ) {
        info!("Crypto signing processor started");

        // Initialize rayon thread pool for CPU-bound work
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_cpus::get())
            .thread_name(|idx| format!("crypto-sign-{idx}"))
            .build()
            .expect("Failed to create rayon thread pool");

        // Use tokio runtime for async signing operations
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("Failed to create tokio runtime");

        loop {
            // Wait for at least one signing request
            let first_command = match receiver.recv() {
                Ok(cmd) => cmd,
                Err(_) => {
                    info!("Crypto signing processor shutting down - channel closed");
                    break;
                }
            };

            // Collect a batch using the eager consumption pattern
            let batch: Vec<StoreCommand> = std::iter::once(first_command)
                .chain(receiver.drain())
                .collect();

            let batch_size = batch.len();
            debug!("Processing signing batch of {} events", batch_size);

            // Process the batch in parallel using rayon
            pool.install(|| {
                batch.into_par_iter().for_each(|command| {
                    if let StoreCommand::SaveUnsignedEvent(event, scope, response_handler) = command
                    {
                        // Sign the event using block_in_place to run async code
                        let signed_result = runtime.block_on(async {
                            keys.sign_event(event)
                                .await
                                .map_err(|e| Error::internal(format!("Failed to sign event: {e}")))
                        });

                        // Send the result back via the oneshot
                        if let Some(sender) = response_handler {
                            let response = match signed_result {
                                Ok(signed_event) => Ok(Some(StoreCommand::SaveSignedEvent(
                                    Box::new(signed_event),
                                    scope,
                                    None,
                                ))),
                                Err(e) => Err(e),
                            };
                            // Ignore send errors if receiver dropped
                            let _ = sender.send(response);
                        }
                    } else {
                        error!("Non-SaveUnsignedEvent command received in signing processor");
                    }
                });
            });

            // Update stats
            signed_count.fetch_add(batch_size, Ordering::Relaxed);

            // Log batch statistics
            use once_cell::sync::Lazy;
            static STATS: Lazy<BatchStats> = Lazy::new(BatchStats::new);
            STATS.record_batch(batch_size, signed_count.load(Ordering::Relaxed), "signing");
        }

        info!("Crypto signing processor stopped");
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
    pub async fn sign_store_command(&self, command: StoreCommand) -> Result<()> {
        match command {
            StoreCommand::SaveUnsignedEvent(..) => {
                // Send to the signing processor for batched processing
                self.sign_sender
                    .send_async(command)
                    .await
                    .map_err(|_| Error::internal("Signing processor unavailable"))?;
                Ok(())
            }
            _ => {
                error!("sign_store_command called with non-SaveUnsignedEvent command");
                Err(Error::internal("Expected SaveUnsignedEvent command"))
            }
        }
    }

    /// Get the number of events signed
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
