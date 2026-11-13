# Slack plugin

`brg-slack` watches a channel for ID notices or listens for allow-listed leading
mentions as requests. Install
from a checkout with `make install` (or `cargo install --path plugins/slack`)
and keep `BINDIR` on PATH. It needs nothing else at runtime; building it needs
a C compiler for TLS. TLS uses bundled roots, so a TLS-intercepting proxy fails
certificate verification. Declare the plugin without `path:`:

```yaml
plugins:
  slack:
    process: [brg-slack, watch]
    delivers_to: triager
    env:
      SLACK_BOT_TOKEN:
      SLACK_CHANNEL_ID: C0000TEST
    actions:
      read: brg-slack read <ts>
      thread: brg-slack thread <ts>
      promote: brg-slack promote <ts> <reason>
      react: brg-slack react <ts> <reaction>
      reply: brg-slack reply <thread_ts> <text>
      page: brg-slack page <user_id> <text>
```

Grant only the actions a role needs. The process runs in the template folder;
actions run in the member's folder. Use `brg-slack --help` for the command list.
The [slack-devops example](../../examples/slack-devops) provides a complete team.

## Mention requests

Use one listener per Slack app, in public channels the bot has joined. This mode
needs `app_mentions:read`, the bot event `app_mention`, `channels:history`,
`chat:write` and `reactions:write`, plus an app-level token with
`connections:write`. Direct messages and private channels are unsupported.

```yaml
plugins:
  slack:
    process: [brg-slack, mentions]
    delivers_to: lead
    env:
      SLACK_APP_TOKEN:
      SLACK_BOT_TOKEN:
      SLACK_MENTION_USERS: U0000ALICE
    actions:
      thread: brg-slack thread <request_id>
      reply: brg-slack reply <request_id> <text>
      react: brg-slack react <request_id> <reaction>
```

Inject both tokens through the launching shell or secret manager. brg supplies
`BORGCHART_DELIVER_TO` and `BORGCHART_STATE_DIR`; direct runs must set them.
`mentions [--once]` accepts only `app_mention` events in the bot's workspace,
from a listed human, with no bot ID or subtype, whose text after whitespace
starts with `<@BOT_USER_ID>` or `<@BOT_USER_ID|`. Other events receive only a
Socket Mode acknowledgement; their text is never logged or stored.

The listener journals the request before acknowledging Slack. It delivers the
request inline to the fixed member from `external:slack`, with its ID, reply and
thread commands, and a reminder to ignore a copy already taken. The request is
the listed person's ask under the member's standing rules. Earlier thread text,
including that person's earlier messages, is framed evidence with no authority.
A vendor recipient with host tools therefore receives other people's text;
framing cannot prevent all attempts to steer it. Configure that role's
`untrusted_input: true` and `accept_risk: true` as required by brg validation.

Request IDs look like `C0000EXAMPLE-1700000000.000100`. `thread`, `reply` and
`react` accept them: reads/replies use the recorded channel and root thread;
reactions use the request message. Missing or expired IDs fail before a Slack
call. Timestamp targets still use `SLACK_CHANNEL_ID`; `read` and `page` remain
channel-only. Grant a member actions from only one Slack entry.

A request stores at most 4,000 characters with a visible truncation marker.
Thread context uses one API attempt with a roughly five-second deadline and no
rate-limit sleep: root plus up to the latest 19 earlier replies returned, at
most 1,000 characters per message and 8,000 bytes for the block. A single page
of up to 1,000 messages is fetched; a partial-page notice warns that later
replies may be missing. The full body stays below 15,000 bytes. Fetch failure
adds a notice and still delivers the request. Other people's context is not
stored in request files; delivered bodies remain in bus history.

Pending requests retry on a wake and every 15 seconds, oldest first. brg exit 0
marks a request sent, then adds `eyes` once. Reaction failure affects health and
is not retried. Delivery failure keeps it pending; after one hour it becomes
failed and health stays not ok. Records expire after 14 days. A restart delivers
pending records. A crash after sending but before marking sent can deliver a
copy; the ID identifies it. Actions do not take the listener's state lock.

Stop any other Socket Mode listener for the app before cutover. More than one
connection makes health not ok, since Slack distributes events between them.
Startup authentication failures retry in normal mode. The listener refreshes
immediately on `disconnect`; other socket failures
reconnect with backoff from one to 60 seconds. It pings after 30 idle seconds
and reconnects after 90 seconds without a frame. Normal mode runs continuously;
`--once` finishes after a socket close/error, draining pending delivery, but
still follows `disconnect` refreshes. Mentions during downtime are lost: no
`eyes` means no confirmed delivery. Replies need another leading mention to
start a new request. The owner should prove scopes, downtime and reconnect
behavior with the app before relying on it.

For watch coexistence, use a separate plugin entry and state directory with
`SLACK_SKIP_MENTIONS: "true"` where mention requests should bypass watch intake.
Unset or any other value preserves ordinary watch behavior. Once cut over, the
app's `message.channels` subscription is unnecessary for mention intake.

