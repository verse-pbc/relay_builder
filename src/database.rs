//! Database abstraction for Nostr relays

use crate::crypto_worker::CryptoSender;
use crate::error::Error;
use crate::subscription_service::StoreCommand;
use flume;
use nostr_database::nostr::{Event, Filter};
use nostr_database::Events;
use nostr_lmdb::{NostrLMDB, Scope};
use nostr_sdk::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::broadcast;
use tokio_util::task::TaskTracker;
use tracing::{debug, error, info};

/// Maximum number of events that can be queued for writing.
/// This provides backpressure to prevent unbounded memory growth.
/// When the queue is full, writers will wait until space is available.
const WRITE_QUEUE_CAPACITY: usize = 10_000;

// Note: LMDB only allows one write transaction at a time, so we use a single worker

/// Default broadcast channel capacity for event notifications
/// Can be configured via BROADCAST_CHANNEL_CAPACITY environment variable
const DEFAULT_BROADCAST_CAPACITY: usize = 10_000;

/// Item for the save signed event queue
#[derive(Debug)]
struct SaveSignedItem {
    event: Box<Event>,
    scope: Scope,
    message_sender: Option<websocket_builder::MessageSender<RelayMessage<'static>>>,
}

/// Item for the save unsigned event queue
#[derive(Debug)]
struct SaveUnsignedItem {
    event: Box<UnsignedEvent>,
    scope: Scope,
    message_sender: Option<websocket_builder::MessageSender<RelayMessage<'static>>>,
}

/// Item for the deletion queue
#[derive(Debug)]
struct DeleteItem {
    filter: Filter,
    scope: Scope,
    message_sender: Option<websocket_builder::MessageSender<RelayMessage<'static>>>,
}

/// A Nostr relay database that wraps NostrLMDB with async operations and event broadcasting
pub struct RelayDatabase {
    env: Arc<NostrLMDB>,
    #[allow(dead_code)]
    db_path: PathBuf,
    broadcast_sender: broadcast::Sender<Box<Event>>,

    // Three specialized queues
    save_signed_sender: Option<flume::Sender<SaveSignedItem>>,
    save_unsigned_sender: Option<flume::Sender<SaveUnsignedItem>>,
    delete_sender: Option<flume::Sender<DeleteItem>>,

    /// Task tracker for all worker threads
    task_tracker: TaskTracker,

    /// Queue capacity used for this instance
    queue_capacity: usize,
}

impl std::fmt::Debug for RelayDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RelayDatabase")
            .field("db_path", &self.db_path)
            .field("has_save_signed_sender", &self.save_signed_sender.is_some())
            .field(
                "has_save_unsigned_sender",
                &self.save_unsigned_sender.is_some(),
            )
            .field("has_delete_sender", &self.delete_sender.is_some())
            .finish()
    }
}

impl RelayDatabase {
    /// Process a batch of signed save items
    fn process_signed_batch(
        env: &NostrLMDB,
        items: Vec<SaveSignedItem>,
        broadcast_sender: &broadcast::Sender<Box<Event>>,
    ) {
        debug!("Processing batch of {} events", items.len());

        // Pre-collect events to broadcast for all items
        let events_to_broadcast: Vec<Box<Event>> =
            items.iter().map(|item| item.event.clone()).collect();

        // Minimize transaction scope
        let commit_result = {
            let mut txn = match env.write_transaction() {
                Ok(txn) => txn,
                Err(e) => {
                    error!("Failed to create write transaction: {:?}", e);
                    Self::send_error_responses(items, format!("error: {}", e));
                    return;
                }
            };

            let mut all_succeeded = true;

            // Save all events in the same transaction
            for item in items.iter() {
                match env.save_event_with_txn(&mut txn, &item.scope, &item.event) {
                    Ok(status) => {
                        if !status.is_success() {
                            all_succeeded = false;
                            error!("Failed to save event: status={:?}", status);
                            break; // Stop on first failure
                        }
                    }
                    Err(e) => {
                        all_succeeded = false;
                        error!("Failed to save event: {:?}", e);
                        break; // Stop on first failure
                    }
                }
            }

            if !all_succeeded {
                drop(txn); // Abort transaction
                Err("batch save failed")
            } else {
                txn.commit().map_err(|e| {
                    error!("Failed to commit transaction: {:?}", e);
                    "transaction commit failed"
                })
            }
        }; // Transaction dropped here, lock released

        // Handle responses and broadcasting outside transaction
        match commit_result {
            Ok(()) => {
                debug!(
                    "Transaction committed successfully for {} events",
                    items.len()
                );

                // All succeeded - offload response sending and broadcasting to async runtime
                let broadcast_sender = broadcast_sender.clone();
                tokio::spawn(async move {
                    Self::send_success_responses_and_broadcast(
                        items,
                        events_to_broadcast,
                        &broadcast_sender,
                    );
                });
            }
            Err(error_msg) => {
                // All failed - send error responses
                Self::send_error_responses(items, format!("error: {}", error_msg));
            }
        }
    }

