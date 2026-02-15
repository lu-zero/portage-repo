# Project Conventions

## Build Commands

```bash
cargo test                        # Run all tests (unit + doc)
cargo clippy -- -D warnings       # Lint — must be warning-free
cargo fmt --check                 # Format check — must pass
cargo doc --no-deps               # Build docs — must have no warnings
cargo run --example enumerate_repo -- /path/to/repo  # Smoke-test the example
```

## Architecture

- One primary type per module (`layout.rs` -> `LayoutConf`, `repository.rs` -> `Repository`, etc.)
- Modules are private (`mod`, not `pub mod`); public API is flat re-exports in `lib.rs`
- Shell integration uses [brush-core](https://crates.io/crates/brush-core) for bash evaluation
- Depends on [portage-atom](https://crates.io/crates/portage-atom) for atom parsing and
  [portage-metadata](https://crates.io/crates/portage-metadata) for cache entry types

## Dependencies

- `portage-atom` — PMS atom parsing (Cpn, Cpv, Dep, etc.)
- `portage-metadata` — metadata cache types (CacheEntry, EbuildMetadata, Eapi)
- `brush-core` + `brush-builtins` — Rust bash shell for sourcing ebuilds/eclasses
- `tokio` — async runtime required by brush
- `thiserror` — error derive macros

## PMS Compliance

This library implements the [Package Manager Specification (PMS)](https://projects.gentoo.org/pms/9/pms.html).
All public types must reference the relevant PMS section in their doc comments
(e.g. `See [PMS 4](...)`).

## Coding Style

- `rustfmt` — all code must be formatted
- No dead code, no unused dependencies
- Doc comments on all public types, fields, and enum variants
- Tests live in a `#[cfg(test)] mod tests` block at the bottom of each module

## Commits

[Conventional Commits](https://www.conventionalcommits.org/):

- `feat:` — new functionality
- `fix:` — bug fix
- `refactor:` — code restructuring without behaviour change
- `docs:` — documentation only
- `test:` — adding or updating tests
- `ci:` — CI/CD changes
- `chore:` — maintenance (dependencies, tooling)

## MSRV

Minimum Supported Rust Version is **1.88** (required by brush-core). CI tests against
both stable and MSRV. Do not use features that require a newer version without updating
`rust-version` in `Cargo.toml` and the CI matrix.

## Slop Warning

This codebase was largely AI-generated. Be skeptical of existing code — it may
contain bugs, incomplete PMS coverage, or surprising edge-case behaviour.
Do not assume existing patterns are correct; verify against the PMS.
