# /// script
# requires-python = ">=3.11"
# ///
"""Read subscription quotas without model turns for one explicit vendor and model."""

from __future__ import annotations

import argparse
import fcntl
import hashlib
import json
import math
import os
import re
import selectors
import shutil
import signal
import stat
import subprocess
import tempfile
import time
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

JSON = dict[str, Any]
TTL = 120
# Cache format; change when normalized scopes or semantics change.
SCHEMA = 1
BACKOFF = 30
TIMEOUT = 20
MAX_OUTPUT = 4 * 1024 * 1024
CLAUDE_MODELS = {"fable", "opus", "sonnet"}
CODEX_CLASSES = {"astra", "sol", "terra", "luna"}
UNAVAILABLE = "usage unavailable; check subscription login and CLI compatibility, then retry after backoff"


def stamp(now: datetime) -> str:
    return now.astimezone(UTC).isoformat()


def parse_time(value: Any) -> datetime | None:
    try:
        if isinstance(value, (float, int)) and not isinstance(value, bool):
            return datetime.fromtimestamp(value, UTC)
        if isinstance(value, str):
            parsed = datetime.fromisoformat(value)
            return parsed.astimezone(UTC) if parsed.tzinfo else None
    except (ValueError, OverflowError, OSError):
        pass
    return None


def percent(value: Any) -> float | None:
    if (
        isinstance(value, (int, float))
        and not isinstance(value, bool)
        and math.isfinite(value)
        and value >= 0
    ):
        return value
    return None


def codex_class(model: str) -> str | None:
    match = re.fullmatch(r"(?:gpt-.+-)?(astra|sol|terra|luna)", model)
    return match[1] if match else None


def normalize(
    provider: str, data: Any, now: datetime, previous: JSON | None = None
) -> JSON:
    """Only typed, known quota fields cross the protocol/cache boundary."""
    try:
        result = _normalize(provider, data, now, previous)
        if result["included_allowed"] is None or not result["windows"]:
            result["status"] = "unknown"
        return result
    except (ValueError, TypeError, KeyError, AttributeError, OverflowError):
        return unknown(provider, now, (previous or {}).get("held_keys", []))


