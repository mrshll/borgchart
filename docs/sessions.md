# Sessions, names and the watcher (proposed)

**Status: proposed, not implemented, and partly superseded by [concepts.md](concepts.md).** Where this page differs from it, concepts.md wins: instances (`<team>-<suffix>`) replace deployments and teams-as-running-things, `create`, `start`, `stop` and `delete` replace `up`, `down` and `--forget`, and addresses are `[instance/]member`. `brg ls` and running from anywhere already work that way. The watcher below is still a proposal. This describes how brg is meant to work from anywhere, with tmux as its server. Nothing here exists in code yet; the commands below are proposed names. The bus pieces under review are described in [bus.md](bus.md) under their current (pre-rename) command names.

## The idea

brg already runs every deployment inside its own private tmux server (`tmux -L borgchart`). tmux is a long-lived server that keeps processes running, knows every session and window, and lets any client query it. So brg doesn't need a daemon of its own: **tmux is brg's server.**

- **Run from anywhere.** Commands take a team by name instead of finding `borgchart.yaml` in the current folder. `brg ls` lists teams by asking tmux which brg sessions exist, plus brg's own record of teams that are stopped.
- **Sessions are kept minimal.** Most people use one: today's `-L borgchart`. `--session NAME` exists only to keep a separate group of teams on its own tmux server, and may change or go away.
- **A watcher runs inside tmux.** Each team with a bus gets one hidden tmux window, the *watcher*, which notices new messages and either nudges the member to read them or alerts a person. Because tmux runs it, tmux keeps it alive: no systemd unit, no launchd agent, and no process a member has to spawn.

Limits, stated up front:

- **tmux is not a permission broker.** Anything running as you can reach the tmux socket and type into, read or stop any member. Nothing here isolates members from each other.
- **A reboot ends the server.** tmux doesn't survive a reboot, so neither do members or the watcher. Messages survive only if their storage does. Storage under `/tmp` (needed today when a member's sandbox can write nowhere else) is cleared by a reboot on many systems.

## Sessions

Sessions are implicit, and deliberately minimal for now:

- The default is today's tmux server, `-L borgchart`. Nothing needs to be chosen.
- `--session NAME` (or `$BORGCHART_SESSION`) uses a separate tmux server, `-L borgchart-NAME`. It is created when a team is first started with it and disappears when its last team is stopped.
- `NAME` follows member-name rules (`[a-z][a-z0-9_-]{0,31}`). Members and plugin processes get `BORGCHART_SESSION`, so their own commands stay in their session.
- This replaces today's `BORGCHART_TMUX_SOCKET` variable.
- There are no session commands. `brg ls` shows the current session's teams; `--all-sessions` shows every session's.

Whether sessions are needed in this form at all is open. Nothing else in this design depends on more than the one default session.

## Names and addressing

A team is named by `name:` in `borgchart.yaml`, defaulting to the deployment file's directory name. Names are unique within a session. A second deployment with the same name is refused; that includes two checkouts of the same example. The refusal points to `name:`.

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

**Registration.** `brg up` (from the folder, or with `--file`) records the team: name, absolute deployment file and deployment ID. The record is kept in two places:
- brg's state (`$XDG_STATE_HOME/borgchart/teams.json`);
- the team's own tmux session, as user options (`@brg-team`, `@brg-file`).

A running team is read from tmux and a stopped one from the state file. If the two disagree, the command refuses and says which to fix. A team whose message storage has disappeared (for example `/tmp` cleared by a reboot) shows as `messages lost`, not just `stopped`. A stopped team whose deployment file no longer exists is dropped from the list automatically.

## Commands (proposed)

A small set for people at the top level. Everything agents use for messages sits under `brg message`. Rare operations are folded into these commands, not given their own.

**Top level:**

