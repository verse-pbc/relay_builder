//! Axum integration for WebSocket handlers
//!
//! This module provides easy integration with the Axum web framework.

use super::{
    connection::{handle_socket, ConnectionConfig},
    handler::{HandlerFactory, WebSocketHandler},
};
use axum::{
    extract::{
        ws::WebSocketUpgrade as AxumWebSocketUpgrade, ConnectInfo, FromRequestParts,
        OptionalFromRequestParts, State,
    },
    http::request::Parts,
    response::Response,
    Router,
};
use std::{net::SocketAddr, sync::Arc};
use tokio::sync::Semaphore;
use tracing::warn;

/// WebSocket upgrade extractor that can be used with Option<>
///
/// This allows handlers to accept both regular HTTP and WebSocket requests
/// on the same route.
pub struct WebSocketUpgrade {
    inner: AxumWebSocketUpgrade,
}

impl<S> FromRequestParts<S> for WebSocketUpgrade
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let inner = AxumWebSocketUpgrade::from_request_parts(parts, state)
            .await
            .map_err(axum::response::IntoResponse::into_response)?;
        Ok(Self { inner })
    }
}

impl<S> OptionalFromRequestParts<S> for WebSocketUpgrade
where
    S: Send + Sync,
{
    type Rejection = Response;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &S,
    ) -> Result<Option<Self>, Self::Rejection> {
        match AxumWebSocketUpgrade::from_request_parts(parts, state).await {
            Ok(inner) => Ok(Some(Self { inner })),
            Err(_) => Ok(None), // Failed WebSocket upgrade becomes None
        }
    }
}

/// Handler function for WebSocket upgrades
pub async fn ws_handler<F>(
    ws: AxumWebSocketUpgrade,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: axum::http::HeaderMap,
    State((factory, config, connection_limiter)): State<(
        Arc<F>,
        ConnectionConfig,
        Option<Arc<Semaphore>>,
    )>,
) -> Response
where
    F: HandlerFactory,
{
    ws.on_upgrade(move |socket| async move {
        // Try to acquire a connection permit
        let _permit = match &connection_limiter {
            Some(limiter) => match limiter.try_acquire() {
                Ok(permit) => Some(permit),
                Err(_) => {
                    warn!(
                        "Connection limit reached, rejecting connection from {}",
                        addr
                    );
                    return;
                }
            },
            None => None,
        };

        // Create a new handler instance for this connection with headers
        let handler = factory.create(&headers);

        // Handle the socket
        handle_socket(socket, addr, handler, config).await;
    })
}

/// Create an Axum route handler for WebSocket connections
///
/// This function returns a router with the WebSocket handler configured.
///
/// # Example
///
/// ```no_run
/// use axum::{extract::ws::{Message, WebSocket}, Router};
/// use futures_util::{stream::SplitSink, SinkExt};
/// use relay_builder::websocket::{websocket_route, HandlerFactory, WebSocketHandler, DisconnectReason, Utf8Bytes};
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
///         if let Some(sink) = &mut self.sink {
///             sink.send(Message::Text(format!("Echo: {}", text).into())).await?;
///         }
///         Ok(())
///     }
///
///     async fn on_disconnect(&mut self, reason: DisconnectReason) {
///         println!("Client disconnected: {:?}", reason);
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
///
/// #[tokio::main]
/// async fn main() {
///     let app = websocket_route("/ws", MyHandlerFactory);
///     
///     let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
///     axum::serve(listener, app).await.unwrap();
/// }
/// ```
pub fn websocket_route<F>(path: &str, factory: F) -> Router
where
    F: HandlerFactory,
{
    websocket_route_with_config(path, factory, ConnectionConfig::default())
}

/// Create an Axum route handler with custom configuration
///
/// This is like `websocket_route` but allows you to specify connection limits.
///
/// # Example
///
/// ```no_run
/// use axum::extract::ws::{Message, WebSocket};
/// use futures_util::stream::SplitSink;
/// use relay_builder::websocket::{websocket_route_with_config, ConnectionConfig, HandlerFactory, WebSocketHandler, DisconnectReason, Utf8Bytes};
/// use std::net::SocketAddr;
/// use anyhow::Result;
///
/// struct MyHandler;
///
/// impl WebSocketHandler for MyHandler {
///     async fn on_connect(&mut self, _addr: SocketAddr, _sink: SplitSink<WebSocket, Message>) -> Result<()> {
///         Ok(())
///     }
///     async fn on_message(&mut self, _text: Utf8Bytes) -> Result<()> {
///         Ok(())
///     }
///     async fn on_disconnect(&mut self, _reason: DisconnectReason) {}
/// }
///
/// struct MyHandlerFactory;
///
/// impl HandlerFactory for MyHandlerFactory {
///     type Handler = MyHandler;
///     fn create(&self, _headers: &axum::http::HeaderMap) -> Self::Handler {
///         MyHandler
///     }
/// }
///
/// let config = ConnectionConfig {
///     max_connections: Some(1000),
///     max_connection_duration: None,
///     idle_timeout: None,
/// };
///
/// let app = websocket_route_with_config("/ws", MyHandlerFactory, config);
/// ```
pub fn websocket_route_with_config<F>(path: &str, factory: F, config: ConnectionConfig) -> Router
where
    F: HandlerFactory,
{
    use axum::routing::get;

    let connection_limiter = config
        .max_connections
        .map(|limit| Arc::new(Semaphore::new(limit)));

    Router::new().route(path, get(ws_handler::<F>)).with_state((
        Arc::new(factory),
        config,
        connection_limiter,
    ))
}

