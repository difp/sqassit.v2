"""Offline Python reference for the opt-in training edit/restore notification test."""
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
    with tempfile.TemporaryDirectory(prefix="sqassit-training-edit-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            await db._execute(
                "INSERT INTO users(telegram_id,first_name,notifications_enabled,notify_trainings,"
                "created_at) VALUES (1,'Coach',1,1,?),(2,'Booked',1,1,?),(3,'Waiting',1,1,?),"
                "(4,'Pending',1,1,?),(5,'Quiet',0,0,?),(6,'Coauthor',1,1,?),(7,'Restored',1,1,?)",
                (now, now, now, now, now, now, now),
            )
            await db._execute(
                "INSERT INTO trainings(id,title,training_type,start_at,duration_min,capacity,"
                "level,created_by,created_at,deleted_at) "
                "VALUES (7,'Edit <A&B>','Squash <A&B>',?,60,4,'Beginner',1,?,NULL),"
                "(8,'Restore','Squash',?,60,4,'Beginner',1,?,?)",
                (now + 7200, now, now + 8000, now, now),
            )
            await db._execute(
                "INSERT INTO bookings(training_id,user_id,status,position,created_at) VALUES "
                "(7,2,'booked',0,?),(7,3,'waitlist',1,?),(7,4,'pending',2,?),"
                "(7,5,'booked',3,?),(7,6,'booked',4,?),(8,7,'booked',0,?)",
                (now, now, now, now, now, now),
            )
            await db._execute(
                "INSERT INTO event_coauthors(event_type,event_id,user_id,added_by,created_at) "
                "VALUES ('training',7,6,1,?)",
                (now,),
            )
            for kind, entity, chat, user, message, text, phase in (
                ("training", 7, 1, 0, 201, "old organizer", ""),
                ("training", 7, 2, 2, 202, "old player", ""),
                ("training", 7, 3, 3, 203, "old waitlist card", ""),
                ("training", 7, 4, 4, 204, "old pending card", ""),
                ("training_booked", 7, 2, 2, 205, "old booked", "final"),
                ("training_waitlist", 7, 3, 3, 206, "old waitlist", "status"),
                ("training_reminder", 7, 2, 2, 207, "old reminder", ""),
                ("training", 8, 1, 0, 208, "deleted organizer", ""),
                ("training", 8, 7, 7, 209, "deleted player", ""),
            ):
                await db._execute(
                    "INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,"
                    "text,updated_at,delete_at,phase,sent_at) VALUES (?,?,?,?,?,?,?,?,?,?)",
                    (kind, entity, chat, user, message, text, now, now + 5000, phase, now),
                )

            # A title-only edit keeps status messages in place.
            await db.update_training_details(7, "Squash <B>", now + 7200)
            updated = await db.get_training(7)
            await main._notify_training_changed(updated, False)

            # A moved training deletes old status messages and recreates them.
            await db.update_training_details(7, "Squash <B>", now + 10800)
            updated = await db.get_training(7)
            await main._notify_training_changed(updated, True)

            # Restoring edits the old cards back to the active state.
            deleted = await db.get_training(8)
            await db.restore_training(8)
            await main._notify_training_restored(deleted)

            notifications = await db._fetchall(
                "SELECT kind,entity_id,chat_id,user_id,message_id,text "
                "FROM event_notifications ORDER BY kind,entity_id,chat_id,user_id"
            )
            print(json.dumps({
                "calls": fake.calls,
                "notifications": [dict(row) for row in notifications],
                "seed_ids": [201, 202, 203, 204, 205, 206, 207, 208, 209],
            }, ensure_ascii=False))
        finally:
            await db.close()


asyncio.run(run())
