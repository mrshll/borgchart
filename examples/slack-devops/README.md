# slack-devops: an on-call pair for one Slack channel

A cheap **triager** (Claude `haiku`) reads new posts and replies in one channel
and promotes the ones that matter to an always-on **lead** (Claude `opus`). The
lead reads the message and replies in its thread.

```text
Slack → brg-slack watch → triager → brg-slack promote → lead → brg-slack reply → thread
```

The [core Slack plugin](../../plugins/slack) is a Rust binary with no additional
runtime requirements. The template finds `brg-slack` on PATH.
Notices contain IDs only. The first poll snapshots the channel without a replay;
later polls deliver new human posts and thread replies, including broadcast
replies, skipping bots, other subtypes and edits. Each poll sends at most six
items and 900 bytes.

Both agents are `locked`: the triager can `read`, `thread` and `promote`; the lead
can `read`, `thread` and `reply`. Neither sees the bot token in its environment;
plugin shims hold it. Promotions carry a timestamp and one fixed reason:
`outage`, `deploy`, `security`, `question`, `other`. Slack text never travels
between agents. Promotions send once with `--from` so brg checks the parent
route; a failed action reports brg's error for the triager to retry. See the
plugin's delivery documentation for retry and crash limits.

Every team has a bus. Locked notices remain readable history with
`ack_required: false`, labelled “ack not required,” and are excluded from pending
inbox/wait/unacked counts. Locked recipients get no bus commands or inbox
recovery. Bodies persist under existing plugin read rights, and store size/scan
cost grow without automatic retention. Code 0 means committed and is never
republished. Failed intake sends stay pending for the next poll and can repeat
an ID notice if brg already received it.

The lead can acknowledge, suggest likely causes and ask questions. It cannot
look at systems. Giving it more read-only tools is future work.

## Setup

1. Create a Slack app with a bot user. Give it `channels:history`, `chat:write`
   and `reactions:write`, plus `groups:history` for private channels. Install it
   and invite it to the channel. Slack's app/token restrictions also apply to
   reading threads.
2. Set `SLACK_CHANNEL_ID` in `template.yaml` to the channel ID.
3. Install brg, tmux and Claude Code. From a checkout,
   `make install` puts `brg-slack` on PATH; keep `BINDIR` on PATH.
4. Pick auth in brg's configuration, `~/.config/borgchart/config.yaml`.
   `auth: api_key` gives each Claude agent `ANTHROPIC_API_KEY` from the launching
   shell; `auth: login` uses your Claude login and passes no key.

   ```yaml
   schema: 1
   auth: api_key
   ```

5. Inject `SLACK_BOT_TOKEN` from your shell or secret manager's `run` command.
   Never write it into a file. `brg secrets ./examples/slack-devops` checks
   whether it is set.
6. Check and create a team in its own folder outside this checkout:

   ```sh
   brg validate ./examples/slack-devops
   mkdir -p ~/work/slack-demo
   brg create ./examples/slack-devops ~/work/slack-demo
   brg status slack-devops-k3f9
   ```

### First run

Claude may ask for folder trust; brg normally answers it for locked members.
With `auto_trust: false`, attach and answer it yourself before expecting input.
With API key auth, answer any first-use key prompt in a plain Claude session
before starting the team, or use login auth.

`brg attach slack-devops-k3f9/<member>` opens a session; detach with your tmux
prefix then `d`. `brg stop slack-devops-k3f9` stops the team and removes the
private token file; `brg start slack-devops-k3f9` starts it again.

## Smoke test (real Slack)

After the plugin reports healthy, post “deploys are failing on main.” The
triager should promote it and the lead should reply in its thread. Post
“lunch?”; it should stay with the triager. A new reply in the deployment thread
should also reach the triager. `brg status slack-devops-k3f9` shows the `slack`
plugin alive, with both members idle between notices.

To let the lead use vendor settings and more tools, set `posture: vendor` and
`accept_risk: true` locally. Anyone posting in the channel can then try to steer
an agent with shell access and credentials; choose the channel accordingly.

## Tests

Fake-backed plugin tests cover intake, thread follow-ups, delivery failures,
promotion results, safe evidence output and write guards:

```sh
tools/cargo-slot cargo test -p brg-slack --locked
```

The full example round trip uses stand-in agents, a private tmux server and a
fake Slack API:

```sh
tools/cargo-slot cargo build --workspace --bins --all-features --locked
tools/cargo-slot cargo test --features test-support --test slack_example --locked
```
