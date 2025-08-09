//! Strfry-style ActiveMonitors optimization for efficient event distribution
//!
//! This module implements an inverted index system that reduces event distribution
//! complexity from O(n*m) to O(log k + m) where:
//! - n = number of connections
//! - m = number of filters per connection
//! - k = number of unique index keys

use dashmap::DashMap;
use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, trace};

/// Unique identifier for a filter
type FilterId = u64;

/// Type alias for tag indexes
type TagIndex = HashMap<String, BTreeMap<String, Vec<Arc<TrackedFilter>>>>;

/// Fields that can be indexed for fast lookup
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IndexedField {
    EventId,
    Author,
    Kind,
    Tag(SingleLetterTag), // Single letter tag like 'e', 'p', 'h'
}

/// A filter with tracking information for deduplication
#[derive(Debug)]
struct TrackedFilter {
    /// The original filter
    filter: Filter,
    /// Last event ID seen by this filter (for deduplication)
    last_seen_event_id: RwLock<Option<EventId>>,
    /// Parent subscription (connection_id, subscription_id)
    parent_subscription: (String, SubscriptionId),
    /// Which field this filter is indexed by (None for match-all filters)
    indexed_field: Option<IndexedField>,
    /// Unique ID for this filter
    id: FilterId,
}

impl TrackedFilter {
    fn new(
        filter: Filter,
        parent_subscription: (String, SubscriptionId),
        indexed_field: Option<IndexedField>,
        id: FilterId,
    ) -> Self {
        Self {
            filter,
            last_seen_event_id: RwLock::new(None),
            parent_subscription,
            indexed_field,
            id,
        }
    }

    fn has_seen_event(&self, event_id: &EventId) -> bool {
        self.last_seen_event_id.read().as_ref() == Some(event_id)
    }

    /// Mark this event as seen by this filter
    fn mark_event_seen(&self, event_id: EventId) {
        *self.last_seen_event_id.write() = Some(event_id);
    }

    fn matches_event(&self, event: &Event) -> bool {
        self.filter
            .match_event(event, nostr_sdk::filter::MatchEventOptions::default())
    }
}

/// A group of filters for a subscription with group-level deduplication
#[derive(Debug)]
struct FilterGroup {
    /// Connection ID that owns this subscription
    #[allow(dead_code)]
    connection_id: String,
    /// Subscription ID
    #[allow(dead_code)]
    subscription_id: SubscriptionId,
    /// Last event seen by any filter in this group
    last_seen_event_id: RwLock<Option<EventId>>,
    /// IDs of filters in this group
    filter_ids: RwLock<HashSet<FilterId>>,
}

impl FilterGroup {
    fn new(connection_id: String, subscription_id: SubscriptionId) -> Self {
        Self {
            connection_id,
            subscription_id,
            last_seen_event_id: RwLock::new(None),
            filter_ids: RwLock::new(HashSet::new()),
        }
    }

    fn has_seen_event(&self, event_id: &EventId) -> bool {
        self.last_seen_event_id.read().as_ref() == Some(event_id)
    }

    /// Mark this event as seen by this group
    fn mark_event_seen(&self, event_id: EventId) {
        *self.last_seen_event_id.write() = Some(event_id);
    }
}

/// Inverted index for efficient event distribution
pub struct SubscriptionIndex {
    /// Index by author public key
    authors: RwLock<BTreeMap<PublicKey, Vec<Arc<TrackedFilter>>>>,
    /// Index by event kind
    kinds: RwLock<BTreeMap<Kind, Vec<Arc<TrackedFilter>>>>,
    /// Index by event ID
    event_ids: RwLock<BTreeMap<EventId, Vec<Arc<TrackedFilter>>>>,
    /// Index by tags (tag name -> tag value -> filters)
    tags: RwLock<TagIndex>,

    /// Filters that match all events (empty filters)
    match_all_filters: RwLock<Vec<Arc<TrackedFilter>>>,

    /// All tracked filters by ID
    all_filters: DashMap<FilterId, Arc<TrackedFilter>>,
    /// Filter groups for subscription-level deduplication
    filter_groups: DashMap<(String, SubscriptionId), Arc<RwLock<FilterGroup>>>,

