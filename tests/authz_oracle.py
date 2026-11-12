"""Offline Python reference for the route/role authorization matrix.

Reads the reference routes from the Python application itself and requests
each one without initData (role "none") and as a player, a non-manager coach
and the manager owner, each against a fresh database. The Rust test uses the
same request list, so a route missing from the Rust router or one that
authorizes a role differently shows up as a status difference.
"""
import asyncio
import json
import os
import re
import sys
import tempfile

os.environ.update(
    BOT_TOKEN="123456:TEST-TOKEN", COACH_TELEGRAM_ID="123", BOT_MODE="polling",
    TIMEZONE="Europe/Moscow", REQUIRE_EVENT_APPROVAL="false", PHONE_REQUIRED_FROM="",
    BOT_PROXY="", WEBHOOK_BASE_URL="",
)
sys.path.insert(0, os.path.abspath("legacy"))
import httpx2
from app import database as db
from app import main

ROUTE = re.compile(r'@app\.(get|post|put|patch|delete)\("([^"]+)"')

ROLES = [
    ("none", 0),
    ("player", 42),
    ("coach", 43),
    ("manager", 123),
]


def sample(schema, components):
    """Build a minimal valid request body from the OpenAPI schema."""
    if "$ref" in schema:
        name = schema["$ref"].rsplit("/", 1)[-1]
        return sample(components[name], components)
    for key in ("anyOf", "oneOf"):
        if key in schema:
            options = [item for item in schema[key] if item.get("type") != "null"]
            return sample(options[0], components) if options else None
    if schema.get("type") == "object" or "properties" in schema:
        return {
            name: sample(prop, components)
            for name, prop in schema.get("properties", {}).items()
            if name in schema.get("required", [])
        }
    if "enum" in schema:
        return schema["enum"][0]
    if schema.get("type") == "array":
        return [sample(schema.get("items", {}), components)] * max(schema.get("minItems", 0), 1)
    if schema.get("type") in ("integer", "number"):
        return max(int(schema.get("minimum", 1)), 1)
    if schema.get("type") == "boolean":
        return True
    if "date" in (schema.get("title", "") or "").lower() or "date" in schema.get("pattern", ""):
        return "2030-01-01"
    if schema.get("pattern") == "^\\d{2}:\\d{2}$":
        return "10:00"
    if schema.get("type") == "string":
        return "x" * max(schema.get("minLength", 1), 1)
    return None


def body_for(route, schema):
    if route["method"] == "GET":
        return None
    path = schema["paths"].get(route["raw"], {}).get(route["method"].lower(), {})
    body = path.get("requestBody", {}).get("content", {}).get("application/json", {})
    if body:
        return sample(body.get("schema", {}), schema["components"]["schemas"])
    return {}


def routes():
    source = open("legacy/app/main.py", encoding="utf-8").read()
    result = []
    for method, path in ROUTE.findall(source):
        filled = re.sub(
            r"\{[^}]+\}",
            lambda match: "bad-token" if "token" in match.group(0) else "999999",
            path,
        )
        result.append({"method": method.upper(), "path": filled, "raw": path})
    return result


def init_data(role_id, now):
    if not role_id:
        return {}
    import hashlib
    import hmac
    from urllib.parse import urlencode

    user = json.dumps({"id": role_id, "first_name": "User"}, ensure_ascii=True, separators=(",", ":"))
    fields = {"auth_date": str(now), "user": user}
    check = "\n".join(f"{key}={fields[key]}" for key in sorted(fields))
    secret = hmac.new(b"WebAppData", os.environ["BOT_TOKEN"].encode(), hashlib.sha256).digest()
    fields["hash"] = hmac.new(secret, check.encode(), hashlib.sha256).hexdigest()
    return {"X-Init-Data": urlencode(fields)}


async def run_pass(role, role_id, now, routes_list, schema):
    with tempfile.TemporaryDirectory(prefix="sqassit-authz-") as directory:
        await db.connect(os.path.join(directory, "reference.sqlite"))
        try:
            # Same seed as the Rust test: coach rows precede their users.
            await db._execute(
                "INSERT INTO coaches (telegram_id, created_at) VALUES (43, 0), (123, 0)"
            )
            await db._execute(
                "INSERT INTO users (telegram_id, first_name, is_coach, created_at) "
                "VALUES (42, 'Player', 0, 0), (43, 'Second', 1, 0), (123, 'Тренер', 1, 0)"
            )
            headers = init_data(role_id, now)
            responses = []
            for route in routes_list:
                body = body_for(route, schema)
                response = await client_request(headers, route, body)
                payload = None
                if response.headers.get("content-type", "").startswith("application/json"):
                    try:
                        payload = response.json()
                    except ValueError:
                        payload = None
                responses.append({
                    "role": role,
                    "method": route["method"],
                    "path": route["path"],
                    "status": response.status_code,
                    "request_json": body,
                    "json": payload,
                })
            return responses
        finally:
            await db.close()


async def client_request(headers, route, body):
    async with httpx2.AsyncClient(
        transport=httpx2.ASGITransport(app=main.app),
        base_url="http://reference",
    ) as client:
        return await client.request(
            route["method"],
            route["path"],
            headers=headers,
            json=body if body is not None else None,
            content=None if body is not None else b"",
        )


async def run():
    now = int(sys.argv[1]) if len(sys.argv) > 1 else 0
    schema = main.app.openapi()
    routes_list = routes()
    responses = []
    for role, role_id in ROLES:
        responses.extend(await run_pass(role, role_id, now, routes_list, schema))
    print(json.dumps({"responses": responses}, ensure_ascii=False))


if __name__ == "__main__":
    asyncio.run(run())
