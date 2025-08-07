# Relay Builder

A framework for building custom Nostr relays with middleware support. Built on top of `websocket_builder`.

## Technology Stack

- **Language**: Rust (edition 2024, requires 1.88.0)
- **Async Runtime**: Tokio
- **WebSocket**: websocket_builder (workspace dependency) for generic WebSocket transport
- **Database**: LMDB via nostr-lmdb with scoped-heed for multi-tenant support
- **Web Framework**: Axum (always included)
- **Nostr**: nostr-sdk for protocol types and operations

## Project Structure

```
relay_builder/
├── src/
│   ├── relay_builder.rs      # Main builder pattern implementation
│   ├── event_processor.rs    # Core trait for business logic
│   ├── relay_middleware.rs   # Relay middleware that bridges EventProcessor to NostrMiddleware
│   ├── nostr_middleware.rs   # NostrMiddleware trait for protocol-level middleware
│   ├── nostr_handler.rs      # WebSocket connection handler
│   ├── middleware_chain.rs   # Static middleware chain with ChainBlueprint/ConnectedChain
│   ├── middlewares/          # Built-in middleware implementations
│   │   ├── error_handling.rs # Global error recovery
│   │   ├── logger.rs         # Request/response logging
│   │   ├── metrics.rs        # Performance metrics
│   │   ├── nip40_expiration.rs # NIP-40 expiration
│   │   ├── nip42_auth.rs    # NIP-42 authentication
│   │   └── nip70_protected.rs # NIP-70 protected events
│   ├── crypto_helper.rs     # Cryptographic operations helper
│   ├── database.rs          # Async database abstraction
│   ├── subscription_coordinator.rs # Event storage and subscription management
│   ├── subscription_registry.rs # Subscription tracking
│   ├── subscription_index.rs # Event distribution optimization
│   ├── state.rs             # Connection state management
│   └── util/                # Utility types
│       └── either.rs        # Either type for conditional middleware
├── examples/
│   ├── 01_minimal_relay.rs     # Basic relay
│   ├── 02_event_processing.rs  # Custom business logic
│   ├── 03_protocol_features.rs # NIPs support
│   ├── 04_advanced_state.rs    # Custom state and multi-tenancy
│   ├── 05_custom_middleware.rs # Middleware patterns
│   └── 06_production.rs        # Production-ready setup
└── tests/
    ├── logic_integration.rs
    ├── middleware_integration.rs
    ├── error_handling_integration.rs
    └── crypto_performance_test.rs
```

## Development Workflow

1. **Create a new relay**:
   ```rust
   use relay_builder::{RelayBuilder, EventProcessor, EventContext, StoreCommand};

   let processor = MyBusinessLogic::new();
   let handler = RelayBuilder::new(config)
       .event_processor(processor)
       .relay_info(relay_info)
       .build()
       .await?;
   ```

2. **Run examples**:
   ```bash
   cargo run --example 01_minimal_relay
   cargo run --example 05_custom_middleware
   ```

3. **Run tests**:
   ```bash
   cargo test                              # All tests
   cargo test --lib                        # Unit tests only
   cargo test --test logic_integration     # Specific integration test
   ```

## Key Concepts

### EventProcessor Trait
The core trait for implementing relay business logic:

```rust
#[async_trait]
pub trait EventProcessor<T = ()>: Send + Sync + Debug + 'static
where
    T: Send + Sync + 'static,
{
    /// Process incoming events
    async fn handle_event(
        &self,
        event: Event,
        custom_state: &mut T,
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>>;

    /// Control event visibility
    /// Leave the blank implementation for no filtering
    fn can_see_event(
        &self,
        event: &Event,
        custom_state: &T,
        context: EventContext<'_>,
    ) -> bool {
        true
    }

    /// Handle non-event messages (REQ, CLOSE, AUTH)
    /// Most of the time you won't need to implement this and the blank implementation is enough
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

### EventContext
Minimal context for zero-allocation performance:
```rust
pub struct EventContext<'a> {
    pub authed_pubkey: Option<&'a PublicKey>,
    pub subdomain: &'a Scope,
    pub relay_pubkey: &'a PublicKey,
}
```

### Middleware Architecture
The framework uses two middleware levels:
1. **NostrMiddleware**: Protocol-level middleware for Nostr messages (implement this for cross-cutting concerns)
2. **EventProcessor**: High-level business logic interface (implement this for most use cases)

Event signature verification is handled automatically by EventIngester, not via middleware.

## Built-in Features

- **NIPs Support**: 09 (deletion), 40 (expiration), 42 (auth for EVENT and REQ), 70 (protected)
- **Multi-tenant**: Subdomain isolation via Scope
- **Database**: StoreCommand enum for async database operations
- **Crypto**: EventIngester handles signature verification automatically
- **State Management**: Generic per-connection state with type safety
- **Performance**: ChainBlueprint → ConnectedChain transformation pre-allocates MessageSenders

## Common Patterns

### Accept All Events
```rust
#[derive(Debug, Clone)]
struct AcceptAllProcessor;

#[async_trait]
impl EventProcessor for AcceptAllProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: &mut (),
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>> {
        Ok(vec![StoreCommand::SaveSignedEvent(
            Box::new(event),
            context.subdomain.clone(),
            None,
        )])
    }
}
```

### Private Relay with Auth
```rust
impl EventProcessor for PrivateRelay {
    fn can_see_event(
        &self,
        event: &Event,
        _custom_state: &(),
        context: EventContext<'_>,
    ) -> bool {
        // Only show events to authenticated users
        context.authed_pubkey.is_some()
    }
}
```

### Custom State Management
```rust
#[derive(Default)]
struct RateLimitState {
    tokens: f32,
    last_update: Instant,
}

impl EventProcessor<RateLimitState> for RateLimitedRelay {
    async fn handle_event(
        &self,
        event: Event,
        state: &mut RateLimitState,
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>> {
        // Update rate limit tokens
        if state.tokens < 1.0 {
            return Err(anyhow!("Rate limit exceeded"));
        }
        state.tokens -= 1.0;
        // Process event...
    }
}
```

## Testing

```bash
# Run all tests
cargo test

# Run specific test file
cargo test --test logic_integration
cargo test --test middleware_integration

# Run with output
cargo test -- --nocapture

# Run benchmarks
cargo bench
```

## Performance Tips

- The `can_see_event` method is synchronous, it's a hot path so keep it efficient
- Middleware chains use static dispatch for zero-cost abstractions
- MessageSenders are pre-allocated per connection to avoid allocations in message processing
- Arc-wrapped middlewares enable cheap cloning when building chains

## Production Deployment

See `examples/06_production.rs` for:
- Metrics collection via metrics middleware
- Error recovery with error_handling middleware
- Rate limiting patterns would need to be added. We will add a middleware for this in the future and grow the collection from community usage.
- NIP-42 authentication setup is optional, enabled by a builder flag, for both inbound and outbound events
- Proper logging configuration

## Error Handling

Common patterns:
- Return `Err` from `handle_event` to reject events
- Use `?` operator for propagation
- Errors are automatically converted to NOTICE messages

## Dependencies

Core dependencies from Cargo.toml:
- `websocket_builder` (workspace dependency) - Generic WebSocket transport layer
- `nostr-sdk`, `nostr`, `nostr-database`, `nostr-lmdb` - Nostr protocol support
- `scoped-heed` - Multi-tenant LMDB support
- `tokio` with full features
- `axum` for web server integration

## Example Applications

This framework can be used to build:
- Basic public relays
- Private/paid relays with access control
- Group chat relays (NIP-29 support)
- Specialized event-type relays
- Multi-tenant relay services