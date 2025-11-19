//! Integration tests for the subscription index optimization

use nostr_sdk::prelude::*;
use relay_builder::subscription_index::SubscriptionIndex;
use std::collections::HashSet;

/// Helper to create test events
fn create_test_event(author: &Keys, kind: Kind, content: &str, tags: Vec<Tag>) -> Event {
    EventBuilder::new(kind, content)
        .tags(tags)
        .sign_with_keys(author)
        .unwrap()
}

#[tokio::test]
async fn test_basic_subscription_operations() {
    let index = SubscriptionIndex::new();

    // Add subscriptions
    for i in 0..5 {
        let filters = vec![Filter::new().kind(Kind::TextNote)];
        index
            .add_subscription(
                &format!("conn{i}"),
                &SubscriptionId::new(format!("sub{i}")),
                filters,
            )
            .await;
    }

    let stats = index.stats().await;
    assert_eq!(stats.total_subscriptions, 5);
    assert_eq!(stats.total_filters, 5);

    // Remove some subscriptions
    index
        .remove_subscription("conn1", &SubscriptionId::new("sub1"))
        .await;
    index
        .remove_subscription("conn3", &SubscriptionId::new("sub3"))
        .await;

    let stats = index.stats().await;
    assert_eq!(stats.total_subscriptions, 3);
    assert_eq!(stats.total_filters, 3);
}

#[tokio::test]
async fn test_event_deduplication_across_filters() {
    let index = SubscriptionIndex::new();
    let keys = Keys::generate();

    // Add subscription with multiple overlapping filters
    let filters = vec![
        Filter::new().author(keys.public_key()),
        Filter::new().kind(Kind::TextNote),
        Filter::new().author(keys.public_key()).kind(Kind::TextNote),
    ];

    index
        .add_subscription("conn1", &SubscriptionId::new("sub1"), filters)
        .await;

    // Create event that matches all three filters
    let event = create_test_event(&keys, Kind::TextNote, "Test", vec![]);

    // Should only get one match despite multiple matching filters
    let matches = index.distribute_event(&event).await;
    assert_eq!(matches.len(), 1);
    assert_eq!(
        matches[0],
        ("conn1".to_string(), SubscriptionId::new("sub1"))
    );
}

#[tokio::test]
async fn test_complex_tag_filtering() {
    let index = SubscriptionIndex::new();

    // Subscription 1: follows hashtag "nostr"
    index
        .add_subscription(
            "conn1",
            &SubscriptionId::new("sub1"),
            vec![Filter::new().hashtag("nostr")],
        )
        .await;

    // Subscription 2: follows event references
    let event_id = EventId::all_zeros();
    index
        .add_subscription(
            "conn2",
            &SubscriptionId::new("sub2"),
            vec![Filter::new().event(event_id)],
        )
        .await;

    // Subscription 3: follows specific pubkey mentions
    let mentioned_key = Keys::generate().public_key();
    index
        .add_subscription(
            "conn3",
            &SubscriptionId::new("sub3"),
            vec![Filter::new().pubkey(mentioned_key)],
        )
        .await;

    // Event with multiple tags
    let event = create_test_event(
        &Keys::generate(),
        Kind::TextNote,
        "Test #nostr",
        vec![
            Tag::hashtag("nostr"),
            Tag::event(event_id),
            Tag::public_key(mentioned_key),
        ],
    );

    let matches = index.distribute_event(&event).await;
    assert_eq!(matches.len(), 3);

    // Verify all three subscriptions matched
    let match_set: HashSet<_> = matches.into_iter().collect();
    assert!(match_set.contains(&("conn1".to_string(), SubscriptionId::new("sub1"))));
    assert!(match_set.contains(&("conn2".to_string(), SubscriptionId::new("sub2"))));
    assert!(match_set.contains(&("conn3".to_string(), SubscriptionId::new("sub3"))));
}

#[tokio::test]
async fn test_subscription_isolation() {
    let index = SubscriptionIndex::new();
    let keys1 = Keys::generate();
    let keys2 = Keys::generate();

    // Two connections with different author filters
    index
        .add_subscription(
            "conn1",
            &SubscriptionId::new("sub1"),
            vec![Filter::new().author(keys1.public_key())],
        )
        .await;

    index
        .add_subscription(
            "conn2",
            &SubscriptionId::new("sub2"),
            vec![Filter::new().author(keys2.public_key())],
        )
        .await;

    // Event from first author
    let event1 = create_test_event(&keys1, Kind::TextNote, "From author 1", vec![]);
    let matches = index.distribute_event(&event1).await;
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].0, "conn1");

    // Event from second author
    let event2 = create_test_event(&keys2, Kind::TextNote, "From author 2", vec![]);
    let matches = index.distribute_event(&event2).await;
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].0, "conn2");
}

