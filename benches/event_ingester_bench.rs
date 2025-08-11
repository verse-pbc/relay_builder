use criterion::{black_box, criterion_group, criterion_main, Criterion, Throughput};
use nostr_sdk::prelude::*;
use relay_builder::event_ingester::EventIngester;
use std::sync::Arc;

fn bench_event_ingestion(c: &mut Criterion) {
    let mut group = c.benchmark_group("event_ingestion");

    // Test with different message counts
    for msg_count in [100, 1000, 10000] {
        group.throughput(Throughput::Elements(msg_count));

        group.bench_function(format!("parse_and_verify_{msg_count}_events"), |b| {
            // Setup
            let keys = Arc::new(Keys::generate());
            let ingester = EventIngester::new(Arc::clone(&keys));

            // Pre-generate events
            let events: Vec<String> = (0..msg_count)
                .map(|i| {
                    let event = EventBuilder::text_note(format!("Test message {i}"))
                        .sign_with_keys(&keys)
                        .unwrap();
                    format!(r#"["EVENT", {}]"#, event.as_json())
                })
                .collect();

            // Benchmark
            b.iter(|| {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    for event_str in &events {
                        let result = ingester.process_message(event_str.clone()).await;
                        let _ = black_box(result);
                    }
                });
            });
        });
    }

    group.finish();
}

fn bench_parallel_ingestion(c: &mut Criterion) {
    let mut group = c.benchmark_group("parallel_ingestion");

    // Test parallel processing with different thread counts
    for concurrency in [1, 4, 8, 16] {
        group.bench_function(format!("concurrent_{concurrency}_clients"), |b| {
            // Setup
            let keys = Arc::new(Keys::generate());
            let ingester = EventIngester::new(Arc::clone(&keys));

            // Pre-generate events for each client
            let events_per_client = 100;
            let all_events: Vec<Vec<String>> = (0..concurrency)
                .map(|client_id| {
                    (0..events_per_client)
                        .map(|msg_id| {
                            let event = EventBuilder::text_note(format!(
                                "Client {client_id} message {msg_id}"
                            ))
                            .sign_with_keys(&keys)
                            .unwrap();
                            format!(r#"["EVENT", {}]"#, event.as_json())
                        })
                        .collect()
                })
                .collect();

            // Benchmark
            b.iter(|| {
                let rt = tokio::runtime::Runtime::new().unwrap();
                rt.block_on(async {
                    let mut handles = Vec::new();

                    for client_events in &all_events {
                        let ingester_clone = ingester.clone();
                        let events = client_events.clone();

                        let handle = tokio::spawn(async move {
                            for event_str in events {
                                let result = ingester_clone.process_message(event_str).await;
                                let _ = black_box(result);
                            }
                        });

                        handles.push(handle);
                    }

                    // Wait for all clients to finish
                    for handle in handles {
                        handle.await.unwrap();
                    }
                });
            });
        });
    }

    group.finish();
}

criterion_group!(benches, bench_event_ingestion, bench_parallel_ingestion);
criterion_main!(benches);
