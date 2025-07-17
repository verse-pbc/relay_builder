use anyhow::{Context, Result};
use clap::Parser;
use heed::types::*;
use heed::{Env, EnvOpenOptions, RoTxn};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::str;
use tracing::{debug, error, info, warn};

#[derive(Parser)]
#[command(name = "nostr-lmdb-integrity")]
#[command(about = "Check and repair LMDB database integrity for nostr-lmdb")]
struct Args {
    #[arg(short, long, help = "Database directory path")]
    db_path: PathBuf,

    #[arg(
        short,
        long,
        help = "Wet run mode - actually remove corrupted entries after confirmation"
    )]
    wet_run: bool,

    #[arg(
        short,
        long,
        help = "Show detailed information about each corrupted entry"
    )]
    verbose: bool,
}

#[derive(Debug)]
struct CorruptedEntry {
    database: String,
    key: Vec<u8>,
    event_id: String,
    index_type: String,
    scope: Option<String>,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let args = Args::parse();

    info!("Opening database at: {:?}", args.db_path);

    let env = unsafe {
        EnvOpenOptions::new()
            .map_size(1024 * 1024 * 1024) // 1GB
            .max_dbs(100)
            .open(&args.db_path)
            .with_context(|| format!("Failed to open database at {:?}", args.db_path))?
    };

    let rtxn = env.read_txn()?;

    // Discover all databases and check which ones are expected
    let all_databases = discover_databases(&env, &rtxn)?;
    let mut expected_databases = Vec::new();
    let mut unexpected_databases = Vec::new();

    for db_name in all_databases {
        if is_expected_database(&db_name) {
            expected_databases.push(db_name);
        } else {
            unexpected_databases.push(db_name);
        }
    }

    info!("Found {} expected databases", expected_databases.len());
    if !unexpected_databases.is_empty() {
        warn!(
            "Found {} unexpected databases: {:?}",
            unexpected_databases.len(),
            unexpected_databases
        );
        println!(
            "⚠️  Found {} unexpected databases:",
            unexpected_databases.len()
        );
        for db_name in &unexpected_databases {
            println!("   - {db_name}");
        }
        println!("   These databases are not part of the nostr-lmdb schema.");
        println!("   To clean up the database completely, use the export_import tool to migrate to a fresh database.");
        println!();
    }

    // Check integrity of expected databases
    let corrupted_entries = check_integrity(&env, &rtxn, &expected_databases, args.verbose)?;

    if corrupted_entries.is_empty() {
        println!("✅ Database integrity check passed! No corrupted entries found.");
        return Ok(());
    }

    println!("❌ Found {} corrupted entries:", corrupted_entries.len());

    // Group by database for cleaner output
    let mut by_database: HashMap<String, Vec<&CorruptedEntry>> = HashMap::new();
    for entry in &corrupted_entries {
        by_database
            .entry(entry.database.clone())
            .or_default()
            .push(entry);
    }

    for (db_name, entries) in by_database {
        println!(
            "\n📊 Database '{}': {} corrupted entries",
            db_name,
            entries.len()
        );

        if args.verbose {
            for entry in entries {
                println!("   - Event ID: {}", entry.event_id);
                println!("     Index type: {}", entry.index_type);
                if let Some(scope) = &entry.scope {
                    println!("     Scope: {scope}");
                }
                println!("     Key: {}", hex::encode(&entry.key));
            }
        } else {
            // Show first few entries
            for entry in entries.iter().take(3) {
                print!("   - {} ({})", entry.event_id, entry.index_type);
                if let Some(scope) = &entry.scope {
                    print!(" [scope: {scope}]");
                }
                println!();
            }
            if entries.len() > 3 {
                println!("   ... and {} more", entries.len() - 3);
            }
        }
    }

