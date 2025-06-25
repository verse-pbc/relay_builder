# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.4.0] - 2025-06-25

### Changed
- **BREAKING**: Implemented database actor pattern with hybrid response system
  - `RelayDatabase::new()` and related constructors now return `(RelayDatabase, DatabaseSender)` tuple
  - `RelayConfig::create_database()` now returns `(Arc<RelayDatabase>, DatabaseSender)` tuple
  - Database write operations moved to `DatabaseSender` actor interface
  - Added `ResponseHandler` enum supporting both fire-and-forget and synchronous operations
  - **Note**: Simple `RelayConfig::new(url, db_path, keys)` API remains unchanged for basic usage
- **BREAKING**: Updated `NostrConnectionState` to require `db_sender` field for database operations
- **BREAKING**: Modified connection setup process to pass `DatabaseSender` to middleware
- **BREAKING**: Changed `EventProcessor` trait methods to use `Arc<RwLock<T>>` for custom state
  - `handle_event`, `verify_filters`, and `can_see_event` now take `Arc<RwLock<T>>` instead of `&mut T` or `&T`
  - Allows processors to choose between read-only or write access to custom state
  - Eliminates performance penalty when state doesn't need to be modified
  - Enables better concurrent access patterns with reduced lock contention
- Renamed `middleware.rs` to `relay_middleware.rs` for better clarity
- Consolidated `NostrConnectionFactory` and `GenericNostrConnectionFactory` into a single implementation
  - `NostrConnectionFactory` now handles both Default and custom state factory scenarios
  - Removed duplicate `GenericNostrConnectionFactory` type

### Added
- `DatabaseSender` wrapper providing clean API for database commands with actor pattern
- `send_sync()` and `save_signed_event_sync()` methods for immediate acknowledgment in tests
- Hybrid response system supporting both WebSocket `MessageSender` and oneshot channels
- Command dispatcher architecture for routing database operations to specialized queues
- Graceful shutdown support by eliminating stored senders in database core

### Fixed
- Resolved graceful shutdown hanging issue where database held copies of senders
- Eliminated race conditions in tests by replacing `sleep()` with synchronous save methods
- Improved test reliability with immediate confirmation of database operations
- Fixed subscription metrics counter bug where counters were not decremented on connection disconnect
  - Added missing `on_disconnect` implementation to `RelayMiddleware`
  - Ensures proper cleanup of subscription counters when connections are dropped

### Performance
- Removed `event_start_time` and `event_kind` fields from `NostrConnectionState`
  - Metrics middleware now tracks its own timing state internally
  - Reduces unnecessary state mutations and write lock requirements
- Optimized `handle_subscription` to use read locks instead of write locks
- Direct `Arc<RwLock<T>>` passing eliminates double-wrapping overhead in custom state management

### Internal
- Added tokio tracing and console-subscriber dependencies for debugging
- Updated all examples and benchmarks to use new `DatabaseSender` interface
- Enhanced CI workflow to use correct example names
- Marked `test_drop_completes_pending_work` as ignored due to LMDB limitation
  - LMDB doesn't allow reopening the same database file in the same process

## [0.3.0] - 2025-06-24

### Changed
- **BREAKING**: Renamed `RelayHandlers` to `RelayService` for better clarity
- **BREAKING**: Removed deprecated builder methods: `build_server()`, `build_handler()`, `build_handlers()`, `build_axum_handler()`
- **BREAKING**: Simplified API with three main build methods:
  - `build()` - Returns the WebSocket handler
  - `build_axum()` - Returns an Axum-compatible handler function
  - `build_relay_service()` - Returns the full RelayService with handler and relay info
- Reorganized examples with numbered format (01-09) for progressive learning
- Improved documentation structure with tutorial moved to examples/README.md

### Added
- New example: `05_protocol_features.rs` demonstrating NIPs 09, 40, and 70
- Comprehensive tutorial in examples/README.md covering all library features

### Removed
- Removed complex example files: `advanced_relay.rs`, `private_relay.rs`
- Removed outdated `TUTORIAL.md` file

## [0.2.0] - 2025-06-21

### Added
- `export_import` binary for database migration between versions
  - Can be installed with `cargo install nostr_relay_builder`
  - Helps migrate databases when there are breaking changes

### Changed
- **BREAKING**: Updated to latest verse-pbc/nostr libraries (rev: ece888e)
  - nostr-lmdb now uses scoped-heed 0.2.0-alpha.7 from crates.io
  - Our fork of nostr-lmdb now aligns with upstream's database format, making it compatible with the original nostr-lmdb, but breaking compatibility with databases created by previous versions of our fork (use the export_import tool to migrate).

### Dependencies
- Updated nostr-sdk to rev: ece888e
- Updated nostr to rev: ece888e
- Updated nostr-database to rev: ece888e
- Updated nostr-lmdb to rev: ece888e

## [0.1.0] - Initial Release

### Added
- Initial release of nostr_relay_builder
- Framework for building custom Nostr relays with pluggable business logic
- Middleware-based architecture for extensibility
- Built-in middleware for NIPs 09, 40, 42, and 70
- EventProcessor trait for high-level business logic implementation
- Support for multi-tenant deployments with subdomain isolation
- Comprehensive examples demonstrating various relay configurations