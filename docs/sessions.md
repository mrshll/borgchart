# Sessions, names and the attention window (proposed)

**Status: proposed, not implemented.** This describes how brg is meant to work from anywhere, with tmux as its server. Nothing here exists in code yet; the commands below are proposed names. The bus pieces under review are described in [bus.md](bus.md) under their current (pre-rename) command names.

## The idea

brg already runs every deployment inside its own private tmux server (`tmux -L borgchart`). tmux is a long-lived server that keeps processes running, knows every session and window, and lets any client query it. So brg doesn't need a daemon of its own: **tmux is brg's server.**

- **Run from anywhere.** Commands take a team by name instead of finding `borgchart.yaml` in the current folder. `brg ls` lists teams by asking tmux which brg sessions exist, plus brg's own record of teams that are stopped.
- **Sessions are tmux servers.** `brg --session NAME` uses the tmux server `-L borgchart-NAME`; the default session is today's `-L borgchart`. Each session is an independent group of teams that can be stopped without touching the others.
- **The attention worker is a tmux window.** The bus's wake and alert loop runs in a hidden window of each team's tmux session, so tmux keeps it running. No systemd unit, no launchd agent, and no process a member has to spawn.

Limits, stated up front:

- **tmux is not a permission broker.** Anything running as you can reach the tmux socket and type into, read or stop any member. Nothing here isolates members from each other.
- **A reboot ends the server.** tmux doesn't survive a reboot, so neither do members or the attention window. Bus records survive only if their root does. A root under `/tmp` (needed today when a member's sandbox can write nowhere else) is cleared by a reboot on many systems. The run-age cap and rotation in bus.md stay as they are.

## Sessions

| | Default session | Named session |
| --- | --- | --- |
| Select | nothing, or `--session default` | `--session NAME`, or `$BORGCHART_SESSION` |
| tmux server | `-L borgchart` | `-L borgchart-NAME` |

`NAME` follows member-name rules (`[a-z][a-z0-9_-]{0,31}`). Members and plugin processes get `BORGCHART_SESSION`, so their own commands stay in their session. `brg session ls` lists sessions; `brg ls` lists the teams in one session (`--all-sessions` for every session). This replaces today's `BORGCHART_TMUX_SOCKET` variable.

## Names and addressing

A team is named by `name:` in `borgchart.yaml`, defaulting to the deployment file's directory name. Names are unique within a session. A second deployment with the same name is refused; that includes two checkouts of the same example. The refusal points to `name:` or `--session`.

Every command that targets a member takes `[team/]member`:

```
brg message oncall/lead "the build is green"
brg message lead "…"          # team omitted when unambiguous
```

The team may be left out when:

1. the command runs inside a member or a plugin process: its own team;
2. the current folder holds a deployment file: that team;
3. exactly one **running** team in the session has a member with that name.

Otherwise the command refuses and lists the matches. A message to a stopped team must name the team; records are durable, so that is allowed. A member can only address its own team: `brg message other/x` from inside a member is refused, like `--external` is today. There are no generated IDs. Team name plus member name is the identity, and record IDs work the same way (`[team/]m42`).

Commands that target a whole team take a team name (`brg up oncall`, `brg down oncall`). In a deployment's folder the name can be omitted, as today.

**Registration.** `brg up` (from the folder, or with `--file`) records the team in its session: name, absolute deployment file and deployment ID. The record is kept in two places:
- brg's state (`$XDG_STATE_HOME/borgchart/sessions/NAME/teams.json`);
- the tmux session's user options (`@brg-team`, `@brg-file`).

A running team is read from tmux and a stopped one from the state file. If the two disagree, the command refuses and says which to fix. `brg forget TEAM` drops a stopped team's record. A team whose run root has disappeared (for example `/tmp` cleared by a reboot) shows as `run missing`, not `stopped`.

## Commands (proposed)

Everyday verbs stay at the top level; rare ones are grouped.

