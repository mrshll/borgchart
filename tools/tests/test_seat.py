"""Quota adapter tests; all usage sources here are fake."""

import importlib.util
import json
import os
import subprocess
import sys
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

import pytest

PLUGIN = (
    Path(__file__).resolve().parents[2]
    / "borgchart-engineering/teams/dev-team/plugins/seat"
)
spec = importlib.util.spec_from_file_location("seat", PLUGIN / "seat.py")
assert spec and spec.loader
seat = importlib.util.module_from_spec(spec)
sys.dont_write_bytecode = True
spec.loader.exec_module(seat)
NOW = datetime.now(UTC)


def response(
    vendor: str = "claude", model: str = "opus", used: float = 43
) -> dict[str, Any]:
    candidate = {"kind": vendor, "model": model}
    warnings = (
        []
        if used < 90
        else [
            {
                "message": f"{used}% scope all; observed now; reset tomorrow; advisory warning"
            }
        ]
    )
    constraints = [] if used < 90 else [{"used_percent": used, "blocked": False}]
    return {
        "decision": "route",
        "route": candidate,
        "evaluations": [
            {
                "candidate": candidate,
                "state": "available",
                "constraints": constraints,
                "warnings": warnings,
            }
        ],
        "warnings": warnings,
        "observations": {
            vendor: {
                "status": "ok",
                "observed_at": (NOW - timedelta(seconds=5)).isoformat(),
                "expires_at": (NOW + timedelta(minutes=5)).isoformat(),
            }
        },
    }


@pytest.mark.parametrize(
    "vendor,model,returned",
    [
        ("claude", "opus", "opus"),
        ("claude", "sonnet", "sonnet"),
        ("codex", "astra", "gpt-6-astra"),
        ("codex", "gpt-6.1-sol", "gpt-6-sol"),
        ("codex", "gpt-6.1-terra", "terra"),
        ("codex", "luna", "gpt-6-luna"),
    ],
)
def test_exact_vendor_and_model_class(vendor: str, model: str, returned: str) -> None:
    assert seat.decision(response(vendor, returned), vendor, model, NOW).startswith(
        "requested quota available"
    )


@pytest.mark.parametrize("used", [89, 90, 99])
def test_advisory_not_a_second_cutoff(used: float) -> None:
    result = seat.decision(response(used=used), "claude", "opus", NOW)
    assert ("advisory warning" in result) == (used >= 90)


@pytest.mark.parametrize(
    "field,value",
    [
        ("decision", "unknown"),
        ("decision", "blocked"),
        ("route", {"kind": "codex", "model": "opus"}),
        ("route", {"kind": "claude", "model": "sonnet"}),
        ("evaluations", []),
        ("warnings", None),
        ("observations", {}),
    ],
)
def test_invalid_route_refused(field: str, value: Any) -> None:
    data = response()
    data[field] = value
    with pytest.raises(ValueError):
        seat.decision(data, "claude", "opus", NOW)


@pytest.mark.parametrize(
    "field,value",
    [
        ("status", "unknown"),
        ("observed_at", None),
        ("observed_at", "2026-01-01T00:00:00"),
        ("observed_at", (NOW + timedelta(seconds=10)).isoformat()),
        ("expires_at", NOW.isoformat()),
        ("expires_at", "garbage"),
    ],
)
def test_fresh_known_observation_required(field: str, value: Any) -> None:
    data = response()
    data["observations"]["claude"][field] = value
    with pytest.raises(ValueError):
        seat.decision(data, "claude", "opus", NOW)


@pytest.mark.parametrize(
    "used,blocked",
    [
        (100, False),
        (99, True),
        (None, False),
        (float("nan"), False),
        (True, False),
        (-1, False),
    ],
)
def test_contradictory_available_decision_refused(used: Any, blocked: bool) -> None:
    data = response(used=99)
    data["evaluations"][0]["constraints"] = [{"used_percent": used, "blocked": blocked}]
    with pytest.raises(ValueError):
        seat.decision(data, "claude", "opus", NOW)


