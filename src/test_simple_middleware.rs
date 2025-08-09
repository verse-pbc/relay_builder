//! Simple test for the new middleware system

#[cfg(test)]
mod tests {
    use crate::middleware_chain::chain;
    use crate::middlewares::{ErrorHandlingMiddleware, NostrLoggerMiddleware};
    use crate::nostr_handler::IntoHandlerFactory;

    #[test]
    fn test_middleware_chain_creation() {
        // Test that we can create a chain with our new middleware
        let _chain = chain::<()>()
            .with(NostrLoggerMiddleware::new())
            .with(ErrorHandlingMiddleware::new())
            .build();
    }

    #[test]
    fn test_handler_factory_creation() {
        // Test that we can create a handler factory
        let chain = chain::<()>()
            .with(NostrLoggerMiddleware::new())
            .with(ErrorHandlingMiddleware::new());

        // Create EventIngester for test
        let keys = std::sync::Arc::new(nostr_sdk::Keys::generate());
        let event_ingester = crate::event_ingester::EventIngester::new(keys);

        let _factory = chain.into_handler_factory(
            event_ingester,
            crate::config::ScopeConfig::Disabled,
            10_000, // Default channel size for tests
        );
    }
}
