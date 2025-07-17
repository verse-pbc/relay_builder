use anyhow::{Context, Result};
use clap::Parser;
use heed::types::*;
use heed::{Database, Env, EnvOpenOptions, RoTxn};
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
        Some(create_progress_bar(
            common_names.len() as u64 + 1,
            "{spinner:.green} Discovering databases... [{bar:40.cyan/blue}] {pos}/{len}",
        ))
    } else {
        None
    };

    for name in &common_names {
        if open_database(&env, &rtxn, Some(name))?.is_some() {
            db_names.push(name.to_string())
        }
        if let Some(pb) = &discovery_pb {
            pb.inc(1);
        }
    }

    // Try unnamed database
    if open_database(&env, &rtxn, None)?.is_some() {
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
    if let Some(scope_db) = open_database(env, rtxn, Some("__global_scope_metadata"))? {
        // Get the count for progress bar
        let stat = scope_db.stat(rtxn)?;
        let scope_count = stat.entries;

        let progress_pb = if show_progress && scope_count > 0 {
            Some(create_progress_bar(
                scope_count as u64,
                "{spinner:.green} Discovering scopes... [{bar:40.cyan/blue}] {pos}/{len}",
            ))
        } else {
            None
        };

        let iter = scope_db.iter(rtxn)?;
        for result in iter {
            let (key, value) = result?;

            // Keys are scope names, values are 4-byte hashes
            if let Ok(scope_name) = str::from_utf8(key) {
                if value.len() == 4 {
                    let hash = parse_u32_be(value);
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
        open_database(env, rtxn, None)?
    } else {
        open_database(env, rtxn, Some(db_name))?
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
        open_database(env, rtxn, None)?
    } else {
        open_database(env, rtxn, Some(db_name))?
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

        Some(create_progress_bar(
            total,
            &format!("{{spinner:.green}} Processing {db_name}... [{{bar:40.cyan/blue}}] {{pos}}/{{len}} entries"),
        ))
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
    let scope_hash = parse_u32_be(scope_hash_bytes);

    // Next 8 bytes: original key length
    let key_len_bytes = &key[4..12];
    let key_len = parse_timestamp_be(key_len_bytes);

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

        if key.len() >= 40 {
            let event_id = &key[8..40];
            Some(json!({
                "timestamp": format_timestamp_field(timestamp_bytes, true),
                "event_id": hex::encode(event_id)
            }))
        } else {
            Some(json!({
                "timestamp": format_timestamp_field(timestamp_bytes, true),
                "incomplete": true
            }))
        }
    } else {
        None
    }
}

fn parse_tci_key(key: &[u8]) -> Option<serde_json::Value> {
    if key.len() >= 194 {
        let tag_name = key[0];
        let tag_value_bytes = &key[1..183];
        let timestamp_bytes = &key[183..191];

        if key.len() >= 226 {
            let event_id = &key[194..226];
            Some(json!({
                "tag_name": format_tag_name(tag_name),
                "tag_value": format_tag_value(tag_value_bytes),
                "timestamp": format_timestamp_field(timestamp_bytes, true),
                "event_id": hex::encode(event_id)
            }))
        } else {
            Some(json!({
                "tag_name": format_tag_name(tag_name),
                "tag_value": format_tag_value(tag_value_bytes),
                "timestamp": format_timestamp_field(timestamp_bytes, true),
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

        if key.len() >= 72 {
            let event_id = &key[40..72];
            Some(json!({
                "pubkey": hex::encode(pubkey),
                "timestamp": format_timestamp_field(timestamp_bytes, true),
                "event_id": hex::encode(event_id)
            }))
        } else {
            Some(json!({
                "pubkey": hex::encode(pubkey),
                "timestamp": format_timestamp_field(timestamp_bytes, true),
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
        let timestamp_bytes = &key[34..42];

        if key.len() >= 74 {
            let event_id = &key[42..74];
            Some(json!({
                "pubkey": hex::encode(pubkey),
                "kind": format_kind_field(kind_bytes),
                "timestamp": format_timestamp_field(timestamp_bytes, true),
                "event_id": hex::encode(event_id)
            }))
        } else {
            Some(json!({
                "pubkey": hex::encode(pubkey),
                "kind": format_kind_field(kind_bytes),
                "timestamp": format_timestamp_field(timestamp_bytes, true),
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
        let tag_name = key[32];
        let tag_value_bytes = &key[33..215];
        let timestamp_bytes = &key[215..223];

        if key.len() >= 258 {
            let event_id = &key[226..258];
            Some(json!({
                "pubkey": hex::encode(pubkey),
                "tag_name": format_tag_name(tag_name),
                "tag_value": format_tag_value(tag_value_bytes),
                "timestamp": format_timestamp_field(timestamp_bytes, true),
                "event_id": hex::encode(event_id)
            }))
        } else {
            Some(json!({
                "pubkey": hex::encode(pubkey),
                "tag_name": format_tag_name(tag_name),
                "tag_value": format_tag_value(tag_value_bytes),
                "timestamp": format_timestamp_field(timestamp_bytes, true),
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
        let tag_name = key[2];
        let tag_value_bytes = &key[3..185];
        let timestamp_bytes = &key[185..193];

        if key.len() >= 228 {
            let event_id = &key[196..228];
            Some(json!({
                "kind": format_kind_field(kind_bytes),
                "tag_name": format_tag_name(tag_name),
                "tag_value": format_tag_value(tag_value_bytes),
                "timestamp": format_timestamp_field(timestamp_bytes, true),
                "event_id": hex::encode(event_id)
            }))
        } else {
            Some(json!({
                "kind": format_kind_field(kind_bytes),
                "tag_name": format_tag_name(tag_name),
                "tag_value": format_tag_value(tag_value_bytes),
                "timestamp": format_timestamp_field(timestamp_bytes, true),
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
        let pubkey = &key[2..34];
        let d_tag_bytes = if key.len() > 34 { &key[34..] } else { &[] };

        let mut result = json!({
            "kind": format_kind_field(kind_bytes),
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

// Helper functions to reduce duplication

fn open_database(
    env: &Env,
    rtxn: &RoTxn,
    name: Option<&str>,
) -> Result<Option<Database<Bytes, Bytes>>> {
    Ok(env.open_database::<Bytes, Bytes>(rtxn, name)?)
}

fn create_progress_bar(total: u64, template: &str) -> ProgressBar {
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::default_bar()
            .template(template)
            .unwrap()
            .progress_chars("#>-"),
    );
    pb
}

fn parse_timestamp_be(bytes: &[u8]) -> u64 {
    u64::from_be_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

fn parse_u16_be(bytes: &[u8]) -> u16 {
    u16::from_be_bytes([bytes[0], bytes[1]])
}

fn parse_u32_be(bytes: &[u8]) -> u32 {
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]])
}

fn format_timestamp_field(bytes: &[u8], reversed: bool) -> serde_json::Value {
    let timestamp = parse_timestamp_be(bytes);
    let parsed_value = if reversed {
        u64::MAX - timestamp
    } else {
        timestamp
    };

    json!({
        "raw_hex": hex::encode(bytes),
        "parsed_value": parsed_value
    })
}

fn format_tag_value(bytes: &[u8]) -> serde_json::Value {
    json!({
        "raw_hex": hex::encode(bytes),
        "parsed_value": String::from_utf8_lossy(bytes).trim_end_matches('\0').to_string()
    })
}

fn format_kind_field(bytes: &[u8]) -> serde_json::Value {
    json!({
        "raw_hex": hex::encode(bytes),
        "parsed_value": parse_u16_be(bytes)
    })
}

fn format_tag_name(byte: u8) -> serde_json::Value {
    json!({
        "raw_hex": hex::encode([byte]),
        "parsed_value": (byte as char).to_string()
    })
}