    /// Next filter ID
    next_filter_id: AtomicU64,
}

impl Default for SubscriptionIndex {
    fn default() -> Self {
        Self::new()
    }
}

impl SubscriptionIndex {
    pub fn new() -> Self {
        Self {
            authors: RwLock::new(BTreeMap::new()),
            kinds: RwLock::new(BTreeMap::new()),
            event_ids: RwLock::new(BTreeMap::new()),
            tags: RwLock::new(HashMap::new()),
            match_all_filters: RwLock::new(Vec::new()),
            all_filters: DashMap::new(),
            filter_groups: DashMap::new(),
            next_filter_id: AtomicU64::new(1),
        }
    }

    /// Add a subscription with its filters
    pub fn add_subscription(
        &self,
        connection_id: &str,
        subscription_id: &SubscriptionId,
        filters: Vec<Filter>,
    ) {
        debug!(
            "Adding subscription {} for connection {} with {} filters",
            subscription_id,
            connection_id,
            filters.len()
        );

        // Create or get the filter group
        let group_key = (connection_id.to_string(), subscription_id.clone());
        let filter_group = self
            .filter_groups
            .entry(group_key)
            .or_insert_with(|| {
                Arc::new(RwLock::new(FilterGroup::new(
                    connection_id.to_string(),
                    subscription_id.clone(),
                )))
            })
            .clone();

        // Add each filter
        for filter in filters {
            let filter_id = self.next_filter_id.fetch_add(1, Ordering::Relaxed);

            // Determine the best index field for this filter
            let indexed_field = self.choose_index_field(&filter);

            let tracked_filter = Arc::new(TrackedFilter::new(
                filter,
                (connection_id.to_string(), subscription_id.clone()),
                indexed_field,
                filter_id,
            ));

            // Add to all_filters map
            self.all_filters.insert(filter_id, tracked_filter.clone());

            // Add to filter group
            filter_group.write().filter_ids.write().insert(filter_id);

            // Add to appropriate indexes
            self.add_filter_to_indexes(&tracked_filter);
        }
    }

    /// Remove a subscription and all its filters
    pub fn remove_subscription(&self, connection_id: &str, subscription_id: &SubscriptionId) {
        debug!(
            "Removing subscription {} for connection {}",
            subscription_id, connection_id
        );

        let group_key = (connection_id.to_string(), subscription_id.clone());

        if let Some((_, filter_group)) = self.filter_groups.remove(&group_key) {
            let filter_ids: Vec<FilterId> = filter_group
                .write()
                .filter_ids
                .read()
                .iter()
                .copied()
                .collect();

            // Remove each filter
            for filter_id in filter_ids {
                if let Some((_, tracked_filter)) = self.all_filters.remove(&filter_id) {
                    self.remove_filter_from_indexes(&tracked_filter);
                }
            }
        }
    }

    /// Distribute an event to matching subscriptions
    pub fn distribute_event(&self, event: &Event) -> Vec<(String, SubscriptionId)> {
        trace!("Distributing event {} using indexes", event.id);

        let mut matching_subscriptions = Vec::new();
        let mut checked_filters = HashSet::new();
        let mut checked_groups = HashSet::new();

        // Collect filters from all relevant indexes
        let candidate_filters = self.get_candidate_filters(event);

        trace!(
            "Found {} candidate filters from indexes for event {}",
            candidate_filters.len(),
            event.id
        );

        for filter in candidate_filters {
            // Skip if we've already checked this filter
            if !checked_filters.insert(filter.id) {
                continue;
            }

            // Skip if filter has already seen this event
            if filter.has_seen_event(&event.id) {
                trace!("Filter {} already saw event {}", filter.id, event.id);
                continue;
            }

            // Check if filter matches the event
            if !filter.matches_event(event) {
                trace!("Filter {} doesn't match event {}", filter.id, event.id);
                continue;
            }

            // Mark event as seen by this filter
            filter.mark_event_seen(event.id);

            let group_key = filter.parent_subscription.clone();

            // Skip if we've already added this subscription
            if !checked_groups.insert(group_key.clone()) {
                continue;
            }

            // Check group-level deduplication
            if let Some(filter_group_ref) = self.filter_groups.get(&group_key) {
                let filter_group = filter_group_ref.value();
                if filter_group.read().has_seen_event(&event.id) {
                    trace!(
                        "Subscription {:?} already saw event {}",
                        group_key,
                        event.id
                    );
                    continue;
                }

                // Mark event as seen by this group
                filter_group.write().mark_event_seen(event.id);

                matching_subscriptions.push(group_key);
            }
        }

        trace!(
            "Event {} matched {} subscriptions",
            event.id,
            matching_subscriptions.len()
        );

        matching_subscriptions
    }