| Command | Today / in review | What it does |
| --- | --- | --- |
| `brg validate` | same | Check a team and deployment without starting anything |
| `brg ls [--all-sessions]` | — | One line per team: name, running or stopped, how many members are up, whether its watcher is running |
| `brg up [team]`, `brg down [team]` | same | Start or stop a team, or one member (`--member`). `up` prints the team as a tree (below). On a running team it starts only what's missing, including a missing watcher. |
| `brg status [team] [--check]` | `status`, `run show`, `bus check`, `bus inspect`, `task list`, `bus alerts` | The same tree as `up`, with each member's live state, plus open tasks and questions, open alerts and the watcher's health. Without a team, every team in the session. `--check` exits non-zero when anything needs a person, for scripts and monitors. |
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

### What `up` and `status` print

`brg up` prints the team as a tree, with each member's vendor, model and what happened:

```
$ brg up oncall
oncall
└─ lead        claude opus high      started
   ├─ helper   codex example-model   started
   └─ reviewer claude sonnet medium  not started (start: false)
```

A member that couldn't start says why on its own line (`failed: PAGER_TOKEN is not set`), and one already running says `already running`. `brg status oncall` prints the same tree with live states (`idle`, `working`, `blocked`, `stopped`, …). Below the tree it lists open tasks and questions, open alerts and the watcher's health. `brg status` with no team prints every team's tree.

**Errors talk about teams and members**, never about storage or internal identifiers. For example:

```
brg: oncall/helper's instruction file changed since the team started; restart the team to apply it: brg down oncall && brg up oncall
brg: no team named `oncal` in this session (teams: oncall, triage)
brg: `lead` is in two running teams (oncall, triage); name one: oncall/lead
```

### Commands dropped or folded

| Was proposed | Now |
| --- | --- |
| `task ls\|show` | Open tasks and questions appear in `brg status TEAM`; one task's history is `brg message read --task T`. |
| `alert test\|ls\|resolve` | Open alerts appear in `brg status`. An alert resolves itself when its cause clears: the member reads its inbox, a draft is cleared, or the watcher is running again. A test alert is `brg status --check --test-alert`. |
| `attention resolve` | Dropped. A nudge left half-done by a crash raises an alert and is never retried. It clears itself the next time that member lists its own inbox (it has then seen the messages) or is restarted. |
| `run carry` | Dropped. When brg rolls a team's message storage over (see Versions), it carries unfinished work over by itself. |
| `session ls` | No session commands; `brg ls --all-sessions` lists every session's teams. |
| `forget` | Dropped. Stopped teams whose deployment file is gone fall off the list, and `brg down TEAM --forget` drops one by hand. |
| `check` | `brg status --check`. |
| `run rebind`, `up --upgrade` | Dropped: brg isn't pinned (see Versions). |

## The watcher

*Being re-checked:* members may be able to wake themselves with a background shell command instead of a typed reminder. That is being tested on the real vendor CLIs, and the result may narrow the watcher to detecting members that aren't waiting, raising alerts, and nudging only vendors that can't wake themselves.

Each team with a bus gets a hidden tmux window, `_watcher`, next to its members. It notices new messages for each member. For each one it either types one fixed reminder into an idle member's input box ("new messages in your inbox"), under strict checks, or alerts a person when it can't safely do that. It never types a message's contents. It runs the installed brg, and restarts itself when that changes (see Versions). The name `watcher` is reserved: no plugin may use it, since plugin windows are `_<plugin>`.

**Environment.** The watcher starts from the deployment's frozen base environment only. It reads the alert plugin's frozen values only when it actually invokes the alert action, as the alert design requires. It never holds plugin secrets.

**Starting it.**
- `brg up` starts it with the team.
- Commands that change something for that team restart it if it's missing: `up`, `message send`, `message ack`, and a member's own `message inbox`/`message wait`.
- Read-only commands never start anything: `ls`, `status`, `message read`, and anyone else's `message inbox`. They show whether the watcher is running and whether its heartbeat is fresh.

