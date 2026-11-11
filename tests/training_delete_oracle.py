"""Offline Python reference for the opt-in training-deletion notification test."""
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
        self.next_id = 300

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

    async def delete_message(self, chat_id, message_id):
        self.calls.append({
            "method": "deleteMessage",
            "params": {"chat_id": chat_id, "message_id": message_id},
        })


async def run():
    now = int(sys.argv[1])
    fake = FakeBot()
    main.app.state.bot = fake
    with tempfile.TemporaryDirectory(prefix="sqassit-training-delete-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            await db._execute(
                "INSERT INTO users(telegram_id,first_name,notifications_enabled,notify_trainings,"
                "created_at) VALUES (1,'Coach',1,1,?),(2,'Player Two',1,1,?),(3,'Waiting',1,1,?),"
                "(4,'Quiet',0,0,?),(5,'Rejected',1,1,?),(6,'Pending',1,1,?)",
                (now, now, now, now, now, now),
            )
            await db._execute(
                "INSERT INTO trainings(id,title,training_type,start_at,duration_min,capacity,"
                "level,created_by,created_at,deleted_at) "
                "VALUES (7,'Squash <A&B>','Squash <A&B>',?,60,4,'Beginner',1,?,?)",
                (now + 7200, now, now),
            )
            await db._execute(
                "INSERT INTO bookings(training_id,user_id,status,position,created_at) VALUES "
                "(7,2,'booked',0,?),(7,3,'waitlist',1,?),(7,4,'booked',2,?),"
                "(7,5,'rejected',3,?),(7,6,'pending',4,?)",
                (now, now, now, now, now),
            )
            for kind, chat, message, text in (
                ("training_booked", 2, 201, "old booked"),
                ("training_waitlist", 3, 202, "old waitlist"),
                ("training_rejected", 5, 203, "old rejected"),
                ("training_reminder", 2, 204, "old reminder"),
                ("training", 1, 205, "old card"),
            ):
                await db._execute(
                    "INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,"
                    "text,updated_at,delete_at,phase,sent_at) VALUES (?,7,?,?,?,?,?,?,?,?)",
                    (
                        kind, chat, chat if kind != "training" else 0, message, text, now,
                        now + 1000 if kind == "training" else now + 500,
                        "final" if kind == "training_booked" else "", now,
                    ),
                )
            training = await db.get_training(7)
            await main._notify_training_deleted(training)
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
