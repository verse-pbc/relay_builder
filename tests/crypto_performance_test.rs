//! Test crypto worker performance
//!
//! By default, these tests run with a small number of events (500) for quick testing.
//! To run full performance tests with more events, use:
//! ```
//! PERF_TEST_EVENT_COUNT=20000 cargo test test_crypto_verification_performance
//! ```

use nostr_relay_builder::crypto_worker::CryptoWorker;
use nostr_sdk::prelude::*;
use std::sync::Arc;
use std::time::Instant;
use tokio_util::task::TaskTracker;

#[tokio::test]
async fn test_crypto_verification_performance() {
    // Run test with different worker counts if specified
    if let Ok(val) = std::env::var("TEST_WORKER_COUNTS") {
        if val == "true" {
            test_with_different_worker_counts().await;
            return;
        }
    }
    // Use environment variable to control test size
    let event_count = std::env::var("PERF_TEST_EVENT_COUNT")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(500); // Default to 500 events for regular tests

    // Initialize crypto worker
    let keys = Arc::new(Keys::generate());
    let task_tracker = TaskTracker::new();

    // Show worker configuration
    let worker_threads = std::env::var("CRYPTO_WORKER_THREADS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or_else(|| {
            // Match the default from crypto_worker.rs
            std::thread::available_parallelism()
                .map(|n| (n.get().saturating_sub(1)).max(1))
                .unwrap_or(1)
        });
    println!(
        "Using {} crypto worker threads (CPU cores: {})",
        worker_threads,
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(0)
    );

    let crypto_sender = CryptoWorker::spawn(Arc::clone(&keys), &task_tracker);

    // Create test events
    let test_keys = Keys::generate();

    println!("Creating {} signed events...", event_count);
    let create_start = Instant::now();

    // Create events in parallel batches for faster generation
    let batch_size = 100;
    let mut handles = Vec::new();

    for batch_start in (0..event_count).step_by(batch_size) {
        let batch_end = (batch_start + batch_size).min(event_count);
        let test_keys = test_keys.clone();

        let handle = tokio::spawn(async move {
            let mut batch_events = Vec::new();
            for i in batch_start..batch_end {
                let event = EventBuilder::text_note(format!("Test message {}", i))
                    .build(test_keys.public_key());
                let signed_event = test_keys.sign_event(event).await.unwrap();
                batch_events.push(signed_event);
            }
            batch_events
        });
        handles.push(handle);
    }

    // Collect all events
    let mut events = Vec::new();
    for handle in handles {
        events.extend(handle.await.unwrap());
    }

    let create_duration = create_start.elapsed();
    println!("Created {} events in {:?}", event_count, create_duration);

    // Test verification performance
    println!("\nVerifying {} events...", event_count);
    let verify_start = Instant::now();

    // Send all verification requests concurrently
    let mut verify_futures = Vec::new();
    for event in events {
        let sender = crypto_sender.clone();
        verify_futures.push(async move { sender.verify_event(event).await });
    }

    // Wait for all verifications to complete
    let results = futures_util::future::join_all(verify_futures).await;
    for result in results {
        result.unwrap();
    }

    let verify_duration = verify_start.elapsed();
    let events_per_sec = event_count as f64 / verify_duration.as_secs_f64();

    println!("Verified {} events in {:?}", event_count, verify_duration);
    println!("Verification rate: {:.0} events/second", events_per_sec);
    println!(
        "Time per event: {:.3} ms",
        verify_duration.as_millis() as f64 / event_count as f64
    );

    // Assert reasonable performance (at least 100 events/second)
    assert!(
        events_per_sec >= 100.0,
        "Verification too slow: {:.0} events/second",
        events_per_sec
    );

    // Cleanup
    drop(crypto_sender);
    task_tracker.close();
    task_tracker.wait().await;
}

