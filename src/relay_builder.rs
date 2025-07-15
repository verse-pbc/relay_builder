//! RelayBuilder for constructing Nostr relays with custom state

use crate::config::{DatabaseConfig, RelayConfig};
use crate::crypto_helper::CryptoHelper;
use crate::error::Error;
use crate::event_processor::{DefaultRelayProcessor, EventContext, EventProcessor};
use crate::message_converter::NostrMessageConverter;
use crate::metrics::SubscriptionMetricsHandler;
use crate::middlewares::MetricsHandler;
use crate::relay_middleware::RelayMiddleware;
use crate::state::NostrConnectionState;
use async_trait::async_trait;
use nostr_sdk::prelude::*;
use std::marker::PhantomData;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tokio::time::Duration;
use tokio_util::{sync::CancellationToken, task::TaskTracker};
use tracing::warn;
use websocket_builder::{Middleware, WebSocketBuilder};

/// HTML rendering options for the relay
#[cfg(feature = "axum")]
#[derive(Clone, Default)]
pub enum HtmlOption {
    /// No HTML - returns 404 for non-WebSocket requests
    None,
    /// Default HTML page that renders RelayInfo
    #[default]
    Default,
    /// Custom HTML provider function
    Custom(Arc<dyn Fn(&crate::handlers::RelayInfo) -> String + Send + Sync>),
}

#[cfg(feature = "axum")]
impl std::fmt::Debug for HtmlOption {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HtmlOption::None => write!(f, "HtmlOption::None"),
            HtmlOption::Default => write!(f, "HtmlOption::Default"),
            HtmlOption::Custom(_) => write!(f, "HtmlOption::Custom(<function>)"),
        }
    }
}

/// Builder for constructing Nostr relays with custom state.
///
/// This builder allows creating relays that maintain custom per-connection state
/// in addition to the standard framework state. This enables sophisticated features
/// like rate limiting, reputation systems, session tracking, and more.
///
/// # Type Parameters
/// - `T`: The custom state type. Must implement `Clone + Send + Sync + Debug + Default`.
///
/// # Default Middleware
/// By default, the builder automatically adds these middlewares:
/// - `LoggerMiddleware` - Request/response logging
/// - `ErrorHandlingMiddleware` - Global error handling
/// - `EventVerifierMiddleware` - Cryptographic signature verification
///
/// Additional middlewares are added when configured:
/// - `MetricsMiddleware` - When `with_metrics()` is called
/// - `Nip42Middleware` - When `enable_auth` is true in config
///
/// # Bare Mode
/// Use `.bare()` to disable automatic middleware and have full control over the
/// middleware stack. WARNING: This disables signature verification unless manually added!
///
/// # Example
/// ```rust,no_run
/// use nostr_relay_builder::{RelayBuilder, EventProcessor, EventContext, RelayConfig};
/// use nostr_sdk::prelude::*;
///
/// #[derive(Debug, Clone, Default)]
/// struct MyState {
///     request_count: u32,
/// }
///
/// # #[derive(Debug)]
/// # struct MyProcessor;
/// # #[async_trait::async_trait]
/// # impl EventProcessor<MyState> for MyProcessor {
/// #     async fn handle_event(
/// #         &self,
/// #         event: nostr_sdk::Event,
/// #         custom_state: std::sync::Arc<parking_lot::RwLock<MyState>>,
/// #         context: EventContext<'_>,
/// #     ) -> Result<Vec<nostr_relay_builder::StoreCommand>, nostr_relay_builder::Error> {
/// #         Ok(vec![])
/// #     }
/// # }
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// # let keys = Keys::generate();
/// # let config = RelayConfig::new("ws://localhost:8080", "./data", keys);
/// let builder = RelayBuilder::<MyState>::new(config)
///     .with_custom_state::<MyState>()
///     .with_event_processor(MyProcessor);
///
/// let handler = builder.build().await?;
/// # Ok(())
/// # }
/// ```
pub struct RelayBuilder<T = ()> {
    config: RelayConfig,
    /// Middlewares to add to the relay
    middlewares: Vec<
        Arc<
            dyn Middleware<
                State = NostrConnectionState<T>,
                IncomingMessage = ClientMessage<'static>,
                OutgoingMessage = RelayMessage<'static>,
            >,
        >,
    >,
    /// Optional cancellation token for graceful shutdown
    cancellation_token: Option<CancellationToken>,
    /// Optional connection counter for metrics
    connection_counter: Option<Arc<AtomicUsize>>,
    /// Optional metrics handler for the relay
    metrics_handler: Option<Arc<dyn MetricsHandler>>,
    /// Optional subscription metrics handler
    subscription_metrics_handler: Option<Arc<dyn SubscriptionMetricsHandler>>,
    /// HTML rendering option for browser requests
    #[cfg(feature = "axum")]
    html_option: HtmlOption,
    /// Optional shared TaskTracker for all background tasks
    task_tracker: Option<TaskTracker>,
    /// Bare mode - skip all default middlewares
    bare_mode: bool,
    /// Event processor - defaults to DefaultRelayProcessor
    event_processor: Arc<dyn EventProcessor<T>>,
    /// Relay information for NIP-11
    #[cfg(feature = "axum")]
    relay_info: Option<crate::handlers::RelayInfo>,
    _phantom: PhantomData<T>,
}

