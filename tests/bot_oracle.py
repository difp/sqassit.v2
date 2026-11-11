"""Offline Python reference for the opt-in Telegram bot contract test."""
import asyncio
import json
import os
import sys
import tempfile
import time
from types import SimpleNamespace

os.environ.update(
    BOT_TOKEN="123456:TEST-TOKEN", COACH_TELEGRAM_ID="123", BOT_MODE="polling",
    TIMEZONE="Europe/Moscow", REQUIRE_EVENT_APPROVAL="false", PHONE_REQUIRED_FROM="",
    BOT_PROXY="",
)
sys.path.insert(0, os.path.abspath("legacy"))
from aiogram.types import Update
from app import bot as bot_module
from app import database as db
from app import main

# aiogram keeps bot defaults (HTML parse mode, link preview settings) in
# sentinel objects that cannot be serialized; the candidate always sends HTML.
DEFAULT_FIELDS = {
    "parse_mode", "link_preview_options", "protect_content", "disable_web_page_preview",
}


def dump(value):
    if hasattr(value, "model_dump"):
        return value.model_dump(mode="json", exclude_none=True)
    return value


class FakeBot:
    """Records outgoing API calls and returns minimal Telegram objects."""

    def __init__(self):
        self.calls = []
        self.next_id = 100

    def _record(self, method, params):
        self.calls.append({"method": method, "params": params})

    async def __call__(self, method):
        name = method.__api_method__
        params = method.model_dump(
            mode="json", exclude_none=True, exclude=DEFAULT_FIELDS
        )
        self._record(name, params)
        if name == "sendMessage":
            self.next_id += 1
            return SimpleNamespace(message_id=self.next_id)
        return True

    async def send_message(self, chat_id, text, **kwargs):
        params = {"chat_id": chat_id, "text": text}
        params.update({key: dump(value) for key, value in kwargs.items()})
        params.pop("parse_mode", None)
        self._record("sendMessage", params)
        self.next_id += 1
        return SimpleNamespace(message_id=self.next_id)

    async def edit_message_text(self, **kwargs):
        params = {key: dump(value) for key, value in kwargs.items()}
        params.pop("parse_mode", None)
        self._record("editMessageText", params)
        return True

    async def pin_chat_message(self, **kwargs):
        self._record("pinChatMessage", kwargs)
        return True

    async def delete_message(self, **kwargs):
        self._record("deleteMessage", kwargs)
        return True

    async def answer_callback_query(self, callback_query_id, text=None, show_alert=None):
        params = {"callback_query_id": callback_query_id}
        if text is not None:
            params["text"] = text
        if show_alert is not None:
            params["show_alert"] = show_alert
        self._record("answerCallbackQuery", params)
        return True

    async def get_user_profile_photos(self, user_id, limit=None):
        self._record("getUserProfilePhotos", {"user_id": user_id, "limit": limit})
        return SimpleNamespace(total_count=0, photos=[])


def message(uid, text=None, contact=None, chat=None):
    payload = {
        "message_id": 10,
        "date": int(time.time()),
        "chat": {"id": chat or uid, "type": "private"},
        "from": {"id": uid, "is_bot": False, "first_name": f"User{uid}"},
    }
    if text is not None:
        payload["text"] = text
    if contact is not None:
        payload["contact"] = contact
    return payload


def callback(uid, data, chat=None):
    return {
        "id": "cb-1",
        "from": {"id": uid, "is_bot": False, "first_name": f"User{uid}"},
        "message": {
            "message_id": 50,
            "date": int(time.time()),
            "chat": {"id": chat or uid, "type": "private"},
        },
        "chat_instance": "instance",
        "data": data,
    }


UPDATES = [
    message(77, "/start"),
    message(77, "/phone"),
    message(77, "hello"),
    message(77, "/slots"),
    message(77, "", contact={"phone_number": "+7 (999) 123-45-67", "first_name": "User77", "user_id": 77}),
    message(88, "", contact={"phone_number": "+7 (999) 000-00-00", "first_name": "Other", "user_id": 77}),
    message(77, "/start"),
    message(123, "/start"),
    message(77, "/slots"),
    {"callback_query": callback(77, "slots:show:1")},
    {"callback_query": callback(77, "slots:show:999")},
    {"callback_query": callback(77, "slots:list")},
    {"callback_query": callback(77, "attendees:list")},
    {"callback_query": callback(77, "attendees:show:1")},
    {"callback_query": callback(77, "attendees:show:999")},
    {"callback_query": callback(77, "apply:ok:45")},
    {"callback_query": callback(123, "apply:ok:44")},
    {"callback_query": callback(123, "apply:ok:44")},
    {"callback_query": callback(123, "apply:no:45")},
]


