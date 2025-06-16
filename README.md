# Nostr Relay Builder

A framework for building custom Nostr relays with middleware support. Built on top of `websocket_builder`.

## Features

- Pluggable business logic via `EventProcessor` trait
- Middleware-based message processing
- Built-in NIPs: 09 (deletion), 40 (expiration), 42 (auth), 70 (protected events)
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
use nostr_relay_builder::{RelayBuilder, RelayConfig, EventProcessor, NostrConnectionState, StoreCommand};
use nostr_sdk::prelude::*;

#[derive(Debug, Clone)]
struct MyEventProcessor;

#[async_trait::async_trait]
impl EventProcessor for MyEventProcessor {
    async fn handle_event(
        &self,
        event: Event,
        state: &mut NostrConnectionState,
    ) -> Result<Vec<StoreCommand>> {
        Ok(vec![StoreCommand::SaveSignedEvent(
            Box::new(event),
            state.subdomain().clone(),
        )])
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let keys = Keys::generate();
    let config = RelayConfig::new("wss://localhost:8080", "./relay.db", keys);
    
    RelayBuilder::new(config)
        .run_server(MyEventProcessor)
        .await
}
```

## EventProcessor Trait

The core trait for relay customization:

```rust
#[async_trait]
pub trait EventProcessor: Send + Sync + Debug + 'static {
    /// Process incoming events - decide whether to accept, reject, or transform them
    async fn handle_event(&self, event: Event, state: &mut NostrConnectionState) -> Result<Vec<StoreCommand>>;
    
    /// Control which stored events are visible to each connection (default: all visible)
    fn can_see_event(&self, event: &Event, state: &NostrConnectionState, relay_pubkey: &PublicKey) -> Result<bool> { Ok(true) }
    
    /// Validate subscription filters before processing (default: all allowed) 
    fn verify_filters(&self, filters: &[Filter], authed_pubkey: Option<PublicKey>, state: &NostrConnectionState) -> Result<()> { Ok(()) }
    
    /// Handle custom protocol messages beyond standard Nostr (default: standard handling)
    async fn handle_message(&self, message: ClientMessage, state: &mut NostrConnectionState) -> Result<Vec<RelayMessage>> { Ok(vec![]) }
}
```

## Examples

See `examples/` directory for complete implementations:

- `minimal_relay.rs` - Basic relay in ~60 lines
- `advanced_relay.rs` - With authentication and moderation
- `private_relay.rs` - Access-controlled relay
- `subdomain_relay.rs` - Multi-tenant with subdomain isolation
- `custom_middleware.rs` - Custom protocol extensions
- `custom_state_relay.rs` - Custom per-connection state

## Production Usage

This crate is used by:

- [groups_relay](https://github.com/verse-pbc/groups_relay) - NIP-29 group chat relay
- [profile_aggregator](https://github.com/verse-pbc/profile_aggregator) - Profile data aggregation relay

## Dependencies

- [websocket_builder](https://github.com/verse-pbc/websocket_builder) - WebSocket framework
- nostr-sdk - Nostr protocol implementation

## License

MIT