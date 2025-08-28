//! WebSocket handler traits
//!
//! This module defines the core traits for implementing WebSocket handlers.

use anyhow::Result;
use axum::extract::ws::{Message, WebSocket};
use futures_util::stream::SplitSink;
use std::future::Future;
use std::net::SocketAddr;

// Re-export Utf8Bytes from axum for the API
pub use axum::extract::ws::Utf8Bytes;

/// Reason for WebSocket disconnection
#[derive(Debug, Clone)]
pub enum DisconnectReason {
    /// Normal close with code and reason
    Close(u16, String),
    /// Connection error
    Error(String),
    /// Connection timeout
    Timeout(String),
    /// Unknown disconnection
    Unknown,
}

impl DisconnectReason {
    /// Create from close frame
    pub fn from_close_frame(frame: Option<axum::extract::ws::CloseFrame>) -> Self {
        match frame {
            Some(frame) => Self::Close(frame.code, frame.reason.to_string()),
            None => Self::Unknown,
        }
    }
}

/// Trait for handling WebSocket connections
///
/// Each connection gets its own instance of the handler, allowing for
/// natural per-connection state management.
///
/// # Example
///
/// ```no_run
/// use relay_builder::websocket::{WebSocketHandler, DisconnectReason, Utf8Bytes};
/// use axum::extract::ws::{WebSocket, Message};
/// use futures_util::{stream::SplitSink, SinkExt};
/// use std::net::SocketAddr;
/// use anyhow::Result;
///
/// struct MyHandler {
///     addr: SocketAddr,
///     sink: Option<SplitSink<WebSocket, Message>>,
/// }
///
/// impl WebSocketHandler for MyHandler {
///     async fn on_connect(
///         &mut self,
///         addr: SocketAddr,
///         sink: SplitSink<WebSocket, Message>,
///     ) -> Result<()> {
///         self.addr = addr;
///         self.sink = Some(sink);
///
///         // Send welcome message
///         if let Some(sink) = &mut self.sink {
///             sink.send(Message::Text("Welcome!".into())).await?;
///         }
///         Ok(())
///     }
///
///     async fn on_message(&mut self, text: Utf8Bytes) -> Result<()> {
///         // Echo the message back
///         if let Some(sink) = &mut self.sink {
///             sink.send(Message::Text(format!("Echo: {}", text).into())).await?;
///         }
///         Ok(())
///     }
///
///     async fn on_disconnect(&mut self, reason: DisconnectReason) {
///         println!("Client {} disconnected: {:?}", self.addr, reason);
///     }
/// }
/// ```
pub trait WebSocketHandler: Send + 'static {
    /// Called when a new WebSocket connection is established
    ///
    /// The sink should be stored for sending messages back to the client.
    fn on_connect(
        &mut self,
        remote_addr: SocketAddr,
        sink: SplitSink<WebSocket, Message>,
    ) -> impl Future<Output = Result<()>> + Send;

    /// Called when a text message is received from the client
    ///
    /// Binary messages are ignored by the framework.
    /// The text is provided as `Utf8Bytes` to avoid unnecessary allocations.
    fn on_message(&mut self, text: Utf8Bytes) -> impl Future<Output = Result<()>> + Send;

    /// Called when the WebSocket connection is closed
    ///
    /// This is called for both normal closure and errors.
    fn on_disconnect(&mut self, reason: DisconnectReason) -> impl Future<Output = ()> + Send;
}

/// Factory for creating WebSocket handler instances
///
/// A new handler instance is created for each WebSocket connection.
///
/// # Example
///
/// ```no_run
/// use relay_builder::websocket::{WebSocketHandler, HandlerFactory, DisconnectReason, Utf8Bytes};
/// use axum::extract::ws::{WebSocket, Message};
/// use futures_util::stream::SplitSink;
/// use std::net::SocketAddr;
/// use anyhow::Result;
///
/// struct MyHandler {
///     addr: SocketAddr,
///     sink: Option<SplitSink<WebSocket, Message>>,
/// }
///
/// impl WebSocketHandler for MyHandler {
///     async fn on_connect(
///         &mut self,
///         addr: SocketAddr,
///         sink: SplitSink<WebSocket, Message>,
///     ) -> Result<()> {
///         self.addr = addr;
///         self.sink = Some(sink);
///         Ok(())
///     }
///
///     async fn on_message(&mut self, text: Utf8Bytes) -> Result<()> {
///         Ok(())
///     }
///
///     async fn on_disconnect(&mut self, reason: DisconnectReason) {
///     }
/// }
///
/// struct MyHandlerFactory;
///
/// impl HandlerFactory for MyHandlerFactory {
///     type Handler = MyHandler;
///
///     fn create(&self, _headers: &axum::http::HeaderMap) -> Self::Handler {
///         MyHandler {
///             addr: "0.0.0.0:0".parse().unwrap(),
///             sink: None,
///         }
///     }
/// }
/// ```
pub trait HandlerFactory: Send + Sync + 'static {
    /// The handler type to create
    type Handler: WebSocketHandler;

    /// Create a new handler instance for a connection
    ///
    /// # Arguments
    /// * `headers` - HTTP headers from the upgrade request, useful for extracting
    ///   context like subdomain, authentication tokens, etc.
    fn create(&self, headers: &axum::http::HeaderMap) -> Self::Handler;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt;

    #[test]
    fn test_disconnect_reason_display() {
        let reason = DisconnectReason::Unknown;
        assert_eq!(format!("{reason:?}"), "Unknown");

        let reason = DisconnectReason::Error("test error".to_string());
        assert!(format!("{reason:?}").contains("test error"));

        let reason = DisconnectReason::Close(1000, "normal".to_string());
        assert!(format!("{reason:?}").contains("1000"));
        assert!(format!("{reason:?}").contains("normal"));

        let reason = DisconnectReason::Timeout("idle timeout".to_string());
        assert!(format!("{reason:?}").contains("idle timeout"));
    }

    #[test]
    fn test_disconnect_reason_variants() {
        // Test Debug trait is implemented
        fn assert_debug<T: fmt::Debug>(_: &T) {}

        // Test that we can create different variants
        assert_debug(&DisconnectReason::Unknown);
        assert_debug(&DisconnectReason::Error("error".to_string()));
        assert_debug(&DisconnectReason::Close(1001, "going away".to_string()));
        assert_debug(&DisconnectReason::Timeout("max duration".to_string()));
    }

    #[test]
    fn test_disconnect_reason_from_close_frame() {
        use axum::extract::ws::CloseFrame;

        // Test with None
        let reason = DisconnectReason::from_close_frame(None);
        matches!(reason, DisconnectReason::Unknown);

        // Test with close frame
        let frame = CloseFrame {
            code: 1000,
            reason: "test".into(),
        };
        let reason = DisconnectReason::from_close_frame(Some(frame));
        matches!(reason, DisconnectReason::Close(1000, _));
    }
}