    if args.wet_run {
        println!("\n🔧 Wet run mode enabled. The corrupted entries listed above will be removed.");
        println!("⚠️  This operation cannot be undone!");
        println!("Do you want to continue? (y/N)");

        let mut input = String::new();
        std::io::stdin().read_line(&mut input)?;

        if input.trim().to_lowercase() != "y" {
            println!("Operation cancelled.");
            return Ok(());
        }

        remove_corrupted_entries(&env, &corrupted_entries)?;
        println!("✅ Removed {} corrupted entries", corrupted_entries.len());
    } else {
        println!("\n💡 Run with --wet-run to remove these corrupted entries.");
        println!(
            "   Note: This will only remove corrupted index entries, not unexpected databases."
        );
        println!("   To get a completely clean database, use the export_import tool.");
    }

    Ok(())
}

fn discover_databases(env: &Env, rtxn: &RoTxn) -> Result<Vec<String>> {
    let mut databases = Vec::new();

    // Try common database names
    let common_names = [
        "events",
        "events_scoped",
        "ci",
        "ci_scoped",
        "tci",
        "tci_scoped",
        "aci",
        "aci_scoped",
        "akci",
        "akci_scoped",
        "atci",
        "atci_scoped",
        "ktci",
        "ktci_scoped",
        "deleted-ids",
        "deleted-ids_scoped",
        "deleted-coordinates",
        "deleted-coordinates_scoped",
        "__global_scope_metadata",
    ];

    for name in &common_names {
        if env
            .open_database::<Bytes, Bytes>(rtxn, Some(name))?
            .is_some()
        {
            databases.push(name.to_string());
        }
    }

    // Check for unnamed database
    if env.open_database::<Bytes, Bytes>(rtxn, None)?.is_some() {
        databases.push("(unnamed)".to_string());
    }

    // Try to discover other databases by iterating through possible names
    // This is a heuristic approach since LMDB doesn't provide a direct way to list all databases
    for i in 0..100 {
        let test_name = format!("unknown_db_{i}");
        if env
            .open_database::<Bytes, Bytes>(rtxn, Some(&test_name))?
            .is_some()
        {
            databases.push(test_name);
        }
    }

    Ok(databases)
}

fn is_expected_database(name: &str) -> bool {
    matches!(
        name,
        "events"
            | "events_scoped"
            | "ci"
            | "ci_scoped"
            | "tci"
            | "tci_scoped"
            | "aci"
            | "aci_scoped"
            | "akci"
            | "akci_scoped"
            | "atci"
            | "atci_scoped"
            | "ktci"
            | "ktci_scoped"
            | "deleted-ids"
            | "deleted-ids_scoped"
            | "deleted-coordinates"
            | "deleted-coordinates_scoped"
            | "__global_scope_metadata"
            | "(unnamed)"
    )
}

fn check_integrity(
    env: &Env,
    rtxn: &RoTxn,
    databases: &[String],
    verbose: bool,
) -> Result<Vec<CorruptedEntry>> {
    let mut corrupted = Vec::new();
    let scope_registry = discover_scope_registry(env, rtxn)?;

    // Get all existing event IDs for verification
    let existing_events = get_all_event_ids(env, rtxn)?;

    info!(
        "Checking integrity against {} existing events",
        existing_events.len()
    );

    for db_name in databases {
        if db_name == "events" || db_name == "events_scoped" || db_name == "__global_scope_metadata"
        {
            continue; // Skip event storage databases
        }

        debug!("Checking database: {}", db_name);

        let db_opt = if db_name == "(unnamed)" {
            env.open_database::<Bytes, Bytes>(rtxn, None)?
        } else {
            env.open_database::<Bytes, Bytes>(rtxn, Some(db_name))?
        };

        let db = match db_opt {
            Some(db) => db,
            None => continue,
        };

        let mut db_corrupted = 0;
        let iter = db.iter(rtxn)?;

        for result in iter {
            let (key, _value) = result?;

            if let Some(event_id) = extract_event_id_from_key(key, db_name, &scope_registry) {
                // Check if this event ID exists in the events databases
                if !existing_events.contains(&event_id.event_id) {
                    corrupted.push(CorruptedEntry {
                        database: db_name.clone(),
                        key: key.to_vec(),
                        event_id: event_id.event_id,
                        index_type: event_id.index_type,
                        scope: event_id.scope,
                    });
                    db_corrupted += 1;
                }
            }
        }

        if db_corrupted > 0 && verbose {
            info!("Database '{}': {} corrupted entries", db_name, db_corrupted);
        }
    }

    Ok(corrupted)
}

