"""Offline Python reference for the opt-in cross-implementation contract test."""
import asyncio
import hashlib
import hmac
import json
import os
import sys
import tempfile
from urllib.parse import urlencode

os.environ.update(
    BOT_TOKEN="123456:TEST-TOKEN", COACH_TELEGRAM_ID="123", BOT_MODE="polling",
    TIMEZONE="Europe/Moscow", REQUIRE_EVENT_APPROVAL="false", PHONE_REQUIRED_FROM="",
    BOT_PROXY="",
)
sys.path.insert(0, os.path.abspath("legacy"))
import httpx2
from app import database as db
from app import main


def signed(user, now):
    fields = {"auth_date": str(now), "user": json.dumps(user, ensure_ascii=True, separators=(",", ":"))}
    check = "\n".join(f"{k}={fields[k]}" for k in sorted(fields))
    secret = hmac.new(b"WebAppData", os.environ["BOT_TOKEN"].encode(), hashlib.sha256).digest()
    fields["hash"] = hmac.new(secret, check.encode(), hashlib.sha256).hexdigest()
    return urlencode(fields)


def discard(coro):
    coro.close()


async def run():
    now, date = int(sys.argv[1]), sys.argv[2]
    main._spawn = discard
    with tempfile.TemporaryDirectory(prefix="sqassit-contract-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            await db.ensure_coach(123)
            await db.upsert_user(telegram_id=123, username=None, first_name="Тренер", is_coach=True)
            headers = {
                "coach": {"X-Init-Data": signed({"id":123,"first_name":"Coach"}, now)},
                "player": {"X-Init-Data": signed({"id":42,"first_name":"Player"}, now)},
                "player2": {"X-Init-Data": signed({"id":43,"first_name":"Second"}, now)},
                "none": {},
            }
            scenarios = [
                ("GET", "/api/me", "none", None),
                ("GET", "/api/me", "player", None),
                ("PUT", "/api/me/profile", "player", {"about":"  Люблю сквош  ","display_mode":"animal","show_username":False}),
                ("PUT", "/api/me/notifications", "player", {"notify_matches":False}),
                ("POST", "/api/me/onboarding", "player", None),
                ("GET", "/api/me", "player", None),
                ("GET", "/api/levels", "player", None),
                ("GET", "/api/coach/players", "player", None),
                ("POST", "/api/trainings/999/book", "player", None),
                ("POST", "/api/trainings/999/cancel", "player", None),
                ("POST", "/api/matches/999/join", "player", None),
                ("POST", "/api/coach/trainings", "coach", {"date":date,"time":"12:00","training_type":"Squash","capacity":1}),
                ("GET", "/api/trainings?days=7", "player", None),
                ("POST", "/api/trainings/1/room", "player", None),
                ("POST", "/api/trainings/1/cancel", "player", None),
                ("POST", "/api/trainings/1/book", "player", None),
                ("GET", "/api/trainings?days=7", "player", None),
                ("POST", "/api/trainings/1/book", "player2", None),
                ("POST", "/api/trainings/1/cancel", "player", None),
                ("GET", "/api/trainings?days=7", "player2", None),
                ("POST", "/api/trainings/1/cancel", "player2", None),
                ("POST", "/api/matches", "player", {"date":date,"time":"13:00","title":"Match"}),
                ("POST", "/api/matches/1/room", "player2", None),
                ("DELETE", "/api/matches/1", "player2", None),
                ("POST", "/api/matches/1/join", "player2", None),
                ("GET", "/api/matches", "player2", None),
                ("POST", "/api/matches/1/leave", "player2", None),
                ("DELETE", "/api/matches/1", "player", None),
                ("POST", "/api/matches/1/join", "player2", None),
                ("POST", "/api/messages/coach", "player", {"text":"  Привет тренер  ","training_id":1}),
                ("GET", "/api/coach/messages", "coach", None),
                ("GET", "/api/coach/messages/42", "coach", None),
                ("POST", "/api/coach/messages/42", "coach", {"text":"Здравствуйте"}),
                ("GET", "/api/messages/thread", "player", None),
                ("GET", "/api/messages/thread", "player2", None),
                ("GET", "/api/coach/messages/42", "player2", None),
                ("GET", "/api/coach/players", "coach", None),
                ("PUT", "/api/coach/players/42/notes", "coach", {}),
            ]
            async with httpx2.AsyncClient(transport=httpx2.ASGITransport(app=main.app), base_url="http://reference") as client:
                result = []
                for method, path, actor, body in scenarios:
                    response = await client.request(method, path, headers=headers[actor], **({"json":body} if body is not None else {}))
                    result.append({"status":response.status_code,"body":response.json()})
            print(json.dumps(result, ensure_ascii=False))
        finally:
            await db.close()

asyncio.run(run())
