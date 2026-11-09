"""Exercise the complete seating path with isolated, fake vendor CLIs."""

import importlib.util
import json
import re
import shutil
import subprocess
import sys
from datetime import UTC, datetime, timedelta
from pathlib import Path
from typing import Any

import pytest

sys.dont_write_bytecode = True

ROOT = Path(__file__).resolve().parents[2]
PLUGIN = ROOT / "borgchart-engineering/teams/dev-team/plugins/seat"
FAKE = r"""
import json
import os
import sys
from pathlib import Path

provider = Path(sys.argv[0]).name
if sys.argv[1:] == ["--version"]:
    print("1.0.0 (quota test fixture)")
    raise SystemExit(0)
assert provider in {"claude", "codex"}
assert not any(key.startswith(("BORGCHART_", "HERDR_")) for key in os.environ)
assert "CLAUDECODE" not in os.environ and "CODEX_THREAD_ID" not in os.environ
payload = json.loads(Path(os.environ["QUOTA_FAKE_RESPONSE"]).read_text())
for line in sys.stdin:
    request = json.loads(line)
    with open(os.environ["QUOTA_FAKE_CALLS"], "a") as handle:
        handle.write(json.dumps({"provider": provider, "request": request}) + "\n")
    if provider == "codex":
        method = request.get("method")
        assert method in {"initialize", "initialized", "account/rateLimits/read"}
        if method == "initialized":
            continue
        result = {} if method == "initialize" else payload
        print(json.dumps({"id": request["id"], "result": result}), flush=True)
    else:
        assert request["type"] == "control_request"
        subtype = request["request"]["subtype"]
        assert subtype in {"initialize", "get_usage"}
        result = {} if subtype == "initialize" else payload
        print(json.dumps({"type": "control_response", "response": {
            "request_id": request["request_id"], "subtype": "success", "response": result
        }}), flush=True)
"""


def payload(vendor: str, used: float) -> dict[str, Any]:
    reset = datetime.now(UTC) + timedelta(hours=1)
    if vendor == "codex":
        return {
            "ordinaryUsageAllowed": True,
            "rateLimits": {
                "limitId": "codex",
                "primary": {
                    "usedPercent": used,
                    "windowDurationMins": 300,
                    "resetsAt": int(reset.timestamp()),
                },
                "secondary": None,
                "rateLimitReachedType": None,
            },
        }
    return {
        "subscription_type": "max",
        "rate_limits_available": True,
        "rate_limits": {
            "five_hour": {"utilization": used, "resets_at": reset.isoformat()},
            "seven_day": {"utilization": used, "resets_at": reset.isoformat()},
            "seven_day_opus": None,
            "seven_day_sonnet": None,
        },
    }


@pytest.mark.parametrize("vendor,model", [("claude", "opus"), ("codex", "gpt-6-astra")])
@pytest.mark.parametrize("used,expected", [(40, 0), (90, 0), (99, 0), (100, 3)])
def test_bundled_seat_uses_only_fake_protocols(
    tmp_path: Path, vendor: str, model: str, used: float, expected: int
) -> None:
    bindir = tmp_path / "bin"
    bindir.mkdir()
    for name in ("claude", "codex"):
        command = bindir / name
        command.write_text(f"#!{sys.executable}\n{FAKE}")
        command.chmod(0o755)
    uv = shutil.which("uv")
    assert uv is not None, "the test suite runs with uv"
    (bindir / "uv").symlink_to(uv)
    response = tmp_path / "response.json"
    response.write_text(json.dumps(payload(vendor, used)))
    calls = tmp_path / "calls.jsonl"
    env = {
        "HOME": str(tmp_path),
        "PATH": f"{bindir}:/usr/bin:/bin",
        "LANG": "C.UTF-8",
        "CODEX_HOME": str(tmp_path / "codex"),
        "CLAUDE_CONFIG_DIR": str(tmp_path / "claude"),
        "XDG_CACHE_HOME": str(tmp_path / "cache"),
        "UV_CACHE_DIR": str(tmp_path / "uv-cache"),
        "UV_PYTHON": sys.executable,
        "UV_PYTHON_DOWNLOADS": "never",
        "UV_OFFLINE": "1",
        "QUOTA_FAKE_RESPONSE": str(response),
        "QUOTA_FAKE_CALLS": str(calls),
        "BORGCHART_INSTANCE": "fixture-2222",
        "HERDR_TEST_SENTINEL": "fixture",
        "CLAUDECODE": "fixture",
        "CODEX_THREAD_ID": "fixture",
    }
    result = subprocess.run(
        [str(PLUGIN / "seat"), "route", vendor, model],
        env=env,
        capture_output=True,
        text=True,
        timeout=30,
        check=False,
    )
    assert result.returncode == expected, result.stdout + result.stderr
    assert result.stdout.startswith("go:" if expected == 0 else "wait:")
    if 90 <= used < 100:
        assert "advisory" in result.stdout
    requests = [json.loads(line) for line in calls.read_text().splitlines()]
    assert {row["provider"] for row in requests} == {vendor}
    assert len(requests) == (3 if vendor == "codex" else 2)


def test_every_shipped_member_model_has_a_quota_scope() -> None:
    spec = importlib.util.spec_from_file_location("shipped_usage", PLUGIN / "usage.py")
    assert spec and spec.loader
    reader = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(reader)
    now = datetime.now(UTC)
    declarations = []
    for member in (ROOT / "borgchart-engineering").rglob("*.md"):
        text = member.read_text()
        if not text.startswith("---\n"):
            continue
        frontmatter = text.split("---", 2)[1]
        for vendor, model in re.findall(
            r"^  (claude|codex):\s+(\S+)", frontmatter, re.MULTILINE
        ):
            declarations.append((vendor, model))
            reading = reader.normalize(vendor, payload(vendor, 40), now)
            route = {"kind": vendor, "model": model}
            assert reader.assess(route, reading, now)["state"] == "available", member
    assert declarations and {vendor for vendor, _ in declarations} == {
        "claude",
        "codex",
    }
