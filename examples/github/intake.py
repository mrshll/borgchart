"""Notification discovery, event qualification and bounded pointer publication."""

from __future__ import annotations

import fcntl
import hashlib
import json
import math
import os
import re
import signal
import stat
import subprocess
import sys
import tempfile
import time
from collections.abc import Callable, Iterator
from contextlib import contextmanager
from datetime import UTC, datetime, timedelta
from email.utils import parsedate_to_datetime
from pathlib import Path
from typing import Any
from urllib.parse import parse_qs, urlencode, urlsplit
from uuid import uuid4

from github_mentions import explicit_mention

NAME = re.compile(r"[A-Za-z0-9][A-Za-z0-9_.-]{0,99}/[A-Za-z0-9_.-]{1,100}\Z")

STOP = ("deadline", "transport", "rate_limit", "clock", "account_identity")

STOP += ("http_401",)

BRG_SEND: Callable[[dict[str, str], str, float], str | None] | None = None

BRG_ATTEMPTS = 3

BRG_TIMEOUT = 15.0

REACTION_ROUTES = {
    "issue": "issues",
    "issue_comment": "issues/comments",
    "review_comment": "pulls/comments",
}

REACTION_PATH = re.compile(
    r"repositories/[1-9][0-9]*/(issues|issues/comments|pulls/comments)"
    r"/[1-9][0-9]*/reactions"
)

REACTION_OBJECT = re.compile(
    r"(event|issue_comment|review|review_comment):([1-9][0-9]*)"
)

REACTION_SPACING = 2.0  # Seconds from POST completion to the next start.

REACTION_TIMEOUT = 15.0  # One POST.

REACTION_LOCK_WAIT = 2.0  # The control file lock, for any read-modify-write.

REACTION_LOCK_POLL = 0.5

REACTION_TARGETS = 10000

REACTION_BATCHES = 200

REACTION_FIELDS = ("version", "recent", "retry_at", "streak", "targets", "batches")

SETTLED = ("created", "existing", "failed", "refused", "uncertain")

SOURCE_FIELDS = ("key", "kind", "object_id", "version", "actor_id", "occurred_at")

SOURCE_FIELDS += ("repo_id", "number", "full_name", "proof", "initiator")

SIGNALS = ("assignment", "handoff", "mention", "merge")

MEMBER = re.compile(r"(app|reviewer):[1-9][0-9]*")

GITHUB_API = "https://api.github.com"

FULL_NAME = re.compile(r"([A-Za-z0-9][A-Za-z0-9-]{0,38})/([A-Za-z0-9._-]{1,100})")


class Fault(Exception):
    def __init__(self, kind: str, retry_at: float = 0) -> None:
        super().__init__(kind)
        self.kind, self.retry_at = kind, retry_at


def utc() -> str:
    return datetime.now(UTC).isoformat()


def stamp(value: str) -> datetime:
    try:
        result = datetime.fromisoformat(value.replace("Z", "+00:00"))
        if result.tzinfo is None:
            raise ValueError
        return result.astimezone(UTC)
    except (ValueError, AttributeError, TypeError) as error:
        raise Fault("timestamp") from error


def digest(*values: Any) -> str:
    return hashlib.sha256(
        json.dumps(values, sort_keys=True, separators=(",", ":")).encode()
    ).hexdigest()


def next_page(
    link: str, path: str, aliases: dict[str, str] | None = None
) -> str | None:
    if not link:
        return None
    matches = re.findall(r'<([^>]+)>;\s*rel="([a-z]+)"', link)
    if not matches or re.sub(r'<[^>]+>;\s*rel="[a-z]+"', "", link).strip(" ,"):
        raise Fault("pagination")
    original = urlsplit(path)
    query = parse_qs(original.query)
    current = int(query.pop("page", ["1"])[0])
    result = None
    for url, relation in matches:
        part = urlsplit(url)
        found = parse_qs(part.query)
        page = found.pop("page", ["1"])
        if (
            part.scheme != "https"
            or part.netloc != "api.github.com"
            or part.path
            not in {
                original.path,
                *(
                    (original.path.replace(a, b, 1))
                    for a, b in (aliases or {}).items()
                    if original.path.startswith(a + "/")
                ),
            }
            or found != query
            or part.fragment
            or len(page) != 1
            or not page[0].isdigit()
        ):
            raise Fault("pagination")
        if relation == "next":
            if int(page[0]) != current + 1 or result is not None:
                raise Fault("pagination")
            result = part.path + "?" + part.query
    return result


def gh(argv: list[str], timeout: float) -> subprocess.CompletedProcess[str]:
    """Run agent-gh in its own process group; on timeout kill only that group."""
    process = subprocess.Popen(
        argv,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.PIPE,
        stderr=subprocess.PIPE,
        text=True,
        start_new_session=True,
    )
    try:
        stdout, stderr = process.communicate(timeout=timeout)
    except subprocess.TimeoutExpired:
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            process.communicate(timeout=2)
        except subprocess.TimeoutExpired:
            # A survivor still holds the pipes; never wait on it indefinitely.
            for pipe in (process.stdout, process.stderr):
                if pipe:
                    pipe.close()
        raise
    return subprocess.CompletedProcess(argv, process.returncode, stdout, stderr)


def api_message(body: str) -> str:
    """The lower-cased API error message; never item contents."""
    try:
        value = json.loads(body)
    except ValueError:
        return ""
    message = value.get("message") if isinstance(value, dict) else None
    return message.lower() if isinstance(message, str) else ""


class API:
    def __init__(self, config: dict[str, Any], deadline: float | None = None) -> None:
        self.config, self.requests = config, 0
        self.aliases: dict[str, str] = {}
        # One run deadline (time.monotonic()) bounds every request of the run.
        self.deadline = (
            time.monotonic() + config["max_seconds"] if deadline is None else deadline
        )
        self.retry_at, self.poll_seconds = 0.0, config.get("poll_seconds", 300)

    def request(
        self, path: str, validator: str | None = None
    ) -> tuple[int, dict[str, str], Any]:
        if (
            not re.fullmatch(r"/[A-Za-z0-9_./?=&%:+,-]+", path)
            or path.startswith("//")
            or ".." in path
        ):
            raise Fault("path")
        if time.monotonic() >= self.deadline:
            raise Fault("deadline")
        if time.time() < self.retry_at:
            raise Fault("rate_limit", self.retry_at)
        argv = [
            *self.config["gh"],
            "api",
            "--hostname",
            "github.com",
            "--method",
            "GET",
            "--include",
            "-H",
            "Accept: application/vnd.github+json",
            "-H",
            "X-GitHub-Api-Version: 2022-11-28",
        ]
        if validator:
            if "\r" in validator or "\n" in validator:
                raise Fault("validator")
            argv += ["-H", "If-Modified-Since: " + validator]
        self.requests += 1
        try:
            result = gh(
                [*argv, path.lstrip("/")],
                max(0.01, self.deadline - time.monotonic()),
            )
            raw = result.stdout.replace("\r\n", "\n")
            head, body = raw.split("\n\n", 1)
            lines = head.splitlines()
            status = int(re.fullmatch(r"HTTP/[\d.]+ (\d{3}).*", lines[0]).group(1))
            headers = {
                name.lower().strip(): value.strip()
                for name, value in (line.split(":", 1) for line in lines[1:])
            }
            if len(raw) > 8 * 1024 * 1024:
                raise Fault("response_cap")
            server_date = parsedate_to_datetime(headers["date"]).timestamp()
            if abs(server_date - time.time()) > 300:
                raise Fault("clock")
        except subprocess.TimeoutExpired as error:
            kind = "deadline" if time.monotonic() >= self.deadline else "transport"
            raise Fault(kind) from error
        except (
            OSError,
            subprocess.SubprocessError,
            ValueError,
            KeyError,
            AttributeError,
        ) as error:
            raise Fault("transport") from error
        if headers.get("x-poll-interval", "").isdigit():
            self.poll_seconds = max(self.poll_seconds, int(headers["x-poll-interval"]))
        remaining = headers.get("x-ratelimit-remaining", "")
        if remaining == "0":
            self.retry_at = max(
                time.time() + 60, float(headers.get("x-ratelimit-reset", "0"))
            )
        if status in (403, 429) and (
            "retry-after" in headers
            or remaining == "0"
            or status == 429
            or status == 403
            and "secondary rate limit" in api_message(body)
        ):
            self.retry_at = max(
                self.retry_at,
                time.time() + max(60, int(headers.get("retry-after", "60"))),
            )
            raise Fault("rate_limit", self.retry_at)
        if "x-github-sso" in headers:
            raise Fault("sso")
        if (
            status == 304
            and validator
            and result.returncode in (0, 1)
            and not body.strip()
        ):
            return status, headers, None
        if status != 200 or result.returncode:
            raise Fault("http_" + str(status))
        try:
            return status, headers, json.loads(body)
        except ValueError as error:
            raise Fault("json") from error

    def get(self, path: str) -> dict[str, Any]:
        _, _, data = self.request(path)
        if not isinstance(data, dict):
            raise Fault("shape")
        if (
            re.fullmatch(r"/repos/[^/]+/[^/]+", path)
            and type(data.get("id")) is int
            and str(data.get("full_name", "")).lower() == path[7:].lower()
        ):
            self.aliases[path] = "/repositories/" + str(data["id"])
        return data

    def pages(self, path: str) -> list[dict[str, Any]]:
        path += ("&" if "?" in path else "?") + "per_page=100&page=1"
        result = []
        for _ in range(self.config["max_pages"]):
            _, headers, data = self.request(path)
            if not isinstance(data, list) or any(not isinstance(x, dict) for x in data):
                raise Fault("shape")
            result.extend(data)
            path = next_page(headers.get("link", ""), path, self.aliases)
            if path is None:
                return result
        raise Fault("page_cap")


def positive(value: Any) -> bool:
    return type(value) is int and value > 0


def trust_classes(cfg: dict[str, Any]) -> dict[str, list]:
    """Validated, sorted app_initiators and trusted_reviewer_ids; both default empty.

    Each class has narrow rights: an App's first assignment of the agent on
    its own issue, and a reviewer's feedback on a review the agent requested.
    No identity may hold two roles.
    """
    apps, reviewers = cfg.get("app_initiators", []), cfg.get("trusted_reviewer_ids", [])
    if not isinstance(apps, list) or any(
        not isinstance(app, dict)
        or sorted(app) != ["app_id", "bot_user_id", "repository_ids"]
        or not positive(app["app_id"])
        or not positive(app["bot_user_id"])
        or not isinstance(app["repository_ids"], list)
        or not app["repository_ids"]
        or not all(positive(each) for each in app["repository_ids"])
        or len(set(app["repository_ids"])) != len(app["repository_ids"])
        for app in apps
    ):
        raise Fault("config_app_initiators")
    if not isinstance(reviewers, list) or not all(positive(r) for r in reviewers):
        raise Fault("config_trusted_reviewers")
    bots = [app["bot_user_id"] for app in apps]
    people = {cfg["agent"]["id"], *cfg["trusted_actor_ids"]}
    if (
        len({app["app_id"] for app in apps}) != len(apps)
        or len(set(bots)) != len(bots)
        or len(set(reviewers)) != len(reviewers)
        or people & set(bots)
        or (people | set(bots)) & set(reviewers)
    ):
        raise Fault("config_class_overlap")
    return dict(
        app_initiators=sorted(
            (app | dict(repository_ids=sorted(app["repository_ids"])) for app in apps),
            key=lambda app: app["app_id"],
        ),
        trusted_reviewer_ids=sorted(reviewers),
    )


