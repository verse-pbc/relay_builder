<!--
Sync Impact Report
==================
Version change: 0.0.0 → 1.0.0 (initial ratification)
Modified principles: N/A (initial version)
Added sections:
  - Core Principles (5 principles)
  - Rust Standards section
  - Quality Gates section
  - Governance section
Templates requiring updates:
  ✅ plan-template.md - Constitution Check section compatible
  ✅ spec-template.md - Requirements section compatible
  ✅ tasks-template.md - Test-first guidance compatible
  ✅ checklist-template.md - General purpose, no changes needed
  ✅ agent-file-template.md - General purpose, no changes needed
Follow-up TODOs: None
-->

# Relay Builder Constitution

## Core Principles

### I. Rust Guidelines Compliance

All Rust code in this project MUST adhere to Microsoft's Pragmatic Rust Guidelines as implemented by the `applying-rust-guidelines` skill. This includes:

- **Error Handling**: Use `Result` and `Option` types appropriately; avoid panics in library code
- **Documentation**: First sentence of doc comments MUST be under 15 words; non-trivial public functions MUST include examples
- **API Design**: Follow Rust API guidelines for naming, borrowing, and trait design
- **Async Patterns**: Use proper async/await patterns; avoid blocking in async contexts
- **Safety**: Unsafe code requires safety comments explaining invariants; prefer safe abstractions

**Rationale**: Consistent adherence to established Rust best practices ensures maintainability, reduces bugs, and makes the codebase accessible to Rust developers familiar with idiomatic patterns.

### II. Test Coverage Requirements

All production code MUST have corresponding tests. Testing standards:

- **Unit Tests**: Required for all non-trivial functions; test edge cases and error paths
- **Integration Tests**: Required for middleware chains, EventProcessor implementations, and WebSocket handling
- **Contract Tests**: Required for public API changes that affect downstream users
- **Test Organization**: Use `#[cfg(test)]` modules for unit tests; `tests/` directory for integration tests
- **Assertion Quality**: Tests MUST verify behavior, not just absence of panics

Run `cargo test` before all commits. Run `cargo test --all-features` before releases.

**Rationale**: A framework used by production relays requires high confidence in correctness. Tests document expected behavior and prevent regressions.

### III. Code Quality Gates

All code changes MUST pass these gates before merge:

- **Compilation**: `cargo check` MUST succeed with no errors
- **Linting**: `cargo clippy --all-targets --all-features -- -D warnings` MUST pass
- **Formatting**: `cargo fmt --check` MUST pass (run `cargo fmt` to auto-fix)
- **Tests**: `cargo test --all-features` MUST pass with no failures
- **Documentation**: `cargo doc --no-deps` MUST succeed with no warnings

**Rationale**: Automated quality gates catch issues early and maintain consistent code quality across all contributions.

### IV. Simplicity and Minimalism

Code complexity MUST be justified. Guidelines:

- **YAGNI**: Do not implement features for hypothetical future requirements
- **Single Responsibility**: Each module, struct, and function SHOULD have one clear purpose
- **Avoid Premature Abstraction**: Three similar instances before creating an abstraction
- **No Dead Code**: Remove unused code, imports, and dependencies; do not comment out code
- **Minimal Comments**: Code SHOULD be self-documenting; comments explain "why", not "what"

**Rationale**: As a framework, relay_builder must remain approachable. Unnecessary complexity increases maintenance burden and makes the codebase harder for users to understand and extend.

### V. API Stability and Breaking Changes

Public API changes require careful consideration:

- **Semantic Versioning**: Follow semver strictly; breaking changes increment MAJOR version
- **Deprecation Path**: Deprecated APIs MUST include migration guidance in deprecation notices
- **Feature Flags**: New experimental features SHOULD be behind feature flags initially
- **Documentation**: All public items MUST have doc comments; breaking changes MUST update examples
- **Changelog**: Maintain CHANGELOG.md with user-facing changes

**Rationale**: Production relays depend on this framework. API stability and clear migration paths are essential for user trust.

## Rust Standards

This section codifies Rust-specific standards beyond the guidelines skill:

- **Edition**: Use Rust 2021 edition
- **MSRV**: Minimum Supported Rust Version changes are breaking changes
- **Dependencies**: Prefer well-maintained crates; evaluate security advisories via `cargo audit`
- **Async Runtime**: Tokio is the standard async runtime; do not introduce alternative runtimes
- **Error Types**: Use `thiserror` for library errors; `anyhow` only in examples and tests
- **Logging**: Use `tracing` for structured logging; avoid `println!` in library code

## Quality Gates

Detailed gate requirements for the CI/CD pipeline:

| Gate | Command | Failure Action |
|------|---------|----------------|
| Build | `cargo build --all-features` | Block merge |
| Test | `cargo test --all-features` | Block merge |
| Clippy | `cargo clippy --all-targets --all-features -- -D warnings` | Block merge |
| Format | `cargo fmt --check` | Block merge |
| Docs | `cargo doc --no-deps` | Warn (block on errors) |
| Audit | `cargo audit` | Warn (block on critical) |

## Governance

### Amendment Process

1. Propose changes via pull request modifying this constitution
2. Changes require review and approval
3. Breaking changes to principles require migration plan for existing code
4. Version the constitution using semantic versioning

### Compliance

- All PRs MUST pass quality gates defined in this constitution
- Code reviews SHOULD verify adherence to principles
- Principle violations MUST be justified in PR description if unavoidable
- Use `.specify/memory/constitution.md` as the authoritative source

### Versioning Policy

- **MAJOR**: Principle removals or redefinitions that change enforcement
- **MINOR**: New principles or materially expanded guidance
- **PATCH**: Clarifications, wording improvements, typo fixes

**Version**: 1.0.0 | **Ratified**: 2025-01-24 | **Last Amended**: 2025-01-24