| Proposed | Today / in review | What it does |
| --- | --- | --- |
| `brg ls` | — | Teams in the session: running/stopped/run missing, attention window state |
| `brg up [team]`, `brg down [team]` | same | Start or stop a team (or `--member`). `up` on a running team only recreates a missing attention window. |
| `brg up --upgrade [--reason TEXT] [team]` | `brg run rebind` | Move a stopped team's run to this brg binary (see Pinning) |
| `brg status [team]` | `brg status`, `brg run show` | Members, states, attention, run, generation and pinned binary |
| `brg attach [team/]member`, `brg logs [team/]member` | same | As today, by name from anywhere |
| `brg send [team/]member TEXT` | same | Type text into a member now (see below) |
| `brg message [team/]member TEXT` (or `--record FILE`) | `brg msg publish` | Publish a durable record |
| `brg inbox [[team/]member]` | `brg inbox` | List unacknowledged records |
| `brg read [team/]ID` | `brg msg read` | Show one record |
| `brg ack [team/]ID…` | `brg msg ack` | Acknowledge records |
| `brg wait [[team/]member] --timeout S` | `brg inbox wait` | Wait for a new record |
| `brg check [team]` | `brg bus check`, `brg bus inspect` | Store health, run age, attention window and heartbeat |
| `brg task ls\|show` | `brg task list\|show` | Task state |
| `brg alert test\|ls\|resolve` | `brg bus alert …`, `brg bus alerts` | The alert action and open alerts |
| `brg attention resolve [team/]member` | `brg bus attention resolve` | Clear a wake left in flight, after a person has looked |
| `brg run carry --from RUN` | `brg bus carry` | Carry unresolved work into a new run |
| `brg session ls`, `brg forget TEAM` | — | Sessions; drop a stopped team's record |

An outside caller still names itself with `--external NAME` (refused inside a member, as today), and `external:NAME` remains the address of records sent out of a team. A name equal to a member of the target team is refused.

The bus stays optional. Without `bus:` in a deployment, the record commands say the team has no bus, and everything else works as today.

**`brg send` and `brg message`.** `send` types text into a member's input box right now and presses Enter, under its existing checks; it is for people. `message` writes a durable record that is never typed. If the member is idle and its terminal qualifies, the attention window then types one fixed reminder to read its inbox.

## The attention window

Each team with a bus gets a hidden window, `_attention`, in its tmux session. It runs `brg _attention` with the run's pinned binary. The name `attention` is reserved: no plugin may use it, since plugin windows are `_<plugin>`.

**Environment.** The window starts from the deployment's frozen base environment only. It reads the alert plugin's frozen values only at the moment it invokes the alert action, as the alert design requires. It never holds plugin secrets.

**Starting it.**
- `brg up` creates it with the session.
- Mutating commands for that team recreate it if it's missing: `up`, `message`, `ack`, and a member's own `inbox`/`wait`.
- Read-only commands never start anything: `ls`, `status`, `check`, `read`, and anyone else's `inbox`. They show whether the window is running and whether its heartbeat is fresh.

**Kept alive by tmux.** A `pane-died` hook scoped to that window only respawns it, e.g. `set-hook -w -t TEAM:_attention pane-died 'respawn-pane'`. brg's tmux config sets `remain-on-exit` for every pane, so the hook must not touch members.
- The window never exits cleanly except at `down`.
- tmux keeps no state, so the crash-loop counter lives in the window's own state file. After a few quick crashes the respawned process records the reason, raises a `worker-unavailable` alert, and stays dead.
- `--upgrade` recreates the window rather than respawning it, because a respawn reuses the old command line and its pinned path.

**Work.** The loop from the bus design: observation watermarks, episodes, budgets, in-flight intents and deadlines. It wakes members through the guarded wake adapter and raises alerts through the alert action. It runs outside every member's sandbox, because tmux started it. It holds `runtime/generation.lock` shared for its life, and it writes a heartbeat that `status`, `check` and `ls` read.

**When it can't help.**

