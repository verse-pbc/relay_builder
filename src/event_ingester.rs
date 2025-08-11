//! Event ingester for combined JSON parsing and signature verification
//!
//! This module provides a high-performance ingester that combines JSON parsing
//! and signature verification in a single thread pool, moving CPU-bound work
//! off the I/O threads for better performance.

use nostr_sdk::prelude::*;
use rayon::prelude::*;
use std::borrow::Cow;
use std::sync::Arc;
use tokio::sync::oneshot;
use tracing::{debug, error, warn};

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

    #[error("Ingester channel closed")]
    ChannelClosed,
}

/// Request to process a message
struct ProcessRequest {
    text: String,
    response: oneshot::Sender<std::result::Result<ClientMessage<'static>, IngesterError>>,
}

/// Event ingester that combines JSON parsing and signature verification
#[derive(Clone)]
pub struct EventIngester {
    /// Keys for signing events (not used for verification, kept for compatibility)
    #[allow(dead_code)]
    keys: Arc<Keys>,
    /// Channel for sending processing requests
    process_sender: flume::Sender<ProcessRequest>,
}

impl std::fmt::Debug for EventIngester {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EventIngester").finish()
    }
}

impl EventIngester {
    pub fn new(keys: Arc<Keys>) -> Self {
        // Create processing channel with reasonable capacity
        let (process_sender, process_receiver) = flume::bounded::<ProcessRequest>(10000);
        // Spawn the processing thread
        std::thread::spawn(move || {
            Self::run_processor(&process_receiver);
        });

        Self {
            keys,
            process_sender,
        }
    }

    /// Process a message (parse JSON and verify signature if it's an EVENT)
    pub async fn process_message(
        &self,
        text: String,
    ) -> std::result::Result<ClientMessage<'static>, IngesterError> {
        // Create oneshot channel for response
        let (tx, rx) = oneshot::channel();

        // Send processing request
        self.process_sender
            .send(ProcessRequest { text, response: tx })
            .map_err(|_| IngesterError::ChannelClosed)?;

        // Wait for response asynchronously - clean and efficient!
        rx.await.map_err(|_| IngesterError::ChannelClosed)?
    }

    /// Run the processor thread that handles parsing and verification
    fn run_processor(receiver: &flume::Receiver<ProcessRequest>) {
        #[cfg(debug_assertions)]
        debug!("Event ingester processor started");

        // Initialize rayon thread pool for CPU-bound work
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(num_cpus::get())
            .thread_name(|idx| format!("ingester-{idx}"))
            .build()
            .expect("Failed to create rayon thread pool");

        // Process messages one at a time to ensure fair distribution
        pool.install(|| {
            receiver.into_iter().par_bridge().for_each(|request| {
                let ProcessRequest { text, response } = request;

                // Process the message
                let result = Self::process_single_message(&text);

                // Send the result back (ignore send errors if receiver dropped)
                let _ = response.send(result);
            });
        });

        #[cfg(debug_assertions)]
        debug!("Event ingester processor stopped");
    }

    /// Process a single message: parse JSON and verify signature if EVENT
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
                // We already have the text as &str, no need for UTF-8 conversion
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
        let result = ingester.process_message(event_json).await;
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
        let result = ingester.process_message(event_json).await;
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
        let result = ingester.process_message(req_json.to_string()).await;
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
        let result = ingester.process_message(invalid_json.to_string()).await;
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

                ingester_clone.process_message(event_json).await
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
        let result = ingester.process_message(event_json).await;
        assert!(result.is_ok());

        if let Ok(ClientMessage::Event(parsed_event)) = result {
            assert_eq!(parsed_event.content, "Test from async");
        } else {
            panic!("Expected EVENT message");
        }
    }
}
