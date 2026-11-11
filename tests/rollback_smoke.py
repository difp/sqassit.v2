"""Rehearse a rollback: the Python reference opens and writes a Rust-migrated DB.

Requires SQASSIT_REFERENCE_PYTHON with legacy/requirements.txt installed. The
Rust server writes a profile marker, the Python reference connects to the same
database, reads and overwrites it, and Rust reads the Python write back.
"""
import hashlib
import hmac
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import subprocess
import sys
import tempfile
import time
import urllib.parse
import urllib.request

root = Path(__file__).resolve().parent.parent
binary = Path(sys.argv[1]).resolve()
reference = os.environ.get("SQASSIT_REFERENCE_PYTHON")
if not reference:
    raise SystemExit("set SQASSIT_REFERENCE_PYTHON to the reference Python interpreter")
token = "123456:ROLLBACK-TOKEN"


def init_data(user_id: int) -> str:
    pairs = {
        "auth_date": str(int(time.time())),
        "user": json.dumps({"id": user_id, "first_name": "Rollback"}, separators=(",", ":")),
    }
    check = "\n".join(f"{key}={value}" for key, value in sorted(pairs.items()))
    secret = hmac.new(b"WebAppData", token.encode(), hashlib.sha256).digest()
    signature = hmac.new(secret, check.encode(), hashlib.sha256).hexdigest()
    return urllib.parse.urlencode({**pairs, "hash": signature})


def start(staged: Path, database: Path):
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    environment = {
        "PATH": os.defpath,
        "SQASSIT_BIND": f"127.0.0.1:{port}",
        "SQASSIT_STATIC_DIR": str(staged / "static"),
        "DB_PATH": str(database),
        "BOT_TOKEN": token,
        "COACH_TELEGRAM_ID": "1",
    }
    process = subprocess.Popen(
        [str(staged / "sqassit")],
        cwd=staged,
        env=environment,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    base = f"http://127.0.0.1:{port}"
    for _ in range(100):
        if process.poll() is not None:
            raise AssertionError(f"server exited during startup: {process.returncode}")
        try:
            with urllib.request.urlopen(base + "/healthz", timeout=1) as response:
                assert response.status == 200
            return process, base
        except (OSError, urllib.error.URLError):
            time.sleep(0.1)
    stop(process)
    raise AssertionError("server did not start")


def stop(process) -> None:
    if process.poll() is None:
        process.send_signal(signal.SIGINT)
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait()
            raise AssertionError("server did not shut down")
    assert process.returncode == 0, f"server exited with {process.returncode}"


def call(base: str, signed: str, payload=None):
    headers = {"X-Init-Data": signed}
    body = None
    method = "GET"
    if payload is not None:
        headers["Content-Type"] = "application/json"
        body = json.dumps(payload).encode()
        method = "PUT"
    request = urllib.request.Request(base + "/api/me" if payload is None else base + "/api/me/profile", data=body, headers=headers, method=method)
    with urllib.request.urlopen(request, timeout=5) as response:
        return json.load(response)


python_code = """
import asyncio, json, os, sys
sys.path.insert(0, os.environ["LEGACY"])
from app import database as db

async def main():
    await db.connect(os.environ["DB_PATH"])
    try:
        row = await db.get_user(42)
        read = row["about"]
        await db._execute("UPDATE users SET about='python rollback' WHERE telegram_id=42")
        await db._execute("INSERT INTO trainings(title,training_type,start_at,duration_min,capacity,created_by,created_at) VALUES('Python rollback','Squash',?,60,2,42,?)", (int(__import__('time').time()) + 3600, int(__import__('time').time())))
        print(json.dumps({"read": read}))
    finally:
        await db.close()

asyncio.run(main())
"""

with tempfile.TemporaryDirectory(prefix="sqassit-rollback-") as directory:
    staged = Path(directory)
    shutil.copy2(binary, staged / "sqassit")
    shutil.copytree(root / "legacy/app/static", staged / "static")
    database = staged / "live.sqlite"
    signed = init_data(42)

    process, base = start(staged, database)
    try:
        call(base, signed, {"about": "rust marker"})
        assert call(base, signed)["user"]["about"] == "rust marker"
    finally:
        stop(process)

    environment = dict(os.environ)
    environment.update(
        LEGACY=str(root / "legacy"),
        DB_PATH=str(database),
        BOT_TOKEN=token,
        COACH_TELEGRAM_ID="1",
        BOT_MODE="polling",
        TIMEZONE="Europe/Moscow",
        REQUIRE_EVENT_APPROVAL="false",
        PHONE_REQUIRED_FROM="",
        BOT_PROXY="",
        WEBHOOK_BASE_URL="https://example.test",
    )
    result = subprocess.run(
        [reference, "-c", python_code],
        env=environment,
        capture_output=True,
        text=True,
    )
    if result.returncode != 0:
        raise AssertionError(f"Python rollback failed: {result.stderr}")
    assert json.loads(result.stdout.strip().splitlines()[-1])["read"] == "rust marker"

    process, base = start(staged, database)
    try:
        assert call(base, signed)["user"]["about"] == "python rollback"
    finally:
        stop(process)
print("Rollback smoke test passed")
