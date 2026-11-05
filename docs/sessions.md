# Sessions, names and the watcher (proposed)

**Status: proposed, not implemented.** This describes how brg is meant to work from anywhere, with tmux as its server. Nothing here exists in code yet; the commands below are proposed names. The bus pieces under review are described in [bus.md](bus.md) under their current (pre-rename) command names.

## The idea

brg already runs every deployment inside its own private tmux server (`tmux -L borgchart`). tmux is a long-lived server that keeps processes running, knows every session and window, and lets any client query it. So brg doesn't need a daemon of its own: **tmux is brg's server.**

- **Run from anywhere.** Commands take a team by name instead of finding `borgchart.yaml` in the current folder. `brg ls` lists teams by asking tmux which brg sessions exist, plus brg's own record of teams that are stopped.
- **Sessions are tmux servers.** `brg --session NAME` uses the tmux server `-L borgchart-NAME`; the default session is today's `-L borgchart`. Each session is an independent group of teams that can be stopped without touching the others.
- **A watcher runs inside tmux.** Each team with a bus gets one hidden tmux window, the *watcher*, which notices new messages and either nudges the member to read them or alerts a person. Because tmux runs it, tmux keeps it alive: no systemd unit, no launchd agent, and no process a member has to spawn.

Limits, stated up front:

- **tmux is not a permission broker.** Anything running as you can reach the tmux socket and type into, read or stop any member. Nothing here isolates members from each other.
- **A reboot ends the server.** tmux doesn't survive a reboot, so neither do members or the watcher. Messages survive only if their storage does. Storage under `/tmp` (needed today when a member's sandbox can write nowhere else) is cleared by a reboot on many systems. The run-age cap and rotation in bus.md stay as they are.

## Sessions

| | Default session | Named session |
| --- | --- | --- |
| Select | nothing, or `--session default` | `--session NAME`, or `$BORGCHART_SESSION` |
| tmux server | `-L borgchart` | `-L borgchart-NAME` |

`NAME` follows member-name rules (`[a-z][a-z0-9_-]{0,31}`). Members and plugin processes get `BORGCHART_SESSION`, so their own commands stay in their session. This replaces today's `BORGCHART_TMUX_SOCKET` variable.

## Names and addressing

A team is named by `name:` in `borgchart.yaml`, defaulting to the deployment file's directory name. Names are unique within a session. A second deployment with the same name is refused; that includes two checkouts of the same example. The refusal points to `name:` or `--session`.

Every command that targets a member takes `[team/]member`:

```
brg message send oncall/lead "the build is green"
brg prompt lead "…"           # team omitted when unambiguous
```

The team may be left out when:

1. the command runs inside a member or a plugin process: its own team;
2. the current folder holds a deployment file: that team;
3. exactly one **running** team in the session has a member with that name.

Otherwise the command refuses and lists the matches. A message to a stopped team must name the team; messages are durable, so that is allowed. A member can only address its own team: sending to `other/x` from inside a member is refused, like `--external` is today. There are no generated IDs. Team name plus member name is the identity, and message IDs work the same way (`[team/]m42`).

Commands that target a whole team take a team name (`brg up oncall`, `brg down oncall`). In a deployment's folder the name can be omitted, as today.

**Registration.** `brg up` (from the folder, or with `--file`) records the team in its session: name, absolute deployment file and deployment ID. The record is kept in two places:
- brg's state (`$XDG_STATE_HOME/borgchart/sessions/NAME/teams.json`);
- the tmux session's user options (`@brg-team`, `@brg-file`).

A running team is read from tmux and a stopped one from the state file. If the two disagree, the command refuses and says which to fix. A team whose run storage has disappeared (for example `/tmp` cleared by a reboot) shows as `run missing`, not `stopped`. A stopped team whose deployment file no longer exists is dropped from the list automatically.

## Commands (proposed)

A small set for people at the top level. Everything agents use for messages sits under `brg message`. Rare operations are folded into these commands, not given their own.

**Top level:**

| Command | Today / in review | What it does |
| --- | --- | --- |
| `brg validate` | same | Check a team and deployment without starting anything |
| `brg ls [--all-sessions]` | — | Teams in the session: running, stopped or run missing; whether their watcher is running |
| `brg up [team]`, `brg down [team]` | same | Start or stop a team, or one member (`--member`). `up` on a running team only restarts a missing watcher. |
| `brg status [team] [--check]` | `status`, `run show`, `bus check`, `bus inspect`, `task list`, `bus alerts` | Members and their states, open tasks and questions, open alerts, the watcher's health, message storage health and run age. `--check` exits non-zero when anything needs a person, for scripts and monitors. |
| `brg attach [team/]member`, `brg logs [team/]member` | same | As today, by name from anywhere |
| `brg prompt [team/]member TEXT` | `brg send` | Type text into a member's input box now and press Enter (renamed from `send`; same checks) |
| `brg message …` | `msg …`, `inbox …` | The message commands below |

