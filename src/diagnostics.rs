//! Diagnostic system for monitoring relay health and detecting memory issues

use crate::subscription_registry::{RegistryDiagnostics, SubscriptionRegistry};
use nostr_lmdb::Scope;
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::interval;
use tracing::info;

/// Stores previous measurements for delta calculation
#[derive(Default)]
struct PreviousMeasurements {
    total_connections: usize,
    total_subscriptions: usize,
    scope_metrics: HashMap<String, ScopeMetrics>,
    index_metrics: IndexMetrics,
}

#[derive(Default, Clone)]
struct ScopeMetrics {
    filter_count: usize,
    #[allow(dead_code)]
    connection_count: usize,
    tracked_events: usize,
}

#[derive(Default, Clone)]
struct IndexMetrics {
    authors: usize,
    kinds: usize,
    tags: usize,
}

/// Runs periodic health checks on the relay
pub async fn run_diagnostics(registry: Arc<SubscriptionRegistry>, interval_minutes: u64) {
    let mut interval = interval(Duration::from_secs(interval_minutes * 60));
    let mut previous = PreviousMeasurements::default();

    // Show initial diagnostics immediately
    info!(
        "Starting diagnostics with {}-minute interval",
        interval_minutes
    );
    let diagnostics = registry.get_diagnostics();
    log_health_check(&diagnostics, &mut previous);

    // Skip the first tick since we just showed initial diagnostics
    interval.tick().await;

    loop {
        interval.tick().await;

        let diagnostics = registry.get_diagnostics();
        log_health_check(&diagnostics, &mut previous);
    }
}

fn log_health_check(diagnostics: &RegistryDiagnostics, previous: &mut PreviousMeasurements) {
    info!("=== Relay Health Check ===");

    // Connections
    let conn_delta = diagnostics.total_connections as i64 - previous.total_connections as i64;
    info!(
        "Connections: {} (△ {:+})",
        diagnostics.total_connections, conn_delta
    );

    // Subscriptions
    let total_subscriptions: usize = diagnostics
        .scope_diagnostics
        .iter()
        .map(|s| s.subscription_count)
        .sum();
    let sub_delta = total_subscriptions as i64 - previous.total_subscriptions as i64;
    let avg_per_conn = if diagnostics.total_connections > 0 {
        total_subscriptions as f64 / diagnostics.total_connections as f64
    } else {
        0.0
    };

    info!(
        "Subscriptions: {} total, {:.2} avg/conn (△ {:+})",
        total_subscriptions, avg_per_conn, sub_delta
    );

    // Scopes
    info!("Scopes: {} active", diagnostics.scope_diagnostics.len());

    for scope_diag in &diagnostics.scope_diagnostics {
        let scope_key = format!("{:?}", scope_diag.scope);
        let prev_metrics = previous
            .scope_metrics
            .get(&scope_key)
            .cloned()
            .unwrap_or_default();

        let events_delta = scope_diag.tracked_events as i64 - prev_metrics.tracked_events as i64;

        info!(
            "  - {}: {} filters, {} connections, {} tracked events (△ {:+})",
            scope_name(&scope_diag.scope),
            scope_diag.filter_count,
            scope_diag.connection_count,
            scope_diag.tracked_events,
            events_delta
        );

        // Update previous metrics
        previous.scope_metrics.insert(
            scope_key,
            ScopeMetrics {
                filter_count: scope_diag.filter_count,
                connection_count: scope_diag.connection_count,
                tracked_events: scope_diag.tracked_events,
            },
        );
    }

    // Indexes (aggregate across all scopes)
    let total_authors: usize = diagnostics
        .scope_diagnostics
        .iter()
        .map(|s| s.authors_indexed)
        .sum();
    let total_kinds: usize = diagnostics
        .scope_diagnostics
        .iter()
        .map(|s| s.kinds_indexed)
        .sum();
    let total_tags: usize = diagnostics
        .scope_diagnostics
        .iter()
        .map(|s| s.tags_indexed)
        .sum();

    let authors_delta = total_authors as i64 - previous.index_metrics.authors as i64;
    let kinds_delta = total_kinds as i64 - previous.index_metrics.kinds as i64;
    let tags_delta = total_tags as i64 - previous.index_metrics.tags as i64;

    info!("Indexes:");
    info!(
        "  - Authors: {} entries (△ {:+})",
        total_authors, authors_delta
    );
    info!("  - Kinds: {} entries (△ {:+})", total_kinds, kinds_delta);
    info!("  - Tags: {} entries (△ {:+})", total_tags, tags_delta);

    // Growth warnings
    let warnings = detect_anomalies(diagnostics, conn_delta, previous);
    if !warnings.is_empty() {
        info!("Growth Warnings:");
        for warning in warnings {
            info!("  ⚠️  {}", warning);
        }
    }

    // Update previous measurements
    previous.total_connections = diagnostics.total_connections;
    previous.total_subscriptions = total_subscriptions;
    previous.index_metrics = IndexMetrics {
        authors: total_authors,
        kinds: total_kinds,
        tags: total_tags,
    };
}

fn detect_anomalies(
    diagnostics: &RegistryDiagnostics,
    conn_delta: i64,
    previous: &PreviousMeasurements,
) -> Vec<String> {
    let mut warnings = Vec::new();

    // Check for rapidly growing tracked events
    if conn_delta > 0 {
        for scope_diag in &diagnostics.scope_diagnostics {
            let scope_key = format!("{:?}", scope_diag.scope);
            if let Some(prev) = previous.scope_metrics.get(&scope_key) {
                let events_delta = scope_diag.tracked_events as i64 - prev.tracked_events as i64;
                if events_delta > 0 && events_delta > conn_delta * 100 {
                    let rate = events_delta as f64 / conn_delta as f64;
                    warnings.push(format!(
                        "Tracked events growing faster than connections ({rate:.1}x rate)"
                    ));
                }
            }
        }
    }

    // Check for empty scopes retaining data
    for scope in &diagnostics.empty_scopes {
        // Find the scope's filter count from a previous measurement
        let scope_key = format!("{scope:?}");
        if let Some(metrics) = previous.scope_metrics.get(&scope_key) {
            if metrics.filter_count > 0 {
                warnings.push(format!(
                    "Scope '{}' has 0 connections but {} filters",
                    scope_name(scope),
                    metrics.filter_count
                ));
            }
        }
    }

    warnings
}

fn scope_name(scope: &Scope) -> String {
    match scope {
        Scope::Default => "Default".to_string(),
        Scope::Named { name, .. } => name.clone(),
    }
}
