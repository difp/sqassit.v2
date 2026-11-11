"""Offline Python reference for the opt-in cross-implementation contract test."""
import asyncio
import hashlib
import hmac
import json
import os
import sys
import tempfile
from urllib.parse import urlencode

os.environ.update(
    BOT_TOKEN="123456:TEST-TOKEN", COACH_TELEGRAM_ID="123", BOT_MODE="polling",
    TIMEZONE="Europe/Moscow", REQUIRE_EVENT_APPROVAL="false", PHONE_REQUIRED_FROM="",
    BOT_PROXY="",
)
sys.path.insert(0, os.path.abspath("legacy"))
import httpx2
from app import database as db
from app import main


def signed(user, now):
    fields = {"auth_date": str(now), "user": json.dumps(user, ensure_ascii=True, separators=(",", ":"))}
    check = "\n".join(f"{k}={fields[k]}" for k in sorted(fields))
    secret = hmac.new(b"WebAppData", os.environ["BOT_TOKEN"].encode(), hashlib.sha256).digest()
    fields["hash"] = hmac.new(secret, check.encode(), hashlib.sha256).hexdigest()
    return urlencode(fields)


def discard(coro):
    coro.close()


async def run():
    now, date = int(sys.argv[1]), sys.argv[2]
    main._spawn = discard
    with tempfile.TemporaryDirectory(prefix="sqassit-contract-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            # Seed the owner coach in a previous hour so registry ordering does not
            # depend on whether the reference and candidate cross a second boundary.
            await db._execute(
                "INSERT OR IGNORE INTO coaches (telegram_id, created_at) VALUES (?, ?)",
                (123, now - 3600),
            )
            await db.upsert_user(telegram_id=123, username=None, first_name="Тренер", is_coach=True)
            headers = {
                "coach": {"X-Init-Data": signed({"id":123,"first_name":"Coach"}, now)},
                "player": {"X-Init-Data": signed({"id":42,"first_name":"Player"}, now)},
                "player2": {"X-Init-Data": signed({"id":43,"first_name":"Second"}, now)},
                "applicant": {"X-Init-Data": signed({"id":44,"first_name":"Applicant"}, now)},
                "none": {},
            }
            scenarios = [
                ("GET", "/api/me", "none", None),
                ("GET", "/api/me", "player", None),
                ("PUT", "/api/me/profile", "player", {"about":"  Люблю сквош  ","display_mode":"animal","show_username":False}),
                ("PUT", "/api/me/notifications", "player", {"notify_matches":False}),
                ("POST", "/api/me/onboarding", "player", None),
                ("GET", "/api/me", "player", None),
                ("GET", "/api/levels", "player", None),
                ("GET", "/api/coach/players", "player", None),
                ("POST", "/api/trainings/999/book", "player", None),
                ("POST", "/api/trainings/999/cancel", "player", None),
                ("POST", "/api/matches/999/join", "player", None),
                ("POST", "/api/coach/trainings", "coach", {"date":date,"time":"12:00","training_type":"Squash","capacity":1}),
                ("GET", "/api/trainings?days=7", "player", None),
                ("POST", "/api/trainings/1/room", "player", None),
                ("POST", "/api/trainings/1/cancel", "player", None),
                ("POST", "/api/trainings/1/book", "player", None),
                ("GET", "/api/trainings?days=7", "player", None),
                ("POST", "/api/trainings/1/book", "player2", None),
                ("POST", "/api/trainings/1/cancel", "player", None),
                ("GET", "/api/trainings?days=7", "player2", None),
                ("POST", "/api/trainings/1/cancel", "player2", None),
                ("POST", "/api/coach/trainings/1/participants", "coach", {"user_id":42}),
                ("POST", "/api/coach/trainings/1/participants", "coach", {"user_id":43}),
                ("GET", "/api/coach/trainings/1/participants", "coach", None),
                ("POST", "/api/coach/trainings/1/participants/42/cancel", "coach", None),
                ("POST", "/api/coach/trainings/1/participants/42/restore", "coach", None),
                ("PATCH", "/api/coach/trainings/1/reminder", "coach", {"enabled":True,"offset_min":60}),
                ("PATCH", "/api/coach/trainings/1", "coach", {"date":date,"time":"14:00","training_type":"Updated"}),
                ("DELETE", "/api/coach/trainings/1", "coach", None),
                ("POST", "/api/coach/trainings/1/restore", "coach", None),
                ("GET", "/api/coach/trainings/1/participants", "coach", None),
                ("POST", "/api/matches", "player", {"date":date,"time":"13:00","title":"Match"}),
                ("POST", "/api/matches/1/room", "player2", None),
                ("DELETE", "/api/matches/1", "player2", None),
                ("POST", "/api/matches/1/join", "player2", None),
                ("GET", "/api/matches", "player2", None),
                ("POST", "/api/matches/1/leave", "player2", None),
                ("DELETE", "/api/matches/1", "player", None),
                ("POST", "/api/matches/1/join", "player2", None),
                ("POST", "/api/messages/coach", "player", {"text":"  Привет тренер  ","training_id":1}),
                ("GET", "/api/coach/messages", "coach", None),
                ("GET", "/api/coach/messages/42", "coach", None),
                ("POST", "/api/coach/messages/42", "coach", {"text":"Здравствуйте"}),
                ("GET", "/api/messages/thread", "player", None),
                ("GET", "/api/messages/thread", "player2", None),
                ("GET", "/api/coach/messages/42", "player2", None),
                ("GET", "/api/coach/players", "coach", None),
                ("PUT", "/api/coach/players/42/notes", "coach", {}),
                # Coach registry, club view, applications, coauthors and transfer.
                ("GET", "/api/coach/coaches", "coach", None),
                ("GET", "/api/coach/club/trainings", "coach", None),
                ("POST", "/api/coach/apply", "player2", {"message":"Хочу тренировать"}),
                ("GET", "/api/coach/apply", "player2", None),
                ("GET", "/api/coach/applications", "coach", None),
                ("POST", "/api/coach/applications/43/approve", "coach", None),
                ("GET", "/api/coach/apply", "player2", None),
                ("GET", "/api/coach/coaches", "player2", None),
                ("PATCH", "/api/coach/coaches/43/manager", "coach", {"enabled":True}),
                ("GET", "/api/coach/coaches", "coach", None),
                ("DELETE", "/api/coach/coaches/123", "coach", None),
                ("DELETE", "/api/coach/coaches/43", "coach", None),
                ("GET", "/api/coach/coaches", "coach", None),
                ("POST", "/api/coach/coaches", "coach", {"telegram_id":43}),
                ("GET", "/api/coach/coaches", "player2", None),
                ("GET", "/api/coach/trainings/1/coauthors", "coach", None),
                ("POST", "/api/coach/trainings/1/coauthors", "coach", {"telegram_id":43}),
                ("GET", "/api/coach/trainings/1/coauthors", "coach", None),
                ("DELETE", "/api/coach/trainings/1/coauthors/43", "coach", None),
                ("GET", "/api/coach/trainings/1/coauthors", "coach", None),
                ("POST", "/api/coach/trainings/1/transfer", "coach", {"telegram_id":43}),
                ("GET", "/api/coach/trainings", "player2", None),
                ("POST", "/api/coach/trainings/1/transfer", "player2", {"telegram_id":123}),
                ("GET", "/api/coach/trainings", "coach", None),
                ("POST", "/api/matches", "player", {"date":date,"time":"15:00","title":"Second match"}),
                ("POST", "/api/matches/2/coauthors", "player", {"telegram_id":43}),
                ("GET", "/api/matches/2/coauthors", "player2", None),
                ("DELETE", "/api/matches/2/coauthors/43", "player", None),
                ("DELETE", "/api/matches/2", "player", None),
                ("POST", "/api/coach/apply", "applicant", {"message":"Ещё заявка"}),
                ("GET", "/api/coach/applications", "coach", None),
                ("POST", "/api/coach/applications/44/reject", "coach", None),
                ("GET", "/api/coach/apply", "applicant", None),
                ("POST", "/api/coach/applications/44/approve", "coach", None),
                ("DELETE", "/api/coach/coaches/44", "coach", None),
                ("GET", "/api/coach/coaches", "player", None),
                ("GET", "/api/coach/players/count", "coach", None),
                # Feedback, export and confirmed account deletion.
                ("POST", "/api/feedback", "player", {"kind":"idea","text":"  Идея  ","page":"Матч"}),
                ("GET", "/api/me/export", "player", None),
                ("POST", "/api/feedback", "player", {"kind":"bug","text":"ok","page":"unknown"}),
                ("DELETE", "/api/me", "applicant", None),
                ("DELETE", "/api/me?confirm=true", "applicant", None),
            ]
            async with httpx2.AsyncClient(transport=httpx2.ASGITransport(app=main.app), base_url="http://reference") as client:
                result = []
                for method, path, actor, body in scenarios:
                    response = await client.request(method, path, headers=headers[actor], **({"json":body} if body is not None else {}))
                    result.append({"status":response.status_code,"body":response.json()})
            queries = {
                "users": "SELECT telegram_id,first_name,is_coach,about,display_mode,show_username,phone,notifications_enabled,notify_matches,notify_trainings,notify_messages,onboarding_seen FROM users ORDER BY telegram_id",
                "trainings": "SELECT id,title,training_type,start_at,capacity,reminder_enabled,reminder_offset_min,reminder_sent,created_by,deleted_at FROM trainings ORDER BY id",
                "bookings": "SELECT training_id,user_id,status,position FROM bookings ORDER BY training_id,user_id",
                "matches": "SELECT id,title,description,location,start_at,duration_min,max_players,level,created_by,deleted_at FROM matches ORDER BY id",
                "match_players": "SELECT match_id,user_id,status FROM match_players ORDER BY match_id,user_id",
                "messages": "SELECT id,from_user,to_user,text FROM messages ORDER BY id",
                "coaches": "SELECT telegram_id,added_by,is_manager FROM coaches ORDER BY telegram_id",
                "coach_applications": "SELECT telegram_id,message,status,decided_by FROM coach_applications ORDER BY telegram_id",
                "event_coauthors": "SELECT event_type,event_id,user_id,added_by FROM event_coauthors ORDER BY event_type,event_id,user_id",
                "feedback": "SELECT user_id,kind,text,page FROM feedback ORDER BY id",
            }
            state = {key: [dict(row) for row in await db._fetchall(query)] for key, query in queries.items()}
            print(json.dumps({"responses":result,"state":state}, ensure_ascii=False))
        finally:
            await db.close()

asyncio.run(run())
