# Deploying the Rust preview

This is an operational sketch for the Rust binary, not a validated production
runbook. The Python deployment in `legacy/docs/DEPLOY.md` remains the reference
until the migration is accepted. See `MIGRATION-STATUS.md` for the remaining
gates, including real Telegram delivery and a target-platform rehearsal.

## Build and stage

```bash
cargo build --release --locked
install -Dm755 target/release/sqassit /opt/sqassit/sqassit
mkdir -p /opt/sqassit/static /opt/sqassit/data
cp -a legacy/app/static/. /opt/sqassit/static/
```

The binary serves the existing templates from disk, so the staged `static`
directory must contain the same files as `legacy/app/static` (the templates are
compiled in, the static assets are read at runtime).

## Configuration

Environment names match the Python service where possible:

```ini
BOT_TOKEN=123456789:AA...
COACH_TELEGRAM_ID=123456789
TIMEZONE=Europe/Moscow
DB_PATH=/opt/sqassit/data/squash.db
AVATAR_CACHE_DIR=/opt/sqassit/data/cache/avatars
SQASSIT_STATIC_DIR=/opt/sqassit/static
SQASSIT_BIND=127.0.0.1:8000
BOT_MODE=polling
WEBHOOK_BASE_URL=https://app.example.com
WEBHOOK_SECRET=
BOT_PROXY=
ALLOW_DEV_INIT_DATA=false
```

`BOT_TOKEN` and `COACH_TELEGRAM_ID` must be set together; without them the HTTP
preview starts with no profile service and `/readyz` reports not ready. Set
`SQASSIT_ENABLE_BOT=true` only for a dedicated test bot. Polling and webhook are
mutually exclusive; a failed webhook check stops startup instead of deleting an
existing webhook. Set `SQASSIT_ENABLE_REMINDERS=true` for a single scheduler per
database. The Xray/VLESS scripts are still Python-only.

## systemd

```ini
[Unit]
Description=SQASSIT Rust preview
After=network.target

[Service]
Type=simple
User=sqassit
WorkingDirectory=/opt/sqassit
EnvironmentFile=/opt/sqassit/.env
ExecStart=/opt/sqassit/sqassit
Restart=always
RestartSec=5

NoNewPrivileges=true
PrivateTmp=true
ProtectSystem=full
ProtectHome=true
ReadWritePaths=/opt/sqassit/data

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable --now sqassit
sudo systemctl status sqassit --no-pager
```

Put nginx in front of `127.0.0.1:8000` exactly as in `legacy/docs/DEPLOY.md`,
including `X-Forwarded-Proto` and `X-Forwarded-For`. Keep one active writer and
one bot consumer; do not run the Rust and Python services on the same database
at the same time.

## Backup and restore

Use SQLite's online backup API while the service is running:

```bash
python3 - <<'PY'
import sqlite3
with sqlite3.connect("/opt/sqassit/data/squash.db") as source, \
     sqlite3.connect("/opt/sqassit/data/backup.sqlite") as destination:
    source.backup(destination)
PY
```

`tests/backup_restore_smoke.py` performs this copy and boots from it. To restore,
stop the service, verify the copy with `PRAGMA integrity_check`, move the old
database aside, put the copy at `DB_PATH`, and start the service. Keep the
avatar cache out of the database backup; it can be deleted and is refilled.

## Rollback to Python

The rehearsal in `tests/rollback_smoke.py` lets the Python reference open a
database migrated by Rust, read and overwrite a profile and add a training, then
reads both changes back through Rust. Before a real rollback, stop both services,
take another SQLite backup, and point Python at the same `DB_PATH`. If a future
Rust migration adds a schema change Python cannot open, restore the pre-cutover
backup instead and account for writes made after it.

## Render

The legacy `render.yaml` is Python-only. For a Rust preview, use a Docker or
native build with:

- build: `cargo build --release --locked`
- start: `./target/release/sqassit`
- `SQASSIT_BIND=0.0.0.0:$PORT`
- a persistent disk mounted at `/var/data`, with `DB_PATH`, `AVATAR_CACHE_DIR`,
  and the staged static directory on that disk
- `BOT_TOKEN` and `COACH_TELEGRAM_ID` as secrets
- health check `/healthz`; readiness `/readyz` also checks Telegram

This has not been rehearsed on Render or on the target VPS.
