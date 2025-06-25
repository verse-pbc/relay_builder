# Nostr Relay Builder Tutorial

This tutorial teaches you to use nostr_relay_builder through 9 progressive examples. Each example adds ONE library feature with minimal code.

## Prerequisites
- Basic Rust knowledge
- Familiarity with Nostr protocol basics

## How to Use This Tutorial
1. Each step has a corresponding example file (`01_*.rs` through `09_*.rs`)
2. Examples build on each other - start from the beginning
3. Run each example: `cargo run --example 01_minimal_relay --features axum`

## Step 1: Minimal Relay

Learn the basics with the simplest possible relay.

**Library Features:**
- `RelayBuilder::new(config)` - Create a builder
- `RelayConfig::new(url, db_path, keys)` - Basic configuration (path or database instance)
- `.with_relay_info()` - NIP-11 relay information
- `.build_axum()` - Build for Axum framework
- Default middlewares (logger, error handler, signature verifier)

**What happens by default:**
- ✅ Signature verification (EventVerifierMiddleware)
- ✅ Error handling (ErrorHandlingMiddleware)
- ✅ Request logging (LoggerMiddleware)
- ✅ Event storage and retrieval
- ✅ Subscription management

📝 **Run:** `cargo run --example 01_minimal_relay --features axum`

## Step 2: Understanding Bare Mode

See what the framework does for you by disabling defaults.

**Library Features:**
- `.bare()` - Disable automatic middleware
- Manual middleware addition order
- Understanding the middleware stack

**Key Learning:**
- Why signature verification is critical
- How middleware ordering affects processing
- When you might need bare mode (rarely!)

⚠️ **Warning:** Without EventVerifierMiddleware, invalid signatures are accepted!

📝 **Run:** `cargo run --example 02_bare_mode --features axum`

## Step 3: Custom Business Logic with EventProcessor

Filter events using high-level business logic.

**Library Features:**
- `EventProcessor` trait - Your business logic interface
- `handle_event()` - Decide what to accept/reject
- `EventContext` - Access connection state and metadata
- `StoreCommand` - Control what gets stored

**EventProcessor vs Middleware:**
- EventProcessor = WHAT events to accept (business logic)
- Middleware = HOW messages flow (protocol handling)

📝 **Run:** `cargo run --example 03_spam_filter --features axum`

## Step 4: Adding Authentication

Enable NIP-42 authentication with one configuration line.

**Library Features:**
- `config.enable_auth = true` - Enable authentication
- `.with_auth(AuthConfig)` - Configure auth behavior
- `context.state.authed_pubkey` - Check authentication
- Automatic AUTH message handling

**Key Points:**
- Auth middleware (Nip42Middleware) is added automatically when enabled
- Users must authenticate before posting
- Check authed_pubkey in your EventProcessor

📝 **Run:** `cargo run --example 04_auth_relay --features axum`

## Step 5: Protocol Features (NIPs)

Add support for deletion, expiration, and protected events.

**Library Features:**
- `config.create_database()` - Database and sender for NIP-09 (returns tuple)
- `.with_middleware(Nip09Middleware)` - Event deletion (NIP-09)
- `.with_middleware(Nip40ExpirationMiddleware)` - Event expiration (NIP-40)
- `.with_middleware(Nip70Middleware)` - Protected events (NIP-70)

**How they work:**
- Run BEFORE your EventProcessor
- Handle protocol-level concerns automatically
- Can be combined as needed

📝 **Run:** `cargo run --example 05_protocol_features --features axum`

## Step 6: Custom Middleware

Create your own middleware for cross-cutting concerns.

**Library Features:**
- `Middleware` trait - Low-level message processing
- `process_inbound()` - Intercept incoming messages
- `ctx.next().await` - Continue the chain
- `ctx.send()` - Send responses

**When to use Middleware vs EventProcessor:**
- Middleware: Rate limiting, metrics, protocol extensions
- EventProcessor: Business rules, access control

📝 **Run:** `cargo run --example 06_rate_limiter --features axum`

## Step 7: Per-Connection State

Track data for each connected client.

**Library Features:**
- `EventProcessor<T>` - Generic over state type
- `.with_custom_state::<T>()` - Change state type
- `.with_state_factory()` - Initialize state
- Mutable state in async contexts

**Use cases:**
- Per-user rate limiting
- Session tracking
- Reputation systems

📝 **Run:** `cargo run --example 07_user_sessions --features axum`

## Step 8: Multi-Tenant Support

Isolate data by subdomain automatically.

**Library Features:**
- `ScopeConfig::Subdomain` - Enable subdomain isolation
- `.with_subdomains_from_url()` - Parse from URL (used in groups_relay)
- `context.subdomain` - Access current subdomain
- Automatic data isolation

**How it works:**
- alice.relay.com → data scoped to "alice"
- bob.relay.com → data scoped to "bob"
- Zero code changes needed!

📝 **Run:** `cargo run --example 08_multi_tenant --features axum`

## Step 9: Production Ready

Add monitoring, metrics, and graceful shutdown.

**Library Features:**
- `.with_task_tracker()` - Graceful shutdown
- `.with_cancellation_token()` - Shutdown signaling
- `.with_connection_counter()` - Active connections
- `.with_metrics()` - Performance metrics
- `.with_subscription_metrics()` - Subscription tracking
- `.with_websocket_config()` - Connection limits
- `.with_subscription_limits()` - Query limits
- `.build_relay_service()` - Full service access

**Production patterns from real relays:**
- **groups_relay**: Custom ValidationMiddleware, PrometheusSubscriptionMetricsHandler
- **profile_aggregator**: TaskTracker for graceful shutdown

📝 **Run:** `cargo run --example 09_production --features axum`

## Quick Reference

### RelayBuilder Methods
```rust
RelayBuilder::new(config)
    // Mode
    .bare()                              // Disable defaults
    
    // State
    .with_custom_state::<T>()            // Custom state type
    .with_state_factory(|| T::default()) // State initialization
    
    // Features
    .with_auth(AuthConfig {...})         // NIP-42 authentication
    .with_middleware(middleware)         // Add middleware
    .with_event_processor(processor)     // Business logic
    
    // Monitoring
    .with_metrics(handler)               // Performance metrics
    .with_subscription_metrics(handler)  // Subscription metrics
    
    // Lifecycle
    .with_task_tracker(tracker)          // Graceful shutdown
    .with_cancellation_token(token)      // Shutdown signal
    .with_connection_counter(counter)    // Connection tracking
    
    // Configuration
    .with_websocket_config(config)       // WebSocket settings
    .with_subscription_limits(max, limit)// Query limits
    .with_relay_info(info)               // NIP-11 info
    
    // Build
    .build()                             // → WebSocket handler
    .build_axum()                        // → Axum handler  
    .build_relay_service(info)           // → Full service
```

### RelayConfig Methods
```rust
RelayConfig::new(url, db_path, keys)    // db_path can be String or Arc<RelayDatabase>
    .with_subdomains_from_url(url)      // Parse subdomains
    .with_auth(AuthConfig {...})         // Auth settings
    .with_websocket_config(config)       // Connection config
    .with_subscription_limits(max, limit)// Query limits
```

## Next Steps

- Explore each example file to see the code in action
- Check out production implementations:
  - [groups_relay](https://github.com/verse-pbc/groups_relay) - NIP-29 group chat relay
  - [profile_aggregator](https://github.com/verse-pbc/profile_aggregator) - Profile aggregation service
- Read the [middleware guide](../docs/middleware_guide.md) for deeper understanding