def normalize_calls(calls):
    for call in calls:
        if call["method"] != "answerCallbackQuery":
            continue
        params = call["params"]
        if params.get("text") in ("", None):
            params.pop("text", None)
        if params.get("show_alert") is False:
            params.pop("show_alert", None)
    return calls


async def run():
    now = int(sys.argv[1])
    main._spawn = lambda coro: coro.close()
    fake = FakeBot()
    with tempfile.TemporaryDirectory(prefix="sqassit-bot-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            await db.ensure_coach(123)
            await db.upsert_user(
                telegram_id=123, username=None, first_name="Тренер", is_coach=True,
            )
            await db.upsert_user(
                telegram_id=88, username=None, first_name="Other", is_coach=False,
            )
            await db._execute(
                "INSERT INTO trainings (id,title,training_type,start_at,duration_min,capacity,created_by,created_at) "
                "VALUES (1,'Squash','Squash',?,60,2,123,?)",
                (now + 10800, now),
            )
            await db._execute(
                "INSERT INTO bookings (training_id,user_id,status,position,created_at) "
                "VALUES (1,77,'booked',0,?), (1,88,'waitlist',1,?)",
                (now, now),
            )
            await db._execute(
                "INSERT INTO coach_applications (telegram_id,message,status,created_at) "
                "VALUES (44,'Хочу тренировать','pending',?), (45,'И я','pending',?)",
                (now, now),
            )
            for raw in UPDATES:
                if "callback_query" in raw:
                    query = Update.model_validate(
                        {"update_id": 1, "callback_query": raw["callback_query"]}
                    ).callback_query.as_(fake)
                    if query.message is not None:
                        object.__setattr__(query.message, "_bot", fake)
                    if query.data == "slots:list":
                        await bot_module.cb_slots(query.as_(fake))
                    elif query.data.startswith("slots:show:"):
                        await bot_module.cb_slot_show(query.as_(fake))
                    elif query.data == "attendees:list":
                        await bot_module.cb_attendees(query.as_(fake))
                    elif query.data.startswith("attendees:show:"):
                        await bot_module.cb_attendees_show(query.as_(fake))
                    elif query.data.startswith("apply:"):
                        await bot_module.cb_apply(query.as_(fake))
                    continue
                update = Update.model_validate({"update_id": 1, "message": raw})
                msg = update.message
                if msg.contact is not None:
                    await bot_module.handle_contact(msg.as_(fake))
                elif msg.text == "/start":
                    await bot_module.cmd_start(msg.as_(fake))
                elif msg.text == "/phone":
                    await bot_module.cmd_phone(msg.as_(fake))
                elif msg.text == "/slots":
                    await bot_module.cmd_slots(msg.as_(fake))
                elif msg.text is not None:
                    await bot_module.fallback(msg.as_(fake))
            users = await db._fetchall(
                "SELECT telegram_id,first_name,phone,is_coach FROM users ORDER BY telegram_id"
            )
            notifications = await db._fetchall(
                "SELECT kind,entity_id,chat_id,user_id,message_id,text "
                "FROM event_notifications ORDER BY kind,entity_id,chat_id,user_id"
            )
            coaches = await db._fetchall(
                "SELECT telegram_id,added_by,is_manager FROM coaches ORDER BY telegram_id"
            )
            applications = await db._fetchall(
                "SELECT telegram_id,message,status,decided_by "
                "FROM coach_applications ORDER BY telegram_id"
            )
            print(json.dumps({
                "calls": normalize_calls(fake.calls),
                "users": [dict(row) for row in users],
                "notifications": [dict(row) for row in notifications],
                "coaches": [dict(row) for row in coaches],
                "applications": [dict(row) for row in applications],
            }, ensure_ascii=False))
        finally:
            await db.close()


asyncio.run(run())
