//! Negentropy Sync Binary
//!
//! This binary performs relay-to-relay synchronization using the Negentropy protocol.
//!
//! ## Usage Examples
//!
//! ### 1. Start Relay 1 (Terminal 1)
//! ```bash
//! RELAY_PORT=8080 RELAY_DB_PATH="./test_relay1_db" RELAY_NAME="Relay-1" \
//!     cargo run --example configurable_relay --features axum
//! ```
//!
//! ### 2. Start Relay 2 (Terminal 2)
//! ```bash
//! RELAY_PORT=8081 RELAY_DB_PATH="./test_relay2_db" RELAY_NAME="Relay-2" \
//!     cargo run --example configurable_relay --features axum
//! ```
//!
//! ### 3. Start Continuous Sync (Terminal 3)
//! ```bash
//! cargo run --bin negentropy_sync -- \
//!     --db ./test_relay1_db \
//!     --relay ws://localhost:8081 \
//!     --mode both \
//!     --interval 10 \
//!     --verbose
//! ```
//!
//! ### 4. Test Events (Terminal 4)
//! ```bash
//! # Send event to Relay 1
//! nak event -c "Test message from Relay 1" ws://localhost:8080
//!
//! # Wait 12 seconds for sync, then check Relay 2
//! sleep 12
//! nak req -k 1 ws://localhost:8081
//!
//! # Send event to Relay 2
//! nak event -c "Test message from Relay 2" ws://localhost:8081
//!
//! # Wait 12 seconds for sync, then check Relay 1
//! sleep 12
//! nak req -k 1 ws://localhost:8080
//! ```
//!
//! ## Key Parameters:
//! - `--db`: Path to local database (Relay 1's database)
//! - `--relay`: Remote relay WebSocket URL
//! - `--mode`: `both` (bidirectional), `push` (local→remote), `pull` (remote→local)
//! - `--interval`: Sync frequency in seconds (0 for one-time sync)
//! - `--verbose`: Enable detailed logging
//!
//! The sync process will run continuously every N seconds, automatically
//! synchronizing events between the two relays.
//!
//! The implementation uses nostr-sdk's built-in Negentropy sync functionality.

use anyhow::{Context, Result};
use clap::{Parser, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};
use nostr_lmdb::{NostrLmdb, Scope};
use nostr_sdk::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::time::{interval, sleep};
use tracing::{error, info, warn};

#[derive(Parser, Debug)]
#[command(
    name = "negentropy-sync",
    version = "0.1.0",
    about = "Sync Nostr events between relays using Negentropy protocol"
)]
struct Args {
    /// Path to the local LMDB database directory
    #[arg(short, long)]
    db: PathBuf,

    /// Remote relay URL(s) to sync with
    #[arg(short, long, value_delimiter = ',')]
    relay: Vec<String>,

    /// Nostr filter in JSON format (default: all events)
    #[arg(short, long)]
    filter: Option<String>,

    /// Sync interval in seconds (0 for one-time sync)
    #[arg(short, long, default_value = "0")]
    interval: u64,

    /// Database scope name (optional, for multi-tenant setups)
    #[arg(short, long)]
    scope: Option<String>,

    /// Authentication key (nsec or hex) for NIP-42 auth
    #[arg(short, long)]
    auth: Option<String>,

    /// Sync mode
    #[arg(short, long, value_enum, default_value = "both")]
    mode: SyncMode,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Connection timeout in seconds
    #[arg(long, default_value = "30")]
    timeout: u64,
}

#[derive(Clone, Debug, ValueEnum)]
enum SyncMode {
    /// Pull events from remote to local
    Pull,
    /// Push events from local to remote
    Push,
    /// Bidirectional sync
    Both,
}

struct SyncStats {
    events_sent: u64,
    events_received: u64,
    errors: u64,
    duration: Duration,
}

struct NegentropySync {
    local_db: Arc<NostrLmdb>,
    scope: Scope,
    filter: Filter,
}

