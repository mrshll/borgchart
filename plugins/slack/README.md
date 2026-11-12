# Slack plugin

`brg-slack` watches one Slack channel and gives members IDs to inspect. Install
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
      react: brg-slack react <ts> <eyes|done>
      reply: brg-slack reply <thread_ts> <text>
      status: brg-slack status <text>
      page: brg-slack page <user_id> <text>
```

Grant only the actions a role needs. The process runs in the template folder;
actions run in the member's folder. Use `brg-slack --help` for the command list.
The [slack-devops example](../../examples/slack-devops) provides a complete team.

## Actions

- `watch [--once]`: snapshot the first window without delivering it, then poll
  new top-level posts and replies to tracked threads. Notices contain timestamps
  and fixed labels, never Slack text. Edits and subtypes are skipped, except
  human `thread_broadcast` replies and explicitly allowlisted bots' `bot_message`
  posts. The plugin's own bot is always skipped. Without an allowlist, every
  human qualifies.
- `read <ts>`: read a top-level post. `thread <ts>`: read up to 200 thread
  messages. Output starts with an untrusted-data warning, frames each message
  with timestamp/author/edit metadata, prefixes every text line with `| `,
  escapes control characters except newline/tab, and redacts the bot token.
- `promote <ts> <reason>`: send a fixed notice to the member's parent, with
  `--from` validating that route. The default reasons are `outage`, `deploy`,
  `security`, `question`, `other`. No Slack text is forwarded.
- `react <ts> <eyes|done>`: add eyes or a white check mark. An existing reaction
  succeeds.
- `reply <thread_ts> <text>`: post in the configured channel's thread, at most
  1,500 characters.
- `status <text>`: post one status message, then update it in place. Changing the
  status channel or deleting the message makes the next call post a new one.
  The channel and timestamp are stored in locked, atomically replaced
  `status.json`. Text is limited to 3,000 characters.
- `page <user_id> <text>`: mention an allowlisted user in the status channel,
  followed by at most 500 characters. Paging is disabled unless an allowlist is
  configured. After a failed request, check the channel before retrying; Slack
  may already have posted it.

All writes refuse empty text, the bot token, user mentions in the supplied text,
Slack `<!…>` controls (except `<!date^…>`), and plain `@here`, `@channel` or
`@everyone`. Refusals and usage errors exit 2; failures exit 1, with one plain
stderr line. Quote multiword text as one argument. No page audit log is kept.

## Configuration

| Environment | Meaning |
| --- | --- |
| `SLACK_BOT_TOKEN` | Required bot token; inject it from the launching shell or secret manager. |
| `SLACK_CHANNEL_ID` | Required intake/read/reply/reaction channel. |
| `SLACK_POLL_SECONDS` | Positive poll interval, default 15 seconds. |
| `SLACK_WINDOW_HOURS` | Positive history window, default 24 hours. |
| `SLACK_ALLOW_IDS` | Comma-separated user/bot IDs; empty or unset means every human. |
| `SLACK_PROMOTE_REASONS` | Distinct comma-separated lowercase reason names (letters, digits, `_`, `-`, starting with a letter, at most 40 characters). |
| `SLACK_STATUS_CHANNEL` | Required for status and pages. |
| `SLACK_PAGE_IDS` | Comma-separated user IDs allowed for paging; empty/unset disables it. |
| `SLACK_API_BASE` | Local fake API test hook; leave unset for Slack. |

brg supplies `BORGCHART_BIN`, `BORGCHART_STATE_DIR`, the resolved
`BORGCHART_DELIVER_TO`, and member identity/parent for actions. Give the bot
`channels:history`, `reactions:write` and `chat:write`; add `groups:history` for
private channels. Install the app and invite the bot to each configured channel.
Slack's app/token restrictions also apply to reading threads.

## Delivery and limits

Pending intake items are sent oldest first, one batch per poll, at most six
items and 900 UTF-8 bytes. Exit 0 from brg means committed, even when typing was
stored or uncertain, and is never retried. A definite failure keeps the batch
for the next poll. A timeout (124), or a nonzero result with a committed ID, is
uncertain: drop it, log to stderr, and increment `uncertain_sends` in
`health.json`. Each poll writes `ok`, UTC `at`, `detail`, and that counter.
Failed intake delivery, pending promotions and failed thread reads make health
not ok; a failed thread is retried without blocking other posts or threads.

Failed promotions append only target, sender, timestamp and reason to locked
`undelivered.jsonl`. The watcher retries each entry on every poll, even when
Slack polling fails. It rebuilds notices from validated IDs/enums and drops
invalid entries and parent-route refusals. It sends as `external:slack`; a direct
member action retains its member identity. `--from` validates the route without
changing the author. Promotion timeouts are dropped and counted, too.

State files use no-follow opens and locks; JSON snapshots are atomically
replaced. Intake removes a batch before sending: a crash around sending can
**lose that one notice**, rather than replay a committed send. The promotion
spool is appended/rewritten under its file lock; a crash after its send but
before removing an entry **can resend that promotion**. These operations cannot
be made transactional with brg's message store. Locked recipients have no inbox
recovery; check plugin health when delivery is uncertain.

Members that can write `status.json` can redirect status updates to another of
the bot's messages in the status channel.

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