**`brg message`**, mostly used by agents:

| Command | Today / in review | What it does |
| --- | --- | --- |
| `brg message send [team/]member TEXT` (or `--record FILE`) | `brg msg publish` | Send a durable message |
| `brg message inbox [[team/]member] [--all]` | `brg inbox` | List unacknowledged messages |
| `brg message read [team/]ID` | `brg msg read` | Show one message, or a task's history with `--task T` |
| `brg message ack [team/]ID…` | `brg msg ack` | Mark messages as handled |
| `brg message wait [[team/]member] --timeout S` | `brg inbox wait` | Wait for a new message |

An outside caller names itself with `--external NAME`, refused inside a member as today. `external:NAME` remains the address of messages sent out of a team. A name equal to a member of the target team is refused.

The bus stays optional. Without `bus:` in a deployment, `brg message` says the team has no messages, and everything else works as today.

**`brg prompt` and `brg message send`.** `prompt` types into a member right now; it is for people. `message send` writes a durable message that is never typed. If the member is idle and its terminal qualifies, the watcher then types one fixed reminder to read its inbox.

### Commands dropped or folded

| Was proposed | Now |
| --- | --- |
| `task ls\|show` | Open tasks and questions appear in `brg status TEAM`; one task's history is `brg message read --task T`. |
| `alert test\|ls\|resolve` | Open alerts appear in `brg status`. An alert resolves itself when its cause clears: the member reads its inbox, a draft is cleared, or the watcher is running again. A test alert is `brg status --check --test-alert`. |
| `attention resolve` | Dropped. A nudge left half-done by a crash raises an alert and is never retried. It clears itself the next time that member lists its own inbox (it has then seen the messages) or is restarted. |
| `run carry` | Dropped. `brg up` of a run that declares `continues: OLD` carries the old run's unfinished work over, once. The `continues:` line in the deployment file is the consent. |
| `session ls` | `brg ls --all-sessions`. |
| `forget` | Dropped. Stopped teams whose deployment file is gone fall off the list, and `brg down TEAM --forget` drops one by hand. |
| `check` | `brg status --check`. |
| `run rebind` | Depends on the open pinning question below. |

## The watcher

Each team with a bus gets a hidden tmux window, `_watcher`, next to its members. It notices new messages for each member. For each one it either types one fixed reminder into an idle member's input box ("new messages in your inbox"), under strict checks, or alerts a person when it can't safely do that. It never types a message's contents. It runs the run's pinned brg (see Pinning). The name `watcher` is reserved: no plugin may use it, since plugin windows are `_<plugin>`.

**Environment.** The watcher starts from the deployment's frozen base environment only. It reads the alert plugin's frozen values only when it actually invokes the alert action, as the alert design requires. It never holds plugin secrets.

**Starting it.**
- `brg up` starts it with the team.
- Commands that change something for that team restart it if it's missing: `up`, `message send`, `message ack`, and a member's own `message inbox`/`message wait`.
- Read-only commands never start anything: `ls`, `status`, `message read`, and anyone else's `message inbox`. They show whether the watcher is running and whether its heartbeat is fresh.

**Kept alive by tmux.** A `pane-died` hook scoped to the watcher's window only respawns it, e.g. `set-hook -w -t TEAM:_watcher pane-died 'respawn-pane'`. brg's tmux config sets `remain-on-exit` for every pane, so the hook must not touch members.
- The watcher never exits cleanly except at `down`.
- tmux keeps no state, so the crash-loop counter lives in the watcher's own state file. After a few quick crashes the respawned process records the reason, raises a `worker-unavailable` alert, and stays stopped.
- An upgrade restarts the window rather than respawning it, because a respawn reuses the old command line.

**Work.** The loop from the bus design: which messages each member has seen, how many nudges have been tried, nudges in progress, and deadlines. It nudges members through the guarded wake adapter and raises alerts through the alert action. It runs outside every member's sandbox, because tmux started it. It holds `runtime/generation.lock` shared for its life, and it writes a heartbeat that `status` and `ls` read.

**When it can't help.**

