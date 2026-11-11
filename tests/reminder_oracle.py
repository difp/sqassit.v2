"""Offline Python reference for the opt-in training reminder contract test."""
import asyncio
import json
import os
import sys
import tempfile

os.environ.update(
    BOT_TOKEN="123456:TEST-TOKEN", COACH_TELEGRAM_ID="123", BOT_MODE="polling",
    TIMEZONE="Europe/Moscow", REQUIRE_EVENT_APPROVAL="false", PHONE_REQUIRED_FROM="",
    BOT_PROXY="",
)
sys.path.insert(0, os.path.abspath("legacy"))
from app import bot as bot_module
from app import database as db


class FakeBot:
    def __init__(self):
        self.calls = []
        self.next_id = 100

    async def send_message(self, chat_id, text, **kwargs):
        params = {"chat_id": chat_id, "text": text}
        params.update({key: value for key, value in kwargs.items() if value is not None})
        params.pop("parse_mode", None)
        self.calls.append({"method": "sendMessage", "params": params})
        self.next_id += 1
        from types import SimpleNamespace
        return SimpleNamespace(message_id=self.next_id)


async def run():
    now = int(sys.argv[1])
    fake = FakeBot()
    with tempfile.TemporaryDirectory(prefix="sqassit-reminder-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            await db._execute(
                "INSERT INTO users (telegram_id,first_name,created_at) VALUES (77,'Player',?)",
                (now,),
            )
            await db._execute(
                "INSERT INTO trainings (id,title,training_type,start_at,duration_min,capacity,"
                "reminder_enabled,reminder_offset_min,created_by,created_at) "
                "VALUES (1,'Squash <A&B>','Squash <A&B>',?,60,4,1,60,123,?)",
                (now + 1800, now),
            )
            await db._execute(
                "INSERT INTO bookings (training_id,user_id,status,position,created_at) "
                "VALUES (1,77,'booked',0,?)",
                (now,),
            )
            due = await db.trainings_due_for_reminder(now)
            for training in due:
                delivered, total = await bot_module.send_training_reminder(fake, training)
                if delivered == total:
                    await db.mark_reminder_sent(training["id"])
            trainings = await db._fetchall(
                "SELECT id,reminder_sent FROM trainings ORDER BY id"
            )
            deliveries = await db._fetchall(
                "SELECT training_id,user_id FROM reminder_deliveries ORDER BY training_id,user_id"
            )
            notifications = await db._fetchall(
                "SELECT kind,entity_id,chat_id,user_id,message_id,text,delete_at "
                "FROM event_notifications ORDER BY kind,entity_id,chat_id,user_id"
            )
            print(json.dumps({
                "calls": fake.calls,
                "trainings": [dict(row) for row in trainings],
                "deliveries": [dict(row) for row in deliveries],
                "notifications": [dict(row) for row in notifications],
            }, ensure_ascii=False))
        finally:
            await db.close()


asyncio.run(run())