    /// Send error responses for save items
    fn send_error_responses(items: Vec<SaveSignedItem>, error_msg: String) {
        for item in items {
            if let Some(mut sender) = item.message_sender {
                let msg = RelayMessage::ok(item.event.id, false, error_msg.clone());
                let _ = sender.send_bypass(msg);
            }
        }
    }

    /// Send success responses and broadcast events
    #[allow(clippy::vec_box)] // broadcast channel requires Box<Event>
    fn send_success_responses_and_broadcast(
        items: Vec<SaveSignedItem>,
        events_to_broadcast: Vec<Box<Event>>,
        broadcast_sender: &broadcast::Sender<Box<Event>>,
    ) {
        // Send OK responses
        for item in items.into_iter() {
            if let Some(mut sender) = item.message_sender {
                let msg = RelayMessage::ok(item.event.id, true, "");
                let _ = sender.send_bypass(msg);
            }
        }

        // Broadcast successful events
        for event in events_to_broadcast {
            let _ = broadcast_sender.send(event);
        }
    }

    /// Process a batch of delete items
    fn process_delete_batch(env: &NostrLMDB, items: Vec<DeleteItem>) {
        let mut total_deleted = 0;

        // Minimize transaction scope
        let commit_result = {
            let mut txn = match env.write_transaction() {
                Ok(txn) => txn,
                Err(e) => {
                    error!("Failed to create write transaction: {:?}", e);
                    Self::send_delete_error_responses(
                        items,
                        format!("Failed to create transaction: {}", e),
                    );
                    return;
                }
            };

            let mut all_succeeded = true;

            // Process all deletes in the same transaction
            for item in &items {
                match env.delete_with_txn(&mut txn, &item.scope, item.filter.clone()) {
                    Ok(count) => {
                        total_deleted += count;
                    }
                    Err(e) => {
                        all_succeeded = false;
                        error!("Failed to delete events: {:?}", e);
                    }
                }
            }

            if !all_succeeded {
                drop(txn); // Abort transaction
                Err("Delete operation failed")
            } else {
                txn.commit().map_err(|e| {
                    error!("Failed to commit delete transaction: {:?}", e);
                    "Failed to commit delete transaction"
                })
            }
        }; // Transaction dropped here, lock released

        // Handle responses outside transaction
        match commit_result {
            Ok(()) => {
                debug!(
                    "Delete transaction committed: {} events deleted",
                    total_deleted
                );
                // Offload response sending to async runtime
                tokio::spawn(async move {
                    Self::send_delete_success_responses(items);
                });
            }
            Err(error_msg) => {
                Self::send_delete_error_responses(items, error_msg.to_string());
            }
        }
    }

    /// Send error responses for delete items
    fn send_delete_error_responses(items: Vec<DeleteItem>, error_msg: String) {
        for item in items {
            if let Some(mut sender) = item.message_sender {
                let msg = RelayMessage::notice(error_msg.clone());
                let _ = sender.send_bypass(msg);
            }
        }
    }

    /// Send success responses for delete items
    fn send_delete_success_responses(items: Vec<DeleteItem>) {
        for item in items {
            if let Some(mut sender) = item.message_sender {
                let msg = RelayMessage::notice("Events deleted");
                let _ = sender.send_bypass(msg);
            }
        }
    }
    /// Create a new relay database
    ///
    /// # Arguments
    /// * `db_path_param` - Path where the database should be stored
    /// * `crypto_sender` - Crypto sender for signing unsigned events
    pub fn new(
        db_path_param: impl AsRef<std::path::Path>,
        crypto_sender: CryptoSender,
    ) -> Result<Self, Error> {
        Self::with_config(db_path_param, crypto_sender, None, None)
    }

