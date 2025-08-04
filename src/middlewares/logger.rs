//! Request/response logging middleware

use crate::nostr_middleware::{
    DisconnectContext, InboundContext, InboundProcessor, NostrMiddleware, OutboundContext,
};
use anyhow::Result;
use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use tracing::{debug, info_span};

/// Middleware that logs all incoming and outgoing messages
#[derive(Debug, Clone)]
pub struct LoggerMiddleware<T = ()> {
    _phantom: std::marker::PhantomData<T>,
}

impl<T> Default for LoggerMiddleware<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> LoggerMiddleware<T> {
    pub fn new() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }

    /// Extract subdomain from connection state
    fn extract_subdomain(
        state: &parking_lot::RwLock<crate::state::NostrConnectionState<T>>,
    ) -> Option<String> {
        let state_guard = state.read();
        match state_guard.subdomain.as_ref() {
            Scope::Named { name, .. } => Some(name.to_string()),
            Scope::Default => None,
        }
    }

    /// Create a span with connection context
    fn create_connection_span(connection_id: &str, subdomain: Option<&String>) -> tracing::Span {
        info_span!(
            parent: None,
            "websocket_connection",
            ip = %connection_id,
            subdomain = ?subdomain
        )
    }
}

impl<T> NostrMiddleware<T> for LoggerMiddleware<T>
where
    T: Clone + Send + Sync + std::fmt::Debug + 'static,
{
    fn process_inbound<Next>(
        &self,
        ctx: InboundContext<'_, T, Next>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send
    where
        Next: InboundProcessor<T>,
    {
        async move {
            // Extract subdomain from connection state
            let subdomain = Self::extract_subdomain(ctx.state);

            // Create a span with connection ID and subdomain to ensure logs always have context
            let connection_span =
                Self::create_connection_span(ctx.connection_id, subdomain.as_ref());
            let _guard = connection_span.enter();

            match ctx.message.as_ref() {
                Some(ClientMessage::Event(event)) => {
                    let event_kind_u16 = event.as_ref().kind.as_u16();
                    let event_json = event.as_ref().as_json();

                    debug!("> EVENT kind {}: {}", event_kind_u16, event_json);
                }
                Some(ClientMessage::Req {
                    subscription_id,
                    filter,
                }) => {
                    let sub_id_clone = subscription_id.clone();
                    let filter_json_clone = filter.as_json();

                    debug!("> REQ {}: {}", sub_id_clone, filter_json_clone);
                }
                Some(ClientMessage::ReqMultiFilter {
                    subscription_id,
                    filters,
                }) => {
                    let sub_id_clone = subscription_id.clone();
                    let filters_json_clone =
                        filters.iter().map(|f| f.as_json()).collect::<Vec<String>>();
                    debug!("> REQ {}: {:?}", sub_id_clone, filters_json_clone);
                }
                Some(ClientMessage::Close(subscription_id)) => {
                    let sub_id_clone = subscription_id.clone();
                    debug!("> CLOSE {}", sub_id_clone);
                }
                Some(ClientMessage::Auth(event)) => {
                    debug!("> AUTH {}", event.as_ref().as_json());
                }
                _ => {
                    let message_clone_for_debug = format!("{:?}", ctx.message);
                    debug!("> {}", message_clone_for_debug);
                }
            }

            // Continue with the middleware chain
            ctx.next().await
        }
    }

    fn process_outbound(
        &self,
        ctx: OutboundContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            // Extract subdomain from connection state
            let subdomain = Self::extract_subdomain(ctx.state);

            // Create a span with connection ID and subdomain to ensure logs always have context
            let connection_span =
                Self::create_connection_span(ctx.connection_id, subdomain.as_ref());
            let _guard = connection_span.enter();

            if let Some(msg_ref) = ctx.message.as_ref() {
                match msg_ref {
                    RelayMessage::Ok {
                        event_id,
                        status,
                        message,
                    } => {
                        let event_id_clone = *event_id;
                        let status_clone = *status;
                        let message_clone = message.clone();

                        debug!("< OK {} {} {}", event_id_clone, status_clone, message_clone);
                    }
                    RelayMessage::Event {
                        subscription_id,
                        event,
                    } => {
                        let sub_id_clone = subscription_id.clone();
                        let event_json_clone = event.as_ref().as_json();

                        debug!("< EVENT {} {}", sub_id_clone, event_json_clone);
                    }
                    RelayMessage::Notice(message) => {
                        let message_clone = message.clone();
                        debug!("< NOTICE {}", message_clone);
                    }
                    RelayMessage::EndOfStoredEvents(subscription_id) => {
                        let sub_id_clone = subscription_id.clone();
                        debug!("< EOSE {}", sub_id_clone);
                    }
                    RelayMessage::Auth { challenge } => {
                        let challenge_clone = challenge.clone();
                        debug!("< AUTH {}", challenge_clone);
                    }
                    _ => {
                        let msg_clone_for_debug = format!("{:?}", ctx.message);
                        debug!("< {}", msg_clone_for_debug);
                    }
                }
            }

            // Outbound processing doesn't call next() - runs sequentially
            Ok(())
        }
    }

    fn on_disconnect(
        &self,
        ctx: DisconnectContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            // Extract subdomain from connection state
            let subdomain = Self::extract_subdomain(ctx.state);

            // Create a span with connection ID and subdomain to ensure logs always have context
            let connection_span =
                Self::create_connection_span(ctx.connection_id, subdomain.as_ref());
            let _guard = connection_span.enter();

            debug!("Disconnected from relay");

            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    // TODO: Add proper tests once test infrastructure is ready
}
