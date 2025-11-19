//! WebSocket connection handling
//!
//! This module contains the core logic for managing WebSocket connections.

use super::handler::{DisconnectReason, WebSocketHandler};
use axum::extract::ws::{Message, WebSocket};
use futures_util::StreamExt;
use std::{net::SocketAddr, time::Duration};
use tracing::{debug, error, trace};

/// Configuration for WebSocket connections
#[derive(Debug, Clone, Default)]
pub struct ConnectionConfig {
    /// Maximum number of concurrent connections (None for unlimited)
    pub max_connections: Option<usize>,
    /// Maximum duration a connection can stay open (None for unlimited)
    pub max_connection_duration: Option<Duration>,
    /// Timeout for idle connections - disconnects after no messages (None for unlimited)
    pub idle_timeout: Option<Duration>,
}

/// Handle a WebSocket connection
///
/// This function manages the lifecycle of a WebSocket connection:
/// 1. Calls `on_connect` when the connection is established
/// 2. Routes incoming text messages to `on_message`
/// 3. Handles ping/pong automatically (via axum)
/// 4. Enforces connection timeouts (max duration and idle)
/// 5. Calls `on_disconnect` when the connection closes
#[allow(clippy::too_many_lines)]
pub async fn handle_socket<H: WebSocketHandler>(
    socket: WebSocket,
    addr: SocketAddr,
    mut handler: H,
    config: ConnectionConfig,
) {
    use tokio::time::{timeout, Instant};

    debug!("New WebSocket connection from {}", addr);

    let (ws_sink, mut ws_stream) = socket.split();

    // Call on_connect with the WebSocket sink directly
    if let Err(e) = handler.on_connect(addr, ws_sink).await {
        error!("Handler on_connect error: {}", e);
        return;
    }

    // Track connection start time and last activity
    let connection_start = Instant::now();
    let mut last_activity = Instant::now();

    // Process incoming messages with timeout handling
    loop {
        // Calculate the next timeout deadline
        let mut next_timeout = None;
        let mut timeout_reason = None;

        // Check max connection duration
        if let Some(max_duration) = config.max_connection_duration {
            let elapsed = connection_start.elapsed();
            if elapsed >= max_duration {
                debug!("Connection exceeded max duration: {:?}", max_duration);
                handler
                    .on_disconnect(DisconnectReason::Timeout(
                        "Maximum connection duration exceeded".to_string(),
                    ))
                    .await;
                break;
            }
            let remaining = max_duration - elapsed;
            next_timeout = Some(remaining);
            timeout_reason = Some("max_duration");
        }

        // Check idle timeout
        if let Some(idle_duration) = config.idle_timeout {
            let idle_elapsed = last_activity.elapsed();
            if idle_elapsed >= idle_duration {
                debug!("Connection idle timeout: {:?}", idle_duration);
                handler
                    .on_disconnect(DisconnectReason::Timeout("Idle timeout".to_string()))
                    .await;
                break;
            }
            let idle_remaining = idle_duration - idle_elapsed;

            // Use the shorter timeout
            match next_timeout {
                None => {
                    next_timeout = Some(idle_remaining);
                    timeout_reason = Some("idle");
                }
                Some(existing) if idle_remaining < existing => {
                    next_timeout = Some(idle_remaining);
                    timeout_reason = Some("idle");
                }
                _ => {}
            }
        }

        // Wait for next message with timeout if configured
        let msg = if let Some(timeout_duration) = next_timeout {
            if let Ok(msg) = timeout(timeout_duration, ws_stream.next()).await {
                msg
            } else {
                // Timeout occurred
                match timeout_reason {
                    Some("idle") => {
                        debug!("Connection idle timeout");
                        handler
                            .on_disconnect(DisconnectReason::Timeout("Idle timeout".to_string()))
                            .await;
                    }
                    Some("max_duration") => {
                        debug!("Connection max duration timeout");
                        handler
                            .on_disconnect(DisconnectReason::Timeout(
                                "Maximum connection duration exceeded".to_string(),
                            ))
                            .await;
                    }
                    _ => {
                        handler
                            .on_disconnect(DisconnectReason::Timeout(
                                "Connection timeout".to_string(),
                            ))
                            .await;
                    }
                }
                break;
            }
        } else {
            ws_stream.next().await
        };

        // Process the message if we got one
        match msg {
            Some(Ok(Message::Text(text))) => {
                trace!("Received text message: {} bytes", text.len());
                last_activity = Instant::now(); // Reset idle timer
                if let Err(e) = handler.on_message(text).await {
                    error!("Handler on_message error: {}", e);
                    break;
                }
            }
            Some(Ok(Message::Binary(_))) => {
                trace!("Ignoring binary message");
                last_activity = Instant::now(); // Reset idle timer even for binary
                                                // Ignore binary messages as per design
            }
            Some(Ok(Message::Close(frame))) => {
                debug!("Received close frame: {:?}", frame);
                let reason = DisconnectReason::from_close_frame(frame);
                handler.on_disconnect(reason).await;
                break;
            }
            Some(Ok(Message::Ping(_) | Message::Pong(_))) => {
                // Axum handles ping/pong automatically
                trace!("Received ping/pong");
                last_activity = Instant::now(); // Reset idle timer for ping/pong
            }
            Some(Err(e)) => {
                // Check if this is a connection reset error
                // This is a common occurrence when clients disconnect abruptly
                let error_str = e.to_string();

                if error_str.contains("Connection reset") {
                    debug!("WebSocket connection reset: {}", e);
                } else {
                    error!("WebSocket error: {}", e);
                }
                handler
                    .on_disconnect(DisconnectReason::Error(error_str))
                    .await;
                break;
            }
            None => {
                // Stream ended
                handler.on_disconnect(DisconnectReason::Unknown).await;
                break;
            }
        }
    }

    debug!("WebSocket connection from {} closed", addr);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_connection_config_default() {
        let config = ConnectionConfig::default();
        assert_eq!(config.max_connections, None);
        assert_eq!(config.max_connection_duration, None);
        assert_eq!(config.idle_timeout, None);
    }

    #[test]
    fn test_connection_config_with_limit() {
        let config = ConnectionConfig {
            max_connections: Some(100),
            max_connection_duration: None,
            idle_timeout: None,
        };
        assert_eq!(config.max_connections, Some(100));
    }

    #[test]
    fn test_connection_config_with_timeouts() {
        let config = ConnectionConfig {
            max_connections: None,
            max_connection_duration: Some(Duration::from_secs(300)),
            idle_timeout: Some(Duration::from_secs(60)),
        };
        assert_eq!(
            config.max_connection_duration,
            Some(Duration::from_secs(300))
        );
        assert_eq!(config.idle_timeout, Some(Duration::from_secs(60)));
    }

    #[test]
    fn test_connection_config_clone() {
        let config = ConnectionConfig {
            max_connections: Some(50),
            max_connection_duration: Some(Duration::from_secs(120)),
            idle_timeout: Some(Duration::from_secs(30)),
        };
        let cloned = config.clone();
        assert_eq!(cloned.max_connections, config.max_connections);
        assert_eq!(
            cloned.max_connection_duration,
            config.max_connection_duration
        );
        assert_eq!(cloned.idle_timeout, config.idle_timeout);
    }
}
