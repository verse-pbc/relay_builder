//! NIP-42: Authentication of clients to relays

use crate::error::Error;
use crate::nostr_middleware::{InboundContext, InboundProcessor, NostrMiddleware, OutboundContext};
use crate::subdomain::extract_subdomain;
use anyhow::Result;
use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::time::Duration;
use tracing::{debug, error};
use url::Url;

/// Configuration for NIP-42 authentication
#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// The relay's URL used for auth validation
    pub relay_url: String,
    /// Whether subdomain validation is enabled
    pub validate_subdomains: bool,
}

impl Default for AuthConfig {
    fn default() -> Self {
        Self {
            relay_url: String::new(),
            validate_subdomains: true,
        }
    }
}

/// Middleware implementing NIP-42 authentication
#[derive(Debug, Clone)]
pub struct Nip42Middleware<T = ()> {
    config: AuthConfig,
    _phantom: std::marker::PhantomData<T>,
}

impl<T> Nip42Middleware<T> {
    pub fn new(config: AuthConfig) -> Self {
        Self {
            config,
            _phantom: std::marker::PhantomData,
        }
    }

    /// Create middleware with just relay URL, using defaults for other settings
    pub fn with_url(relay_url: String) -> Self {
        Self::new(AuthConfig {
            relay_url,
            ..Default::default()
        })
    }

    // Extract the host from a URL
    fn extract_host_from_url(&self, url_str: &str) -> Option<String> {
        match Url::parse(url_str) {
            Ok(url) => url.host_str().map(|s| s.to_string()),
            Err(_) => None,
        }
    }

    // Check if the auth event's relay URL is valid for the current connection
    fn validate_relay_url(&self, client_relay_url: &str, connection_scope: &Scope) -> bool {
        debug!(target: "auth", "Validating relay URL - client: {}, relay: {}, connection_scope: {:?}",
              client_relay_url, self.config.relay_url, connection_scope);

        // For localhost or IP addresses, require exact match
        if client_relay_url.contains("localhost")
            || client_relay_url.contains("127.0.0.1")
            || self.config.relay_url.contains("localhost")
            || self.config.relay_url.contains("127.0.0.1")
        {
            let exact_match = client_relay_url.trim_end_matches('/')
                == self.config.relay_url.trim_end_matches('/');
            debug!(target: "auth", "Localhost/IP match result: {}", exact_match);
            return exact_match;
        }

        // Extract hosts from URLs
        let client_host = match self.extract_host_from_url(client_relay_url) {
            Some(host) => host,
            None => {
                debug!(target: "auth", "Failed to extract host from client URL: {}", client_relay_url);
                return false;
            }
        };

        let relay_host = match self.extract_host_from_url(&self.config.relay_url) {
            Some(host) => host,
            None => {
                debug!(target: "auth", "Failed to extract host from relay URL: {}", self.config.relay_url);
                return false;
            }
        };

        debug!(target: "auth", "Extracted hosts - client: {}, relay: {}", client_host, relay_host);

        // Extract parts from hosts
        let client_parts: Vec<&str> = client_host.split('.').collect();
        let relay_parts: Vec<&str> = relay_host.split('.').collect();

        // The base domain is determined by the relay URL's number of parts
        let base_domain_parts = relay_parts.len();

        // Extract base domains
        let client_base_start = if client_parts.len() >= base_domain_parts {
            client_parts.len() - base_domain_parts
        } else {
            0
        };

        let client_base = client_parts[client_base_start..].join(".");
        let relay_base = relay_parts.join(".");

        debug!(target: "auth", "Base domains - client: {}, relay: {}", client_base, relay_base);

        // Base domains must match
        if client_base != relay_base {
            debug!(target: "auth", "Base domain mismatch");
            return false;
        }

        // If subdomain validation is disabled, we're done
        if !self.config.validate_subdomains {
            return true;
        }

        // If we have a specific subdomain from the connection, ensure it matches
        if let Scope::Named {
            name: conn_subdomain,
            ..
        } = connection_scope
        {
            // Extract subdomain from client's relay URL
            let client_subdomain = extract_subdomain(&client_host, base_domain_parts);

            debug!(target: "auth", "Comparing subdomains - connection: {}, client: {:?}",
                   conn_subdomain, client_subdomain);

            // Check subdomain match
            match client_subdomain {
                Some(client_sub) => client_sub == conn_subdomain.as_str(),
                None => false,
            }
        } else {
            // If no specific subdomain in connection (Scope::Default), ensure client URL has no subdomain
            let client_subdomain = extract_subdomain(&client_host, base_domain_parts);
            client_subdomain.is_none()
        }
    }
}

