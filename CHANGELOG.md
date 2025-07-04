# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **Note**: This project is in alpha. Breaking changes are expected between releases.

## [0.5.0-alpha.1] - Unreleased

### Added
- WebSocket backend support: can now use either tungstenite (default) or fastwebsockets
- Unified WebSocket API via websocket_builder v0.2.0-alpha.1

### Changed
- **BREAKING**: Updated to websocket_builder v0.2.0-alpha.1 with new unified API
- **BREAKING**: MessageConverter trait now uses byte-based methods for better performance
- **BREAKING**: Database actor pattern with hybrid response system
- Moved to alpha versioning to reflect active development status

### Previous Changes (v0.1.0 - v0.4.1)
Previous versions implemented core functionality including:
- Middleware-based architecture with EventProcessor trait
- Built-in NIPs: 09 (deletion), 40 (expiration), 42 (auth), 70 (protected)
- Subdomain isolation for multi-tenant deployments
- Metrics, monitoring, and graceful shutdown
- Database optimizations and batch operations
- Simplified builder API with `build()`, `build_axum()`, and `build_relay_service()` methods

For detailed history of pre-alpha versions, see git history.