| Case | Who notices |
| --- | --- |
| Crash loop given up | The last instance raises `worker-unavailable` before staying dead. |
| Hung but alive | `status`/`check`/`ls` report a stale heartbeat. They never restart it. |
| The whole tmux server gone (killed, reboot) | An outside caller's `message` finds the team registered but its session missing. It reports `attention-unavailable` and raises one `worker-unavailable` alert, through the alert action's isolated environment, not the caller's. Members can't hit this case. Nothing else notices until someone runs a command. |

An operator's or supervisor's periodic `brg check` is the backstop for these cases.

**Stopping.** `brg down` stops the window with the session. It never creates a tmux session and never starts members.

## Pinning and upgrades

**Recommendation: keep pinning; make upgrade an explicit flag on `up`; let any brg hand off to the pinned copy.**

- **Why keep pinning.** Each run keeps its own copy of the `brg` binary. Members' shims, gates, plugin processes and the attention window all run that copy, so rebuilding or reinstalling brg can't change a running team. That matters most when a team develops brg itself. It costs one file copy per upgrade.
- **Run from anywhere needs a hand-off.** Today every bus mutation refuses unless it is run by the pinned binary. So `brg message oncall/lead …` from an ordinary shell would be refused. Instead, any brg that resolves a team and isn't that team's pinned binary re-executes the pinned binary with the same arguments. The pinning check still holds, and nobody needs to look up the pinned path. Read-only commands can run in place.
- **Upgrades are explicit.** `brg up --upgrade [--reason TEXT]` on a stopped team moves its run to the invoking binary. It uses the same stopped-run, `generation.lock` and no-wake-in-flight checks as the rebind command in review, and it recreates the attention window. A plain `brg up` with a different binary never switches silently: it hands off to the pinned copy. Rollback is `brg up --upgrade` run with the old binary, whose pinned copy is kept. This replaces `brg run rebind`.

## What this simplifies in the bus pieces

Mostly the attention worker. Per piece:

| Piece | What changes |
| --- | --- |
| Foundation (#7) | Small. Record format, write helper, privacy checks, caller context and outcome codes stay. The worker's lock and state files become the window's lock, state and heartbeat. Members gain `BORGCHART_SESSION`, and `BORGCHART_TMUX_SOCKET` goes. The deployment file gains `name:`. The run root, run-age cap and rotation are unchanged. |
| Alert action (#8) | None in the library. Commands become `brg alert …`. It is invoked by the attention window, and by an outside caller in the session-gone case. |
| Inbox (#9) | None in the store. Commands are renamed (table above) and find the team by name. Publishing reports whether the attention window is running. |
| Lifecycle (#10) | Moderate: `--session` replaces the environment variable; `up` registers the team and creates the attention window; the pinned-binary hand-off; `up --upgrade` replaces `run rebind`; `run show` folds into `status`. Validate-before-mutate, serialized starts, pinning, incarnations and fail-closed liveness all stay. |
| Wake adapter (#11) | None. It is called by the attention window. |
| Task facts (#12) | None in the fold or checks. Commands become `brg task …`, and carry becomes `brg run carry`. |
| Attention worker (not built) | **Large.** Gone: the on-demand spawn from inside a member, the spawn handoff, the "final empty scan, then exit" race, and proving that a worker spawned in a member's sandbox survives its tool call and can reach tmux. `bus service ensure` also goes: `brg up` on a running team recreates a missing window, and tmux respawns it. What remains is the loop, the heartbeat and the crash-loop cap. |

## Effects on work in review

- **Lifecycle (#10):** session selection, registration, the attention window in `up`/`down`, the pinned-binary hand-off and `up --upgrade`. These come as follow-up changes after this design is approved; #10 itself isn't reopened.
- **Wake adapter (#11):** no code change; its caller becomes the attention window.
- **Inbox, alert, task facts (#9, #8, #12):** the command renames land in one follow-up rename change.
- **Team configurations** that call brg move from `--file` to `team/member` names after the rename.

## Open questions

See the pull request proposing this document.
