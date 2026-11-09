from __future__ import annotations

import copy
import json
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

import pytest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import intake as g
from github_mentions import explicit_mention

T0 = "2026-01-01T00:00:00+00:00"
T1 = "2026-01-02T00:00:00+00:00"
T2 = "2026-01-03T00:00:00+00:00"
ROOT = "https://api.github.com/repos/example-owner/example-repo"


def user(number: int) -> dict[str, Any]:
    return dict(id=number, type="User", login=f"example-user-{number}")


@pytest.fixture
def cfg(tmp_path: Path) -> dict[str, Any]:
    tmp_path.chmod(0o700)
    return dict(
        agent=dict(id=101, login="example-agent"),
        trusted_actor_ids=[102],
        owner_policy=dict(
            owner=dict(id=103, login="example-owner", type="User"),
            repository_policy="include_new",
            exclude_repository_ids=[],
            exclude_repositories=[],
        ),
        gh=["/nonexistent/gh"],
        mode="wake",
        reactions=False,
        poll_seconds=60,
        max_seconds=120,
        max_pages=10,
        max_records=10000,
        max_state_bytes=1024 * 1024,
        state=str(tmp_path / "state.json"),
        config_path=str(tmp_path / "config.json"),
        initial_repositories={},
        app_initiators=[],
        trusted_reviewer_ids=[],
        issue_mentions_after=T0,
        merge_wakes_after=T0,
    )


@pytest.fixture
def state(cfg: dict[str, Any]) -> dict[str, Any]:
    g.initialize(cfg, T0)
    return g.load_state(cfg)


class FakeAPI:
    def __init__(self, cfg: dict[str, Any], pr: bool = False) -> None:
        self.aliases: dict[str, str] = {}
        self.retry_at = 0.0
        self.poll_seconds = 60
        self.config = cfg
        self.events: list[dict[str, Any]] = []
        self.comments: list[dict[str, Any]] = []
        self.reviews: list[dict[str, Any]] = []
        self.review_comments: list[dict[str, Any]] = []
        self.repo = dict(
            id=104,
            full_name="example-owner/example-repo",
            owner=cfg["owner_policy"]["owner"],
        )
        self.issue = dict(
            id=105,
            number=7,
            repository_url=ROOT,
            url=ROOT + "/issues/7",
            state="open",
            assignees=[user(101)],
            user=user(101),
        )
        if pr:
            self.issue["pull_request"] = dict(url=ROOT + "/pulls/7")

    def get(self, path: str) -> dict[str, Any]:
        if path == "/user":
            return dict(id=101, login="example-agent", type="User")
        if path == "/repositories/104":
            return copy.deepcopy(self.repo)
        if path in (
            "/repos/example-owner/example-repo/issues/7",
            "/repositories/104/issues/7",
        ):
            return copy.deepcopy(self.issue)
        raise AssertionError(path)

    def pages(self, path: str) -> list[dict[str, Any]]:
        if path.endswith("/events"):
            return copy.deepcopy(self.events)
        if path.endswith("/reviews"):
            return copy.deepcopy(self.reviews)
        if "/pulls/" in path:
            return copy.deepcopy(self.review_comments)
        return copy.deepcopy(self.comments)


def assignment(actor: int = 102) -> dict[str, Any]:
    return dict(
        id=106,
        url=ROOT + "/issues/events/106",
        event="assigned",
        created_at=T1,
        actor=user(101),
        assigner=user(actor),
        assignee=user(101),
    )


def comment(body: str = "@example-agent please inspect") -> dict[str, Any]:
    return dict(
        id=107,
        url=ROOT + "/issues/comments/107",
        issue_url=ROOT + "/issues/7",
        user=user(102),
        body=body,
        created_at=T1,
        updated_at=T1,
    )


def snapshot(api: FakeAPI) -> dict[str, Any]:
    return g.verify_item(
        api,
        api.config["owner_policy"],
        api.config,
        104,
        "example-owner/example-repo",
        7,
    )


