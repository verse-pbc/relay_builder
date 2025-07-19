# Middleware vs EventProcessor Guide

## Overview

The `relay_builder` provides two ways to customize relay behavior:

1. **EventProcessor** - High-level business logic API (recommended for most use cases)
2. **Middleware** - Low-level message processing API (for protocol extensions)

## EventProcessor (High-Level API)

### What is it?
`EventProcessor` is a specialized trait that handles your relay's business logic. Behind the scenes, it's wrapped in a `RelayMiddleware` that handles all the complex protocol details for you.

### Custom State Support
The framework now supports custom per-connection state through `EventProcessor<T>`:

```rust
// Define custom state
#[derive(Debug, Clone, Default)]
struct MyState {
    rate_limit_tokens: f32,
    reputation: u32,
}

// Use EventProcessor with your state type
#[async_trait]
impl EventProcessor<MyState> for MyProcessor {
    async fn handle_event(
        &self,
        event: Event,
        custom_state: Arc<tokio::sync::RwLock<MyState>>,
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>> {
        // Get write lock to modify state
        let mut state = custom_state.write().await;
        if state.rate_limit_tokens < 1.0 {
            return Err(Error::restricted("Rate limited"));
        }
        state.rate_limit_tokens -= 1.0;
        // ... rest of logic
    }
}
```

### When to use EventProcessor?
Use `EventProcessor` when you want to:
- ✅ Define which events to accept or reject
- ✅ Control who can read/write events
- ✅ Implement custom access control
- ✅ Add business-specific validation
- ✅ Create different relay types (public, private, paid, etc.)

### What it handles for you:
- ✅ Subscription management (REQ/CLOSE)
- ✅ Database operations
- ✅ Response generation (OK/NOTICE/EOSE)
- ✅ Event broadcasting to subscribers
- ✅ Proper message ordering

### Example:
```rust
#[derive(Debug, Clone)]
struct PrivateRelayProcessor {
    allowed_pubkeys: HashSet<PublicKey>,
}

#[async_trait]
impl EventProcessor for PrivateRelayProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<tokio::sync::RwLock<()>>,
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>> {
        // Simple business logic
        if self.allowed_pubkeys.contains(&event.pubkey) {
            Ok(vec![(event, context.subdomain.clone()).into()])
        } else {
            Ok(vec![]) // Reject silently
        }
    }
}
```

## Middleware (Low-Level API)

### What is it?
`Middleware` is a generic trait from `websocket_builder` that intercepts messages at various stages of processing. It gives you full control over the message flow.

### When to use Middleware?
Use `Middleware` when you need to:
- ✅ Implement new NIPs that modify the protocol
- ✅ Transform messages before they reach the EventProcessor
- ✅ Add cross-cutting concerns (logging, metrics, rate limiting)
- ✅ Reject messages early in the pipeline
- ✅ Modify outbound messages

### Common middleware use cases:
1. **Protocol Extensions** (NIPs)
   - `Nip40ExpirationMiddleware` - Removes expired events
   - `Nip42Middleware` - Authentication
   - `Nip70Middleware` - Protected events

2. **Validation/Filtering**
   - Event signature verification is handled automatically by EventIngester
   - `ValidationMiddleware` - Custom validation rules
   - Rate limiting middleware

3. **Cross-cutting Concerns**
   - `LoggerMiddleware` - Logs all messages
   - `ErrorHandlingMiddleware` - Graceful error handling
   - Metrics collection

### Middleware with Custom State
All middleware work seamlessly with custom state:

```rust
// For relays with custom state
let builder = RelayBuilder::<MyState>::new(config)
    .with_middleware(LoggerMiddleware::new())
    .with_middleware(ErrorHandlingMiddleware::new())
    // Event verification is handled automatically by EventIngester
```

### Example:
```rust
#[derive(Debug, Clone)]
pub struct RateLimitMiddleware {
    max_events_per_minute: usize,
}

#[async_trait]
impl<T> Middleware for RateLimitMiddleware<T>
where
    T: Send + Sync + std::fmt::Debug + 'static,
{
    type State = NostrConnectionState<T>;
    type IncomingMessage = ClientMessage<'static>;
    type OutgoingMessage = RelayMessage<'static>;

    async fn process_inbound(
        &self,
        ctx: &mut InboundContext<Self::State, Self::IncomingMessage, Self::OutgoingMessage>,
    ) -> Result<(), anyhow::Error> {
        if let Some(ClientMessage::Event(_)) = &ctx.message {
            // Check rate limit (accessing state requires read lock)
            let state = ctx.state.read().await;
            if self.exceeds_rate_limit(&*state) {
                // Drop the message and send notice
                ctx.message = None;
                ctx.send_message(RelayMessage::notice("rate limit exceeded")).await?;
                return Ok(()); // Don't call next()
            }
        }
        
        // Continue to next middleware
        ctx.next().await
    }
}
```

## Key Differences

