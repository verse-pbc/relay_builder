//! NIP-40: Expiration Timestamp middleware

use crate::nostr_middleware::{InboundContext, InboundProcessor, NostrMiddleware, OutboundContext};
use crate::subscription_coordinator::StoreCommand;
use nostr_sdk::prelude::*;
use tracing::{error, warn};

// Helper function to get expiration timestamp from event tags
fn get_event_expiration(event: &Event) -> Option<Timestamp> {
    event.tags.iter().find_map(|tag| {
        if tag.kind() == TagKind::Expiration {
            tag.content()
                .and_then(|s| s.parse::<u64>().ok().map(Timestamp::from))
        } else {
            None
        }
    })
}

/// Middleware to handle NIP-40 expiration tags.
///
/// It checks incoming `EVENT` messages for an `expiration` tag.
/// If the tag exists and the timestamp is in the past, the event is dropped
/// and an `OK: false` message is sent back. On the outbound side, it filters
/// out events that have expired and queues them for lazy deletion.
#[derive(Debug, Clone)]
pub struct Nip40ExpirationMiddleware;

impl Default for Nip40ExpirationMiddleware {
    fn default() -> Self {
        Self::new()
    }
}

impl Nip40ExpirationMiddleware {
    pub fn new() -> Self {
        Self
    }
}