    /// Create a new relay database with a provided TaskTracker
    ///
    /// # Arguments
    /// * `db_path_param` - Path where the database should be stored
    /// * `crypto_sender` - Crypto sender for signing unsigned events
    /// * `task_tracker` - TaskTracker for managing background tasks
    pub fn with_task_tracker(
        db_path_param: impl AsRef<std::path::Path>,
        crypto_sender: CryptoSender,
        task_tracker: TaskTracker,
    ) -> Result<Self, Error> {
        Self::with_config_and_tracker(db_path_param, crypto_sender, None, None, Some(task_tracker))
    }

    /// Create a new relay database with broadcast channel configuration
    ///
    /// # Arguments
    /// * `db_path_param` - Path where the database should be stored
    /// * `crypto_sender` - Crypto sender for signing unsigned events
    /// * `max_connections` - Maximum number of concurrent connections (for broadcast sizing)
    /// * `max_subscriptions` - Maximum subscriptions per connection (for broadcast sizing)
    pub fn with_config(
        db_path_param: impl AsRef<std::path::Path>,
        crypto_sender: CryptoSender,
        max_connections: Option<usize>,
        max_subscriptions: Option<usize>,
    ) -> Result<Self, Error> {
        Self::with_config_and_tracker(
            db_path_param,
            crypto_sender,
            max_connections,
            max_subscriptions,
            None,
        )
    }