def _normalize(
    provider: str, data: JSON, now: datetime, previous: JSON | None = None
) -> JSON:
    result: JSON = {
        "schema_version": SCHEMA,
        "provider": provider,
        "source": "app-server" if provider == "codex" else "get_usage (experimental)",
        "status": "ok",
        "observed_at": stamp(now),
        "expires_at": stamp(now + timedelta(seconds=TTL)),
        "included_allowed": None,
        "windows": [],
        "known_scopes": [],
        "held_keys": [],
    }
    old_held = (previous or {}).get("held_keys", [])

    def window(
        key: str,
        scope: str,
        row: JSON,
        used_key: str,
        reset_key: str,
        duration: int | None = None,
    ) -> None:
        used = percent(row.get(used_key))
        reset = parse_time(row.get(reset_key))
        blocked = bool(row.get("locked_reason")) or row.get("severity") in {
            "blocked",
            "exceeded",
        }
        result["windows"].append(
            {
                "key": key,
                "scope": scope,
                "used_percent": used,
                "resets_at": stamp(reset) if reset else None,
                "window_seconds": duration,
                "blocked": blocked,
            }
        )
        if used is not None and (used >= 90 or (key in old_held and used >= 80)):
            result["held_keys"].append(key)
        if reset and reset > now:
            result["expires_at"] = stamp(min(parse_time(result["expires_at"]), reset))

    if provider == "codex":
        allowed = data.get("ordinaryUsageAllowed")
        result["included_allowed"] = allowed if isinstance(allowed, bool) else None
        buckets = data.get("rateLimitsByLimitId") or {}
        general = buckets.get("codex") or data.get("rateLimits") or {}
        if not isinstance(general, dict) or general.get("limitId") != "codex":
            return result
        result["timed_denial"] = (
            general.get("rateLimitReachedType") == "rate_limit_reached"
        )
        result["known_scopes"] = ["all"]
        for name, bucket in [("codex", general)] + [
            (key, value)
            for key, value in buckets.items()
            if key != "codex" and isinstance(value, dict)
        ]:
            # A model bucket is capacity for the class its slug belongs to.
            scope = (
                "all"
                if name == "codex"
                else codex_class(str(bucket.get("normalModelSlug") or ""))
            )
            if not scope:
                continue  # An unrelated quota alias is not capacity for the requested models.
            if bucket.get("spendControlReached") is True or bucket.get(
                "rateLimitReachedType"
            ) not in {None, "rate_limit_reached"}:
                result.setdefault("denied_scopes", []).append(scope)
            for label in ("primary", "secondary"):
                row = bucket.get(label)
                if row is not None and not isinstance(row, dict):
                    raise TypeError("invalid quota window")
                if isinstance(row, dict):
                    mins = row.get("windowDurationMins")
                    duration = (
                        mins * 60
                        if isinstance(mins, int)
                        and not isinstance(mins, bool)
                        and mins > 0
                        else None
                    )
                    window(
                        f"{'codex' if name == 'codex' else scope}.{label}",
                        scope,
                        row,
                        "usedPercent",
                        "resetsAt",
                        duration,
                    )
    elif provider == "claude":
        limits = data.get("rate_limits")
        if data.get("rate_limits_available") is not True or not isinstance(
            limits, dict
        ):
            return result
        if not isinstance(limits.get("seven_day"), dict):
            return result
        # The plan may carry no session window: then five_hour is null and the
        # limits list has no session row. Anything else is missing data.
        if not isinstance(limits.get("five_hour"), dict):
            rows = limits.get("limits")
            kinds = (
                [row.get("kind") for row in rows if isinstance(row, dict)]
                if isinstance(rows, list)
                else []
            )
            if "weekly_all" not in kinds or "session" in kinds:
                return result
        result["included_allowed"] = data.get("subscription_type") in {
            "pro",
            "max",
            "team",
            "enterprise",
        }
        result["known_scopes"] = ["all"]
        for key, scope, duration in (
            ("five_hour", "all", 18000),
            ("seven_day", "all", 604800),
            ("seven_day_opus", "opus", 604800),
            ("seven_day_sonnet", "sonnet", 604800),
        ):
            if key in limits and scope not in result["known_scopes"]:
                result["known_scopes"].append(scope)
            row = limits.get(key)
            if row is not None and not isinstance(row, dict):
                raise TypeError("invalid quota window")
            if isinstance(row, dict):
                window(key, scope, row, "utilization", "resets_at", duration)
        seen: set[str] = set()
        rows = limits.get("limits")
        if isinstance(rows, list):
            for row in rows:
                if not isinstance(row, dict):
                    continue
                scope = row.get("scope") or {}
                model = (scope.get("model") or {}).get("display_name")
                if row.get("kind") == "weekly_scoped" and isinstance(model, str):
                    model = model.lower()
                    if model in {"fable", "opus", "sonnet"} and not scope.get(
                        "surface"
                    ):
                        window(
                            f"model.{model}", model, row, "percent", "resets_at", 604800
                        )
                        seen.add(model)
                    elif row.get("is_active") is True:
                        result["unrecognized_constraint"] = True
                elif row.get("is_active") is True and row.get("kind") not in {
                    "session",
                    "weekly_all",
                }:
                    result["unrecognized_constraint"] = True
        for row in limits.get("model_scoped") or []:
            if not isinstance(row, dict):
                continue
            model = str(row.get("display_name", "")).lower()
            if model in {"fable", "opus", "sonnet"} and model not in seen:
                window(f"model.{model}", model, row, "utilization", "resets_at", 604800)
                seen.add(model)
        result["known_scopes"] = sorted(set(result["known_scopes"]) | seen)
        # Fable is never inferred eligible from a plan name, a hidden key, or an empty array.
        result["extra_usage_enabled"] = (limits.get("extra_usage") or {}).get(
            "is_enabled"
        ) is True
    else:
        raise ValueError("unsupported provider")
    known = {w["key"] for w in result["windows"] if w["used_percent"] is not None}
    result["held_keys"] = sorted(set(result["held_keys"]) | (set(old_held) - known))
    return result