def members(cfg: dict[str, Any]) -> dict[str, dict[str, Any]]:
    """The enrolled class members by initiator; trusted_since keys use these names."""
    found: dict[str, dict[str, Any]] = {}
    for app in cfg.get("app_initiators", []):
        found[f"app:{app['app_id']}"] = dict(
            app_id=app["app_id"],
            bot=app["bot_user_id"],
            repos=frozenset(app["repository_ids"]),
        )
    for reviewer in cfg.get("trusted_reviewer_ids", []):
        found[f"reviewer:{reviewer}"] = dict(reviewer=reviewer)
    return found


def hold(row: dict[str, Any], key: str, reason: str | None = None) -> None:
    """Keep the first record, and its clock, while the same instance is unresolved."""
    row["hold"] = row.get("hold") or {}
    row["hold"].setdefault(key, dict(reason=reason or key, since=utc()))


def collect_feed(api: API, config: dict[str, Any], state: dict[str, Any]) -> None:
    feed = state["feed"]
    path = "/notifications?" + urlencode(
        dict(
            all="true",
            participating="false",
            per_page=50,
            page=1,
            # The documented whole-second form, truncated; floor stays exact.
            since=stamp(state["feature_start"])
            .replace(microsecond=0)
            .strftime("%Y-%m-%dT%H:%M:%SZ"),
        )
    )
    floor = (
        stamp(feed["last_complete"]) - timedelta(minutes=10)
        if feed["last_complete"]
        else stamp(state["feature_start"])
    )
    validator = feed.get("validator")
    complete_at, new_validator = None, None
    for page in range(config["max_pages"]):
        status, headers, data = api.request(path, validator if page == 0 else None)
        feed["due"] = time.time() + api.poll_seconds
        if status == 304:
            return
        if page == 0:
            if (
                status == 200
                and data == []
                and "last-modified" not in headers
                and "link" not in headers
            ):
                # Empty GitHub responses can omit Last-Modified. Do not invent
                # a watermark from Date; keep the fixed since and prior floor.
                feed["validator"] = None
                return
            try:
                new_validator = headers["last-modified"]
                complete_at = (
                    parsedate_to_datetime(new_validator).astimezone(UTC).isoformat()
                )
            except (KeyError, ValueError) as error:
                raise Fault("feed_metadata") from error
        if not isinstance(data, list):
            raise Fault("feed_shape")
        older = bool(data)
        for thread in data:
            try:
                updated = stamp(thread["updated_at"])
                older = older and updated < floor
                repo = thread["repository"]
                row = (
                    state["row"]
                    if repo["owner"]["id"] == config["owner_policy"]["owner"]["id"]
                    else None
                )
                if (
                    row is None
                    or updated < floor
                    or thread["subject"]["type"] not in ("Issue", "PullRequest")
                ):
                    continue
                full_name = repo["full_name"]
                route = "issues" if thread["subject"]["type"] == "Issue" else "pulls"
                match = re.fullmatch(
                    r"https://api\.github\.com/repos/([^/]+/[^/]+)/"
                    + route
                    + r"/([1-9][0-9]*)",
                    thread["subject"]["url"],
                )
                if (
                    not NAME.fullmatch(full_name)
                    or not match
                    or match[1].lower() != full_name.lower()
                    or type(repo["id"]) is not int
                ):
                    raise Fault("feed_subject")
                number = int(match[2])
                key, version = (
                    f"{repo['id']}:{number}",
                    str(thread["id"]) + ":" + thread["updated_at"],
                )
                if row.setdefault("hints", {}).get(key) != version:
                    row["dirty"].setdefault(
                        key,
                        dict(
                            repo_id=repo["id"],
                            number=number,
                            full_name=full_name,
                            version=version,
                            at=utc(),
                            attempts=0,
                            held=False,
                        ),
                    )
            except (KeyError, TypeError, ValueError) as error:
                raise Fault("feed_shape") from error
        path = next_page(headers.get("link", ""), path)
        if path is None or older:
            # Nominations and the validator are written in one atomic transaction.
            feed.update(validator=new_validator, last_complete=complete_at)
            return
    raise Fault("feed_page_cap")


def persist(cfg: dict[str, Any], state: dict[str, Any]) -> None:
    try:
        if len(encode_state(cfg, state)) > cfg[
            "max_state_bytes"
        ] - 4096 and "state_capacity" not in (state["feed"].get("hold") or {}):
            raise Fault("state_capacity")
        save_state(cfg, state)
    except Fault as error:
        if error.kind != "state_capacity":
            raise
        previous = load_state(cfg)
        previous["fault"] = "state_capacity"
        hold(previous["feed"], "state_capacity")
        save_state(cfg, previous)
        state.clear()
        state.update(previous)
        raise


def excluded(
    session: dict[str, Any], row: dict[str, Any], repo_id: int, full_name: str
) -> bool:
    for name in session["exclude_repositories"]:
        if name.lower() == full_name.lower():
            row["pins"].setdefault(name.lower(), repo_id)
    return (
        repo_id in session["exclude_repository_ids"] or repo_id in row["pins"].values()
    )


def cancel_pending(row: dict[str, Any], repo_id: int) -> None:
    row["pending"] = {
        key: value
        for key, value in row["pending"].items()
        if value["repo_id"] != repo_id
    }


def targeted(
    api: API,
    cfg: dict[str, Any],
    session: dict[str, Any],
    row: dict[str, Any],
    pointer: dict[str, Any],
) -> str | None:
    """Verify and merge one item; its canonical name, or None when out of scope."""
    repo_id = pointer["repo_id"]
    entry = row["repos"].get(str(repo_id))
    if (entry is None and session["repository_policy"] != "include_new") or (
        entry is not None and entry["status"] in ("held", "excluded")
    ):
        # Outside the snapshot or already excluded: a scope decision, no GET.
        row["scope_skipped"] += 1
        cancel_pending(row, repo_id)
        return None
    snapshot = verify_item(
        api, session, cfg, repo_id, pointer["full_name"], pointer["number"]
    )
    repo = snapshot["repo"]
    if repo["id"] != repo_id or repo["owner_id"] != session["owner"]["id"]:
        raise Fault("repository_identity")
    if excluded(session, row, repo_id, pointer["full_name"]) or excluded(
        session, row, repo_id, repo["full_name"]
    ):
        if entry:
            entry["status"] = "excluded"
        cancel_pending(row, repo_id)
        return None
    if entry is None:
        entry = dict(
            full_name=repo["full_name"],
            baseline=row["new_repo_cut"],
            status="admitted",
            listed=True,
        )
        row["repos"][str(repo_id)] = entry
    if entry["status"] != "admitted":
        raise Fault("repository_held")
    entry["full_name"] = repo["full_name"]
    merge_snapshot(row, snapshot, utc(), members(cfg))
    return repo["full_name"]


def verify_account(api: API, cfg: dict[str, Any]) -> None:
    account = api.get("/user")
    if (
        account.get("id") != cfg["agent"]["id"]
        or str(account.get("login", "")).lower() != cfg["agent"]["login"].lower()
        or account.get("type") != "User"
    ):
        raise Fault("account_identity")


def collect(api: API, cfg: dict[str, Any], state: dict[str, Any]) -> None:
    if "state_capacity" in (state["feed"].get("hold") or {}):
        return
    feed = state["feed"]
    if not feed.get("hold") and time.time() >= feed.get("due", 0):
        try:
            collect_feed(api, cfg, state)
        except Fault as error:
            feed["due"] = max(time.time() + api.poll_seconds, error.retry_at)
            if error.kind not in ("deadline", "rate_limit", "transport") and not (
                transient(error.kind)
            ):
                hold(feed, error.kind)
            state["fault"] = error.kind
            if error.kind in STOP:
                persist(cfg, state)
                return
        persist(cfg, state)
    pointers = sorted(
        (p["at"], owner, key)
        for owner, row in [("owner", state["row"])]
        for key, p in row["dirty"].items()
        if not p["held"]
    )
    for _, owner, key in pointers:
        row, session = state["row"], cfg["owner_policy"]
        pointer = row["dirty"][key]
        try:
            targeted(api, cfg, session, row, pointer)
            row.setdefault("hints", {})[key] = pointer["version"]
            del row["dirty"][key]
            (row.get("hold") or {}).pop("item:" + key, None)
        except Fault as error:
            state["fault"] = error.kind
            if error.kind in STOP:
                persist(cfg, state)
                break
            # A server error defers this item to a later run; anything else
            # is deterministic and waits for an operator.
            if not transient(error.kind):
                pointer["held"] = True
                hold(row, "item:" + key, error.kind)
        persist(cfg, state)


def transient(kind: str) -> bool:
    """Any server error (5xx) defers the read to a later run."""
    return re.fullmatch(r"http_5\d\d", kind) is not None


def item_key(proof: dict[str, Any]) -> str:
    return f"{proof['repo_id']}:{proof['number']}"


def pending_keys(row: dict[str, Any]) -> list[str]:
    """Pending keys whose item pointer is not held for an operator."""
    held = {key for key, p in (row.get("dirty") or {}).items() if p["held"]}
    return [
        key
        for key, proof in row["pending"].items()
        if not (held and item_key(proof) in held)
    ]


def hold_item(row: dict[str, Any], item: str, full_name: str, kind: str) -> None:
    """Hold a deterministically failed item for an operator."""
    repo_id, number = (int(part) for part in item.split(":"))
    pointer = row["dirty"].setdefault(
        item,
        dict(
            repo_id=repo_id,
            number=number,
            full_name=full_name,
            version="verify:" + utc(),
            at=utc(),
            attempts=0,
            held=False,
        ),
    )
    pointer["held"] = True
    hold(row, "item:" + item, kind)


class BrgUnavailable(Exception):
    """Transport/config is unavailable and definitely did not publish anything."""


def brg_pointer(item: dict[str, Any]) -> str:
    """Only verified pointer metadata, never GitHub titles, bodies or raw URLs."""
    kind, number = item["object_id"].split(":", 1)
    path, fragment = "issues", ""
    if kind == "issue_comment":
        fragment = "#issuecomment-" + number
    elif kind == "review_comment":
        path, fragment = "pull", "#discussion_r" + number
    elif kind == "review":
        path, fragment = "pull", "#pullrequestreview-" + number
    elif item["kind"] == "merge":
        path = "pull"
    return json.dumps(
        dict(
            untrusted=True,
            key=item["key"],
            kind=item["kind"],
            signal=item.get("signal"),
            repository_id=item["repo_id"],
            repository=item["full_name"],
            number=item["number"],
            actor_id=item["actor_id"],
            initiator=item.get("initiator", "human"),
            occurred_at=item["occurred_at"],
            url=f"https://github.com/{item['full_name']}/{path}/{item['number']}{fragment}",
        ),
        sort_keys=True,
    )


