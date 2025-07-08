//! Database abstraction for Nostr relays

use crate::crypto_helper::CryptoHelper;
use crate::error::Error;
use crate::subscription_coordinator::{ResponseHandler, StoreCommand};
use flume;
use nostr_database::nostr::{Event, Filter};
use nostr_database::Events;
use nostr_lmdb::{NostrLMDB, Scope};
use nostr_sdk::prelude::*;
use std::iter;
use std::sync::Arc;
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;
use tracing::{debug, error, info, warn};

/// Default maximum number of events that can be queued for writing.
/// This provides backpressure to prevent unbounded memory growth.
/// When the queue is full, writers will wait until space is available.
/// Can be overridden by passing max_connections and max_subscriptions to calculate dynamically.
const DEFAULT_WRITE_QUEUE_CAPACITY: usize = 100_000;

/// A cloneable sender for database commands with direct routing.
///
/// This wrapper provides a clean API for sending commands to the database
/// and routes commands directly to the appropriate processor.
#[derive(Clone)]
pub struct DatabaseSender {
    /// Channel for unsigned events that need signing
    unsigned_sender: flume::Sender<StoreCommand>,
    /// Unified channel for signed saves and deletes (both are LMDB write operations)
    store_sender: flume::Sender<StoreCommand>,
}

impl std::fmt::Debug for DatabaseSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DatabaseSender").finish()
    }
}

impl Drop for DatabaseSender {
    fn drop(&mut self) {
        debug!("DatabaseSender dropped");
    }
}

impl DatabaseSender {
    /// Create a new DatabaseSender with the two internal channels
    fn new(
        unsigned_sender: flume::Sender<StoreCommand>,
        store_sender: flume::Sender<StoreCommand>,
    ) -> Self {
        Self {
            unsigned_sender,
            store_sender,
        }
    }

    /// Send a store command to the database
    pub async fn send(&self, command: StoreCommand) -> Result<(), Error> {
        self.send_with_sender(command, None).await
    }

    /// Send a store command with an optional message sender for responses (fire-and-forget)
    pub async fn send_with_sender(
        &self,
        mut command: StoreCommand,
        message_sender: Option<ResponseHandler>,
    ) -> Result<(), Error> {
        // Set the response handler in the command
        match &mut command {
            StoreCommand::SaveUnsignedEvent(_, _, ref mut handler) => {
                *handler = message_sender;
            }
            StoreCommand::SaveSignedEvent(_, _, ref mut handler) => {
                *handler = message_sender;
            }
            StoreCommand::DeleteEvents(_, _, ref mut handler) => {
                *handler = message_sender;
            }
        }

        // Route to appropriate channel
        match &command {
            StoreCommand::SaveUnsignedEvent(_, _, _) => self
                .unsigned_sender
                .send_async(command)
                .await
                .map_err(|_| Error::internal("Unsigned event processor shut down")),
            StoreCommand::SaveSignedEvent(_, _, _) | StoreCommand::DeleteEvents(_, _, _) => self
                .store_sender
                .send_async(command)
                .await
                .map_err(|_| Error::internal("Store processor shut down")),
        }
    }

    /// Send a store command synchronously with immediate acknowledgment
    pub async fn send_sync(&self, mut command: StoreCommand) -> Result<(), Error> {
        let (tx, rx) = oneshot::channel();

        // Set the response handler in the command
        match &mut command {
            StoreCommand::SaveUnsignedEvent(_, _, ref mut handler) => {
                *handler = Some(ResponseHandler::Oneshot(tx));
            }
            StoreCommand::SaveSignedEvent(_, _, ref mut handler) => {
                *handler = Some(ResponseHandler::Oneshot(tx));
            }
            StoreCommand::DeleteEvents(_, _, ref mut handler) => {
                *handler = Some(ResponseHandler::Oneshot(tx));
            }
        }

        // Route to appropriate channel
        match &command {
            StoreCommand::SaveUnsignedEvent(_, _, _) => {
                self.unsigned_sender
                    .send_async(command)
                    .await
                    .map_err(|_| Error::internal("Unsigned event processor shut down"))?;
            }
            StoreCommand::SaveSignedEvent(_, _, _) | StoreCommand::DeleteEvents(_, _, _) => {
                self.store_sender
                    .send_async(command)
                    .await
                    .map_err(|_| Error::internal("Store processor shut down"))?;
            }
        }

        rx.await
            .map_err(|_| Error::internal("Database response channel closed"))?
    }