async fn test_with_different_worker_counts() {
    let cpu_count = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(4);

    println!("System has {} CPU cores", cpu_count);
    println!("\nTesting crypto performance with different worker counts...\n");

    // Test with different worker counts
    let worker_counts = vec![
        cpu_count / 2, // Half CPU count
        cpu_count,     // Equal to CPU count
        cpu_count * 2, // 2x CPU count
        cpu_count * 4, // 4x CPU count
    ];

    for worker_count in worker_counts {
        println!("Testing with {} workers...", worker_count);
        std::env::set_var("CRYPTO_WORKER_THREADS", worker_count.to_string());

        // Run a performance test
        let keys = Arc::new(Keys::generate());
        let task_tracker = TaskTracker::new();
        let crypto_sender = CryptoWorker::spawn(Arc::clone(&keys), &task_tracker);

        // Create test events
        let test_keys = Keys::generate();
        let event_count = 2000;
        let mut events = Vec::new();

        for i in 0..event_count {
            let event = EventBuilder::text_note(format!("Test message {}", i))
                .build(test_keys.public_key());
            let signed_event = test_keys.sign_event(event).await.unwrap();
            events.push(signed_event);
        }

        // Test concurrent verification
        let verify_start = Instant::now();
        let mut verify_futures = Vec::new();

        for event in events {
            let sender = crypto_sender.clone();
            verify_futures.push(async move { sender.verify_event(event).await });
        }

        let results = futures_util::future::join_all(verify_futures).await;
        for result in results {
            result.unwrap();
        }

        let verify_duration = verify_start.elapsed();
        let events_per_sec = event_count as f64 / verify_duration.as_secs_f64();

        println!("  - Verification rate: {:.0} events/second", events_per_sec);
        println!(
            "  - Time per event: {:.3} ms\n",
            verify_duration.as_millis() as f64 / event_count as f64
        );

        // Cleanup
        drop(crypto_sender);
        task_tracker.close();
        task_tracker.wait().await;
    }
}

#[tokio::test]
async fn test_crypto_worker_concurrency() {
    // Test with concurrent verification
    let keys = Arc::new(Keys::generate());
    let task_tracker = TaskTracker::new();
    let crypto_sender = CryptoWorker::spawn(Arc::clone(&keys), &task_tracker);

    // Create test events
    let test_keys = Keys::generate();
    let mut events = Vec::new();

    for i in 0..1000 {
        let event =
            EventBuilder::text_note(format!("Test message {}", i)).build(test_keys.public_key());
        let signed_event = test_keys.sign_event(event).await.unwrap();
        events.push(signed_event);
    }

    // Test concurrent verification
    println!("\nVerifying 1000 events concurrently...");
    let verify_start = Instant::now();

    let mut handles = Vec::new();
    for event in events {
        let sender = crypto_sender.clone();
        let handle = tokio::spawn(async move {
            sender.verify_event(event).await.unwrap();
        });
        handles.push(handle);
    }

    // Wait for all verifications
    for handle in handles {
        handle.await.unwrap();
    }

    let verify_duration = verify_start.elapsed();
    let events_per_sec = 1000.0 / verify_duration.as_secs_f64();

    println!("Verified 1000 events concurrently in {:?}", verify_duration);
    println!(
        "Concurrent verification rate: {:.0} events/second",
        events_per_sec
    );

    // Cleanup
    drop(crypto_sender);
    task_tracker.close();
    task_tracker.wait().await;
}

#[tokio::test]
async fn test_crypto_worker_saturation() {
    // Test to find the saturation point of crypto workers
    let task_tracker = TaskTracker::new();
    let keys = Arc::new(Keys::generate());

    let worker_count = std::thread::available_parallelism()
        .map(|n| (n.get().saturating_sub(1)).max(1))
        .unwrap_or(1);

    println!("\n=== Testing Crypto Worker Saturation ===");
    println!("Worker count: {}", worker_count);

    let crypto_sender = CryptoWorker::spawn(Arc::clone(&keys), &task_tracker);

    // Test with increasing concurrent operations - reduced counts for faster testing
    let test_counts = vec![100, 500, 1000, 2000];

    for count in test_counts {
        // Create events
        let mut events = Vec::new();
        for i in 0..count {
            let event =
                EventBuilder::text_note(format!("Saturation test {}", i)).build(keys.public_key());
            let signed = keys.sign_event(event).await.unwrap();
            events.push(signed);
        }

        // Launch all verifications concurrently
        let start = Instant::now();
        let mut futures = Vec::new();

        for event in events {
            let sender = crypto_sender.clone();
            futures.push(async move { sender.verify_event(event).await });
        }

        let results = futures_util::future::join_all(futures).await;
        let duration = start.elapsed();

        let successes = results.iter().filter(|r| r.is_ok()).count();
        let rate = successes as f64 / duration.as_secs_f64();

        println!(
            "\n{} concurrent ops: {:.0} ops/sec ({:.2}ms avg latency)",
            count,
            rate,
            duration.as_millis() as f64 / count as f64
        );
    }

    // Cleanup
    drop(crypto_sender);
    task_tracker.close();
    task_tracker.wait().await;
}
