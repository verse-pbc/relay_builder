use nostr_relay_builder::{CryptoWorker, RelayDatabase};
use nostr_sdk::prelude::*;
use std::sync::Arc;
use std::time::Instant;
use tempfile::TempDir;
use tokio_util::task::TaskTracker;

/// Generate a test event
async fn generate_event(index: usize) -> Event {
    let keys = Keys::generate();
    EventBuilder::text_note(format!("Benchmark event #{}", index))
        .sign(&keys)
        .await
        .expect("Failed to create event")
}

#[tokio::main]
async fn main() {
    let event_counts = vec![100, 1000, 5000];
    let channel_type = "Flume";

    println!(
        "Running channel performance comparison with: {}",
        channel_type
    );
    println!("================================================");

    for event_count in event_counts {
        println!("\nTesting with {} events:", event_count);

        // Setup
        let tmp_dir = TempDir::new().unwrap();
        let db_path = tmp_dir.path().join("bench.db");
        let keys = Arc::new(Keys::generate());
        let task_tracker = TaskTracker::new();
        let crypto_sender = CryptoWorker::spawn(keys.clone(), &task_tracker);
        let database = Arc::new(
            RelayDatabase::new(&db_path, crypto_sender).expect("Failed to create database"),
        );

        // Measure time to send all events
        let start = Instant::now();

        for i in 0..event_count {
            let event = generate_event(i).await;
            database
                .save_signed_event(event, nostr_lmdb::Scope::Default)
                .await
                .expect("Failed to save event");
        }

        let send_duration = start.elapsed();

        // Check queue metrics if available
        let queue_len = database.write_queue_len();
        let utilization = database.write_queue_utilization();
        println!(
            "  Queue status after sending: {} items ({:.1}% full)",
            queue_len, utilization
        );

        // Measure time to fully persist
        let persist_start = Instant::now();

        Arc::try_unwrap(database)
            .expect("Failed to unwrap Arc")
            .shutdown()
            .await
            .expect("Failed to shutdown");

        let persist_duration = persist_start.elapsed();
        let total_duration = start.elapsed();

        // Calculate throughput
        let send_throughput = event_count as f64 / send_duration.as_secs_f64();
        let total_throughput = event_count as f64 / total_duration.as_secs_f64();

        println!(
            "  Send time: {:.3}s ({:.0} events/sec)",
            send_duration.as_secs_f64(),
            send_throughput
        );
        println!("  Persist time: {:.3}s", persist_duration.as_secs_f64());
        println!(
            "  Total time: {:.3}s ({:.0} events/sec)",
            total_duration.as_secs_f64(),
            total_throughput
        );

        // Cleanup
        task_tracker.close();
        task_tracker.wait().await;
    }

    println!("\n================================================");
    println!("Benchmark complete!");
}
