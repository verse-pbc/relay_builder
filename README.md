# Nostr Relay Builder

A high-performance framework for building custom Nostr relays in Rust.

## Features

- 🚀 **Fast** - Built on async Rust with Tokio
- 🔌 **Extensible** - Pluggable business logic via EventProcessor  
- 🛡️ **Secure** - Automatic signature verification and error handling
- 📦 **Batteries Included** - Built-in NIPs: 09 (deletion), 40 (expiration), 42 (auth), 70 (protected)
- 🏢 **Multi-tenant** - Subdomain isolation for SaaS deployments
- 📊 **Observable** - Metrics, monitoring, and graceful shutdown

## Quick Start

```toml
[dependencies]
nostr_relay_builder = { git = "https://github.com/verse-pbc/nostr_relay_builder", features = ["axum"] }
```

```rust
use anyhow::Result;
use axum::{routing::get, Router};
use nostr_relay_builder::{RelayBuilder, RelayConfig, RelayInfo};
use nostr_sdk::prelude::*;
use std::net::SocketAddr;

#[tokio::main]
async fn main() -> Result<()> {
    // Configure relay
    let keys = Keys::generate();
    let config = RelayConfig::new("ws://localhost:8080", "./data", keys);
    
    let relay_info = RelayInfo {
        name: "My Relay".to_string(),
        description: "A simple Nostr relay".to_string(),
        ..Default::default()
    };
    
    // Build with default settings (accepts all valid events)
    let handler = RelayBuilder::new(config)
        .with_relay_info(relay_info)
        .build_axum()
        .await?;
    
    // Serve with Axum
    let app = Router::new().route("/", get(handler));
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    
    println!("🚀 Relay running at ws://localhost:8080");
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
```

## Learn More

- **[📚 Tutorial](./examples/README.md)** - Step-by-step guide with 9 progressive examples
- **[📂 Examples](./examples/)** - Working code for each tutorial step
- **[🔧 API Docs](https://docs.rs/nostr_relay_builder)** - Detailed documentation

## Production Usage

This framework powers:
- [groups_relay](https://github.com/verse-pbc/groups_relay) - NIP-29 group chat relay
- [profile_aggregator](https://github.com/verse-pbc/profile_aggregator) - Profile aggregation service

## License

MIT