"""Offline Python reference for coach participant-action notifications."""
import asyncio
import json
import os
import sys
import tempfile
from types import SimpleNamespace

os.environ.update(
    BOT_TOKEN="123456:TEST-TOKEN", COACH_TELEGRAM_ID="1", BOT_MODE="polling",
    TIMEZONE="Europe/Moscow", REQUIRE_EVENT_APPROVAL="true", PHONE_REQUIRED_FROM="",
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
    with tempfile.TemporaryDirectory(prefix="sqassit-coach-actions-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            await db._execute(
                "INSERT INTO users(telegram_id,first_name,notifications_enabled,notify_trainings,"
                "created_at) VALUES (1,'Coach',1,1,?),(2,'Approved',1,1,?),(3,'Removed',1,1,?),"
                "(4,'Waiter',1,1,?),(5,'Rejected',1,1,?),(6,'Coauthor',1,1,?)",
                (now, now, now, now, now, now),
            )
            await db._execute(
                "INSERT INTO trainings(id,title,training_type,start_at,duration_min,capacity,"
                "level,created_by,created_at) VALUES "
                "(7,'Actions <A&B>','Squash <A&B>',?,60,2,'Beginner',1,?)",
                (now + 7200, now),
            )
            await db._execute(
                "INSERT INTO bookings(training_id,user_id,status,position,created_at) VALUES "
                "(7,2,'pending',0,?),(7,3,'booked',0,?),(7,4,'waitlist',1,?),(7,5,'pending',2,?)",
                (now, now, now, now),
            )
            await db._execute(
                "INSERT INTO event_coauthors(event_type,event_id,user_id,added_by,created_at) "
                "VALUES ('training',7,6,1,?)",
                (now,),
            )
            await db._execute(
                "INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,"
                "text,updated_at,delete_at,phase,sent_at) VALUES "
                "('training_booked',7,3,3,301,'old booked',?,?, 'final',?),"
                "('training',7,3,3,302,'old card',?,?, '',?)",
                (now, now + 5000, now, now, now + 5000, now),
            )

            training = await db.get_training(7)
            status = await db.approve_booking(7, 2)
            await main._notify_training_event(training, 2, status, actor_id=1)

            assert await db.reject_booking(7, 5)
            await main._notify_training_event(training, 5, "rejected", actor_id=1)

            promoted = await db.cancel_booking(7, 3)
            await main._notify_participant_removed(3, training, actor_id=1)
            await main._notify_promoted(int(promoted), training)

            status = await db.restore_booking(7, 3, 2)
            await main._notify_participant_restored(3, training, status, actor_id=1)

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
