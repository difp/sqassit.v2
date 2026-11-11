"""Exercise a release binary and staged assets without real credentials or Telegram."""
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
import urllib.error
import urllib.request

root = Path(__file__).resolve().parent.parent
binary = Path(sys.argv[1]).resolve()
with tempfile.TemporaryDirectory(prefix="sqassit-release-smoke-") as directory:
    staged = Path(directory)
    shutil.copy2(binary, staged / "sqassit")
    shutil.copytree(root / "legacy/app/static", staged / "static")
    # Bind an ephemeral loopback port. The child is the only network service used.
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        port = probe.getsockname()[1]
    environment = {"PATH": os.defpath, "SQASSIT_BIND": f"127.0.0.1:{port}", "SQASSIT_STATIC_DIR": str(staged / "static")}
    process = subprocess.Popen([str(staged / "sqassit")], cwd=staged, env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        base = f"http://127.0.0.1:{port}"
        for _ in range(100):
            if process.poll() is not None:
                raise AssertionError(f"server exited during startup: {process.returncode}")
            try:
                with urllib.request.urlopen(base + "/healthz", timeout=1) as response:
                    assert response.status == 200 and json.load(response) == {"ok": True}
                break
            except (OSError, urllib.error.URLError):
                time.sleep(0.1)
        else:
            raise AssertionError("release server did not start")
        with urllib.request.urlopen(base + "/", timeout=2) as response:
            assert response.status == 200 and b"telegram-web-app.js" in response.read()
            assert response.headers["X-Content-Type-Options"] == "nosniff"
        with urllib.request.urlopen(base + "/static/telegram-web-app.js", timeout=2) as response:
            assert response.status == 200 and response.read()
        try:
            urllib.request.urlopen(base + "/readyz", timeout=2)
        except urllib.error.HTTPError as error:
            assert error.code == 503
        else:
            raise AssertionError("preview must not report readiness")
    finally:
        if process.poll() is None:
            process.send_signal(signal.SIGINT)
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
                raise AssertionError("server did not shut down")
    assert process.returncode == 0, f"server exited with {process.returncode}"
print("Release smoke test passed")