def assess(route: JSON, snapshot: JSON, now: datetime) -> JSON:
    result: JSON = {
        "state": "unknown",
        "constraints": [],
        "warnings": [],
        "reason": UNAVAILABLE,
    }
    expiry = parse_time(snapshot.get("expires_at"))
    observed = parse_time(snapshot.get("observed_at"))
    if (
        snapshot.get("status") != "ok"
        or not expiry
        or expiry <= now
        or not observed
        or observed > now
        or expiry > observed + timedelta(seconds=TTL)
    ):
        return result
    model = route["model"]
    if route["kind"] == "claude" and model not in CLAUDE_MODELS:
        result["reason"] = "quota mapping is not known for this model"
        return result
    scope = model if route["kind"] == "claude" else "all"
    if route["kind"] == "codex":
        # A Codex route is assessed by class, whatever member it resolves to.
        model = codex_class(model)
        if not model:
            result["reason"] = "quota mapping is not known for this model"
            return result
    denied_usage = snapshot.get("included_allowed") is False
    if snapshot.get("included_allowed") is None or snapshot.get(
        "unrecognized_constraint"
    ):
        return result
    if scope not in snapshot.get("known_scopes", []):
        result["reason"] = f"{scope} quota scope is unknown"
        return result
    denied = set(snapshot.get("denied_scopes", [])) & {"all", model}
    if denied:
        return {
            **result,
            "state": "blocked",
            "reason": "backend reports a reached or spend-control limit",
        }
    windows = [w for w in snapshot.get("windows", []) if w["scope"] in {"all", model}]
    if not any(w["scope"] == "all" for w in windows) or any(
        w["used_percent"] is None for w in windows
    ):
        result["reason"] = "applicable quota window is missing utilization"
        return result
    constraints = [
        w
        for w in windows
        if w["blocked"]
        or w["used_percent"] >= 90
        or w["key"] in snapshot.get("held_keys", [])
    ]
    blocked = denied_usage or any(
        w["blocked"] or w["used_percent"] >= 100 for w in constraints
    )
    result = {
        "state": "blocked" if blocked else "available",
        "constraints": constraints,
        "warnings": []
        if blocked
        else [quota_warning(route, snapshot, w, now) for w in constraints],
        "reason": "backend denies included usage"
        if denied_usage
        else "included quota available"
        if not constraints
        else "applicable quota is constrained",
    }
    hard_constraints = [
        w for w in constraints if w["blocked"] or w["used_percent"] >= 100
    ]
    resets = [parse_time(w["resets_at"]) for w in hard_constraints]
    if (
        blocked
        and resets
        and all(reset and reset > now for reset in resets)
        and not any(w["blocked"] for w in constraints)
        and (not denied_usage or snapshot.get("timed_denial") is True)
    ):
        result["expected_recovery_at"] = stamp(max(resets))
    return result


def quota_warning(route: JSON, snapshot: JSON, window: JSON, now: datetime) -> JSON:
    reset = parse_time(window.get("resets_at"))
    remaining = max(0, (reset - now).total_seconds()) if reset else None
    scope = window["scope"]
    observed = snapshot.get("observed_at")
    reset_text = (
        f"reset {stamp(reset)}; {remaining:g} seconds remaining"
        if reset
        else "reset unknown; time remaining unknown"
    )
    held = "; hysteresis warning held below 90%" if window["used_percent"] < 90 else ""
    return {
        **{key: route[key] for key in ("kind", "model")},
        **{key: window[key] for key in ("key", "scope", "used_percent", "resets_at")},
        "observed_at": observed,
        "seconds_until_reset": remaining,
        "message": (
            f"{route['kind']} {route['model']}: "
            f"{window['used_percent']:g}% quota used; "
            f"scope {'shared (all)' if scope == 'all' else scope}/{window['key']}; "
            f"observed {observed or 'unknown'}; {reset_text}{held}; advisory warning"
        ),
    }