## Actions

- `watch [--once]`: snapshot the first window without delivering it, then poll
  new top-level posts and replies to tracked threads. Notices contain timestamps
  and fixed labels, never Slack text. Edits and subtypes are skipped, except
  human `thread_broadcast` replies and explicitly allowlisted bots' `bot_message`
  posts. The plugin's own bot is always skipped. Without an allowlist, every
  human qualifies.
- `read <ts>`: read a top-level post. `thread <ts-or-request-id>`: read up to 200 thread
  messages. Output starts with an untrusted-data warning, frames each message
  with timestamp/author/edit metadata, prefixes every text line with `| `,
  escapes control characters except newline/tab, and redacts both Slack tokens.
- `promote <ts> <reason>`: send a fixed notice to the member's parent, with
  `--from` validating that route. The reasons are `outage`, `deploy`,
  `security`, `question`, `other`. No Slack text is forwarded.
- `react <ts-or-request-id> <reaction>`: use `eyes` or `done` to add eyes or a white check mark.
  An existing reaction succeeds.
- `reply <ts-or-request-id> <text>`: post in the target thread, at most
  1,500 characters.
- `page <user_id> <text>`: post a top-level mention of an allowlisted user in
  `SLACK_CHANNEL_ID`, followed by at most 500 characters. Paging is disabled unless an allowlist is
  configured. After a failed request, check the channel before retrying; Slack
  may already have posted it.

All writes refuse empty text, either Slack token, user mentions in the supplied text,
Slack `<!…>` controls (except `<!date^…>`), and plain `@here`, `@channel` or
`@everyone`. Refusals and usage errors exit 2; failures exit 1, with one plain
stderr line. Quote multiword text as one argument. No page audit log is kept.

## Configuration

| Environment | Meaning |
| --- | --- |
| `SLACK_BOT_TOKEN` | Required bot token; inject it from the launching shell or secret manager. |
| `SLACK_CHANNEL_ID` | Required for watch, read, page and timestamp-based actions; not needed for mentions or request IDs. |
| `SLACK_APP_TOKEN` | Required for mentions; app-level token with `connections:write`. |
| `SLACK_MENTION_USERS` | Required for mentions: non-empty comma-separated human user IDs (`U…` or `W…`), with no empty entries. |
| `SLACK_SKIP_MENTIONS` | Set exactly `true` on watch to skip leading mentions of its bot in posts and replies, while still advancing its cursor. |
| `SLACK_POLL_SECONDS` | Positive poll interval, default 15 seconds. |
| `SLACK_WINDOW_HOURS` | Positive history window, default 24 hours. |
| `SLACK_ALLOW_IDS` | Comma-separated user/bot IDs; empty or unset means every human. |
| `SLACK_PAGE_IDS` | Comma-separated user IDs allowed for paging; empty/unset disables it. |
| `SLACK_API_BASE` | Local fake API test hook; leave unset for Slack. |

brg supplies `BORGCHART_BIN`, `BORGCHART_STATE_DIR`, the resolved
`BORGCHART_DELIVER_TO`, and member identity/parent for actions. Give the bot
`channels:history`, `reactions:write` and `chat:write`; add `groups:history` for
private channels. Install the app and invite the bot to each configured channel.
Slack's app/token restrictions also apply to reading threads.

`watch` requires `BORGCHART_STATE_DIR`. When running it directly, set it to a
dedicated state directory. New directories are private; existing directory permissions are preserved.

## Delivery and limits

Pending intake items are sent oldest first, one batch per poll, at most six
items and 900 UTF-8 bytes. Exit 0 from brg means sent: remove the batch from
pending and save the cursor. Every other result, including a timeout, keeps the
batch for the next poll. Each poll writes `ok`, UTC `at` and `detail` to
`health.json`. Failed intake delivery and failed thread reads make health not ok;
a failed thread is retried without blocking other posts or threads.

Promotions send once and print brg's result, including its message ID, on success.
On failure they exit 1 with brg's sanitized error; the member retries the action.
The action retains its member identity and `--from` validates the parent route.
The brg call is bounded to 30 seconds and kills its process group on timeout.

State files are created with mode 0600. One plugin state lock serializes polling;
cursor and health snapshots use a temporary file and rename. A crash after an
intake send but before saving its removal, or a failed send that already reached
brg, can repeat an ID notice. Pending items remain available for the next poll.
Locked recipients have no inbox recovery; check plugin health after failures.

Edits are not delivered. Threads whose roots left the window are not watched,
and pending items expire with that window. History scans one page of 200; more
than 200 posts in the window leave older posts unscanned. Thread reads return at
most 200 messages; the watcher fetches replies in pages of at most 200, advancing
its timestamp until it catches the advertised latest reply. Slack rate limits
honour Retry-After, capped at 60 seconds, with at most three retries per call.

Run the fake-backed tests with:

```sh
tools/cargo-slot cargo test -p brg-slack --locked
```
