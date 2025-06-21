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
- `cargo test --test <test_name>` - Run specific integration test

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

nostr_relay_builder is a Rust framework for building custom Nostr relays with a middleware-based architecture. It provides a flexible foundation for implementing Nostr protocol relays with pluggable business logic.

### Core Design Principles

1. **Two-Level Architecture**:
   - **EventProcessor**: High-level trait for business logic (most users should use this)
   - **Middleware**: Low-level trait for protocol extensions (advanced use cases)

2. **Builder Pattern**: `RelayBuilder` provides fluent API for relay construction

3. **Async-First**: Built on Tokio with fully async message processing

4. **State Management**: Supports custom per-connection state via generics

### Key Components

- `src/relay_builder.rs` - Main builder for constructing relays
- `src/event_processor.rs` - High-level business logic trait
- `src/middleware.rs` - Low-level middleware infrastructure
- `src/state.rs` - Connection state management with generic support
- `src/database.rs` - Async database abstraction layer
- `src/subscription_service.rs` - Nostr subscription and filtering
- `src/crypto_worker.rs` - Parallel cryptographic operations

### Middleware Implementations

Built-in middleware in `src/middlewares/`:
- `error_handling.rs` - Global error handling and recovery
- `event_verifier.rs` - Cryptographic signature verification
- `logger.rs` - Request/response logging
- `metrics.rs` - Performance metrics collection
- `nip09_deletion.rs` - NIP-09 event deletion support
- `nip40_expiration.rs` - NIP-40 event expiration
- `nip42_auth.rs` - NIP-42 authentication
- `nip70_protected.rs` - NIP-70 protected events

### Multi-Tenant Support

- `src/subdomain.rs` - Subdomain isolation for multi-tenant deployments
- Each subdomain gets isolated event storage and subscriptions

### Database Architecture

- Uses `StoreCommand` enum for all database operations
- Supports pluggable storage backends via traits
- Default implementation uses LMDB via `nostr-lmdb`
- All operations are async

### Example Progression

Examples in `examples/` directory (require `--features axum`):
1. `minimal_relay.rs` - Start here! ~80 lines for basic relay
2. `custom_middleware.rs` - Learn middleware pattern
3. `private_relay.rs` - Add authentication and access control
4. `advanced_relay.rs` - Moderation and advanced features
5. `subdomain_relay.rs` - Multi-tenant with subdomain isolation
6. `custom_state_relay.rs` - Custom per-connection state
7. `production_relay.rs` - Production-ready with monitoring

### Common Patterns

#### EventProcessor Implementation
```rust
#[async_trait]
impl EventProcessor for MyRelay {
    async fn process_event(&self, event: &Event) -> Result<ProcessEventResponse> {
        // Business logic here
        Ok(ProcessEventResponse::Accept)
    }
}
```

#### Middleware Implementation
```rust
#[async_trait]
impl Middleware for MyMiddleware {
    async fn process_event(&self, event: ProcessEventRequest, next: Next<'_, ProcessEventRequest>) -> Result<ProcessEventResponse> {
        // Pre-processing
        let response = next.run(event).await?;
        // Post-processing
        Ok(response)
    }
}
```

### Testing Strategy

- Unit tests embedded in source files
- Integration tests in `tests/` directory:
  - `logic_integration.rs` - EventProcessor behavior
  - `middleware_integration.rs` - Middleware chain
  - `error_handling_integration.rs` - Error scenarios
  - `crypto_performance_test.rs` - Crypto performance

### Dependencies

- Built on `websocket_builder` from same monorepo
- Requires Rust 1.87.0 (specified in `rust-toolchain.toml`)
- Core deps: nostr-sdk, tokio, async-trait, tracing
- Optional: axum for web server integration

### Production Usage

This framework is used by:
- `groups_relay` - NIP-29 group chat relay
- `profile_aggregator` - Profile data aggregation service

Both are in the same monorepo under `relay_repos/`.