def protocol_read(provider: str, timeout: float = TIMEOUT) -> JSON:
    if provider not in {"claude", "codex"}:
        raise ValueError("unsupported provider")
    deadline = time.monotonic() + timeout
    if provider == "codex":
        argv = ["codex", "app-server", "--stdio"]
        requests = [
            {
                "id": 1,
                "method": "initialize",
                "params": {
                    "clientInfo": {"name": "borgchart_usage", "version": "1"},
                    "capabilities": {"experimentalApi": True},
                },
            },
            {
                "id": 2,
                "method": "account/rateLimits/read",
                "params": {"excludeResetCreditDetails": True},
            },
        ]
    else:
        argv = [
            "claude",
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--verbose",
            "--no-session-persistence",
            "--setting-sources",
            "",
            "--settings",
            '{"disableAllHooks":true}',
            "--strict-mcp-config",
            "--mcp-config",
            '{"mcpServers":{}}',
        ]
        requests = [
            {
                "type": "control_request",
                "request_id": "init",
                "request": {"subtype": "initialize"},
            },
            {
                "type": "control_request",
                "request_id": "usage",
                "request": {"subtype": "get_usage", "skip_behaviors": True},
            },
        ]
    env = {
        k: v
        for k, v in os.environ.items()
        if not k.startswith(("HERDR_", "BORGCHART_"))
        and k not in {"CODEX_THREAD_ID", "CLAUDECODE"}
    }
    process = subprocess.Popen(
        argv,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=subprocess.DEVNULL,
        env=env,
        cwd=tempfile.gettempdir(),
        start_new_session=True,
    )
    assert process.stdin is not None and process.stdout is not None
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    buffer = b""
    total = 0

    def send(request: JSON) -> None:
        process.stdin.write((json.dumps(request) + "\n").encode())
        process.stdin.flush()

    def receive(expected: int | str) -> JSON:
        nonlocal buffer, total
        while time.monotonic() < deadline:
            if b"\n" not in buffer:
                if not selector.select(
                    timeout=min(1, max(0, deadline - time.monotonic()))
                ):
                    continue
                chunk = os.read(process.stdout.fileno(), 65536)
                if not chunk:
                    raise ValueError("usage CLI exited without a response")
                total += len(chunk)
                buffer += chunk
                if total > MAX_OUTPUT:
                    raise ValueError("usage response exceeded the size limit")
            while b"\n" in buffer:
                if time.monotonic() >= deadline:
                    raise TimeoutError("usage read timed out")
                line, buffer = buffer.split(b"\n", 1)
                try:
                    data = json.loads(line)
                except (ValueError, UnicodeError, RecursionError):
                    raise ValueError("invalid usage response") from None
                if not isinstance(data, dict):
                    raise TypeError("invalid usage response")
                if provider == "codex" and data.get("id") == expected:
                    if "error" in data or not isinstance(data.get("result"), dict):
                        raise ValueError("Codex usage request failed")
                    return data["result"]
                if (
                    provider == "claude"
                    and data.get("type") == "control_response"
                    and isinstance(data.get("response"), dict)
                    and data["response"].get("request_id") == expected
                ):
                    response = data["response"]
                    if response.get("subtype") == "error" or not isinstance(
                        response.get("response"), dict
                    ):
                        raise ValueError("Claude usage request failed")
                    return response["response"]
        raise TimeoutError("usage read timed out")

    try:
        send(requests[0])
        receive(1 if provider == "codex" else "init")
        if provider == "codex":
            send({"method": "initialized"})
        send(requests[1])
        return receive(2 if provider == "codex" else "usage")
    finally:
        selector.close()
        # Kill the whole group even when its leader has already exited: descendants
        # can retain pipes or ignore TERM. Reaping is bounded too.
        try:
            os.killpg(process.pid, signal.SIGTERM)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=0.2)
        except subprocess.TimeoutExpired:
            pass
        try:
            os.killpg(process.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=0.5)
        except subprocess.TimeoutExpired:
            pass
        process.stdin.close()
        process.stdout.close()


def config_dir(provider: str) -> Path:
    return Path(
        os.environ.get(
            "CODEX_HOME" if provider == "codex" else "CLAUDE_CONFIG_DIR",
            str(Path.home() / (".codex" if provider == "codex" else ".claude")),
        )
    )


def claude_profiles() -> list[Path]:
    return [Path.home() / ".claude.json", config_dir("claude") / ".claude.json"]