def produce_brg(
    cfg: dict[str, Any],
    state: dict[str, Any],
    deadline: float,
    plans: list[dict[str, Any]],
    api: API | None = None,
) -> None:
    """Best-effort producer: no member inspection, binding, claim or bus ACK."""
    if cfg["mode"] != "wake":
        return
    remaining = 100
    for session in [cfg["owner_policy"]]:
        row = state["row"]
        if BRG_SEND is None:
            hold(row, "brg_unavailable", "brg_not_ready")
            persist(cfg, state)
            continue
        (row.get("hold") or {}).pop("brg_unavailable", None)
        if deadline - time.monotonic() < BRG_TIMEOUT or remaining <= 0:
            return
        try:
            keys, stop = verified_pending(cfg, state, session, row, deadline, api)
        except Fault as error:
            state["fault"] = error.kind
            persist(cfg, state)
            continue
        if stop is not None:
            state["fault"] = stop.kind
        # Verification can cancel an event while it waits between attempts.
        for name in list(row.get("hold") or {}):
            if name.startswith("brg:") and name[4:] not in row["pending"]:
                del row["hold"][name]
        persist(cfg, state)
        owner_plan = dict(
            owner=str(session["owner"]["id"]),
            batch="brg:" + uuid4().hex,
            targets={},
            dispositions={},
            trace=None,
            deadline=deadline,
        )
        plan_queued = False
        for key in keys[:remaining]:
            item = row["pending"][key]
            name = "brg:" + key
            attempt = (row.get("hold") or {}).get(name)
            if attempt and time.time() < attempt["next_attempt"]:
                continue
            if deadline - time.monotonic() < BRG_TIMEOUT:
                return
            if attempt is None:
                hold(row, name, "brg_retry")
                attempt = row["hold"][name]
                attempt.update(attempts=0, next_attempt=0, result="unattempted")
            if attempt["attempts"] < BRG_ATTEMPTS:
                text = brg_pointer(item)
                attempt.update(
                    attempts=attempt["attempts"] + 1,
                    next_attempt=time.time() + cfg["poll_seconds"],
                    result="uncertain",
                )
                persist(cfg, state)  # A crash consumes an attempt, never resets it.
                remaining -= 1
                try:
                    bus_id = BRG_SEND(delivery_target(), text, BRG_TIMEOUT)
                    if bus_id is not None and (
                        not isinstance(bus_id, str) or not bus_id or len(bus_id) > 160
                    ):
                        raise ValueError("invalid bus commitment")
                except BrgUnavailable:
                    attempt["attempts"] -= 1
                    if not attempt["attempts"]:
                        del row["hold"][name]
                    hold(row, "brg_unavailable", "brg_not_ready")
                    persist(cfg, state)
                    break
                except (OSError, subprocess.SubprocessError, ValueError):
                    bus_id = None
                    attempt["result"] = "uncertain"
                else:
                    attempt["result"] = "committed" if bus_id else "refused"
                if bus_id:
                    if reactions_on(cfg):
                        try:
                            item_plan = item_reactions(
                                cfg, session, row, [key], owner_plan["batch"]
                            )
                            owner_plan["targets"].update(item_plan["targets"])
                            owner_plan["dispositions"].update(item_plan["dispositions"])
                            prepare_reactions(cfg, owner_plan)
                        except Exception:  # noqa: BLE001 - a reaction never loses intake.
                            owner_plan["trace"] = "unavailable"
                    del row["pending"][key]
                    del row["hold"][name]
                    persist(cfg, state)  # Durable dedup before any GitHub POST.
                    if reactions_on(cfg) and not plan_queued:
                        plans.append(owner_plan)
                        plan_queued = True
                    continue
            if attempt["attempts"] >= BRG_ATTEMPTS:
                del row["pending"][key]
                del row["hold"][name]
                print(
                    f"github-intake: abandoned event {item['key']} after {BRG_ATTEMPTS} attempts",
                    file=sys.stderr,
                )
            persist(cfg, state)


def verified_pending(
    cfg: dict[str, Any],
    state: dict[str, Any],
    session: dict[str, Any],
    row: dict[str, Any],
    deadline: float | None,
    api: API | None = None,
) -> tuple[list[str], Fault | None]:
    """Shared oldest-first revalidation; never creates a consumer receipt."""
    waiting = pending_keys(row)
    if not waiting:
        return [], None
    if cfg["mode"] == "off":
        raise Fault("mode_off")
    wait = max(state["backoff"], reaction_cooldown(cfg))
    if time.time() < wait:
        raise Fault("rate_limit", wait)
    groups: dict[str, list[tuple[str, str]]] = {}
    for key in waiting:
        proof = row["pending"][key]
        groups.setdefault(item_key(proof), []).append((proof["occurred_at"], key))
    api, keys, stop = api or API(cfg, deadline), [], None
    try:
        verify_account(api, cfg)
        for item in sorted(groups, key=lambda i: min(groups[i])):
            if len(keys) >= 100:
                break
            left = [key for _, key in groups[item] if key in row["pending"]]
            if not left:
                continue  # An earlier item of this repository cancelled it.
            repo_id, number = (int(part) for part in item.split(":"))
            # A staged retry pointer, else the last verified repository name.
            source = row["dirty"].get(item) or row["repos"].get(str(repo_id))
            name = (source or row["pending"][left[0]])["full_name"]
            try:
                targeted(
                    api,
                    cfg,
                    session,
                    row,
                    dict(repo_id=repo_id, number=number, full_name=name),
                )
            except Fault as error:
                if error.kind in STOP:
                    raise
                if not transient(error.kind):
                    hold_item(row, item, name, error.kind)
                continue
            (row.get("hold") or {}).pop("item:" + item, None)
            found = sorted(
                (p["occurred_at"], k)
                for k, p in row["pending"].items()
                if item_key(p) == item
            )
            keys += [k for _, k in found][: 100 - len(keys)]
    except Fault as error:
        stop = error
    state["backoff"] = max(state["backoff"], api.retry_at)
    return keys, stop


def reaction_target(
    item: dict[str, Any], trusted: dict[str, Any], policy: dict[str, dict[str, Any]]
) -> tuple[str | None, str | None]:
    """The object one verified item reacts on, or why none; pure, never guessed."""
    try:
        if item["kind"] == "merge":
            return None, "skipped:merge"  # News, not work started.
        found = REACTION_OBJECT.fullmatch(item["object_id"])
        repo_id, number, proof = item["repo_id"], item["number"], item["proof"]
        if (
            found is None
            or not positive(repo_id)
            or not positive(number)
            or not isinstance(proof, dict)
            or item["key"]
            != digest(
                "github.com", repo_id, item["kind"], item["object_id"], item["version"]
            )
        ):
            return None, "unsupported:malformed"
        kind, object_id = found[1], int(found[2])
        signal, shape = (item["kind"], item.get("signal")), proof.get("object_type")

        def same(field: str) -> bool:
            return positive(proof.get(field)) and proof[field] == object_id

        if signal == ("assignment", "assignment") and kind == "event":
            valid, target = same("event_id"), f"issue:{number}"
        elif signal == ("feedback", "mention") and kind == "issue_comment":
            valid, target = shape == kind and same("comment_id"), f"{kind}:{object_id}"
        elif signal == ("feedback", "handoff") and kind != "event":
            valid = (
                shape == kind
                and positive(proof.get("request_event_id"))
                and positive(proof.get("reviewer_id"))
                and (kind != "review" or same("review_id"))
            )
            target = f"{kind}:{object_id}"
        else:
            return None, "unsupported:malformed"
        if not valid:
            return None, "unsupported:malformed"
        initiator, actor = item.get("initiator"), item["actor_id"]
        member = policy.get(initiator) if isinstance(initiator, str) else None
        if not positive(actor):
            return None, "unsupported:provenance"
        if initiator == "human":
            if str(actor) not in trusted:
                return None, "unsupported:provenance"
        elif MEMBER.fullmatch(_text(initiator)) is None:
            return None, "unsupported:provenance"
        elif initiator.startswith("app:"):
            app_id = proof.get("app_id")
            if signal[0] != "assignment" or not (
                positive(app_id) and app_id == int(initiator[4:])
            ):
                return None, "unsupported:provenance"
            if member is None or repo_id not in member["repos"]:
                return None, "skipped:unenrolled"
            if actor != member["bot"]:
                return None, "unsupported:provenance"
        else:
            reviewer = int(initiator[9:])
            if signal[1] != "handoff" or not actor == reviewer == proof["reviewer_id"]:
                return None, "unsupported:provenance"
            if member is None or member["reviewer"] != reviewer:
                return None, "skipped:unenrolled"
        if kind == "review":
            return None, "unsupported:whole_review"  # No REST reaction route.
        return f"{repo_id}:{target}", None
    except (KeyError, TypeError, AttributeError):
        return None, "unsupported:malformed"


def reactions_on(cfg: dict[str, Any]) -> bool:
    return cfg.get("reactions") is True


def item_reactions(
    cfg: dict[str, Any],
    session: dict[str, Any],
    row: dict[str, Any],
    keys: list[str],
    batch_id: str,
) -> dict[str, Any]:
    """Reaction targets for confirmed producer commits."""
    targets: dict[str, int] = {}
    dispositions: dict[str, str] = {}
    pins = set(row["pins"].values()) | set(session["exclude_repository_ids"])
    for key in keys:
        item = row["pending"][key]
        target, why = reaction_target(item, row["trusted_since"], members(cfg))
        repo = row["repos"].get(str(item.get("repo_id")))
        if why is None and (
            repo is None or repo["status"] != "admitted" or item["repo_id"] in pins
        ):
            why = "skipped:scope"
        if why is None and cfg["mode"] != "wake":
            why = "skipped:mode_" + cfg["mode"]
        if why is not None:
            dispositions[f"{item.get('repo_id')}:{item.get('object_id')}"[:160]] = why
        else:
            targets.setdefault(target, item["number"])
    return dict(
        owner=str(session["owner"]["id"]),
        batch=batch_id,
        targets=targets,
        dispositions=dispositions,
        trace=None,
    )


def _reaction_file(cfg: dict[str, Any], suffix: str) -> Path:
    path = Path(cfg["state"])
    return path.with_name(path.name + ".reactions" + suffix)