**Kept alive by tmux.** A `pane-died` hook scoped to the watcher's window only respawns it, e.g. `set-hook -w -t TEAM:_watcher pane-died 'respawn-pane'`. brg's tmux config sets `remain-on-exit` for every pane, so the hook must not touch members.
- The watcher never exits cleanly except at `down`.
- tmux keeps no state, so the crash-loop counter lives in the watcher's own state file. After a few quick crashes the respawned process records the reason, raises a `worker-unavailable` alert, and stays stopped.
- When the installed brg changes, the watcher restarts on the new one. It records that this was a planned restart rather than a crash, then exits, and the hook starts the new brg. If the new brg can't read the team's message format, the watcher refuses as described under Versions, raises an alert and stays stopped.

**Work.** The loop from the bus design: which messages each member has seen, how many nudges have been tried, nudges in progress, and deadlines. It nudges members through the guarded wake adapter and raises alerts through the alert action. It runs outside every member's sandbox, because tmux started it. It writes a heartbeat that `status` and `ls` read.

**When it can't help.**

| Case | Who notices |
| --- | --- |
| It keeps crashing | The last attempt raises a `worker-unavailable` alert before staying stopped. |
| Hung but alive | `status` reports a stale heartbeat (and `--check` fails). It is never restarted automatically. |
| The whole tmux server gone (killed, reboot) | An outside caller's `message send` finds the team registered but its session missing. It reports that nobody will be nudged and raises one `worker-unavailable` alert, through the alert action's isolated environment, not the caller's. Members can't hit this case. |

brg itself runs nothing on a schedule. For these cases the backstop is whatever watches the team from outside, such as a person, a monitoring job or a supervising agent running `brg status --check` on a schedule.

**Stopping.** `brg down` stops the watcher with the team. The watcher never creates a tmux session and never starts members.

## Versions (decided)

