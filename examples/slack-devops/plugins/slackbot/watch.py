#!/usr/bin/env -S uv run --script --quiet
# /// script
# requires-python = ">=3.11"
# dependencies = []
# ///
"""The slackbot watcher: brg runs it as the plugin's process.

Every few seconds it reads new messages in the configured channel and hands
each new top-level human message to $BORGCHART_DELIVER_TO as an ID only. It
also retries deliveries that failed (its own and `slackbot escalate`'s), and
reports health in $BORGCHART_STATE_DIR/health.json.
"""

from __future__ import annotations

import fcntl
import json
import os
import sys
import time
from typing import Any

import slackapi
from slackapi import SlackError

POLL_SECONDS = float(os.environ.get("SLACK_POLL_SECONDS", "10"))
MAX_BACKOFF = 300.0


def write_json(name: str, value: Any) -> None:
    """Replace a state file atomically, never following a planted symlink."""
    path = slackapi.state_dir() / name
    tmp = path.with_suffix(f".{os.getpid()}.tmp")
    try:
        tmp.unlink()
    except FileNotFoundError:
        pass
    fd = os.open(tmp, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as f:
        f.write(json.dumps(value, sort_keys=True))
    os.replace(tmp, path)


def read_json(name: str, default: Any) -> Any:
    try:
        fd = os.open(slackapi.state_dir() / name, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd, encoding="utf-8") as f:
            return json.load(f)
    except (OSError, ValueError):
        return default


def health(ok: bool, detail: str) -> None:
    write_json("health.json", {"ok": ok, "at": slackapi.now_iso(), "detail": detail})


def deliver(target: str, ts: str) -> bool:
    code, message = slackapi.brg_send(target, slackapi.message_notice(ts))
    if code == 0:
        return True
    slackapi.record_undelivered(
        {
            "kind": "message",
            "target": target,
            "ts": ts,
            "attempts": 1,
            "first_failed_at": slackapi.now_iso(),
            "next_attempt": time.time() + 15,
            "last_error": message,
        }
    )
    return False


def retry_undelivered(now: float) -> int:
    """Retry due entries; keep the rest. Returns how many are still pending."""
    path = slackapi.state_dir() / slackapi.UNDELIVERED
    if not path.exists():
        return 0
    fd = os.open(path, os.O_RDWR | os.O_NOFOLLOW)
    with os.fdopen(fd, "r+", encoding="utf-8") as f:
        fcntl.flock(f, fcntl.LOCK_EX)
        entries = []
        for line in f:
            try:
                entries.append(json.loads(line))
            except ValueError:
                continue  # drop unreadable lines
        keep = []
        for entry in entries:
            if not isinstance(entry, dict):
                continue
            if entry.get("next_attempt", 0) > now:
                keep.append(entry)
                continue
            try:
                target, text, sender = slackapi.notice_for(entry)
            except ValueError as e:
                print(f"dropping a queued delivery: {e}", file=sys.stderr)
                continue
            code, message = slackapi.brg_send(target, text, sender=sender)
            if code == 5 and "may only send to its parent" in message:
                print(f"dropping a queued delivery: {message}", file=sys.stderr)
                continue
            if code != 0:
                entry["attempts"] = entry.get("attempts", 1) + 1
                entry["last_error"] = message
                entry["next_attempt"] = now + min(15 * 2 ** (entry["attempts"] - 1), MAX_BACKOFF)
                keep.append(entry)
        f.seek(0)
        f.truncate()
        for entry in keep:
            f.write(json.dumps(entry, sort_keys=True) + "\n")
        f.flush()
        fcntl.flock(f, fcntl.LOCK_UN)
    return len(keep)


def poll_once(bot_user: str | None) -> int:
    """Read new channel messages and deliver them. Returns how many were delivered."""
    target = os.environ.get("BORGCHART_DELIVER_TO", "")
    state = read_json("cursor.json", {})
    cursor = state.get("oldest")
    seen: list[str] = state.get("seen", [])
    if cursor is None:
        # First run: start from now rather than replaying the channel's history.
        write_json("cursor.json", {"oldest": f"{time.time():.6f}", "seen": []})
        return 0
    messages: list[dict] = []
    params: dict[str, Any] = {"channel": slackapi.channel(), "oldest": cursor, "limit": 100}
    for _ in range(5):  # up to 500 new messages per poll
        data = slackapi.call("conversations.history", params)
        messages.extend(m for m in data.get("messages", []) if isinstance(m, dict))
        next_cursor = (data.get("response_metadata") or {}).get("next_cursor")
        if not data.get("has_more") or not next_cursor:
            break
        params = {**params, "cursor": next_cursor}
    messages.sort(key=lambda m: float(m.get("ts", "0")) if slackapi.TS_RE.match(str(m.get("ts", ""))) else 0.0)
    delivered = 0
    newest = cursor
    for message in messages:
        ts = message.get("ts", "")
        if not slackapi.TS_RE.match(ts) or ts in seen:
            continue
        seen.append(ts)
        if float(ts) > float(newest):
            newest = ts
        if not slackapi.is_human_top_level(message, bot_user) or not target:
            continue
        if deliver(target, ts):
            delivered += 1
    write_json("cursor.json", {"oldest": newest, "seen": seen[-500:]})
    return delivered


def main(argv: list[str]) -> int:
    once = "--once" in argv
    bot_user = None
    while True:
        try:
            if bot_user is None:
                bot_user = slackapi.call("auth.test", {}).get("user_id")
            poll_once(bot_user)
            pending = retry_undelivered(time.time())
            health(True, f"{pending} undelivered" if pending else "watching")
        except SlackError as e:
            health(False, str(e))
        except Exception as e:  # noqa: BLE001 - keep watching; report it instead
            try:
                health(False, f"unexpected {type(e).__name__}")
            except OSError:
                pass
            print(f"watcher error: {type(e).__name__}: {e}", file=sys.stderr)
        if once:
            return 0
        time.sleep(POLL_SECONDS)


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
