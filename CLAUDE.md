# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Common Development Commands

### Building and Running
- `cargo build` - Build the library in debug mode
- `cargo build --release` - Build in release mode
- `cargo build --features axum` - Build with axum web server support
- `cargo run --example minimal_relay --features axum` - Run the minimal relay example
- `cargo run --example <example_name> --features axum` - Run other examples (most require axum feature)

### Testing
- `cargo test` - Run all tests (unit and integration)
- `cargo test -- --nocapture` - Run tests with println! output visible
- `cargo test --lib` - Run only library unit tests
- `cargo test --test <test_name>` - Run specific integration test (e.g., `cargo test --test logic_integration`)

### Code Quality
- `cargo fmt` - Format code according to Rust standards
- `cargo fmt -- --check` - Check formatting without making changes
- `cargo clippy` - Run linter for common mistakes
- `cargo clippy --all-targets --all-features -- -D warnings` - Strict clippy check with all features

### Documentation
- `cargo doc` - Generate documentation
- `cargo doc --open` - Generate and open documentation in browser
- `cargo doc --no-deps` - Generate docs without dependencies

### Benchmarking
- `cargo bench` - Run performance benchmarks

### Git Hooks Setup
- `./scripts/setup-hooks.sh` - Set up pre-commit hooks for formatting and linting

## Architecture Overview

relay_builder is a Rust framework for building custom Nostr relays with a middleware-based architecture. It provides a flexible foundation for implementing Nostr protocol relays with pluggable business logic.

### Core Design Principles

1. **EventProcessor Trait**: High-level trait for business logic - most users should implement this
2. **Middleware Pattern**: Low-level WebSocket middleware from `websocket_builder` for protocol extensions
3. **Builder Pattern**: `RelayBuilder` provides fluent API for relay construction
4. **Async-First**: Built on Tokio with fully async message processing
5. **State Management**: Supports custom per-connection state via generics

### Key Components

- `src/relay_builder.rs` - Main builder for constructing relays
- `src/event_processor.rs` - Core trait for business logic implementation
- `src/middleware.rs` - Relay middleware that bridges EventProcessor to WebSocket
- `src/state.rs` - Connection state management with generic support
- `src/database.rs` - Async database abstraction layer
- `src/subscription_service.rs` - Nostr subscription and filtering
- `src/crypto_helper.rs` - Cryptographic operations helper

### Middleware Implementations

Built-in middleware in `src/middlewares/`:
- `error_handling.rs` - Global error handling and recovery
- `logger.rs` - Request/response logging
- `metrics.rs` - Performance metrics collection
- `nip40_expiration.rs` - NIP-40 event expiration
- `nip42_auth.rs` - NIP-42 authentication
- `nip70_protected.rs` - NIP-70 protected events

Note: Event signature verification is handled automatically by EventIngester (not a middleware).

### Multi-Tenant Support

- `src/subdomain.rs` - Subdomain isolation for multi-tenant deployments
- Each subdomain gets isolated event storage and subscriptions using `Scope`

### Database Architecture

- Uses `StoreCommand` enum for all database operations
- Supports pluggable storage backends via traits
- Default implementation uses LMDB via `nostr-lmdb`
- All operations are async

### Example Progression

Examples in `examples/` directory (require `--features axum`):
1. `01_minimal_relay.rs` - Start here! Basic relay implementation
2. `02_bare_mode.rs` - Direct WebSocket middleware without EventProcessor
3. `03_spam_filter.rs` - Add spam filtering with rate limits
4. `04_auth_relay.rs` - Authentication and access control (NIP-42)
5. `05_protocol_features.rs` - Advanced protocol features (NIPs)
6. `06_rate_limiter.rs` - Rate limiting implementation
7. `07_user_sessions.rs` - Custom per-connection state management
8. `08_multi_tenant.rs` - Multi-tenant with subdomain isolation
9. `09_production.rs` - Production-ready with monitoring

### Common Patterns

#### EventProcessor Implementation
```rust
#[async_trait]
impl EventProcessor for MyRelay {
    async fn handle_event(
        &self,
        event: Event,
        custom_state: &mut (),
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>> {
        // Business logic here
        Ok(vec![StoreCommand::SaveSignedEvent(
            Box::new(event),
            context.subdomain.clone(),
        )])
    }
}
```

#### WebSocket Middleware Implementation (Advanced)

For low-level protocol extensions, you can implement the WebSocket Middleware trait directly:

```rust
#[async_trait]
impl<T> Middleware for MyMiddleware<T> {
    type State = NostrConnectionState<T>;
    type IncomingMessage = ClientMessage<'static>;
    type OutgoingMessage = RelayMessage<'static>;

    async fn process_inbound(
        &self,
        ctx: &mut InboundContext<Self::State, Self::IncomingMessage, Self::OutgoingMessage>,
    ) -> Result<(), anyhow::Error> {
        // Access the message
        if let Some(message) = &ctx.message {
            // Process or modify the message
        }
        
        // Send messages back to client
        ctx.send_message(RelayMessage::notice("Hello"))?;
        
        // Continue to next middleware
        ctx.next().await
    }

    async fn process_outbound(
        &self,
        ctx: &mut OutboundContext<Self::State, Self::IncomingMessage, Self::OutgoingMessage>,
    ) -> Result<(), anyhow::Error> {
        // Filter or modify outgoing messages
        ctx.next().await
    }

    async fn on_connect(
        &self,
        ctx: &mut ConnectionContext<Self::State, Self::IncomingMessage, Self::OutgoingMessage>,
    ) -> Result<(), anyhow::Error> {
        // Handle new connections
        ctx.next().await
    }

    async fn on_disconnect(
        &self,
        ctx: &mut DisconnectContext<Self::State, Self::IncomingMessage, Self::OutgoingMessage>,
    ) -> Result<(), anyhow::Error> {
        // Handle connection cleanup
        ctx.next().await
    }
}
```

Key points:
- Context objects provide `connection_id`, `state`, `sender`, and `message` fields
- Use `ctx.send_message()` to send messages back to the client
- Always call `ctx.next().await` to continue the middleware chain
- Access connection state via `ctx.state.read().await` or `ctx.state.write().await`
- See `src/middlewares/` for implementation examples

### Testing Strategy

- Unit tests embedded in source files
- Integration tests in `tests/` directory:
  - `logic_integration.rs` - EventProcessor behavior
  - `middleware_integration.rs` - Middleware chain
  - `error_handling_integration.rs` - Error scenarios
  - `crypto_performance_test.rs` - Crypto performance

### Dependencies

- Built on `websocket_builder` (workspace dependency)
- Requires Rust 1.87.0 (specified in `rust-toolchain.toml`)
- Core deps: nostr-sdk, tokio, async-trait, tracing
- Optional: axum for web server integration

### Production Examples

This framework can be used to build:
- Group chat relays (NIP-29)
- Private/paid relays with access control
- Profile aggregation services
- Specialized event-type relays