| Aspect | EventProcessor | Middleware |
|--------|---------------|------------|
| **Abstraction Level** | High-level business logic | Low-level message processing |
| **Complexity** | Simple - just implement business rules | Complex - manage message flow |
| **Protocol Knowledge** | Minimal required | Must understand Nostr protocol |
| **Subscription Handling** | Automatic | Manual |
| **Database Access** | Via StoreCommand returns | Direct access if needed |
| **Message Types** | All types via handle_message | All message types |
| **Chainable** | No - always terminal | Yes - can have many |
| **Use Cases** | Relay business logic | Protocol extensions, filtering |

## Architecture

```
Client Message Flow:
    ↓
[LoggerMiddleware]           // Logs messages
    ↓
[ErrorHandlingMiddleware]    // Catches errors
    ↓
[Nip42Middleware]           // Authentication
    ↓
[EventIngester - automatic] // Signature verification (not a middleware)
    ↓
[ValidationMiddleware]      // Custom validation
    ↓
[Nip40ExpirationMiddleware] // Expiration handling
    ↓
[Nip70Middleware]          // Protected events
    ↓
[RelayMiddleware<EventProcessor>] // Your business logic (TERMINAL)
    ↓
Database
```

## Best Practices

### For EventProcessor:
1. Keep it focused on business logic
2. Don't worry about protocol details
3. Return appropriate `StoreCommand`s
4. For `can_see_event` (sync method), use `blocking_read()` or `blocking_write()` on the Arc<RwLock<T>>
5. For `handle_event` (async method), use `read().await` or `write().await` on the Arc<RwLock<T>>

### For Middleware:
1. Always call `ctx.next()` unless you're consuming the message
2. Be careful with message ownership (`ctx.message.take()`)
3. Place validation middleware early in the chain
4. Place transformation middleware late in the chain
5. Never place middleware after RelayMiddleware

## Common Patterns

### 1. Public Relay (EventProcessor)
```rust
struct PublicRelay;

impl EventProcessor for PublicRelay {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<tokio::sync::RwLock<()>>,
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>> {
        // Accept everything
        Ok(vec![StoreCommand::SaveSignedEvent(Box::new(event), context.subdomain.clone())])
    }
}
```

### 2. Authenticated Relay (EventProcessor + Middleware)
```rust
// Use Nip42Middleware for auth, then check in EventProcessor
struct AuthenticatedRelay;

impl EventProcessor for AuthenticatedRelay {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<tokio::sync::RwLock<()>>,
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>> {
        if context.authed_pubkey.is_some() {
            Ok(vec![StoreCommand::SaveSignedEvent(Box::new(event), context.subdomain.clone())])
        } else {
            Ok(vec![]) // Reject unauthenticated
        }
    }
}
```

### 3. Custom Protocol Extension (Middleware)
```rust
// Middleware for a hypothetical NIP that adds custom tags
struct CustomNipMiddleware;

#[async_trait]
impl<T> Middleware for CustomNipMiddleware
where
    T: Send + Sync + std::fmt::Debug + 'static,
{
    type State = NostrConnectionState<T>;
    type IncomingMessage = ClientMessage<'static>;
    type OutgoingMessage = RelayMessage<'static>;

    async fn process_inbound(
        &self,
        ctx: &mut InboundContext<Self::State, Self::IncomingMessage, Self::OutgoingMessage>,
    ) -> Result<(), anyhow::Error> {
        // Custom protocol logic here
        ctx.next().await
    }
}
```

## Summary

- **Start with EventProcessor** - It handles 90% of relay use cases
- **Use Middleware only when** you need protocol-level control
- **Don't chain EventProcessors** - There can be only one
- **Do chain Middlewares** - But order matters!
- **EventProcessor is always last** - It's the terminal handler

## Migration Guide: Adding Custom State

### Using EventProcessor with Custom State

If you have an existing `EventProcessor` implementation and want to add custom state:

1. **Define your state type:**
```rust
#[derive(Debug, Clone, Default)]
struct MyCustomState {
    // Your per-connection data
}
```

2. **Change trait implementation:**
```rust
// Before
impl EventProcessor for MyProcessor {
    async fn handle_event(
        &self,
        event: Event,
        _custom_state: Arc<tokio::sync::RwLock<()>>,
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>> {
        // ...
    }
}

// After
impl EventProcessor<MyCustomState> for MyProcessor {
    async fn handle_event(
        &self,
        event: Event,
        custom_state: Arc<tokio::sync::RwLock<MyCustomState>>,  // Typed state
        context: EventContext<'_>,
    ) -> Result<Vec<StoreCommand>> {
        // Access custom state through Arc<RwLock<T>>
        let mut state = custom_state.write().await;
        // ...
    }
}
```

3. **Update builder usage:**
```rust
// Before
let builder = RelayBuilder::new(config);

// After
let builder = RelayBuilder::<MyCustomState>::new(config);
```

### Backward Compatibility

The `EventProcessor` trait supports both stateless (`EventProcessor<()>`) and stateful (`EventProcessor<T>`) implementations. Existing stateless code continues to work without changes.