    /// Choose the best index field for a filter
    fn choose_index_field(&self, filter: &Filter) -> Option<IndexedField> {
        // Priority order (most selective to least selective):
        // 1. Event IDs (most specific)
        // 2. Authors (usually selective)
        // 3. Kinds (less selective)
        // 4. Tags (varies)

        if let Some(ids) = &filter.ids {
            if !ids.is_empty() {
                return Some(IndexedField::EventId);
            }
        }

        if let Some(authors) = &filter.authors {
            if !authors.is_empty() {
                return Some(IndexedField::Author);
            }
        }

        if let Some(kinds) = &filter.kinds {
            if !kinds.is_empty() {
                return Some(IndexedField::Kind);
            }
        }

        // Check for indexable tags
        for (tag_name, values) in filter.generic_tags.iter() {
            if !values.is_empty() {
                return Some(IndexedField::Tag(*tag_name));
            }
        }

        // No specific field - this is a match-all filter
        None
    }

    /// Add a filter to the appropriate indexes
    fn add_filter_to_indexes(&self, filter: &Arc<TrackedFilter>) {
        match filter.indexed_field {
            None => {
                // Match-all filter
                self.match_all_filters.write().push(filter.clone());
            }
            Some(IndexedField::EventId) => {
                if let Some(ids) = &filter.filter.ids {
                    let mut index = self.event_ids.write();
                    for id in ids {
                        index.entry(*id).or_default().push(filter.clone());
                    }
                }
            }
            Some(IndexedField::Author) => {
                if let Some(authors) = &filter.filter.authors {
                    let mut index = self.authors.write();
                    for author in authors {
                        index.entry(*author).or_default().push(filter.clone());
                    }
                }
            }
            Some(IndexedField::Kind) => {
                if let Some(kinds) = &filter.filter.kinds {
                    let mut index = self.kinds.write();
                    for kind in kinds {
                        index.entry(*kind).or_default().push(filter.clone());
                    }
                } else {
                    // If no specific kinds, this filter matches all kinds
                    // We'll need to check it for every event
                    // For now, we'll skip indexing it
                    debug!(
                        "Filter {} has no specific kinds, cannot index by kind",
                        filter.id
                    );
                    // TODO: Keep a separate list of "match-all" filters
                }
            }
            Some(IndexedField::Tag(tag_name)) => {
                if let Some(values) = filter.filter.generic_tags.get(&tag_name) {
                    let mut tags_index = self.tags.write();
                    let tag_index = tags_index.entry(tag_name.as_str().to_string()).or_default();
                    for value in values {
                        tag_index
                            .entry(value.clone())
                            .or_default()
                            .push(filter.clone());
                    }
                }
            }
        }
    }

