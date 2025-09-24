# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

> **Note**: This project is in alpha. Breaking changes are expected between releases.

## [0.5.0-alpha.1] - Unreleased

### Added
- Rate limiting middleware (`RateLimitMiddleware`) using governor crate for efficient token bucket rate limiting
- WebSocket module integrated directly into relay_builder (previously external websocket_builder dependency)
- Example 07: Rate limiting demonstration

### Changed
- **BREAKING**: WebSocket handling now uses integrated `websocket` module instead of external websocket_builder dependency
- **BREAKING**: Consolidated websocket_builder functionality into relay_builder for simpler dependency management
- Optimized lock usage and eliminated allocations in hot paths for better performance
- Fixed duplicate message bug in error handling middleware chain
- Fixed middleware chain ordering to ensure RelayMiddleware is always innermost
- Updated to latest nostr-lmdb API
- Moved to alpha versioning to reflect active development status

### Removed
- External websocket_builder dependency (functionality now integrated)

### Previous Changes (v0.1.0 - v0.4.1)
Previous versions implemented core functionality including:
- Middleware-based architecture with EventProcessor trait
- Built-in NIPs: 09 (deletion), 40 (expiration), 42 (auth), 70 (protected)
- Subdomain isolation for multi-tenant deployments
- Metrics, monitoring, and graceful shutdown
- Database optimizations and batch operations
- Simplified builder API with `build()`, `build_axum()`, and `build_relay_service()` methods

For detailed history of pre-alpha versions, see git history.