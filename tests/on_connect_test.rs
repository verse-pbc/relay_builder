//! Test that on_connect is properly propagated through the middleware chain

use nostr_sdk::prelude::*;
use parking_lot::RwLock;
use relay_builder::{
    middleware_chain::chain,
    nostr_middleware::{InboundContext, InboundProcessor, NostrMiddleware, OutboundContext},
    state::NostrConnectionState,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Test middleware that tracks when on_connect is called
#[derive(Clone)]
struct TrackingMiddleware {
    name: String,
    on_connect_called: Arc<AtomicBool>,
}

impl TrackingMiddleware {
    fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            on_connect_called: Arc::new(AtomicBool::new(false)),
        }
    }

    fn was_on_connect_called(&self) -> bool {
        self.on_connect_called.load(Ordering::Relaxed)
    }
}

impl<T> NostrMiddleware<T> for TrackingMiddleware
where
    T: Send + Sync + Clone + 'static,
{
    async fn on_connect(
        &self,
        _ctx: relay_builder::nostr_middleware::ConnectionContext<'_, T>,
    ) -> Result<(), anyhow::Error> {
        println!("{}: NostrMiddleware::on_connect called", self.name);
        self.on_connect_called.store(true, Ordering::Relaxed);
        Ok(())
    }

    async fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, T, Next>,
    ) -> Result<(), anyhow::Error>
    where
        Next: InboundProcessor<T>,
    {
        println!("{}: process_inbound called", self.name);
        ctx.next().await
    }

    async fn process_outbound(&self, _ctx: OutboundContext<'_, T>) -> Result<(), anyhow::Error> {
        Ok(())
    }
}

impl<T> InboundProcessor<T> for TrackingMiddleware
where
    T: Send + Sync + 'static,
{
    async fn process_inbound_chain(
        &self,
        _connection_id: &str,
        _message: &mut Option<ClientMessage<'static>>,
        _state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
    ) -> Result<(), anyhow::Error> {
        println!("{}: InboundProcessor::process_inbound called", self.name);
        Ok(())
    }

    async fn on_connect_chain(
        &self,
        _connection_id: &str,
        _state: &Arc<parking_lot::RwLock<NostrConnectionState<T>>>,
    ) -> Result<(), anyhow::Error> {
        println!("{}: InboundProcessor::on_connect called", self.name);
        Ok(())
    }
}

#[tokio::test]
async fn test_on_connect_propagation() {
    // Create tracking middlewares
    let outer = TrackingMiddleware::new("outer");
    let middle = TrackingMiddleware::new("middle");
    let inner = TrackingMiddleware::new("inner");

    // Build chain: outer -> middle -> inner
    let chain = chain::<()>()
        .with(inner.clone())
        .with(middle.clone())
        .with(outer.clone())
        .build();

    // Create test state
    let relay_url = RelayUrl::parse("ws://test.relay").unwrap();
    let state = Arc::new(RwLock::new(
        NostrConnectionState::<()>::new(relay_url).unwrap(),
    ));

    // Create test sender
    let (tx, _rx) = flume::unbounded();

    // Build the connected chain from the blueprint
    use relay_builder::middleware_chain::BuildConnected;
    let connected_chain = chain.build_connected(tx);

    // Call on_connect through the connected chain
    InboundProcessor::on_connect_chain(&connected_chain, "test_conn", &state)
        .await
        .unwrap();

    // Check if on_connect was called on each middleware
    // Now that we've fixed the chain propagation, all middlewares should have their on_connect called
    assert!(
        outer.was_on_connect_called(),
        "Outer middleware on_connect should be called"
    );
    assert!(
        middle.was_on_connect_called(),
        "Middle middleware on_connect should be called"
    );
    assert!(
        inner.was_on_connect_called(),
        "Inner middleware on_connect should be called"
    );
}
