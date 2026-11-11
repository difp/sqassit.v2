# SQASSIT v2 — Rust migration

This repository tracks the migration of [SQASSIT](https://github.com/difp/sqassit) to Rust. The reference implementation in `legacy/` is a snapshot of commit `736a779f59d9e13d7e3a0f0401d6dd1c642cfada`. Its tests, static assets, and deployment documentation remain available for compatibility checks.

**Status:** migration in progress. The Python implementation remains the reference and is not yet replaced. See [the migration plan](RUST-MIGRATION-PLAN.md).

## Rust preview

With Rust installed, run `cargo run` from the repository root and open `http://127.0.0.1:8000`. This preview serves the original pages and static assets; the application API and bot are not implemented. `/readyz` deliberately returns 503, so this build must not be deployed as a replacement. `SQASSIT_BIND` and `SQASSIT_STATIC_DIR` override the bind address and static directory for development.

Run `cargo test --locked` for the current Rust checks. Progress and remaining work are documented in [MIGRATION-STATUS.md](MIGRATION-STATUS.md).
