use criterion::{black_box, criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use nostr_relay_builder::{CryptoWorker, RelayDatabase};
use nostr_sdk::prelude::*;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::runtime::Runtime;
use tokio_util::task::TaskTracker;

/// Generate a test event
async fn generate_event(index: usize) -> Event {
    let keys = Keys::generate();
    EventBuilder::text_note(format!("Benchmark event #{}", index))
        .sign(&keys)
        .await
        .expect("Failed to create event")
}

/// Benchmark write throughput with different channel implementations
fn bench_write_throughput(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("write_throughput");
    group.throughput(Throughput::Elements(1000));
    group.sample_size(10);

    // Test different event counts
    for event_count in [100, 1000].iter() {
        // Benchmark with current channel implementation
        let bench_name = "flume";

        group.bench_with_input(
            BenchmarkId::new(bench_name, event_count),
            event_count,
            |b, &count| {
                b.to_async(&rt).iter(|| async {
                    let tmp_dir = TempDir::new().unwrap();
                    let db_path = tmp_dir.path().join("bench.db");
                    let keys = Arc::new(Keys::generate());
                    let task_tracker = TaskTracker::new();
                    let crypto_sender = CryptoWorker::spawn(keys.clone(), &task_tracker);
                    let (database, db_sender) = RelayDatabase::new(&db_path, crypto_sender)
                        .expect("Failed to create database");
                    let _database = Arc::new(database);

                    // Send events
                    for i in 0..count {
                        let event = generate_event(i).await;
                        db_sender
                            .save_signed_event(event, nostr_lmdb::Scope::Default)
                            .await
                            .expect("Failed to save event");
                    }

                    // Drop sender and ensure all events are persisted
                    drop(db_sender);

                    black_box(count);
                });
            },
        );
    }

    group.finish();
}

/// Benchmark backpressure behavior
fn bench_backpressure(c: &mut Criterion) {
    let rt = Runtime::new().unwrap();

    let mut group = c.benchmark_group("backpressure");
    group.throughput(Throughput::Elements(10000));
    group.sample_size(10);

    // Generate a large number of events to test backpressure
    let event_count = 10_000;

    let bench_name = "flume_backpressure";

    group.bench_function(bench_name, |b| {
        b.to_async(&rt).iter(|| async {
            let tmp_dir = TempDir::new().unwrap();
            let db_path = tmp_dir.path().join("bench.db");
            let keys = Arc::new(Keys::generate());
            let task_tracker = TaskTracker::new();
            let crypto_sender = CryptoWorker::spawn(keys.clone(), &task_tracker);
            let (database, db_sender) =
                RelayDatabase::new(&db_path, crypto_sender).expect("Failed to create database");
            let _database = Arc::new(database);

            // Send many events rapidly to trigger backpressure
            let mut handles = vec![];
            for i in 0..event_count {
                let sender = db_sender.clone();
                let handle = tokio::spawn(async move {
                    let event = generate_event(i).await;
                    sender
                        .save_signed_event(event, nostr_lmdb::Scope::Default)
                        .await
                        .expect("Failed to save event");
                });
                handles.push(handle);
            }

            // Wait for all to complete
            for handle in handles {
                handle.await.unwrap();
            }

            // Shutdown
            drop(db_sender);

            black_box(event_count);
        });
    });

    group.finish();
}

criterion_group!(benches, bench_write_throughput, bench_backpressure);
criterion_main!(benches);
