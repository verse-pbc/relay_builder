//! `RelayBuilder` for constructing Nostr relays with custom state

use crate::config::{DatabaseConfig, RelayConfig};
use crate::crypto_helper::CryptoHelper;
use crate::error::Error;
use crate::event_processor::{DefaultRelayProcessor, EventProcessor};
use crate::metrics::SubscriptionMetricsHandler;
use crate::middleware_chain::chain;
// Middleware chain types are used in type signatures
use crate::middlewares::{MetricsHandler, Nip42Middleware};
use crate::nostr_handler::IntoHandlerFactory;
use crate::nostr_middleware::{InboundProcessor, OutboundProcessor};
use crate::relay_middleware::RelayMiddleware;
use nostr_sdk::prelude::*;
use std::marker::PhantomData;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

/// HTML rendering options for the relay

#[derive(Clone, Default)]
pub enum HtmlOption {
    /// No HTML - returns 404 for non-WebSocket requests
    None,
    /// Default HTML page that renders `RelayInfo`
    #[default]
    Default,
    /// Custom HTML provider function
    Custom(Arc<dyn Fn(&crate::handlers::RelayInfo) -> String + Send + Sync>),
}

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
/// # Default Processing
/// By default, the builder automatically provides:
/// - **`EventIngester`** - High-performance JSON parsing and signature verification (pre-middleware)
/// - `NostrLoggerMiddleware` - Request/response logging (outermost layer)
/// - `ErrorHandlingMiddleware` - Global error handling (second layer)
/// - `Nip42Middleware` - NIP-42 authentication (when `config.enable_auth` is true)
///
/// These default middlewares are added as the outermost layers to ensure proper
/// error handling and complete logging of all requests and responses. Auth middleware
/// is placed after error handling so custom middlewares can access authentication state.
///
/// # Without Defaults
/// Use `.without_defaults()` to disable automatic middleware and have full control over the
/// middleware stack. Note: `EventIngester` signature verification is always active regardless.
///
/// # Example
/// ```rust,no_run
/// use relay_builder::{RelayBuilder, EventProcessor, EventContext, RelayConfig};
/// use relay_builder::middlewares::{NostrLoggerMiddleware, ErrorHandlingMiddleware};
/// use nostr_sdk::prelude::*;
///
/// #[derive(Debug, Clone, Default)]
/// struct MyState {
///     request_count: u32,
/// }
///
/// # #[derive(Debug, Clone)]
/// # struct MyProcessor;
/// # impl EventProcessor<MyState> for MyProcessor {
/// #     fn handle_event(
/// #         &self,
/// #         event: nostr_sdk::Event,
/// #         custom_state: std::sync::Arc<parking_lot::RwLock<MyState>>,
/// #         context: &EventContext,
/// #     ) -> impl std::future::Future<Output = Result<Vec<relay_builder::StoreCommand>, relay_builder::Error>> + Send {
/// #         async move { Ok(vec![]) }
/// #     }
/// # }
/// # #[derive(Debug, Clone)]
/// # struct MyCustomMiddleware;
/// # impl<T> relay_builder::NostrMiddleware<T> for MyCustomMiddleware
/// # where T: Send + Sync + Clone + 'static {}
/// # async fn example() -> Result<(), Box<dyn std::error::Error>> {
/// # let keys = Keys::generate();
/// # let config = RelayConfig::new("ws://localhost:8080", "./data", keys);
/// // With defaults (Logger and ErrorHandling added automatically)
/// let handler = RelayBuilder::<MyState>::new(config.clone())
///     .custom_state::<MyState>()
///     .event_processor(MyProcessor)
///     .build()
///     .await?;
///
/// // Without defaults (full control)
/// let handler = RelayBuilder::<MyState>::new(config)
///     .custom_state::<MyState>()
///     .event_processor(MyProcessor)
///     .without_defaults()
///     .build_with(|chain| {
///         chain
///             .with(MyCustomMiddleware)
///             .with(ErrorHandlingMiddleware::new())
///             .with(NostrLoggerMiddleware::new())
///     })
///     .await?;
/// # Ok(())
/// # }
/// ```
#[allow(clippy::struct_excessive_bools)] // Builder pattern naturally accumulates boolean flags
pub struct RelayBuilder<T = (), P = DefaultRelayProcessor<T>>
where
    T: Clone + Send + Sync + std::fmt::Debug + 'static,
    P: EventProcessor<T>,
{
    config: RelayConfig,
    /// Optional cancellation token for graceful shutdown
    cancellation_token: Option<CancellationToken>,
    /// Optional connection counter for metrics
    connection_counter: Option<Arc<AtomicUsize>>,
    /// Optional metrics handler for the relay
    metrics_handler: Option<Arc<dyn MetricsHandler>>,
    /// Optional subscription metrics handler
    subscription_metrics_handler: Option<Arc<dyn SubscriptionMetricsHandler>>,
    /// HTML rendering option for browser requests
    html_option: HtmlOption,
    /// Optional shared `TaskTracker` for all background tasks
    task_tracker: Option<TaskTracker>,
    /// Skip default middlewares (logger, error handling)
    without_defaults: bool,
    /// Event processor
    event_processor: Arc<P>,
    /// Relay information for NIP-11
    relay_info: Option<crate::handlers::RelayInfo>,
    /// Enable logger middleware
    enable_logger: bool,
    /// Enable error handling middleware
    enable_error_handling: bool,
    /// Enable metrics middleware
    enable_metrics: bool,
    _phantom: PhantomData<(T, P)>,
}

