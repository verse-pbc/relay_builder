//! Database abstraction for Nostr relays

use crate::error::Error;
use nostr_database::Events;
use nostr_lmdb::{NostrLMDB, Scope};
use nostr_sdk::prelude::*;
use std::sync::Arc;
use tracing::{debug, error, info};

/// A Nostr relay database that wraps NostrLMDB with async operations
#[derive(Debug, Clone)]
pub struct RelayDatabase {
    lmdb: Arc<NostrLMDB>,
}

impl RelayDatabase {
    /// Create a new relay database
    ///
    /// # Arguments
    /// * `db_path_param` - Path where the database should be stored
    pub fn new(db_path_param: impl AsRef<std::path::Path>) -> Result<Self, Error> {
        Self::with_config(db_path_param, None, None)
    }

    /// Create a new relay database with specified max_readers
    ///
    /// # Arguments
    /// * `db_path_param` - Path where the database should be stored
    /// * `max_readers` - Maximum number of LMDB readers (None uses default of 126)
    pub fn with_max_readers(
        db_path_param: impl AsRef<std::path::Path>,
        max_readers: Option<u32>,
    ) -> Result<Self, Error> {
        Self::with_config(db_path_param, max_readers, None)
    }

    /// Create a new relay database with full configuration
    ///
    /// # Arguments
    /// * `db_path_param` - Path where the database should be stored
    /// * `max_readers` - Maximum number of LMDB readers (None uses default of 126)
    /// * `ingester_thread_config` - Optional configuration to run on the ingester thread (e.g., CPU pinning)
    pub fn with_config(
        db_path_param: impl AsRef<std::path::Path>,
        max_readers: Option<u32>,
        ingester_thread_config: Option<Box<dyn FnOnce() + Send>>,
    ) -> Result<Self, Error> {
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

        // Open LMDB database with configuration
        info!("Opening LMDB database with max_readers: {:?}", max_readers);
        if let Ok(mode) = std::env::var("NOSTR_LMDB_MODE") {
            info!("LMDB mode: {}", mode);
        }

        let mut builder = NostrLMDB::builder(&db_path);
        if let Some(readers) = max_readers {
            builder = builder.max_readers(readers);
        }
        if let Some(config) = ingester_thread_config {
            builder = builder.with_ingester_thread_config(config);
        }

        let lmdb_instance = builder.build().map_err(|e| {
            Error::database(format!(
                "Failed to open NostrLMDB at path '{db_path:?}': {e}"
            ))
        })?;
        let lmdb = Arc::new(lmdb_instance);

        Ok(Self { lmdb })
    }

    /// Save an event directly (borrows the event)
    pub async fn save_event(&self, event: &Event, scope: &Scope) -> Result<()> {
        debug!(
            "Saving event {} to scope: {:?} (scope_name: {:?})",
            event.id,
            scope,
            scope.name()
        );

        let env = Arc::clone(&self.lmdb);
        let scoped_view = env.scoped(scope).map_err(|e| {
            error!("Error getting scoped view: {:?}", e);
            Error::database(format!("Failed to get scoped view: {e}"))
        })?;

        scoped_view.save_event(event).await.map_err(|e| {
            error!("Error saving event for scope {:?}: {:?}", scope, e);
            Box::new(e) as Box<dyn std::error::Error>
        })?;

        debug!(
            "Event {} saved successfully to scope: {:?} (scope_name: {:?})",
            event.id,
            scope,
            scope.name()
        );
        Ok(())
    }

    /// Save an event directly (takes ownership to avoid cloning)
    pub async fn save_event_owned(&self, event: Event, scope: &Scope) -> Result<()> {
        debug!(
            "Saving event {} to scope: {:?} (scope_name: {:?})",
            event.id,
            scope,
            scope.name()
        );

        let env = Arc::clone(&self.lmdb);
        let scoped_view = env.scoped(scope).map_err(|e| {
            error!("Error getting scoped view: {:?}", e);
            Error::database(format!("Failed to get scoped view: {e}"))
        })?;

        scoped_view.save_event_owned(event).await.map_err(|e| {
            error!("Error saving event for scope {:?}: {:?}", scope, e);
            Box::new(e) as Box<dyn std::error::Error>
        })?;

        debug!("Event saved successfully to scope: {:?}", scope);
        Ok(())
    }