def queued(cfg: dict[str, Any], state: dict[str, Any]) -> tuple[FakeAPI, str]:
    api = FakeAPI(cfg)
    api.events = [assignment()]
    g.targeted(
        api,
        cfg,
        cfg["owner_policy"],
        state["row"],
        dict(repo_id=104, number=7, full_name="example-owner/example-repo"),
    )
    g.persist(cfg, state)
    return api, next(iter(state["row"]["pending"]))


@pytest.mark.parametrize(
    "body,expected",
    [
        ("@example-agent: please inspect", True),
        ("  @EXAMPLE-AGENT please inspect", True),
        ("@example-agent-other no", False),
        ("hello @example-agent", False),
        ("> @example-agent", False),
        ("```\n@example-agent\n```", False),
        ("    @example-agent", False),
        ("<pre>\n@example-agent\n</pre>", False),
        ("`open\n@example-agent", False),
        ("@example-agent\n---", False),
        ("- @example-agent", False),
        ("text\n\n@example-agent", True),
    ],
)
def test_explicit_address(body: str, expected: bool) -> None:
    assert explicit_mention(body, "example-agent") is expected


@pytest.mark.parametrize("actor,expected", [(102, 1), (101, 0), (999, 0)])
def test_assignment_trust(cfg: dict[str, Any], actor: int, expected: int) -> None:
    api = FakeAPI(cfg)
    api.events = [assignment(actor)]
    assert len(snapshot(api)["signals"]) == expected


@pytest.mark.parametrize(
    "mutation",
    ["bot", "app", "conflict", "unassigned", "closed", "wrong_owner", "wrong_parent"],
)
def test_assignment_fails_closed(cfg: dict[str, Any], mutation: str) -> None:
    api = FakeAPI(cfg)
    api.events = [assignment()]
    if mutation == "bot":
        api.events[0]["assigner"]["type"] = "Bot"
    if mutation == "app":
        api.events[0]["performed_via_github_app"] = {"id": 999}
    if mutation == "conflict":
        api.events[0]["actor"] = user(999)
    if mutation == "unassigned":
        api.issue["assignees"] = []
    if mutation == "closed":
        api.issue["state"] = "closed"
    if mutation == "wrong_owner":
        api.repo["owner"] = dict(id=999, type="User", login="example-owner")
    if mutation == "wrong_parent":
        api.issue["repository_url"] = "https://api.github.com/repos/example-other/repo"
    if mutation in ("wrong_owner", "wrong_parent"):
        with pytest.raises(g.Fault):
            snapshot(api)
    else:
        assert not snapshot(api)["signals"]


@pytest.mark.parametrize(
    "mutation,expected",
    [
        ("none", 1),
        ("edited", 0),
        ("old", 0),
        ("bot", 0),
        ("untrusted", 0),
        ("quote", 0),
    ],
)
def test_mentions(cfg: dict[str, Any], mutation: str, expected: int) -> None:
    api = FakeAPI(cfg)
    api.comments = [comment()]
    if mutation == "edited":
        api.comments[0]["updated_at"] = T2
    if mutation == "old":
        api.comments[0].update(created_at=T0, updated_at=T0)
    if mutation == "bot":
        api.comments[0]["user"]["type"] = "Bot"
    if mutation == "untrusted":
        api.comments[0]["user"] = user(999)
    if mutation == "quote":
        api.comments[0]["body"] = "> @example-agent"
    assert len(snapshot(api)["signals"]) == expected