fn discover_scope_registry(env: &Env, rtxn: &RoTxn) -> Result<HashMap<u32, String>> {
    let mut registry = HashMap::new();

    if let Some(scope_db) =
        env.open_database::<Bytes, Bytes>(rtxn, Some("__global_scope_metadata"))?
    {
        let iter = scope_db.iter(rtxn)?;
        for result in iter {
            let (key, value) = result?;

            if let Ok(scope_name) = str::from_utf8(key) {
                if value.len() == 4 {
                    let hash = u32::from_be_bytes([value[0], value[1], value[2], value[3]]);
                    registry.insert(hash, scope_name.to_string());
                }
            }
        }
    }

    debug!("Discovered {} scopes in registry", registry.len());
    Ok(registry)
}

fn get_all_event_ids(env: &Env, rtxn: &RoTxn) -> Result<HashSet<String>> {
    let mut event_ids = HashSet::new();

    // Check unnamed database
    if let Some(db) = env.open_database::<Bytes, Bytes>(rtxn, None)? {
        let iter = db.iter(rtxn)?;
        for result in iter {
            let (key, _value) = result?;
            if key.len() == 32 {
                event_ids.insert(hex::encode(key));
            }
        }
    }

    // Check events_scoped database
    if let Some(db) = env.open_database::<Bytes, Bytes>(rtxn, Some("events_scoped"))? {
        let iter = db.iter(rtxn)?;
        for result in iter {
            let (key, _value) = result?;

            // Parse scoped key to get original event ID
            if key.len() >= 12 {
                let key_len = u64::from_be_bytes([
                    key[4], key[5], key[6], key[7], key[8], key[9], key[10], key[11],
                ]);

                if key.len() == 12 + key_len as usize && key_len == 32 {
                    let event_id = &key[12..];
                    event_ids.insert(hex::encode(event_id));
                }
            }
        }
    }

    Ok(event_ids)
}

#[derive(Debug)]
struct EventIdInfo {
    event_id: String,
    index_type: String,
    scope: Option<String>,
}

fn extract_event_id_from_key(
    key: &[u8],
    db_name: &str,
    scope_registry: &HashMap<u32, String>,
) -> Option<EventIdInfo> {
    // Handle scoped databases
    let (scope_name, actual_key) = if db_name.ends_with("_scoped") {
        match parse_scoped_key(key, scope_registry) {
            Some((scope, inner_key)) => (Some(scope), inner_key),
            None => return None,
        }
    } else {
        (None, key)
    };

    let base_db = db_name.trim_end_matches("_scoped");

    match base_db {
        "ci" => extract_from_ci_key(actual_key, scope_name),
        "tci" => extract_from_tci_key(actual_key, scope_name),
        "aci" => extract_from_aci_key(actual_key, scope_name),
        "akci" => extract_from_akci_key(actual_key, scope_name),
        "atci" => extract_from_atci_key(actual_key, scope_name),
        "ktci" => extract_from_ktci_key(actual_key, scope_name),
        "deleted-ids" => extract_from_deleted_ids_key(actual_key, scope_name),
        "deleted-coordinates" => None, // Coordinates don't reference specific events
        _ => None,
    }
}