def read_reactions(cfg: dict[str, Any]) -> dict[str, Any]:
    """The control file, strictly validated; missing means empty, never reset."""
    path = _reaction_file(cfg, ".json")
    try:
        info = os.lstat(path)
    except FileNotFoundError:
        return dict(version=1, recent=[], retry_at=0, streak=0, targets={}, batches={})
    except OSError as error:
        raise Fault("reactions") from error
    _private(info, "reactions")
    if info.st_size > 8 * 1024 * 1024:
        raise Fault("reactions")
    try:
        data = json.loads(path.read_bytes())
        if not (
            isinstance(data, dict)
            and sorted(data) == sorted(REACTION_FIELDS)
            and data["version"] == 1
            and isinstance(data["recent"], list)
            and len(data["recent"]) <= 240  # Bound the attempt history.
            and all(_moment(each) for each in data["recent"])
            and _moment(data["retry_at"])
            and type(data["streak"]) is int
            and 0 <= data["streak"] <= 64
            and isinstance(data["targets"], dict)
            and len(data["targets"]) <= REACTION_TARGETS
            and all(_target_record(k, v) for k, v in data["targets"].items())
            and isinstance(data["batches"], dict)
            and len(data["batches"]) <= REACTION_BATCHES
            and all(_batch_record(k, v) for k, v in data["batches"].items())
        ):
            raise Fault("reactions")
    except (
        OSError,
        ValueError,
        TypeError,
        KeyError,
        ArithmeticError,
        RecursionError,
        Fault,
    ) as error:
        raise Fault("reactions") from error
    return data


def _moment(value: Any) -> bool:
    """A finite, non-negative epoch time before the year 5000."""
    if type(value) is int:
        return 0 <= value <= 10**11
    return type(value) is float and math.isfinite(value) and 0 <= value <= 1e11


def _short(value: Any, empty: bool = False) -> bool:
    return (value is None and empty) or (
        isinstance(value, str) and 0 < len(value) <= 160
    )


def _target_record(key: Any, value: Any) -> bool:
    if not (_short(key) and isinstance(value, dict)):
        return False
    stamp(value["at"])
    return (
        sorted(value) == ["at", "batch", "outcome", "owner", "reason", "status"]
        and value["outcome"] in SETTLED
        and _short(value["reason"], empty=True)
        and (value["status"] is None or positive(value["status"]))
        and _short(value["batch"])
        and _short(value["owner"])
    )


def _batch_record(key: Any, value: Any) -> bool:
    if not (_short(key) and isinstance(value, dict)):
        return False
    stamp(value["at"])
    return (
        sorted(value) == ["at", "owner", "phase", "targets"]
        and value["phase"] in ("prepared", "authorized", "done")
        and _short(value["owner"])
        and isinstance(value["targets"], dict)
        and len(value["targets"]) <= 100
        and all(
            _short(k) and _short(v, empty=True) for k, v in value["targets"].items()
        )
    )