def identity(provider: str) -> str:
    """Credentials are never opened: any stat change invalidates the reading."""
    config = config_dir(provider)
    binary = shutil.which(provider)
    markers: list[Any] = [provider, str(config.resolve()), binary]
    names = [
        config / name
        for name in (
            ("config.toml", "auth.json")
            if provider == "codex"
            else ("settings.json", ".credentials.json")
        )
    ]
    if binary:
        names.append(Path(binary))
    for path in names:
        try:
            info = path.stat()
            markers.append((str(path), info.st_ino, info.st_mtime_ns, info.st_size))
        except FileNotFoundError:
            markers.append((str(path), None))
    if provider == "claude":
        # Only account metadata contributes; routine utilization/UI writes do not.
        for profile in claude_profiles():
            account = read_profile(profile).get("oauthAccount", {})
            if isinstance(account, dict):
                markers.append(
                    [account.get("accountUuid"), account.get("organizationUuid")]
                )
    return hashlib.sha256(json.dumps(markers).encode()).hexdigest()


def claude_observed_at(data: JSON, now: datetime) -> datetime:
    # get_usage silently seeds failures from persisted utilization. Its only added
    # field is model_scoped; matching cached utilization is not a new observation.
    returned = {
        key: value
        for key, value in data.get("rate_limits", {}).items()
        if key != "model_scoped"
    }
    observed = now
    for path in claude_profiles():
        try:
            profile = read_profile(path)
        except FileNotFoundError:
            continue
        cached = profile.get("cachedUsageUtilization")
        if not isinstance(cached, dict) or cached.get("utilization") != returned:
            continue
        fetched_ms = percent(cached.get("fetchedAtMs"))
        fetched = parse_time(fetched_ms / 1000) if fetched_ms is not None else None
        if not fetched or fetched > now:
            raise ValueError("Claude cached observation time is invalid")
        observed = min(observed, fetched)
    if observed + timedelta(seconds=TTL) <= now:
        raise ValueError("Claude returned stale persisted usage")
    return observed