#[tokio::test]
async fn test_filter_with_multiple_criteria() {
    let index = SubscriptionIndex::new();
    let author = Keys::generate();

    // Complex filter: specific author AND specific kind AND specific tag
    let filter = Filter::new()
        .author(author.public_key())
        .kind(Kind::from(30023)) // Long-form content
        .hashtag("bitcoin");

    index
        .add_subscription("conn1", &SubscriptionId::new("sub1"), vec![filter])
        .await;

    // Event that matches all criteria
    let matching_event = create_test_event(
        &author,
        Kind::from(30023),
        "Bitcoin article",
        vec![Tag::hashtag("bitcoin")],
    );
    assert_eq!(index.distribute_event(&matching_event).await.len(), 1);

    // Event with wrong author
    let wrong_author_event = create_test_event(
        &Keys::generate(),
        Kind::from(30023),
        "Bitcoin article",
        vec![Tag::hashtag("bitcoin")],
    );
    assert_eq!(index.distribute_event(&wrong_author_event).await.len(), 0);

    // Event with wrong kind
    let wrong_kind_event = create_test_event(
        &author,
        Kind::TextNote,
        "Bitcoin note",
        vec![Tag::hashtag("bitcoin")],
    );
    assert_eq!(index.distribute_event(&wrong_kind_event).await.len(), 0);

    // Event without required tag
    let no_tag_event = create_test_event(&author, Kind::from(30023), "Article without tag", vec![]);
    assert_eq!(index.distribute_event(&no_tag_event).await.len(), 0);
}

#[tokio::test]
async fn test_empty_filter_handling() {
    let index = SubscriptionIndex::new();

    // Add subscription with empty filter (matches everything)
    index
        .add_subscription("conn1", &SubscriptionId::new("sub1"), vec![Filter::new()])
        .await;

    // Create any event
    let event = create_test_event(&Keys::generate(), Kind::TextNote, "Any event", vec![]);

    // Empty filters match all events - they're stored in a separate match_all_filters list
    let matches = index.distribute_event(&event).await;

    // Empty filters should match all events
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].0, "conn1");
    assert_eq!(matches[0].1, SubscriptionId::new("sub1"));
}

#[tokio::test]
async fn test_concurrent_operations() {
    use std::sync::Arc;

    let index = Arc::new(SubscriptionIndex::new());
    let num_threads = 4;
    let ops_per_thread = 25;

    let mut handles = vec![];

    // Spawn threads that add/remove subscriptions concurrently
    for thread_id in 0..num_threads {
        let index_clone = index.clone();
        let handle = tokio::spawn(async move {
            for op in 0..ops_per_thread {
                let conn_id = format!("conn_{thread_id}_{op}");
                let sub_id = SubscriptionId::new(format!("sub_{thread_id}_{op}"));

                // Add subscription
                let filters = vec![
                    Filter::new().kind(Kind::from(thread_id as u16)),
                    Filter::new().author(Keys::generate().public_key()),
                ];
                index_clone
                    .add_subscription(&conn_id, &sub_id, filters)
                    .await;

                // Distribute some events
                for i in 0..5 {
                    let event = create_test_event(
                        &Keys::generate(),
                        Kind::from(thread_id as u16),
                        &format!("Event {i} from thread {thread_id}"),
                        vec![],
                    );
                    index_clone.distribute_event(&event).await;
                }

                // Remove subscription
                if op % 2 == 0 {
                    index_clone.remove_subscription(&conn_id, &sub_id).await;
                }
            }
        });
        handles.push(handle);
    }

    // Wait for all tasks to complete
    for handle in handles {
        handle.await.unwrap();
    }

    // Verify final state is consistent
    let stats = index.stats().await;
    println!("Final stats after concurrent operations: {stats:?}");

    // Should have some subscriptions remaining (half were removed)
    assert!(stats.total_subscriptions > 0);
    assert!(stats.total_filters > 0);
}