impl NegentropySync {
    fn new(local_db: Arc<NostrLmdb>, scope: Scope, filter: Filter) -> Self {
        Self {
            local_db,
            scope,
            filter,
        }
    }

    async fn sync_with_relay(
        &self,
        relay_url: &str,
        mode: SyncMode,
        auth_keys: Option<Keys>,
    ) -> Result<SyncStats> {
        let start_time = std::time::Instant::now();
        let mut stats = SyncStats {
            events_sent: 0,
            events_received: 0,
            errors: 0,
            duration: Duration::default(),
        };

        info!("Connecting to relay: {}", relay_url);

        // Create client with authentication if provided and use our local database
        // NOTE: nostr-sdk doesn't support scoped databases directly, so the sync
        // will operate on the entire database, not just the specified scope.
        // TODO: Implement scope-aware sync by wrapping the database
        if self.scope != Scope::Default {
            warn!("Scope-aware sync not yet implemented. Syncing entire database.");
        }

        let mut client_builder = Client::builder().database(self.local_db.clone());
        if let Some(ref keys) = auth_keys {
            client_builder = client_builder.signer(keys.clone());
        }
        let client = client_builder.build();

        // Add relay
        client.add_relay(relay_url).await?;
        client.connect().await;

        // Wait for connection
        sleep(Duration::from_secs(2)).await;

        info!("Starting Negentropy sync with {}", relay_url);

        // Use nostr-sdk's built-in Negentropy sync with appropriate direction
        let sync_direction = match mode {
            SyncMode::Pull => SyncDirection::Down,
            SyncMode::Push => SyncDirection::Up,
            SyncMode::Both => SyncDirection::Both,
        };
        let sync_opts = SyncOptions::default().direction(sync_direction);
        let output = client.sync(self.filter.clone(), &sync_opts).await?;

        // Process results based on sync mode
        match mode {
            SyncMode::Pull => {
                // We only care about events we received from remote
                stats.events_received = output.received.len() as u64;
                info!(
                    "Pull sync complete: received {} events from remote",
                    stats.events_received
                );
            }
            SyncMode::Push => {
                // We only care about events we sent to remote
                stats.events_sent = output.sent.len() as u64;
                info!(
                    "Push sync complete: sent {} events to remote",
                    stats.events_sent
                );
            }
            SyncMode::Both => {
                // Bidirectional sync - count both sent and received
                stats.events_sent = output.sent.len() as u64;
                stats.events_received = output.received.len() as u64;
                info!(
                    "Bidirectional sync complete: sent {} events, received {} events",
                    stats.events_sent, stats.events_received
                );
            }
        }

        // Count any failures as errors
        for (_url, failures) in output.send_failures.iter() {
            stats.errors += failures.len() as u64;
            for (event_id, error) in failures {
                warn!("Failed to send event {}: {}", event_id, error);
            }
        }

        // Disconnect
        client.disconnect().await;

        stats.duration = start_time.elapsed();
        Ok(stats)
    }
}

fn setup_tracing(verbose: bool) {
    use tracing_subscriber::{fmt, EnvFilter};

    let env_filter = if verbose {
        EnvFilter::new("debug,negentropy_sync=trace")
    } else {
        EnvFilter::try_from_default_env()
            .unwrap_or_else(|_| EnvFilter::new("info,negentropy_sync=debug"))
    };

    fmt()
        .with_env_filter(env_filter)
        .with_timer(fmt::time::SystemTime)
        .with_target(true)
        .with_thread_ids(false)
        .with_thread_names(false)
        .with_file(false)
        .with_line_number(false)
        .with_level(true)
        .init();
}