fn parse_scoped_key<'a>(
    key: &'a [u8],
    scope_registry: &HashMap<u32, String>,
) -> Option<(String, &'a [u8])> {
    if key.len() < 12 {
        return None;
    }

    let scope_hash = u32::from_be_bytes([key[0], key[1], key[2], key[3]]);
    let key_len = u64::from_be_bytes([
        key[4], key[5], key[6], key[7], key[8], key[9], key[10], key[11],
    ]);

    if key.len() != 12 + key_len as usize {
        return None;
    }

    let original_key = &key[12..];
    let scope_name = scope_registry
        .get(&scope_hash)
        .cloned()
        .unwrap_or_else(|| panic!("Unknown scope hash: {scope_hash:08x}"));

    Some((scope_name, original_key))
}

fn extract_from_ci_key(key: &[u8], scope: Option<String>) -> Option<EventIdInfo> {
    if key.len() >= 40 {
        let event_id = hex::encode(&key[8..40]);
        Some(EventIdInfo {
            event_id,
            index_type: "created_at_index".to_string(),
            scope,
        })
    } else {
        None
    }
}

fn extract_from_tci_key(key: &[u8], scope: Option<String>) -> Option<EventIdInfo> {
    if key.len() >= 226 {
        let event_id = hex::encode(&key[194..226]);
        Some(EventIdInfo {
            event_id,
            index_type: "tag_created_at_index".to_string(),
            scope,
        })
    } else {
        None
    }
}

fn extract_from_aci_key(key: &[u8], scope: Option<String>) -> Option<EventIdInfo> {
    if key.len() >= 72 {
        let event_id = hex::encode(&key[40..72]);
        Some(EventIdInfo {
            event_id,
            index_type: "author_created_at_index".to_string(),
            scope,
        })
    } else {
        None
    }
}

fn extract_from_akci_key(key: &[u8], scope: Option<String>) -> Option<EventIdInfo> {
    if key.len() >= 74 {
        let event_id = hex::encode(&key[42..74]);
        Some(EventIdInfo {
            event_id,
            index_type: "author_kind_created_at_index".to_string(),
            scope,
        })
    } else {
        None
    }
}

fn extract_from_atci_key(key: &[u8], scope: Option<String>) -> Option<EventIdInfo> {
    if key.len() >= 258 {
        let event_id = hex::encode(&key[226..258]);
        Some(EventIdInfo {
            event_id,
            index_type: "author_tag_created_at_index".to_string(),
            scope,
        })
    } else {
        None
    }
}

fn extract_from_ktci_key(key: &[u8], scope: Option<String>) -> Option<EventIdInfo> {
    if key.len() >= 228 {
        let event_id = hex::encode(&key[196..228]);
        Some(EventIdInfo {
            event_id,
            index_type: "kind_tag_created_at_index".to_string(),
            scope,
        })
    } else {
        None
    }
}

fn extract_from_deleted_ids_key(key: &[u8], scope: Option<String>) -> Option<EventIdInfo> {
    if key.len() == 32 {
        Some(EventIdInfo {
            event_id: hex::encode(key),
            index_type: "deleted_ids".to_string(),
            scope,
        })
    } else {
        None
    }
}

fn remove_corrupted_entries(env: &Env, corrupted_entries: &[CorruptedEntry]) -> Result<()> {
    let mut wtxn = env.write_txn()?;

    for entry in corrupted_entries {
        debug!(
            "Removing corrupted entry from {}: {}",
            entry.database, entry.event_id
        );

        let db_opt = if entry.database == "(unnamed)" {
            env.open_database::<Bytes, Bytes>(&wtxn, None)?
        } else {
            env.open_database::<Bytes, Bytes>(&wtxn, Some(&entry.database))?
        };

        if let Some(db) = db_opt {
            match db.delete(&mut wtxn, &entry.key) {
                Ok(deleted) => {
                    if deleted {
                        debug!("Successfully removed entry");
                    } else {
                        warn!("Entry not found when trying to delete");
                    }
                }
                Err(e) => {
                    error!("Failed to delete entry: {}", e);
                }
            }
        }
    }

    wtxn.commit()?;
    Ok(())
}
