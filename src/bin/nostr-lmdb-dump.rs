use anyhow::{Context, Result};
use clap::Parser;
use heed::types::*;
use heed::{Env, EnvOpenOptions, RoTxn};
use indicatif::{ProgressBar, ProgressStyle};
use serde_json::json;
use std::collections::HashMap;
use std::path::PathBuf;
use std::str;

#[derive(Parser)]
#[command(name = "nostr-lmdb-dump")]
#[command(about = "Dump and parse LMDB database contents with key interpretation")]
struct Args {
    #[arg(short, long, help = "Database directory path")]
    db_path: PathBuf,

    #[arg(
        short,
        long,
        help = "Optional database name to filter (shows all if not specified)"
    )]
    name: Option<String>,

    #[arg(short, long, help = "Show only key count per database")]
    count_only: bool,

    #[arg(short, long, help = "Limit number of entries to show per database")]
    limit: Option<usize>,

    #[arg(long, help = "Disable progress bar")]
    no_progress: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let env = unsafe {
        EnvOpenOptions::new()
            .map_size(1024 * 1024 * 1024) // 1GB
            .max_dbs(100)
            .open(&args.db_path)
            .with_context(|| format!("Failed to open database at {:?}", args.db_path))?
    };

    let rtxn = env.read_txn()?;

    // Get all database names
    let mut db_names = Vec::new();

    // Try to open databases by common names
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

    // Create progress bar for database discovery
    let discovery_pb = if !args.no_progress {
        let pb = ProgressBar::new(common_names.len() as u64 + 1);
        pb.set_style(
            ProgressStyle::default_bar()
                .template(
                    "{spinner:.green} Discovering databases... [{bar:40.cyan/blue}] {pos}/{len}",
                )
                .unwrap()
                .progress_chars("#>-"),
        );
        Some(pb)
    } else {
        None
    };

    for name in &common_names {
        if env
            .open_database::<Bytes, Bytes>(&rtxn, Some(name))?
            .is_some()
        {
            db_names.push(name.to_string())
        }
        if let Some(pb) = &discovery_pb {
            pb.inc(1);
        }
    }

    // Try unnamed database
    if env.open_database::<Bytes, Bytes>(&rtxn, None)?.is_some() {
        db_names.push("(unnamed)".to_string());
    }
    if let Some(pb) = &discovery_pb {
        pb.inc(1);
        pb.finish_and_clear();
    }

    // Discover scope registry for parsing
    let scope_registry = discover_scope_registry(&env, &rtxn, !args.no_progress)?;

    // Filter databases if name specified
    let filtered_names: Vec<String> = if let Some(filter) = &args.name {
        db_names
            .into_iter()
            .filter(|name| name.contains(filter))
            .collect()
    } else {
        db_names
    };

    // Start JSON output
    println!("{{");
    println!("  \"db_path\": {:?},", args.db_path.display());

    if args.count_only {
        // For count-only mode, show database summary
        println!("  \"database_counts\": {{");
        let mut first_db = true;
        for db_name in &filtered_names {
            if !first_db {
                println!(",");
            }
            first_db = false;
            print!("    \"{db_name}\": ");
            dump_database_count(&env, &rtxn, db_name)?;
        }
        println!();
        println!("  }}");
    } else {
        // For full dump mode, show flattened entries array
        println!("  \"entries\": [");

        let mut first_entry = true;
        for db_name in filtered_names {
            dump_database_entries(
                &env,
                &rtxn,
                &db_name,
                &scope_registry,
                &args,
                &mut first_entry,
            )?;
        }

        if !first_entry {
            println!();
        }
        println!("  ]");
    }

    println!("}}");

    Ok(())
}

fn discover_scope_registry(
    env: &Env,
    rtxn: &RoTxn,
    show_progress: bool,
) -> Result<HashMap<u32, String>> {
    let mut registry = HashMap::new();

    // Try to open the scope metadata database
    if let Some(scope_db) =
        env.open_database::<Bytes, Bytes>(rtxn, Some("__global_scope_metadata"))?
    {
        // Get the count for progress bar
        let stat = scope_db.stat(rtxn)?;
        let scope_count = stat.entries;

        let progress_pb = if show_progress && scope_count > 0 {
            let pb = ProgressBar::new(scope_count as u64);
            pb.set_style(
                ProgressStyle::default_bar()
                    .template(
                        "{spinner:.green} Discovering scopes... [{bar:40.cyan/blue}] {pos}/{len}",
                    )
                    .unwrap()
                    .progress_chars("#>-"),
            );
            Some(pb)
        } else {
            None
        };

        let iter = scope_db.iter(rtxn)?;
        for result in iter {
            let (key, value) = result?;

            // Keys are scope names, values are 4-byte hashes
            if let Ok(scope_name) = str::from_utf8(key) {
                if value.len() == 4 {
                    let hash = u32::from_be_bytes([value[0], value[1], value[2], value[3]]);
                    registry.insert(hash, scope_name.to_string());
                }
            }

            if let Some(pb) = &progress_pb {
                pb.inc(1);
            }
        }

        if let Some(pb) = &progress_pb {
            pb.finish_and_clear();
        }
    }

    Ok(registry)
}

