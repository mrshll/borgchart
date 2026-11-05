# slack-devops: an on-call pair for one Slack channel

A cheap **triager** (Claude `haiku`) sees every new message in one channel and escalates the ones that matter to an always-on **lead** (Claude `opus`). The lead reads the message and replies in its thread.

```
Slack channel ──▶ watch.py ──(message ts)──▶ triager ──slackbot escalate──▶ lead ──slackbot reply──▶ thread
```

- **The plugin** (`plugins/slackbot/`) is plain Python scripts run with `uv`. It has no MCP server.
  - **`watch.py`** is the plugin's process. It polls the channel and hands each new top-level human message to the triager. It sends the message's **ID only**, never its text, and skips bots, edits and thread replies.
  - **`slackbot`** is what the agents run, through brg's shims: `read`, `thread`, `reply`, `escalate`.
- **Both agents are `locked`:** each can run only its granted commands, with plain arguments.
  - The triager has `read` and `escalate`.
  - The lead has `read`, `thread` and `reply`.
  - Neither can run anything else, read files, or see the bot token: the token lives with brg's shims, not in the agents' environment.
- **`escalate` sends IDs and fixed values only.** Its `severity` is one of `low`, `medium`, `high`, and its `reason` one of `outage`, `deploy`, `security`, `question`, `other`. Message text never travels from one agent to another.
- **Failed deliveries** (say the lead is waiting at a prompt) are kept in the plugin's `undelivered.jsonl` and retried by the watcher until they go through. `brg status` shows how many are pending.

What the lead can't do: look at systems. It is a smart auto-responder that acknowledges, explains likely causes and asks good questions. Giving it more read-only tools is future work.

## Setup

1. **Create a Slack app** with a bot user. Give it the bot scopes `channels:history` and `chat:write`, install it to your workspace, and invite the bot to **one** channel.
2. **Copy the channel's ID** (it starts with `C`) into `borgchart.yaml` as `SLACK_CHANNEL_ID`.
3. **Install** `brg`, tmux, uv and Claude Code.
4. **Pick auth.** The example uses `auth: api_key` (recommended), so export `ANTHROPIC_API_KEY`. To use your Claude login instead, set `auth: login`.
5. **Export the bot token** in the shell you start from, e.g. `export SLACK_BOT_TOKEN=<your-bot-token>`, or inject it from your secret manager. Never write it into a file.
6. **Check and start:**

   ```sh
   cd examples/slack-devops
   brg validate
   brg up
   brg status
   ```

`brg attach triager` (or `lead`) shows a live session. Detach with your tmux prefix, then `d`. `brg down` stops everything and removes the private files that held the token.

## Smoke test (real Slack)

1. Post "deploys are failing on main" in the channel. Within about two minutes:
   - the triager escalates it (`brg logs triager`);
   - the lead replies in the message's thread.
2. Post "lunch?". It is not escalated.
3. `brg status` shows the `slackbot` plugin `alive` with `ok`, and both agents `idle` between messages.

## Running in `vendor` posture

To let the lead use your own vendor settings (e.g. Claude's auto mode) and tools beyond `slackbot`, set in `borgchart.yaml`:

```yaml
members:
  lead: {posture: vendor}
```

The team marks the lead `untrusted_input`, so `brg validate` will also ask you to add `accept_risk: true` to the lead in `team.yaml`. Only do this for a channel you trust: anyone who can post there can then try to steer an agent that has a shell and your credentials.

## Tests

The plugin's replay tests use a fake Slack API, and cover:

- duplicates;
- the bot's own messages;
- a rate limit;
- prompt-injection text that never reaches another agent;
- out-of-list escalation values;
- a blocked lead, retried until delivered;
- forged queue entries;
- replies that contain the token.

Run them with:

```sh
uv run --no-project --with pytest==8.3.5 pytest examples/slack-devops/plugins/slackbot/tests
```

`cargo test --features test-support --test slack_example` runs the whole round trip through `brg`, with stand-in agents and a fake Slack API.