    /// Save a signed event to the database (fire-and-forget)
    pub async fn save_signed_event(&self, event: Event, scope: Scope) -> Result<(), Error> {
        self.send(StoreCommand::SaveSignedEvent(Box::new(event), scope, None))
            .await
    }

    /// Save a signed event to the database with immediate acknowledgment (for tests)
    pub async fn save_signed_event_sync(&self, event: Event, scope: Scope) -> Result<(), Error> {
        self.send_sync(StoreCommand::SaveSignedEvent(Box::new(event), scope, None))
            .await
    }

    /// Save an unsigned event to the database
    pub async fn save_unsigned_event(
        &self,
        event: UnsignedEvent,
        scope: Scope,
    ) -> Result<(), Error> {
        self.send(StoreCommand::SaveUnsignedEvent(event, scope, None))
            .await
    }

    /// Delete events matching the filter from the database
    pub async fn delete_events(&self, filter: Filter, scope: Scope) -> Result<(), Error> {
        self.send(StoreCommand::DeleteEvents(filter, scope, None))
            .await
    }
}

// Note: LMDB only allows one write transaction at a time, so we use a single worker

/// A Nostr relay database that wraps NostrLMDB with async operations
#[derive(Debug)]
pub struct RelayDatabase {
    lmdb: Arc<NostrLMDB>,
    /// Optional event sender for distribution
    #[allow(dead_code)] // Used in send_success_responses_and_distribute
    event_sender: Option<flume::Sender<Arc<Event>>>,

    /// Queue capacity used for this instance
    queue_capacity: usize,
}

impl RelayDatabase {
    /// Process a unified batch of store items (saves and deletes) in a single transaction
    fn process_store_batch(
        env: &Arc<NostrLMDB>,
        items: impl Iterator<Item = StoreCommand>,
        event_sender: Option<&flume::Sender<Arc<Event>>>,
    ) {
        // Separate saves and deletes
        let mut save_commands = Vec::new();
        let mut delete_commands = Vec::new();

        for command in items {
            match command {
                StoreCommand::SaveSignedEvent(_, _, _) => save_commands.push(command),
                StoreCommand::DeleteEvents(_, _, _) => delete_commands.push(command),
                StoreCommand::SaveUnsignedEvent(_, _, _) => {
                    error!("Unsigned event should not reach store processor");
                }
            }
        }

        // Pre-collect event references for broadcasting (avoiding clone)
        let mut events_to_broadcast = Vec::new();

        // Process everything in a single transaction
        let commit_result = {
            let mut txn = match env.write_transaction() {
                Ok(txn) => txn,
                Err(e) => {
                    error!("Failed to create write transaction: {:?}", e);
                    Self::send_error_responses(save_commands, format!("error: {e}"));
                    Self::send_delete_error_responses(delete_commands, format!("error: {e}"));
                    return;
                }
            };

            let mut all_succeeded = true;

            // Process saves first
            for command in save_commands.iter() {
                if let StoreCommand::SaveSignedEvent(event, scope, _) = command {
                    match env.save_event_with_txn(&mut txn, scope, event) {
                        Ok(status) => {
                            if status.is_success() {
                                // Collect event for broadcasting
                                events_to_broadcast.push((**event).clone());
                            } else {
                                // Rejected is not an error, we just log it
                                warn!("Failed to save event: status={:?}", status);
                            }
                        }
                        Err(e) => {
                            all_succeeded = false;
                            error!("Failed to save event: {:?}", e);
                            break; // Stop on first failure
                        }
                    }
                }
            }

            // Process deletes if saves succeeded
            if all_succeeded {
                for command in delete_commands.iter() {
                    if let StoreCommand::DeleteEvents(filter, scope, _) = command {
                        match env.delete_with_txn(&mut txn, scope, filter.clone()) {
                            Ok(_) => {
                                debug!(
                                    "Successfully deleted events matching filter for scope {:?}",
                                    scope
                                );
                            }
                            Err(e) => {
                                all_succeeded = false;
                                error!("Failed to delete events: {:?}", e);
                                break;
                            }
                        }
                    }
                }
            }

            if !all_succeeded {
                drop(txn); // Abort transaction
                Err("batch operation failed")
            } else {
                txn.commit().map_err(|e| {
                    error!("Failed to commit transaction: {:?}", e);
                    "transaction commit failed"
                })
            }
        }; // Transaction dropped here, lock released

        // Handle responses outside transaction
        match commit_result {
            Ok(()) => {
                debug!(
                    "Transaction committed successfully for {} saves and {} deletes",
                    save_commands.len(),
                    delete_commands.len()
                );

                // Send success responses for saves and distribute events
                Self::send_success_responses_and_distribute_sync(
                    save_commands,
                    events_to_broadcast,
                    event_sender,
                );

                // Send success responses for deletes
                Self::send_delete_success_responses(delete_commands);
            }
            Err(error_msg) => {
                // All failed - send error responses
                Self::send_error_responses(save_commands, format!("error: {error_msg}"));
                Self::send_delete_error_responses(delete_commands, format!("error: {error_msg}"));
            }
        }
    }