@pytest.mark.parametrize("reviewer", [102, 108])
def test_requested_review_feedback(cfg: dict[str, Any], reviewer: int) -> None:
    cfg["trusted_reviewer_ids"] = [108]
    api = FakeAPI(cfg, pr=True)
    api.events = [
        dict(
            id=109,
            url=ROOT + "/issues/events/109",
            event="review_requested",
            created_at=T1,
            actor=user(101),
            review_requester=user(101),
            requested_reviewer=user(reviewer),
        )
    ]
    api.reviews = [
        dict(
            id=110,
            pull_request_url=ROOT + "/pulls/7",
            state="APPROVED",
            user=user(reviewer),
            submitted_at=T2,
            body="example feedback",
            commit_id="abc",
        )
    ]
    assert snapshot(api)["signals"][0]["signal"] == "handoff"
    api.events[0]["actor"] = user(999)
    assert not snapshot(api)["signals"]


def test_merge_only_trusted_human_on_agent_pr(cfg: dict[str, Any]) -> None:
    api = FakeAPI(cfg, pr=True)
    api.issue["state"] = "closed"
    api.events = [
        dict(
            id=111,
            url=ROOT + "/issues/events/111",
            event="merged",
            created_at=T1,
            actor=user(102),
        )
    ]
    assert snapshot(api)["signals"][0]["kind"] == "merge"
    api.issue["user"] = user(999)
    assert not snapshot(api)["signals"]


def test_app_first_assignment(cfg: dict[str, Any]) -> None:
    cfg["app_initiators"] = [dict(app_id=112, bot_user_id=113, repository_ids=[104])]
    api = FakeAPI(cfg)
    bot = dict(id=113, type="Bot")
    api.issue.update(user=bot, performed_via_github_app=dict(id=112))
    event = assignment()
    event.update(assigner=bot, performed_via_github_app=dict(id=112))
    api.events = [event]
    assert snapshot(api)["signals"][0]["initiator"] == "app:112"
    event["assigner"] = user(999)
    assert not snapshot(api)["signals"]


def test_freshness_dedup_and_cancellation(
    cfg: dict[str, Any], state: dict[str, Any]
) -> None:
    api, key = queued(cfg, state)
    first = copy.deepcopy(state["row"]["pending"])
    g.targeted(
        api,
        cfg,
        cfg["owner_policy"],
        state["row"],
        dict(repo_id=104, number=7, full_name="example-owner/example-repo"),
    )
    assert state["row"]["pending"] == first
    api.issue["state"] = "closed"
    g.targeted(
        api,
        cfg,
        cfg["owner_policy"],
        state["row"],
        dict(repo_id=104, number=7, full_name="example-owner/example-repo"),
    )
    assert key not in state["row"]["pending"]
    assert key in state["row"]["seen"]


def test_no_history_on_init(cfg: dict[str, Any]) -> None:
    g.initialize(cfg, T2)
    state = g.load_state(cfg)
    api = FakeAPI(cfg)
    api.events = [assignment()]
    g.targeted(
        api,
        cfg,
        cfg["owner_policy"],
        state["row"],
        dict(repo_id=104, number=7, full_name="example-owner/example-repo"),
    )
    assert not state["row"]["pending"]
    assert state["row"]["seen"]


def test_missing_corrupt_scope_and_repeat_init(
    cfg: dict[str, Any], state: dict[str, Any]
) -> None:
    with pytest.raises(g.Fault, match="state_exists"):
        g.initialize(cfg, T0)
    changed = copy.deepcopy(cfg)
    changed["trusted_actor_ids"] = [999]
    with pytest.raises(g.Fault, match="scope_changed"):
        g.load_state(changed)
    Path(cfg["state"]).write_text("{}")
    with pytest.raises(g.Fault, match="state_corrupt"):
        g.load_state(cfg)
    Path(cfg["state"]).unlink()
    with pytest.raises(g.Fault, match="state_missing"):
        g.load_state(cfg)


def test_feed_changes_do_not_change_scope(
    cfg: dict[str, Any], state: dict[str, Any]
) -> None:
    before = g.scope(cfg)
    state["feed"].update(
        validator="example validator", due=time.time() + 60, last_complete=T2
    )
    g.persist(cfg, state)
    assert g.load_state(cfg)["scope"] == before
    cfg["poll_seconds"] = 120
    cfg["reactions"] = True
    cfg["gh"] = ["/another/gh"]
    assert g.scope(cfg) == before