/// Handle a WebSocket upgrade directly with a handler instance
///
/// This is useful when you want to handle WebSocket upgrades conditionally,
/// such as when the same route serves both HTTP and WebSocket requests.
///
/// # Example
///
/// ```no_run
/// use axum::{
///     extract::{ConnectInfo, ws::{Message, WebSocket}},
///     response::{Html, IntoResponse, Response},
/// };
/// use futures_util::stream::SplitSink;
/// use std::net::SocketAddr;
/// use relay_builder::websocket::{WebSocketUpgrade, HandlerFactory, WebSocketHandler, DisconnectReason, Utf8Bytes};
/// use anyhow::Result;
///
/// struct MyHandler;
///
/// impl WebSocketHandler for MyHandler {
///     async fn on_connect(&mut self, _addr: SocketAddr, _sink: SplitSink<WebSocket, Message>) -> Result<()> {
///         Ok(())
///     }
///     async fn on_message(&mut self, _text: Utf8Bytes) -> Result<()> {
///         Ok(())
///     }
///     async fn on_disconnect(&mut self, _reason: DisconnectReason) {}
/// }
///
/// struct MyHandlerFactory;
///
/// impl HandlerFactory for MyHandlerFactory {
///     type Handler = MyHandler;
///     fn create(&self, _headers: &axum::http::HeaderMap) -> Self::Handler {
///         MyHandler
///     }
/// }
///
/// async fn flexible_handler(
///     ConnectInfo(addr): ConnectInfo<SocketAddr>,
///     headers: axum::http::HeaderMap,
///     ws: Option<WebSocketUpgrade>,
/// ) -> Response {
///     match ws {
///         Some(ws) => {
///             // Handle WebSocket upgrade
///             let handler = MyHandlerFactory.create(&headers);
///             relay_builder::websocket::handle_upgrade(ws, addr, handler).await
///         }
///         None => {
///             // Handle regular HTTP request
///             Html("<h1>Hello from HTTP!</h1>").into_response()
///         }
///     }
/// }
/// ```
pub async fn handle_upgrade<H>(ws: WebSocketUpgrade, addr: SocketAddr, handler: H) -> Response
where
    H: WebSocketHandler,
{
    handle_upgrade_with_config(ws, addr, handler, ConnectionConfig::default()).await
}

/// Handle a WebSocket upgrade with custom configuration
pub async fn handle_upgrade_with_config<H>(
    ws: WebSocketUpgrade,
    addr: SocketAddr,
    handler: H,
    config: ConnectionConfig,
) -> Response
where
    H: WebSocketHandler,
{
    ws.inner.on_upgrade(move |socket| async move {
        handle_socket(socket, addr, handler, config).await;
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DisconnectReason, Utf8Bytes};
    use axum::extract::ws::{Message, WebSocket};
    use futures_util::stream::SplitSink;

    struct TestHandler;

    impl WebSocketHandler for TestHandler {
        async fn on_connect(
            &mut self,
            _addr: SocketAddr,
            _sink: SplitSink<WebSocket, Message>,
        ) -> anyhow::Result<()> {
            Ok(())
        }

        async fn on_message(&mut self, _text: Utf8Bytes) -> anyhow::Result<()> {
            Ok(())
        }

        async fn on_disconnect(&mut self, _reason: DisconnectReason) {}
    }

    struct TestFactory;

    impl HandlerFactory for TestFactory {
        type Handler = TestHandler;

        fn create(&self, _headers: &axum::http::HeaderMap) -> Self::Handler {
            TestHandler
        }
    }

    #[test]
    fn test_websocket_route_creation() {
        let _app = websocket_route("/ws", TestFactory);
        // If this compiles and doesn't panic, the route was created successfully
    }

    #[test]
    fn test_websocket_route_with_config_creation() {
        let config = ConnectionConfig {
            max_connections: Some(100),
            max_connection_duration: None,
            idle_timeout: None,
        };
        let _app = websocket_route_with_config("/ws", TestFactory, config);
        // If this compiles and doesn't panic, the route was created successfully
    }

    #[tokio::test]
    async fn test_semaphore_connection_limiting() {
        let semaphore = Arc::new(Semaphore::new(2));

        // Acquire first permit
        let permit1 = semaphore.try_acquire().unwrap();

        // Acquire second permit
        let _permit2 = semaphore.try_acquire().unwrap();

        // Third should fail - no more permits available
        assert!(semaphore.try_acquire().is_err());

        // Drop first permit
        drop(permit1);

        // Now we should be able to acquire again
        assert!(semaphore.try_acquire().is_ok());
    }
}
