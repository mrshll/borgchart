"""Shared helpers for the slackbot plugin: Slack Web API calls and delivery to members.

Standard library only. Every value that comes from Slack is untrusted: it is
printed for an agent to read, never forwarded as an instruction.
"""

from __future__ import annotations

import fcntl
import json
import os
import re
import subprocess
import time
import urllib.error
import urllib.parse
import urllib.request
from collections.abc import Callable
from datetime import UTC, datetime
from pathlib import Path
from typing import Any

TS_RE = re.compile(r"^\d{9,11}\.\d{6}$")
SEVERITIES = ("low", "medium", "high")
REASONS = ("outage", "deploy", "security", "question", "other")
MAX_REPLY_CHARS = 3000
UNDELIVERED = "undelivered.jsonl"


class SlackError(Exception):
    """A Slack API call failed."""


def api_base() -> str:
    return os.environ.get("SLACK_API_BASE", "https://slack.com/api").rstrip("/")


def token() -> str:
    value = os.environ.get("SLACK_BOT_TOKEN", "")
    if not value:
        raise SlackError("SLACK_BOT_TOKEN is not set")
    return value


def channel() -> str:
    value = os.environ.get("SLACK_CHANNEL_ID", "")
    if not value:
        raise SlackError("SLACK_CHANNEL_ID is not set")
    return value


def call(
    method: str,
    params: dict[str, Any],
    *,
    post: bool = False,
    retries: int = 3,
    sleep: Callable[[float], None] = time.sleep,
) -> dict[str, Any]:
    """Call a Slack Web API method, honoring rate limits (HTTP 429 + Retry-After)."""
    url = f"{api_base()}/{method}"
    headers = {"Authorization": f"Bearer {token()}"}
    for attempt in range(retries + 1):
        if post:
            body = json.dumps(params).encode()
            req = urllib.request.Request(
                url, data=body, headers={**headers, "Content-Type": "application/json; charset=utf-8"}
            )
        else:
            req = urllib.request.Request(f"{url}?{urllib.parse.urlencode(params)}", headers=headers)
        try:
            with urllib.request.urlopen(req, timeout=20) as resp:
                data = json.loads(resp.read().decode())
        except urllib.error.HTTPError as e:
            if e.code == 429 and attempt < retries:
                wait = min(float(e.headers.get("Retry-After", "1") or 1), 60.0)
                sleep(wait)
                continue
            raise SlackError(f"{method}: HTTP {e.code}") from None
        except urllib.error.URLError as e:
            raise SlackError(f"{method}: {e.reason}") from None
        if not data.get("ok"):
            raise SlackError(f"{method}: {data.get('error', 'unknown error')}")
        return data
    raise SlackError(f"{method}: rate limited")


def check_ts(value: str) -> str:
    if not TS_RE.match(value):
        raise ValueError(f"not a Slack message timestamp: {value!r}")
    return value


def is_human_top_level(message: dict[str, Any], bot_user: str | None) -> bool:
    """New top-level messages from people only: no bots, edits, joins or thread replies."""
    if message.get("subtype") or message.get("bot_id"):
        return False
    if bot_user and message.get("user") == bot_user:
        return False
    thread_ts = message.get("thread_ts")
    return not (thread_ts and thread_ts != message.get("ts"))


def now_iso() -> str:
    return datetime.now(UTC).strftime("%Y-%m-%dT%H:%M:%SZ")


def state_dir() -> Path:
    path = Path(os.environ.get("BORGCHART_STATE_DIR", ".")).resolve()
    path.mkdir(parents=True, exist_ok=True)
    return path


def brg_send(member: str, text: str) -> tuple[int, str]:
    """Hand `text` (IDs and fixed values only) to a member with `brg send`."""
    brg = os.environ.get("BORGCHART_BIN", "brg")
    proc = subprocess.run(
        [brg, "send", "--json", member, text], capture_output=True, text=True, timeout=30, check=False
    )
    message = proc.stdout.strip() or proc.stderr.strip()
    try:
        message = json.loads(message).get("message", message)
    except (ValueError, AttributeError):
        pass
    return proc.returncode, message


MEMBER_RE = re.compile(r"^[a-z][a-z0-9_-]{0,31}$")


def notice_for(entry: dict[str, Any]) -> tuple[str, str]:
    """Rebuild a queued delivery's target and text from its validated fields.

    The queue file lives in a directory sandboxed members may write, so the
    watcher never sends stored text: it re-creates the fixed notice from IDs
    and enum values, and refuses anything else.
    """
    target = str(entry.get("target", ""))
    if not MEMBER_RE.match(target):
        raise ValueError(f"bad target {target!r}")
    ts = check_ts(str(entry.get("ts", "")))
    kind = entry.get("kind")
    if kind == "message":
        if target != os.environ.get("BORGCHART_DELIVER_TO", ""):
            raise ValueError("a message notice may only go to the watcher's own target")
        return target, message_notice(ts)
    if kind == "escalation":
        sender = str(entry.get("sender", ""))
        severity = entry.get("severity")
        reason = entry.get("reason")
        if not MEMBER_RE.match(sender) or severity not in SEVERITIES or reason not in REASONS:
            raise ValueError("bad escalation fields")
        return target, escalation_notice(sender, ts, severity, reason)
    raise ValueError(f"unknown kind {kind!r}")


def record_undelivered(entry: dict[str, Any]) -> None:
    """Append one failed delivery; the watcher retries it until it is delivered."""
    path = state_dir() / UNDELIVERED
    line = json.dumps(entry, sort_keys=True) + "\n"
    fd = os.open(path, os.O_WRONLY | os.O_APPEND | os.O_CREAT | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "a", encoding="utf-8") as f:
        fcntl.flock(f, fcntl.LOCK_EX)
        f.write(line)
        f.flush()
        fcntl.flock(f, fcntl.LOCK_UN)


def message_notice(ts: str) -> str:
    return (
        f"New message in the channel: ts={ts}. "
        "Read it with your read command and decide whether it needs escalating."
    )


def escalation_notice(sender: str, ts: str, severity: str, reason: str) -> str:
    return (
        f"Escalation from {sender}: message ts={ts}, severity={severity}, reason={reason}. "
        "Read it with your read command, then triage it and reply in its thread."
    )
