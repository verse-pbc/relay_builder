# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

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