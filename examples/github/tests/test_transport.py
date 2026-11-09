from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path
from typing import Any

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import intake as g


@pytest.fixture
def executable(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> Path:
    path = tmp_path / "brg"
    path.write_text(
        '#!/bin/sh\nprintf "%s\\n" "$@" > "$EXAMPLE_ARGS"\ncat > "$EXAMPLE_BODY"\ncat "$EXAMPLE_RESPONSE"\nexit "$EXAMPLE_EXIT"\n'
    )
    path.chmod(0o700)
    monkeypatch.setenv("BORGCHART_BIN", str(path))
    monkeypatch.setenv("BORGCHART_INSTANCE", "example-2345")
    monkeypatch.setenv("BORGCHART_DELIVER_TO", "lead-2345")
    monkeypatch.delenv("BORGCHART_MEMBER", raising=False)
    monkeypatch.delenv("BORGCHART_INCARNATION", raising=False)
    monkeypatch.setenv("EXAMPLE_ARGS", str(tmp_path / "argv"))
    monkeypatch.setenv("EXAMPLE_RESPONSE", str(tmp_path / "response"))
    monkeypatch.setenv("EXAMPLE_EXIT", "0")
    monkeypatch.setenv("EXAMPLE_BODY", str(tmp_path / "body"))
    return path


def reply(path: Path, monkeypatch: pytest.MonkeyPatch, code: int, data: Any) -> None:
    (path.parent / "response").write_text(json.dumps(data))
    monkeypatch.setenv("EXAMPLE_EXIT", str(code))


@pytest.mark.parametrize(
    "delivery", ["typed", "stored", "uncertain", "recorded-not-typed"]
)
@pytest.mark.parametrize("code", [0, 5])
def test_committed_id_wins_over_delivery(
    executable: Path, monkeypatch: pytest.MonkeyPatch, delivery: str, code: int
) -> None:
    reply(
        executable,
        monkeypatch,
        code,
        dict(
            ok=code == 0,
            code=code,
            outcome="committed" if code == 0 else "refused",
            id="m17",
            delivery=delivery,
        ),
    )
    body = '{"untrusted":true,"key":"example"}'
    assert g.send_brg(g.delivery_target(), body, 1) == (
        "committed" if code == 0 else "m17"
    )
    assert (executable.parent / "argv").read_text().splitlines() == [
        "send",
        "--external",
        "github",
        "--json",
        "example-2345/lead-2345",
        "-",
    ]
    assert (executable.parent / "body").read_text() == body


@pytest.mark.parametrize(
    "code,outcome", [(3, "not-alive"), (4, "blocked"), (5, "refused")]
)
def test_precommit_refusal(
    executable: Path, monkeypatch: pytest.MonkeyPatch, code: int, outcome: str
) -> None:
    reply(executable, monkeypatch, code, dict(ok=False, code=code, outcome=outcome))
    if code == 5:
        assert g.send_brg(g.delivery_target(), "{}", 1) is None
    else:
        with pytest.raises(g.BrgUnavailable):
            g.send_brg(g.delivery_target(), "{}", 1)


def test_old_binary_usage_is_unavailable(
    executable: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    reply(executable, monkeypatch, 2, dict(ok=False, code=2, outcome="usage"))
    with pytest.raises(g.BrgUnavailable):
        g.send_brg(g.delivery_target(), "{}", 1)


@pytest.mark.parametrize(
    "data",
    [
        {},
        [],
        {"ok": True, "code": 0, "outcome": "committed"},
        {
            "ok": True,
            "code": 0,
            "outcome": "committed",
            "id": "bad",
            "delivery": "stored",
        },
    ],
)
def test_zero_exit_is_committed_without_json(
    executable: Path, monkeypatch: pytest.MonkeyPatch, data: Any
) -> None:
    reply(executable, monkeypatch, 0, data)
    assert g.send_brg(g.delivery_target(), "{}", 1) == "committed"


def test_timeout_is_bounded(executable: Path) -> None:
    executable.write_text("#!/bin/sh\nsleep 60\n")
    with pytest.raises(subprocess.TimeoutExpired):
        g.send_brg(g.delivery_target(), "{}", 0.05)


@pytest.mark.parametrize("name", ["BORGCHART_MEMBER", "BORGCHART_INCARNATION"])
def test_member_identity_is_never_stripped(
    executable: Path, monkeypatch: pytest.MonkeyPatch, name: str
) -> None:
    monkeypatch.setenv(name, "example-member")
    with pytest.raises(g.BrgUnavailable):
        g.send_brg(g.delivery_target(), "{}", 1)
    assert not (executable.parent / "argv").exists()


def test_missing_executable_is_unavailable(executable: Path) -> None:
    executable.unlink()
    with pytest.raises(g.BrgUnavailable):
        g.send_brg(g.delivery_target(), "{}", 1)


def test_real_brg_inbox_commit(tmp_path: Path, monkeypatch: pytest.MonkeyPatch) -> None:
    binary = os.environ.get("GITHUB_INTAKE_TEST_BRG")
    if not binary:
        pytest.skip("set GITHUB_INTAKE_TEST_BRG to a test-support brg binary")
    for key in list(os.environ):
        if key.startswith(("BORGCHART_", "GIT_")) or key in (
            "TMUX",
            "TMUX_PANE",
            "TMUX_TMPDIR",
        ):
            monkeypatch.delenv(key, raising=False)
    root = tmp_path / "state"
    instance = "example-2345"
    member = "lead-2345"
    team = tmp_path / "team"
    team.mkdir()

    def write(path: Path, value: Any) -> None:
        path.parent.mkdir(parents=True, exist_ok=True)
        for directory in [root, root / instance, path.parent]:
            directory.chmod(0o700)
        path.write_text(json.dumps(value))
        path.chmod(0o600)

    (root / instance).mkdir(parents=True)
    meta = dict(
        team="example",
        team_dir=str(team),
        folder=str(tmp_path / "work"),
        created_at="2026-01-01T00:00:00Z",
    )
    write(
        root / "registry.json",
        dict(schema=1, instances={instance: meta}, issued={"example": ["2345"]}),
    )
    write(
        root / instance / "instance.json",
        dict(schema=1, id=instance, **meta, members={member: dict(role="lead")}),
    )
    write(root / instance / "messages/store.json", dict(format=3, instance=instance))
    write(
        root / instance / "start.json",
        dict(
            team_dir=str(team),
            folder=str(tmp_path / "work"),
            brg=binary,
            config=dict(version=1, digest="00", inputs={}),
            socket="example-unused",
            windows_started=True,
            base_env={},
            members={
                member: dict(
                    role="lead",
                    vendor="claude",
                    posture="vendor",
                    interactive=True,
                    parent=None,
                    vendor_version=None,
                    started=True,
                )
            },
            plugins={},
        ),
    )
    write(
        root / instance / "members" / member / "incarnation.json",
        dict(
            member=member,
            incarnation="example-start",
            started_at="2026-01-01T00:00:00Z",
            ended_at="2026-01-02T00:00:00Z",
            ended_reason="down",
        ),
    )
    monkeypatch.setenv("HOME", str(tmp_path))
    monkeypatch.setenv("BORGCHART_CONFIG", str(tmp_path / "config.yaml"))
    monkeypatch.setenv("BORGCHART_STATE_HOME", str(root))
    monkeypatch.setenv("BORGCHART_BIN", binary)
    monkeypatch.setenv("BORGCHART_INSTANCE", instance)
    monkeypatch.setenv("BORGCHART_DELIVER_TO", member)
    assert (
        g.send_brg(g.delivery_target(), '{"untrusted":true,"key":"example"}', 5)
        == "committed"
    )
    records = list((root / instance / "messages/records").glob("*.json"))
    assert len(records) == 1
    record = json.loads(records[0].read_text())
    assert record["from"] == "external:github" and record["to"] == member
    assert json.loads(record["body"])["key"] == "example"
