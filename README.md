# SQASSIT v2 — Rust migration

This repository tracks the migration of [SQASSIT](https://github.com/difp/sqassit) to Rust. The reference implementation in `legacy/` is a snapshot of commit `736a779f59d9e13d7e3a0f0401d6dd1c642cfada`. Its tests, static assets, and deployment documentation remain available for compatibility checks.

**Status:** migration in progress. The Python implementation remains the reference and is not yet replaced. See [the migration plan](RUST-MIGRATION-PLAN.md).

## Rust preview

With Rust installed, run `cargo run` from the repository root and open `http://127.0.0.1:8000`. The preview serves the original pages and implements much of the API, but parity and real Telegram delivery are not yet verified. Set `BOT_TOKEN`, `COACH_TELEGRAM_ID`, and optionally `DB_PATH` to enable authenticated endpoints. With the opt-in bot, booking and cancellation trigger best-effort Telegram notifications. `/readyz` deliberately returns 503, so this build must not be deployed as a replacement. `SQASSIT_BIND` and `SQASSIT_STATIC_DIR` override the bind address and static directory for development.

Run `cargo test --locked` for the current Rust checks. Progress and remaining work are documented in [MIGRATION-STATUS.md](MIGRATION-STATUS.md).

The Python/Rust contract test uses separate temporary databases and suppresses Telegram background tasks. Install `legacy/requirements.txt` and `httpx2==2.14.0` in a Python 3.12 environment, then run `SQASSIT_REFERENCE_PYTHON=/path/to/python cargo test --locked --test python_contract -- --ignored`. It compares a limited profile, training, coach management, waitlist, match and private-chat workflow, including selected access-denied cases and selected final database columns; timestamps and scoped-token values are normalized, so this is not complete parity verification. CI runs it separately. The Python SQLite worker may require running outside restrictive sandboxes.

Experimental training reminders can be enabled explicitly with `SQASSIT_ENABLE_REMINDERS=true`, a test bot token and a test database. This sends real Telegram messages. Run at most one scheduler per database. This preview is not suitable for production. `BOT_PROXY` can configure HTTP or SOCKS proxy access.

For a dedicated **test bot only**, `SQASSIT_ENABLE_BOT=true` enables update handling. `BOT_MODE=polling` checks Telegram for an existing webhook and refuses to start if one is active or its status cannot be checked; it never deletes the webhook or drops updates. `BOT_MODE=webhook` requires `WEBHOOK_BASE_URL` and a secret of at least 32 characters in `WEBHOOK_SECRET`. Set `SQASSIT_REGISTER_WEBHOOK=true` only when you intentionally want this preview to replace the bot’s registered Telegram webhook. Unrelated updates are ignored; failed supported updates may be retried. Do not run this alongside another poller or use a production bot: the migration and event notifications are incomplete.