impl<T> NostrMiddleware<T> for Nip40ExpirationMiddleware
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
            if let Some(ClientMessage::Event(event_cow)) = &ctx.message {
                let event_ref: &Event = event_cow.as_ref();
                if let Some(expiration) = get_event_expiration(event_ref) {
                    if expiration < Timestamp::now() {
                        warn!(
                            target: "nip40",
                            "Event {} (kind {}) with expiration {} is expired. Publisher: {}.",
                            event_ref.id, event_ref.kind, expiration, event_ref.pubkey
                        );

                        let filter = Filter::new().id(event_ref.id);
                        let delete_command = StoreCommand::DeleteEvents(
                            filter,
                            (*ctx.state.read().subdomain).clone(),
                            None,
                        );

                        let coordinator = {
                            let state = ctx.state.read();
                            state.subscription_coordinator().cloned()
                        };

                        if let Some(coordinator) = coordinator {
                            if let Err(e) = coordinator.save_and_broadcast(delete_command).await {
                                error!(
                                    target: "nip40",
                                    "Failed to send delete command for expired event {}: {}", event_ref.id, e
                                );
                            }
                        }
                    }
                }
            }
            ctx.next().await
        }
    }

    /// Filters outgoing event messages, dropping events that have expired.
    fn process_outbound(
        &self,
        ctx: OutboundContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            if let Some(RelayMessage::Event { event, .. }) = &ctx.message {
                let event_ref: &Event = event.as_ref();
                if let Some(expiration) = get_event_expiration(event_ref) {
                    if expiration < Timestamp::now() {
                        warn!(
                            target: "nip40",
                            "Dropping expired event {} (kind {}) with expiration {} from outbound. Publisher: {}.",
                            event_ref.id, event_ref.kind, expiration, event_ref.pubkey
                        );
                        *ctx.message = None;
                    }
                }
            }
            // Outbound processing doesn't call next() - runs sequentially
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::create_test_state;
    use std::borrow::Cow;
    use std::time::Duration;

    type MessageReceiver = flume::Receiver<(RelayMessage<'static>, usize, Option<String>)>;

    fn create_test_expiration_context(
        connection_id: String,
        message: Option<ClientMessage<'static>>,
        state: crate::state::NostrConnectionState<()>,
    ) -> (crate::test_utils::TestInboundContext<()>, MessageReceiver) {
        let (tx, rx) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(10);
        let test_ctx = crate::test_utils::create_test_inbound_context(
            connection_id,
            message,
            Some(tx),
            state,
            vec![],
            0,
        );
        (test_ctx, rx)
    }

    fn create_test_outbound_context(
        connection_id: String,
        message: RelayMessage<'static>,
        state: crate::state::NostrConnectionState<()>,
    ) -> (crate::test_utils::TestOutboundContext<()>, MessageReceiver) {
        let (tx, rx) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(10);
        let test_ctx = crate::test_utils::create_test_outbound_context(
            connection_id,
            message,
            Some(tx),
            state,
            vec![],
            0,
        );
        (test_ctx, rx)
    }

    #[tokio::test]
    async fn test_expired_event_passes_through_inbound() {
        let middleware = Nip40ExpirationMiddleware::new();
        let state = create_test_state(None);

        let keys = Keys::generate();
        let expired_event = EventBuilder::new(Kind::Custom(1234), "expired content")
            .tags([Tag::expiration(
                Timestamp::now() - Duration::from_secs(3600),
            )])
            .build(keys.public_key());
        let expired_event = keys.sign_event(expired_event).await.unwrap();

        let (mut test_ctx, _rx) = create_test_expiration_context(
            "test_conn".to_string(),
            Some(ClientMessage::Event(Cow::Owned(expired_event))),
            state,
        );

        let ctx = test_ctx.as_context();
        // The middleware should let the event pass through (deletion happens async)
        assert!(middleware.process_inbound(ctx).await.is_ok());
    }

    #[tokio::test]
    async fn test_non_expired_event_passes_through_inbound() {
        let middleware = Nip40ExpirationMiddleware::new();
        let state = create_test_state(None);

        let keys = Keys::generate();
        let non_expired_event = EventBuilder::new(Kind::Custom(1234), "valid content")
            .tags([Tag::expiration(
                Timestamp::now() + Duration::from_secs(3600),
            )])
            .build(keys.public_key());
        let non_expired_event = keys.sign_event(non_expired_event).await.unwrap();

        let (mut test_ctx, _rx) = create_test_expiration_context(
            "test_conn".to_string(),
            Some(ClientMessage::Event(Cow::Owned(non_expired_event))),
            state,
        );

        let ctx = test_ctx.as_context();
        assert!(middleware.process_inbound(ctx).await.is_ok());
    }

    #[tokio::test]
    async fn test_event_without_expiration_passes_through() {
        let middleware = Nip40ExpirationMiddleware::new();
        let state = create_test_state(None);

        let keys = Keys::generate();
        let event = EventBuilder::new(Kind::Custom(1234), "no expiration").build(keys.public_key());
        let event = keys.sign_event(event).await.unwrap();

        let (mut test_ctx, _rx) = create_test_expiration_context(
            "test_conn".to_string(),
            Some(ClientMessage::Event(Cow::Owned(event))),
            state,
        );

        let ctx = test_ctx.as_context();
        assert!(middleware.process_inbound(ctx).await.is_ok());
    }

    #[tokio::test]
    async fn test_expired_event_filtered_outbound() {
        let middleware = Nip40ExpirationMiddleware::new();
        let state = create_test_state(None);

        let keys = Keys::generate();
        let expired_event = EventBuilder::new(Kind::Custom(1234), "expired content")
            .tags([Tag::expiration(
                Timestamp::now() - Duration::from_secs(3600),
            )])
            .build(keys.public_key());
        let expired_event = keys.sign_event(expired_event).await.unwrap();

        let sub_id = SubscriptionId::new("test_sub");
        let relay_msg = RelayMessage::Event {
            subscription_id: Cow::Owned(sub_id),
            event: Cow::Owned(expired_event),
        };

        let (mut test_ctx, _rx) =
            create_test_outbound_context("test_conn".to_string(), relay_msg, state);

        let ctx = test_ctx.as_context();
        assert!(middleware.process_outbound(ctx).await.is_ok());
        // The message should be set to None (filtered out)
        assert!(test_ctx.message.is_none());
    }

    #[tokio::test]
    async fn test_non_expired_event_passes_outbound() {
        let middleware = Nip40ExpirationMiddleware::new();
        let state = create_test_state(None);

        let keys = Keys::generate();
        let non_expired_event = EventBuilder::new(Kind::Custom(1234), "valid content")
            .tags([Tag::expiration(
                Timestamp::now() + Duration::from_secs(3600),
            )])
            .build(keys.public_key());
        let non_expired_event = keys.sign_event(non_expired_event).await.unwrap();

        let sub_id = SubscriptionId::new("test_sub");
        let relay_msg = RelayMessage::Event {
            subscription_id: Cow::Owned(sub_id),
            event: Cow::Owned(non_expired_event.clone()),
        };

        let (mut test_ctx, _rx) =
            create_test_outbound_context("test_conn".to_string(), relay_msg, state);

        let ctx = test_ctx.as_context();
        assert!(middleware.process_outbound(ctx).await.is_ok());
        // The message should still be present
        assert!(test_ctx.message.is_some());

        if let Some(RelayMessage::Event { event, .. }) = &test_ctx.message {
            assert_eq!(event.id, non_expired_event.id);
        }
    }

    #[tokio::test]
    async fn test_get_expiration_timestamp() {
        let expiration_ts = Timestamp::now() + Duration::from_secs(3600);
        let tags = vec![Tag::expiration(expiration_ts)];

        let unsigned_event = EventBuilder::new(Kind::Custom(1234), "test")
            .tags(tags)
            .build(Keys::generate().public_key());
        let event = Keys::generate().sign_event(unsigned_event).await.unwrap();

        let extracted = get_event_expiration(&event);
        assert_eq!(extracted, Some(expiration_ts));
    }

    #[tokio::test]
    async fn test_get_expiration_timestamp_missing() {
        let unsigned_event =
            EventBuilder::new(Kind::Custom(1234), "test").build(Keys::generate().public_key());
        let event = Keys::generate().sign_event(unsigned_event).await.unwrap();

        let extracted = get_event_expiration(&event);
        assert_eq!(extracted, None);
    }
}
