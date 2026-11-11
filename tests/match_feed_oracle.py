"""Offline Python reference for refreshing and removing existing match feeds."""
import asyncio
import json
import os
import sys
import tempfile
from types import SimpleNamespace

os.environ.update(
    BOT_TOKEN="123456:TEST-TOKEN", COACH_TELEGRAM_ID="1", BOT_MODE="polling",
    TIMEZONE="Europe/Moscow", REQUIRE_EVENT_APPROVAL="false", PHONE_REQUIRED_FROM="",
    BOT_PROXY="", WEBHOOK_BASE_URL="https://example.test",
)
sys.path.insert(0, os.path.abspath("legacy"))
from app import database as db
from app import main


def dump(value):
    if hasattr(value, "model_dump"):
        return value.model_dump(mode="json", exclude_none=True)
    return value


class FakeBot:
    def __init__(self):
        self.calls = []
        self.next_id = 1000

    async def send_message(self, chat_id, text, **kwargs):
        self.next_id += 1
        params = {"chat_id": chat_id, "text": text}
        params.update({key: dump(value) for key, value in kwargs.items() if value is not None})
        params.pop("parse_mode", None)
        self.calls.append({"method": "sendMessage", "params": params, "message_id": self.next_id})
        return SimpleNamespace(message_id=self.next_id)

    async def edit_message_text(self, **kwargs):
        params = {key: dump(value) for key, value in kwargs.items() if value is not None}
        params.pop("parse_mode", None)
        self.calls.append({"method": "editMessageText", "params": params})
        return True

    async def delete_message(self, chat_id, message_id):
        self.calls.append({
            "method": "deleteMessage",
            "params": {"chat_id": chat_id, "message_id": message_id},
        })


def sorted_calls(calls):
    return sorted(calls, key=lambda call: json.dumps(call, sort_keys=True))


async def snapshot():
    rows = await db._fetchall(
        "SELECT kind,entity_id,chat_id,user_id,message_id,text "
        "FROM event_notifications ORDER BY chat_id"
    )
    return [dict(row) for row in rows]


async def run():
    now = int(sys.argv[1])
    fake = FakeBot()
    main.app.state.bot = fake
    with tempfile.TemporaryDirectory(prefix="sqassit-match-feed-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            await db._execute(
                "INSERT INTO users(telegram_id,first_name,notifications_enabled,notify_matches,"
                "created_at) VALUES (2,'Stale',1,1,?),(3,'Current',1,1,?),(4,'Quiet',0,0,?)",
                (now, now, now),
            )
            await db._execute(
                "INSERT INTO matches(id,title,location,start_at,duration_min,max_players,"
                "created_by,created_at) VALUES (9,'Feed <A&B>','Hall & Court',?,60,4,1,?)",
                (now + 7200, now),
            )
            match = await db.get_match(9)
            text = main._match_feed_text([match])
            for chat, message, body in (
                (2, 401, "old feed"),
                (3, 402, text),
                (4, 403, "quiet feed"),
            ):
                await db._execute(
                    "INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,"
                    "text,updated_at,delete_at,sent_at) VALUES ('match_feed',0,?,?,?,?,?,?,?)",
                    (chat, chat, message, body, now, now + 86400, now),
                )

            await main._refresh_existing_match_feeds(now)
            first_calls = sorted_calls(fake.calls)
            first_rows = await snapshot()
            fake.calls.clear()

            # All matches are now in the past: remaining feeds are removed.
            await main._refresh_existing_match_feeds(now + 7201)
            second_calls = sorted_calls(fake.calls)
            second_rows = await snapshot()
            print(json.dumps({
                "first_calls": first_calls,
                "first_rows": first_rows,
                "second_calls": second_calls,
                "second_rows": second_rows,
            }, ensure_ascii=False))
        finally:
            await db.close()


asyncio.run(run())