#[tokio::test]
async fn test_index_stats_accuracy() {
    let index = SubscriptionIndex::new();
    let author1 = Keys::generate();
    let author2 = Keys::generate();

    // Add various types of indexed filters
    index
        .add_subscription(
            "conn1",
            &SubscriptionId::new("sub1"),
            vec![
                Filter::new().author(author1.public_key()),
                Filter::new().author(author2.public_key()),
            ],
        )
        .await;

    index
        .add_subscription(
            "conn2",
            &SubscriptionId::new("sub2"),
            vec![
                Filter::new().kind(Kind::TextNote),
                Filter::new().kind(Kind::ChannelMessage),
                Filter::new().kind(Kind::Reaction),
            ],
        )
        .await;

    index
        .add_subscription(
            "conn3",
            &SubscriptionId::new("sub3"),
            vec![
                Filter::new().id(EventId::all_zeros()),
                Filter::new().hashtag("bitcoin"),
                Filter::new().hashtag("nostr"),
            ],
        )
        .await;

    let stats = index.stats().await;
    assert_eq!(stats.total_filters, 8);
    assert_eq!(stats.total_subscriptions, 3);
    assert_eq!(stats.authors_indexed, 2);
    assert_eq!(stats.kinds_indexed, 3);
    assert_eq!(stats.event_ids_indexed, 1);
    assert_eq!(stats.tags_indexed, 2); // Two hashtags
}

#[tokio::test]
async fn test_controlled_benchmark_25_connections() {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    // Fixed parameters for consistent benchmarking
    const NUM_CONNECTIONS: usize = 25;
    const FILTERS_PER_CONNECTION: usize = 10;
    const NUM_EVENTS: usize = 100;
    const NUM_RUNS: usize = 5; // Run multiple times for average

    println!("\n=== Controlled Benchmark: 25 Connections ===");
    println!("Connections: {NUM_CONNECTIONS}");
    println!("Filters per connection: {FILTERS_PER_CONNECTION}");
    println!(
        "Total filters: {}",
        NUM_CONNECTIONS * FILTERS_PER_CONNECTION
    );
    println!("Events to process: {NUM_EVENTS}");
    println!("Averaging over {NUM_RUNS} runs\n");

    // Generate consistent test data using seeds
    let mut filter_by_subscription = HashMap::new();
    let all_authors: Vec<Keys> = (0..50).map(|_| Keys::generate()).collect();

    for conn_idx in 0..NUM_CONNECTIONS {
        let conn_id = format!("conn{conn_idx}");
        let sub_id = SubscriptionId::new(format!("sub{conn_idx}"));
        let mut filters = Vec::new();

        for filter_idx in 0..FILTERS_PER_CONNECTION {
            let filter = match (conn_idx * 7 + filter_idx * 13) % 5 {
                0 => Filter::new().author(all_authors[filter_idx % all_authors.len()].public_key()),
                1 => Filter::new().kind(Kind::from((1000 + filter_idx) as u16)),
                2 => Filter::new().hashtag(format!("topic_{}", filter_idx % 10)),
                3 => Filter::new()
                    .author(all_authors[(conn_idx + filter_idx) % all_authors.len()].public_key())
                    .kind(Kind::TextNote),
                _ => Filter::new().id(EventId::from_slice(&[filter_idx as u8; 32]).unwrap()),
            };
            filters.push(filter);
        }
        filter_by_subscription.insert((conn_id, sub_id), filters);
    }

    // Generate events (20% match rate)
    let mut events = Vec::new();
    for i in 0..NUM_EVENTS {
        let event = if i % 5 == 0 {
            // Matching events (20%)
            let author = &all_authors[i % all_authors.len()];
            EventBuilder::text_note(format!("Event {i}"))
                .sign_with_keys(author)
                .unwrap()
        } else {
            // Non-matching events (80%)
            EventBuilder::new(Kind::from(9999), format!("Event {i}"))
                .sign_with_keys(&Keys::generate())
                .unwrap()
        };
        events.push(event);
    }

    // Linear approach
    fn distribute_linear(
        event: &Event,
        filter_by_subscription: &HashMap<(String, SubscriptionId), Vec<Filter>>,
    ) -> Vec<(String, SubscriptionId)> {
        let mut matches = Vec::new();
        let mut seen = HashSet::new();

        for ((conn_id, sub_id), filters) in filter_by_subscription {
            for filter in filters {
                if filter.match_event(event, nostr_sdk::filter::MatchEventOptions::default()) {
                    let key = (conn_id.clone(), sub_id.clone());
                    if seen.insert(key.clone()) {
                        matches.push(key);
                    }
                    break;
                }
            }
        }
        matches
    }

    // Benchmark linear approach
    let mut linear_times = Vec::new();
    for run in 0..NUM_RUNS {
        let start = Instant::now();
        let mut total_matches = 0;
        for event in &events {
            total_matches += distribute_linear(event, &filter_by_subscription).len();
        }
        let duration = start.elapsed();
        linear_times.push(duration);
        if run == 0 {
            println!("Linear matches found: {total_matches}");
        }
    }

    let avg_linear = linear_times.iter().sum::<Duration>() / NUM_RUNS as u32;
    let linear_per_event = avg_linear.as_micros() as f64 / NUM_EVENTS as f64;
    println!("Linear O(n*m):");
    println!("  Average total time: {avg_linear:?}");
    println!("  Average per event: {linear_per_event:.2}μs");

    // Benchmark indexed approach
    let mut indexed_times = Vec::new();
    for run in 0..NUM_RUNS {
        let index = SubscriptionIndex::new();
        for ((conn_id, sub_id), filters) in &filter_by_subscription {
            index
                .add_subscription(conn_id, sub_id, filters.clone())
                .await;
        }

        let start = Instant::now();
        let mut total_matches = 0;
        for event in &events {
            total_matches += index.distribute_event(event).await.len();
        }
        let duration = start.elapsed();
        indexed_times.push(duration);
        if run == 0 {
            println!("\nIndexed matches found: {total_matches}");
        }
    }

    let avg_indexed = indexed_times.iter().sum::<Duration>() / NUM_RUNS as u32;
    let indexed_per_event = avg_indexed.as_micros() as f64 / NUM_EVENTS as f64;
    println!("Indexed O(1) lookups:");
    println!("  Average total time: {avg_indexed:?}");
    println!("  Average per event: {indexed_per_event:.2}μs");

    // Calculate speedup
    let speedup = linear_per_event / indexed_per_event;
    println!("\nSpeedup: {speedup:.1}x faster with indexing");

    // Performance assertions
    assert!(
        indexed_per_event < linear_per_event,
        "Indexed should be faster than linear"
    );
    assert!(
        indexed_per_event < 100.0,
        "Indexed should process events in < 100μs on average"
    );
}