    /// Remove a filter from all indexes
    fn remove_filter_from_indexes(&self, filter: &Arc<TrackedFilter>) {
        match filter.indexed_field {
            None => {
                // Remove from match-all filters
                self.match_all_filters.write().retain(|f| f.id != filter.id);
            }
            Some(IndexedField::EventId) => {
                if let Some(ids) = &filter.filter.ids {
                    let mut index = self.event_ids.write();
                    for id in ids {
                        if let Some(filters) = index.get_mut(id) {
                            filters.retain(|f| f.id != filter.id);
                            if filters.is_empty() {
                                index.remove(id);
                            }
                        }
                    }
                }
            }
            Some(IndexedField::Author) => {
                if let Some(authors) = &filter.filter.authors {
                    let mut index = self.authors.write();
                    for author in authors {
                        if let Some(filters) = index.get_mut(author) {
                            filters.retain(|f| f.id != filter.id);
                            if filters.is_empty() {
                                index.remove(author);
                            }
                        }
                    }
                }
            }
            Some(IndexedField::Kind) => {
                if let Some(kinds) = &filter.filter.kinds {
                    let mut index = self.kinds.write();
                    for kind in kinds {
                        if let Some(filters) = index.get_mut(kind) {
                            filters.retain(|f| f.id != filter.id);
                            if filters.is_empty() {
                                index.remove(kind);
                            }
                        }
                    }
                }
            }
            Some(IndexedField::Tag(tag_name)) => {
                if let Some(values) = filter.filter.generic_tags.get(&tag_name) {
                    let mut tags_index = self.tags.write();
                    let tag_str = tag_name.as_str().to_string();
                    if let Some(tag_index) = tags_index.get_mut(&tag_str) {
                        for value in values {
                            if let Some(filters) = tag_index.get_mut(value) {
                                filters.retain(|f| f.id != filter.id);
                                if filters.is_empty() {
                                    tag_index.remove(value);
                                }
                            }
                        }
                        if tag_index.is_empty() {
                            tags_index.remove(&tag_str);
                        }
                    }
                }
            }
        }
    }