def test_unavailable_evaluation_refused() -> None:
    data = response()
    data["evaluations"][0]["state"] = "blocked"
    with pytest.raises(ValueError):
        seat.decision(data, "claude", "opus", NOW)


def test_timeout_is_wait(
    monkeypatch: pytest.MonkeyPatch, capsys: pytest.CaptureFixture[str]
) -> None:
    monkeypatch.setattr(seat, "reader_command", lambda: "fake-reader")

    def timeout(*args: Any, **kwargs: Any) -> None:
        raise subprocess.TimeoutExpired("fake-reader", 60)

    monkeypatch.setattr(seat.subprocess, "run", timeout)
    assert seat.main(["route", "claude", "opus"]) == 3
    assert capsys.readouterr().out.startswith("wait:")


def run_plugin(
    tmp_path: Path,
    source: str | None,
    config: bool = False,
    args: list[str] | None = None,
) -> subprocess.CompletedProcess[str]:
    bindir = tmp_path / "bin"
    bindir.mkdir(exist_ok=True)
    env = {"HOME": str(tmp_path), "PATH": str(bindir), "LANG": "C.UTF-8"}
    setting = tmp_path / ".config/borgchart/seat-reader"
    if config:
        env["BORGCHART_CONFIG"] = str(tmp_path / "custom/config.yaml")
        setting = tmp_path / "custom/seat-reader"
    if source is not None:
        reader = bindir / "usage-route"
        reader.write_text("#!/bin/sh\n" + source)
        reader.chmod(0o755)
        setting.parent.mkdir(parents=True)
        setting.write_text(str(reader))
        env["PATH"] = "/nonexistent"
    return subprocess.run(
        [
            sys.executable,
            str(PLUGIN / "seat.py"),
            *(args or ["route", "claude", "opus"]),
        ],
        env=env,
        capture_output=True,
        text=True,
        check=False,
    )


@pytest.mark.parametrize("config", [False, True])
def test_reader_discovery_and_exact_arguments(tmp_path: Path, config: bool) -> None:
    data = json.dumps(response())
    result = run_plugin(
        tmp_path,
        f"""[ "$#" = 2 ] && [ "$1" = claude ] && [ "$2" = opus ] || exit 2
printf '%s\\n' '{data}'
""",
        config,
    )
    assert result.returncode == 0, result.stdout + result.stderr
    assert result.stdout.startswith("go:")


@pytest.mark.parametrize(
    "source",
    [
        None,
        "exit 2",
        "exit 3",
        "exit 1",
        "printf invalid",
        "printf null",
        "printf '[]'",
        "printf '{}'",
    ],
)
def test_reader_failure_is_wait(tmp_path: Path, source: str | None) -> None:
    result = run_plugin(tmp_path, source)
    assert result.returncode == 3
    assert result.stdout.startswith("wait:")


@pytest.mark.parametrize(
    "args",
    [
        ["route", "auto", "opus"],
        ["route", "claude", "--now"],
        ["route", "claude", "opus high"],
        ["route", "claude"],
        ["start", "claude", "opus"],
    ],
)
def test_invalid_arguments_do_not_call_reader(tmp_path: Path, args: list[str]) -> None:
    result = run_plugin(tmp_path, "exit 99", args=args)
    assert result.returncode == 2


