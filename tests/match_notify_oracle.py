"""Offline Python reference for the opt-in match notification test."""
import asyncio
import json
import os
import sys
import tempfile
from types import SimpleNamespace

os.environ.update(
    BOT_TOKEN="123456:TEST-TOKEN", COACH_TELEGRAM_ID="123", BOT_MODE="polling",
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
        self.next_id = 100

    async def send_message(self, chat_id, text, **kwargs):
        self.next_id += 1
        params = {"chat_id": chat_id, "text": text}
        params.update({key: dump(value) for key, value in kwargs.items() if value is not None})
        params.pop("parse_mode", None)
        self.calls.append({"method": "sendMessage", "params": params})
        return SimpleNamespace(message_id=self.next_id)

    async def edit_message_text(self, **kwargs):
        params = {key: dump(value) for key, value in kwargs.items() if value is not None}
        params.pop("parse_mode", None)
        self.calls.append({"method": "editMessageText", "params": params})
        return True


async def run():
    now = int(sys.argv[1])
    fake = FakeBot()
    main.app.state.bot = fake
    with tempfile.TemporaryDirectory(prefix="sqassit-match-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            await db._execute(
                "INSERT INTO users(telegram_id,first_name,display_mode,notifications_enabled,"
                "notify_matches,created_at) VALUES "
                "(1,'Owner','name',1,1,?),(2,'Player Two','name',1,1,?),"
                "(3,'Leaver','name',1,1,?),(4,'Quiet','name',0,0,?),"
                "(5,'Coauthor','name',1,1,?),(6,'Pending','name',1,1,?),"
                "(7,'Requester','name',1,1,?)",
                (now, now, now, now, now, now, now),
            )
            await db._execute(
                "INSERT INTO matches(id,title,location,start_at,duration_min,max_players,"
                "created_by,created_at) VALUES (1,'Cup <A&B>','Court & 1',?,60,4,1,?)",
                (now + 7200, now),
            )
            await db._execute(
                "INSERT INTO match_players(match_id,user_id,status,created_at) VALUES "
                "(1,2,'booked',?),(1,4,'booked',?),(1,6,'pending',?)",
                (now + 10, now + 20, now + 30),
            )
            await db._execute(
                "INSERT INTO event_coauthors(event_type,event_id,user_id,added_by,created_at) "
                "VALUES ('match',1,5,1,?)",
                (now,),
            )
            match = await db.get_match(1)
            await main._notify_match_event(match, 3, "left")
            await db._execute(
                "INSERT INTO match_players(match_id,user_id,status,created_at) VALUES (1,7,'pending',?)",
                (now + 40,),
            )
            match = await db.get_match(1)
            await main._notify_match_event(match, 7, "pending")
            await db.delete_match(1)
            match = await db.get_match(1)
            await main._notify_match_deleted(match)
            notifications = await db._fetchall(
                "SELECT kind,entity_id,chat_id,user_id,message_id,text "
                "FROM event_notifications ORDER BY chat_id,user_id"
            )
            print(json.dumps({
                "calls": fake.calls,
                "notifications": [dict(row) for row in notifications],
            }, ensure_ascii=False))
        finally:
            await db.close()


asyncio.run(run())