fn dump_database_count(env: &Env, rtxn: &RoTxn, db_name: &str) -> Result<()> {
    let db_opt = if db_name == "(unnamed)" {
        env.open_database::<Bytes, Bytes>(rtxn, None)?
    } else {
        env.open_database::<Bytes, Bytes>(rtxn, Some(db_name))?
    };

    let db = match db_opt {
        Some(db) => db,
        None => {
            print!("0");
            return Ok(());
        }
    };

    let stat = db.stat(rtxn)?;
    print!("{}", stat.entries);

    Ok(())
}

fn dump_database_entries(
    env: &Env,
    rtxn: &RoTxn,
    db_name: &str,
    scope_registry: &HashMap<u32, String>,
    args: &Args,
    first_entry: &mut bool,
) -> Result<()> {
    let db_opt = if db_name == "(unnamed)" {
        env.open_database::<Bytes, Bytes>(rtxn, None)?
    } else {
        env.open_database::<Bytes, Bytes>(rtxn, Some(db_name))?
    };

    let db = match db_opt {
        Some(db) => db,
        None => return Ok(()),
    };

    let stat = db.stat(rtxn)?;
    let entry_count = stat.entries;

    if entry_count == 0 {
        return Ok(());
    }

    // Create progress bar for this database
    let progress_pb = if !args.no_progress && entry_count > 0 {
        let total = if let Some(limit) = args.limit {
            std::cmp::min(limit, entry_count) as u64
        } else {
            entry_count as u64
        };

        let pb = ProgressBar::new(total);
        pb.set_style(
            ProgressStyle::default_bar()
                .template(&format!("{{spinner:.green}} Processing {db_name}... [{{bar:40.cyan/blue}}] {{pos}}/{{len}} entries"))
                .unwrap()
                .progress_chars("#>-"),
        );
        Some(pb)
    } else {
        None
    };

    let iter = db.iter(rtxn)?;

    for (shown, result) in iter.enumerate() {
        let (key, value) = result?;

        if let Some(limit) = args.limit {
            if shown >= limit {
                break;
            }
        }

        if !*first_entry {
            println!(",");
        }
        *first_entry = false;

        // Build entry object
        print!("    {{");

        // Database name
        print!("\"database\": \"{db_name}\", ");

        // Key hex
        print!("\"key_hex\": \"{}\", ", hex::encode(key));

        // Value hex (truncated if large)
        if value.len() > 100 {
            print!("\"value_hex\": \"{}\", ", hex::encode(&value[..50]));
            print!("\"value_truncated\": true, ");
            print!("\"value_size_bytes\": {}", value.len());
        } else {
            print!("\"value_hex\": \"{}\"", hex::encode(value));
        }

        // Parse key if possible (but skip for events and deleted-ids databases)
        if db_name != "events"
            && db_name != "events_scoped"
            && db_name != "deleted-ids"
            && db_name != "deleted-ids_scoped"
        {
            if let Some(parsed) = parse_key(key, db_name, scope_registry) {
                print!(", \"parsed_key\": {}", serde_json::to_string(&parsed)?);
            }
        }

        print!("}}");

        if let Some(pb) = &progress_pb {
            pb.inc(1);
        }
    }

    if let Some(pb) = &progress_pb {
        pb.finish_and_clear();
    }

    Ok(())
}

