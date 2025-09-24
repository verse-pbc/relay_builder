# Custom Middleware Guide

This guide explains how to create and use custom middleware with relay_builder's static dispatch system.

## Background: Static vs Dynamic Dispatch

relay_builder uses **static dispatch** for middleware, which means:
- All middleware types are resolved at compile time
- Zero runtime overhead (no vtables or dynamic dispatch)
- Better performance and inlining opportunities
- Type safety enforced by the compiler

## The NostrMiddleware Trait

Custom middleware implements the `NostrMiddleware<T>` trait:

```rust
pub trait NostrMiddleware<T>: Clone + Send + Sync + 'static
where
    T: Send + Sync + Clone + 'static,
{
    // Required: Process incoming messages
    fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, T, Next>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send
    where
        Next: InboundProcessor<T>;

    // Optional: Process outgoing messages
    fn process_outbound(
        &self,
        ctx: OutboundContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move { Ok(()) }
    }

    // Optional: Handle new connections
    fn on_connect(
        &self,
        ctx: ConnectionContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move { Ok(()) }
    }

    // Optional: Handle disconnections
    fn on_disconnect(
        &self,
        ctx: ConnectionContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move { Ok(()) }
    }
}
```

## Creating Custom Middleware

Here's a simple example that logs timing information:

```rust
use relay_builder::nostr_middleware::{InboundContext, InboundProcessor, NostrMiddleware};
use std::time::Instant;

#[derive(Debug, Clone)]
struct TimingMiddleware {
    name: String,
}

impl TimingMiddleware {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
        }
    }
}

impl<T> NostrMiddleware<T> for TimingMiddleware
where
    T: Send + Sync + Clone + 'static,
{
    fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, T, Next>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send
    where
        Next: InboundProcessor<T>,
    {
        async move {
            let start = Instant::now();
            
            // Continue processing
            let result = ctx.next().await;
            
            let duration = start.elapsed();
            tracing::info!("[{}] Request processed in {:?}", self.name, duration);
            
            result
        }
    }
}
```

## Using Custom Middleware

There are two approaches for adding custom middleware:

### Simple Approach: Using `with_essentials()`

For most use cases, you'll want to keep the essential middleware while adding your own:

```rust
use relay_builder::{RelayBuilder, middleware_chain::chain};

let handler_factory = RelayBuilder::<()>::new(config)
    .with_event_processor(MyProcessor)
    .build_with_chain(|chain, crypto_helper| {
        chain
            // Add your custom middleware
            .with(TimingMiddleware::new("request-timer"))
            .with(RateLimitMiddleware::new(10))
            
            // Add all essential middleware with one call
            .with_essentials(crypto_helper)
    })
    .await?;
```

The `with_essentials()` helper adds:
1. `NostrLoggerMiddleware` - Request/response logging
2. `ErrorHandlingMiddleware` - Error recovery and client notifications

Note: Event signature verification is handled automatically by EventIngester (not a middleware).

### Advanced Approach: Full Control

For complete control over the middleware stack:

```rust
let handler_factory = RelayBuilder::<()>::new(config)
    .with_event_processor(MyProcessor)
    .build_with_chain(|chain, crypto_helper| {
        chain
            // Add middleware in the exact order you want
            .with(TimingMiddleware::new("outer"))
            .with(CustomAuthMiddleware::new())
            .with(NostrLoggerMiddleware::new())
            .with(RateLimitMiddleware::new(10))
            // Event verification is handled automatically by EventIngester
            .with(ErrorHandlingMiddleware::new())
    })
    .await?;
```

## Important Considerations

### 1. Middleware Order

Middleware executes in the order you add it:
- **Outer middleware** (added first) sees messages before inner middleware
- Use this to control processing flow
- Essential middleware like error handling should usually be outer

### 2. Essential Middleware

When using `build_with_chain`, you should manually add:
- **ErrorHandlingMiddleware** - Error recovery and client notifications
- **NostrLoggerMiddleware** - Request/response logging (optional but recommended)

Note: Event signature verification is handled automatically by EventIngester before messages reach the middleware chain.

The relay middleware is automatically added as the innermost layer.

### 3. Context Access

The `InboundContext` provides:
- `connection_id` - Unique connection identifier
- `message` - The incoming message (mutable)
- `state` - Connection state (behind RwLock)
- `sender` - Send messages back to the client
- `next` - Continue to the next middleware

### 4. Error Handling

- Return errors from middleware to stop processing
- The error handling middleware will convert errors to client notices
- Use `anyhow::Error` for flexibility

## Advanced Example: Rate Limiting

```rust
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use parking_lot::Mutex;

#[derive(Clone)]
struct RateLimiter {
    limits: Arc<Mutex<HashMap<String, Vec<Instant>>>>,
    max_requests: usize,
    window: Duration,
}

impl RateLimiter {
    fn new(max_requests: usize, window: Duration) -> Self {
        Self {
            limits: Arc::new(Mutex::new(HashMap::new())),
            max_requests,
            window,
        }
    }
}

impl<T> NostrMiddleware<T> for RateLimiter
where
    T: Send + Sync + Clone + 'static,
{
    fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, T, Next>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send
    where
        Next: InboundProcessor<T>,
    {
        async move {
            let now = Instant::now();
            let mut limits = self.limits.lock();
            
            let requests = limits.entry(ctx.connection_id.to_string())
                .or_insert_with(Vec::new);
            
            // Remove old requests outside the window
            requests.retain(|&req_time| now.duration_since(req_time) < self.window);
            
            // Check rate limit
            if requests.len() >= self.max_requests {
                ctx.sender.send_notice("Rate limit exceeded").await?;
                return Ok(()); // Stop processing
            }
            
            // Record this request
            requests.push(now);
            drop(limits);
            
            // Continue processing
            ctx.next().await
        }
    }
}
```

## EventProcessor vs Middleware

Choose the right abstraction:

**Use EventProcessor when:**
- Implementing business logic
- Deciding which events to accept/reject
- Managing application state
- Handling domain-specific rules

**Use Middleware when:**
- Adding cross-cutting concerns
- Implementing protocol features
- Rate limiting or authentication
- Monitoring and metrics
- Message transformation

## Migration from Dynamic Dispatch

If you're migrating from the old dynamic dispatch system:

1. Remove `#[async_trait]` from your middleware
2. Use native `async fn` syntax
3. Add return type annotations with `impl Future`
4. Use `build_with_chain` instead of `.with_middleware`

## Performance Benefits

Static dispatch provides:
- **Zero-cost abstractions** - No runtime overhead
- **Better inlining** - Compiler can optimize across middleware boundaries
- **Type safety** - All types known at compile time
- **Smaller binaries** - No vtables or type erasure

## Full Examples

- [examples/05_custom_middleware.rs](../examples/05_custom_middleware.rs) - Complete example showing both simple and advanced approaches
- [examples/07_rate_limiting.rs](../examples/07_rate_limiting.rs) - Production-ready rate limiting middleware example