| Case | Who notices |
| --- | --- |
| It keeps crashing | The last attempt raises a `worker-unavailable` alert before staying stopped. |
| Hung but alive | `status` reports a stale heartbeat (and `--check` fails). It is never restarted automatically. |
| The whole tmux server gone (killed, reboot) | An outside caller's `message send` finds the team registered but its session missing. It reports that nobody will be nudged and raises one `worker-unavailable` alert, through the alert action's isolated environment, not the caller's. Members can't hit this case. |

brg itself runs nothing on a schedule. For these cases the backstop is whatever watches the team from outside, such as a person, a monitoring job or a supervising agent running `brg status --check` on a schedule.

**Stopping.** `brg down` stops the watcher with the team. The watcher never creates a tmux session and never starts members.

## Pinning (open question)

**What is pinned.** The brg *program*, not the configuration. Configuration is handled separately and already settled: a run records a fingerprint of its team and deployment files when it starts. Changing, say, a member's model from medium to hard in the team file means stopping the team and starting a new run; brg refuses to start that member into the old run.

**Why pin the program.** A team keeps running for hours or days while brg calls happen inside it: members send and acknowledge messages, the watcher nudges, plugins run through brg's shims. Suppose the team is working on brg itself, and the lead rebuilds brg halfway through. Without pinning, every member's next brg call silently runs the new build. If that build reads or writes messages differently, the team breaks mid-task. A run therefore keeps its own copy of the brg program it started with, and everything in the team uses that copy until someone deliberately upgrades.

**What changes with run-from-anywhere.** Today's rule is that a bus change must come from the run's own copy. That would refuse `brg message send oncall/lead …` typed in an ordinary shell. The proposal: any brg that targets a team hands off to that team's copy, re-running the same command with it. You never see it happen. Read-only commands run as they are.

**Upgrading** would be `brg up --upgrade [team]` on a stopped team: it switches the team to the brg you just ran and restarts the watcher. A plain `brg up` with a different brg never switches silently; it hands off to the team's copy. Rolling back is the same command run with the old brg. This would replace the `run rebind` command in review.

The alternative is no pinning: the team runs whatever brg is installed at each moment. That is simpler, but the mid-task breakage above becomes possible.

## What this simplifies in the bus pieces

Mostly the attention worker, which becomes the watcher. Per piece:

| Piece | What changes |
| --- | --- |
| Foundation (#7) | Small. Message format, write helper, privacy checks, caller context and outcome codes stay. The worker's lock and state files become the watcher's lock, state and heartbeat. Members gain `BORGCHART_SESSION`, and `BORGCHART_TMUX_SOCKET` goes. The deployment file gains `name:`. The run storage, run-age cap and rotation are unchanged. |
| Alert action (#8) | None in the library. Its three commands go: alerts show in `status` and resolve themselves when their cause clears. It is invoked by the watcher, and by an outside caller in the session-gone case. |
| Inbox (#9) | None in the store. Commands move under `brg message` and find the team by name. `bus check`/`bus inspect` fold into `status`. Sending reports whether the watcher is running. |
| Lifecycle (#10) | Moderate. `--session` replaces the environment variable. `up` registers the team and starts the watcher. `run show` folds into `status`. Pinning changes depend on the open question. Validate-before-mutate, serialized starts, incarnations and fail-closed liveness all stay. |
| Wake adapter (#11) | None. It is called by the watcher. |
| Task facts (#12) | None in the fold or checks. `task list/show` fold into `status` and `message read --task`, and `bus carry` becomes automatic at `up` with `continues:`. |
| Attention worker (not built) | **Large.** It becomes the watcher. Gone: the on-demand spawn from inside a member, the spawn handoff, the "final empty scan, then exit" race, proving a worker spawned in a member's sandbox survives its tool call and can reach tmux, and `bus service ensure`. What remains is the loop, the heartbeat and the crash-loop cap. |
| `brg send` | Renamed `brg prompt`, unchanged otherwise. |

## Effects on work in review

- **Lifecycle (#10):** session selection, registration, the watcher in `up`/`down`, and pinning changes once that's decided. These come as follow-up changes after this design is approved; #10 itself isn't reopened.
- **Wake adapter (#11):** no code change; its caller becomes the watcher.
- **Inbox, alert, task facts (#9, #8, #12):** the command renames and folds land in one follow-up change.
- **Team configurations** that call brg move from `--file` to `team/member` names, and from `send` to `prompt`, after the rename.

## Open questions

See the pull request proposing this document.