fn parse_key(
    key: &[u8],
    db_name: &str,
    scope_registry: &HashMap<u32, String>,
) -> Option<serde_json::Value> {
    // Handle scoped databases
    if db_name.ends_with("_scoped") {
        match parse_scoped_key(key, scope_registry) {
            Some((scope_components, inner_key)) => {
                // Parse the inner key
                let mut parsed = match db_name.trim_end_matches("_scoped") {
                    "events" => parse_events_key(inner_key),
                    "ci" => parse_ci_key(inner_key),
                    "tci" => parse_tci_key(inner_key),
                    "aci" => parse_aci_key(inner_key),
                    "akci" => parse_akci_key(inner_key),
                    "atci" => parse_atci_key(inner_key),
                    "ktci" => parse_ktci_key(inner_key),
                    "deleted-ids" => parse_deleted_ids_key(inner_key),
                    "deleted-coordinates" => parse_deleted_coordinates_key(inner_key),
                    _ => return None,
                }?;

                // Prepend scope components to the beginning
                if let Some(obj) = parsed.as_object_mut() {
                    // Create new object with scope components first
                    let mut new_obj = serde_json::Map::new();
                    new_obj.insert(
                        "scope_hash".to_string(),
                        scope_components["scope_hash"].clone(),
                    );
                    new_obj.insert(
                        "original_key_length".to_string(),
                        scope_components["original_key_length"].clone(),
                    );

                    // Then add all other components
                    for (k, v) in obj.iter() {
                        new_obj.insert(k.clone(), v.clone());
                    }

                    return Some(json!(new_obj));
                }
                None
            }
            None => None,
        }
    } else {
        // Non-scoped databases
        match db_name {
            "events" => parse_events_key(key),
            "ci" => parse_ci_key(key),
            "tci" => parse_tci_key(key),
            "aci" => parse_aci_key(key),
            "akci" => parse_akci_key(key),
            "atci" => parse_atci_key(key),
            "ktci" => parse_ktci_key(key),
            "deleted-ids" => parse_deleted_ids_key(key),
            "deleted-coordinates" => parse_deleted_coordinates_key(key),
            "__global_scope_metadata" => parse_scope_metadata_key(key),
            _ => None,
        }
    }
}

fn parse_scoped_key<'a>(
    key: &'a [u8],
    scope_registry: &HashMap<u32, String>,
) -> Option<(serde_json::Value, &'a [u8])> {
    if key.len() < 12 {
        return None;
    }

    // First 4 bytes: scope hash
    let scope_hash_bytes = &key[0..4];
    let scope_hash = u32::from_be_bytes([key[0], key[1], key[2], key[3]]);

    // Next 8 bytes: original key length
    let key_len_bytes = &key[4..12];
    let key_len = u64::from_be_bytes([
        key[4], key[5], key[6], key[7], key[8], key[9], key[10], key[11],
    ]);

    // Remaining bytes: original key
    if key.len() != 12 + key_len as usize {
        return None;
    }

    let original_key = &key[12..];
    let scope_name = scope_registry
        .get(&scope_hash)
        .cloned()
        .unwrap_or_else(|| format!("unknown_scope_{scope_hash:08x}"));

    let scope_components = json!({
        "scope_hash": {
            "raw_hex": hex::encode(scope_hash_bytes),
            "lookup_value": scope_name
        },
        "original_key_length": {
            "raw_hex": hex::encode(key_len_bytes),
            "parsed_value": key_len
        }
    });

    Some((scope_components, original_key))
}

fn parse_events_key(key: &[u8]) -> Option<serde_json::Value> {
    if key.len() == 32 {
        Some(json!({
            "event_id": hex::encode(key)
        }))
    } else {
        None
    }
}

fn parse_ci_key(key: &[u8]) -> Option<serde_json::Value> {
    if key.len() >= 8 {
        let timestamp_bytes = &key[0..8];
        let timestamp = u64::from_be_bytes([
            key[0], key[1], key[2], key[3], key[4], key[5], key[6], key[7],
        ]);

        let reversed_timestamp = u64::MAX - timestamp;

        if key.len() >= 40 {
            let event_id = &key[8..40];
            Some(json!({
                "timestamp": {
                    "raw_hex": hex::encode(timestamp_bytes),
                    "parsed_value": reversed_timestamp
                },
                "event_id": hex::encode(event_id)
            }))
        } else {
            Some(json!({
                "timestamp": {
                    "raw_hex": hex::encode(timestamp_bytes),
                    "parsed_value": reversed_timestamp
                },
                "incomplete": true
            }))
        }
    } else {
        None
    }
}

