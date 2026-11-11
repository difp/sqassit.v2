"""Capture the Python schema and backfills in an isolated disposable database."""
import asyncio
import json
import os
import sqlite3
import sys
import tempfile

os.environ.update(BOT_TOKEN="123456:TEST-TOKEN", COACH_TELEGRAM_ID="123", BOT_MODE="polling", TIMEZONE="Europe/Moscow")
sys.path.insert(0, os.path.abspath("legacy"))
from app import database as db

async def main():
    with tempfile.TemporaryDirectory(prefix="sqassit-schema-") as directory:
        path = os.path.join(directory, "test.sqlite")
        if sys.argv[1] == "old":
            with sqlite3.connect(path) as connection:
                connection.executescript(open("src/schema.sql", encoding="utf-8").read())
                connection.executescript(open("tests/old_schema_fixture.sql", encoding="utf-8").read())
        for _ in range(2):
            await db.connect(path)
            await db.close()
        with sqlite3.connect(path) as connection:
            connection.row_factory = sqlite3.Row
            tables = [r[0] for r in connection.execute("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' ORDER BY name")]
            result = {}
            for table in tables:
                columns = [dict(row) for row in connection.execute(f'PRAGMA table_info("{table}")')]
                for column in columns:
                    column.pop("cid")
                indexes = [dict(row) for row in connection.execute(f'PRAGMA index_list("{table}")')]
                for index in indexes:
                    index.pop("seq")
                    index["columns"] = [dict(row)["name"] for row in connection.execute(f'PRAGMA index_info("{index["name"]}")')]
                result[table] = {"columns": sorted(columns, key=lambda x: x["name"]), "indexes": sorted(indexes, key=lambda x: x["name"])}
            rows = {}
            for table in ("users", "trainings", "bookings", "event_notifications"):
                rows[table] = [dict(row) for row in connection.execute(f'SELECT * FROM "{table}" ORDER BY rowid')]
            print(json.dumps({"schema":result,"rows":rows}, ensure_ascii=False))

asyncio.run(main())