def read_profile(path: Path) -> JSON:
    """Read a local non-credential profile only for freshness/account metadata."""
    try:
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(fd, "rb") as handle:
            info = os.fstat(handle.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid():
                raise OSError("unsafe usage profile")
            raw = handle.read(MAX_OUTPUT + 1)
        if len(raw) > MAX_OUTPUT:
            raise ValueError("profile is too large")
        value = json.loads(raw)
        if not isinstance(value, dict):
            raise TypeError("invalid profile")
        return {
            key: value.get(key) for key in ("oauthAccount", "cachedUsageUtilization")
        }
    except FileNotFoundError:
        return {}


def unknown(provider: str, now: datetime, held: list[str] | None = None) -> JSON:
    return {
        "schema_version": SCHEMA,
        "provider": provider,
        "source": "app-server" if provider == "codex" else "get_usage (experimental)",
        "status": "unknown",
        "observed_at": stamp(now),
        "expires_at": stamp(now + timedelta(seconds=BACKOFF)),
        "held_keys": held or [],
        "error": UNAVAILABLE,
    }


def private_file(fd: int) -> None:
    info = os.fstat(fd)
    if (
        not stat.S_ISREG(info.st_mode)
        or info.st_uid != os.getuid()
        or stat.S_IMODE(info.st_mode) != 0o600
        or info.st_nlink != 1
    ):
        raise OSError("unsafe usage cache file")


def cache_directory(path: Path) -> int:
    # Refuse symlinked directories, including ancestors, before opening a dirfd.
    # All subsequent operations stay relative to that descriptor.
    for parent in [*reversed(path.absolute().parents), path.absolute()]:
        if parent.is_symlink():
            raise OSError("unsafe usage cache directory")
    path.mkdir(parents=True, exist_ok=True, mode=0o700)
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
    info = os.fstat(fd)
    if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
        os.close(fd)
        raise OSError("unsafe usage cache directory")
    return fd


def window_key(value: Any) -> bool:
    return isinstance(value, str) and bool(
        re.fullmatch(
            r"(?:codex|(?:astra|sol|terra|luna))\.(?:primary|secondary)"
            r"|five_hour|seven_day(?:_opus|_sonnet)?|model\.(?:fable|opus|sonnet)",
            value,
        )
    )


def valid_cache(value: Any, provider: str, now: datetime) -> bool:
    """Reject foreign schemas and arbitrary fields, not just old expiry times."""
    if not isinstance(value, dict) or set(value) - {
        "schema_version",
        "provider",
        "source",
        "status",
        "observed_at",
        "expires_at",
        "included_allowed",
        "windows",
        "known_scopes",
        "held_keys",
        "timed_denial",
        "denied_scopes",
        "unrecognized_constraint",
        "extra_usage_enabled",
        "error",
        "last_success_at",
    }:
        return False
    if (
        type(value.get("schema_version")) is not int
        or value["schema_version"] != SCHEMA
    ):
        return False
    if (
        value.get("provider") != provider
        or value.get("source") != unknown(provider, now)["source"]
    ):
        return False
    status = value.get("status")
    if status not in {"ok", "unknown"}:
        return False
    if not all(
        isinstance(value.get(key), str) for key in ("observed_at", "expires_at")
    ):
        return False
    if value.get("last_success_at") is not None and not isinstance(
        value["last_success_at"], str
    ):
        return False
    observed, expiry = (
        parse_time(value.get("observed_at")),
        parse_time(value.get("expires_at")),
    )
    if not observed or not expiry or observed > now or expiry <= observed:
        return False
    if expiry > observed + timedelta(seconds=TTL if status == "ok" else BACKOFF):
        return False
    last = parse_time(value.get("last_success_at"))
    if value.get("last_success_at") is not None and (not last or last > observed):
        return False
    if "error" in value and value["error"] != unknown(provider, now)["error"]:
        return False
    if not isinstance(value.get("held_keys"), list) or not all(
        window_key(k) for k in value["held_keys"]
    ):
        return False
    scopes = {"all", *CLAUDE_MODELS, *CODEX_CLASSES}
    for key in ("known_scopes", "denied_scopes"):
        if key in value and (
            not isinstance(value[key], list)
            or not all(isinstance(s, str) and s in scopes for s in value[key])
        ):
            return False
    for key in ("timed_denial", "unrecognized_constraint", "extra_usage_enabled"):
        if key in value and type(value[key]) is not bool:
            return False
    if (
        value.get("included_allowed") is not None
        and type(value["included_allowed"]) is not bool
    ):
        return False
    windows = value.get("windows", [])
    if not isinstance(windows, list):
        return False
    for window in windows:
        if not isinstance(window, dict) or set(window) != {
            "key",
            "scope",
            "used_percent",
            "resets_at",
            "window_seconds",
            "blocked",
        }:
            return False
        if (
            not window_key(window["key"])
            or not isinstance(window["scope"], str)
            or window["scope"] not in scopes
        ):
            return False
        if (
            window["used_percent"] is not None
            and percent(window["used_percent"]) is None
        ):
            return False
        if type(window["blocked"]) is not bool:
            return False
        if window["resets_at"] is not None and (
            not isinstance(window["resets_at"], str)
            or parse_time(window["resets_at"]) is None
        ):
            return False
        duration = window["window_seconds"]
        if duration is not None and (type(duration) is not int or duration <= 0):
            return False
    return status != "ok" or (
        value.get("included_allowed") is not None
        and bool(windows)
        and isinstance(value.get("known_scopes"), list)
    )


def read_cache(directory: int, name: str, provider: str, now: datetime) -> JSON:
    try:
        fd = os.open(
            name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=directory
        )
    except FileNotFoundError:
        return {}
    with os.fdopen(fd, "rb") as handle:
        private_file(handle.fileno())
        raw = handle.read(MAX_OUTPUT + 1)
    if len(raw) > MAX_OUTPUT:
        return {}
    try:
        value = json.loads(raw)
        return value if valid_cache(value, provider, now) else {}
    except (ValueError, TypeError, OverflowError, RecursionError):
        return {}


def write_cache(directory: int, name: str, result: JSON) -> None:
    temporary = f"usage-{os.getpid()}-{os.urandom(8).hex()}.tmp"
    fd = os.open(
        temporary,
        os.O_CREAT | os.O_EXCL | os.O_WRONLY | os.O_NOFOLLOW,
        0o600,
        dir_fd=directory,
    )
    try:
        with os.fdopen(fd, "w") as handle:
            json.dump(result, handle, allow_nan=False)
        os.replace(temporary, name, src_dir_fd=directory, dst_dir_fd=directory)
    finally:
        try:
            os.unlink(temporary, dir_fd=directory)
        except FileNotFoundError:
            pass


def snapshot(
    provider: str, refresh: bool = False, cache_dir: Path | None = None
) -> JSON:
    now = datetime.now(UTC)
    if provider not in {"claude", "codex"}:
        raise ValueError("unsupported provider")
    incompatible = (
        ("OPENAI_API_KEY", "OPENAI_BASE_URL")
        if provider == "codex"
        else (
            "ANTHROPIC_API_KEY",
            "ANTHROPIC_AUTH_TOKEN",
            "ANTHROPIC_BASE_URL",
            "CLAUDE_CODE_OAUTH_TOKEN",
            "CLAUDE_CODE_USE_BEDROCK",
            "CLAUDE_CODE_USE_VERTEX",
            "CLAUDE_CODE_USE_FOUNDRY",
        )
    )
    if any(os.environ.get(key) for key in incompatible):
        return unknown(provider, now)
    directory_path = (
        cache_dir
        or Path(os.environ.get("XDG_CACHE_HOME", str(Path.home() / ".cache")))
        / "borgchart/usage"
    )
    directory = None
    try:
        directory = cache_directory(directory_path)
        key = identity(provider)
        name = f"{key}.json"
        fd = os.open(
            f"{key}.lock",
            os.O_CREAT | os.O_RDWR | os.O_NOFOLLOW | os.O_NONBLOCK,
            0o600,
            dir_fd=directory,
        )
        with os.fdopen(fd, "w") as lock:
            private_file(lock.fileno())
            deadline = time.monotonic() + TIMEOUT + 2
            while True:
                try:
                    fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    break
                except BlockingIOError:
                    if time.monotonic() >= deadline:
                        return unknown(provider, datetime.now(UTC))
                    time.sleep(0.05)
            if identity(provider) != key:
                return unknown(provider, datetime.now(UTC))
            now = datetime.now(UTC)
            previous = read_cache(directory, name, provider, now)
            if identity(provider) != key:
                return unknown(provider, datetime.now(UTC))
            last_success = previous.get("last_success_at") or (
                previous.get("observed_at") if previous.get("status") == "ok" else None
            )
            expiry = parse_time(previous.get("expires_at"))
            # Even explicit refresh obeys the failure backoff.
            if (
                expiry
                and expiry > now
                and (not refresh or previous.get("status") == "unknown")
            ):
                return previous
            try:
                data = protocol_read(provider)
                observed = datetime.now(UTC)
                if provider == "claude":
                    observed = claude_observed_at(data, observed)
                prior = parse_time(last_success)
                if prior and observed < prior:
                    raise ValueError("usage observation regressed")
                result = normalize(provider, data, observed, previous)
                if identity(provider) != key or result["status"] != "ok":
                    raise ValueError("usage unavailable or identity changed")
            except (
                OSError,
                ValueError,
                TypeError,
                KeyError,
                AttributeError,
                TimeoutError,
                subprocess.SubprocessError,
                RecursionError,
            ):
                result = unknown(
                    provider, datetime.now(UTC), previous.get("held_keys", [])
                )
            result["last_success_at"] = (
                result["observed_at"] if result["status"] == "ok" else last_success
            )
            write_cache(directory, name, result)
            return result
    except (OSError, ValueError, TypeError, RecursionError):
        return unknown(provider, datetime.now(UTC))
    finally:
        if directory is not None:
            os.close(directory)


def choose(route: JSON, reading: JSON, now: datetime) -> JSON:
    evaluation = {"candidate": route, **assess(route, reading, now)}
    result = {
        "decision": "route"
        if evaluation["state"] == "available"
        else evaluation["state"],
        "evaluations": [evaluation],
        "warnings": evaluation["warnings"],
        "reason": evaluation["reason"],
        "observations": {
            route["kind"]: {
                key: reading.get(key)
                for key in ("status", "observed_at", "expires_at", "source")
            }
        },
    }
    if result["decision"] == "route":
        result["route"] = route
    return result


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    route = commands.add_parser("route")
    route.add_argument("kind", choices=("claude", "codex"))
    route.add_argument("model")
    route.add_argument("--refresh", action="store_true")
    args = parser.parse_args()
    if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9._-]{0,127}", args.model):
        parser.error("model must be a single model identifier")
    reading = snapshot(args.kind, args.refresh)
    result = choose(
        {"kind": args.kind, "model": args.model}, reading, datetime.now(UTC)
    )
    print(json.dumps(result, allow_nan=False))
    return 0 if result["decision"] == "route" else 3


if __name__ == "__main__":
    raise SystemExit(main())