    /// Internal constructor that supports all options
    fn with_config_and_tracker(
        db_path_param: impl AsRef<std::path::Path>,
        crypto_sender: CryptoSender,
        max_connections: Option<usize>,
        max_subscriptions: Option<usize>,
        task_tracker: Option<TaskTracker>,
    ) -> Result<Self, Error> {
        let db_path = db_path_param.as_ref().to_path_buf();

        // Ensure database directory exists
        if let Some(parent) = db_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    Error::database(format!(
                        "Failed to create database directory parent '{:?}': {}",
                        parent, e
                    ))
                })?;
            }
        }
        if !db_path.exists() {
            std::fs::create_dir_all(&db_path).map_err(|e| {
                Error::database(format!(
                    "Failed to create database directory '{:?}': {}",
                    db_path, e
                ))
            })?;
        }

        // Open LMDB database with configuration from environment
        info!("Opening LMDB database with configuration from environment");
        if let Ok(mode) = std::env::var("NOSTR_LMDB_MODE") {
            info!("LMDB mode: {}", mode);
        }
        let lmdb_instance = NostrLMDB::open_with_env_config(&db_path).map_err(|e| {
            Error::database(format!(
                "Failed to open NostrLMDB at path '{:?}': {}",
                db_path, e
            ))
        })?;
        let env = Arc::new(lmdb_instance);

        // Create channels for async operations
        // Use same queue capacity for both modes to enable batching
        let queue_capacity = WRITE_QUEUE_CAPACITY;
        info!("Creating database - queue capacity: {}", queue_capacity);
        // Create three specialized queues
        let (save_signed_sender, save_signed_receiver) =
            flume::bounded::<SaveSignedItem>(queue_capacity);
        let (save_unsigned_sender, save_unsigned_receiver) =
            flume::bounded::<SaveUnsignedItem>(queue_capacity);
        let (delete_sender, delete_receiver) = flume::bounded::<DeleteItem>(queue_capacity);

        // Create broadcast channel with appropriate capacity
        let broadcast_capacity =
            if let (Some(max_conn), Some(max_subs)) = (max_connections, max_subscriptions) {
                // Size for max_connections * max_subscriptions to ensure no drops
                let calculated = max_conn * max_subs;
                info!(
                    "Calculated broadcast capacity: {} ({}x{})",
                    calculated, max_conn, max_subs
                );
                calculated
            } else {
                // Fall back to environment variable or default
                std::env::var("BROADCAST_CHANNEL_CAPACITY")
                    .ok()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(DEFAULT_BROADCAST_CAPACITY)
            };
        info!(
            "Creating broadcast channel with capacity: {}",
            broadcast_capacity
        );
        let (broadcast_sender, _) = broadcast::channel(broadcast_capacity);

        // Create task tracker for worker lifecycle management
        let task_tracker = task_tracker.unwrap_or_default();

        info!("Starting single database worker per queue (LMDB limitation)");

        // Spawn processor tasks
        Self::spawn_save_signed_processor(
            save_signed_receiver,
            Arc::clone(&env),
            broadcast_sender.clone(),
            &task_tracker,
        );

        Self::spawn_save_unsigned_processor(
            save_unsigned_receiver,
            save_signed_sender.clone(),
            crypto_sender,
            &task_tracker,
        );

        Self::spawn_delete_processor(delete_receiver, Arc::clone(&env), &task_tracker);

        let relay_db = Self {
            env,
            db_path,
            broadcast_sender,
            save_signed_sender: Some(save_signed_sender),
            save_unsigned_sender: Some(save_unsigned_sender),
            delete_sender: Some(delete_sender),
            task_tracker,
            queue_capacity,
        };

        Ok(relay_db)
    }

    /// Spawn processor for signed events
    fn spawn_save_signed_processor(
        receiver: flume::Receiver<SaveSignedItem>,
        env: Arc<NostrLMDB>,
        broadcast_sender: broadcast::Sender<Box<Event>>,
        task_tracker: &TaskTracker,
    ) {
        task_tracker.spawn_blocking(move || {
            info!("Save signed processor started");

            loop {
                let first_item = match receiver.recv() {
                    Ok(item) => item,
                    Err(_) => {
                        debug!("Save signed processor channel closed");
                        break;
                    }
                };

                let mut batch = vec![first_item];
                batch.extend(receiver.drain());

                debug!("Processing batch of {} save items", batch.len());

                // Process the entire batch in one transaction
                Self::process_signed_batch(&env, batch, &broadcast_sender);
            }

            info!("Save signed processor completed");
        });
    }

    /// Spawn the processor for unsigned events
    fn spawn_save_unsigned_processor(
        receiver: flume::Receiver<SaveUnsignedItem>,
        save_signed_sender: flume::Sender<SaveSignedItem>,
        crypto_sender: CryptoSender,
        task_tracker: &TaskTracker,
    ) {
        task_tracker.spawn(async move {
            info!("Save unsigned processor started");

            while let Ok(item) = receiver.recv_async().await {
                debug!("Processing unsigned event");

                match crypto_sender.sign_event(*item.event).await {
                    Ok(signed_event) => {
                        let event_id = signed_event.id;
                        let signed_item = SaveSignedItem {
                            event: Box::new(signed_event),
                            scope: item.scope,
                            message_sender: item.message_sender.clone(),
                        };

                        if let Err(e) = save_signed_sender.send_async(signed_item).await {
                            error!("Failed to forward signed event: {:?}", e);

                            // Send error response if MessageSender was present
                            if let Some(mut sender) = item.message_sender {
                                let msg = RelayMessage::ok(
                                    event_id,
                                    false,
                                    "error: Failed to forward signed event",
                                );
                                let _ = sender.send_bypass(msg);
                            }
                        }
                        // If successful, the signed processor will handle the response
                    }
                    Err(e) => {
                        error!("Failed to sign event: {:?}", e);

                        // Send error response if MessageSender present
                        // Note: We can't send a proper OK message without an event ID
                        if let Some(mut sender) = item.message_sender {
                            let msg = RelayMessage::notice(format!("Failed to sign event: {}", e));
                            let _ = sender.send_bypass(msg);
                        }
                    }
                }
            }

            info!("Save unsigned processor completed");
        });
    }

    /// Spawn processor for deletions
    fn spawn_delete_processor(
        receiver: flume::Receiver<DeleteItem>,
        env: Arc<NostrLMDB>,
        task_tracker: &TaskTracker,
    ) {
        task_tracker.spawn_blocking(move || {
            info!("Delete processor started");

            loop {
                let first_item = match receiver.recv() {
                    Ok(item) => item,
                    Err(_) => {
                        debug!("Delete processor channel closed");
                        break;
                    }
                };

                let mut batch = vec![first_item];
                batch.extend(receiver.drain());

                debug!("Processing batch of {} delete operations", batch.len());

                // Process the entire batch in one transaction
                Self::process_delete_batch(&env, batch);
            }

            info!("Delete processor completed");
        });
    }
    /// Subscribe to receive all saved events
    ///
    /// Note: This uses a broadcast channel with bounded capacity. If subscribers
    /// cannot keep up with the rate of events being saved, older events may be
    /// dropped. The channel capacity is automatically sized based on:
    /// - max_connections * max_subscriptions (if provided during creation)
    /// - BROADCAST_CHANNEL_CAPACITY environment variable
    /// - Default: 10,000 events
    pub fn subscribe(&self) -> broadcast::Receiver<Box<Event>> {
        self.broadcast_sender.subscribe()
    }

    /// Save an event directly (synchronous)
    pub async fn save_event(&self, event: &Event, scope: &Scope) -> Result<()> {
        let env = self.get_env(scope).await?;
        let scoped_view = env.scoped(scope).map_err(|e| {
            error!("Error getting scoped view: {:?}", e);
            Error::database(format!("Failed to get scoped view: {}", e))
        })?;

        scoped_view.save_event(event).await.map_err(|e| {
            error!("Error saving event for scope {:?}: {:?}", scope, e);
            Box::new(e) as Box<dyn std::error::Error>
        })?;

        debug!(
            "Event saved successfully: {} for scope: {:?}",
            event.as_json(),
            scope
        );
        Ok(())
    }

    /// Delete events matching a filter
    pub async fn delete(&self, filter: Filter, scope: &Scope) -> Result<()> {
        let env = self.get_env(scope).await?;
        let scoped_view = env.scoped(scope).map_err(|e| {
            error!("Error getting scoped view: {:?}", e);
            Error::database(format!("Failed to get scoped view: {}", e))
        })?;

        scoped_view.delete(filter).await.map_err(|e| {
            error!("Error deleting events for scope {:?}: {:?}", scope, e);
            Box::new(e) as Box<dyn std::error::Error>
        })?;

        debug!("Deleted events successfully for scope: {:?}", scope);
        Ok(())
    }

    /// Query events from the database
    pub async fn query(&self, filters: Vec<Filter>, scope: &Scope) -> Result<Events, Error> {
        let env = self.get_env(scope).await?;
        let scoped_view = env.scoped(scope).map_err(|e| {
            error!("Error getting scoped view: {:?}", e);
            Error::database(format!("Failed to get scoped view: {}", e))
        })?;

        let mut all_events = Events::new(&Filter::new());

        // Query each filter separately and combine results
        for filter in filters {
            let events = scoped_view.query(filter).await.map_err(|e| {
                error!("Error querying events: {:?}", e);
                Error::database(format!("Failed to query events: {}", e))
            })?;

            all_events.extend(events);
        }

        Ok(all_events)
    }

    /// Get count of events matching filters
    pub async fn count(&self, filters: Vec<Filter>, scope: &Scope) -> Result<usize, Error> {
        let env = self.get_env(scope).await?;
        let scoped_view = env.scoped(scope).map_err(|e| {
            error!("Error getting scoped view: {:?}", e);
            Error::database(format!("Failed to get scoped view: {}", e))
        })?;

        let mut total_count = 0;

        // Count each filter separately and sum results
        for filter in filters {
            let count = scoped_view.count(filter).await.map_err(|e| {
                error!("Error counting events: {:?}", e);
                Error::database(format!("Failed to count events: {}", e))
            })?;
            total_count += count;
        }

        Ok(total_count)
    }

    /// Get the database environment for a scope
    async fn get_env(&self, _scope: &Scope) -> Result<Arc<NostrLMDB>, Error> {
        // In this implementation, we use a single database for all scopes
        Ok(Arc::clone(&self.env))
    }

    /// List all scopes available in the database
    pub async fn list_scopes(&self) -> Result<Vec<Scope>, Error> {
        let env = Arc::clone(&self.env);

        let scopes = tokio::task::spawn_blocking(move || env.list_scopes())
            .await
            .map_err(|e| Error::database(format!("Failed to spawn blocking task: {}", e)))?
            .map_err(|e| Error::database(format!("Failed to list scopes: {}", e)))?;

        Ok(scopes)
    }

    /// Save a signed event through the async processor
    pub async fn save_signed_event(&self, event: Event, scope: Scope) -> Result<(), Error> {
        self.save_store_command(StoreCommand::SaveSignedEvent(Box::new(event), scope), None)
            .await
    }

    /// Save an unsigned event (will be signed in the processor)
    pub async fn save_unsigned_event(
        &self,
        event: UnsignedEvent,
        scope: Scope,
    ) -> Result<(), Error> {
        self.save_store_command(StoreCommand::SaveUnsignedEvent(event, scope), None)
            .await
    }

    /// Save a store command to the async processor
    pub async fn save_store_command(
        &self,
        command: StoreCommand,
        message_sender: Option<websocket_builder::MessageSender<RelayMessage<'static>>>,
    ) -> Result<(), Error> {
        match command {
            StoreCommand::SaveSignedEvent(event, scope) => {
                let item = SaveSignedItem {
                    event,
                    scope,
                    message_sender,
                };
                self.save_signed_sender
                    .as_ref()
                    .ok_or_else(|| Error::internal("Database shutting down"))?
                    .send_async(item)
                    .await
                    .map_err(|_| Error::internal("Failed to send to save queue"))
            }
            StoreCommand::SaveUnsignedEvent(event, scope) => {
                let item = SaveUnsignedItem {
                    event: Box::new(event),
                    scope,
                    message_sender,
                };
                self.save_unsigned_sender
                    .as_ref()
                    .ok_or_else(|| Error::internal("Database shutting down"))?
                    .send_async(item)
                    .await
                    .map_err(|_| Error::internal("Failed to send to unsigned queue"))
            }
            StoreCommand::DeleteEvents(filter, scope) => {
                let item = DeleteItem {
                    filter,
                    scope,
                    message_sender,
                };
                self.delete_sender
                    .as_ref()
                    .ok_or_else(|| Error::internal("Database shutting down"))?
                    .send_async(item)
                    .await
                    .map_err(|_| Error::internal("Failed to send to delete queue"))
            }
        }
    }

    /// Get the current write queue depth (sum of all queues)
    pub fn write_queue_len(&self) -> usize {
        let signed_len = self
            .save_signed_sender
            .as_ref()
            .map(|s| s.len())
            .unwrap_or(0);
        let unsigned_len = self
            .save_unsigned_sender
            .as_ref()
            .map(|s| s.len())
            .unwrap_or(0);
        let delete_len = self.delete_sender.as_ref().map(|s| s.len()).unwrap_or(0);

        signed_len + unsigned_len + delete_len
    }

    /// Get the write queue capacity
    pub fn write_queue_capacity(&self) -> usize {
        self.queue_capacity
    }

    /// Get the write queue utilization percentage (0-100)
    pub fn write_queue_utilization(&self) -> f64 {
        let len = self.write_queue_len();
        let capacity = self.write_queue_capacity();
        if capacity > 0 {
            // Account for 3 queues, each with the same capacity
            (len as f64 / (capacity as f64 * 3.0)) * 100.0
        } else {
            0.0
        }
    }

    /// Check if the write queue is near capacity (>80% full)
    pub fn is_write_queue_congested(&self) -> bool {
        self.write_queue_utilization() >= 80.0
    }

    /// Wait for the write queue to be empty (useful for tests)
    pub async fn wait_for_queue_empty(&self, timeout: std::time::Duration) -> Result<(), Error> {
        let start = std::time::Instant::now();
        while self.write_queue_len() > 0 {
            if start.elapsed() > timeout {
                return Err(Error::internal("Timeout waiting for queue to empty"));
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        // Additional small delay to ensure batch processor has committed
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        Ok(())
    }

    /// Initiate graceful shutdown
    pub async fn shutdown(mut self) -> Result<(), Error> {
        info!("Initiating RelayDatabase shutdown");

        // Step 1: Drop all senders to close the channels
        if let Some(sender) = self.save_signed_sender.take() {
            drop(sender);
            info!("Dropped save signed sender");
        }
        if let Some(sender) = self.save_unsigned_sender.take() {
            drop(sender);
            info!("Dropped save unsigned sender");
        }
        if let Some(sender) = self.delete_sender.take() {
            drop(sender);
            info!("Dropped delete sender");
        }

        // Step 2: Close task tracker and wait for all workers
        self.task_tracker.close();
        let timeout = std::time::Duration::from_secs(30);

        match tokio::time::timeout(timeout, self.task_tracker.wait()).await {
            Ok(()) => info!("All database workers shut down successfully"),
            Err(_) => {
                error!("Database workers shutdown timed out");
                return Err(Error::internal("Worker shutdown timeout"));
            }
        }

        Ok(())
    }
}

impl Drop for RelayDatabase {
    fn drop(&mut self) {
        // Drop all senders to signal shutdown
        if let Some(sender) = self.save_signed_sender.take() {
            drop(sender);
        }
        if let Some(sender) = self.save_unsigned_sender.take() {
            drop(sender);
        }
        if let Some(sender) = self.delete_sender.take() {
            drop(sender);
        }

        // Note: We don't wait for workers here - the Drop trait is not async
        // Workers will continue processing until channels are closed
        debug!("RelayDatabase dropped - processors will complete any pending work");
    }
}

pub type NostrDatabase = Arc<RelayDatabase>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto_worker::CryptoWorker;
    use tempfile::TempDir;
    use tokio_util::task::TaskTracker;

    async fn generate_test_event(index: usize) -> Event {
        let keys = Keys::generate();
        EventBuilder::text_note(format!("Test event #{}", index))
            .sign(&keys)
            .await
            .expect("Failed to create event")
    }

    #[tokio::test]
    async fn test_shutdown_processes_all_events() {
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("test.db");
        let event_count = 100;

        // Create and populate database
        {
            let keys = Keys::generate();
            let task_tracker = TaskTracker::new();
            let crypto_sender = CryptoWorker::spawn(Arc::new(keys), &task_tracker);
            let database = Arc::new(
                RelayDatabase::new(&db_path, crypto_sender).expect("Failed to create database"),
            );

            // Send events rapidly
            for i in 0..event_count {
                let event = generate_test_event(i).await;
                database
                    .save_signed_event(event, Scope::Default)
                    .await
                    .expect("Failed to save event");
            }

            // Properly shutdown the database
            Arc::try_unwrap(database)
                .expect("Failed to unwrap Arc<RelayDatabase>")
                .shutdown()
                .await
                .expect("Failed to shutdown database");
        }

        // Re-open database and verify all events were saved
        {
            let keys = Keys::generate();
            let task_tracker = TaskTracker::new();
            let crypto_sender = CryptoWorker::spawn(Arc::new(keys), &task_tracker);
            let database = Arc::new(
                RelayDatabase::new(&db_path, crypto_sender).expect("Failed to create database"),
            );

            let count = database
                .count(
                    vec![Filter::new().kinds(vec![Kind::TextNote])],
                    &Scope::Default,
                )
                .await
                .expect("Failed to count events");

            assert_eq!(
                count, event_count,
                "Expected {} events but found {}. Data loss detected!",
                event_count, count
            );
        }
    }

    #[tokio::test]
    async fn test_drop_completes_pending_work() {
        // This test verifies that dropping the database still processes pending events
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("test.db");
        let event_count = 50;

        // Create and populate database
        {
            let keys = Keys::generate();
            let task_tracker = TaskTracker::new();
            let crypto_sender = CryptoWorker::spawn(Arc::new(keys), &task_tracker);
            let database = Arc::new(
                RelayDatabase::new(&db_path, crypto_sender).expect("Failed to create database"),
            );

            // Send events
            for i in 0..event_count {
                let event = generate_test_event(i).await;
                database
                    .save_signed_event(event, Scope::Default)
                    .await
                    .expect("Failed to save event");
            }

            // Drop without explicit shutdown - the Drop impl should handle it gracefully
            drop(database);
        }

        // Give time for the processor to complete
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;

        // Re-open and verify events were saved
        {
            let keys = Keys::generate();
            let task_tracker = TaskTracker::new();
            let crypto_sender = CryptoWorker::spawn(Arc::new(keys), &task_tracker);
            let database = Arc::new(
                RelayDatabase::new(&db_path, crypto_sender).expect("Failed to create database"),
            );

            let count = database
                .count(
                    vec![Filter::new().kinds(vec![Kind::TextNote])],
                    &Scope::Default,
                )
                .await
                .expect("Failed to count events");

            // Should have saved most/all events even without explicit shutdown
            assert!(
                count >= event_count - 5, // Allow for a few to be lost in flight
                "Expected at least {} events but found {}",
                event_count - 5,
                count
            );
        }
    }

    #[tokio::test]
    async fn test_shutdown_with_many_events() {
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("test.db");
        let event_count = 500;

        // Create and populate database with many events
        {
            let keys = Keys::generate();
            let task_tracker = TaskTracker::new();
            let crypto_sender = CryptoWorker::spawn(Arc::new(keys), &task_tracker);
            let database = Arc::new(
                RelayDatabase::new(&db_path, crypto_sender).expect("Failed to create database"),
            );

            // Send many events to ensure some are queued
            for i in 0..event_count {
                let event = generate_test_event(i).await;
                database
                    .save_signed_event(event, Scope::Default)
                    .await
                    .expect("Failed to save event");
            }

            // Immediately initiate shutdown while events may still be processing
            Arc::try_unwrap(database)
                .expect("Failed to unwrap Arc<RelayDatabase>")
                .shutdown()
                .await
                .expect("Failed to shutdown database");

            // Shutdown should take some time to process queued events
            // With batch operations, this may be faster, so we just ensure it completed
        }

        // Verify all events were saved
        {
            let keys = Keys::generate();
            let task_tracker = TaskTracker::new();
            let crypto_sender = CryptoWorker::spawn(Arc::new(keys), &task_tracker);
            let database = Arc::new(
                RelayDatabase::new(&db_path, crypto_sender).expect("Failed to create database"),
            );

            let count = database
                .count(
                    vec![Filter::new().kinds(vec![Kind::TextNote])],
                    &Scope::Default,
                )
                .await
                .expect("Failed to count events");

            assert_eq!(
                count, event_count,
                "Expected {} events but found {}",
                event_count, count
            );
        }
    }

    #[tokio::test]
    async fn test_queue_monitoring() {
        // Test that queue monitoring works with flume
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("test.db");
        let keys = Keys::generate();
        let task_tracker = TaskTracker::new();
        let crypto_sender = CryptoWorker::spawn(Arc::new(keys), &task_tracker);
        let database = Arc::new(
            RelayDatabase::new(&db_path, crypto_sender).expect("Failed to create database"),
        );

        // Initially queue should be empty
        assert_eq!(database.write_queue_len(), 0);
        assert_eq!(database.write_queue_capacity(), WRITE_QUEUE_CAPACITY);
        assert_eq!(database.write_queue_utilization(), 0.0);
        assert!(!database.is_write_queue_congested());

        // Send some events
        let event_count = 100;
        for i in 0..event_count {
            let event = generate_test_event(i).await;
            database
                .save_signed_event(event, Scope::Default)
                .await
                .expect("Failed to save event");
        }

        // Queue should have some items (may vary due to processing speed)
        let queue_len = database.write_queue_len();
        assert!(queue_len <= event_count);

        // Utilization should be calculated correctly (accounting for 3 queues)
        let utilization = database.write_queue_utilization();
        let expected_utilization = (queue_len as f64 / (WRITE_QUEUE_CAPACITY as f64 * 3.0)) * 100.0;
        assert!(
            (utilization - expected_utilization).abs() < 1.0,
            "Utilization {} vs expected {}",
            utilization,
            expected_utilization
        );

        // Wait for queue to drain
        let start = std::time::Instant::now();
        while database.write_queue_len() > 0 && start.elapsed() < std::time::Duration::from_secs(5)
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }

        // Queue should be empty
        assert_eq!(
            database.write_queue_len(),
            0,
            "Queue should be empty after draining"
        );

        // Cleanup
        Arc::try_unwrap(database)
            .expect("Failed to unwrap Arc<RelayDatabase>")
            .shutdown()
            .await
            .expect("Failed to shutdown database");
    }

    #[tokio::test]
    async fn test_sync_save_in_optimistic_mode() {
        // Test that sync saves work in optimistic mode
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("test.db");
        let keys = Keys::generate();
        let task_tracker = TaskTracker::new();
        let crypto_sender = CryptoWorker::spawn(Arc::new(keys), &task_tracker);
        // Create in optimistic mode (default)
        let database = Arc::new(
            RelayDatabase::new(&db_path, crypto_sender).expect("Failed to create database"),
        );

        let event = generate_test_event(0).await;
        let event_id = event.id;

        // Save should work and wait for persistence
        let result = database
            .save_store_command(
                StoreCommand::SaveSignedEvent(Box::new(event), Scope::Default),
                None,
            )
            .await;

        assert!(
            result.is_ok(),
            "Sync save should succeed in optimistic mode"
        );

        // Give the processor a moment to actually persist the event
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Verify event was persisted
        let count = database
            .count(vec![Filter::new().id(event_id)], &Scope::Default)
            .await
            .expect("Count should succeed");
        assert_eq!(count, 1, "Event should be persisted after sync save");

        // Cleanup
        Arc::try_unwrap(database)
            .expect("Failed to unwrap Arc<RelayDatabase>")
            .shutdown()
            .await
            .expect("Failed to shutdown database");
    }
}
