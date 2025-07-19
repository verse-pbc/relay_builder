# Relay Builder Examples

This guide walks through relay_builder features with progressive examples.

## Prerequisites
- Basic Rust knowledge
- Familiarity with Nostr protocol basics

## Structure
- Examples are numbered 01 through 06
- Each builds on previous concepts
- Run with: `cargo run --example 01_minimal_relay`


## Example 1: Minimal Relay

Basic relay with default configuration - the simplest possible implementation.

**Library Features:**
- `RelayBuilder::new(config)` - Create a builder
- `RelayConfig::new(url, db_path, keys)` - Basic configuration
- Default middlewares (logger, error handler)
- Automatic event signature verification via EventIngester

**What happens by default:**
- Event signature verification (automatic)
- Error handling and request logging
- Event storage and retrieval
- Subscription management

**Run:** `cargo run --example 01_minimal_relay`

## Example 2: Event Processing

Implement custom business logic for event handling.

**Library Features:**
- `EventProcessor` trait - Your business logic interface
- `handle_event()` - Decide what to accept/reject
- `EventContext` - Access connection state and metadata
- `StoreCommand` - Control what gets stored
- Content filtering (spam detection)
- Rate limiting per public key
- Custom rejection messages

**Key Learning:**
- EventProcessor = WHAT events to accept (business logic)
- Implement complex validation rules
- Track state across events
- Provide user-friendly error messages

**Run:** `cargo run --example 02_event_processing`

## Example 3: Protocol Features

NIPs support for authentication, expiration, and protected events.

**Library Features:**
- `config.enable_auth = true` - Enable NIP-42 authentication
- Event expiration handling (NIP-40)
- Protected events (NIP-70)
- `context.authed_pubkey` - Check authentication status

**Key Points:**
- Auth middleware is added automatically when enabled
- Combine multiple protocol features
- Protocol-level concerns handled automatically
- Check auth status in your EventProcessor

**Run:** `cargo run --example 03_protocol_features`

## Example 4: Advanced State Management

Per-connection state and multi-tenant support.

**Library Features:**
- `EventProcessor<T>` - Generic over state type
- `.custom_state::<T>()` - Change state type
- `ScopeConfig::Subdomain` - Enable subdomain isolation
- `context.subdomain` - Access current subdomain
- Automatic data isolation by subdomain
- Different behavior per tenant

**Use cases:**
- Per-user rate limiting and session tracking
- Multi-tenant SaaS deployments
- Community hosting with isolation
- Reputation systems

**Run:** `cargo run --example 04_advanced_state`

## Example 5: Custom Middleware

Create your own middleware for cross-cutting concerns.

**Library Features:**
- `NostrMiddleware<T>` trait - Implement custom middleware
- `build_with()` - Compose custom middleware chains
- `without_defaults()` - Disable default middleware for full control
- `process_inbound()` - Intercept incoming messages
- `on_connect()` - Handle new connections
- `.without_defaults()` mode for complete control

**When to use Custom Middleware:**
- Request/response timing and monitoring
- Custom authentication schemes
- Protocol extensions
- Message transformation
- Welcome messages and connection handling

**Two Approaches Shown:**
1. Default: Automatic logger and error handling
2. Advanced: `without_defaults()` mode with full control

**Run:** `cargo run --example 05_custom_middleware`

## Example 6: Production Configuration

Monitoring, metrics, and graceful shutdown for production deployments.

**Library Features:**
- `.with_task_tracker()` - Graceful shutdown
- `.with_cancellation_token()` - Shutdown signaling
- `.with_connection_counter()` - Active connections
- `.with_metrics()` - Performance metrics
- `.with_subscription_metrics()` - Subscription tracking
- `.with_websocket_config()` - Connection limits
- `.with_subscription_limits()` - Query limits
- `.build_relay_service()` - Full service access

**Production patterns:**
- Health check endpoints
- Prometheus metrics
- Graceful shutdown on signals
- Connection and subscription limits
- Resource management

**Run:** `cargo run --example 06_production`

## Quick Reference

### RelayBuilder Methods
```rust
RelayBuilder::new(config)
    // Mode
    .without_defaults()                  // Disable default middleware
    
    // State
    .custom_state::<T>()                 // Custom state type (uses T::default())
    
    // Features
    .event_processor(processor)          // Business logic
    
    // Monitoring
    .metrics(handler)                    // Performance metrics
    .subscription_metrics(handler)       // Subscription metrics
    
    // Lifecycle
    .task_tracker(tracker)               // Graceful shutdown
    .cancellation_token(token)           // Shutdown signal
    .connection_counter(counter)         // Connection tracking
    
    // Configuration
    .relay_info(info)                    // NIP-11 info
    
    // Build
    .build()                             // → WebSocket handler
    .build_with(closure)                 // → Custom middleware chain
```

### RelayConfig Methods
```rust
RelayConfig::new(url, db_path, keys)    // db_path can be String or Arc<RelayDatabase>
    .with_subdomains_from_url(url)      // Parse subdomains
    .with_websocket_config(config)       // Connection config
    .with_subscription_limits(max, limit)// Query limits
```

## Learning Path

1. **Start with Example 1** - Understand the basics
2. **Example 2** - Learn EventProcessor for business logic
3. **Example 3** - Add protocol features as needed
4. **Example 4** - Advanced state when you need per-connection data or multi-tenancy
5. **Example 5** - Custom middleware for specialized requirements
6. **Example 6** - Production deployment considerations

## Next Steps

- Explore each example file to see the code in action
- Check out production implementations:
  - [groups_relay](https://github.com/verse-pbc/groups) - NIP-29 group chat relay
  - [profile_aggregator](https://github.com/verse-pbc/profile_aggregator) - Profile aggregation service
- Read the [custom middleware guide](../docs/CUSTOM_MIDDLEWARE.md) for creating your own middleware