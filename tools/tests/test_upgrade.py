"""Run the upgrade command against isolated brg and make executables."""

import json
import os
import shlex
import shutil
import subprocess
import time
from pathlib import Path

import pytest

UPGRADE = Path(__file__).resolve().parents[1] / "upgrade"
ROOT = UPGRADE.parent.parent
FAKE = """#!/usr/bin/env -S uv run --script --quiet
import json, os, sys, time
from pathlib import Path
p = Path(os.environ["SCENARIO"])
s = json.loads(p.read_text())
args = sys.argv[1:]
name = Path(sys.argv[0]).name
with open(os.environ["CALLS"], "a") as f:
    f.write(json.dumps([name, *args]) + "\\n")
code, out = 0, {}
if name == "make":
    code = s.get("build_code" if args[-1] == "build" else "install_code", 0)
elif args[0] == "status":
    out = s["statuses"].pop(0) if len(s["statuses"]) > 1 else s["statuses"][0]
    if s.get("status_codes"):
        code = s["status_codes"].pop(0)
elif args[:2] == ["msg", "send"]:
    s["sent"] = s.get("sent", 0) + 1
    out = {"id": "m" + str(s["sent"])}
elif args[:2] in (["msg", "list"], ["msg", "wait"]):
    team = args[args.index("--team") + 1]
    inbox = s.setdefault("inboxes", {}).setdefault(team, [])
    delay = 0
    if args[1] == "wait":
        # Already-present records only return after the CLI's minimum delay.
        delay = s.get("wait_delay", 60 if inbox else 0)
        item = s.get("waits", []).pop(0) if s.get("waits") else {"code": 9}
        code = item.get("code", 0)
        inbox.extend(item.get("records", []))
        for target, records in item.get("publish", {}).items():
            s["inboxes"].setdefault(target, []).extend(records)
    out = {"records": inbox}
    p.write_text(json.dumps(s))
    time.sleep(delay)
elif args[:2] == ["msg", "ack"]:
    team = args[args.index("--team") + 1]
    s["inboxes"][team] = [r for r in s["inboxes"][team] if r["id"] not in args]
elif args[0] == "stop":
    code = s.get("stop_code", 0)
elif args[0] == "start":
    code = s.get("start_code", 0)
p.write_text(json.dumps(s))
print(json.dumps(out))
sys.exit(code)
"""


def member(
    team: str = "sample-1111",
    name: str = "lead-aaaa",
    state: str = "idle",
    parent: str | None = None,
    liveness: str | None = "alive",
) -> dict[str, str | None]:
    return {
        "kind": "member",
        "team": team,
        "member": name,
        "state": state,
        "parent": parent,
        "liveness": liveness,
    }


def reply(
    sender: str = "sample-1111/lead-aaaa", sent: str = "m1", record: str = "m10"
) -> dict[str, object]:
    return {"id": record, "from": sender, "reply_to": [sent]}


class FakeUpgrade:
    def __init__(self, directory: Path) -> None:
        self.directory = directory
        self.env = dict(os.environ)
        # The caller is outside all teams; no test can reach real team state.
        for key in list(self.env):
            if key.startswith("BORGCHART_") or key in (
                "BRG",
                "MAKE",
                "FORCE",
                "DRY_RUN",
                "TIMEOUT",
                "MAKEFLAGS",
                "MFLAGS",
                "MAKELEVEL",
            ):
                self.env.pop(key)
        for name in ("brg", "make"):
            path = directory / name
            path.write_text(FAKE)
            path.chmod(0o755)
        self.env.update(
            PATH=f"{directory}:{os.environ['PATH']}",
            SCENARIO=str(directory / "scenario.json"),
            CALLS=str(directory / "calls.jsonl"),
            TIMEOUT="0.8",
        )

    def run(
        self,
        scenario: dict[str, object],
        *,
        command: list[str] | None = None,
        **settings: str,
    ) -> subprocess.CompletedProcess[str]:
        Path(self.env["SCENARIO"]).write_text(json.dumps(scenario))
        self.env.update(settings)
        return subprocess.run(
            command or [str(UPGRADE)],
            cwd=self.directory,
            env=self.env,
            capture_output=True,
            text=True,
            check=False,
            timeout=20,
        )

    def calls(self) -> list[list[str]]:
        path = Path(self.env["CALLS"])
        return (
            [json.loads(line) for line in path.read_text().splitlines()]
            if path.exists()
            else []
        )

    def actions(self) -> list[str]:
        return [
            call[-1] if call[0] == "make" else call[1]
            for call in self.calls()
            if call[0] == "make" or call[1] in ("stop", "start")
        ]


