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
- **Failed deliveries** (say the lead is waiting at a prompt) are kept in the plugin's `undelivered.jsonl` and retried by the watcher until they go through. `brg status` shows how many are pending in this plugin queue. That is separate from the message bus's per-member unread counts (tree, `-i`, `--json`); this `locked` team has no bus and shows no such counts.

What the lead can't do: look at systems. It is a smart auto-responder that acknowledges, explains likely causes and asks good questions. Giving it more read-only tools is future work.

## Setup

1. **Create a Slack app** with a bot user. Give it the bot scopes `channels:history` and `chat:write`, install it to your workspace, and invite the bot to **one** channel.
2. **Copy the channel's ID** (it starts with `C`) into `team.yaml` as the value of `SLACK_CHANNEL_ID`.
3. **Install** `brg`, tmux, uv and Claude Code.
4. **Pick auth** in brg's configuration, `~/.config/borgchart/config.yaml`. `auth: api_key` (recommended for this example) gives each Claude agent `ANTHROPIC_API_KEY` from the shell you start from, and nothing else of yours; `auth: login` (the default) uses your Claude login and passes no key, even if one is set.

   ```yaml
   schema: 1
   auth: api_key
   ```

5. **Export the bot token** in the shell you start from, e.g. `export SLACK_BOT_TOKEN=<your-bot-token>`, or start brg under your secret manager's `run`. Never write it into a file: brg stores no secrets. `brg secret ./examples/slack-devops` says whether it is set.
6. **Check and start** an instance, in a folder of its own outside this checkout (the agents are `locked`, so their folder can't hold the team's files):

   ```sh
   brg validate ./examples/slack-devops
   mkdir -p ~/work/oncall
   brg create ./examples/slack-devops ~/work/oncall   # prints the instance's ID, e.g. slack-devops-k3f9
   brg status slack-devops-k3f9
   ```

### First run

- **Folder trust:** Claude asks once per folder whether you trust it. For these `locked` agents, brg answers it for the instance's folder by itself (`auto_trust: false` in brg's configuration turns that off; then run `brg attach slack-devops-k3f9/<member>` once and answer it yourself). Until it's answered, the agent gets no input, and messages wait in the queue.
- **API key:** with `auth: api_key`, Claude may ask once whether to use the key. The start then reports the agent stopped, because its gate didn't report in. Answer that prompt in a plain `claude` session first, or use `auth: login`.

`brg attach slack-devops-k3f9/<member>` shows a live session (`brg status` lists the members' IDs). Detach with your tmux prefix, then `d`. `brg stop slack-devops-k3f9` stops everything and removes the private file that held the token; `brg start slack-devops-k3f9` brings it back.

## Smoke test (real Slack)

1. Post "deploys are failing on main" in the channel. Within about two minutes:
   - the triager escalates it (`brg logs slack-devops-k3f9/<triager member>`);
   - the lead replies in the message's thread.
2. Post "lunch?". It is not escalated.
3. `brg status slack-devops-k3f9` shows the `slackbot` plugin `alive` with `ok`, and both agents `idle` between messages.

## Running in `vendor` posture

To let the lead use your own vendor settings (e.g. Claude's auto mode) and tools beyond `slackbot`, set `posture: vendor` on the lead in your copy of `team.yaml`. The team marks the lead `untrusted_input`, so `brg validate` will also ask you to add `accept_risk: true` to it. Only do this for a channel you trust: anyone who can post there can then try to steer an agent that has a shell and your credentials.

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