impl<T> NostrMiddleware<T> for Nip42Middleware<T>
where
    T: Send + Sync + Clone + 'static,
{
    fn on_connect(
        &self,
        ctx: crate::nostr_middleware::ConnectionContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            debug!(
                target: "auth",
                "[{}] New connection, sending auth challenge",
                ctx.connection_id
            );
            let challenge_event = {
                let mut state_write = ctx.state.write();
                state_write.get_challenge_event()
            };
            debug!(
                target: "auth",
                "[{}] Generated challenge event: {:?}",
                ctx.connection_id,
                challenge_event
            );
            // Send challenge using NostrMessageSender
            ctx.send_message(challenge_event)
                .map_err(|e| anyhow::anyhow!("Failed to send challenge: {}", e))?;
            Ok(())
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
            match ctx.message.as_ref() {
                Some(ClientMessage::Auth(auth_event_cow)) => {
                    let auth_event = auth_event_cow.as_ref();
                    let auth_event_id = auth_event.id;
                    let auth_event_pubkey = auth_event.pubkey;
                    let connection_id_clone = ctx.connection_id;

                    debug!(
                        target: "auth",
                        "[{}] Processing AUTH message for event ID {}",
                        connection_id_clone, auth_event_id
                    );

                    let (expected_challenge, connection_subdomain) = {
                        let state_guard = ctx.state.read();
                        let challenge = state_guard.challenge.as_ref().cloned();
                        let subdomain = Arc::clone(&state_guard.subdomain);
                        (challenge, subdomain)
                    };

                    let Some(expected_challenge) = expected_challenge else {
                        let conn_id_err = ctx.connection_id;
                        error!(
                            target: "auth",
                            "[{}] No challenge found in state for AUTH message (event ID {}).",
                            conn_id_err, auth_event_id
                        );
                        ctx.sender.send(RelayMessage::ok(
                            auth_event_id,
                            false,
                            "auth-required: no challenge pending",
                        ))?;
                        return Err(Error::auth_required("No challenge found in state").into());
                    };

                    if auth_event.kind != Kind::Authentication {
                        let conn_id_err = ctx.connection_id;
                        error!(
                            target: "auth",
                            "[{}] Invalid event kind for AUTH message: {} (event ID {}).",
                            conn_id_err, auth_event.kind, auth_event_id
                        );
                        ctx.sender.send(RelayMessage::ok(
                            auth_event_id,
                            false,
                            "auth-required: invalid event kind",
                        ))?;
                        return Err(Error::auth_required("Invalid event kind").into());
                    }

                    if auth_event.verify().is_err() {
                        let conn_id_err = ctx.connection_id;
                        error!(
                            target: "auth",
                            "[{}] Invalid signature for AUTH message (event ID {}).",
                            conn_id_err, auth_event_id
                        );
                        ctx.sender.send(RelayMessage::ok(
                            auth_event_id,
                            false,
                            "auth-required: invalid signature",
                        ))?;
                        return Err(Error::auth_required("Invalid signature").into());
                    }

                    let found_challenge_in_tag: Option<String> =
                        auth_event.tags.iter().find_map(|tag_ref: &Tag| {
                            match tag_ref.as_standardized() {
                                Some(TagStandard::Challenge(s)) => Some(s.clone()),
                                _ => None,
                            }
                        });

                    match found_challenge_in_tag {
                        Some(tag_challenge_str) => {
                            if tag_challenge_str != expected_challenge {
                                let conn_id_err = ctx.connection_id;
                                error!(
                                    target: "auth",
                                    "[{}] Challenge mismatch for AUTH. Expected '{}', got '{}'. Event ID: {}.",
                                    conn_id_err, expected_challenge, tag_challenge_str, auth_event_id
                                );
                                ctx.sender.send(RelayMessage::ok(
                                    auth_event_id,
                                    false,
                                    "auth-required: challenge mismatch",
                                ))?;
                                return Err(Error::auth_required("Challenge mismatch").into());
                            }
                        }
                        None => {
                            let conn_id_err = ctx.connection_id;
                            error!(
                                target: "auth",
                                "[{}] No challenge tag found in AUTH message. Event ID: {}.",
                                conn_id_err, auth_event_id
                            );
                            ctx.sender.send(RelayMessage::ok(
                                auth_event_id,
                                false,
                                "auth-required: missing challenge tag",
                            ))?;
                            return Err(Error::auth_required("No challenge tag found").into());
                        }
                    }

                    let found_relay_in_tag: Option<RelayUrl> =
                        auth_event.tags.iter().find_map(|tag_ref: &Tag| {
                            match tag_ref.as_standardized() {
                                Some(TagStandard::Relay(r)) => Some(r.clone()),
                                _ => None,
                            }
                        });

                    match found_relay_in_tag {
                        Some(tag_relay_url) => {
                            let client_relay_url = tag_relay_url.as_str_without_trailing_slash();

                            // Validate the relay URL against the current connection
                            if !self.validate_relay_url(client_relay_url, &connection_subdomain) {
                                let conn_id_err = ctx.connection_id;
                                let subdomain_msg = match &*connection_subdomain {
                                    Scope::Named { name, .. } => {
                                        format!(" with subdomain '{name}'")
                                    }
                                    Scope::Default => String::new(),
                                };

                                error!(
                                    target: "auth",
                                    "[{}] Relay URL mismatch for AUTH. Expected domain matching '{}'{}. Got '{}'. Event ID: {}.",
                                    conn_id_err, self.config.relay_url, subdomain_msg, client_relay_url, auth_event_id
                                );

                                ctx.sender.send(RelayMessage::ok(
                                    auth_event_id,
                                    false,
                                    "auth-required: relay mismatch",
                                ))?;
                                return Err(Error::auth_required("Relay mismatch").into());
                            }
                        }
                        None => {
                            let conn_id_err = ctx.connection_id;
                            error!(
                                target: "auth",
                                "[{}] No relay tag found in AUTH message. Event ID: {}.",
                                conn_id_err, auth_event_id
                            );
                            ctx.sender.send(RelayMessage::ok(
                                auth_event_id,
                                false,
                                "auth-required: missing relay tag",
                            ))?;
                            return Err(Error::auth_required("No relay tag found").into());
                        }
                    }

                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_else(|_| Duration::from_secs(0))
                        .as_secs();
                    if auth_event.created_at.as_u64() < now.saturating_sub(600) {
                        let conn_id_err = ctx.connection_id;
                        error!(
                            target: "auth",
                            "[{}] Expired AUTH message (event ID {}). Created at: {}, Now: {}",
                            conn_id_err, auth_event_id, auth_event.created_at.as_u64(), now
                        );
                        ctx.sender.send(RelayMessage::ok(
                            auth_event_id,
                            false,
                            "auth-required: expired auth event",
                        ))?;
                        return Err(Error::auth_required("Expired auth event").into());
                    }

                    // Authentication successful
                    {
                        let mut state_write = ctx.state.write();
                        state_write.authed_pubkey = Some(auth_event_pubkey);
                        state_write.challenge = None;
                    }
                    debug!(
                        target: "auth",
                        "[{}] Successfully authenticated pubkey {} (event ID {}).",
                        connection_id_clone, auth_event_pubkey, auth_event_id
                    );
                    ctx.sender
                        .send(RelayMessage::ok(auth_event_id, true, "authenticated"))?;
                    Ok(())
                }
                _ => ctx.next().await,
            }
        }
    }

    fn process_outbound(
        &self,
        _ctx: OutboundContext<'_, T>,
    ) -> impl std::future::Future<Output = Result<(), anyhow::Error>> + Send {
        async move {
            // No outbound processing needed for authentication
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::create_test_state;
    use nostr_lmdb::Scope;
    use std::borrow::Cow;
    use std::sync::Arc;
    use std::time::{Instant, SystemTime, UNIX_EPOCH};

    type MessageReceiver = flume::Receiver<(RelayMessage<'static>, usize, Option<String>)>;
    // Helper function for creating test contexts with the new NostrMiddleware system
    fn create_test_auth_context(
        connection_id: String,
        message: Option<ClientMessage<'static>>,
        state: crate::state::NostrConnectionState<()>,
    ) -> (crate::test_utils::TestInboundContext<()>, MessageReceiver) {
        // Create a proper channel for testing
        let (tx, rx) = flume::bounded::<(RelayMessage<'static>, usize, Option<String>)>(10);
        let test_ctx = crate::test_utils::create_test_inbound_context(
            connection_id,
            message,
            Some(tx), // sender
            state,
            vec![], // middlewares (unused in new system)
            0,      // index
        );
        (test_ctx, rx)
    }

    #[tokio::test]
    async fn test_authed_pubkey_valid_auth() {
        let keys = Keys::generate();
        let auth_url = "wss://test.relay".to_string();
        let middleware = Nip42Middleware::with_url(auth_url.clone());
        let mut state = create_test_state(None);
        let challenge = "test_challenge".to_string();
        state.challenge = Some(challenge.clone());

        let auth_event = EventBuilder::new(Kind::Authentication, "")
            .tag(Tag::from_standardized(TagStandard::Challenge(challenge)))
            .tag(Tag::from_standardized(TagStandard::Relay(
                RelayUrl::parse(&auth_url).unwrap(),
            )))
            .build_with_ctx(&Instant::now(), keys.public_key());
        let auth_event = keys.sign_event(auth_event).await.unwrap();

        let (mut test_ctx, _rx) = create_test_auth_context(
            "test_conn".to_string(),
            Some(ClientMessage::Auth(Cow::Owned(auth_event.clone()))),
            state,
        );

        // Create the context and call the middleware
        let ctx = test_ctx.as_context();
        assert!(
            <Nip42Middleware<()> as NostrMiddleware<()>>::process_inbound(&middleware, ctx)
                .await
                .is_ok()
        );
        assert_eq!(test_ctx.state.read().authed_pubkey, Some(keys.public_key()));
    }

    #[tokio::test]
    async fn test_authed_pubkey_missing_challenge() {
        let keys = Keys::generate();
        let auth_url = "wss://test.relay".to_string();
        let middleware = Nip42Middleware::with_url(auth_url.clone());
        let state = create_test_state(None);

        let auth_event = EventBuilder::new(Kind::Authentication, "")
            .tag(Tag::from_standardized(TagStandard::Relay(
                RelayUrl::parse(&auth_url).unwrap(),
            )))
            .build_with_ctx(&Instant::now(), keys.public_key());
        let auth_event = keys.sign_event(auth_event).await.unwrap();

        let (mut test_ctx, _rx) = create_test_auth_context(
            "test_conn".to_string(),
            Some(ClientMessage::Auth(Cow::Owned(auth_event.clone()))),
            state,
        );

        // Create the context and call the middleware
        let ctx = test_ctx.as_context();
        assert!(
            <Nip42Middleware<()> as NostrMiddleware<()>>::process_inbound(&middleware, ctx)
                .await
                .is_err()
        );
        assert_eq!(test_ctx.state.read().authed_pubkey, None);
    }

    #[tokio::test]
    async fn test_authed_pubkey_wrong_challenge() {
        let keys = Keys::generate();
        let auth_url = "wss://test.relay".to_string();
        let middleware = Nip42Middleware::with_url(auth_url.clone());
        let mut state = create_test_state(None);
        let challenge = "test_challenge".to_string();
        state.challenge = Some(challenge);

        let auth_event = EventBuilder::new(Kind::Authentication, "")
            .tag(Tag::from_standardized(TagStandard::Challenge(
                "wrong_challenge".to_string(),
            )))
            .tag(Tag::from_standardized(TagStandard::Relay(
                RelayUrl::parse(&auth_url).unwrap(),
            )))
            .build_with_ctx(&Instant::now(), keys.public_key());
        let auth_event = keys.sign_event(auth_event).await.unwrap();

        let (mut test_ctx, _rx) = create_test_auth_context(
            "test_conn".to_string(),
            Some(ClientMessage::Auth(Cow::Owned(auth_event.clone()))),
            state,
        );

        let ctx = test_ctx.as_context();
        assert!(
            <Nip42Middleware<()> as NostrMiddleware<()>>::process_inbound(&middleware, ctx)
                .await
                .is_err()
        );
        assert_eq!(test_ctx.state.read().authed_pubkey, None);
    }

    #[tokio::test]
    async fn test_wrong_relay() {
        let keys = Keys::generate();
        let auth_url = "wss://test.relay".to_string();
        let middleware = Nip42Middleware::with_url(auth_url);
        let mut state = create_test_state(None);
        let challenge = "test_challenge".to_string();
        state.challenge = Some(challenge.clone());

        let auth_event = EventBuilder::new(Kind::Authentication, "")
            .tag(Tag::from_standardized(TagStandard::Challenge(challenge)))
            .tag(Tag::from_standardized(TagStandard::Relay(
                RelayUrl::parse("wss://wrong.relay").unwrap(),
            )))
            .build_with_ctx(&Instant::now(), keys.public_key());
        let auth_event = keys.sign_event(auth_event).await.unwrap();

        let (mut test_ctx, _rx) = create_test_auth_context(
            "test_conn".to_string(),
            Some(ClientMessage::Auth(Cow::Owned(auth_event.clone()))),
            state,
        );

        let ctx = test_ctx.as_context();
        assert!(
            <Nip42Middleware<()> as NostrMiddleware<()>>::process_inbound(&middleware, ctx)
                .await
                .is_err()
        );
        assert_eq!(test_ctx.state.read().authed_pubkey, None);
    }

    #[tokio::test]
    async fn test_wrong_signature() {
        let keys = Keys::generate();
        let wrong_keys = Keys::generate();
        let auth_url = "wss://test.relay".to_string();
        let middleware = Nip42Middleware::with_url(auth_url.clone());
        let mut state = create_test_state(None);
        let challenge = "test_challenge".to_string();
        state.challenge = Some(challenge.clone());

        let auth_event = EventBuilder::new(Kind::Authentication, "")
            .tag(Tag::from_standardized(TagStandard::Challenge(challenge)))
            .tag(Tag::from_standardized(TagStandard::Relay(
                RelayUrl::parse(&auth_url).unwrap(),
            )))
            .build_with_ctx(&Instant::now(), keys.public_key());
        let auth_event = wrong_keys.sign_event(auth_event).await.unwrap();

        let (mut test_ctx, _rx) = create_test_auth_context(
            "test_conn".to_string(),
            Some(ClientMessage::Auth(Cow::Owned(auth_event.clone()))),
            state,
        );

        let ctx = test_ctx.as_context();
        assert!(
            <Nip42Middleware<()> as NostrMiddleware<()>>::process_inbound(&middleware, ctx)
                .await
                .is_err()
        );
        assert_eq!(test_ctx.state.read().authed_pubkey, None);
    }

    #[tokio::test]
    async fn test_expired_auth() {
        let keys = Keys::generate();
        let auth_url = "wss://test.relay".to_string();
        let middleware = Nip42Middleware::with_url(auth_url.clone());
        let mut state = create_test_state(None);
        let challenge = "test_challenge".to_string();
        state.challenge = Some(challenge.clone());

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let expired_time = now - 601; // Just over 10 minutes ago

        let auth_event = EventBuilder::new(Kind::Authentication, "")
            .tag(Tag::from_standardized(TagStandard::Challenge(challenge)))
            .tag(Tag::from_standardized(TagStandard::Relay(
                RelayUrl::parse(&auth_url).unwrap(),
            )))
            .custom_created_at(Timestamp::from(expired_time))
            .build(keys.public_key());
        let auth_event = keys.sign_event(auth_event).await.unwrap();

        let (mut test_ctx, _rx) = create_test_auth_context(
            "test_conn".to_string(),
            Some(ClientMessage::Auth(Cow::Owned(auth_event.clone()))),
            state,
        );

        let ctx = test_ctx.as_context();
        assert!(
            <Nip42Middleware<()> as NostrMiddleware<()>>::process_inbound(&middleware, ctx)
                .await
                .is_err()
        );
        assert_eq!(test_ctx.state.read().authed_pubkey, None);
    }

    // TODO: Re-implement this test once on_connect functionality is moved to a proper location
    // #[tokio::test]
    // async fn test_on_connect_sends_challenge() {
    //     let auth_url = "wss://test.relay".to_string();
    //     let middleware = Nip42Middleware::with_url(auth_url);
    //     let state = create_test_state(None);

    //     // Create a proper channel for testing
    //     let (tx, _rx) = flume::bounded(10);
    //     let test_ctx = crate::test_utils::create_test_connection_context(
    //         "test_conn".to_string(),
    //         Some(tx),
    //         state,
    //         vec![], // middlewares (unused in new system)
    //         0,
    //     );

    //     let ctx = test_ctx.as_context();
    //     assert!(middleware.on_connect(ctx).await.is_ok());
    //     assert!(test_ctx.state.read().challenge.is_some());
    // }

    #[tokio::test]
    async fn test_subdomain_auth_matching_subdomain() {
        let keys = Keys::generate();
        // Use WebSocket URL format as required by RelayUrl
        let auth_url = "wss://example.com".to_string();
        let middleware = Nip42Middleware::with_url(auth_url.clone());

        let mut state = create_test_state(None);
        let challenge = "test_challenge".to_string();
        state.challenge = Some(challenge.clone());
        state.subdomain = Arc::new(Scope::named("test").unwrap()); // Connection is for test.example.com

        // Debug connection state before creating context
        let subdomain_str = match state.subdomain.as_ref() {
            Scope::Named { name, .. } => name.clone(),
            Scope::Default => "Default".to_string(),
        };
        println!("Test setup - subdomain: {subdomain_str}, auth_url: {auth_url}");

        // Auth event with correct subdomain (test.example.com)
        let auth_event = EventBuilder::new(Kind::Authentication, "")
            .tag(Tag::from_standardized(TagStandard::Challenge(challenge)))
            .tag(Tag::from_standardized(TagStandard::Relay(
                RelayUrl::parse("wss://test.example.com").unwrap(),
            )))
            .build_with_ctx(&Instant::now(), keys.public_key());
        let auth_event = keys.sign_event(auth_event).await.unwrap();

        // Debug auth event
        let client_url = auth_event
            .tags
            .iter()
            .find_map(|tag| match tag.as_standardized() {
                Some(TagStandard::Relay(r)) => Some(r.as_str_without_trailing_slash()),
                _ => None,
            })
            .unwrap_or("No relay URL found");
        println!("Auth event relay URL: {client_url}");

        let (mut test_ctx, _rx) = create_test_auth_context(
            "test_conn".to_string(),
            Some(ClientMessage::Auth(Cow::Owned(auth_event.clone()))),
            state,
        );

        let ctx = test_ctx.as_context();
        let result =
            <Nip42Middleware<()> as NostrMiddleware<()>>::process_inbound(&middleware, ctx).await;
        if let Err(e) = &result {
            println!("Auth failed with error: {e}");
        } else {
            println!("Auth succeeded!");
        }

        assert!(result.is_ok());
        assert_eq!(test_ctx.state.read().authed_pubkey, Some(keys.public_key()));
    }

    #[tokio::test]
    async fn test_subdomain_auth_wrong_subdomain() {
        let keys = Keys::generate();
        let auth_url = "wss://example.com".to_string();
        let middleware = Nip42Middleware::with_url(auth_url.clone());
        let mut state = create_test_state(None);
        let challenge = "test_challenge".to_string();
        state.challenge = Some(challenge.clone());
        state.subdomain = Arc::new(Scope::named("test").unwrap()); // Connection is for test.example.com

        println!("Wrong subdomain test - connection subdomain: test, auth_url: {auth_url}");

        // Auth event with WRONG subdomain (wrong.example.com)
        let auth_event = EventBuilder::new(Kind::Authentication, "")
            .tag(Tag::from_standardized(TagStandard::Challenge(challenge)))
            .tag(Tag::from_standardized(TagStandard::Relay(
                RelayUrl::parse("wss://wrong.example.com").unwrap(),
            )))
            .build_with_ctx(&Instant::now(), keys.public_key());
        let auth_event = keys.sign_event(auth_event).await.unwrap();

        let (mut test_ctx, _rx) = create_test_auth_context(
            "test_conn".to_string(),
            Some(ClientMessage::Auth(Cow::Owned(auth_event.clone()))),
            state,
        );

        let ctx = test_ctx.as_context();
        assert!(
            <Nip42Middleware<()> as NostrMiddleware<()>>::process_inbound(&middleware, ctx)
                .await
                .is_err()
        );
        assert_eq!(test_ctx.state.read().authed_pubkey, None);
    }

    #[tokio::test]
    async fn test_subdomain_auth_different_base_domain() {
        let keys = Keys::generate();
        let auth_url = "wss://example.com".to_string();
        let middleware = Nip42Middleware::with_url(auth_url.clone());
        let mut state = create_test_state(None);
        let challenge = "test_challenge".to_string();
        state.challenge = Some(challenge.clone());
        state.subdomain = Arc::new(Scope::named("test").unwrap()); // Connection is for test.example.com

        println!("Different base domain test - connection subdomain: test, auth_url: {auth_url}");

        // Auth event with wrong base domain (test.different.com)
        let auth_event = EventBuilder::new(Kind::Authentication, "")
            .tag(Tag::from_standardized(TagStandard::Challenge(challenge)))
            .tag(Tag::from_standardized(TagStandard::Relay(
                RelayUrl::parse("wss://test.different.com").unwrap(),
            )))
            .build_with_ctx(&Instant::now(), keys.public_key());
        let auth_event = keys.sign_event(auth_event).await.unwrap();

        let (mut test_ctx, _rx) = create_test_auth_context(
            "test_conn".to_string(),
            Some(ClientMessage::Auth(Cow::Owned(auth_event.clone()))),
            state,
        );

        let ctx = test_ctx.as_context();
        assert!(
            <Nip42Middleware<()> as NostrMiddleware<()>>::process_inbound(&middleware, ctx)
                .await
                .is_err()
        );
        assert_eq!(test_ctx.state.read().authed_pubkey, None);
    }
}