    /// Delete events matching a filter
    pub async fn delete(&self, filter: Filter, scope: &Scope) -> Result<()> {
        let lmdb = Arc::clone(&self.lmdb);
        let scoped_view = lmdb.scoped(scope).map_err(|e| {
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
        let lmdb = Arc::clone(&self.lmdb);
        let scoped_view = lmdb.scoped(scope).map_err(|e| {
            error!("Error getting scoped view: {:?}", e);
            Error::database(format!("Failed to get scoped view: {e}"))
        })?;

        let mut all_events = Events::new(&Filter::new());

        // Query each filter separately and combine results
        for filter in filters {
            match scoped_view.query(filter).await {
                Ok(events) => all_events.extend(events),
                Err(e) => {
                    // Check if this is a NotFound error (database integrity issue)
                    match &e {
                        DatabaseError::Backend(backend_err) => {
                            let error_str = backend_err.to_string();
                            if error_str.contains("Not found") {
                                // This is NOT an empty result - it's a database integrity issue!
                                // An index points to an event that doesn't exist in storage
                                error!(
                                    "Database integrity error: Index contains reference to non-existent event. \
                                    This indicates corrupted indices in the database. \
                                    Please run the nostr-lmdb-integrity tool to check and repair the database. \
                                    Error: {}", error_str
                                );
                                // NotFound errors indicate corrupted indices that need repair
                                // The nostr-lmdb-integrity tool can identify and remove these corrupt entries

                                // For now, continue with other filters to avoid blocking queries
                                continue;
                            }
                            // Other backend errors should be propagated
                            error!("Database backend error: {}", error_str);
                            return Err(Error::database(format!("Database backend error: {e}")));
                        }
                        DatabaseError::NotSupported => {
                            error!("Database operation not supported: {:?}", e);
                            return Err(Error::database("Database operation not supported"));
                        }
                    }
                }
            }
        }

        Ok(all_events)
    }

    /// Get count of events matching filters
    pub async fn count(&self, filters: Vec<Filter>, scope: &Scope) -> Result<usize, Error> {
        let lmdb = Arc::clone(&self.lmdb);
        let scoped_view = lmdb.scoped(scope).map_err(|e| {
            error!("Error getting scoped view: {:?}", e);
            Error::database(format!("Failed to get scoped view: {e}"))
        })?;

        let mut total_count = 0;

        // Count each filter separately and sum results
        for filter in filters {
            match scoped_view.count(filter).await {
                Ok(count) => total_count += count,
                Err(e) => {
                    // Check if this is a NotFound error (database integrity issue)
                    let error_str = e.to_string();
                    if error_str.contains("Backend(NotFound)")
                        || error_str.contains("NotFound")
                        || error_str.contains("Not found")
                    {
                        error!(
                            "Database integrity error during count: Index contains reference to non-existent event. \
                            This indicates corrupted indices. Please run the nostr-lmdb-integrity tool. \
                            Error: {}", error_str
                        );
                        // Continue with 0 count for this filter to avoid blocking
                        // Note: This count may be inaccurate due to corruption
                        continue;
                    }
                    error!("Error counting events: {:?}", e);
                    return Err(Error::database(format!("Failed to count events: {e}")));
                }
            }
        }

        Ok(total_count)
    }

    /// Get negentropy items (EventId, Timestamp) for efficient set reconciliation
    pub async fn negentropy_items(
        &self,
        filter: Filter,
        scope: &Scope,
    ) -> Result<Vec<(EventId, Timestamp)>, Error> {
        let lmdb = Arc::clone(&self.lmdb);
        let scoped_view = lmdb.scoped(scope).map_err(|e| {
            error!("Error getting scoped view: {:?}", e);
            Error::database(format!("Failed to get scoped view: {e}"))
        })?;

        match scoped_view.negentropy_items(filter).await {
            Ok(items) => {
                debug!("Retrieved {} negentropy items from database", items.len());
                Ok(items)
            }
            Err(e) => {
                // Check if this is a NotFound error (empty database)
                let error_str = e.to_string();
                if error_str.contains("Backend(NotFound)")
                    || error_str.contains("NotFound")
                    || error_str.contains("Not found")
                {
                    debug!(
                        "No negentropy items found (database may be empty): {}",
                        error_str
                    );
                    // Return empty result for empty database
                    Ok(Vec::new())
                } else {
                    error!("Error getting negentropy items: {:?}", e);
                    Err(Error::database(format!(
                        "Failed to get negentropy items: {e}"
                    )))
                }
            }
        }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    use tempfile::TempDir;

    async fn generate_test_event(index: usize) -> Event {
        let keys = Keys::generate();
        EventBuilder::text_note(format!("Test event #{index}"))
            .sign_with_keys(&keys)
            .expect("Failed to create event")
    }

    #[tokio::test]
    async fn test_save_and_query_events() {
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("test_save_query.db");
        let event_count = 10;

        // Create and populate database
        let database = RelayDatabase::new(&db_path).expect("Failed to create database");
        let database = Arc::new(database);

        // Save events
        for i in 0..event_count {
            let event = generate_test_event(i).await;
            database
                .save_event(&event, &Scope::Default)
                .await
                .expect("Failed to save event");
        }

        // Query and verify events were saved
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

    #[tokio::test]
    async fn test_delete_events() {
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("test_delete.db");

        let database = RelayDatabase::new(&db_path).expect("Failed to create database");
        let database = Arc::new(database);

        // Save some events
        let keys = Keys::generate();
        for i in 0..5 {
            let event = EventBuilder::text_note(format!("Event {i}"))
                .sign_with_keys(&keys)
                .expect("Failed to create event");
            database
                .save_event(&event, &Scope::Default)
                .await
                .expect("Failed to save event");
        }

        // Verify events exist
        let count_before = database
            .count(
                vec![Filter::new().author(keys.public_key())],
                &Scope::Default,
            )
            .await
            .expect("Failed to count events");
        assert_eq!(count_before, 5);

        // Delete events
        database
            .delete(Filter::new().author(keys.public_key()), &Scope::Default)
            .await
            .expect("Failed to delete events");

        // Verify events are deleted
        let count_after = database
            .count(
                vec![Filter::new().author(keys.public_key())],
                &Scope::Default,
            )
            .await
            .expect("Failed to count events");
        assert_eq!(count_after, 0);
    }

    #[tokio::test]
    async fn test_scoped_operations() {
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("test_scoped.db");

        let database = RelayDatabase::new(&db_path).expect("Failed to create database");
        let database = Arc::new(database);

        // Create events in different scopes
        let scope_a = Scope::named("tenant_a").unwrap();
        let scope_b = Scope::named("tenant_b").unwrap();

        // Save events in scope A
        for i in 0..3 {
            let event = generate_test_event(i).await;
            database
                .save_event(&event, &scope_a)
                .await
                .expect("Failed to save event in scope A");
        }

        // Save events in scope B
        for i in 3..6 {
            let event = generate_test_event(i).await;
            database
                .save_event(&event, &scope_b)
                .await
                .expect("Failed to save event in scope B");
        }

        // Save events in default scope
        for i in 6..9 {
            let event = generate_test_event(i).await;
            database
                .save_event(&event, &Scope::Default)
                .await
                .expect("Failed to save event in default scope");
        }

        // Verify scope isolation
        let count_a = database
            .count(vec![Filter::new()], &scope_a)
            .await
            .expect("Failed to count in scope A");
        let count_b = database
            .count(vec![Filter::new()], &scope_b)
            .await
            .expect("Failed to count in scope B");
        let count_default = database
            .count(vec![Filter::new()], &Scope::Default)
            .await
            .expect("Failed to count in default scope");

        assert_eq!(count_a, 3);
        assert_eq!(count_b, 3);
        assert_eq!(count_default, 3);
    }
}
