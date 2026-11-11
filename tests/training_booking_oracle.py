"""Offline Python reference for the opt-in training booking notification test."""
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
    with tempfile.TemporaryDirectory(prefix="sqassit-training-booking-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            await db._execute(
                "INSERT INTO users(telegram_id,first_name,notifications_enabled,notify_trainings,"
                "created_at) VALUES (1,'Coach',1,1,?),(2,'Booked',1,1,?),(3,'Second',1,1,?),"
                "(4,'Wait',1,1,?),(5,'Quiet',0,0,?),(6,'Coauthor',1,1,?)",
                (now, now, now, now, now, now),
            )
            await db._execute(
                "INSERT INTO trainings(id,title,training_type,start_at,duration_min,capacity,"
                "level,created_by,created_at) VALUES "
                "(7,'Booking <A&B>','Squash <A&B>',?,60,2,'Beginner',1,?)",
                (now + 7200, now),
            )
            await db._execute(
                "INSERT INTO event_coauthors(event_type,event_id,user_id,added_by,created_at) "
                "VALUES ('training',7,6,1,?)",
                (now,),
            )
            training = await db.get_training(7)

            await db._execute(
                "INSERT INTO bookings(training_id,user_id,status,position,created_at) "
                "VALUES (7,2,'booked',0,?)",
                (now,),
            )
            await main._notify_training_event(training, 2, "booked", actor_id=2)
            training = await db.get_training(7)

            await db._execute(
                "INSERT INTO bookings(training_id,user_id,status,position,created_at) "
                "VALUES (7,3,'booked',0,?)",
                (now + 1,),
            )
            await main._notify_training_event(training, 3, "booked", actor_id=3)
            training = await db.get_training(7)

            await db._execute(
                "INSERT INTO bookings(training_id,user_id,status,position,created_at) "
                "VALUES (7,4,'waitlist',1,?)",
                (now + 2,),
            )
            await main._notify_training_event(training, 4, "waitlist", actor_id=4)
            training = await db.get_training(7)

            await db._execute(
                "INSERT INTO bookings(training_id,user_id,status,position,created_at) "
                "VALUES (7,5,'waitlist',2,?)",
                (now + 3,),
            )
            await main._notify_training_event(training, 5, "waitlist", actor_id=5)
            training = await db.get_training(7)

            await db._execute(
                "UPDATE bookings SET status='cancelled', position=0 "
                "WHERE training_id=7 AND user_id=3"
            )
            await main._notify_training_event(training, 3, "cancelled", actor_id=3)
            await db._execute(
                "UPDATE bookings SET status='booked', position=0 "
                "WHERE training_id=7 AND user_id=4"
            )
            training = await db.get_training(7)
            await main._notify_training_event(training, 4, "promoted")

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
