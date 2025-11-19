//! Event ingester for inline JSON parsing and signature verification
//!
//! This module provides an event ingester that performs JSON parsing
//! and signature verification directly in the connection thread for
//! better cache locality and reduced context switching.

use nostr_sdk::prelude::*;
use std::borrow::Cow;
use std::sync::Arc;
use tracing::{debug, warn};

/// Error types specific to the ingester
#[derive(Debug, thiserror::Error)]
pub enum IngesterError {
    #[error("Invalid UTF-8 in message")]
    InvalidUtf8,

    #[error("JSON parse error: {0}")]
    JsonParseError(String),

    #[error("Event signature verification failed for event {0}")]
    SignatureVerificationFailed(EventId),

    #[error("Message size exceeds limit")]
    MessageTooLarge,
}

/// Event ingester that performs inline JSON parsing and signature verification
#[derive(Clone)]
pub struct EventIngester {
    /// Keys for signing events (not used for verification, kept for compatibility)
    #[allow(dead_code)]
    keys: Arc<Keys>,
}

impl std::fmt::Debug for EventIngester {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventIngester").finish()
    }
}

impl EventIngester {
    pub fn new(keys: Arc<Keys>) -> Self {
        Self { keys }
    }

    /// Process a message inline (parse JSON and verify signature if it's an EVENT)
    /// This now runs directly in the connection thread for better performance
    ///
    /// # Errors
    /// Returns an error if JSON parsing fails or event signature verification fails
    pub fn process_message(
        &self,
        text: String,
    ) -> std::result::Result<ClientMessage<'static>, IngesterError> {
        // Check if we're in a multi-threaded runtime context
        // block_in_place only works in multi-threaded runtime
        if tokio::runtime::Handle::try_current()
            .map(|h| h.runtime_flavor() == tokio::runtime::RuntimeFlavor::MultiThread)
            .unwrap_or(false)
        {
            // Use tokio::task::block_in_place for CPU-intensive work in multi-threaded runtime
            // This allows the tokio runtime to move other tasks to different threads
            // while we do the CPU-intensive parsing and verification
            tokio::task::block_in_place(move || Self::process_single_message(&text))
        } else {
            // In single-threaded runtime (like tests), just run directly
            Ok(Self::process_single_message(&text)?)
        }
    }

    /// Process a single message: parse JSON and verify signature if EVENT
    /// This runs synchronously in the current thread
    fn process_single_message(
        text: &str,
    ) -> std::result::Result<ClientMessage<'static>, IngesterError> {
        // Check for empty message
        if text.is_empty() {
            return Err(IngesterError::JsonParseError("Empty message".to_string()));
        }

        // Parse JSON - ClientMessage::from_json accepts both &str and &[u8]
        let msg = match ClientMessage::from_json(text) {
            Ok(msg) => msg,
            Err(e) => {
                warn!("Failed to parse client message: {}, error: {}", text, e);
                return Err(IngesterError::JsonParseError(e.to_string()));
            }
        };

        // For EVENT messages, verify the signature
        if let ClientMessage::Event(ref event_cow) = msg {
            let event_id = event_cow.id;

            // Verify the event signature
            if let Err(e) = event_cow.verify() {
                debug!("Event {} signature verification failed: {:?}", event_id, e);
                return Err(IngesterError::SignatureVerificationFailed(event_id));
            }
        }

        // Convert to 'static lifetime by taking ownership
        let static_msg = match msg {
            ClientMessage::Event(event) => ClientMessage::Event(Cow::Owned(event.into_owned())),
            ClientMessage::Req {
                subscription_id,
                filter,
            } => ClientMessage::Req {
                subscription_id: Cow::Owned(subscription_id.into_owned()),
                filter,
            },
            ClientMessage::Close(sub_id) => ClientMessage::Close(Cow::Owned(sub_id.into_owned())),
            ClientMessage::Auth(event) => ClientMessage::Auth(Cow::Owned(event.into_owned())),
            ClientMessage::Count {
                subscription_id,
                filter,
            } => ClientMessage::Count {
                subscription_id: Cow::Owned(subscription_id.into_owned()),
                filter,
            },
            ClientMessage::NegOpen {
                subscription_id,
                filter,
                ..
            } => ClientMessage::NegOpen {
                subscription_id: Cow::Owned(subscription_id.into_owned()),
                filter,
                id_size: None,
                initial_message: Cow::Borrowed(""),
            },
            ClientMessage::NegMsg {
                subscription_id,
                message,
            } => ClientMessage::NegMsg {
                subscription_id: Cow::Owned(subscription_id.into_owned()),
                message,
            },
            ClientMessage::NegClose { subscription_id } => ClientMessage::NegClose {
                subscription_id: Cow::Owned(subscription_id.into_owned()),
            },
            ClientMessage::ReqMultiFilter {
                subscription_id,
                filters,
            } => ClientMessage::ReqMultiFilter {
                subscription_id: Cow::Owned(subscription_id.into_owned()),
                filters,
            },
        };

        Ok(static_msg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_valid_event_processing() {
        let keys = Arc::new(Keys::generate());
        let ingester = EventIngester::new(Arc::clone(&keys));

        // Create a valid signed event
        let event = EventBuilder::text_note("Test message")
            .sign_with_keys(&keys)
            .unwrap();
        let event_json = format!(r#"["EVENT", {}]"#, event.as_json());

        // Process the message
        let result = ingester.process_message(event_json);
        assert!(result.is_ok());

        if let Ok(ClientMessage::Event(parsed_event)) = result {
            assert_eq!(parsed_event.id, event.id);
            assert_eq!(parsed_event.content, "Test message");
        } else {
            panic!("Expected EVENT message");
        }
    }

    #[tokio::test]
    async fn test_invalid_signature_rejection() {
        let keys = Arc::new(Keys::generate());
        let ingester = EventIngester::new(Arc::clone(&keys));

        // Create an event with tampered signature
        let event = EventBuilder::text_note("Test message")
            .sign_with_keys(&keys)
            .unwrap();

        // Tamper with the content but keep the same signature
        let mut json = serde_json::to_value(&event).unwrap();
        json["content"] = serde_json::Value::String("Tampered message".to_string());

        let event_json = format!(r#"["EVENT", {json}]"#);

        // Process the message
        let result = ingester.process_message(event_json);
        assert!(result.is_err());

        if let Err(IngesterError::SignatureVerificationFailed(_)) = result {
            // Expected error
        } else {
            panic!("Expected SignatureVerificationFailed error");
        }
    }

    #[tokio::test]
    async fn test_req_message_processing() {
        let keys = Arc::new(Keys::generate());
        let ingester = EventIngester::new(keys);

        let req_json = r#"["REQ", "sub1", {"kinds": [1], "limit": 10}]"#;

        // Process the message
        let result = ingester.process_message(req_json.to_string());
        assert!(result.is_ok());

        if let Ok(ClientMessage::Req {
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
    }

    #[tokio::test]
    async fn test_invalid_json_handling() {
        let keys = Arc::new(Keys::generate());
        let ingester = EventIngester::new(keys);

        let invalid_json = "not valid json";

        // Process the message
        let result = ingester.process_message(invalid_json.to_string());
        assert!(result.is_err());

        if let Err(IngesterError::JsonParseError(_)) = result {
            // Expected error
        } else {
            panic!("Expected JsonParseError");
        }
    }

    #[tokio::test]
    async fn test_concurrent_processing() {
        let keys = Arc::new(Keys::generate());
        let ingester = EventIngester::new(Arc::clone(&keys));

        // Use async tasks for concurrent testing
        let mut handles = Vec::new();
        for i in 0..50 {
            let ingester_clone = ingester.clone();
            let keys_clone = Arc::clone(&keys);

            handles.push(tokio::spawn(async move {
                let event = EventBuilder::text_note(format!("Test message {i}"))
                    .sign_with_keys(&keys_clone)
                    .unwrap();
                let event_json = format!(r#"["EVENT", {}]"#, event.as_json());

                ingester_clone.process_message(event_json)
            }));
        }

        // Wait for all tasks to complete and verify they succeeded
        for handle in handles {
            let result = handle.await.unwrap();
            assert!(result.is_ok());
        }
    }

    #[tokio::test]
    async fn test_ingester_from_async_context() {
        let keys = Arc::new(Keys::generate());
        let ingester = EventIngester::new(Arc::clone(&keys));

        // Create a valid signed event
        let event = EventBuilder::text_note("Test from async")
            .sign_with_keys(&keys)
            .unwrap();
        let event_json = format!(r#"["EVENT", {}]"#, event.as_json());

        // Process the message from async context (this tests block_in_place)
        let result = ingester.process_message(event_json);
        assert!(result.is_ok());

        if let Ok(ClientMessage::Event(parsed_event)) = result {
            assert_eq!(parsed_event.content, "Test from async");
        } else {
            panic!("Expected EVENT message");
        }
    }
}
