"""Subscription reader tests: isolated homes, fake executables, no live probes."""

from __future__ import annotations

import ast
import importlib.util
import json
import os
import stat
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any
from unittest.mock import Mock

import pytest

PLUGIN = (
    Path(__file__).resolve().parents[2]
    / "borgchart-engineering/teams/dev-team/plugins/seat"
)
spec = importlib.util.spec_from_file_location("quota_usage", PLUGIN / "usage.py")
assert spec and spec.loader
usage = importlib.util.module_from_spec(spec)
sys.dont_write_bytecode = True
spec.loader.exec_module(usage)
NOW = datetime(2026, 1, 1, tzinfo=UTC)


def codex(used: Any = 10, now: datetime = NOW) -> dict[str, Any]:
    return {
        "ordinaryUsageAllowed": True,
        "rateLimits": {
            "limitId": "codex",
            "primary": {
                "usedPercent": used,
                "windowDurationMins": 10080,
                "resetsAt": (now + timedelta(days=1)).timestamp(),
            },
        },
    }


def claude(used: Any = 10, now: datetime = NOW) -> dict[str, Any]:
    reset = (now + timedelta(days=1)).isoformat()
    return {
        "subscription_type": "max",
        "rate_limits_available": True,
        "rate_limits": {
            "five_hour": {"utilization": used, "resets_at": reset},
            "seven_day": {"utilization": used, "resets_at": reset},
            "seven_day_opus": None,
            "seven_day_sonnet": None,
            "model_scoped": [
                {"display_name": "Fable", "utilization": used, "resets_at": reset}
            ],
        },
    }


