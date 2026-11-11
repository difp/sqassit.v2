"""Offline Python reference for the opt-in role-notice test."""
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
        self.next_id = 100

    async def send_message(self, chat_id, text, **kwargs):
        self.next_id += 1
        params = {"chat_id": chat_id, "text": text}
        params.update({key: dump(value) for key, value in kwargs.items() if value is not None})
        params.pop("parse_mode", None)
        self.calls.append({"method": "sendMessage", "params": params})
        return SimpleNamespace(message_id=self.next_id)

    async def edit_message_text(self, **kwargs):
        self.calls.append({"method": "editMessageText", "params": kwargs})
        return True


async def run():
    now = int(sys.argv[1])
    fake = FakeBot()
    main.app.state.bot = fake
    with tempfile.TemporaryDirectory(prefix="sqassit-role-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            await db._execute(
                "INSERT INTO users(telegram_id,first_name,notifications_enabled,notify_trainings,"
                "created_at) VALUES (5,'New Coach',1,1,?),(6,'Quiet',1,0,?),"
                "(7,'Applicant',1,1,?)",
                (now, now, now),
            )
            await db._execute(
                "INSERT INTO trainings(id,title,training_type,start_at,duration_min,capacity,"
                "created_by,created_at) VALUES (7,'x','Squash <A&B>',?,60,4,1,?)",
                (now + 7200, now),
            )
            training = await db.get_training(7)
            await main._notify_new_coach(5)
            await main._notify_training_transferred(5, training)
            await main._notify_training_transferred(6, training)
            await main._notify_application_decision(7, approved=False)
            notifications = await db._fetchall(
                "SELECT kind,entity_id,chat_id,user_id,message_id,text "
                "FROM event_notifications ORDER BY message_id"
            )
            print(json.dumps({
                "calls": fake.calls,
                "notifications": [dict(row) for row in notifications],
            }, ensure_ascii=False))
        finally:
            await db.close()


asyncio.run(run())