impl<T> RelayBuilder<T>
where
    T: Clone + Send + Sync + std::fmt::Debug + 'static,
{
    /// Create a new relay builder with the given configuration
    pub fn new(config: RelayConfig) -> Self {
        Self {
            config,
            middlewares: Vec::new(),
            cancellation_token: None,
            connection_counter: None,
            metrics_handler: None,
            subscription_metrics_handler: None,
            #[cfg(feature = "axum")]
            html_option: HtmlOption::Default,
            task_tracker: None,
            bare_mode: false,
            event_processor: Arc::new(DefaultRelayProcessor::default()),
            #[cfg(feature = "axum")]
            relay_info: None,
            _phantom: PhantomData,
        }
    }

    /// Set a cancellation token for graceful shutdown
    #[must_use]
    pub fn with_cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }

    /// Set a connection counter for metrics
    #[must_use]
    pub fn with_connection_counter(mut self, counter: Arc<AtomicUsize>) -> Self {
        self.connection_counter = Some(counter);
        self
    }

    /// Set a metrics handler for the relay
    #[must_use]
    pub fn with_metrics<M>(mut self, handler: M) -> Self
    where
        M: MetricsHandler + 'static,
    {
        self.metrics_handler = Some(Arc::new(handler));
        self
    }

    /// Set a subscription metrics handler
    #[must_use]
    pub fn with_subscription_metrics<M>(mut self, handler: M) -> Self
    where
        M: SubscriptionMetricsHandler + 'static,
    {
        self.subscription_metrics_handler = Some(Arc::new(handler));
        self
    }

    /// Set a shared TaskTracker for all background tasks
    #[must_use]
    pub fn with_task_tracker(mut self, tracker: TaskTracker) -> Self {
        self.task_tracker = Some(tracker);
        self
    }

    /// Enable bare mode - skip automatic middleware (logger, error handling, event verification)
    ///
    /// WARNING: This mode skips critical security features like signature verification.
    /// You must manually add these middlewares if needed.
    ///
    /// Middlewares that are still added when explicitly configured:
    /// - MetricsMiddleware (if with_metrics is called)
    /// - Nip42Middleware (if enable_auth is true in config)
    #[must_use]
    pub fn bare(mut self) -> Self {
        self.bare_mode = true;
        self
    }

    /// Set a custom event processor for handling relay business logic
    ///
    /// If not set, uses DefaultRelayProcessor which accepts all valid events.
    ///
    /// This method accepts both `MyProcessor` and `Arc::new(MyProcessor)`.
    #[must_use]
    pub fn with_event_processor<P>(mut self, processor: impl Into<Arc<P>>) -> Self
    where
        P: EventProcessor<T> + 'static,
    {
        self.event_processor = processor.into();
        self
    }

    /// Set relay information for NIP-11 responses
    #[cfg(feature = "axum")]
    #[must_use]
    pub fn with_relay_info(mut self, relay_info: crate::handlers::RelayInfo) -> Self {
        self.relay_info = Some(relay_info);
        self
    }

    /// Transform the builder to use a different state type
    pub fn with_custom_state<U>(self) -> RelayBuilder<U>
    where
        U: Clone + Send + Sync + std::fmt::Debug + 'static,
    {
        RelayBuilder {
            config: self.config,
            middlewares: Vec::new(),
            cancellation_token: self.cancellation_token,
            connection_counter: self.connection_counter,
            metrics_handler: None,
            subscription_metrics_handler: None,
            #[cfg(feature = "axum")]
            html_option: self.html_option,
            task_tracker: self.task_tracker,
            bare_mode: self.bare_mode,
            event_processor: Arc::new(DefaultRelayProcessor::default()), // Reset to default processor
            #[cfg(feature = "axum")]
            relay_info: self.relay_info,
            _phantom: PhantomData,
        }
    }

    /// Set HTML rendering option
    #[cfg(feature = "axum")]
    #[must_use]
    pub fn with_html(mut self, html_option: HtmlOption) -> Self {
        self.html_option = html_option;
        self
    }

    /// Disable HTML rendering (return 404 for browser requests)
    #[cfg(feature = "axum")]
    #[must_use]
    pub fn without_html(mut self) -> Self {
        self.html_option = HtmlOption::None;
        self
    }

    /// Set custom HTML provider
    #[cfg(feature = "axum")]
    #[must_use]
    pub fn with_custom_html<F>(mut self, html_provider: F) -> Self
    where
        F: Fn(&crate::handlers::RelayInfo) -> String + Send + Sync + 'static,
    {
        self.html_option = HtmlOption::Custom(Arc::new(html_provider));
        self
    }

    /// Add a middleware to the relay
    ///
    /// Middleware are executed in the order they are added for inbound messages,
    /// and in reverse order for outbound messages.
    #[must_use]
    pub fn with_middleware<M>(mut self, middleware: M) -> Self
    where
        M: Middleware<
                State = NostrConnectionState<T>,
                IncomingMessage = ClientMessage<'static>,
                OutgoingMessage = RelayMessage<'static>,
            > + 'static,
    {
        self.middlewares.push(Arc::new(middleware));
        self
    }

    // ===== New Clean API Methods =====

    /// Build a raw WebSocket handler without any framework integration
    ///
    /// This returns the core WebSocket handler that can be used with any
    /// WebSocket server implementation.
    pub async fn build(self) -> Result<RelayWebSocketHandler<T>, Error>
    where
        T: Default,
    {
        self.build_internal().await
    }

    /// Build an Axum-compatible handler with WebSocket and optional NIP-11 support
    ///
    /// This returns a handler function that can be used directly with Axum routing.
    /// If `with_relay_info()` was called, NIP-11 responses will be enabled.
    ///
    /// # Example
    /// ```ignore
    /// let handler = RelayBuilder::new(config)
    ///     .with_relay_info(relay_info)
    ///     .build_axum()
    ///     .await?;
    ///
    /// let app = Router::new().route("/", get(handler));
    /// ```
    #[cfg(feature = "axum")]
    pub async fn build_axum(
        self,
    ) -> Result<
        impl Fn(
                Option<websocket_builder::WebSocketUpgrade>,
                axum::extract::ConnectInfo<std::net::SocketAddr>,
                axum::http::HeaderMap,
            ) -> std::pin::Pin<
                Box<dyn std::future::Future<Output = axum::response::Response> + Send>,
            > + Clone
            + Send
            + 'static,
        Error,
    >
    where
        T: Default,
    {
        let relay_info = self.relay_info.clone();
        let service = self.build_relay_service_internal().await?;

        Ok(
            move |ws: Option<websocket_builder::WebSocketUpgrade>,
                  connect_info: axum::extract::ConnectInfo<std::net::SocketAddr>,
                  headers: axum::http::HeaderMap| {
                let service = service.clone();
                let relay_info = relay_info.clone();

                Box::pin(async move {
                    // Delegate to service's axum handler
                    if ws.is_some()
                        || headers
                            .get("accept")
                            .and_then(|h| h.to_str().ok())
                            .map(|s| s == "application/nostr+json")
                            .unwrap_or(false)
                    {
                        service.axum_root_handler()(ws, connect_info, headers).await
                    } else if relay_info.is_some() {
                        // Serve default relay info HTML
                        use axum::response::{Html, IntoResponse};
                        Html(crate::handlers::default_relay_html(
                            relay_info.as_ref().unwrap(),
                        ))
                        .into_response()
                    } else {
                        // No relay info, return 404
                        use axum::response::IntoResponse;
                        axum::http::StatusCode::NOT_FOUND.into_response()
                    }
                })
                    as std::pin::Pin<
                        Box<dyn std::future::Future<Output = axum::response::Response> + Send>,
                    >
            },
        )
    }

    /// Build a relay service with full control over individual components
    ///
    /// This returns a service object that provides methods for handling
    /// different aspects of the relay protocol separately. Useful when
    /// you need to integrate the relay into existing infrastructure.
    ///
    /// # Example
    /// ```ignore
    /// let service = RelayBuilder::new(config)
    ///     .build_relay_service(relay_info)
    ///     .await?;
    ///
    /// // Use service.axum_root_handler() for WebSocket + NIP-11
    /// // Or access individual components as needed
    /// ```
    #[cfg(feature = "axum")]
    pub async fn build_relay_service(
        mut self,
        relay_info: crate::handlers::RelayInfo,
    ) -> Result<Arc<crate::handlers::RelayService<T>>, Error>
    where
        T: Default,
    {
        self.relay_info = Some(relay_info);
        self.build_relay_service_internal().await
    }

    /// Internal method to build relay service
    #[cfg(feature = "axum")]
    async fn build_relay_service_internal(
        self,
    ) -> Result<Arc<crate::handlers::RelayService<T>>, Error>
    where
        T: Default,
    {
        let cancellation_token = self.cancellation_token.clone();
        let connection_counter = self.connection_counter.clone();
        let scope_config = self.config.scope_config.clone();
        let relay_info = self
            .relay_info
            .clone()
            .unwrap_or_else(|| crate::handlers::RelayInfo {
                name: "Nostr Relay".to_string(),
                description: "A Nostr relay".to_string(),
                pubkey: self.config.keys.public_key().to_string(),
                contact: "".to_string(),
                supported_nips: vec![1],
                software: "nostr_relay_builder".to_string(),
                version: env!("CARGO_PKG_VERSION").to_string(),
                icon: None,
            });

        let handler = self.build_internal().await?;
        Ok(Arc::new(crate::handlers::RelayService::new(
            handler,
            relay_info,
            cancellation_token,
            connection_counter,
            scope_config,
        )))
    }

    // ===== Internal Methods =====

    /// Internal builder that constructs the WebSocket handler
    async fn build_internal(mut self) -> Result<RelayWebSocketHandler<T>, Error>
    where
        T: Default,
    {
        let websocket_config = self.config.websocket_config.clone();

        // Set the global subscription metrics handler if provided
        if let Some(handler) = self.subscription_metrics_handler.clone() {
            crate::global_metrics::set_subscription_metrics_handler(handler);
        }

        let task_tracker = self.task_tracker.take().unwrap_or_default();

        // Calculate channel sizes
        let per_connection_channel_size = self.config.calculate_channel_size();
        let event_distribution_channel_size = self.config.calculate_event_channel_size();
        let relay_url = self.config.relay_url.clone();
        let _scope_config = self.config.scope_config.clone();

        // Create subscription registry and event channel first
        let subscription_registry =
            Arc::new(crate::subscription_registry::SubscriptionRegistry::new(
                self.subscription_metrics_handler.clone(),
            ));

        // Create mpsc channel for event distribution (global channel)
        let (event_sender, event_receiver) = flume::bounded(event_distribution_channel_size);

        // Start the distribution task
        subscription_registry
            .clone()
            .start_distribution_task(event_receiver);

        // Only create crypto worker if we're creating a new database
        let database = self.config.database.take();
        let (database, crypto_helper) = match database {
            Some(DatabaseConfig::Instance(db)) => {
                // Use existing database instance - create crypto worker for signature verification
                let keys = Arc::new(self.config.keys.clone());
                let crypto_helper = CryptoHelper::new(keys);
                (db, crypto_helper)
            }
            Some(database_config @ DatabaseConfig::Path(_)) => {
                // Create new database with keys
                let keys = Arc::new(self.config.keys.clone());
                let crypto_helper = CryptoHelper::new(keys);
                let database = RelayConfig::create_database_from_config(
                    database_config,
                    &self.config.websocket_config,
                    self.config.max_subscriptions,
                    Some(task_tracker.clone()),
                    self.cancellation_token.clone(),
                    Some(event_sender),
                )?;
                (database, crypto_helper)
            }
            None => {
                return Err(Error::internal(
                    "Database configuration is required".to_string(),
                ));
            }
        };

        let custom_middlewares = std::mem::take(&mut self.middlewares);

        // Create a wrapper to use Arc<dyn EventProcessor<T>> with RelayMiddleware
        struct DynProcessor<T>(Arc<dyn EventProcessor<T>>);

        #[async_trait]
        impl<T> EventProcessor<T> for DynProcessor<T>
        where
            T: Send + Sync + std::fmt::Debug + 'static,
        {
            fn can_see_event(
                &self,
                event: &Event,
                custom_state: Arc<parking_lot::RwLock<T>>,
                context: EventContext<'_>,
            ) -> Result<bool, Error> {
                self.0.can_see_event(event, custom_state, context)
            }

            fn verify_filters(
                &self,
                filters: &[Filter],
                custom_state: Arc<parking_lot::RwLock<T>>,
                context: EventContext<'_>,
            ) -> Result<(), Error> {
                self.0.verify_filters(filters, custom_state, context)
            }

            async fn handle_event(
                &self,
                event: Event,
                custom_state: Arc<parking_lot::RwLock<T>>,
                context: EventContext<'_>,
            ) -> Result<Vec<crate::subscription_coordinator::StoreCommand>, Error> {
                self.0.handle_event(event, custom_state, context).await
            }
        }

        impl<T> std::fmt::Debug for DynProcessor<T> {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.debug_struct("DynProcessor").finish()
            }
        }

        let max_subscriptions = if self.config.max_subscriptions > 0 {
            Some(self.config.max_subscriptions)
        } else {
            None
        };

        let relay_middleware = RelayMiddleware::new(
            DynProcessor(self.event_processor.clone()),
            self.config.keys.public_key(),
            database,
            subscription_registry.clone(),
            self.config.max_limit,
            RelayUrl::parse(&relay_url).expect("Valid relay URL"),
            crypto_helper.clone(),
            max_subscriptions,
        );

        let mut builder = WebSocketBuilder::<
            NostrConnectionState<T>,
            ClientMessage<'static>,
            RelayMessage<'static>,
            NostrMessageConverter,
        >::new(NostrMessageConverter);

        builder = builder.with_channel_size(per_connection_channel_size);

        // Apply other websocket configurations if supported
        if let Some(max_connections) = websocket_config.max_connections {
            builder = builder.with_max_connections(max_connections);
        }
        if let Some(max_time) = websocket_config.max_connection_time {
            builder = builder.with_max_connection_time(Duration::from_secs(max_time));
        }

        // Add standard middlewares unless in bare mode
        if !self.bare_mode {
            builder = builder.with_middleware(crate::middlewares::LoggerMiddleware::new());
            builder = builder.with_middleware(crate::middlewares::ErrorHandlingMiddleware::new());
        } else {
            warn!(
                "⚠️  BARE MODE ENABLED: Relay is running without default middleware! \
                Signature verification, error handling, and logging are disabled unless manually added."
            );
        }

        // Add metrics middleware if handler is provided
        if let Some(metrics_handler) = self.metrics_handler.clone() {
            builder = builder.with_arc_middleware(Arc::new(
                crate::middlewares::MetricsMiddleware::with_arc_handler(metrics_handler),
            ));
        }

        // Add NIP-42 authentication middleware if enabled
        if self.config.enable_auth {
            let auth_config =
                self.config
                    .auth_config
                    .clone()
                    .unwrap_or_else(|| crate::middlewares::AuthConfig {
                        relay_url: self.config.relay_url.clone(),
                        validate_subdomains: matches!(
                            self.config.scope_config,
                            crate::config::ScopeConfig::Subdomain { .. }
                        ),
                    });
            builder =
                builder.with_middleware(crate::middlewares::Nip42Middleware::new(auth_config));
        }

        // Add event verification middleware unless in bare mode
        if !self.bare_mode {
            builder = builder.with_middleware(crate::middlewares::EventVerifierMiddleware::new(
                crypto_helper.clone(),
            ));
        }

        // Add custom middlewares
        for middleware in custom_middlewares {
            builder = builder.with_arc_middleware(middleware);
        }

        // Relay middleware must be last to process messages after all validation
        builder = builder.with_middleware(relay_middleware);

        Ok(builder.build())
    }
}

/// Type alias for the complete WebSocket handler type used by the relay
pub type RelayWebSocketHandler<T> = websocket_builder::WebSocketHandler<
    NostrConnectionState<T>,
    ClientMessage<'static>,
    RelayMessage<'static>,
    NostrMessageConverter,
>;

/// Type alias for the default relay handler (with unit state for backward compatibility)
pub type DefaultRelayWebSocketHandler = RelayWebSocketHandler<()>;
