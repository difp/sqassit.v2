# SQASSIT v2 — Rust migration

Rust migration of [SQASSIT](https://github.com/difp/sqassit), a Telegram Mini App
for a squash club. `legacy/` holds the Python reference (snapshot of
`736a779f59d9e13d7e3a0f0401d6dd1c642cfada`) used for compatibility checks.

> **Status: preview.** All 75 reference HTTP routes, the bot commands/callbacks
> and the reminder tick are ported and cross-checked against Python, but real
> Telegram delivery and production deployment are **not** verified. Do not
> deploy this crate in place of the Python application.
>
> Progress and remaining gates: [MIGRATION-STATUS.md](MIGRATION-STATUS.md) ·
> migration plan: [RUST-MIGRATION-PLAN.md](RUST-MIGRATION-PLAN.md)

## Run

```sh
cargo run                 # then open http://127.0.0.1:8000
```

Set `BOT_TOKEN` and `COACH_TELEGRAM_ID` to enable authenticated endpoints.
`/readyz` probes SQLite and the Telegram Bot API and returns 503 with a Russian
detail when either is unavailable.

### Environment

| Variable | Purpose |
| --- | --- |
| `BOT_TOKEN`, `COACH_TELEGRAM_ID` | Bot identity and owner (manager) |
| `DB_PATH` | SQLite file (default `./data/squash.db`) |
| `SQASSIT_BIND`, `SQASSIT_STATIC_DIR` | Bind address and static dir (development) |
| `AVATAR_CACHE_DIR` | Avatar disk cache (default `./data/cache/avatars`) |
| `SQASSIT_ENABLE_BOT=true` | Opt-in update handling (dedicated test bot only) |
| `BOT_MODE` | `polling` (default) or `webhook` |
| `WEBHOOK_BASE_URL`, `WEBHOOK_SECRET` | Required for `webhook`; secret ≥ 32 chars |
| `SQASSIT_REGISTER_WEBHOOK=true` | Let this preview claim the bot's webhook |
| `SQASSIT_ENABLE_REMINDERS=true` | Opt-in training reminder tick |
| `BOT_PROXY` | HTTP or SOCKS proxy for Telegram API |
| `ALLOW_DEV_INIT_DATA=true` | Dev-only `?initData=` sign-in (HMAC still verified) |

Bot notes: polling refuses to start while a webhook is active and never drops
updates; unrelated updates are ignored, failed supported ones may be retried.
Never run alongside another poller or with a production bot.

## Checks

```sh
cargo test --locked                       # unit tests (no Python needed)
cargo fmt --all -- --check && cargo clippy --all-targets -- -D warnings
```

Release smoke — stages the binary + static files, boots without credentials,
checks HTTP, stops (also rehearses the SQLite online backup and restart):

```sh
cargo build --release --locked
python3 tests/release_smoke.py target/release/sqassit
python3 tests/backup_restore_smoke.py target/release/sqassit
```

Rollback rehearsal — the Python reference opens the Rust-migrated database,
reads and overwrites it, and Rust reads the changes back (needs the reference
Python; see below):

```sh
SQASSIT_REFERENCE_PYTHON=/path/to/python python3 tests/rollback_smoke.py target/release/sqassit
```

Synthetic benchmarks (in-memory, not a production load test):

```sh
cargo test --release --locked --test search_benchmark -- --ignored --nocapture   # 50k players
cargo test --release --locked --test http_benchmark -- --ignored --nocapture     # 2k trainings / 12k bookings
```

### Python/Rust contract tests (opt-in)

Compare the Rust implementation with `legacy/`: HTTP workflows and final data,
the old-schema migration, security headers, bot commands/callbacks, reminder
tick, notification cleanup, enrollment replacement, training/match/chat
notifications, role notices, Unicode search normalization across every scalar
value, and all 75 routes requested unauthenticated and as player / coach /
manager — statuses and bodies for all 300 route/role pairs, except pydantic's error arrays for malformed bodies.
Timestamps, scoped tokens and bot defaults are normalized — this is
strong, but not complete, parity verification.

Needs a Python 3.12 environment with `legacy/requirements.txt` and
`httpx2==2.14.0` (may require running outside restrictive sandboxes):

```sh
tests=(python_contract schema_contract bot_contract reminder_contract cleanup_contract \
final_enrollment_contract match_notify_contract match_create_contract match_feed_contract \
chat_notify_contract headers_contract training_delete_contract application_notify_contract \
role_notify_contract training_edit_contract training_booking_contract training_coach_contract \
search_norm_contract authz_matrix_contract)
SQASSIT_REFERENCE_PYTHON=/path/to/python cargo test --locked "${tests[@]/#/--test=}" -- --ignored
```

CI ([`.github/workflows/rust.yml`](.github/workflows/rust.yml)) runs the fast
checks plus both smokes, and the contract suite against the reference
interpreter.

## Deployment

[DEPLOY-RUST.md](DEPLOY-RUST.md) sketches staging, systemd/nginx/Render setup,
SQLite backups and Python rollback. It has not been rehearsed on a target
platform yet.