**brg isn't pinned.** A team's brg calls (members' messages, the watcher, plugin shims) use whatever `brg` is installed. Rebuilding or reinstalling brg takes effect at the next call, and most rebuilds are harmless or are fixes you want.

**The message format is versioned.** A team's message storage records the format version it was created with (the `schema` already in every message). A brg that can't read that format refuses before reading or writing anything, with an exact message:

```
brg: team `oncall`'s messages use format 1; this brg uses format 2.
Restart the team to start fresh storage in the new format: brg down oncall && brg up oncall
(Open work in the old format isn't carried over; the old messages stay on disk.)
```

**Restarting is the upgrade.** When `brg up` starts a team whose configuration has changed, or whose storage has reached its age limit, brg silently starts fresh storage for it and carries over its unfinished work: open tasks, unanswered questions and unread messages. You never name or manage this; it is internal, and nothing in `borgchart.yaml` refers to it (the `run:` and `continues:` settings in the current proposal, #7, are dropped). A message format change is the exception: a brg that can't read the old messages can't carry them, so it starts fresh storage, leaves the old messages on disk and says what was left behind. Moving brg between message formats without that gap is future work.

**Configuration stays frozen while a team runs.** Changing, say, a member's model from medium to hard in the team file takes effect at the next `brg down`/`brg up`. Starting one member into a running team with changed files is refused, with the message shown above.

**Long-running pieces.** The watcher restarts itself when the installed brg changes (above). Plugin processes keep running the code they started with until the team is restarted. They only read and write their own files, not the message storage.

**Future work, not now:** moving a team to a new message format without stopping it, and carrying its open work across. Until team names land, restart hints use the form that works today: `brg down && brg up` in the team's folder, `--file …` elsewhere.

## What this simplifies in the bus pieces

Mostly the attention worker, which becomes the watcher, and everything that pinned the brg program. Per piece:

| Piece | What changes |
| --- | --- |
| Foundation (#7) | **Removes** runtime generations (`runtime/generations/`, `Generation` with its binary hash and pinned copy), `runtime/bin/<sha>/brg`, `runtime/generation.lock`, and the generation field of incarnations. **Keeps** the message format, write helper, privacy checks, caller context, outcome codes and the configuration snapshot. The message `schema` becomes the format version that brg checks. The worker's lock and state files become the watcher's lock, state and heartbeat. Members gain `BORGCHART_SESSION`, and `BORGCHART_TMUX_SOCKET` goes. In the deployment file, `bus:` loses `run:` and `continues:` (storage identity and rollover become internal), and the file gains `name:`. |
| Alert action (#8) | None in the library. Its three commands go: alerts show in `status` and resolve themselves when their cause clears. It is invoked by the watcher, and by an outside caller in the session-gone case. |
| Inbox (#9) | **Removes** the check that refuses changes from anything but the pinned binary, and taking `generation.lock` around writes. A format mismatch becomes a refusal with the restart message, instead of a corrupt-store error. Commands move under `brg message` and find the team by name. `bus check`/`inspect` fold into `status`. Nothing else in the store changes. |
| Lifecycle (#10) | **Removes** copying and pinning the binary, generations and `run rebind`, the "different binary" refusal on child starts, and the `brg`/`borgchart` shims and pinned `BORGCHART_BIN` in members. **Keeps** validate-before-mutate, serialized starts, frozen configuration (a changed file still refuses a child start), incarnations with pane identity, and fail-closed liveness and `down`. **Adds** `--session`, registration, starting the watcher, the team tree printed by `up` and `status`, team/member wording in errors, and silent storage rollover at `up` when the format or configuration changed. `run show` and `run rebind` go. |
| Wake adapter (#11) | None. It is called by the watcher. |
| Task facts (#12) | None in the fold or checks. `task list/show` fold into `status` and `message read --task`. `bus carry` becomes internal: rollover at `up` does it by itself. |
| Attention worker (not built) | **Large.** It becomes the watcher. Gone: the on-demand spawn from inside a member, the spawn handoff, the "final empty scan, then exit" race, proving that a worker spawned in a member's sandbox survives its tool call and can reach tmux, `bus service ensure`, and the generation lock it would have held. What remains is the loop, the heartbeat, the crash-loop cap and restarting on a new brg. |
| `brg send` | Renamed `brg prompt`, unchanged otherwise. |

## Effects on work in review

- **Foundation and lifecycle (#7, #10):** removing generations, pinning and the user-visible run settings, then adding session selection, registration, the watcher, the tree output and silent rollover at `up`. These come as follow-up changes after this design is approved; #7 and #10 themselves aren't reopened.
- **Wake adapter (#11):** no code change; its caller becomes the watcher.
- **Inbox, alert, task facts (#9, #8, #12):** removing the pinned-binary check, plus the command renames and folds, land in one follow-up change.
- **Team configurations** that call brg move from `--file` to `team/member` names, and from `send` to `prompt`, after the rename.

## Try-it sections to update

The pull requests in review describe their current behavior, so their try-it sections keep the old terms until the follow-up changes land. Those changes rewrite them:

- **#7 (foundation):** `validate` with a `bus:` section that has no `run:`.
- **#9 (messages):** `brg message send oncall/lead …`, `brg message inbox oncall/lead`, and `brg status oncall` in place of `bus check`.
- **#10 (lifecycle):**
  - `brg up oncall` printing the tree;
  - `brg status oncall` live;
  - `brg up --member helper` from a contaminated shell, printing `helper … started`;
  - the changed-file refusal in team/member words;
  - `brg ls`.

  No `run show`, `run rebind` or second binary.
- **#12 (tasks):** tasks shown in `brg status oncall` and `brg message read --task t-1`; rollover shown by editing the team and running `brg down oncall && brg up oncall`.
- **#8 (alerts):** an open alert in `brg status`, and `brg status --check --test-alert`.

## Open questions

See the pull request proposing this document.
