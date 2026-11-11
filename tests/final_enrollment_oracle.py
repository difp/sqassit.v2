"""Offline Python reference for the opt-in preliminary-to-final enrollment test."""
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
from app import database as db
from app import main


class FakeBot:
    def __init__(self):
        self.calls = []
        self.next_id = 100

    async def send_message(self, chat_id, text, **kwargs):
        self.next_id += 1
        params = {"chat_id": chat_id, "text": text}
        params.update({key: value for key, value in kwargs.items() if value is not None})
        params.pop("parse_mode", None)
        self.calls.append({"method": "sendMessage", "params": params})
        from types import SimpleNamespace
        return SimpleNamespace(message_id=self.next_id)

    async def delete_message(self, chat_id, message_id):
        self.calls.append({
            "method": "deleteMessage",
            "params": {"chat_id": chat_id, "message_id": message_id},
        })


async def snapshot():
    rows = await db._fetchall(
        "SELECT id,kind,entity_id,chat_id,user_id,message_id,text,delete_at,phase,final_at "
        "FROM event_notifications ORDER BY id"
    )
    return [dict(row) for row in rows]


async def run():
    now = int(sys.argv[1])
    fake = FakeBot()
    main.app.state.bot = fake
    with tempfile.TemporaryDirectory(prefix="sqassit-final-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            await db._execute(
                "INSERT INTO users(telegram_id,first_name,notifications_enabled,notify_trainings,created_at) "
                "VALUES (123,'Тренер',1,1,?),(77,'Player',1,1,?),(88,'Quiet',0,0,?)",
                (now, now, now),
            )
            await db.ensure_coach(123)
            await db._execute(
                "INSERT INTO trainings(id,title,training_type,start_at,duration_min,capacity,created_by,created_at) "
                "VALUES (2,'Squash','Squash',?,60,4,123,?)",
                (now + 3600, now),
            )
            await db._execute(
                "INSERT INTO bookings(training_id,user_id,status,position,created_at) "
                "VALUES (2,77,'booked',0,?),(2,88,'booked',1,?)",
                (now, now),
            )
            for user, message in ((77, 111), (88, 112)):
                await db._execute(
                    "INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,"
                    "text,updated_at,delete_at,phase,sent_at,final_at) "
                    "VALUES ('training_booked',2,?,?,?,'preliminary',?,?,'preliminary',?,?)",
                    (user, user, message, now, now + 1000, now, now - 10),
                )
            await main._process_due_deletions(now)
            await main._process_due_final_enrollments(now)
            print(json.dumps({
                "calls": sorted(fake.calls, key=lambda call: json.dumps(call, sort_keys=True)),
                "rows": await snapshot(),
            }, ensure_ascii=False))
        finally:
            await db.close()


asyncio.run(run())