@pytest.fixture
def fake(tmp_path: Path) -> FakeUpgrade:
    return FakeUpgrade(tmp_path)


@pytest.mark.parametrize(
    "state,liveness", [("stopped", "gone"), ("not started", None), ("exited", "gone")]
)
def test_no_live_members_installs_without_touching_stopped_trees(
    fake: FakeUpgrade,
    state: str,
    liveness: str | None,
) -> None:
    result = fake.run({"statuses": [[member(state=state, liveness=liveness)]]})
    assert result.returncode == 0, result.stderr
    assert fake.actions() == ["build", "install"]
    assert not any(call[1:3] == ["msg", "send"] for call in fake.calls())
    assert ["make", "-C", str(ROOT), "-o", "build", "install"] in fake.calls()


def test_matching_handover_precedes_stop_install_and_fresh_start(
    fake: FakeUpgrade,
) -> None:
    rows = [member(), member(team="sample-1111/dev", parent="sample-1111")]
    result = fake.run(
        {
            "statuses": [rows],
            "waits": [{"records": [reply()]}],
        }
    )
    assert result.returncode == 0, result.stderr
    assert fake.actions() == ["build", "stop", "install", "start"]
    assert next(i for i, call in enumerate(fake.calls()) if call[-1] == "build") < next(
        i for i, call in enumerate(fake.calls()) if call[1:3] == ["msg", "send"]
    )
    assert [
        "brg",
        "msg",
        "ack",
        "--team",
        "sample-1111",
        "m10",
        "--json",
    ] in fake.calls()
    assert len([call for call in fake.calls() if call[1:3] == ["msg", "ack"]]) == 1
    assert fake.calls()[-1] == ["brg", "start", "sample-1111"]


def test_timeout_reports_committed_message_and_stops_nothing(fake: FakeUpgrade) -> None:
    result = fake.run({"statuses": [[member(state="blocked")]]})
    assert result.returncode != 0
    assert fake.actions() == ["build"]
    assert "sample-1111" in result.stdout
    assert "m1" in result.stdout
    assert "brg status sample-1111" in result.stdout


def test_unrelated_and_wrong_sender_replies_never_ack_or_authorize_stop(
    fake: FakeUpgrade,
) -> None:
    records = [
        reply(sent="m99"),
        reply(sender="sample-1111/helper-bbbb"),
        {"id": "m12", "from": "sample-1111/lead-aaaa"},
    ]
    result = fake.run({"statuses": [[member()]], "waits": [{"records": records}]})
    assert result.returncode != 0
    assert fake.actions() == ["build"]
    assert any(call[1:3] == ["msg", "send"] for call in fake.calls())
    assert any(call[1:3] == ["msg", "wait"] for call in fake.calls())
    assert not any(call[1:3] == ["msg", "ack"] for call in fake.calls())


def test_working_child_after_handover_refuses_before_stopping(
    fake: FakeUpgrade,
) -> None:
    child = member(
        team="sample-1111/dev",
        name="worker-bbbb",
        parent="sample-1111/dev",
        state="working",
    )
    result = fake.run(
        {"statuses": [[member()], [member(), child]], "waits": [{"records": [reply()]}]}
    )
    assert result.returncode != 0
    assert fake.actions() == ["build"]
    assert "sample-1111/dev/worker-bbbb" in result.stdout


def test_force_skips_handover_wait_and_working_refusal(fake: FakeUpgrade) -> None:
    result = fake.run({"statuses": [[member(state="working")]]}, FORCE="1")
    assert result.returncode == 0, result.stderr
    assert fake.actions() == ["build", "stop", "install", "start"]
    assert not any(call[1:3] == ["msg", "send"] for call in fake.calls())
    assert not any(call[1:3] == ["msg", "wait"] for call in fake.calls())


