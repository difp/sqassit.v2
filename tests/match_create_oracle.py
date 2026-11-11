"""Offline Python reference for match creation and feed notifications."""
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


async def run():
    now = int(sys.argv[1])
    fake = FakeBot()
    main.app.state.bot = fake
    with tempfile.TemporaryDirectory(prefix="sqassit-match-create-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            await db._execute(
                "INSERT INTO users(telegram_id,first_name,display_mode,notifications_enabled,"
                "notify_matches,created_at) VALUES (1,'Creator','name',1,1,?),"
                "(2,'Second','name',1,1,?),(3,'Third','name',1,0,?),"
                "(4,'Pending','name',1,1,?),(5,'Quiet','name',0,0,?),"
                "(6,'Coauthor','name',1,1,?)",
                (now, now, now, now, now, now),
            )
            await db._execute(
                "INSERT INTO matches(id,title,location,start_at,duration_min,max_players,level,"
                "created_by,created_at) VALUES (9,'Created <A&B>','Hall & Court',?,60,4,"
                "'Intermediate',1,?)",
                (now + 7200, now),
            )
            await db._execute(
                "INSERT INTO match_players(match_id,user_id,status,created_at) VALUES "
                "(9,1,'booked',?),(9,2,'booked',?),(9,4,'pending',?),"
                "(9,5,'booked',?),(9,6,'booked',?)",
                (now, now + 1, now + 2, now + 3, now + 4),
            )
            await db._execute(
                "INSERT INTO event_coauthors(event_type,event_id,user_id,added_by,created_at) "
                "VALUES ('match',9,6,1,?)",
                (now,),
            )
            await db._execute(
                "INSERT INTO matches(id,title,location,start_at,duration_min,max_players,"
                "created_by,created_at) VALUES (10,'Later','',?,60,4,2,?)",
                (now + 100000, now),
            )

            created = await db.get_match(9)
            await main._notify_match_event(created, 1, "joined")
            await main._notify_match_feed(1)

            notifications = await db._fetchall(
                "SELECT kind,entity_id,chat_id,user_id,message_id,text "
                "FROM event_notifications ORDER BY kind,entity_id,chat_id,user_id"
            )
            print(json.dumps({
                "calls": fake.calls,
                "notifications": [dict(row) for row in notifications],
            }, ensure_ascii=False))
        finally:
            await db.close()


asyncio.run(run())
