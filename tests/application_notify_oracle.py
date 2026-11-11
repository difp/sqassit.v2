"""Offline Python reference for the opt-in coach-application notification test."""
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
from app import identity
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
    with tempfile.TemporaryDirectory(prefix="sqassit-application-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            await db._execute(
                "INSERT INTO users(telegram_id,first_name,display_mode,notifications_enabled,"
                "notify_messages,created_at) VALUES (1,'Owner','name',1,1,?),"
                "(2,'Secret Name','animal',1,1,?),(3,'Manager','name',1,1,?),"
                "(4,'Quiet Manager','name',0,0,?)",
                (now, now, now, now),
            )
            await db._execute(
                "INSERT INTO coaches(telegram_id,is_manager,created_at) VALUES "
                "(1,1,?),(3,1,?),(4,1,?)",
                (now, now, now),
            )
            await db._execute(
                "INSERT INTO coach_applications(telegram_id,message,status,created_at) "
                "VALUES (2,'Хочу тренировать','pending',?)",
                (now,),
            )
            applicant = await db.get_user(2)
            name = (
                identity.animal_identity(2)["name"]
                if applicant["display_mode"] == "animal"
                else (applicant["first_name"] or "Пользователь")
            )
            await main._notify_managers_about_application(2, name)
            notifications = await db._fetchall(
                "SELECT kind,entity_id,chat_id,user_id,message_id,text "
                "FROM event_notifications ORDER BY chat_id,user_id"
            )
            print(json.dumps({
                "calls": sorted(fake.calls, key=lambda call: call["params"]["chat_id"]),
                "notifications": [dict(row) for row in notifications],
            }, ensure_ascii=False))
        finally:
            await db.close()


asyncio.run(run())