def test_failed_install_starts_stopped_trees_and_preserves_failure(
    fake: FakeUpgrade,
) -> None:
    result = fake.run({"statuses": [[member()]], "install_code": 42}, FORCE="1")
    assert result.returncode != 0
    assert fake.actions() == ["build", "stop", "install", "start"]


def test_dry_run_sends_stops_and_installs_nothing(fake: FakeUpgrade) -> None:
    result = fake.run({"statuses": [[member()]]}, DRY_RUN="1")
    assert result.returncode == 0, result.stderr
    assert fake.calls() == [["brg", "status", "--json"]]
    assert "sample-1111" in result.stdout


def test_every_root_lead_must_reply_before_any_tree_stops(fake: FakeUpgrade) -> None:
    rows = [member(), member(name="other-bbbb"), member(team="second-2222")]
    result = fake.run({"statuses": [rows], "waits": [{"records": [reply()]}]})
    assert result.returncode != 0
    assert fake.actions() == ["build"]
    assert "sample-1111/other-bbbb" in result.stdout
    assert "second-2222" in result.stdout


def test_fenced_wait_never_stops_or_installs(fake: FakeUpgrade) -> None:
    result = fake.run({"statuses": [[member()]], "waits": [{"code": 8}]})
    assert result.returncode != 0
    assert fake.actions() == ["build"]
    assert any(call[1:3] == ["msg", "send"] for call in fake.calls())
    assert any(call[1:3] == ["msg", "wait"] for call in fake.calls())


def test_stop_failure_never_installs(fake: FakeUpgrade) -> None:
    result = fake.run({"statuses": [[member()]], "stop_code": 1}, FORCE="1")
    assert result.returncode != 0
    assert fake.actions() == ["build", "stop"]


def test_restart_failure_is_reported_as_failed_upgrade(fake: FakeUpgrade) -> None:
    result = fake.run({"statuses": [[member()]], "start_code": 1}, FORCE="1")
    assert result.returncode != 0
    assert fake.actions() == ["build", "stop", "install", "start"]


@pytest.mark.parametrize("force", [False, True])
def test_live_child_without_live_root_requires_force(
    fake: FakeUpgrade, force: bool
) -> None:
    result = fake.run(
        {
            "statuses": [
                [
                    member(state="stopped", liveness="gone"),
                    member(
                        team="sample-1111/dev",
                        name="worker-bbbb",
                        parent="sample-1111/dev",
                    ),
                ]
            ]
        },
        FORCE="1" if force else "0",
    )
    assert not any(call[1:3] == ["msg", "send"] for call in fake.calls())
    if force:
        assert result.returncode == 0, result.stderr
        assert fake.actions() == ["build", "stop", "install", "start"]
    else:
        assert result.returncode != 0
        assert fake.actions() == []
        assert "sample-1111/dev/worker-bbbb" in result.stdout


def test_all_trees_are_stopped_before_install_and_all_are_restarted(
    fake: FakeUpgrade,
) -> None:
    rows = [member(), member(team="second-2222")]
    result = fake.run(
        {
            "statuses": [rows],
            "waits": [
                {"records": [reply()]},
                {
                    "records": [
                        reply(sender="second-2222/lead-aaaa", sent="m2", record="m11")
                    ]
                },
            ],
        }
    )
    assert result.returncode == 0, result.stderr
    assert fake.actions() == ["build", "stop", "stop", "install", "start", "start"]
    assert [
        "brg",
        "msg",
        "ack",
        "--team",
        "second-2222",
        "m11",
        "--json",
    ] in fake.calls()


def test_global_deadline_bounds_wait_below_cli_minimum(fake: FakeUpgrade) -> None:
    started = time.monotonic()
    result = fake.run({"statuses": [[member()]], "wait_delay": 5}, TIMEOUT="0.3")
    assert time.monotonic() - started < 3
    assert result.returncode != 0
    assert fake.actions() == ["build"]
    assert "m1" in result.stdout


def test_explicit_brg_and_make_paths_work_with_spaces(fake: FakeUpgrade) -> None:
    commands = fake.directory / "other commands"
    commands.mkdir()
    for name in ("brg", "make"):
        path = commands / name
        path.write_text(FAKE)
        path.chmod(0o755)
        (fake.directory / name).write_text("#!/bin/sh\nexit 99\n")
    result = fake.run(
        {"statuses": [[member()]]},
        FORCE="1",
        BRG=str(commands / "brg"),
        MAKE=shlex.quote(str(commands / "make")),
    )
    assert result.returncode == 0, result.stderr
    assert fake.actions() == ["build", "stop", "install", "start"]


