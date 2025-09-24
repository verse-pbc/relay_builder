# Relay Builder

A framework for building custom Nostr relays in Rust.

> **⚠️ Alpha Software**: This crate is under active development (v0.5.0-alpha.1). Breaking changes are expected between releases as we refine the API based on user feedback and implementation experience.

## Features

- Built on async Rust with Tokio
- Pluggable business logic via EventProcessor trait
- Custom middleware support with static dispatch (zero-cost abstractions)
- Automatic signature verification and error handling
- Built-in NIPs: 09 (deletion), 40 (expiration), 42 (auth), 70 (protected)
- Rate limiting middleware with configurable limits
- Subdomain isolation for multi-tenant deployments
- Metrics, monitoring, and graceful shutdown
- Integrated WebSocket support via internal websocket module

## Quick Start

```toml
[dependencies]
relay_builder = { git = "https://github.com/verse-pbc/relay_builder" }
```

```rust
use anyhow::Result;
use axum::{routing::get, Router};
use relay_builder::{RelayBuilder, RelayConfig};
use relay_builder::handlers::RelayInfo;
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
        .relay_info(relay_info)
        .build()
        .await?;
    
    // Serve with Axum
    let app = Router::new().route("/", get(handler));
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    
    println!("Relay running at ws://localhost:8080");
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
```

## WebSocket Support

The framework includes an integrated websocket module for handling WebSocket connections using tungstenite.

## Documentation

- [Tutorial](./examples/README.md) - Step-by-step guide with progressive examples
- [Custom Middleware Guide](./docs/CUSTOM_MIDDLEWARE.md) - Creating custom middleware
- [Examples](./examples/) - Working code samples
- [API Docs](https://docs.rs/relay_builder) - API reference

## Production Usage

This framework powers:
- [groups_relay](https://github.com/verse-pbc/groups) - NIP-29 group chat relay
- [profile_aggregator](https://github.com/verse-pbc/profile_aggregator) - Profile aggregation service

## License

MIT