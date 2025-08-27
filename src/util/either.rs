//! Either middleware implementation for conditional middleware support

use crate::nostr_middleware::{
    ConnectionContext, DisconnectContext, InboundContext, InboundProcessor, NostrMiddleware,
    OutboundContext,
};
use anyhow::Result;

/// Either type for conditional middleware - can be Left (middleware) or Right (identity/no-op)
#[derive(Debug, Clone)]
pub enum Either<L, R> {
    Left(L),
    Right(R),
}

/// Identity middleware - a no-op middleware that just passes through
#[derive(Debug, Clone, Default)]
pub struct IdentityMiddleware;

impl<T> NostrMiddleware<T> for IdentityMiddleware
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
        async move { ctx.next().await }
    }

    fn process_outbound(
        &self,
        _ctx: OutboundContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            // Default implementation does nothing
            Ok(())
        }
    }

    fn on_disconnect(
        &self,
        _ctx: DisconnectContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            // Default implementation does nothing
            Ok(())
        }
    }
}

impl<L, R, T> NostrMiddleware<T> for Either<L, R>
where
    L: NostrMiddleware<T>,
    R: NostrMiddleware<T>,
    T: Send + Sync + Clone + 'static,
{
    fn on_connect(
        &self,
        ctx: ConnectionContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            match self {
                Either::Left(l) => l.on_connect(ctx).await,
                Either::Right(r) => r.on_connect(ctx).await,
            }
        }
    }

    fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, T, Next>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send
    where
        Next: InboundProcessor<T>,
    {
        async move {
            match self {
                Either::Left(l) => l.process_inbound(ctx).await,
                Either::Right(r) => r.process_inbound(ctx).await,
            }
        }
    }

    fn process_outbound(
        &self,
        ctx: OutboundContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            match self {
                Either::Left(l) => l.process_outbound(ctx).await,
                Either::Right(r) => r.process_outbound(ctx).await,
            }
        }
    }

    fn on_disconnect(
        &self,
        ctx: DisconnectContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            match self {
                Either::Left(l) => l.on_disconnect(ctx).await,
                Either::Right(r) => r.on_disconnect(ctx).await,
            }
        }
    }
}