def test_make_dry_run_never_sends_or_stops(fake: FakeUpgrade) -> None:
    result = fake.run(
        {"statuses": [[member()]]},
        command=[
            shutil.which("make") or "/usr/bin/make",
            "-n",
            "-f",
            str(ROOT / "Makefile"),
            "upgrade",
        ],
        FORCE="1",
    )
    assert result.returncode == 0, result.stderr
    assert fake.calls() == []


@pytest.mark.parametrize("flags", ["n", "t", "q", "kn -- TIMEOUT=1"])
def test_makeflags_dry_modes_never_send_stop_or_build(
    fake: FakeUpgrade, flags: str
) -> None:
    result = fake.run({"statuses": [[member()]]}, MAKEFLAGS=flags)
    assert result.returncode == 0, result.stderr
    assert fake.calls() == [["brg", "status", "--json"]]


def test_failed_build_never_sends_or_stops(fake: FakeUpgrade) -> None:
    result = fake.run({"statuses": [[member()]], "build_code": 42})
    assert result.returncode != 0
    assert fake.actions() == ["build"]
    assert not any(call[1:3] == ["msg", "send"] for call in fake.calls())


def test_working_replier_can_settle_before_deadline(fake: FakeUpgrade) -> None:
    result = fake.run(
        {
            "statuses": [[member()], [member(state="working")], [member()]],
            "waits": [{"records": [reply()]}],
        },
        TIMEOUT="5",
    )
    assert result.returncode == 0, result.stderr
    assert fake.actions() == ["build", "stop", "install", "start"]


@pytest.mark.parametrize("after_stop", [False, True])
def test_new_live_tree_outside_stop_set_blocks_install(
    fake: FakeUpgrade, after_stop: bool
) -> None:
    rows = [member(), member(team="second-2222/dev", parent="second-2222/dev")]
    statuses = [[member()], [member()], rows] if after_stop else [[member()], rows]
    result = fake.run({"statuses": statuses}, FORCE="1")
    assert result.returncode != 0
    assert fake.actions() == (["build", "stop", "start"] if after_stop else ["build"])
    assert "second-2222/dev/lead-aaaa" in result.stdout


def test_failed_final_live_tree_check_restarts_stopped_trees(fake: FakeUpgrade) -> None:
    result = fake.run({"statuses": [[member()]], "status_codes": [0, 0, 1]}, FORCE="1")
    assert result.returncode != 0
    assert fake.actions() == ["build", "stop", "start"]


def test_already_present_reply_avoids_wait_minimum(fake: FakeUpgrade) -> None:
    result = fake.run(
        {
            "statuses": [[member(), member(team="second-2222")]],
            "inboxes": {
                "second-2222": [
                    reply(sender="second-2222/lead-aaaa", sent="m2", record="m11")
                ]
            },
            "waits": [{"records": [reply()]}],
        },
        TIMEOUT="2",
    )
    assert result.returncode == 0, result.stderr
    assert fake.actions() == ["build", "stop", "stop", "install", "start", "start"]
    assert not any(
        call[1:5] == ["msg", "wait", "--team", "second-2222"] for call in fake.calls()
    )


def test_timeout_names_only_tree_without_reply(fake: FakeUpgrade) -> None:
    result = fake.run(
        {
            "statuses": [[member(), member(team="second-2222")]],
            "wait_delay": 5,
            "waits": [
                {
                    "publish": {
                        "second-2222": [
                            reply(
                                sender="second-2222/lead-aaaa", sent="m2", record="m11"
                            )
                        ]
                    }
                }
            ],
        },
        TIMEOUT="0.5",
    )
    assert result.returncode != 0
    assert fake.actions() == ["build"]
    timeouts = [
        line for line in result.stdout.splitlines() if line.startswith("timeout:")
    ]
    assert len(timeouts) == 1
    assert "sample-1111" in timeouts[0]
    assert "second-2222" not in timeouts[0]
    assert any(call[1:3] == ["msg", "ack"] and "m11" in call for call in fake.calls())