@pytest.fixture(autouse=True)
def isolation(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Mock:
    # No inherited vendor configuration, credentials, caches, or executable paths.
    for key in list(os.environ):
        monkeypatch.delenv(key)
    for name in ("bin", "codex", "claude", "config", "cache"):
        (tmp_path / name).mkdir()
    for key, value in {
        "HOME": str(tmp_path),
        "PATH": str(tmp_path / "bin"),
        "CODEX_HOME": str(tmp_path / "codex"),
        "CLAUDE_CONFIG_DIR": str(tmp_path / "claude"),
        "XDG_CONFIG_HOME": str(tmp_path / "config"),
        "XDG_CACHE_HOME": str(tmp_path / "cache"),
    }.items():
        monkeypatch.setenv(key, value)
    probe = Mock(side_effect=AssertionError("unit test attempted a vendor probe"))
    monkeypatch.setattr(usage, "protocol_read", probe)
    return probe


@pytest.mark.parametrize(
    "vendor,model",
    [
        ("codex", "astra"),
        ("codex", "gpt-6.1-sol"),
        ("claude", "fable"),
        ("claude", "opus"),
        ("claude", "sonnet"),
    ],
)
@pytest.mark.parametrize(
    "used,decision",
    [
        (89, "route"),
        (90, "route"),
        (99.9, "route"),
        (100, "blocked"),
        (None, "unknown"),
        (True, "unknown"),
        (-1, "unknown"),
        (float("nan"), "unknown"),
        (float("inf"), "unknown"),
    ],
)
def test_single_candidate_thresholds(
    vendor: str, model: str, used: Any, decision: str
) -> None:
    data = codex(used) if vendor == "codex" else claude(used)
    snapshot = usage.normalize(vendor, data, NOW)
    route = {"kind": vendor, "model": model}
    result = usage.choose(route, snapshot, NOW)
    assert result["decision"] == decision
    assert len(result["evaluations"]) == 1
    assert result["evaluations"][0]["candidate"] == route
    assert result["warnings"] == result["evaluations"][0]["warnings"]
    if decision == "route":
        assert result["route"] == route
        assert bool(result["warnings"]) == (used >= 90)
    else:
        assert "route" not in result


@pytest.mark.parametrize("vendor,model", [("codex", "unknown"), ("claude", "unknown")])
def test_unknown_model_waits(vendor: str, model: str) -> None:
    data = codex() if vendor == "codex" else claude()
    assert (
        usage.choose(
            {"kind": vendor, "model": model}, usage.normalize(vendor, data, NOW), NOW
        )["decision"]
        == "unknown"
    )


def test_hysteresis_and_recovery() -> None:
    previous = usage.normalize("codex", codex(95), NOW)
    for used, held in [(85, True), (80, True), (79, False)]:
        previous = usage.normalize("codex", codex(used), NOW, previous)
        assert bool(previous["held_keys"]) is held
    blocked = usage.choose(
        {"kind": "codex", "model": "terra"},
        usage.normalize("codex", codex(100), NOW),
        NOW,
    )
    assert (
        blocked["evaluations"][0]["expected_recovery_at"]
        == (NOW + timedelta(days=1)).isoformat()
    )


@pytest.mark.parametrize(
    "data",
    [
        None,
        [],
        {"rateLimitsByLimitId": []},
        {"ordinaryUsageAllowed": True, "rateLimitsByLimitId": "invalid"},
    ],
)
def test_malformed_is_unknown(data: Any) -> None:
    snapshot = usage.normalize("codex", data, NOW)
    assert (
        usage.choose({"kind": "codex", "model": "sol"}, snapshot, NOW)["decision"]
        == "unknown"
    )


def test_scoped_codex_limits_and_no_payload_leak() -> None:
    data = codex()
    data["rateLimitsByLimitId"] = {
        "codex": data["rateLimits"],
        "PRIVATE_BUCKET_NAME": {
            "normalModelSlug": "gpt-6-sol",
            "primary": {
                "usedPercent": 100,
                "resetsAt": (NOW + timedelta(hours=1)).timestamp(),
            },
        },
    }
    snapshot = usage.normalize("codex", data, NOW)
    assert "PRIVATE_BUCKET_NAME" not in json.dumps(snapshot)
    assert (
        usage.choose({"kind": "codex", "model": "gpt-6.1-sol"}, snapshot, NOW)[
            "decision"
        ]
        == "blocked"
    )
    assert (
        usage.choose({"kind": "codex", "model": "terra"}, snapshot, NOW)["decision"]
        == "route"
    )


def test_claude_sessionless_and_unknown_constraint() -> None:
    data = claude()
    data["rate_limits"]["five_hour"] = None
    assert (
        usage.choose(
            {"kind": "claude", "model": "opus"},
            usage.normalize("claude", data, NOW),
            NOW,
        )["decision"]
        == "unknown"
    )
    data["rate_limits"]["limits"] = [{"kind": "weekly_all"}]
    assert (
        usage.choose(
            {"kind": "claude", "model": "opus"},
            usage.normalize("claude", data, NOW),
            NOW,
        )["decision"]
        == "route"
    )
    data["rate_limits"]["limits"].append({"kind": "unknown", "is_active": True})
    assert (
        usage.choose(
            {"kind": "claude", "model": "opus"},
            usage.normalize("claude", data, NOW),
            NOW,
        )["decision"]
        == "unknown"
    )


def test_stdlib_script() -> None:
    source = (PLUGIN / "usage.py").read_text()
    assert '# requires-python = ">=3.11"' in source
    assert "dependencies =" not in source
    for node in ast.walk(ast.parse(source)):
        if isinstance(node, ast.Import):
            assert all(
                alias.name.split(".")[0] in sys.stdlib_module_names
                for alias in node.names
            )
        if isinstance(node, ast.ImportFrom):
            assert node.module.split(".")[0] in sys.stdlib_module_names


def test_cache_private_and_failure_backoff(tmp_path: Path, isolation: Mock) -> None:
    isolation.side_effect = None
    isolation.return_value = codex(94, datetime.now(UTC))
    first = usage.snapshot("codex")
    assert first["status"] == "ok"
    assert usage.snapshot("codex") == first
    assert isolation.call_count == 1
    directory = tmp_path / "cache/borgchart/usage"
    assert stat.S_IMODE(directory.stat().st_mode) == 0o700
    for path in directory.iterdir():
        assert stat.S_IMODE(path.stat().st_mode) == 0o600
    isolation.side_effect = ValueError("DO_NOT_LOG_PROVIDER_ERROR")
    failed = usage.snapshot("codex", refresh=True)
    assert failed["status"] == "unknown"
    assert failed["held_keys"] == first["held_keys"]
    assert usage.snapshot("codex", refresh=True) == failed
    assert isolation.call_count == 2
    assert "DO_NOT_LOG" not in json.dumps(failed)


@pytest.mark.parametrize(
    "vendor,credential", [("codex", "auth.json"), ("claude", ".credentials.json")]
)
def test_credentials_stat_only_and_rotation(
    tmp_path: Path,
    monkeypatch: pytest.MonkeyPatch,
    isolation: Mock,
    vendor: str,
    credential: str,
) -> None:
    path = tmp_path / vendor / credential
    path.write_text("fake credential; must never be read")
    original = Path.open

    def guarded_open(self: Path, *args: Any, **kwargs: Any) -> Any:
        assert self.name not in {"auth.json", ".credentials.json"}
        return original(self, *args, **kwargs)

    monkeypatch.setattr(Path, "open", guarded_open)
    first = usage.identity(vendor)
    os.utime(path, ns=(1, 1))
    assert usage.identity(vendor) != first

    def rotate(_: str) -> dict[str, Any]:
        os.utime(path, ns=(2, 2))
        return (
            codex(now=datetime.now(UTC))
            if vendor == "codex"
            else claude(now=datetime.now(UTC))
        )

    isolation.side_effect = rotate
    assert usage.snapshot(vendor)["status"] == "unknown"
    isolation.side_effect = None
    isolation.return_value = (
        codex(now=datetime.now(UTC))
        if vendor == "codex"
        else claude(now=datetime.now(UTC))
    )
    assert usage.snapshot(vendor)["status"] == "ok"


def test_concurrent_cache_serializes(isolation: Mock) -> None:
    def read(_: str) -> dict[str, Any]:
        time.sleep(0.05)
        return codex(now=datetime.now(UTC))

    isolation.side_effect = read
    with ThreadPoolExecutor(max_workers=3) as pool:
        results = list(pool.map(lambda _: usage.snapshot("codex"), range(3)))
    assert isolation.call_count == 1
    assert results[0] == results[1] == results[2]


@pytest.mark.parametrize(
    "unsafe", ["symlink", "permissions", "schema", "future", "extra"]
)
def test_unsafe_cache_never_authorizes(
    tmp_path: Path, isolation: Mock, unsafe: str
) -> None:
    isolation.side_effect = None
    isolation.return_value = codex(now=datetime.now(UTC))
    usage.snapshot("codex")
    path = next((tmp_path / "cache/borgchart/usage").glob("*.json"))
    if unsafe == "symlink":
        target = tmp_path / "private"
        path.rename(target)
        path.symlink_to(target)
    elif unsafe == "permissions":
        path.chmod(0o644)
    else:
        data = json.loads(path.read_text())
        if unsafe == "schema":
            data["schema_version"] = 999
        elif unsafe == "future":
            data["observed_at"] = (datetime.now(UTC) + timedelta(days=1)).isoformat()
        else:
            data["raw_secret"] = "DO_NOT_LOG"
        path.write_text(json.dumps(data))
    isolation.side_effect = ValueError("probe unavailable")
    result = usage.snapshot("codex")
    assert result["status"] == "unknown"
    assert "DO_NOT_LOG" not in json.dumps(result)


# Save the implementation before the autouse fixture installs the probe guard.
PROTOCOL_READ = usage.protocol_read


@pytest.fixture
def fake_cli(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    source = (Path(__file__).parent / "support/usage_cli.py").read_text()
    for vendor in ("claude", "codex"):
        path = tmp_path / "bin" / vendor
        path.write_text(f"#!{sys.executable}\n" + source)
        path.chmod(0o700)
    monkeypatch.setenv("USAGE_TEST_GUARD", "isolated-fake")
    monkeypatch.setenv("USAGE_TEST_LOG", str(tmp_path / "calls"))
    monkeypatch.setenv("USAGE_TEST_RESPONSE", json.dumps(codex(now=datetime.now(UTC))))
    monkeypatch.setattr(usage, "protocol_read", PROTOCOL_READ)
    return tmp_path / "calls"


@pytest.mark.parametrize("vendor", ["claude", "codex"])
def test_actual_protocol_exchange(
    fake_cli: Path, monkeypatch: pytest.MonkeyPatch, vendor: str
) -> None:
    data = (
        codex(now=datetime.now(UTC))
        if vendor == "codex"
        else claude(now=datetime.now(UTC))
    )
    monkeypatch.setenv("USAGE_TEST_RESPONSE", json.dumps(data))
    monkeypatch.setenv("BORGCHART_TEST_PRIVATE", "must be scrubbed")
    monkeypatch.setenv("HERDR_TEST_PRIVATE", "must be scrubbed")
    monkeypatch.setenv("CLAUDECODE", "must be scrubbed")
    monkeypatch.setenv("CODEX_THREAD_ID", "must be scrubbed")
    assert usage.protocol_read(vendor, timeout=2) == data
    calls = [json.loads(line) for line in fake_cli.read_text().splitlines()]
    if vendor == "codex":
        assert [call.get("method") for call in calls] == [
            "initialize",
            "initialized",
            "account/rateLimits/read",
        ]
        assert calls[-1]["params"] == {"excludeResetCreditDetails": True}
    else:
        assert [call["request"]["subtype"] for call in calls] == [
            "initialize",
            "get_usage",
        ]
        assert calls[-1]["request"]["skip_behaviors"] is True


@pytest.mark.parametrize(
    "mode", ["error", "garbage", "eof", "timeout", "flood", "notifications"]
)
def test_protocol_failure_is_bounded(
    fake_cli: Path, monkeypatch: pytest.MonkeyPatch, mode: str
) -> None:
    monkeypatch.setenv("USAGE_TEST_MODE", mode)
    start = time.monotonic()
    with pytest.raises((ValueError, TimeoutError)):
        usage.protocol_read("codex", timeout=0.3)
    assert time.monotonic() - start < 3


def test_cli_contract(fake_cli: Path) -> None:
    completed = subprocess.run(
        [sys.executable, str(PLUGIN / "usage.py"), "route", "codex", "gpt-6.1-sol"],
        capture_output=True,
        text=True,
        timeout=5,
        check=False,
    )
    assert completed.returncode == 0, completed.stderr
    result = json.loads(completed.stdout)
    assert result["route"] == {"kind": "codex", "model": "gpt-6.1-sol"}
    assert len(result["evaluations"]) == 1
    assert result["observations"]["codex"]["source"] == "app-server"


def test_scoped_hysteresis_survives_bucket_order_changes() -> None:
    data = codex()
    bucket = {"normalModelSlug": "gpt-6-sol", "primary": {"usedPercent": 94}}
    data["rateLimitsByLimitId"] = {"model": bucket}
    previous = usage.normalize("codex", data, NOW)
    bucket["primary"]["usedPercent"] = 85
    data["rateLimitsByLimitId"] = {
        "unrelated": {"normalModelSlug": "unknown"},
        "model": bucket,
    }
    current = usage.normalize("codex", data, NOW, previous)
    result = usage.choose({"kind": "codex", "model": "sol"}, current, NOW)
    assert result["warnings"], "reordering buckets must not release a held warning"


@pytest.mark.parametrize("vendor", ["claude", "codex"])
def test_malformed_applicable_window_does_not_disappear(vendor: str) -> None:
    data = codex() if vendor == "codex" else claude()
    if vendor == "codex":
        data["rateLimits"]["secondary"] = "invalid"
    else:
        data["rate_limits"]["seven_day_opus"] = "invalid"
    snapshot = usage.normalize(vendor, data, NOW)
    assert (
        usage.choose(
            {"kind": vendor, "model": "sol" if vendor == "codex" else "opus"},
            snapshot,
            NOW,
        )["decision"]
        == "unknown"
    )


def test_claude_typed_scope_and_shared_scope() -> None:
    data = claude()
    data["rate_limits"]["limits"] = [
        {
            "kind": "weekly_scoped",
            "is_active": True,
            "scope": {"model": {"display_name": "Fable"}},
            "percent": 100,
            "resets_at": (NOW + timedelta(hours=1)).isoformat(),
        }
    ]
    snapshot = usage.normalize("claude", data, NOW)
    assert (
        usage.assess({"kind": "claude", "model": "fable"}, snapshot, NOW)["state"]
        == "blocked"
    )
    assert (
        usage.assess({"kind": "claude", "model": "opus"}, snapshot, NOW)["state"]
        == "available"
    )
    data["rate_limits"]["seven_day"]["utilization"] = 100
    assert (
        usage.assess(
            {"kind": "claude", "model": "opus"},
            usage.normalize("claude", data, NOW),
            NOW,
        )["state"]
        == "blocked"
    )


@pytest.mark.parametrize("reset", [None, "garbage", "2025-01-01T00:00:00+00:00"])
def test_bad_reset_never_promises_recovery(reset: Any) -> None:
    data = codex(100)
    data["rateLimits"]["primary"]["resetsAt"] = reset
    result = usage.assess(
        {"kind": "codex", "model": "sol"}, usage.normalize("codex", data, NOW), NOW
    )
    assert result["state"] == "blocked"
    assert "expected_recovery_at" not in result


def test_backend_denial_and_latest_hard_reset() -> None:
    data = codex(100)
    data["ordinaryUsageAllowed"] = False
    result = usage.assess(
        {"kind": "codex", "model": "sol"}, usage.normalize("codex", data, NOW), NOW
    )
    assert result["state"] == "blocked" and "expected_recovery_at" not in result
    data["rateLimits"]["rateLimitReachedType"] = "rate_limit_reached"
    data["rateLimits"]["secondary"] = {
        "usedPercent": 100,
        "resetsAt": (NOW + timedelta(days=2)).timestamp(),
    }
    result = usage.assess(
        {"kind": "codex", "model": "sol"}, usage.normalize("codex", data, NOW), NOW
    )
    assert result["expected_recovery_at"] == (NOW + timedelta(days=2)).isoformat()
    data["rateLimits"]["spendControlReached"] = True
    result = usage.assess(
        {"kind": "codex", "model": "sol"}, usage.normalize("codex", data, NOW), NOW
    )
    assert result["state"] == "blocked" and "expected_recovery_at" not in result


def write_profile(tmp_path: Path, data: dict[str, Any], observed: datetime) -> None:
    (tmp_path / ".claude.json").write_text(
        json.dumps(
            {
                "oauthAccount": {
                    "accountUuid": "fake-account",
                    "organizationUuid": "fake-organization",
                },
                "private_profile_field": "PRIVATE_PROFILE_DATA",
                "cachedUsageUtilization": {
                    "fetchedAtMs": observed.timestamp() * 1000,
                    "utilization": {
                        k: v
                        for k, v in data["rate_limits"].items()
                        if k != "model_scoped"
                    },
                },
            }
        )
    )


def test_profile_freshness_regression_and_held_warning(
    tmp_path: Path, isolation: Mock
) -> None:
    now = datetime.now(UTC)
    data = claude(94, now)
    write_profile(tmp_path, data, now - timedelta(seconds=10))
    isolation.side_effect = None
    isolation.return_value = data
    first = usage.snapshot("claude")
    assert first["status"] == "ok"
    assert usage.parse_time(first["observed_at"]) < now - timedelta(seconds=9)
    data = claude(20, now)
    write_profile(tmp_path, data, now - timedelta(seconds=60))
    isolation.return_value = data
    failed = usage.snapshot("claude", refresh=True)
    assert failed["status"] == "unknown"
    assert failed["held_keys"] == first["held_keys"]
    assert failed["last_success_at"] == first["observed_at"]
    assert "PRIVATE_PROFILE_DATA" not in json.dumps(failed)
    cache = next((tmp_path / "cache/borgchart/usage").glob("*.json"))
    stored = json.loads(cache.read_text())
    # Age the failure record beyond backoff while retaining the last success.
    stored["observed_at"] = (now - timedelta(seconds=40)).isoformat()
    stored["expires_at"] = (now - timedelta(seconds=10)).isoformat()
    # Keep timestamps consistent: the last success precedes the failed read.
    stored["last_success_at"] = (now - timedelta(seconds=45)).isoformat()
    cache.write_text(json.dumps(stored))
    retried = usage.snapshot("claude", refresh=True)
    assert retried["status"] == "unknown"
    assert retried["held_keys"] == first["held_keys"]


@pytest.mark.parametrize("age", [1800, -5])
def test_stale_or_future_claude_seed_is_unknown(
    tmp_path: Path, isolation: Mock, age: int
) -> None:
    now = datetime.now(UTC)
    data = claude(now=now)
    write_profile(tmp_path, data, now - timedelta(seconds=age))
    isolation.side_effect = None
    isolation.return_value = data
    assert usage.snapshot("claude")["status"] == "unknown"


def test_profile_usage_updates_do_not_change_account_identity(tmp_path: Path) -> None:
    now = datetime.now(UTC)
    write_profile(tmp_path, claude(20, now), now)
    before = usage.identity("claude")
    write_profile(tmp_path, claude(90, now), now)
    assert usage.identity("claude") == before
    profile = json.loads((tmp_path / ".claude.json").read_text())
    profile["oauthAccount"]["accountUuid"] = "second-fake-account"
    (tmp_path / ".claude.json").write_text(json.dumps(profile))
    assert usage.identity("claude") != before


@pytest.mark.parametrize(
    "key,vendor",
    [
        ("OPENAI_API_KEY", "codex"),
        ("OPENAI_BASE_URL", "codex"),
        ("ANTHROPIC_API_KEY", "claude"),
        ("CLAUDE_CODE_USE_BEDROCK", "claude"),
    ],
)
def test_incompatible_auth_never_probes(
    monkeypatch: pytest.MonkeyPatch, isolation: Mock, key: str, vendor: str
) -> None:
    monkeypatch.setenv(key, "fake-override")
    assert usage.snapshot(vendor)["status"] == "unknown"
    isolation.assert_not_called()


def test_binary_and_settings_invalidate_identity(tmp_path: Path) -> None:
    before = usage.identity("codex")
    binary = tmp_path / "bin/codex"
    binary.write_text("fake")
    binary.chmod(0o700)
    after = usage.identity("codex")
    assert after != before
    binary.write_text("fake changed")
    assert usage.identity("codex") != after
    before = usage.identity("codex")
    (tmp_path / "codex/config.toml").write_text("# fake settings")
    assert usage.identity("codex") != before


@pytest.mark.parametrize(
    "mode", ["directory-link", "lock-link", "lock-mode", "directory-mode"]
)
def test_unsafe_cache_paths_never_probe(
    tmp_path: Path, isolation: Mock, mode: str
) -> None:
    directory = tmp_path / "cache/borgchart/usage"
    directory.mkdir(parents=True, mode=0o700)
    if mode == "directory-link":
        directory.rmdir()
        directory.symlink_to(tmp_path)
    elif mode == "directory-mode":
        directory.chmod(0o755)
    else:
        lock = directory / f"{usage.identity('codex')}.lock"
        if mode == "lock-link":
            lock.symlink_to(tmp_path / "do-not-create")
        else:
            lock.touch(mode=0o644)
            lock.chmod(0o644)
    assert usage.snapshot("codex")["status"] == "unknown"
    isolation.assert_not_called()
    assert not (tmp_path / "do-not-create").exists()


@pytest.mark.parametrize(
    "args,code",
    [
        ([], 2),
        (["route", "auto", "sol"], 2),
        (["route", "codex"], 2),
        (["route", "codex", "sol", "high"], 2),
        (["route", "claude", "unknown"], 3),
    ],
)
def test_cli_invalid_arguments_or_unknown_model(
    fake_cli: Path, args: list[str], code: int
) -> None:
    completed = subprocess.run(
        [sys.executable, str(PLUGIN / "usage.py"), *args],
        check=False,
        capture_output=True,
        text=True,
        timeout=5,
    )
    assert completed.returncode == code
    if code == 2:
        assert not fake_cli.exists()
    else:
        assert json.loads(completed.stdout)["decision"] == "unknown"


@pytest.mark.parametrize("vendor", ["claude", "codex"])
def test_protocol_errors_never_leak_cli_payload(
    fake_cli: Path, monkeypatch: pytest.MonkeyPatch, vendor: str
) -> None:
    monkeypatch.setenv("USAGE_TEST_MODE", "error")
    result = usage.snapshot(vendor)
    assert result["status"] == "unknown"
    assert "PRIVATE" not in json.dumps(result)
    calls = fake_cli.read_text()
    assert usage.snapshot(vendor, refresh=True) == result
    assert fake_cli.read_text() == calls


def test_total_output_bound_counts_notifications(
    fake_cli: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("USAGE_TEST_MODE", "notifications")
    with pytest.raises(ValueError, match="size limit"):
        usage.protocol_read("codex", timeout=5)


def test_total_deadline_includes_initialization(
    fake_cli: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("USAGE_TEST_MODE", "slow")
    start = time.monotonic()
    with pytest.raises(TimeoutError):
        usage.protocol_read("codex", timeout=0.3)
    assert time.monotonic() - start < 1.1


@pytest.mark.skipif(not Path("/proc").is_dir(), reason="requires Linux process state")
def test_descendant_cleanup_when_leader_exits(
    fake_cli: Path, monkeypatch: pytest.MonkeyPatch, tmp_path: Path
) -> None:
    import signal

    pidfile = tmp_path / "child-pid"
    monkeypatch.setenv("USAGE_TEST_MODE", "child")
    monkeypatch.setenv("USAGE_TEST_CHILD", str(pidfile))
    usage.protocol_read("codex", timeout=2)
    pid = int(pidfile.read_text())
    try:
        for _ in range(50):
            process = Path(f"/proc/{pid}/stat")
            if not process.exists() or process.read_text().split()[2] == "Z":
                break
            time.sleep(0.01)
        else:
            pytest.fail("quota CLI descendant survived process-group cleanup")
    finally:
        try:
            os.kill(pid, signal.SIGKILL)
        except ProcessLookupError:
            pass


def test_deeply_nested_response_is_safe_unknown(
    fake_cli: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("USAGE_TEST_MODE", "nested")
    result = usage.snapshot("codex")
    assert result["status"] == "unknown"


def test_profile_symlink_is_not_read(tmp_path: Path, isolation: Mock) -> None:
    (tmp_path / "codex/auth.json").write_text('{"fake": "credential"}')
    (tmp_path / ".claude.json").symlink_to(tmp_path / "codex/auth.json")
    assert usage.snapshot("claude")["status"] == "unknown"
    isolation.assert_not_called()


def test_launcher_exact_arguments(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    fake_uv = tmp_path / "bin/uv"
    fake_uv.write_text(
        f"#!{sys.executable}\nimport json, sys\nprint(json.dumps(sys.argv[1:]))\n"
    )
    fake_uv.chmod(0o700)
    # dirname is the launcher's only shell utility. Supply a fake, absolute-interpreter one.
    dirname = tmp_path / "bin/dirname"
    dirname.write_text(
        f"#!{sys.executable}\nimport os, sys\nprint(os.path.dirname(sys.argv[1]))\n"
    )
    dirname.chmod(0o700)
    completed = subprocess.run(
        [str(PLUGIN / "usage-route"), "codex", "gpt-6.1-sol"],
        check=False,
        capture_output=True,
        text=True,
        timeout=5,
    )
    assert completed.returncode == 0
    assert json.loads(completed.stdout) == [
        "run",
        "--quiet",
        "--no-project",
        "--script",
        str(PLUGIN / "usage.py"),
        "route",
        "codex",
        "gpt-6.1-sol",
    ]


def test_identity_change_while_reading_cache_is_unknown(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, isolation: Mock
) -> None:
    isolation.side_effect = None
    isolation.return_value = codex(now=datetime.now(UTC))
    assert usage.snapshot("codex")["status"] == "ok"
    original = usage.read_cache

    def change_identity(*args: Any) -> dict[str, Any]:
        result = original(*args)
        (tmp_path / "codex/auth.json").write_text("fake replacement")
        return result

    monkeypatch.setattr(usage, "read_cache", change_identity)
    assert usage.snapshot("codex")["status"] == "unknown"
    assert isolation.call_count == 1


def test_cache_lock_deadline(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, isolation: Mock
) -> None:
    import fcntl

    directory = tmp_path / "cache/borgchart/usage"
    directory.mkdir(parents=True, mode=0o700)
    lock = directory / f"{usage.identity('codex')}.lock"
    lock.touch(mode=0o600)
    monkeypatch.setattr(usage, "TIMEOUT", -1.95)
    with lock.open("w") as handle:
        fcntl.flock(handle, fcntl.LOCK_EX)
        start = time.monotonic()
        assert usage.snapshot("codex")["status"] == "unknown"
        assert time.monotonic() - start < 1
    isolation.assert_not_called()


def test_explicit_refreshes_are_serialized(isolation: Mock) -> None:
    import threading

    active = 0
    highest = 0
    guard = threading.Lock()

    def read(_: str) -> dict[str, Any]:
        nonlocal active, highest
        with guard:
            active += 1
            highest = max(highest, active)
        time.sleep(0.03)
        with guard:
            active -= 1
        return codex(now=datetime.now(UTC))

    isolation.side_effect = read
    with ThreadPoolExecutor(max_workers=3) as pool:
        results = list(
            pool.map(lambda _: usage.snapshot("codex", refresh=True), range(3))
        )
    assert highest == 1
    assert all(result["status"] == "ok" for result in results)


def test_same_class_warning_does_not_spill_below_release_threshold() -> None:
    data = codex()
    data["rateLimitsByLimitId"] = {
        "first": {"normalModelSlug": "gpt-6-sol", "primary": {"usedPercent": 95}},
        "second": {"normalModelSlug": "gpt-6.1-sol", "primary": {"usedPercent": 50}},
    }
    current = usage.normalize("codex", data, NOW)
    result = usage.choose({"kind": "codex", "model": "sol"}, current, NOW)
    assert result["decision"] == "route"
    assert [row["used_percent"] for row in result["warnings"]] == [95]
    data["rateLimitsByLimitId"]["first"]["primary"]["usedPercent"] = 79
    current = usage.normalize("codex", data, NOW, current)
    assert not usage.choose({"kind": "codex", "model": "sol"}, current, NOW)["warnings"]