    /// Get candidate filters that might match an event
    fn get_candidate_filters(&self, event: &Event) -> Vec<Arc<TrackedFilter>> {
        let mut candidates = Vec::new();
        let mut seen_filter_ids = HashSet::new();

        // First, add all match-all filters
        for filter in self.match_all_filters.read().iter() {
            if seen_filter_ids.insert(filter.id) {
                candidates.push(filter.clone());
            }
        }

        // Check event ID index
        {
            let index = self.event_ids.read();
            if let Some(filters) = index.get(&event.id) {
                for filter in filters {
                    if seen_filter_ids.insert(filter.id) {
                        candidates.push(filter.clone());
                    }
                }
            }
        }

        // Check author index
        {
            let index = self.authors.read();
            if let Some(filters) = index.get(&event.pubkey) {
                for filter in filters {
                    if seen_filter_ids.insert(filter.id) {
                        candidates.push(filter.clone());
                    }
                }
            }
        }

        // Check kind index
        {
            let index = self.kinds.read();
            if let Some(filters) = index.get(&event.kind) {
                for filter in filters {
                    if seen_filter_ids.insert(filter.id) {
                        candidates.push(filter.clone());
                    }
                }
            }
        }

        // Check tag indexes
        {
            let tags_index = self.tags.read();
            for tag in event.tags.iter() {
                // Try to parse tag kind as single letter
                if let Ok(single_letter) = tag.kind().as_str().parse::<SingleLetterTag>() {
                    let tag_name = single_letter.as_str().to_string();
                    if let Some(tag_index) = tags_index.get(&tag_name) {
                        for value in tag.as_slice().iter().skip(1) {
                            if let Some(filters) = tag_index.get(value) {
                                for filter in filters {
                                    if seen_filter_ids.insert(filter.id) {
                                        candidates.push(filter.clone());
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        candidates
    }

    /// Get statistics about the index
    pub fn stats(&self) -> IndexStats {
        // Count tracked events (filters that have seen at least one event)
        let tracked_events = self
            .all_filters
            .iter()
            .filter(|entry| entry.value().last_seen_event_id.read().is_some())
            .count();

        IndexStats {
            total_filters: self.all_filters.len(),
            total_subscriptions: self.filter_groups.len(),
            authors_indexed: self.authors.read().len(),
            kinds_indexed: self.kinds.read().len(),
            event_ids_indexed: self.event_ids.read().len(),
            tags_indexed: self.tags.read().values().map(|m| m.len()).sum(),
            tracked_events,
        }
    }
}

/// Statistics about the subscription index
#[derive(Debug, Clone)]
pub struct IndexStats {
    pub total_filters: usize,
    pub total_subscriptions: usize,
    pub authors_indexed: usize,
    pub kinds_indexed: usize,
    pub event_ids_indexed: usize,
    pub tags_indexed: usize,
    pub tracked_events: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::{EventBuilder, Keys};

    #[test]
    fn test_index_creation() {
        let index = SubscriptionIndex::new();
        let stats = index.stats();
        assert_eq!(stats.total_filters, 0);
        assert_eq!(stats.total_subscriptions, 0);
    }

    #[test]
    fn test_add_remove_subscription() {
        let index = SubscriptionIndex::new();

        // Add a subscription with filters
        let filters = vec![
            Filter::new().author(Keys::generate().public_key()),
            Filter::new().kind(Kind::TextNote),
        ];

        index.add_subscription("conn1", &SubscriptionId::new("sub1"), filters);

        let stats = index.stats();
        assert_eq!(stats.total_filters, 2);
        assert_eq!(stats.total_subscriptions, 1);

        // Remove the subscription
        index.remove_subscription("conn1", &SubscriptionId::new("sub1"));

        let stats = index.stats();
        assert_eq!(stats.total_filters, 0);
        assert_eq!(stats.total_subscriptions, 0);
    }

    #[test]
    fn test_event_distribution() {
        let index = SubscriptionIndex::new();
        let keys = Keys::generate();

        // Add subscription that matches our test event
        let filters = vec![Filter::new().author(keys.public_key()).kind(Kind::TextNote)];

        index.add_subscription("conn1", &SubscriptionId::new("sub1"), filters);

        // Create matching event
        let event = EventBuilder::text_note("Test message")
            .sign_with_keys(&keys)
            .unwrap();

        // Distribute event
        let matches = index.distribute_event(&event);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].0, "conn1");
        assert_eq!(matches[0].1, SubscriptionId::new("sub1"));

        // Distribute same event again - should not match due to deduplication
        let matches = index.distribute_event(&event);
        assert_eq!(matches.len(), 0);
    }

    #[test]
    fn test_multiple_subscriptions() {
        let index = SubscriptionIndex::new();
        let keys = Keys::generate();

        // Add multiple subscriptions
        index.add_subscription(
            "conn1",
            &SubscriptionId::new("sub1"),
            vec![Filter::new().author(keys.public_key())],
        );

        index.add_subscription(
            "conn2",
            &SubscriptionId::new("sub2"),
            vec![Filter::new().kind(Kind::TextNote)],
        );

        index.add_subscription(
            "conn3",
            &SubscriptionId::new("sub3"),
            vec![Filter::new().author(Keys::generate().public_key())], // Different author
        );

        // Create event that matches first two subscriptions
        let event = EventBuilder::text_note("Test message")
            .sign_with_keys(&keys)
            .unwrap();

        let matches = index.distribute_event(&event);
        assert_eq!(matches.len(), 2);

        // Verify both matching subscriptions are included
        let match_ids: HashSet<_> = matches
            .iter()
            .map(|(c, s)| (c.as_str(), s.as_str()))
            .collect();
        assert!(match_ids.contains(&("conn1", "sub1")));
        assert!(match_ids.contains(&("conn2", "sub2")));
        assert!(!match_ids.contains(&("conn3", "sub3")));
    }

    #[test]
    fn test_tag_indexing() {
        let index = SubscriptionIndex::new();

        // Add subscription with tag filter
        let filters = vec![Filter::new().hashtag("nostr")];

        index.add_subscription("conn1", &SubscriptionId::new("sub1"), filters);

        // Create event with matching hashtag
        let event = EventBuilder::text_note("Test #nostr message")
            .tags(vec![Tag::hashtag("nostr")])
            .sign_with_keys(&Keys::generate())
            .unwrap();

        let matches = index.distribute_event(&event);
        assert_eq!(matches.len(), 1);

        // Create event without matching hashtag
        let event2 = EventBuilder::text_note("Test message")
            .sign_with_keys(&Keys::generate())
            .unwrap();

        let matches = index.distribute_event(&event2);
        assert_eq!(matches.len(), 0);
    }
}