fn parse_tci_key(key: &[u8]) -> Option<serde_json::Value> {
    if key.len() >= 194 {
        let tag_name_byte = &key[0..1];
        let tag_name = key[0] as char;
        let tag_value_bytes = &key[1..183];
        let timestamp_bytes = &key[183..191];
        let timestamp = u64::from_be_bytes([
            key[183], key[184], key[185], key[186], key[187], key[188], key[189], key[190],
        ]);

        let reversed_timestamp = u64::MAX - timestamp;

        if key.len() >= 226 {
            let event_id = &key[194..226];
            Some(json!({
                "tag_name": {
                    "raw_hex": hex::encode(tag_name_byte),
                    "parsed_value": tag_name.to_string()
                },
                "tag_value": {
                    "raw_hex": hex::encode(tag_value_bytes),
                    "parsed_value": String::from_utf8_lossy(tag_value_bytes).trim_end_matches('\0').to_string()
                },
                "timestamp": {
                    "raw_hex": hex::encode(timestamp_bytes),
                    "parsed_value": reversed_timestamp
                },
                "event_id": hex::encode(event_id)
            }))
        } else {
            Some(json!({
                "tag_name": {
                    "raw_hex": hex::encode(tag_name_byte),
                    "parsed_value": tag_name.to_string()
                },
                "tag_value": {
                    "raw_hex": hex::encode(tag_value_bytes),
                    "parsed_value": String::from_utf8_lossy(tag_value_bytes).trim_end_matches('\0').to_string()
                },
                "timestamp": {
                    "raw_hex": hex::encode(timestamp_bytes),
                    "parsed_value": reversed_timestamp
                },
                "incomplete": true
            }))
        }
    } else {
        None
    }
}

fn parse_aci_key(key: &[u8]) -> Option<serde_json::Value> {
    if key.len() >= 40 {
        let pubkey = &key[0..32];
        let timestamp_bytes = &key[32..40];
        let timestamp = u64::from_be_bytes([
            key[32], key[33], key[34], key[35], key[36], key[37], key[38], key[39],
        ]);

        let reversed_timestamp = u64::MAX - timestamp;

        if key.len() >= 72 {
            let event_id = &key[40..72];
            Some(json!({
                "pubkey": hex::encode(pubkey),
                "timestamp": {
                    "raw_hex": hex::encode(timestamp_bytes),
                    "parsed_value": reversed_timestamp
                },
                "event_id": hex::encode(event_id)
            }))
        } else {
            Some(json!({
                "pubkey": hex::encode(pubkey),
                "timestamp": {
                    "raw_hex": hex::encode(timestamp_bytes),
                    "parsed_value": reversed_timestamp
                },
                "incomplete": true
            }))
        }
    } else {
        None
    }
}

fn parse_akci_key(key: &[u8]) -> Option<serde_json::Value> {
    if key.len() >= 42 {
        let pubkey = &key[0..32];
        let kind_bytes = &key[32..34];
        let kind = u16::from_be_bytes([key[32], key[33]]);
        let timestamp_bytes = &key[34..42];
        let timestamp = u64::from_be_bytes([
            key[34], key[35], key[36], key[37], key[38], key[39], key[40], key[41],
        ]);

        let reversed_timestamp = u64::MAX - timestamp;

        if key.len() >= 74 {
            let event_id = &key[42..74];
            Some(json!({
                "pubkey": hex::encode(pubkey),
                "kind": {
                    "raw_hex": hex::encode(kind_bytes),
                    "parsed_value": kind
                },
                "timestamp": {
                    "raw_hex": hex::encode(timestamp_bytes),
                    "parsed_value": reversed_timestamp
                },
                "event_id": hex::encode(event_id)
            }))
        } else {
            Some(json!({
                "pubkey": hex::encode(pubkey),
                "kind": {
                    "raw_hex": hex::encode(kind_bytes),
                    "parsed_value": kind
                },
                "timestamp": {
                    "raw_hex": hex::encode(timestamp_bytes),
                    "parsed_value": reversed_timestamp
                },
                "incomplete": true
            }))
        }
    } else {
        None
    }
}