def test_exclusion_pins_numeric_id(cfg: dict[str, Any], state: dict[str, Any]) -> None:
    api, key = queued(cfg, state)
    cfg["owner_policy"]["exclude_repositories"] = ["EXAMPLE-OWNER/EXAMPLE-REPO"]
    g.targeted(
        api,
        cfg,
        cfg["owner_policy"],
        state["row"],
        dict(repo_id=104, number=7, full_name="example-owner/example-repo"),
    )
    assert not state["row"]["pending"]
    assert 104 in state["row"]["pins"].values()


def test_per_cycle_lock_does_not_strand(
    cfg: dict[str, Any], state: dict[str, Any]
) -> None:
    with g.locked(cfg), pytest.raises(g.Fault, match="busy"):
        g.cycle(cfg)
    with g.locked(cfg):
        assert g.load_state(cfg) == state


def test_pointer_has_no_content(cfg: dict[str, Any], state: dict[str, Any]) -> None:
    _, key = queued(cfg, state)
    item = state["row"]["pending"][key]
    item.update(body="DO NOT COPY", title="DO NOT COPY")
    text = g.brg_pointer(item)
    assert "DO NOT COPY" not in text
    assert (
        json.loads(text)["url"]
        == "https://github.com/example-owner/example-repo/issues/7"
    )


def test_retry_budget_and_spacing(
    cfg: dict[str, Any],
    state: dict[str, Any],
    monkeypatch: pytest.MonkeyPatch,
    capsys: pytest.CaptureFixture[str],
) -> None:
    api, key = queued(cfg, state)
    now = [time.time()]
    calls = []
    monkeypatch.setattr(g.time, "time", lambda: now[0])
    monkeypatch.setattr(
        g, "delivery_target", lambda: dict(instance="example-2345", member="lead-2345")
    )

    def send(target: dict[str, str], text: str, timeout: float) -> None:
        persisted = g.load_state(cfg)["row"]["hold"]["brg:" + key]
        assert persisted["result"] == "uncertain"
        calls.append(text)
        raise subprocess.TimeoutExpired("fake-brg", timeout)

    monkeypatch.setattr(g, "BRG_SEND", send)
    for attempt in range(3):
        g.produce_brg(cfg, state, time.monotonic() + 120, [], api)
        assert len(calls) == attempt + 1
        g.produce_brg(cfg, state, time.monotonic() + 120, [], api)
        assert len(calls) == attempt + 1
        now[0] += 60
    assert not state["row"]["pending"]
    assert not state["row"]["hold"]
    assert "abandoned" in capsys.readouterr().err


def test_commit_retires_before_reaction(
    cfg: dict[str, Any], state: dict[str, Any], monkeypatch: pytest.MonkeyPatch
) -> None:
    api, key = queued(cfg, state)
    cfg["reactions"] = True
    monkeypatch.setattr(
        g,
        "delivery_target",
        lambda: {"instance": "example-2345", "member": "lead-2345"},
    )
    monkeypatch.setattr(g, "BRG_SEND", lambda target, text, timeout: "example-bus-id")
    plans = []
    g.produce_brg(cfg, state, time.monotonic() + 120, plans, api)
    assert key not in g.load_state(cfg)["row"]["pending"]
    assert len(plans) == 1 and plans[0]["targets"] == {"104:issue:7": 7}
    assert not g.read_reactions(cfg)["targets"]


def test_staged_transport_does_not_consume(
    cfg: dict[str, Any], state: dict[str, Any]
) -> None:
    api, key = queued(cfg, state)
    g.produce_brg(cfg, state, time.monotonic() + 120, [], api)
    assert key in state["row"]["pending"]
    assert state["row"]["hold"]["brg_unavailable"]["reason"] == "brg_not_ready"


