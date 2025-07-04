//! Message conversion utilities

use anyhow::Result;
use nostr_sdk::prelude::*;
use std::borrow::Cow;
use websocket_builder::MessageConverter;

/// Message converter for Nostr protocol messages
#[derive(Clone, Debug)]
pub struct NostrMessageConverter;

impl<'a> MessageConverter<ClientMessage<'a>, RelayMessage<'a>> for NostrMessageConverter {
    fn inbound_from_bytes(&self, bytes: &[u8]) -> Result<Option<ClientMessage<'a>>> {
        if bytes.is_empty() {
            return Ok(None);
        }

        // Convert bytes to string first
        let message = match std::str::from_utf8(bytes) {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("Invalid UTF-8 in client message: {}", e);
                return Ok(None);
            }
        };

        match ClientMessage::from_json(message) {
            Ok(sdk_msg) => Ok(Some(sdk_msg)),
            Err(e) => {
                if message.trim().is_empty() {
                    Ok(None)
                } else {
                    tracing::warn!("Failed to parse client message: {}, error: {}", message, e);
                    Err(anyhow::anyhow!("Failed to parse client message: {}", e))
                }
            }
        }
    }

    fn outbound_to_bytes(&self, message: RelayMessage<'a>) -> Result<Cow<'_, [u8]>> {
        let json = message.as_json();
        Ok(Cow::Owned(json.into_bytes()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nostr_sdk::{EventBuilder, Keys, Kind, RelayUrl, SubscriptionId};

    #[test]
    fn test_outbound_to_bytes() {
        let converter = NostrMessageConverter;

        // Test with EVENT message
        let keys = Keys::generate();
        let event = EventBuilder::text_note("Hello")
            .sign_with_keys(&keys)
            .unwrap();
        let message = RelayMessage::event(SubscriptionId::new("test"), event);

        let result = converter.outbound_to_bytes(message).unwrap();
        let json = String::from_utf8(result.into_owned()).unwrap();
        assert!(json.contains("EVENT"));
        assert!(json.contains("test"));

        // Test with NOTICE message
        let notice = RelayMessage::notice("Test notice");
        let result = converter.outbound_to_bytes(notice).unwrap();
        let json = String::from_utf8(result.into_owned()).unwrap();
        assert!(json.contains("NOTICE"));
        assert!(json.contains("Test notice"));

        // Test with EOSE message
        let eose = RelayMessage::eose(SubscriptionId::new("sub1"));
        let result = converter.outbound_to_bytes(eose).unwrap();
        let json = String::from_utf8(result.into_owned()).unwrap();
        assert!(json.contains("EOSE"));
        assert!(json.contains("sub1"));

        // Test with OK message
        let ok = RelayMessage::ok(EventId::all_zeros(), true, "saved");
        let result = converter.outbound_to_bytes(ok).unwrap();
        let json = String::from_utf8(result.into_owned()).unwrap();
        assert!(json.contains("OK"));
        assert!(json.contains("true"));
        assert!(json.contains("saved"));
    }

    #[test]
    fn test_inbound_from_bytes_valid_messages() {
        let converter = NostrMessageConverter;

        // Test EVENT message
        let keys = Keys::generate();
        let event = EventBuilder::text_note("Test")
            .sign_with_keys(&keys)
            .unwrap();
        let event_json = format!(r#"["EVENT", {}]"#, event.as_json());

        let result = converter.inbound_from_bytes(event_json.as_bytes()).unwrap();
        assert!(result.is_some());
        if let Some(ClientMessage::Event(parsed_event)) = result {
            assert_eq!(parsed_event.id, event.id);
        } else {
            panic!("Expected EVENT message");
        }

        // Test REQ message
        let req_json = r#"["REQ", "sub1", {"kinds": [1], "limit": 10}]"#;
        let result = converter.inbound_from_bytes(req_json.as_bytes()).unwrap();
        assert!(result.is_some());
        if let Some(ClientMessage::Req {
            subscription_id,
            filter,
        }) = result
        {
            assert_eq!(subscription_id.as_str(), "sub1");
            assert!(filter.kinds.as_ref().unwrap().contains(&Kind::TextNote));
            assert_eq!(filter.limit, Some(10));
        } else {
            panic!("Expected REQ message");
        }

        // Test CLOSE message
        let close_json = r#"["CLOSE", "sub1"]"#;
        let result = converter.inbound_from_bytes(close_json.as_bytes()).unwrap();
        assert!(result.is_some());
        if let Some(ClientMessage::Close(sub_id)) = result {
            assert_eq!(sub_id.as_str(), "sub1");
        } else {
            panic!("Expected CLOSE message");
        }
    }

    #[test]
    fn test_inbound_from_bytes_empty_message() {
        let converter = NostrMessageConverter;

        // Test empty bytes
        let result = converter.inbound_from_bytes(&[]).unwrap();
        assert!(result.is_none());

        // Test whitespace only
        let result = converter
            .inbound_from_bytes("   \n\t  ".as_bytes())
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_inbound_from_bytes_invalid_json() {
        let converter = NostrMessageConverter;

        // Test invalid JSON
        let result = converter.inbound_from_bytes(b"not json");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Failed to parse client message"));

        // Test invalid message format
        let result = converter.inbound_from_bytes(br#"{"invalid": "format"}"#);
        assert!(result.is_err());

        // Test unknown message type
        let result = converter.inbound_from_bytes(br#"["UNKNOWN", "data"]"#);
        assert!(result.is_err());

        // Test invalid UTF-8
        let invalid_utf8 = &[0xFF, 0xFE];
        let result = converter.inbound_from_bytes(invalid_utf8).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_auth_message() {
        let converter = NostrMessageConverter;

        // Test AUTH message
        let keys = Keys::generate();
        let relay_url = RelayUrl::parse("wss://relay.example.com").unwrap();
        let auth_event = EventBuilder::auth("challenge", relay_url)
            .sign_with_keys(&keys)
            .unwrap();
        let auth_json = format!(r#"["AUTH", {}]"#, auth_event.as_json());

        let result = converter.inbound_from_bytes(auth_json.as_bytes()).unwrap();
        assert!(result.is_some());
        if let Some(ClientMessage::Auth(event)) = result {
            assert_eq!(event.kind, Kind::Authentication);
        } else {
            panic!("Expected AUTH message");
        }
    }
}
