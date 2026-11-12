"""Offline Python reference for the opt-in expired-notification cleanup test."""
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
    """Deletes messages, failing transiently for one configured message ID."""

    def __init__(self):
        self.calls = []
        self.fail_for = None

    async def delete_message(self, chat_id, message_id):
        self.calls.append({
            "method": "deleteMessage",
            "params": {"chat_id": chat_id, "message_id": message_id},
        })
        if self.fail_for == message_id:
            raise RuntimeError("timed out")

    async def answer_callback_query(self, *args, **kwargs):
        raise AssertionError("cleanup must not answer callbacks")

    async def send_message(self, *args, **kwargs):
        raise AssertionError("no final enrollment is due in this fixture")


ROWS = [
    # kind, entity, chat, user, message, text, delete_at offset, phase, final_at offset
    # Offsets are distinct and deliberately out of row order, so both sides
    # must sort by delete_at to produce the same call sequence.
    ("training", 1, 77, 77, 101, "card", -5, "", None),
    ("chat", 5, 77, 77, 102, "chat", -9, "", None),
    ("match", 2, 77, 77, 103, "match", -1, "", None),
    ("match_feed", 1, 77, 77, 104, "feed", -7, "", None),
    ("coach_application", 44, 123, 123, 105, "application", -3, "", None),
    ("notice", 0, 77, 77, 106, "notice", -8, "", None),
    ("training_reminder", 1, 77, 77, 107, "reminder", -2, "", None),
    ("training_booked", 1, 77, 77, 108, "final booked", -10, "final", None),
    ("training_waitlist", 1, 88, 88, 0, "waitlist without message", -6, "status", None),
    ("training_rejected", 1, 99, 99, 110, "rejected", -4, "status", None),
    # Preliminary confirmation whose final message is not due yet: the message
    # is deleted, but the row is kept so the final confirmation can arrive.
    ("training_booked", 2, 77, 77, 111, "preliminary", -11, "preliminary", 3600),
    ("training", 3, 77, 77, 112, "future card", 1000, "", None),
    ("chat", 6, 77, 77, 113, "no deadline", None, "", None),
    # Deletion fails transiently on the first tick and succeeds on the second.
    ("notice", 9, 88, 88, 114, "transient", -12, "", None),
]


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
    with tempfile.TemporaryDirectory(prefix="sqassit-cleanup-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            for kind, entity, chat, user, message, text, delete_offset, phase, final_offset in ROWS:
                await db._execute(
                    "INSERT INTO event_notifications(kind,entity_id,chat_id,user_id,message_id,"
                    "text,updated_at,delete_at,phase,sent_at,final_at) "
                    "VALUES (?,?,?,?,?,?,?,?,?,?,?)",
                    (
                        kind, entity, chat, user, message, text, now,
                        None if delete_offset is None else now + delete_offset,
                        phase, now,
                        None if final_offset is None else now + final_offset,
                    ),
                )
            fake.fail_for = 114
            # The full notifications loop order: deletions, final enrollments,
            # then stale preliminary cleanup.
            await main._process_due_deletions(now)
            await main._process_due_final_enrollments(now)
            await main._cleanup_stale_preliminary(now)
            first_calls = list(fake.calls)
            first_rows = await snapshot()
            fake.calls.clear()
            fake.fail_for = None
            await main._process_due_deletions(now)
            await main._process_due_final_enrollments(now)
            await main._cleanup_stale_preliminary(now)
            second_calls = list(fake.calls)
            second_rows = await snapshot()
            print(json.dumps({
                "first_calls": first_calls,
                "first_rows": first_rows,
                "second_calls": second_calls,
                "second_rows": second_rows,
            }, ensure_ascii=False))
        finally:
            await db.close()


asyncio.run(run())