def test_reaction_path_and_arguments(
    cfg: dict[str, Any], monkeypatch: pytest.MonkeyPatch
) -> None:
    calls = []

    def run(argv: list[str], timeout: float) -> subprocess.CompletedProcess[str]:
        calls.append(argv)
        return subprocess.CompletedProcess(
            argv, 0, 'HTTP/2.0 201 Created\n\n{"content":"eyes","user":{"id":101}}', ""
        )

    monkeypatch.setattr(g, "gh", run)
    assert g.post_eyes(cfg, "104:issue:7", 15)[0] == 201
    assert calls[0][-1] == "repositories/104/issues/7/reactions"
    assert "POST" in calls[0] and "content=eyes" in calls[0]
    with pytest.raises(g.Fault):
        g.post_eyes(cfg, "104:evil:7", 15)
    assert len(calls) == 1


def test_reaction_deadline_stops_before_read(
    cfg: dict[str, Any], state: dict[str, Any], monkeypatch: pytest.MonkeyPatch
) -> None:
    _, key = queued(cfg, state)
    cfg["reactions"] = True
    plan = g.item_reactions(
        cfg, cfg["owner_policy"], state["row"], [key], "example-batch"
    )
    g.prepare_reactions(cfg, plan)
    monkeypatch.setattr(g, "read_config", lambda path: cfg)
    result = g.send_reactions(cfg, plan, time.monotonic() + 1)
    assert result["skipped"] == 1
    assert not g.read_reactions(cfg)["targets"]


def test_watch_recovers_and_health_is_atomic(
    cfg: dict[str, Any], tmp_path: Path, monkeypatch: pytest.MonkeyPatch
) -> None:
    calls = []
    sleeps = []
    monkeypatch.setenv("BORGCHART_STATE_DIR", str(tmp_path))
    monkeypatch.setattr(g, "read_config", lambda path: cfg)

    def cycle(config: dict[str, Any]) -> dict[str, Any]:
        calls.append(1)
        if len(calls) == 1:
            raise KeyError("example")
        return dict(ok=True, detail="poll complete")

    monkeypatch.setattr(g, "cycle", cycle)
    monkeypatch.setattr(g.time, "sleep", lambda delay: sleeps.append(delay))
    g.watch("example", lambda: len(calls) == 2)
    assert len(calls) == 2 and sleeps == [60]
    assert json.loads((tmp_path / "health.json").read_text())["ok"]
    assert (tmp_path / "health.json").stat().st_mode & 0o077 == 0


def test_native_target(monkeypatch: pytest.MonkeyPatch) -> None:
    monkeypatch.setenv("BORGCHART_INSTANCE", "example-2345")
    monkeypatch.setenv("BORGCHART_DELIVER_TO", "lead-2345")
    assert g.delivery_target() == dict(instance="example-2345", member="lead-2345")
    monkeypatch.delenv("BORGCHART_DELIVER_TO")
    with pytest.raises(g.BrgUnavailable):
        g.delivery_target()


def test_feed_empty_and_page_cap(cfg: dict[str, Any], state: dict[str, Any]) -> None:
    class Feed:
        poll_seconds = 60

        def request(
            self, path: str, validator: str | None = None
        ) -> tuple[int, dict[str, str], Any]:
            return 200, {}, []

    g.collect_feed(Feed(), cfg, state)
    assert state["feed"]["last_complete"] is None
    assert state["feed"]["due"] > time.time()


def test_pagination_cannot_escape() -> None:
    with pytest.raises(g.Fault):
        g.next_page(
            '<https://example.invalid/notifications?page=2>; rel="next"',
            "/notifications?page=1",
        )
    assert (
        g.next_page(
            '<https://api.github.com/notifications?page=2>; rel="next"',
            "/notifications?page=1",
        )
        == "/notifications?page=2"
    )