impl<T, P> RelayBuilder<T, P>
where
    T: Clone + Send + Sync + std::fmt::Debug + 'static,
    P: EventProcessor<T> + Clone,
{
    /// Create a new relay builder with the given configuration
    pub fn new(config: RelayConfig) -> RelayBuilder<T, DefaultRelayProcessor<T>> {
        RelayBuilder {
            config,
            cancellation_token: None,
            connection_counter: None,
            metrics_handler: None,
            subscription_metrics_handler: None,

            html_option: HtmlOption::Default,
            task_tracker: None,
            without_defaults: false,
            event_processor: Arc::new(DefaultRelayProcessor::default()),

            relay_info: None,
            enable_logger: true,
            enable_error_handling: true,
            enable_metrics: false, // Only enabled when with_metrics is called
            _phantom: PhantomData,
        }
    }

    /// Set a cancellation token for graceful shutdown
    #[must_use]
    pub fn cancellation_token(mut self, token: CancellationToken) -> Self {
        self.cancellation_token = Some(token);
        self
    }

    /// Set a connection counter for metrics
    #[must_use]
    pub fn connection_counter(mut self, counter: Arc<AtomicUsize>) -> Self {
        self.connection_counter = Some(counter);
        self
    }

    /// Set a metrics handler for the relay
    #[must_use]
    pub fn metrics<M>(mut self, handler: M) -> Self
    where
        M: MetricsHandler + 'static,
    {
        self.metrics_handler = Some(Arc::new(handler));
        self.enable_metrics = true;
        self
    }

    /// Set a subscription metrics handler
    #[must_use]
    pub fn subscription_metrics<M>(mut self, handler: M) -> Self
    where
        M: SubscriptionMetricsHandler + 'static,
    {
        self.subscription_metrics_handler = Some(Arc::new(handler));
        self
    }

    /// Set a shared `TaskTracker` for all background tasks
    #[must_use]
    pub fn task_tracker(mut self, tracker: TaskTracker) -> Self {
        self.task_tracker = Some(tracker);
        self
    }

    /// Skip default middlewares (logger, error handling)
    ///
    /// By default, the relay automatically adds `NostrLoggerMiddleware` and
    /// `ErrorHandlingMiddleware` as the outermost layers. Call this method
    /// to opt out of these defaults and have full control over the middleware stack.
    ///
    /// Note: `EventIngester` signature verification is always active and cannot be disabled.
    #[must_use]
    pub fn without_defaults(mut self) -> Self {
        self.without_defaults = true;
        self.enable_logger = false;
        self.enable_error_handling = false;
        self
    }

    /// Set a custom event processor for handling relay business logic
    ///
    /// If not set, uses `DefaultRelayProcessor` which accepts all valid events.
    ///
    /// This method accepts both `MyProcessor` and `Arc::new(MyProcessor)`.
    #[must_use]
    pub fn event_processor<NewP>(self, processor: impl Into<Arc<NewP>>) -> RelayBuilder<T, NewP>
    where
        NewP: EventProcessor<T> + 'static,
    {
        RelayBuilder {
            config: self.config,
            cancellation_token: self.cancellation_token,
            connection_counter: self.connection_counter,
            metrics_handler: self.metrics_handler,
            subscription_metrics_handler: self.subscription_metrics_handler,

            html_option: self.html_option,
            task_tracker: self.task_tracker,
            without_defaults: self.without_defaults,
            event_processor: processor.into(),

            relay_info: self.relay_info,
            enable_logger: self.enable_logger,
            enable_error_handling: self.enable_error_handling,
            enable_metrics: self.enable_metrics,
            _phantom: PhantomData,
        }
    }

    /// Set relay information for NIP-11 responses
    #[must_use]
    pub fn relay_info(mut self, relay_info: crate::handlers::RelayInfo) -> Self {
        self.relay_info = Some(relay_info);
        self
    }

    /// Transform the builder to use a different state type
    pub fn custom_state<U>(self) -> RelayBuilder<U, DefaultRelayProcessor<U>>
    where
        U: Clone + Send + Sync + std::fmt::Debug + 'static,
    {
        RelayBuilder {
            config: self.config,
            cancellation_token: self.cancellation_token,
            connection_counter: self.connection_counter,
            metrics_handler: None,
            subscription_metrics_handler: None,

            html_option: self.html_option,
            task_tracker: self.task_tracker,
            without_defaults: self.without_defaults,
            event_processor: Arc::new(DefaultRelayProcessor::default()), // Reset to default processor

            relay_info: self.relay_info,
            enable_logger: self.enable_logger,
            enable_error_handling: self.enable_error_handling,
            enable_metrics: self.enable_metrics,
            _phantom: PhantomData,
        }
    }

    /// Set HTML rendering option
    #[must_use]
    pub fn html(mut self, html_option: HtmlOption) -> Self {
        self.html_option = html_option;
        self
    }

    /// Disable HTML rendering (return 404 for browser requests)
    #[must_use]
    pub fn without_html(mut self) -> Self {
        self.html_option = HtmlOption::None;
        self
    }

    /// Set custom HTML provider
    #[must_use]
    pub fn custom_html<F>(mut self, html_provider: F) -> Self
    where
        F: Fn(&crate::handlers::RelayInfo) -> String + Send + Sync + 'static,
    {
        self.html_option = HtmlOption::Custom(Arc::new(html_provider));
        self
    }

    /// Disable logger middleware
    #[must_use]
    pub fn without_logger(mut self) -> Self {
        self.enable_logger = false;
        self
    }

    /// Disable error handling middleware
    ///
    /// WARNING: This disables global error recovery. Errors will propagate to the WebSocket layer.
    #[must_use]
    pub fn without_error_handling(mut self) -> Self {
        self.enable_error_handling = false;
        self
    }

    // ===== Build API =====

    /// Build a WebSocket handler factory with default middleware
    ///
    /// This returns the concrete handler factory that can be used with `websocket_builder`'s
    /// axum integration or any other WebSocket server that accepts a `HandlerFactory`.
    ///
    /// Unless `.without_defaults()` has been called, this will automatically add:
    /// - `NostrLoggerMiddleware` as the outermost layer
    /// - `ErrorHandlingMiddleware` as the second layer
    ///
    /// # Example
    /// ```ignore
    /// let factory = RelayBuilder::new(config)
    ///     .event_processor(MyProcessor)
    ///     .build()
    ///     .await?;
    ///
    /// // Use with websocket_builder's route helper
    /// let app = crate::websocket::websocket_route("/", factory);
    /// ```
    ///
    /// # Errors
    ///
    /// Returns error if database configuration is missing or setup fails.
    pub async fn build(self) -> Result<impl crate::websocket::HandlerFactory, Error>
    where
        T: Default,
    {
        // Call build_with with an identity function
        self.build_with(|chain| chain).await
    }

    /// Build a WebSocket handler factory with a custom middleware chain
    ///
    /// This method gives you full control over the middleware chain composition.
    /// You provide a function that takes a chain builder and returns a configured chain.
    ///
    /// # Example
    /// ```ignore
    /// use relay_builder::middlewares::*;
    ///
    /// let factory = RelayBuilder::new(config)
    ///     .event_processor(MyProcessor)
    ///     .build_with(|chain| {
    ///         chain
    ///             .with(MyCustomMiddleware::new())
    ///             .with(RateLimitMiddleware::new(10))
    ///             .with(SpamFilterMiddleware::new())
    ///     })
    ///     .await?;
    /// ```
    ///
    /// # Note
    /// Unless `.without_defaults()` is called, the default middleware (logger, error handling, auth)
    /// are automatically added as the outermost layers. This ensures proper error handling
    /// and logging for all requests.
    /// `EventIngester` signature verification is always active regardless of middleware configuration.
    /// The relay middleware is automatically added as the innermost middleware.
    ///
    /// When `without_defaults()` is NOT set, the chain order will be:
    /// `NostrLoggerMiddleware` -> `ErrorHandlingMiddleware` -> `Nip42Middleware` (if auth enabled) -> `YourMiddleware` -> `RelayMiddleware`
    ///
    /// # Errors
    ///
    /// Returns error if database configuration is missing or chain setup fails.
    ///
    /// # Panics
    ///
    /// Panics if the subscription metrics handler has already been set globally
    #[allow(clippy::unused_async)]
    #[allow(clippy::too_many_lines)] // Complex builder setup with conditional middleware
    pub async fn build_with<F, C>(
        mut self,
        chain_fn: F,
    ) -> Result<impl crate::websocket::HandlerFactory, Error>
    where
        T: Default,
        F: FnOnce(
            crate::middleware_chain::NostrChainBuilder<
                T,
                crate::middleware_chain::ChainBlueprint<
                    RelayMiddleware<P, T>,
                    crate::middleware_chain::End<T>,
                >,
            >,
        ) -> crate::middleware_chain::NostrChainBuilder<T, C>,
        C: crate::middleware_chain::BuildConnected + Send + Sync + 'static + Clone,
        C::Output: InboundProcessor<T> + OutboundProcessor<T> + Send + Sync + 'static + Clone,
    {
        use crate::middlewares::MetricsMiddleware;
        use crate::util::{Either, IdentityMiddleware};

        // Set the global subscription metrics handler if provided
        if let Some(handler) = self.subscription_metrics_handler.clone() {
            crate::global_metrics::set_subscription_metrics_handler(handler);
        }

        let task_tracker = self.task_tracker.take().unwrap_or_default();
        let relay_url = self.config.relay_url.clone();

        // Create subscription registry
        let subscription_registry =
            Arc::new(crate::subscription_registry::SubscriptionRegistry::new(
                self.subscription_metrics_handler.clone(),
            ));

        // Setup database, crypto helper, and event ingester
        let database = self.config.database.take();
        let (database, crypto_helper, event_ingester) = match database {
            Some(DatabaseConfig::Instance(db)) => {
                let keys = Arc::new(self.config.keys.clone());
                let crypto_helper = CryptoHelper::new(Arc::clone(&keys));
                let event_ingester = crate::event_ingester::EventIngester::new(keys);
                (db, crypto_helper, event_ingester)
            }
            Some(database_config @ DatabaseConfig::Path(_)) => {
                let keys = Arc::new(self.config.keys.clone());
                let crypto_helper = CryptoHelper::new(Arc::clone(&keys));
                let event_ingester = crate::event_ingester::EventIngester::new(keys);
                let database = RelayConfig::create_database_from_config(
                    database_config,
                    &self.config.websocket_config,
                    self.config.max_subscriptions,
                    Some(task_tracker),
                    self.cancellation_token.clone(),
                    self.config.max_readers,
                    self.config.ingester_cpu_affinity,
                )
                .await?;
                (database, crypto_helper, event_ingester)
            }
            None => {
                return Err(Error::internal(
                    "Database configuration is required".to_string(),
                ));
            }
        };

        let max_subscriptions = if self.config.max_subscriptions > 0 {
            Some(self.config.max_subscriptions)
        } else {
            None
        };

        // Create relay-wide replaceable events buffer with registry for broadcasting
        let buffer = crate::subscription_coordinator::ReplaceableEventsBuffer::with_registry(
            subscription_registry.clone(),
        );
        let replaceable_event_queue = buffer.get_sender();

        // Start the buffer with relay's cancellation token
        buffer.start_with_sender(
            database.clone(),
            crypto_helper.clone(),
            self.cancellation_token.clone().unwrap_or_default(),
            "relay_replaceable_events_buffer".to_string(),
        );

        // Create the core relay middleware with the configured event processor
        let processor = Arc::try_unwrap(self.event_processor).unwrap_or_else(|arc| (*arc).clone());

        let relay_middleware = RelayMiddleware::new(
            processor,
            self.config.keys.public_key(),
            database,
            Arc::clone(&subscription_registry),
            self.config.max_limit,
            RelayUrl::parse(&relay_url).expect("Valid relay URL"),
            crypto_helper,
            max_subscriptions,
            replaceable_event_queue,
        );

        // Start with relay middleware as the innermost middleware
        let base_chain = chain::<T>().with(relay_middleware);

        // Let the user build their custom middleware chain on top of relay middleware
        let chain_with_relay = chain_fn(base_chain);

        // Build the final chain with conditionally applied defaults using Either
        let final_chain = if self.without_defaults {
            // Without defaults - use Either::Right (identity/no-op) for metrics, auth, error handling, and logger
            let chain_with_metrics = if self.enable_metrics {
                if let Some(handler) = self.metrics_handler.clone() {
                    chain_with_relay
                        .with(Either::Left(MetricsMiddleware::with_arc_handler(handler)))
                } else {
                    chain_with_relay.with(Either::Right(IdentityMiddleware))
                }
            } else {
                chain_with_relay.with(Either::Right(IdentityMiddleware))
            };

            chain_with_metrics
                .with(Either::Right(IdentityMiddleware))
                .with(Either::Right(IdentityMiddleware))
                .with(Either::Right(IdentityMiddleware))
        } else {
            // With defaults - add metrics middleware if enabled
            let chain_with_metrics = if self.enable_metrics {
                if let Some(handler) = self.metrics_handler.clone() {
                    chain_with_relay
                        .with(Either::Left(MetricsMiddleware::with_arc_handler(handler)))
                } else {
                    chain_with_relay.with(Either::Right(IdentityMiddleware))
                }
            } else {
                chain_with_relay.with(Either::Right(IdentityMiddleware))
            };

            // Then add auth middleware if enabled
            let chain_with_auth = if self.config.enable_auth {
                chain_with_metrics.with(Either::Left(Nip42Middleware::with_url(relay_url)))
            } else {
                chain_with_metrics.with(Either::Right(IdentityMiddleware))
            };

            // Then add error handling and logger as outermost layers
            chain_with_auth
                .with(Either::Left(
                    crate::middlewares::ErrorHandlingMiddleware::new(),
                ))
                .with(Either::Left(
                    crate::middlewares::NostrLoggerMiddleware::new(),
                ))
        };

        let channel_size = self.config.calculate_channel_size();
        let handler_factory = final_chain.into_handler_factory(
            event_ingester,
            self.config.scope_config.clone(),
            channel_size,
            self.config.keys.public_key(),
        );

        // Spawn diagnostics task if enabled
        if self.config.enable_diagnostics {
            tokio::spawn(async move {
                // Use 1 minute interval for now (was 30 minutes)
                crate::diagnostics::run_diagnostics(subscription_registry, 1).await;
            });
        }

        Ok(handler_factory)
    }
}
