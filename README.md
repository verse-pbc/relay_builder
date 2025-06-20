# Nostr Relay Builder

A framework for building custom Nostr relays with middleware support. Built on top of `websocket_builder`.

## Features

- Pluggable business logic via `EventProcessor` trait
- Middleware-based message processing
- Built-in NIPs: 09 (deletion), 40 (expiration), 42 (auth for EVENT and REQ), 70 (protected events)
- Database abstraction with async storage
- Multi-tenant support via subdomain isolation
- Custom per-connection state

## Usage

Add to `Cargo.toml`:

```toml
[dependencies]
nostr_relay_builder = { git = "https://github.com/verse-pbc/nostr_relay_builder" }
# Optional: Enable Axum integration
nostr_relay_builder = { git = "https://github.com/verse-pbc/nostr_relay_builder", features = ["axum"] }
```

## Basic Example

```rust
use anyhow::Result;
use axum::{routing::get, Router};
use nostr_relay_builder::{
    EventContext, EventProcessor, RelayBuilder, RelayConfig, RelayInfo,
    Result as RelayResult, StoreCommand,
};
use nostr_sdk::prelude::*;
use std::net::SocketAddr;

#[derive(Debug, Clone)]
struct AcceptAllProcessor;

#[async_trait::async_trait]
impl EventProcessor for AcceptAllProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: &mut (),
        context: EventContext<'_>,
    ) -> RelayResult<Vec<StoreCommand>> {
        Ok(vec![StoreCommand::SaveSignedEvent(
            Box::new(event),
            context.subdomain.clone(),
        )])
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let keys = Keys::generate();
    let config = RelayConfig::new("ws://localhost:8080", "./relay.db", keys);
    let relay_info = RelayInfo {
        name: "My Relay".to_string(),
        description: "A simple Nostr relay".to_string(),
        ..Default::default()
    };
    
    let processor = AcceptAllProcessor;
    let handler = RelayBuilder::new(config)
        .build_axum_handler(processor, relay_info)
        .await?;
    
    let app = Router::new().route("/", get(handler));
    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    
    axum::serve(listener, app.into_make_service()).await?;
    Ok(())
}
```

## EventProcessor Trait

The core trait for relay customization:

```rust
#[async_trait]
pub trait EventProcessor<T = ()>: Send + Sync + Debug + 'static
where
    T: Send + Sync + 'static,
{
    /// Process incoming events with custom state and event context
    async fn handle_event(
        &self,
        event: Event,
        custom_state: &mut T,
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>>;

    /// Control which stored events are visible to each connection
    fn can_see_event(
        &self,
        event: &Event,
        custom_state: &T,
        context: EventContext<'_>,
    ) -> bool {
        true
    }

    /// Handle non-event messages (REQ, CLOSE, AUTH, etc.)
    async fn handle_message(
        &self,
        message: ClientMessage,
        custom_state: &mut T,
        context: EventContext<'_>,
    ) -> Result<Vec<RelayMessage>> {
        Ok(vec![])
    }
}
```

## Examples

See `examples/` directory for complete implementations:

- `minimal_relay.rs` - Basic relay in ~80 lines - **Start here!**
- `custom_middleware.rs` - Learn the middleware pattern
- `private_relay.rs` - Access-controlled relay with authentication
- `advanced_relay.rs` - With moderation and advanced features
- `subdomain_relay.rs` - Multi-tenant with subdomain isolation
- `custom_state_relay.rs` - Custom per-connection state management
- `production_relay.rs` - Production-ready with monitoring (requires `axum` feature)

## Production Usage

This crate is used by:

- [groups_relay](https://github.com/verse-pbc/groups_relay) - NIP-29 group chat relay
- [profile_aggregator](https://github.com/verse-pbc/profile_aggregator) - Profile data aggregation relay

## Dependencies

- [websocket_builder](https://github.com/verse-pbc/websocket_builder) - WebSocket framework
- nostr-sdk - Nostr protocol implementation

## License

MIT