async fn run_sync(args: &Args) -> Result<()> {
    // Open local database
    info!("Opening local database at {:?}", args.db);
    let local_db = Arc::new(
        NostrLmdb::open(&args.db)
            .await
            .with_context(|| format!("Failed to open database at {:?}", args.db))?,
    );

    // Parse scope
    let scope = match &args.scope {
        Some(name) => Scope::named(name)?,
        None => Scope::Default,
    };

    // Parse filter
    let filter = match &args.filter {
        Some(json) => Filter::from_json(json)?,
        None => Filter::new(),
    };

    info!("Using filter: {:?}", filter);
    info!("Using scope: {:?}", scope);

    // Parse authentication keys if provided
    let auth_keys = args.auth.as_ref().map(|key| {
        if key.starts_with("nsec") {
            Keys::parse(key).expect("Invalid nsec key")
        } else {
            let secret_key = SecretKey::from_hex(key).expect("Invalid hex key");
            Keys::new(secret_key)
        }
    });

    // Create sync instance
    let sync = NegentropySync::new(local_db, scope, filter);

    // Create progress bar
    let progress = ProgressBar::new(args.relay.len() as u64);
    progress.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} relays")?
            .progress_chars("#>-"),
    );

    let mut total_stats = SyncStats {
        events_sent: 0,
        events_received: 0,
        errors: 0,
        duration: Duration::default(),
    };

    // Sync with each relay
    for relay_url in &args.relay {
        info!("Syncing with relay: {}", relay_url);

        match sync
            .sync_with_relay(relay_url, args.mode.clone(), auth_keys.clone())
            .await
        {
            Ok(stats) => {
                info!(
                    "Sync with {} complete: {} sent, {} received, {} errors in {:?}",
                    relay_url,
                    stats.events_sent,
                    stats.events_received,
                    stats.errors,
                    stats.duration
                );
                total_stats.events_sent += stats.events_sent;
                total_stats.events_received += stats.events_received;
                total_stats.errors += stats.errors;
            }
            Err(e) => {
                error!("Failed to sync with {}: {}", relay_url, e);
                total_stats.errors += 1;
            }
        }

        progress.inc(1);
    }

    progress.finish_with_message("Sync complete");

    info!(
        "Total sync stats: {} events sent, {} events received, {} errors",
        total_stats.events_sent, total_stats.events_received, total_stats.errors
    );

    Ok(())
}

async fn run_periodic_sync(args: Args) -> Result<()> {
    let mut sync_interval = interval(Duration::from_secs(args.interval));
    let mut backoff = Duration::from_secs(10);
    let max_backoff = Duration::from_secs(300); // 5 minutes

    loop {
        sync_interval.tick().await;

        info!("Starting periodic sync run");

        match run_sync(&args).await {
            Ok(_) => {
                info!("Periodic sync completed successfully");
                // Reset backoff on success
                backoff = Duration::from_secs(args.interval.max(10));
            }
            Err(e) => {
                error!("Periodic sync failed: {}", e);
                // Increase backoff on failure
                backoff = (backoff * 2).min(max_backoff);
                warn!("Backing off for {:?} before retry", backoff);
                sleep(backoff).await;
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    setup_tracing(args.verbose);

    info!("Nostr Negentropy Sync Tool v{}", env!("CARGO_PKG_VERSION"));

    if args.interval > 0 {
        // Continuous sync mode
        info!(
            "Running in continuous sync mode with interval: {} seconds",
            args.interval
        );
        run_periodic_sync(args).await
    } else {
        // One-time sync mode
        info!("Running one-time sync");
        run_sync(&args).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_args() {
        let args = Args::parse_from([
            "negentropy_sync",
            "--db",
            "/tmp/test.db",
            "--relay",
            "wss://relay.example.com",
        ]);

        assert_eq!(args.db, PathBuf::from("/tmp/test.db"));
        assert_eq!(args.relay, vec!["wss://relay.example.com"]);
        assert_eq!(args.interval, 0);
    }

    #[test]
    fn test_multiple_relays() {
        let args = Args::parse_from([
            "negentropy_sync",
            "--db",
            "/tmp/test.db",
            "--relay",
            "wss://relay1.com,wss://relay2.com",
        ]);

        assert_eq!(args.relay.len(), 2);
    }
}