fn parse_atci_key(key: &[u8]) -> Option<serde_json::Value> {
    if key.len() >= 226 {
        let pubkey = &key[0..32];
        let tag_name_byte = &key[32..33];
        let tag_name = key[32] as char;
        let tag_value_bytes = &key[33..215];
        let timestamp_bytes = &key[215..223];
        let timestamp = u64::from_be_bytes([
            key[215], key[216], key[217], key[218], key[219], key[220], key[221], key[222],
        ]);

        let reversed_timestamp = u64::MAX - timestamp;

        if key.len() >= 258 {
            let event_id = &key[226..258];
            Some(json!({
                "pubkey": hex::encode(pubkey),
                "tag_name": {
                    "raw_hex": hex::encode(tag_name_byte),
                    "parsed_value": tag_name.to_string()
                },
                "tag_value": {
                    "raw_hex": hex::encode(tag_value_bytes),
                    "parsed_value": String::from_utf8_lossy(tag_value_bytes).trim_end_matches('\0').to_string()
                },
                "timestamp": {
                    "raw_hex": hex::encode(timestamp_bytes),
                    "parsed_value": reversed_timestamp
                },
                "event_id": hex::encode(event_id)
            }))
        } else {
            Some(json!({
                "pubkey": hex::encode(pubkey),
                "tag_name": {
                    "raw_hex": hex::encode(tag_name_byte),
                    "parsed_value": tag_name.to_string()
                },
                "tag_value": {
                    "raw_hex": hex::encode(tag_value_bytes),
                    "parsed_value": String::from_utf8_lossy(tag_value_bytes).trim_end_matches('\0').to_string()
                },
                "timestamp": {
                    "raw_hex": hex::encode(timestamp_bytes),
                    "parsed_value": reversed_timestamp
                },
                "incomplete": true
            }))
        }
    } else {
        None
    }
}

fn parse_ktci_key(key: &[u8]) -> Option<serde_json::Value> {
    if key.len() >= 196 {
        let kind_bytes = &key[0..2];
        let kind = u16::from_be_bytes([key[0], key[1]]);
        let tag_name_byte = &key[2..3];
        let tag_name = key[2] as char;
        let tag_value_bytes = &key[3..185];
        let timestamp_bytes = &key[185..193];
        let timestamp = u64::from_be_bytes([
            key[185], key[186], key[187], key[188], key[189], key[190], key[191], key[192],
        ]);

        let reversed_timestamp = u64::MAX - timestamp;

        if key.len() >= 228 {
            let event_id = &key[196..228];
            Some(json!({
                "kind": {
                    "raw_hex": hex::encode(kind_bytes),
                    "parsed_value": kind
                },
                "tag_name": {
                    "raw_hex": hex::encode(tag_name_byte),
                    "parsed_value": tag_name.to_string()
                },
                "tag_value": {
                    "raw_hex": hex::encode(tag_value_bytes),
                    "parsed_value": String::from_utf8_lossy(tag_value_bytes).trim_end_matches('\0').to_string()
                },
                "timestamp": {
                    "raw_hex": hex::encode(timestamp_bytes),
                    "parsed_value": reversed_timestamp
                },
                "event_id": hex::encode(event_id)
            }))
        } else {
            Some(json!({
                "kind": {
                    "raw_hex": hex::encode(kind_bytes),
                    "parsed_value": kind
                },
                "tag_name": {
                    "raw_hex": hex::encode(tag_name_byte),
                    "parsed_value": tag_name.to_string()
                },
                "tag_value": {
                    "raw_hex": hex::encode(tag_value_bytes),
                    "parsed_value": String::from_utf8_lossy(tag_value_bytes).trim_end_matches('\0').to_string()
                },
                "timestamp": {
                    "raw_hex": hex::encode(timestamp_bytes),
                    "parsed_value": reversed_timestamp
                },
                "incomplete": true
            }))
        }
    } else {
        None
    }
}

fn parse_deleted_ids_key(key: &[u8]) -> Option<serde_json::Value> {
    if key.len() == 32 {
        Some(json!({
            "event_id": hex::encode(key)
        }))
    } else {
        None
    }
}

fn parse_deleted_coordinates_key(key: &[u8]) -> Option<serde_json::Value> {
    if key.len() >= 34 {
        let kind_bytes = &key[0..2];
        let kind = u16::from_be_bytes([key[0], key[1]]);
        let pubkey = &key[2..34];
        let d_tag_bytes = if key.len() > 34 { &key[34..] } else { &[] };

        let mut result = json!({
            "kind": {
                "raw_hex": hex::encode(kind_bytes),
                "parsed_value": kind
            },
            "pubkey": hex::encode(pubkey)
        });

        if !d_tag_bytes.is_empty() {
            result["d_tag"] = json!({
                "raw_hex": hex::encode(d_tag_bytes),
                "parsed_value": String::from_utf8_lossy(d_tag_bytes).into_owned()
            });
        }

        Some(result)
    } else {
        None
    }
}

fn parse_scope_metadata_key(key: &[u8]) -> Option<serde_json::Value> {
    if let Ok(scope_name) = str::from_utf8(key) {
        Some(json!({
            "scope_name": scope_name
        }))
    } else {
        None
    }
}
