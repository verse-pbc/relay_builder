# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with the relay_builder framework.

## Critical Context

**This is a framework, not an application** - Users build custom Nostr relays by implementing the `EventProcessor` trait or creating custom middleware.

## Architecture Decisions & Trade-offs

### Two-Level API Design
1. **EventProcessor (High-level)**: Most users implement this for business logic - handles what events to accept/reject
2. **NostrMiddleware (Low-level)**: For protocol extensions and cross-cutting concerns - avoid unless EventProcessor is insufficient

### Performance Optimizations
- **ChainBlueprint → ConnectedChain**: Pre-allocates MessageSenders per connection to avoid allocations in hot path
- **Arc-wrapped middlewares**: Cheap cloning when building chains
- **Position-based outbound routing**: Messages skip unnecessary middleware based on origin position
- **Static dispatch**: Zero-cost abstractions through compile-time middleware chains

### Non-Obvious Implementation Details

- **Event signature verification**: Handled automatically by EventIngester, NOT a middleware
- **Subscription system**: Split across three files (coordinator, registry, index) for separation of concerns
- **relay_middleware.rs**: The bridge between EventProcessor and the WebSocket layer - rarely needs modification
- **Multi-tenant isolation**: Uses `Scope` parameter throughout for data isolation by subdomain

### Common Pitfalls & Solutions

1. **Don't implement NostrMiddleware for business logic** - Use EventProcessor instead
2. **Don't modify relay_middleware.rs** - It's the stable bridge layer
3. **Auth status**: Check `context.authed_pubkey` in EventProcessor, not raw connection state
4. **Subdomain data**: Always include `context.subdomain` in StoreCommands for proper isolation
5. **Custom state**: Use `.custom_state::<T>()` on RelayBuilder, type must impl Default

### Learning Path for New Users

1. Start with `examples/01_minimal_relay.rs` - simplest working relay
2. Implement EventProcessor (`examples/02_event_processing.rs`) for custom logic
3. Only if needed: Custom middleware (`examples/05_custom_middleware.rs`)
4. Production setup: See `examples/06_production.rs` for monitoring/shutdown patterns

Detailed docs in `examples/README.md`

### Key Patterns

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

#### NostrMiddleware Implementation (Rarely Needed)

```rust
// Only implement NostrMiddleware for cross-cutting concerns like:
// - Protocol extensions
// - Message transformation/filtering
// - Connection-level monitoring

impl<T> NostrMiddleware<T> for MyMiddleware {
    async fn process_inbound<Next>(&self, ctx: InboundContext<'_, T, Next>) -> Result<()> 
    where Next: InboundProcessor<T> 
    {
        // ALWAYS call ctx.next().await to continue chain
        ctx.next().await
    }
}
```

**Note**: NostrMiddleware is specific to relay_builder and handles Nostr protocol messages. The underlying websocket_builder focuses on generic WebSocket transport.

## Testing Gotchas

- **outbound_middleware.rs**: Tests the position-based routing - critical for understanding message flow
- **on_connect_test.rs**: Tests connection lifecycle with BuildConnected trait
- Integration tests often need to build ConnectedChain from ChainBlueprint

## Production Deployments Using This Framework

- **groups_relay** (in this monorepo): NIP-29 implementation, good reference for complex EventProcessor
- Features graceful shutdown, metrics, multi-tenant support