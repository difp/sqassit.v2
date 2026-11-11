# SQASSIT v2 — Rust migration

This repository tracks the migration of [SQASSIT](https://github.com/difp/sqassit) to Rust. The reference implementation in `legacy/` is a snapshot of commit `736a779f59d9e13d7e3a0f0401d6dd1c642cfada`. Its tests, static assets, and deployment documentation remain available for compatibility checks.

**Status:** migration in progress. The Python implementation remains the reference and is not yet replaced. See [the migration plan](RUST-MIGRATION-PLAN.md).

## Rust preview

With Rust installed, run `cargo run` from the repository root and open `http://127.0.0.1:8000`. This preview serves the original pages and static assets; only the training schedule, booking/cancellation, basic coach training creation/list/edit/trash and participant management, profile read/edit, private player/coach chat (without Telegram notifications), public player profiles, avatar proxy, calendar export, onboarding acknowledgement and notification settings are available; the rest of the API and substantial bot behavior are not implemented. Set `BOT_TOKEN`, `COACH_TELEGRAM_ID`, and optionally `DB_PATH` to enable the profile endpoints. Booking and cancellation do not yet send Telegram notifications. `/readyz` deliberately returns 503, so this build must not be deployed as a replacement. `SQASSIT_BIND` and `SQASSIT_STATIC_DIR` override the bind address and static directory for development.

Run `cargo test --locked` for the current Rust checks. Progress and remaining work are documented in [MIGRATION-STATUS.md](MIGRATION-STATUS.md).

Experimental training reminders can be enabled explicitly with `SQASSIT_ENABLE_REMINDERS=true`, a test bot token and a test database. This sends real Telegram messages. Cleanup for other notification types has not yet been migrated; run at most one scheduler per database. This preview is not suitable for production. `BOT_PROXY` can configure HTTP or SOCKS proxy access.

For a dedicated **test bot only**, `SQASSIT_ENABLE_BOT=true` enables update handling. `BOT_MODE=polling` uses long polling; `BOT_MODE=webhook` requires `WEBHOOK_BASE_URL` and a secret of at least 32 characters in `WEBHOOK_SECRET`. Set `SQASSIT_REGISTER_WEBHOOK=true` only when you intentionally want this preview to replace the bot’s registered Telegram webhook. Unsupported updates stop polling or receive HTTP 503 in webhook mode. Do not run this alongside another poller or use a production bot: the migration and event notifications are incomplete.