def write_reactions(cfg: dict[str, Any], data: dict[str, Any]) -> None:
    path = _reaction_file(cfg, ".json")
    done = sorted(
        (each["at"], batch)
        for batch, each in data["batches"].items()
        if each["phase"] == "done"
    )
    for _, batch in done[: max(0, len(data["batches"]) - REACTION_BATCHES)]:
        del data["batches"][batch]
    if len(data["batches"]) > REACTION_BATCHES:
        raise Fault("reaction_capacity")
    fd, name = tempfile.mkstemp(prefix=".reactions-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(json.dumps(data, sort_keys=True).encode())
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(name, path)
    finally:
        if os.path.lexists(name):
            os.unlink(name)
    _sync_dir(path.parent)


@contextmanager
def reaction_lock(
    cfg: dict[str, Any], suffix: str, wait: float | None = None
) -> Iterator[bool]:
    """A stable private lock; wait indefinitely for the sender, bound file edits."""
    fd = os.open(
        _reaction_file(cfg, suffix), os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600
    )
    try:
        _private(os.fstat(fd), "lock")
        until = None if wait is None else time.monotonic() + wait
        while True:
            try:
                fcntl.flock(fd, fcntl.LOCK_EX | (fcntl.LOCK_NB if until else 0))
                break
            except BlockingIOError:
                if time.monotonic() >= until:
                    yield False
                    return
                time.sleep(REACTION_LOCK_POLL)
        yield True
    finally:
        os.close(fd)


def update_reactions(cfg: dict[str, Any], change: Any) -> dict[str, Any]:
    with reaction_lock(cfg, ".lock", REACTION_LOCK_WAIT) as held:
        if not held:
            raise Fault("reactions_busy")
        data = read_reactions(cfg)
        change(data)
        write_reactions(cfg, data)
        return data


def reaction_cooldown(cfg: dict[str, Any]) -> float:
    """The reactions' server cooldown, honored read-only by intake."""
    try:
        return float(read_reactions(cfg)["retry_at"])
    except Fault:
        return 0.0  # Intake never stops on the reaction trace.


def prepare_reactions(cfg: dict[str, Any], plan: dict[str, Any]) -> None:
    """Record the planned targets before the commit; this never authorizes."""
    if plan["trace"] is not None:
        return

    def change(data: dict[str, Any]) -> None:
        data["batches"][plan["batch"]] = dict(
            owner=plan["owner"],
            at=utc(),
            phase="prepared",
            targets={target: None for target in plan["targets"]} | plan["dispositions"],
        )

    update_reactions(cfg, change)


def post_eyes(
    cfg: dict[str, Any], target: str, timeout: float
) -> tuple[int, dict[str, str], Any]:
    """The plugin's only GitHub write: one eyes reaction on one numeric object."""
    repo_id, route, number = target.split(":")
    path = f"repositories/{repo_id}/{REACTION_ROUTES.get(route, '?')}/{number}"
    path += "/reactions"
    if not REACTION_PATH.fullmatch(path):
        raise Fault("path")
    argv = [*cfg["gh"], "api", "--hostname", "github.com"]
    argv += [
        "--method",
        "POST",
        "--include",
        "-H",
        "Accept: application/vnd.github+json",
    ]
    argv += ["-H", "X-GitHub-Api-Version: 2022-11-28", "-f", "content=eyes", path]
    result = gh(argv, timeout)
    raw = result.stdout.replace("\r\n", "\n")
    head, body = raw.split("\n\n", 1)
    lines = head.splitlines()
    status = int(re.fullmatch(r"HTTP/[\d.]+ (\d{3}).*", lines[0]).group(1))
    headers = {
        name.lower().strip(): value.strip()
        for name, value in (line.split(":", 1) for line in lines[1:])
    }
    try:
        data = json.loads(body)
    except ValueError:
        data = None
    return status, headers, data


def _reaction_outcome(
    cfg: dict[str, Any], status: int, headers: dict[str, str], body: Any, streak: int
) -> tuple[str, str | None, float, bool]:
    """(outcome, reason, retry_at, stop) for one POST response."""
    now, retry_at = time.time(), 0.0
    remaining = headers.get("x-ratelimit-remaining", "")
    if remaining == "0":
        retry_at = max(now + 60, float(headers.get("x-ratelimit-reset", "0") or 0))
    if status in (200, 201):
        user = body.get("user") if isinstance(body, dict) else None
        if (
            isinstance(body, dict)
            and body.get("content") == "eyes"
            and isinstance(user, dict)
            and type(user.get("id")) is int
            and user["id"] == cfg["agent"]["id"]
        ):
            outcome = "created" if status == 201 else "existing"
            return outcome, None, retry_at, retry_at > 0
        return "uncertain", "unconfirmed", retry_at, True
    if 300 <= status < 400:
        return "failed", "redirect", retry_at, False
    if status in (403, 429) and (
        "retry-after" in headers
        or remaining == "0"
        or status == 429
        or "secondary rate limit" in api_message(json.dumps(body))
    ):
        wait = int(headers.get("retry-after", "0") or 0)
        retry_at = max(retry_at, now + wait, now + min(3600, 60 * 2**streak))
        return "refused", "rate_limited", retry_at, True
    if status in (404, 410):
        return "failed", "not_found_or_inaccessible", retry_at, False
    if status == 422:  # Validation failed, or the endpoint has been spammed.
        return "failed", "http_422", max(retry_at, now + 60), True
    if status == 401:
        return "failed", "http_401", retry_at, True
    if 400 <= status < 500:
        return "failed", f"http_{status}", retry_at, status == 403
    return "uncertain", f"http_{status}", retry_at, True


def _reaction_read(
    api: API, session: dict[str, Any], target: str, number: int, seen: dict
) -> str | None:
    """Why the target must not be touched now, or None: numeric reads only."""
    repo_id, route, object_id = target.split(":")
    if repo_id not in seen:
        repo, owner = api.get(f"/repositories/{repo_id}"), session["owner"]
        found = repo.get("owner")
        named = FULL_NAME.fullmatch(_text(repo.get("full_name")))
        if (
            repo.get("id") != int(repo_id)
            or not isinstance(found, dict)
            or found.get("id") != owner["id"]
            or found.get("type") != owner["type"]
            or _text(found.get("login")).lower() != owner["login"].lower()
            or named is None
        ):
            seen[repo_id] = None
        else:
            seen[repo_id] = f"{GITHUB_API}/repos/{repo['full_name']}".lower()
    root = seen[repo_id]
    if root is None:
        return "moved"
    if route == "issue":
        issue = api.get(f"/repositories/{repo_id}/issues/{object_id}")
        same = issue.get("number") == number and (
            _text(issue.get("repository_url")).lower() == root
        )
    elif route == "issue_comment":
        comment = api.get(f"/repositories/{repo_id}/issues/comments/{object_id}")
        same = _text(comment.get("issue_url")).lower() == f"{root}/issues/{number}"
    else:
        comment = api.get(f"/repositories/{repo_id}/pulls/comments/{object_id}")
        parent = _text(comment.get("pull_request_url")).lower()
        same = parent == f"{root}/pulls/{number}"
    return None if same else "moved"


def send_reactions(
    cfg: dict[str, Any],
    plan: dict[str, Any],
    deadline: float | None = None,
) -> dict[str, Any]:
    """Post the planned reactions serially; never raises, never fails publication."""
    counts = dict.fromkeys(
        ("created", "existing", "suppressed", "failed", "refused", "uncertain"), 0
    ) | dict(unsupported=0, skipped=0)
    for why in plan["dispositions"].values():
        counts[why.split(":")[0]] += 1
    if plan["trace"] is not None:
        counts["skipped"] += len(plan["targets"])
        return counts | dict(trace=plan["trace"])
    batch = plan["batch"]
    left = list(plan["targets"])

    def settle(target: str, disposition: str, record: dict | None = None) -> None:
        def change(data: dict[str, Any]) -> None:
            entry = data["batches"].get(batch)
            if entry is not None:
                entry["targets"][target] = disposition
                if not left:
                    entry["phase"] = "done"
            if record is not None:
                data["targets"][target] = record

        counts[disposition.split(":")[0]] += 1
        update_reactions(cfg, change)

    def skip_rest(why: str) -> None:
        while left:
            settle(left.pop(0), "skipped:" + why)

    try:
        if not left:

            def finish(data: dict[str, Any]) -> None:
                data["batches"][batch]["phase"] = "done"

            update_reactions(cfg, finish)
            return counts
        session = cfg["owner_policy"]

        def authorize(data: dict[str, Any]) -> None:
            data["batches"][batch]["phase"] = "authorized"

        update_reactions(cfg, authorize)
        wait = None if deadline is None else max(0, deadline - time.monotonic())
        with reaction_lock(cfg, ".send.lock", wait) as held:
            if not held:
                skip_rest("deadline")
                return counts
            api, seen, forbidden = API(cfg), {}, 0

            def gate(data: dict[str, Any], reserved: int = 0) -> str | None:
                """Why no POST may start now, from current config, state and file."""
                if (
                    deadline is not None
                    and deadline - time.monotonic()
                    < REACTION_TIMEOUT + 2 + REACTION_LOCK_WAIT
                ):
                    return "deadline"
                current = read_config(cfg["config_path"])
                if not reactions_on(current) or current["mode"] != "wake":
                    return "disabled"
                now = time.time()
                cooldown = (data["retry_at"], read_state(cfg)["backoff"], api.retry_at)
                if now < max(cooldown):
                    return "backoff"
                if len(data["targets"]) - reserved >= REACTION_TARGETS:
                    return "capacity"
                return None

            def cool(data: dict[str, Any]) -> None:
                data["retry_at"] = max(data["retry_at"], api.retry_at)

            while left:
                target = left[0]
                data = read_reactions(cfg)
                prior = data["targets"].get(target)
                if prior is not None:
                    left.pop(0)
                    settle(target, "suppressed:" + prior["outcome"])
                    continue
                why = gate(data)
                if why is not None:
                    skip_rest(why)
                    break
                try:
                    api.deadline = (
                        min(time.monotonic() + cfg["max_seconds"], deadline)
                        if deadline is not None
                        else time.monotonic() + cfg["max_seconds"]
                    )
                    moved = _reaction_read(
                        api, session, target, plan["targets"][target], seen
                    )
                except Fault as error:
                    if error.kind in STOP or transient(error.kind):
                        if error.kind == "rate_limit":
                            update_reactions(cfg, cool)
                        skip_rest(
                            "backoff" if error.kind == "rate_limit" else "stopped"
                        )
                        break
                    moved = (
                        "not_found_or_inaccessible"
                        if error.kind in ("http_404", "http_410")
                        else "read_" + error.kind
                    )
                if api.retry_at > time.time():  # A read exhausted the quota.
                    update_reactions(cfg, cool)
                    skip_rest("backoff")
                    break
                if moved is not None:
                    left.pop(0)
                    settle(
                        target,
                        "failed:" + moved,
                        _reaction_record(plan, "failed", moved),
                    )
                    continue
                # Spacing runs from the end of the previous POST, recorded below.
                pause = max(data["recent"], default=0) + REACTION_SPACING - time.time()
                if (
                    deadline is not None
                    and time.monotonic()
                    + pause
                    + REACTION_TIMEOUT
                    + 2
                    + REACTION_LOCK_WAIT
                    > deadline
                ):
                    skip_rest("deadline")
                    break
                if pause > 0:
                    time.sleep(pause)
                data = read_reactions(cfg)
                why = gate(data)  # The reads and the wait took time.
                if why is not None:
                    skip_rest(why)
                    break
                left.pop(0)
                previous = data["recent"]

                def reserve(data: dict[str, Any]) -> None:
                    data["recent"] = [end]
                    data["targets"][target] = _reaction_record(
                        plan, "uncertain", "in_flight"
                    )
                    data["batches"][batch]["targets"][target] = "uncertain:in_flight"

                # Reserve a conservative end for this POST, renewing if saving
                # or checking it took too long. Expiry never drops a reaction.
                margin = REACTION_LOCK_WAIT
                while True:
                    started = time.monotonic()
                    end = time.time() + REACTION_TIMEOUT + 2 + margin
                    update_reactions(cfg, reserve)  # No trace, no POST.
                    why = gate(read_reactions(cfg), reserved=1)
                    elapsed = time.monotonic() - started
                    if why is not None or (
                        elapsed <= margin and time.time() + REACTION_TIMEOUT + 2 <= end
                    ):
                        break
                    margin = max(margin * 2, elapsed * 2)
                if why is not None:

                    def release(data: dict[str, Any]) -> None:
                        data["recent"] = previous
                        data["targets"].pop(target, None)
                        data["batches"][batch]["targets"][target] = None

                    update_reactions(cfg, release)
                    settle(target, "skipped:" + why)
                    skip_rest(why)
                    break
                try:
                    status, headers, body = post_eyes(cfg, target, REACTION_TIMEOUT)
                    outcome, reason, retry_at, stop = _reaction_outcome(
                        cfg, status, headers, body, data["streak"]
                    )
                except subprocess.TimeoutExpired:
                    status, retry_at, stop = None, 0.0, True
                    outcome, reason = "uncertain", "timeout"
                except OSError:
                    status, retry_at, stop = None, 0.0, True
                    outcome, reason = "failed", "spawn"  # Nothing left the host.
                except (
                    ValueError,
                    AttributeError,
                    IndexError,
                    subprocess.SubprocessError,
                ):
                    status, retry_at, stop = None, 0.0, True
                    outcome, reason = "uncertain", "unparseable"
                if outcome == "failed" and reason == "http_403":
                    forbidden += 1
                    stop = forbidden >= 3

                def record(data: dict[str, Any]) -> None:
                    # The attempt's actual end replaces its latest possible end.
                    data["recent"] = [time.time()]
                    data["retry_at"] = max(data["retry_at"], retry_at)
                    if outcome == "refused":
                        data["streak"] = min(64, data["streak"] + 1)
                    elif outcome in ("created", "existing"):
                        data["streak"] = 0

                update_reactions(cfg, record)
                settle(
                    target,
                    f"{outcome}:{reason}" if reason else outcome,
                    _reaction_record(plan, outcome, reason, status),
                )
                if stop:
                    skip_rest("stopped")
    except Exception as error:  # noqa: BLE001 - a reaction never fails publication.
        kind = error.kind if isinstance(error, Fault) else type(error).__name__
        return counts | dict(fault=kind)
    return counts


def _reaction_record(
    plan: dict[str, Any], outcome: str, reason: str | None, status: int | None = None
) -> dict[str, Any]:
    return dict(
        outcome=outcome,
        reason=reason,
        status=status,
        at=utc(),
        batch=plan["batch"],
        owner=plan["owner"],
    )


def reaction_summary(cfg: dict[str, Any]) -> dict[str, Any]:
    """Counts for status; never fails it."""
    try:
        data = read_reactions(cfg)
    except Fault as error:
        return dict(enabled=reactions_on(cfg), unreadable=error.kind)
    counts = dict.fromkeys(SETTLED + ("unattempted", "unsupported"), 0)
    for each in data["targets"].values():
        counts[each["outcome"]] += 1
    skipped: dict[str, int] = {}
    for batch in data["batches"].values():
        for value in batch["targets"].values():
            if value is None:
                counts["unattempted"] += batch["phase"] != "done"
            elif value.startswith("skipped:"):
                reason = value.removeprefix("skipped:")
                skipped[reason] = skipped.get(reason, 0) + 1
            elif value.startswith("unsupported:"):
                counts["unsupported"] += 1
    return counts | dict(
        enabled=reactions_on(cfg),
        skipped=sum(skipped.values()),
        skipped_reasons=skipped,
        retry_at=data["retry_at"],
        last_attempt_at=max(data["recent"], default=None),
    )


def _private(info: os.stat_result, kind: str, directory: bool = False) -> None:
    form = stat.S_ISDIR if directory else stat.S_ISREG
    if not form(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise Fault(kind)


def _private_dir(path: Path) -> None:
    try:
        info = os.lstat(path)
    except OSError as error:
        raise Fault("state_directory") from error
    _private(info, "state_directory", directory=True)


def encode_state(config: dict[str, Any], state: dict[str, Any]) -> bytes:
    """Bytes to persist, after the scope, record and byte caps all pass."""
    if state["scope"] != scope(config):
        raise Fault("scope_changed")
    records = sum(len(state["row"][name] or ()) for name in CAPPED)
    data = json.dumps(state, sort_keys=True, separators=(",", ":")).encode()
    if records > config["max_records"] or len(data) > config["max_state_bytes"]:
        raise Fault("state_capacity")
    return data


def _sync_dir(path: Path) -> None:
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def save_state(config: dict[str, Any], state: dict[str, Any]) -> None:
    """Atomic private replace; a refused or failed save keeps the previous file."""
    data = encode_state(config, state)
    path = Path(config["state"])
    _private_dir(path.parent)
    fd, name = tempfile.mkstemp(prefix=".state-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(data)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(name, path)
    finally:
        if os.path.lexists(name):
            os.unlink(name)
    _sync_dir(path.parent)


@contextmanager
def locked(config: dict[str, Any]) -> Iterator[None]:
    """Nonblocking exclusive lock on a stable file beside the state; never removed."""
    path = Path(config["state"])
    _private_dir(path.parent)
    try:
        fd = os.open(
            path.with_name(path.name + ".lock"),
            os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW,
            0o600,
        )
    except OSError as error:
        raise Fault("lock") from error
    try:
        _private(os.fstat(fd), "lock")
        try:
            fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
        except BlockingIOError as error:
            raise Fault("busy") from error
        yield
    finally:
        os.close(fd)


def _source(
    value: Any,
    snapshot: dict[str, Any],
    trusted: dict[str, Any],
    signal: bool,
    policy: dict[str, dict[str, Any]],
) -> dict[str, Any]:
    """One trusted assignment/feedback source: identifiers and times only.

    The source's initiator class is checked here, independently of the
    verifier: a human by numeric ID, an App only for its bot's assignment in
    a listed repository's ordinary issue, a reviewer only for returned
    feedback on a review the agent requested.
    """
    fields = set(SOURCE_FIELDS)
    if (
        not isinstance(value, dict)
        or set(value) - {"signal"} != fields
        or (signal and value.get("signal") not in SIGNALS)
        or value["kind"] not in ("assignment", "feedback", "merge")
        or value["repo_id"] != snapshot["repo"]["id"]
        or value["number"] != snapshot["number"]
        or not isinstance(value["proof"], dict)
        or any(
            not isinstance(each, (int, bool, type(None)))
            and not (isinstance(each, str) and len(each) <= 256)
            for each in value["proof"].values()
        )
    ):
        raise Fault("snapshot")
    initiator, actor, proof = value["initiator"], value["actor_id"], value["proof"]
    member = policy.get(initiator)
    if initiator == "human":
        if type(actor) is not int or str(actor) not in trusted:
            raise Fault("untrusted_source")
        if value["kind"] == "merge" and snapshot.get("is_pr") is not True:
            raise Fault("untrusted_source")
    elif member is None or initiator not in trusted:
        raise Fault("untrusted_source")
    elif "bot" in member:
        if (
            value["kind"] != "assignment"
            or value.get("signal") not in (None, "assignment")
            or actor != member["bot"]
            or snapshot["repo"]["id"] not in member["repos"]
            or snapshot["is_pr"] is not False
            or proof.get("app_id") != member["app_id"]
        ):
            raise Fault("untrusted_source")
    elif (
        value["kind"] != "feedback"
        or snapshot.get("is_pr") is not True
        or value.get("signal") != "handoff"
        or not actor == member["reviewer"] == proof.get("reviewer_id")
    ):
        raise Fault("untrusted_source")
    else:
        stamp(proof.get("requested_at"))
    stamp(value["occurred_at"])
    return value


def _fresh(
    row: dict[str, Any],
    repo: dict[str, Any],
    signal: dict[str, Any],
    prior: dict[str, Any] | None,
) -> bool:
    """Source freshness, judged against objects as they were before this read.

    A class member's cutoff is its enrollment. An App's assignment must
    happen after it. A reviewer's feedback follows the human rule, edits
    included, but only on a review requested after it, so nothing from
    before enrollment ever qualifies.
    """
    initiator = signal["initiator"]
    if initiator == "human":
        enrolled = row["trusted_since"][str(signal["actor_id"])]
    else:
        enrolled = row["trusted_since"][initiator]
        floor = signal["occurred_at"]
        if initiator.startswith("reviewer:"):
            floor = signal["proof"]["requested_at"]
        if stamp(floor) <= stamp(enrolled):
            return False
    cuts = (repo["baseline"], enrolled)
    cutoff = max(stamp(each) for each in cuts if each is not None)
    occurred = stamp(signal["occurred_at"])
    if signal["signal"] == "mention":
        # No edit override, and work once per comment.
        late = occurred > max(cutoff, stamp(row["mention_since"]))
        return late and (prior is None or prior["kind"] != "feedback")
    # A new version of an object first read inside scope is an edit.
    edited = (
        prior is not None
        and prior["kind"] == signal["kind"]
        and stamp(prior["seen_at"]) >= cutoff
    )
    return occurred > cutoff or edited


def merge_snapshot(
    row: dict[str, Any],
    snapshot: dict[str, Any],
    now: str,
    policy: dict[str, dict[str, Any]] | None = None,
) -> int:
    """Record one verified item's trusted sources; return how many were queued.

    Every trusted assignment/feedback version is recorded in seen on first
    sight and refreshes objects on every read, whatever its freshness; only
    eligible, fresh, first-seen signals queue. Pending work on the
    item cancels when the item closed, an assignment stopped being eligible,
    or its source was deleted. A merge has already happened, so none of these
    cancels one.
    """
    repo_id = snapshot["repo"]["id"]
    repo = row["repos"].get(str(repo_id))
    if repo is None or repo["status"] != "admitted":
        raise Fault("repo_scope")
    observed: dict[str, dict[str, Any]] = {}
    for value in snapshot["observations"]:
        source = _source(value, snapshot, row["trusted_since"], False, policy or {})
        if observed.setdefault(source["key"], source) != source:
            raise Fault("snapshot")
    apps = [s for s in observed.values() if s["initiator"].startswith("app:")]
    if len(apps) > 1:
        raise Fault("snapshot")  # Only the first App assignment is ever a source.
    eligible: dict[str, dict[str, Any]] = {}
    for value in snapshot["signals"]:
        signal = _source(value, snapshot, row["trusted_since"], True, policy or {})
        base = observed.get(signal["key"], {})
        if {k: v for k, v in base.items() if k != "signal"} != {
            k: v for k, v in signal.items() if k != "signal"
        } or eligible.setdefault(signal["key"], signal) != signal:
            raise Fault("snapshot")
    present = set(snapshot["present"])
    for key, proof in list(row["pending"].items()):
        if (proof["repo_id"], proof["number"]) != (
            repo_id,
            snapshot["number"],
        ):
            continue
        if proof["kind"] != "merge" and (
            not snapshot["open"]
            or (proof["kind"] == "assignment" and proof["key"] not in eligible)
            or proof["object_id"] not in present
        ):
            del row["pending"][key]
        else:
            # Pending work follows a verified rename.
            proof["full_name"] = snapshot["repo"]["full_name"]
    previous = dict(row["objects"])
    queued = 0
    for key, source in observed.items():
        composite, target = f"{repo_id}:{key}", f"{repo_id}:{source['object_id']}"
        prior = previous.get(target)
        signal = eligible.get(key)
        # Judge freshness first so a timestamp fault records nothing as seen.
        fresh = (
            composite not in row["seen"]
            and signal is not None
            and _fresh(row, repo, signal, prior)
        )
        # Every read refreshes the edit-rule timestamp, for subsequent edit detection.
        row["objects"][target] = {"seen_at": now, "kind": source["kind"]}
        if composite in row["seen"]:
            continue
        row["seen"][composite] = now
        if fresh:
            row["pending"][composite] = dict(signal)
            queued += 1
    return queued


def _id(value: Any) -> int:
    if type(value) is not int or value < 1:
        raise Fault("malformed")
    return value


def _text(value: Any) -> str:
    if not isinstance(value, str):
        raise Fault("malformed")
    return value


def _user(value: Any) -> int | None:
    """A deleted ("ghost") or absent user has no trustworthy identity."""
    if value is None:
        return None
    if not isinstance(value, dict):
        raise Fault("malformed")
    return _id(value.get("id"))


def _human(raw: dict, field: str) -> int | None:
    """The ID of a plain user acting directly; bots, apps and ghosts give None."""
    found = _user(raw.get(field))
    if (
        found is None
        or raw[field].get("type") != "User"
        or raw.get("performed_via_github_app") is not None
    ):
        return None
    return found


def _bot(value: Any, bot_id: int) -> bool:
    """The configured App bot user itself, typed Bot; nothing is inferred."""
    return (
        isinstance(value, dict)
        and type(value.get("id")) is int
        and value["id"] == bot_id
        and value.get("type") == "Bot"
    )


def _app(value: Any, app_id: int) -> bool:
    """A present performed_via_github_app object naming exactly this App."""
    return (
        isinstance(value, dict)
        and type(value.get("id")) is int
        and value["id"] == app_id
    )


def _echo(raw: dict, agent_id: int) -> bool:
    """GitHub's actor for an assignment of the agent: the agent itself.

    Only the assigner says who acted, so this actor carries no identity.
    """
    actor = raw.get("actor")
    return (
        isinstance(actor, dict)
        and type(actor.get("id")) is int
        and actor["id"] == agent_id
        and actor.get("type") == "User"
    )


def _maybe_app(raw: dict, app: dict, actor: int | None) -> bool:
    """An assignment that is not conclusively someone else's.

    It names the bot or the App, or carries no identity at all; such an
    event can only be the App's first assignment or block a later one.
    """
    ids = {_user(raw.get("assigner")), actor} - {None}
    return (
        app["bot_user_id"] in ids
        or not ids
        or _app(raw.get("performed_via_github_app"), app["app_id"])
    )


def _time(value: Any) -> datetime:
    try:
        moment = datetime.fromisoformat(_text(value).replace("Z", "+00:00"))
    except ValueError:
        raise Fault("malformed") from None
    if moment.tzinfo is None:
        raise Fault("malformed")
    return moment.astimezone(UTC)


def _utc(moment: datetime) -> str:
    return moment.isoformat(timespec="microseconds")


def _body(raw: dict) -> str | None:
    body = raw.get("body")
    return None if body is None else _text(body)


def _parent(raw: dict, field: str, expected: str) -> int:
    """The source's ID after its canonical parent check, whoever wrote it."""
    _id(raw.get("id"))
    if _text(raw.get(field)).lower() != expected:
        raise Fault("identity")
    return raw["id"]


def _handoffs(
    facts: list[tuple], reviews: dict[int, list[dict]], trusted: set[int]
) -> dict[int, list[dict]]:
    """Each trusted reviewer's handoff intervals, rebuilt from full history.

    A request by the agent opens one; a repeat while it is open continues it.
    It closes at the earliest of the reviewer's first review strictly after
    the request, a removal for that reviewer, or a close or merge; that review
    matches when at or before the close. A same-second review is ignored, a
    same-second removal or close closes (fail closed), equal-time reviews
    match the lowest ID.
    """
    closes = [at for kind, at, _, _ in facts if kind == "close"]
    out: dict[int, list[dict]] = {}
    for reviewer in sorted(trusted):
        ends = closes + [
            at for kind, at, _, who in facts if kind == "removal" and who == reviewer
        ]
        submitted = sorted(reviews.get(reviewer, []), key=lambda r: (r["at"], r["id"]))
        requests = sorted(
            (at, event_id)
            for kind, at, event_id, who in facts
            if kind == "request" and who == reviewer
        )
        intervals: list[dict] = []
        for at, event_id in requests:
            if intervals and (
                intervals[-1]["closed"] is None or at < intervals[-1]["closed"]
            ):
                continue
            first = next((r for r in submitted if r["at"] > at), None)
            candidates = [end for end in ends if end >= at]
            if first is not None:
                candidates.append(first["at"])
            closed = min(candidates, default=None)
            intervals.append(
                {
                    "opened": at,
                    "closed": closed,
                    "review": first
                    if first is not None and (closed is None or first["at"] <= closed)
                    else None,
                    "handoff": {
                        "request_event_id": event_id,
                        "requested_at": _utc(at),
                        "reviewer_id": reviewer,
                    },
                }
            )
        out[reviewer] = intervals
    return out


def verify_item(
    api: Any, session: dict, config: dict, repo_id: int, full_name: str, number: int
) -> dict:
    agent, owner = config["agent"], session["owner"]
    trusted = set(config["trusted_actor_ids"]) - {agent["id"]}
    # Reviewers return feedback on reviews the agent requested; nothing else.
    reviewers = set(config.get("trusted_reviewer_ids", [])) - trusted - {agent["id"]}
    returning = trusted | reviewers
    named = FULL_NAME.fullmatch(full_name) if isinstance(full_name, str) else None
    if (
        named is None
        or named[2] in (".", "..")
        or named[1].lower() != owner["login"].lower()
    ):
        raise Fault("malformed")
    _id(repo_id)
    _id(number)

    repo = api.get(f"/repositories/{repo_id}")
    found = repo.get("owner")
    canonical = repo.get("full_name")
    canonical_name = (
        FULL_NAME.fullmatch(canonical) if isinstance(canonical, str) else None
    )
    if (
        repo.get("id") != repo_id
        or not isinstance(found, dict)
        or found.get("id") != owner["id"]
        or found.get("type") != owner["type"]
        or _text(found.get("login")).lower() != owner["login"].lower()
        or canonical_name is None
        or canonical_name[2] in (".", "..")
        or canonical_name[1].lower() != owner["login"].lower()
    ):
        raise Fault("identity")
    base = f"/repos/{canonical}"
    api.aliases[base] = f"/repositories/{repo_id}"
    root = f"{GITHUB_API}{base}".lower()
    pull_url, thread_url = f"{root}/pulls/{number}", f"{root}/issues/{number}"

    issue = api.get(f"{base}/issues/{number}")
    _id(issue.get("id"))
    if (
        issue.get("number") != number
        or _text(issue.get("repository_url")).lower() != root
        or _text(issue.get("url")).lower() != thread_url
    ):
        raise Fault("identity")  # Transferred, renumbered or foreign.
    state, assignees = issue.get("state"), issue.get("assignees")
    if state not in ("open", "closed") or not isinstance(assignees, list):
        raise Fault("malformed")
    assigned = agent["id"] in [_id(_user(a)) for a in assignees]
    is_pr = issue.get("pull_request") is not None
    if is_pr and (
        not isinstance(issue["pull_request"], dict)
        or _text(issue["pull_request"].get("url")).lower() != pull_url
    ):
        raise Fault("identity")

    observations: list[dict] = []
    present: list[str] = []

    def role(author: int) -> str:
        return f"reviewer:{author}" if author in reviewers else "human"

    def observe(
        kind: str,
        signal: str | None,
        object_id: str,
        version: str,
        actor: int,
        at: datetime,
        proof: dict[str, Any],
        initiator: str = "human",
    ) -> None:
        observations.append(
            {
                "key": digest("github.com", repo_id, kind, object_id, version),
                "kind": kind,
                "signal": signal,
                "object_id": object_id,
                "version": version,
                "actor_id": actor,
                "occurred_at": _utc(at),
                "repo_id": repo_id,
                "number": number,
                "full_name": canonical,
                "proof": proof,
                "initiator": initiator,
            }
        )

    # An App initiator only on an ordinary issue its own bot filed through it.
    app = next(
        (
            each
            for each in config.get("app_initiators", [])
            if not is_pr
            and repo_id in each["repository_ids"]
            and _bot(issue.get("user"), each["bot_user_id"])
            and _app(issue.get("performed_via_github_app"), each["app_id"])
        ),
        None,
    )
    candidates: dict[int, tuple[datetime, dict]] = {}
    bodies: dict[int, dict] = {}
    conflicted = False

    # A merge of the agent's own PR, after the boundary, by the trusted human.
    merges_after = config.get("merge_wakes_after")
    own_pr = merges_after is not None and is_pr and _human(issue, "user") == agent["id"]

    # History: assignments, handoff facts, and the bounds that obsolete work.
    facts: list[tuple] = []  # (request | removal | close, at, event ID, reviewer)
    bounds: list[datetime] = []
    for raw in api.pages(f"{base}/issues/{number}/events"):
        event_id = _parent(raw, "url", f"{root}/issues/events/{raw.get('id')}")
        present.append(f"event:{event_id}")
        if app is not None:
            # One event ID read two ways leaves firstness unproven.
            conflicted |= bodies.setdefault(event_id, raw) != raw
        name, at = _text(raw.get("event")), _time(raw.get("created_at"))
        actor = _user(raw.get("actor"))
        if name in ("assigned", "unassigned"):
            if _user(raw.get("assignee")) != agent["id"]:
                continue
            if name == "unassigned":
                bounds.append(at)  # Any unassignment bounds earlier work.
                continue
            if _echo(raw, agent["id"]):
                actor = None
            if app is not None and _maybe_app(raw, app, actor):
                candidates[event_id] = (at, raw)
                continue
            assigner = _human(raw, "assigner")
            if assigner not in trusted or actor not in (None, assigner):
                continue  # Missing, self, app, bot, untrusted or conflicting.
            version = digest(
                event_id, at.isoformat()
            )  # Stable timestamp representation.
            observe(
                "assignment",
                "assignment",
                f"event:{event_id}",
                version,
                assigner,
                at,
                {"event_id": event_id},
            )
        elif name == "review_requested":
            requester = _user(raw.get("review_requester"))
            reviewer = _human(raw, "requested_reviewer")
            # Team, bot and app-mediated requests never qualify.
            if requester == agent["id"] == actor and reviewer in returning:
                facts.append(("request", at, event_id, reviewer))
        elif name == "review_request_removed":
            reviewer = _user(raw.get("requested_reviewer"))
            if reviewer in returning:
                facts.append(("removal", at, event_id, reviewer))
        elif name in ("closed", "merged"):
            facts.append(("close", at, event_id, None))
            bounds.append(at)
            if (
                name == "merged"
                and own_pr
                and at > stamp(merges_after)
                and _human(raw, "actor") in trusted
            ):
                observe(
                    "merge",
                    "merge",
                    f"event:{event_id}",
                    digest(event_id, at.isoformat()),
                    actor,
                    at,
                    {"event_id": event_id},
                )

    if candidates and not conflicted:
        # Only the first App assignment of the agent counts; if it is not
        # fully proven, none does (equal times take the lowest event ID).
        event_id = min(candidates, key=lambda key: (candidates[key][0], key))
        at, raw = candidates[event_id]
        bot, app_id = app["bot_user_id"], app["app_id"]
        if (
            _bot(raw.get("assigner"), bot)
            and (_bot(raw.get("actor"), bot) or _echo(raw, agent["id"]))
            and (
                "performed_via_github_app" not in raw
                or _app(raw["performed_via_github_app"], app_id)
            )
        ):
            observe(
                "assignment",
                "assignment",
                f"event:{event_id}",
                digest(event_id, at.isoformat()),
                bot,
                at,
                {"event_id": event_id, "app_id": app_id},
                f"app:{app_id}",
            )

    reviews: dict[int, list[dict]] = {}
    handoffs: dict[int, list[dict]] = {}
    if is_pr:
        for raw in api.pages(f"{base}/pulls/{number}/reviews"):
            if raw.get("state") == "PENDING":
                continue  # Unsubmitted drafts are invisible to their peers.
            review_id = _parent(raw, "pull_request_url", pull_url)
            present.append(f"review:{review_id}")
            author = _human(raw, "user")
            if author in returning and raw.get("submitted_at") is not None:
                reviews.setdefault(author, []).append(
                    {"id": review_id, "at": _time(raw["submitted_at"]), "raw": raw}
                )
        handoffs = _handoffs(facts, reviews, returning)
        matched = {
            interval["review"]["id"]: interval
            for intervals in handoffs.values()
            for interval in intervals
            if interval["review"] is not None
        }

    # Ordinary issues are read only once an activation boundary is configured;
    # their mentions must be created strictly after it.
    after = config.get("issue_mentions_after")
    after = None if after is None else stamp(after)
    comments = api.pages(f"{base}/issues/{number}/comments") if is_pr or after else []
    for raw in comments:
        comment_id = _parent(raw, "issue_url", thread_url)
        present.append(f"issue_comment:{comment_id}")
        author, body = _human(raw, "user"), _body(raw)
        if author not in returning:  # Also every bot, app, ghost and the agent.
            continue
        if _text(raw.get("url")).lower() != f"{root}/issues/comments/{comment_id}":
            raise Fault("identity")
        created, updated = (
            _time(raw.get("created_at")),
            _time(raw.get("updated_at")),
        )
        # Temporal correlation with the original creation, never reply proof.
        interval = next(
            (
                each
                for each in handoffs.get(author, [])
                if each["opened"] < created
                and (each["closed"] is None or created < each["closed"])
            ),
            None,
        )
        if interval is not None:  # A handoff wins over a mention (one key).
            signal, proof = "handoff", dict(interval["handoff"])
        elif author in reviewers:
            continue  # A reviewer's comment counts only as returned feedback.
        elif (
            body is not None
            and created == updated  # Unedited at first sight; no editor proof.
            and (is_pr or created > after)
            and explicit_mention(body, agent["login"])
        ):
            signal = "mention"
            proof = {
                "comment_id": comment_id,
                "agent_id": agent["id"],
                "created_at": _utc(created),
            }
        else:
            signal, proof = None, {}
        proof["object_type"] = "issue_comment"
        version = digest(raw.get("updated_at"), body)
        object_id = f"issue_comment:{comment_id}"
        observe(
            "feedback", signal, object_id, version, author, updated, proof, role(author)
        )

    if is_pr:
        for reviewer in sorted(reviews):
            for review in reviews[reviewer]:
                interval = matched.get(review["id"])
                if interval is None and reviewer in reviewers:
                    continue
                raw = review["raw"]
                # State is excluded: a dismissal is not new author feedback.
                version = digest(_body(raw), raw["submitted_at"], raw.get("commit_id"))
                observe(
                    "feedback",
                    None if interval is None else "handoff",
                    f"review:{review['id']}",
                    version,
                    reviewer,
                    review["at"],
                    {"object_type": "review", "review_id": review["id"]}
                    | ({} if interval is None else interval["handoff"]),
                    role(reviewer),
                )

        for raw in api.pages(f"{base}/pulls/{number}/comments"):
            comment_id = _parent(raw, "pull_request_url", pull_url)
            present.append(f"review_comment:{comment_id}")
            author = _human(raw, "user")
            if author not in returning:
                continue
            review_id = raw.get("pull_request_review_id")
            review_id = None if review_id is None else _id(review_id)
            updated = _time(raw.get("updated_at"))
            interval = matched.get(review_id)
            if interval is not None and interval["handoff"]["reviewer_id"] != author:
                interval = None
            if interval is None and author in reviewers:
                continue
            proof = {"object_type": "review_comment", "review_id": review_id}
            if interval is not None:
                proof |= interval["handoff"]
                # A draft comment can predate its review's submission.
                updated = max(updated, interval["review"]["at"])
            observe(
                "feedback",
                None if interval is None else "handoff",
                f"review_comment:{comment_id}",
                digest(raw.get("updated_at"), _body(raw)),
                author,
                updated,
                proof,
                role(author),
            )

    def eligible(each: dict) -> bool:
        """Open item; an assignment also current with no later-or-equal bound."""
        if each["kind"] == "merge":
            return True  # The closure is the news.
        if each["signal"] is None or state != "open":
            return False
        if each["kind"] == "feedback":
            return True
        at = datetime.fromisoformat(each["occurred_at"])
        return assigned and all(bound < at for bound in bounds)

    return {
        "repo": {"id": repo_id, "full_name": canonical, "owner_id": owner["id"]},
        "number": number,
        "is_pr": is_pr,
        "open": state == "open",
        "assigned": assigned,
        "observations": observations,
        "signals": [dict(each) for each in observations if eligible(each)],
        "present": sorted(present),
    }


CAPPED = ("seen", "objects", "pending", "dirty", "hints", "hold", "repos", "pins")


def delivery_target() -> dict[str, str]:
    """brg supplies the process's single recipient through delivers_to."""
    instance = os.environ.get("BORGCHART_INSTANCE", "")
    member = os.environ.get("BORGCHART_DELIVER_TO", "")
    if not all(
        re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_-]{0,150}", s) for s in (instance, member)
    ):
        raise BrgUnavailable("delivery_target")
    return dict(instance=instance, member=member)


def scope(cfg: dict[str, Any]) -> str:
    policy = cfg["owner_policy"]
    return digest(
        cfg["agent"],
        policy["owner"],
        sorted(cfg["trusted_actor_ids"]),
        trust_classes(cfg),
        policy["repository_policy"],
        cfg["initial_repositories"],
        cfg.get("issue_mentions_after"),
        cfg.get("merge_wakes_after"),
    )


def read_config(path: str) -> dict[str, Any]:
    """All identities and policy are operator-provided; no executable shell text."""
    try:
        cfg = json.loads(Path(path).read_text())
        if not isinstance(cfg, dict):
            raise ValueError
        defaults = dict(
            poll_seconds=300,
            max_seconds=120,
            max_pages=10,
            max_records=100000,
            max_state_bytes=32 * 1024 * 1024,
            reactions=False,
            mode="wake",
            initial_repositories={},
            app_initiators=[],
            trusted_reviewer_ids=[],
        )
        cfg = defaults | cfg
        for name, lo, hi in (
            ("poll_seconds", 60, 86400),
            ("max_seconds", 20, 300),
            ("max_pages", 1, 10),
            ("max_records", 100, 1000000),
            ("max_state_bytes", 8192, 128 * 1024 * 1024),
        ):
            if type(cfg[name]) is not int or not lo <= cfg[name] <= hi:
                raise ValueError
        if cfg["mode"] not in ("wake", "off") or type(cfg["reactions"]) is not bool:
            raise ValueError
        if (
            not isinstance(cfg["gh"], list)
            or not cfg["gh"]
            or not all(isinstance(s, str) and s and "\0" not in s for s in cfg["gh"])
            or not Path(cfg["gh"][0]).is_absolute()
        ):
            raise ValueError
        agent, policy = cfg["agent"], cfg["owner_policy"]
        owner = policy["owner"]
        for who in (agent, owner):
            if not positive(who["id"]) or not re.fullmatch(
                r"[A-Za-z0-9][A-Za-z0-9-]{0,38}", who["login"]
            ):
                raise ValueError
        if owner["type"] not in ("User", "Organization") or policy[
            "repository_policy"
        ] not in ("snapshot", "include_new"):
            raise ValueError
        humans = cfg["trusted_actor_ids"]
        if (
            not isinstance(humans, list)
            or not humans
            or not all(positive(x) for x in humans)
            or len(humans) != len(set(humans))
            or agent["id"] in humans
        ):
            raise ValueError
        policy.setdefault("exclude_repository_ids", [])
        policy.setdefault("exclude_repositories", [])
        if not isinstance(policy["exclude_repository_ids"], list) or not all(
            positive(x) for x in policy["exclude_repository_ids"]
        ):
            raise ValueError
        if not isinstance(policy["exclude_repositories"], list) or not all(
            isinstance(x, str) and FULL_NAME.fullmatch(x)
            for x in policy["exclude_repositories"]
        ):
            raise ValueError
        for name in ("issue_mentions_after", "merge_wakes_after"):
            if cfg.get(name) is not None:
                stamp(cfg[name])
        if not isinstance(cfg["initial_repositories"], dict):
            raise ValueError
        for key, entry in cfg["initial_repositories"].items():
            if (
                not key.isdigit()
                or not positive(int(key))
                or not FULL_NAME.fullmatch(entry["full_name"])
            ):
                raise ValueError
            stamp(entry["baseline"])
        trust_classes(cfg)
        home = Path(cfg["state_home"])
        if not home.is_absolute() or any(
            home.resolve().is_relative_to(p)
            for p in ("/tmp", "/var/tmp", "/private/tmp", "/private/var/tmp")
        ):
            raise ValueError
        cfg["state"] = str(
            home / "plugins" / "github" / f"{agent['id']}-{owner['id']}" / "state.json"
        )
        cfg["config_path"] = str(Path(path).absolute())
        return cfg
    except (OSError, ValueError, TypeError, KeyError, AttributeError) as error:
        raise Fault("config") from error


def read_state(cfg: dict[str, Any]) -> dict[str, Any]:
    path = Path(cfg["state"])
    _private_dir(path.parent)
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
        with os.fdopen(fd, "rb") as stream:
            _private(os.fstat(stream.fileno()), "state_private")
            raw = stream.read(cfg["max_state_bytes"] + 1)
        if len(raw) > cfg["max_state_bytes"]:
            raise Fault("state_capacity")
        data = json.loads(raw)
        required = {
            "version",
            "scope",
            "created_at",
            "feature_start",
            "feed",
            "row",
            "backoff",
            "last_tick",
            "fault",
        }
        if not isinstance(data, dict) or set(data) != required or data["version"] != 1:
            raise ValueError
        row = data["row"]
        if set(row) != set(CAPPED) | {
            "new_repo_cut",
            "mention_since",
            "trusted_since",
            "scope_skipped",
        }:
            raise ValueError
        if not all(
            isinstance(row[k], dict) for k in CAPPED if k != "hold"
        ) or not isinstance(row["hold"], (dict, type(None))):
            raise ValueError
        for value in (
            data["created_at"],
            data["feature_start"],
            row["new_repo_cut"],
            row["mention_since"],
            *row["trusted_since"].values(),
        ):
            stamp(value)
        if not _moment(data["backoff"]) or not _moment(data["feed"]["due"]):
            raise ValueError
        return data
    except FileNotFoundError as error:
        raise Fault("state_missing") from error
    except (OSError, ValueError, TypeError, KeyError, AttributeError) as error:
        raise Fault("state_corrupt") from error


def load_state(cfg: dict[str, Any]) -> dict[str, Any]:
    data = read_state(cfg)
    if data["scope"] != scope(cfg):
        raise Fault("scope_changed")
    if set(data["row"]["trusted_since"]) != {
        str(x) for x in cfg["trusted_actor_ids"]
    } | set(members(cfg)):
        raise Fault("state_corrupt")
    return data


def initialize(cfg: dict[str, Any], since: str) -> None:
    cutoff = stamp(since).isoformat()
    if stamp(cutoff) > datetime.now(UTC):
        raise Fault("future_cutoff")
    parent = Path(cfg["state"]).parent
    parent.mkdir(parents=True, mode=0o700, exist_ok=True)
    with locked(cfg):
        if os.path.lexists(cfg["state"]):
            raise Fault("state_exists")
        row = {name: {} for name in CAPPED}
        row.update(
            new_repo_cut=cutoff,
            mention_since=cutoff,
            trusted_since={
                str(k): cutoff for k in [*cfg["trusted_actor_ids"], *members(cfg)]
            },
            scope_skipped=0,
        )
        for key, entry in cfg["initial_repositories"].items():
            row["repos"][key] = dict(
                full_name=entry["full_name"],
                baseline=max(stamp(entry["baseline"]), stamp(cutoff)).isoformat(),
                status="admitted",
                listed=True,
            )
        save_state(
            cfg,
            dict(
                version=1,
                scope=scope(cfg),
                created_at=utc(),
                feature_start=cutoff,
                feed=dict(validator=None, last_complete=None, due=0, hold={}),
                row=row,
                backoff=0,
                last_tick=None,
                fault=None,
            ),
        )


def cycle(cfg: dict[str, Any]) -> dict[str, Any]:
    """One serialized bounded tick; state is never silently initialized."""
    with locked(cfg):
        state = load_state(cfg)
        if cfg["mode"] == "off":
            return dict(ok=False, detail="disabled")
        deadline = time.monotonic() + cfg["max_seconds"]
        state["fault"] = None
        api = API(cfg, deadline)
        plans: list[dict[str, Any]] = []
        try:
            wait = max(state["backoff"], reaction_cooldown(cfg), state["feed"]["due"])
            if time.time() < wait:
                return dict(ok=False, detail="waiting", due=wait)
            verify_account(api, cfg)
            collect(api, cfg, state)
            produce_brg(cfg, state, deadline, plans, api)
        except Fault as error:
            state["fault"] = error.kind
            state["backoff"] = max(state["backoff"], error.retry_at)
        finally:
            state["backoff"] = max(state["backoff"], api.retry_at)
            state["feed"]["due"] = max(
                state["feed"]["due"], time.time() + api.poll_seconds
            )
            state["last_tick"] = utc()
            persist(cfg, state)
        for plan in plans:
            send_reactions(cfg, plan, deadline)
        holds = bool(state["feed"].get("hold") or state["row"].get("hold"))
        ok = not state["fault"] and not holds
        return dict(
            ok=ok,
            detail=state["fault"] or ("held" if holds else "poll complete"),
            due=state["feed"]["due"],
        )


def health(result: dict[str, Any]) -> None:
    directory = Path(os.environ["BORGCHART_STATE_DIR"])
    _private_dir(directory)
    fd, name = tempfile.mkstemp(prefix=".health-", dir=directory)
    try:
        with os.fdopen(fd, "w") as stream:
            json.dump(dict(at=utc(), **result), stream)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(name, directory / "health.json")
    finally:
        if os.path.lexists(name):
            os.unlink(name)


def watch(path: str, stop: Callable[[], bool] = lambda: False) -> None:
    while not stop():
        delay = 300.0
        try:
            cfg = read_config(path)
            delay = cfg["poll_seconds"]
            result = cycle(cfg)
            delay = max(delay, result.get("due", 0) - time.time())
        except Exception as error:  # A plugin process is not automatically restarted.
            result = dict(
                ok=False,
                detail=error.kind if isinstance(error, Fault) else type(error).__name__,
            )
        try:
            health(result)
        except Exception as error:
            print(
                f"github-intake: health unavailable ({type(error).__name__})",
                file=sys.stderr,
            )
        if not result["ok"]:
            print(f"github-intake: {result['detail']}", file=sys.stderr)
        if not stop():
            time.sleep(delay)


def main() -> None:
    import argparse

    parser = argparse.ArgumentParser(description="GitHub pointer intake for brg")
    parser.add_argument("--config", default=os.environ.get("GITHUB_INTAKE_CONFIG"))
    subs = parser.add_subparsers(dest="command", required=True)
    init = subs.add_parser("init")
    init.add_argument("--since", required=True)
    subs.add_parser("watch")
    subs.add_parser("status")
    args = parser.parse_args()
    if not args.config:
        parser.error("set GITHUB_INTAKE_CONFIG or --config")
    try:
        if args.command == "watch":
            watch(args.config)
        else:
            cfg = read_config(args.config)
            if args.command == "init":
                initialize(cfg, args.since)
            else:
                state = load_state(cfg)
                print(
                    json.dumps(
                        dict(
                            last_tick=state["last_tick"],
                            fault=state["fault"],
                            pending=len(state["row"]["pending"]),
                            holds=state["row"]["hold"],
                            reactions=reaction_summary(cfg),
                        ),
                        sort_keys=True,
                    )
                )
    except (Fault, OSError) as error:
        print(f"github-intake: {error}", file=sys.stderr)
        raise SystemExit(1) from None
    except KeyboardInterrupt:
        pass
