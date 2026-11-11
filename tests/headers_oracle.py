"""Offline Python reference for the opt-in security/cache header test."""
import asyncio
import json
import os
import sys
import tempfile

os.environ.update(
    BOT_TOKEN="123456:TEST-TOKEN", COACH_TELEGRAM_ID="123", BOT_MODE="polling",
    TIMEZONE="Europe/Moscow", REQUIRE_EVENT_APPROVAL="false", PHONE_REQUIRED_FROM="",
    BOT_PROXY="", WEBHOOK_BASE_URL="https://example.test",
)
sys.path.insert(0, os.path.abspath("legacy"))
import httpx2
from app import database as db
from app import main

PATHS = [
    "/",
    "/coach",
    "/match",
    "/tabs",
    "/healthz",
    "/api/me",
    "/static/telegram-web-app.js",
    "/static/squashtabs/index.html",
]
HEADERS = [
    "content-security-policy",
    "x-content-type-options",
    "referrer-policy",
    "permissions-policy",
    "cache-control",
    "content-type",
]


async def run():
    with tempfile.TemporaryDirectory(prefix="sqassit-headers-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            async with httpx2.AsyncClient(
                transport=httpx2.ASGITransport(app=main.app),
                base_url="http://reference",
            ) as client:
                responses = []
                for path in PATHS:
                    response = await client.get(path)
                    responses.append({
                        "path": path,
                        "status": response.status_code,
                        "headers": {
                            name: response.headers.get(name) for name in HEADERS
                        },
                    })
                print(json.dumps({"responses": responses}, ensure_ascii=False))
        finally:
            await db.close()


asyncio.run(run())