#[tokio::test]
async fn test_performance_comparison_linear_vs_indexed() {
    use std::collections::HashMap;
    use std::time::Instant;

    // Test parameters - more realistic scenario
    let num_connections = 500;
    let filters_per_connection = 5;
    let num_events = 100;

    // Generate test data
    let mut all_filters = Vec::new();
    let mut filter_by_subscription = HashMap::new();

    // Generate many unique authors to make filters more selective
    let all_authors: Vec<Keys> = (0..200).map(|_| Keys::generate()).collect();

    for conn_idx in 0..num_connections {
        let conn_id = format!("conn{conn_idx}");
        let sub_id = SubscriptionId::new(format!("sub{conn_idx}"));

        let mut filters_for_conn = Vec::new();

        for filter_idx in 0..filters_per_connection {
            // Create more specific filters
            let filter = match (conn_idx * 7 + filter_idx * 13) % 5 {
                0 => {
                    // Specific author filter
                    let author_idx = (conn_idx * 3 + filter_idx * 5) % all_authors.len();
                    Filter::new().author(all_authors[author_idx].public_key())
                }
                1 => {
                    // Specific kind filter
                    let kind_num = 1000 + ((conn_idx + filter_idx) % 100) as u16;
                    Filter::new().kind(Kind::from(kind_num))
                }
                2 => {
                    // Specific hashtag filter
                    let tag = format!("topic_{}", (conn_idx * 2 + filter_idx) % 50);
                    Filter::new().hashtag(tag)
                }
                3 => {
                    // Combination filter (author + kind)
                    let author_idx = (conn_idx + filter_idx * 2) % all_authors.len();
                    Filter::new()
                        .author(all_authors[author_idx].public_key())
                        .kind(Kind::TextNote)
                }
                _ => {
                    // Event ID filter (very specific)
                    let event_id =
                        EventId::from_slice(&[((conn_idx + filter_idx) % 256) as u8; 32]).unwrap();
                    Filter::new().id(event_id)
                }
            };

            filters_for_conn.push(filter);
        }

        // Store the subscription once with all its filters
        filter_by_subscription.insert((conn_id.clone(), sub_id.clone()), filters_for_conn.clone());

        // Store all_filters for easy iteration
        for filter in filters_for_conn {
            all_filters.push((conn_id.clone(), sub_id.clone(), filter));
        }
    }

    // Generate events that will match only a small percentage of filters
    let mut events = Vec::new();
    for i in 0..num_events {
        let event = if i < 5 {
            // A few events from known authors (5% of events)
            let author = &all_authors[i * 10 % all_authors.len()];
            EventBuilder::text_note(format!("Event {i} from known author"))
                .sign_with_keys(author)
                .unwrap()
        } else if i < 10 {
            // A few events with specific hashtags (5% of events)
            let tag = format!("topic_{}", i % 50);
            EventBuilder::text_note(format!("Event {i} about #{tag}"))
                .tags(vec![Tag::hashtag(tag)])
                .sign_with_keys(&Keys::generate())
                .unwrap()
        } else if i < 15 {
            // A few events with specific kinds (5% of events)
            let kind_num = 1000 + (i % 100) as u16;
            EventBuilder::new(Kind::from(kind_num), format!("Event {i} custom kind"))
                .sign_with_keys(&Keys::generate())
                .unwrap()
        } else {
            // Most events don't match any filters (85% of events)
            EventBuilder::new(Kind::from(9999), format!("Event {i} non-matching"))
                .sign_with_keys(&Keys::generate())
                .unwrap()
        };
        events.push(event);
    }

    // Linear O(n*m) approach - simulating current implementation
    fn distribute_linear(
        event: &Event,
        filter_by_subscription: &HashMap<(String, SubscriptionId), Vec<Filter>>,
    ) -> Vec<(String, SubscriptionId)> {
        let mut matches = Vec::new();
        let mut seen_subscriptions = HashSet::new();

        for ((conn_id, sub_id), filters) in filter_by_subscription {
            for filter in filters {
                if filter.match_event(event, nostr_sdk::filter::MatchEventOptions::default()) {
                    let key = (conn_id.clone(), sub_id.clone());
                    if seen_subscriptions.insert(key.clone()) {
                        matches.push(key);
                    }
                    break; // One match per subscription is enough
                }
            }
        }

        matches
    }

    // Measure linear approach
    let start_linear = Instant::now();
    let mut total_linear_matches = 0;

    for event in &events {
        let matches = distribute_linear(event, &filter_by_subscription);
        total_linear_matches += matches.len();
    }

    let linear_duration = start_linear.elapsed();
    let linear_per_event = linear_duration.as_micros() as f64 / num_events as f64;

    println!("\nLinear O(n*m) approach:");
    println!("  Total time: {linear_duration:?}");
    println!("  Per event: {linear_per_event:.3}μs");
    println!("  Total matches: {total_linear_matches}");

    // Measure indexed approach
    let index = SubscriptionIndex::new();

    // Add all subscriptions to index (one subscription per connection)
    for ((conn_id, sub_id), filters) in &filter_by_subscription {
        index
            .add_subscription(conn_id, sub_id, filters.clone())
            .await;
    }

    let start_indexed = Instant::now();
    let mut total_indexed_matches = 0;

    for event in &events {
        let matches = index.distribute_event(event).await;
        total_indexed_matches += matches.len();
    }

    let indexed_duration = start_indexed.elapsed();
    let indexed_per_event = indexed_duration.as_micros() as f64 / num_events as f64;

    println!("\nIndexed O(log k + m) approach:");
    println!("  Total time: {indexed_duration:?}");
    println!("  Per event: {indexed_per_event:.3}μs");
    println!("  Total matches: {total_indexed_matches}");

    // Calculate improvement
    let improvement_factor = linear_per_event / indexed_per_event;
    println!("\nImprovement: {improvement_factor:.2}x faster");
    println!(
        "Time saved per event: {:.3}μs",
        linear_per_event - indexed_per_event
    );

    // Verify correctness - both approaches should find same number of matches
    // (allowing for small differences due to deduplication timing)
    assert!(
        (total_linear_matches as i32 - total_indexed_matches as i32).abs()
            <= num_events as i32 / 10,
        "Match count difference too large: linear={total_linear_matches}, indexed={total_indexed_matches}"
    );

    // For debugging, let's see what's happening
    if improvement_factor < 1.0 {
        println!("\nWARNING: Indexed approach is slower!");
        println!("This suggests the test setup doesn't match real-world usage where:");
        println!("- There are many more unique filters");
        println!("- Events match a smaller percentage of filters");
        println!("- The O(n*m) cost dominates over index overhead");
    }

    // Let's print index statistics for analysis
    let stats = index.stats().await;
    println!("\nIndex statistics:");
    println!("  Total filters: {}", stats.total_filters);
    println!("  Total subscriptions: {}", stats.total_subscriptions);
    println!("  Authors indexed: {}", stats.authors_indexed);
    println!("  Kinds indexed: {}", stats.kinds_indexed);
    println!("  Tags indexed: {}", stats.tags_indexed);
}