def test_broken_config_does_not_fall_back(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv("BORGCHART_CONFIG", raising=False)
    setting = tmp_path / ".config/borgchart/seat-reader"
    setting.parent.mkdir(parents=True)
    setting.symlink_to(tmp_path / "absent")
    with pytest.raises(ValueError):
        seat.reader_command()


@pytest.mark.parametrize("value", ["", "reader\n--now", "relative/reader"])
def test_invalid_config_refused(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch, value: str
) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.delenv("BORGCHART_CONFIG", raising=False)
    setting = tmp_path / ".config/borgchart/seat-reader"
    setting.parent.mkdir(parents=True)
    setting.write_text(value)
    with pytest.raises(ValueError):
        seat.reader_command()


def test_constraints_cannot_drop_advisories() -> None:
    data = response(used=99)
    data["warnings"] = []
    data["evaluations"][0]["warnings"] = []
    with pytest.raises(ValueError):
        seat.decision(data, "claude", "opus", NOW)


@pytest.mark.parametrize("status", ["blocked", "unknown", "suggest_wait"])
def test_refusal_retains_safe_retry_time(status: str) -> None:
    reset = NOW + timedelta(hours=1)
    data = {
        "decision": status,
        "expected_recovery_at": reset.isoformat(),
        "reason": "private detail never relayed",
    }
    reason = seat.refusal(json.dumps(data), "claude", "opus", NOW)
    assert reset.isoformat() in reason
    assert "private" not in reason


def test_refusal_finds_matching_evaluation_recovery() -> None:
    data = response()
    data["decision"] = "blocked"
    reset = NOW + timedelta(hours=1)
    data["evaluations"][0]["expected_recovery_at"] = reset.isoformat()
    assert reset.isoformat() in seat.refusal(json.dumps(data), "claude", "opus", NOW)


@pytest.mark.parametrize(
    "output",
    [
        "invalid",
        "[]",
        '{"decision":[]}',
        '{"decision":"blocked","expected_recovery_at":"private text"}',
    ],
)
def test_bad_refusal_does_not_leak_output(output: str) -> None:
    assert (
        seat.refusal(output, "claude", "opus", NOW)
        == "reader unavailable or quota denied"
    )


def test_model_error_is_distinct_from_quota_unknown() -> None:
    data = {
        "decision": "unknown",
        "reason": "selected model/effort could not be validated by model-route",
    }
    assert (
        seat.refusal(json.dumps(data), "codex", "astra", NOW)
        == "reader model validation failed"
    )


def test_shell_entry_without_uv_waits(tmp_path: Path) -> None:
    result = subprocess.run(
        [str(PLUGIN / "seat"), "route", "claude", "opus"],
        env={**os.environ, "PATH": str(tmp_path)},
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == 3
    assert result.stdout == "wait: uv is unavailable\n"


@pytest.mark.parametrize(
    "status,output,expected",
    [
        (2, "", 3),
        (127, "", 3),
        (0, "invalid", 3),
        (0, "go: quota available", 0),
        (2, "wait: bad arguments", 2),
        (3, "wait: quota unknown", 3),
    ],
)
def test_shell_entry_validates_runtime_protocol(
    tmp_path: Path, status: int, output: str, expected: int
) -> None:
    runtime = tmp_path / "uv"
    runtime.write_text(f"#!/bin/sh\nprintf '%s\\n' '{output}'\nexit {status}\n")
    runtime.chmod(0o755)
    result = subprocess.run(
        [str(PLUGIN / "seat"), "route", "claude", "opus"],
        env={**os.environ, "PATH": f"{tmp_path}:/usr/bin:/bin"},
        capture_output=True,
        text=True,
        check=False,
    )
    assert result.returncode == expected
    assert result.stdout.startswith("go:" if expected == 0 else "wait:")
    if output.startswith(("go:", "wait:")):
        assert result.stdout.strip() == output


def test_default_reader_is_bundled_not_taken_from_path(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "unrelated"))
    monkeypatch.delenv("BORGCHART_CONFIG", raising=False)
    assert seat.reader_command() == str(PLUGIN / "usage-route")


def test_override_follows_the_actual_brg_config_directory(
    tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    monkeypatch.setenv("BORGCHART_CONFIG", str(tmp_path / "settings/custom.yaml"))
    monkeypatch.setenv("XDG_CONFIG_HOME", str(tmp_path / "unrelated"))
    setting = tmp_path / "settings/seat-reader"
    setting.parent.mkdir()
    setting.write_text("custom-reader")
    assert seat.reader_command() == "custom-reader"