    /// Send error responses for save items
    fn send_error_responses(commands: Vec<StoreCommand>, error_msg: String) {
        for command in commands {
            if let StoreCommand::SaveSignedEvent(event, _, Some(handler)) = command {
                match handler {
                    ResponseHandler::MessageSender(mut sender) => {
                        let msg = RelayMessage::ok(event.id, false, error_msg.clone());
                        sender.send_bypass(msg);
                    }
                    ResponseHandler::Oneshot(oneshot_sender) => {
                        let _ = oneshot_sender.send(Err(Error::internal(error_msg.clone())));
                    }
                }
            }
        }
    }

    /// Send success responses and distribute events synchronously
    fn send_success_responses_and_distribute_sync(
        commands: Vec<StoreCommand>,
        events_to_distribute: Vec<Event>,
        event_sender: Option<&flume::Sender<Arc<Event>>>,
    ) {
        // Send OK responses
        for command in commands {
            if let StoreCommand::SaveSignedEvent(event, _, Some(handler)) = command {
                match handler {
                    ResponseHandler::MessageSender(mut sender) => {
                        let msg = RelayMessage::ok(event.id, true, "");
                        sender.send_bypass(msg);
                    }
                    ResponseHandler::Oneshot(oneshot_sender) => {
                        let _ = oneshot_sender.send(Ok(()));
                    }
                }
            }
        }

        // Distribute successful events
        if let Some(event_sender) = event_sender {
            for event in events_to_distribute {
                // Convert Event to Arc<Event>
                let arc_event = Arc::new(event);
                // Use try_send for non-blocking send
                if let Err(e) = event_sender.try_send(arc_event) {
                    match e {
                        flume::TrySendError::Full(_) => {
                            error!("Event distribution channel is full - applying backpressure");
                            // Could implement blocking here if needed
                        }
                        flume::TrySendError::Disconnected(_) => {
                            error!("Event distribution channel closed");
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Send success responses for delete items
    fn send_delete_success_responses(commands: Vec<StoreCommand>) {
        for command in commands {
            if let StoreCommand::DeleteEvents(_, _, Some(handler)) = command {
                match handler {
                    ResponseHandler::MessageSender(mut sender) => {
                        let msg = RelayMessage::notice("delete: success");
                        sender.send_bypass(msg);
                    }
                    ResponseHandler::Oneshot(oneshot_sender) => {
                        let _ = oneshot_sender.send(Ok(()));
                    }
                }
            }
        }
    }

    /// Send error responses for delete items
    fn send_delete_error_responses(commands: Vec<StoreCommand>, error_msg: String) {
        for command in commands {
            if let StoreCommand::DeleteEvents(_, _, Some(handler)) = command {
                match handler {
                    ResponseHandler::MessageSender(mut sender) => {
                        let msg = RelayMessage::notice(error_msg.clone());
                        sender.send_bypass(msg);
                    }
                    ResponseHandler::Oneshot(oneshot_sender) => {
                        let _ = oneshot_sender.send(Err(Error::internal(error_msg.clone())));
                    }
                }
            }
        }
    }

    /// Create a new relay database
    ///
    /// # Arguments
    /// * `db_path_param` - Path where the database should be stored
    /// * `keys` - Keys for signing unsigned events
    pub fn new(
        db_path_param: impl AsRef<std::path::Path>,
        keys: Arc<Keys>,
    ) -> Result<(Self, DatabaseSender), Error> {
        let crypto_helper = CryptoHelper::new(keys);
        Self::with_config_and_tracker(db_path_param, crypto_helper, None, None, None, None, None)
    }

    /// Create a new relay database with a provided TaskTracker
    ///
    /// # Arguments
    /// * `db_path_param` - Path where the database should be stored
    /// * `keys` - Keys for signing unsigned events
    /// * `task_tracker` - TaskTracker for managing background tasks
    pub fn with_task_tracker(
        db_path_param: impl AsRef<std::path::Path>,
        keys: Arc<Keys>,
        task_tracker: TaskTracker,
    ) -> Result<(Self, DatabaseSender), Error> {
        let crypto_helper = CryptoHelper::new(keys);
        Self::with_config_and_tracker(
            db_path_param,
            crypto_helper,
            None,
            None,
            Some(task_tracker),
            None,
            None,
        )
    }

    /// Create a new relay database with a provided TaskTracker and CancellationToken
    ///
    /// # Arguments
    /// * `db_path_param` - Path where the database should be stored
    /// * `keys` - Keys for signing unsigned events
    /// * `task_tracker` - TaskTracker for managing background tasks
    /// * `cancellation_token` - CancellationToken for graceful shutdown
    pub fn with_task_tracker_and_token(
        db_path_param: impl AsRef<std::path::Path>,
        keys: Arc<Keys>,
        task_tracker: TaskTracker,
        cancellation_token: CancellationToken,
    ) -> Result<(Self, DatabaseSender), Error> {
        let crypto_helper = CryptoHelper::new(keys);
        Self::with_config_and_tracker(
            db_path_param,
            crypto_helper,
            None,
            None,
            Some(task_tracker),
            Some(cancellation_token),
            None,
        )
    }

    /// Create a new relay database with broadcast channel configuration
    ///
    /// # Arguments
    /// * `db_path_param` - Path where the database should be stored
    /// * `keys` - Keys for signing unsigned events
    /// * `max_connections` - Maximum number of concurrent connections (for broadcast sizing)
    /// * `max_subscriptions` - Maximum subscriptions per connection (for broadcast sizing)
    pub fn with_config(
        db_path_param: impl AsRef<std::path::Path>,
        keys: Arc<Keys>,
        max_connections: Option<usize>,
        max_subscriptions: Option<usize>,
    ) -> Result<(Self, DatabaseSender), Error> {
        let crypto_helper = CryptoHelper::new(keys);
        Self::with_config_and_tracker(
            db_path_param,
            crypto_helper,
            max_connections,
            max_subscriptions,
            None,
            None,
            None,
        )
    }

    /// Create a new relay database with event sender
    pub fn with_event_sender(
        db_path_param: impl AsRef<std::path::Path>,
        keys: Arc<Keys>,
        event_sender: flume::Sender<Arc<Event>>,
        task_tracker: Option<TaskTracker>,
        cancellation_token: Option<CancellationToken>,
    ) -> Result<(Self, DatabaseSender), Error> {
        let crypto_helper = CryptoHelper::new(keys);
        Self::with_config_and_tracker(
            db_path_param,
            crypto_helper,
            None,
            None,
            task_tracker,
            cancellation_token,
            Some(event_sender),
        )
    }

    /// Internal constructor that supports all options
    fn with_config_and_tracker(
        db_path_param: impl AsRef<std::path::Path>,
        crypto_helper: CryptoHelper,
        max_connections: Option<usize>,
        max_subscriptions: Option<usize>,
        task_tracker: Option<TaskTracker>,
        cancellation_token: Option<CancellationToken>,
        event_sender: Option<flume::Sender<Arc<Event>>>,
    ) -> Result<(Self, DatabaseSender), Error> {
        let db_path = db_path_param.as_ref().to_path_buf();

        // Ensure database directory exists
        if let Some(parent) = db_path.parent() {
            if !parent.exists() {
                std::fs::create_dir_all(parent).map_err(|e| {
                    Error::database(format!(
                        "Failed to create database directory parent '{parent:?}': {e}"
                    ))
                })?;
            }
        }
        if !db_path.exists() {
            std::fs::create_dir_all(&db_path).map_err(|e| {
                Error::database(format!(
                    "Failed to create database directory '{db_path:?}': {e}"
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
                "Failed to open NostrLMDB at path '{db_path:?}': {e}"
            ))
        })?;
        let lmdb = Arc::new(lmdb_instance);

        // Create channels for async operations
        // Calculate queue capacity based on configuration or use default
        let queue_capacity = match (max_connections, max_subscriptions) {
            (Some(connections), Some(subscriptions)) => {
                // Formula: max_connections * max_subscriptions * 10
                // This allows each subscription to send 10 events without backpressure
                let calculated = connections * subscriptions * 10;
                info!(
                    "Calculated queue capacity: {} ({}x{}x10)",
                    calculated, connections, subscriptions
                );
                calculated
            }
            _ => {
                info!(
                    "Using default queue capacity: {}",
                    DEFAULT_WRITE_QUEUE_CAPACITY
                );
                DEFAULT_WRITE_QUEUE_CAPACITY
            }
        };

        // Create two specialized queues: one for unsigned events, one for store operations
        let (save_unsigned_sender, save_unsigned_receiver) =
            flume::bounded::<StoreCommand>(queue_capacity);
        let (store_sender, store_receiver) = flume::bounded::<StoreCommand>(queue_capacity);

        // Create task tracker for worker lifecycle management
        let task_tracker = task_tracker.unwrap_or_default();

        info!("Starting database workers (2 total: unsigned processor + unified store processor)");

        // Spawn processor tasks
        Self::spawn_save_unsigned_processor(
            save_unsigned_receiver,
            store_sender.clone(),
            crypto_helper,
            &task_tracker,
        );

        Self::spawn_store_processor(
            store_receiver,
            Arc::clone(&lmdb),
            event_sender.clone(),
            &task_tracker,
            cancellation_token,
        );

        let relay_db = Self {
            lmdb,
            event_sender,
            queue_capacity,
        };

        // Create the DatabaseSender for external use
        let db_sender = DatabaseSender::new(save_unsigned_sender, store_sender);

        Ok((relay_db, db_sender))
    }

    /// Spawn unified store processor that handles both saves and deletes
    fn spawn_store_processor(
        receiver: flume::Receiver<StoreCommand>,
        env: Arc<NostrLMDB>,
        event_sender: Option<flume::Sender<Arc<Event>>>,
        task_tracker: &TaskTracker,
        cancellation_token: Option<CancellationToken>,
    ) {
        // Use a simple tokio::spawn loop instead of batching runtime
        task_tracker.spawn_blocking(move || {
            info!("Store processor started on tokio runtime");

            let cancellation_token = cancellation_token.unwrap_or_default();

            loop {
                // // Check for cancellation
                // if cancellation_token.is_cancelled() {
                //     info!("Store processor shutting down due to cancellation");
                //     break;
                // }

                // Wait for at least one item asynchronously
                let first_item = match receiver.recv() {
                    Ok(item) => item,
                    Err(_) => {
                        info!("Store processor channel closed - all senders dropped");
                        break;
                    }
                };

                let batch = iter::once(first_item).chain(receiver.drain());

                Self::process_store_batch(&env, batch, event_sender.as_ref());
            }

            info!("Store processor completed");
            cancellation_token.cancel();
        });
    }

    /// Spawn the processor for unsigned events
    fn spawn_save_unsigned_processor(
        receiver: flume::Receiver<StoreCommand>,
        store_sender: flume::Sender<StoreCommand>,
        crypto_helper: CryptoHelper,
        task_tracker: &TaskTracker,
    ) {
        task_tracker.spawn(async move {
            info!("Save unsigned processor started");

            while let Ok(command) = receiver.recv_async().await {
                if let StoreCommand::SaveUnsignedEvent(event, scope, response_handler) = command {
                    debug!("Processing unsigned event");

                    match crypto_helper.sign_event(event).await {
                        Ok(signed_event) => {
                            let signed_command = StoreCommand::SaveSignedEvent(
                                Box::new(signed_event),
                                scope,
                                response_handler,
                            );

                            if let Err(e) = store_sender.send_async(signed_command).await {
                                error!("Failed to forward signed event to store: {:?}", e);
                                // The response_handler was already moved into signed_command
                                // So we can't send an error response here - the channel is closed anyway
                            }
                            // If successful, the store processor will handle the response
                        }
                        Err(e) => {
                            error!("Failed to sign event: {:?}", e);

                            // Send error response if response handler present
                            // Note: We can't send a proper OK message without an event ID
                            if let Some(response_handler) = response_handler {
                                match response_handler {
                                    ResponseHandler::MessageSender(mut sender) => {
                                        let msg = RelayMessage::notice(format!(
                                            "Failed to sign event: {e}"
                                        ));
                                        sender.send_bypass(msg);
                                    }
                                    ResponseHandler::Oneshot(oneshot_sender) => {
                                        let _ = oneshot_sender.send(Err(Error::internal(format!(
                                            "Failed to sign event: {e}"
                                        ))));
                                    }
                                }
                            }
                        }
                    }
                } else {
                    error!("Non-unsigned event received in unsigned processor");
                }
            }

            info!("Save unsigned processor completed");
        });
    }

    /// Subscribe to receive all saved events
    ///
    /// Note: This uses a broadcast channel with bounded capacity. If subscribers
    /// cannot keep up with the rate of events being saved, older events may be
    /// dropped. The channel capacity is automatically sized based on:
    /// - max_connections * max_subscriptions (if provided during creation)
    ///
    /// Save an event directly (synchronous)
    pub async fn save_event(&self, event: &Event, scope: &Scope) -> Result<()> {
        let env = self.get_env(scope).await?;
        let scoped_view = env.scoped(scope).map_err(|e| {
            error!("Error getting scoped view: {:?}", e);
            Error::database(format!("Failed to get scoped view: {e}"))
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
            Error::database(format!("Failed to get scoped view: {e}"))
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
            Error::database(format!("Failed to get scoped view: {e}"))
        })?;

        let mut all_events = Events::new(&Filter::new());

        // Query each filter separately and combine results
        for filter in filters {
            let events = scoped_view.query(filter).await.map_err(|e| {
                error!("Error querying events: {:?}", e);
                Error::database(format!("Failed to query events: {e}"))
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
            Error::database(format!("Failed to get scoped view: {e}"))
        })?;

        let mut total_count = 0;

        // Count each filter separately and sum results
        for filter in filters {
            let count = scoped_view.count(filter).await.map_err(|e| {
                error!("Error counting events: {:?}", e);
                Error::database(format!("Failed to count events: {e}"))
            })?;
            total_count += count;
        }

        Ok(total_count)
    }

    /// Get the database environment for a scope
    async fn get_env(&self, _scope: &Scope) -> Result<Arc<NostrLMDB>, Error> {
        // In this implementation, we use a single database for all scopes
        Ok(Arc::clone(&self.lmdb))
    }

    /// List all scopes available in the database
    pub async fn list_scopes(&self) -> Result<Vec<Scope>, Error> {
        let env = Arc::clone(&self.lmdb);

        let scopes = tokio::task::spawn_blocking(move || env.list_scopes())
            .await
            .map_err(|e| Error::database(format!("Failed to spawn blocking task: {e}")))?
            .map_err(|e| Error::database(format!("Failed to list scopes: {e}")))?;

        Ok(scopes)
    }

    /// Get the write queue capacity
    pub fn write_queue_capacity(&self) -> usize {
        self.queue_capacity
    }
}

pub type NostrDatabase = Arc<RelayDatabase>;

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;
    use tokio_util::task::TaskTracker;

    async fn generate_test_event(index: usize) -> Event {
        let keys = Keys::generate();
        EventBuilder::text_note(format!("Test event #{index}"))
            .sign(&keys)
            .await
            .expect("Failed to create event")
    }

    #[tokio::test]
    async fn test_shutdown_processes_all_events() {
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("test_shutdown_processes.db");
        let event_count = 100;

        // Create and populate database
        {
            let keys = Keys::generate();
            let task_tracker = TaskTracker::new();
            let keys_arc = Arc::new(keys);
            let (_database, db_sender) =
                RelayDatabase::with_task_tracker(&db_path, keys_arc.clone(), task_tracker.clone())
                    .expect("Failed to create database");

            // Send events rapidly
            for i in 0..event_count {
                let event = generate_test_event(i).await;
                db_sender
                    .save_signed_event(event, Scope::Default)
                    .await
                    .expect("Failed to save event");
            }

            // Properly shutdown the database
            drop(db_sender);

            // Close and wait for task tracker
            task_tracker.close();
            task_tracker.wait().await;
        }

        // Give LMDB time to fully release resources
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Re-open database and verify all events were saved
        {
            let keys = Keys::generate();
            let keys_arc = Arc::new(keys);
            let (database, _db_sender) =
                RelayDatabase::new(&db_path, keys_arc.clone()).expect("Failed to create database");
            let database = Arc::new(database);

            let count = database
                .count(
                    vec![Filter::new().kinds(vec![Kind::TextNote])],
                    &Scope::Default,
                )
                .await
                .expect("Failed to count events");

            assert_eq!(
                count, event_count,
                "Expected {event_count} events but found {count}. Data loss detected!"
            );
        }
    }

    #[tokio::test]
    async fn test_drop_completes_pending_work() {
        // This test verifies that dropping the database still processes pending events
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("test_drop_completes.db");
        let event_count = 50;

        // Create a task tracker that we'll wait on
        let task_tracker = TaskTracker::new();

        // Create and populate database
        {
            let keys = Keys::generate();
            let keys_arc = Arc::new(keys);
            let (database, db_sender) =
                RelayDatabase::with_task_tracker(&db_path, keys_arc.clone(), task_tracker.clone())
                    .expect("Failed to create database");
            let database = Arc::new(database);

            // Send events
            for i in 0..event_count {
                let event = generate_test_event(i).await;
                db_sender
                    .save_signed_event(event, Scope::Default)
                    .await
                    .expect("Failed to save event");
            }

            // Drop without explicit shutdown - the Drop impl should handle it gracefully
            drop(db_sender);
            drop(database);
        }

        // Close the tracker and wait for all tasks to complete
        task_tracker.close();
        task_tracker.wait().await;

        // Give LMDB time to fully release resources
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Re-open and verify events were saved
        {
            let keys = Keys::generate();
            let keys_arc = Arc::new(keys);
            let (database, _db_sender) =
                RelayDatabase::new(&db_path, keys_arc.clone()).expect("Failed to create database");
            let database = Arc::new(database);

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
        let db_path = tmp_dir.path().join("test_shutdown_many.db");
        let event_count = 500;

        // Create and populate database with many events
        {
            let keys = Keys::generate();
            let task_tracker = TaskTracker::new();
            let keys_arc = Arc::new(keys);
            let (_database, db_sender) =
                RelayDatabase::with_task_tracker(&db_path, keys_arc.clone(), task_tracker.clone())
                    .expect("Failed to create database");

            // Send many events to ensure some are queued
            for i in 0..event_count {
                let event = generate_test_event(i).await;
                db_sender
                    .save_signed_event(event, Scope::Default)
                    .await
                    .expect("Failed to save event");
            }

            // Drop sender first
            drop(db_sender);

            // Shutdown should take some time to process queued events
            // With batch operations, this may be faster, so we just ensure it completed

            // Close and wait for task tracker
            task_tracker.close();
            task_tracker.wait().await;
        }

        // Small delay to ensure LMDB environment is fully closed
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Verify all events were saved
        {
            let keys = Keys::generate();
            let keys_arc = Arc::new(keys);
            let (database, _db_sender) =
                RelayDatabase::new(&db_path, keys_arc.clone()).expect("Failed to create database");
            let database = Arc::new(database);

            let count = database
                .count(
                    vec![Filter::new().kinds(vec![Kind::TextNote])],
                    &Scope::Default,
                )
                .await
                .expect("Failed to count events");

            assert_eq!(
                count, event_count,
                "Expected {event_count} events but found {count}"
            );
        }
    }

    #[tokio::test]
    async fn test_batch_save_handles_rejected_events() {
        // This test verifies that batch saves continue processing even when some events are rejected
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("test_batch_save.db");

        // Create database
        let keys = Keys::generate();
        let task_tracker = TaskTracker::new();
        let keys_arc = Arc::new(keys);
        let (database, db_sender) =
            RelayDatabase::with_task_tracker(&db_path, keys_arc.clone(), task_tracker.clone())
                .expect("Failed to create database");
        let database = Arc::new(database);

        // Create a unique event that we'll save twice to trigger rejection
        let duplicate_event = generate_test_event(999).await;

        // First, save the duplicate event once
        db_sender
            .save_signed_event(duplicate_event.clone(), Scope::Default)
            .await
            .expect("Failed to save initial event");

        // Wait a bit to ensure it's processed
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;

        // Now send a batch with:
        // - Some new events (should succeed)
        // - The duplicate event (should be rejected)
        // - More new events (should still succeed despite the rejection)
        let batch_size = 10;
        for i in 0..batch_size {
            if i == 5 {
                // Insert the duplicate in the middle of the batch
                db_sender
                    .save_signed_event(duplicate_event.clone(), Scope::Default)
                    .await
                    .expect("Failed to queue duplicate event");
            } else {
                let event = generate_test_event(i).await;
                db_sender
                    .save_signed_event(event, Scope::Default)
                    .await
                    .expect("Failed to queue event");
            }
        }

        // Wait for processing before checking results
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;

        // Verify the results using the existing database instance
        let count = database
            .count(
                vec![Filter::new().kinds(vec![Kind::TextNote])],
                &Scope::Default,
            )
            .await
            .expect("Failed to count events");

        // We should have:
        // - 1 event from the initial save (event 999)
        // - 9 events from the batch (0-4, 6-9, but not the duplicate at position 5)
        // Total: 10 events
        assert_eq!(
            count, 10,
            "Expected 10 unique events (1 initial + 9 new), but found {count}"
        );

        // Verify the duplicate event exists (only one copy)
        let duplicate_filter = vec![Filter::new().id(duplicate_event.id)];
        let duplicate_count = database
            .count(duplicate_filter, &Scope::Default)
            .await
            .expect("Failed to count duplicate event");

        assert_eq!(
            duplicate_count, 1,
            "Expected exactly 1 copy of the duplicate event, but found {duplicate_count}"
        );

        // Cleanup
        drop(db_sender);
        task_tracker.close();
        task_tracker.wait().await;
    }

    #[tokio::test]
    async fn test_sync_save_in_optimistic_mode() {
        // Test that sync saves work in optimistic mode
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("test_sync_save.db");
        let keys = Keys::generate();
        let task_tracker = TaskTracker::new();
        let keys_arc = Arc::new(keys);
        // Create in optimistic mode (default)
        let (database, db_sender) =
            RelayDatabase::new(&db_path, keys_arc.clone()).expect("Failed to create database");
        let database = Arc::new(database);

        let event = generate_test_event(0).await;
        let event_id = event.id;

        // Save should work and wait for persistence
        let result = db_sender
            .save_signed_event_sync(event, Scope::Default)
            .await;

        assert!(
            result.is_ok(),
            "Sync save should succeed in optimistic mode"
        );

        // Verify event was persisted
        let count = database
            .count(vec![Filter::new().id(event_id)], &Scope::Default)
            .await
            .expect("Count should succeed");
        assert_eq!(count, 1, "Event should be persisted after sync save");

        // Cleanup
        drop(db_sender);

        // Close and wait for task tracker
        task_tracker.close();
        task_tracker.wait().await;
    }
}
