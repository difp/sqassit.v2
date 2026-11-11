"""Rehearse an online SQLite backup and restore with a release binary.

The server writes a marker profile, the database is copied with SQLite's online
backup API while the server is running, and a second server boots from the copy
and reads the marker back. This is a local rehearsal, not a deployment check.
"""
import hashlib
import hmac
import json
import os
from pathlib import Path
import shutil
import signal
import socket
import sqlite3
import subprocess
import sys
import tempfile
import time
import urllib.error
import urllib.parse
import urllib.request

root = Path(__file__).resolve().parent.parent
binary = Path(sys.argv[1]).resolve()
token = "123456:BACKUP-TOKEN"


def init_data(user_id: int) -> str:
    pairs = {
        "auth_date": str(int(time.time())),
        "user": json.dumps({"id": user_id, "first_name": "Backup"}, separators=(",", ":")),
    }
    check = "\n".join(f"{key}={value}" for key, value in sorted(pairs.items()))
    secret = hmac.new(b"WebAppData", token.encode(), hashlib.sha256).digest()
    signature = hmac.new(secret, check.encode(), hashlib.sha256).hexdigest()
    return urllib.parse.urlencode({**pairs, "hash": signature})


def free_port() -> int:
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


def start(staged: Path, database: Path):
    port = free_port()
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


def request(base: str, path: str, *, payload=None, init: str = ""):
    headers = {"X-Init-Data": init}
    body = None
    if payload is not None:
        headers["Content-Type"] = "application/json"
        body = json.dumps(payload).encode()
    request = urllib.request.Request(base + path, data=body, headers=headers, method="PUT" if payload is not None else "GET")
    with urllib.request.urlopen(request, timeout=5) as response:
        return response.status, json.load(response)


with tempfile.TemporaryDirectory(prefix="sqassit-backup-") as directory:
    staged = Path(directory)
    shutil.copy2(binary, staged / "sqassit")
    shutil.copytree(root / "legacy/app/static", staged / "static")
    database = staged / "live.sqlite"
    restored = staged / "restored.sqlite"
    signed = init_data(42)

    process, base = start(staged, database)
    try:
        status, _ = request(
            base,
            "/api/me/profile",
            payload={"about": "backup marker"},
            init=signed,
        )
        assert status == 200
        status, body = request(base, "/api/me", init=signed)
        assert status == 200 and body["user"]["about"] == "backup marker"

        # SQLite's backup API is safe while the server keeps the database open.
        source = sqlite3.connect(database)
        with sqlite3.connect(restored) as destination:
            source.backup(destination)
        source.close()
    finally:
        stop(process)

    process, base = start(staged, restored)
    try:
        status, body = request(base, "/api/me", init=signed)
        assert status == 200 and body["user"]["about"] == "backup marker"
        with sqlite3.connect(restored) as connection:
            count = connection.execute("SELECT count(*) FROM users").fetchone()[0]
        assert count >= 1
    finally:
        stop(process)
print("Backup/restore smoke test passed")
