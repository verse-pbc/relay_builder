//! Event processor trait for handling Nostr events with zero-allocation performance
//!
//! This module provides the [`EventProcessor`] trait, which is the core abstraction
//! for implementing custom relay business logic. The API is designed to minimize
//! allocations and maximize performance in hot paths like subscription processing.

use crate::error::Result;
use crate::subscription_coordinator::StoreCommand;
use nostr_lmdb::Scope;
use nostr_sdk::prelude::*;
use std::future::Future;
use std::sync::Arc;

/// Minimal context for event visibility checks
///
/// Contains only the essential data needed for most visibility decisions.
/// Now uses owned data for lock-free access via ArcSwap.
#[derive(Debug, Clone)]
pub struct EventContext {
    /// Authenticated public key of the connection (if any)
    pub authed_pubkey: Option<PublicKey>,
    /// The subdomain/scope this connection is operating in
    pub subdomain: Arc<Scope>,
    /// The relay's public key
    pub relay_pubkey: PublicKey,
}

/// Core trait defining relay business logic with optimized performance.
///
/// This trait uses a generic type parameter `T` for custom per-connection state,
/// enabling type-safe access to application-specific data without runtime overhead.
///
/// ## Design Principles
///
/// 1. **Zero-allocation hot paths**: Methods like `can_see_event` receive only references
/// 2. **Direct state access**: Custom state is passed by reference, avoiding cloning
/// 3. **Minimal context**: EventContext is stack-allocated with only essential data
/// 4. **Type safety**: Generic parameter ensures compile-time type checking
///
/// ## Example
///
/// ```rust,no_run
/// use relay_builder::{EventProcessor, EventContext, StoreCommand};
/// use nostr_sdk::prelude::*;
/// use std::sync::Arc;
/// use std::future::Future;
///
/// #[derive(Debug, Clone, Default)]
/// struct RateLimitState {
///     tokens: f32,
///     events_processed: u64,
/// }
///
/// #[derive(Debug)]
/// struct RateLimitedProcessor {
///     tokens_per_event: f32,
/// }
///
/// impl EventProcessor<RateLimitState> for RateLimitedProcessor {
///     fn can_see_event(
///         &self,
///         _event: &Event,
///         custom_state: Arc<parking_lot::RwLock<RateLimitState>>,
///         _context: &EventContext,
///     ) -> Result<bool, relay_builder::Error> {
///         // For read-only access, use read lock
///         let state = custom_state.read();
///         Ok(state.tokens > 0.0)
///     }
///
///     fn handle_event(
///         &self,
///         event: Event,
///         custom_state: Arc<parking_lot::RwLock<RateLimitState>>,
///         context: &EventContext,
///     ) -> impl Future<Output = Result<Vec<StoreCommand>, relay_builder::Error>> + Send {
///         async move {
///         // Get a write lock since we need to modify the rate limit state
///         let mut state = custom_state.write();
///
///         if state.tokens < self.tokens_per_event {
///             return Err(relay_builder::Error::restricted("Rate limit exceeded"));
///         }
///
///         state.tokens -= self.tokens_per_event;
///         state.events_processed += 1;
///
///         Ok(vec![StoreCommand::SaveSignedEvent(
///             Box::new(event),
///             (*context.subdomain).clone(),
///             None,
///         )])
///         }
///     }
/// }
/// ```
pub trait EventProcessor<T = ()>: Send + Sync + std::fmt::Debug + 'static
where
    T: Send + Sync + 'static,
{
    /// Check if an event should be visible to this connection.
    ///
    /// This method is called in hot loops during subscription processing,
    /// so it must be synchronous for maximum performance with zero allocations.
    /// The Arc<RwLock<T>> allows implementors to choose whether they need read
    /// or write access to the state.
    ///
    /// # Arguments
    /// * `event` - The event to check visibility for
    /// * `custom_state` - Custom per-connection state wrapped in Arc<RwLock<T>>
    /// * `context` - Minimal context with auth info and relay details
    ///
    /// # Returns
    /// * `Ok(true)` - Event is visible to this connection
    /// * `Ok(false)` - Event should be filtered out
    /// * `Err(Error)` - Processing error (will be logged)
    fn can_see_event(
        &self,
        event: &Event,
        custom_state: Arc<parking_lot::RwLock<T>>,
        context: &EventContext,
    ) -> Result<bool> {
        // Default implementation: allow all events (public relay behavior)
        let _ = (event, custom_state, context);
        Ok(true)
    }

    /// Verify if filters are allowed for this connection.
    ///
    /// This method validates subscription filters before processing.
    ///
    /// # Arguments
    /// * `filters` - The filters from the REQ message
    /// * `custom_state` - Custom per-connection state wrapped in Arc<RwLock<T>>
    /// * `context` - Minimal context with auth info
    ///
    /// # Returns
    /// * `Ok(())` - Filters are allowed
    /// * `Err(Error)` - Filter validation failed (will be converted to NOTICE)
    fn verify_filters(
        &self,
        filters: &[Filter],
        custom_state: Arc<parking_lot::RwLock<T>>,
        context: &EventContext,
    ) -> Result<()> {
        // Default implementation: allow all filters
        let _ = (filters, custom_state, context);
        Ok(())
    }

    /// Process an incoming event and return database commands.
    ///
    /// This method handles EVENT messages with access to custom state through Arc<RwLock<T>>,
    /// allowing the implementor to choose between read-only or write access as needed.
    ///
    /// # Arguments
    /// * `event` - The event to process
    /// * `custom_state` - Custom per-connection state wrapped in Arc<RwLock<T>>
    /// * `context` - Minimal context with auth info
    ///
    /// # Returns
    /// * `Ok(commands)` - List of database commands to execute
    /// * `Err(Error)` - Processing failed (will be converted to NOTICE)
    fn handle_event(
        &self,
        event: Event,
        custom_state: Arc<parking_lot::RwLock<T>>,
        context: &EventContext,
    ) -> impl Future<Output = Result<Vec<StoreCommand>>> + Send {
        async move {
            // Default implementation: store all valid events
            let _ = custom_state; // Unused in default implementation
                                  // Need to dereference Arc<Scope> to get Scope
            Ok(vec![(event, (*context.subdomain).clone()).into()])
        }
    }
}

/// Default relay processor - uses all trait default implementations
#[derive(Debug, Clone)]
pub struct DefaultRelayProcessor<T = ()> {
    _phantom: std::marker::PhantomData<T>,
}

impl<T> Default for DefaultRelayProcessor<T> {
    fn default() -> Self {
        Self {
            _phantom: std::marker::PhantomData,
        }
    }
}

impl<T> EventProcessor<T> for DefaultRelayProcessor<T>
where
    T: Send + Sync + std::fmt::Debug + 'static,
{
    // Uses all default implementations from the trait
}
