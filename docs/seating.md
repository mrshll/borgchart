# Seating

`brg-seat` is an optional plugin that answers one question before a lead launches a member: does this vendor's subscription have room for this model?

```sh
brg-seat route claude opus
brg-seat route codex gpt-6-astra
```

| Answer | Exit | Meaning |
| --- | --- | --- |
| `go: …` | 0 | launch |
| `wait: …` | 3 | don't launch; check again later |
| usage error | 2 | wrong arguments |

Anything else, including an error or a missing executable (127), means wait. A wait never authorizes another vendor or model. The command only checks quota: it launches nothing and changes no settings, and brg's own `create` and `start` don't run it for you.

## What it checks

It asks the installed vendor CLI for subscription usage, using your existing login: Claude's `get_usage` control request, or Codex's `app-server` usage endpoint. It never reads credential contents.

- **100% of a relevant window:** wait.
- **90%:** go, with a warning that stays until usage falls below 80%.
- **Anything it can't establish** (denied, unknown model, missing or malformed data, no CLI, no login, no network, unwritable cache): wait.

A relevant window is the shared one or the model's own. Retry advice names a reset time only when every relevant hard limit has one, and it is not a promise of access.

## Declaring it

A team declares the plugin and grants its action to the roles that launch members:

```yaml
plugins:
  seat:
    actions:
      route: brg-seat route <vendor> <model>
roles:
  lead:
    prompt: lead.md
    plugins: {seat: [route]}
```

The executable is found on the `PATH` the instance was created or fully started with, so install `brg-seat` beside the `brg` of the same revision and keep that directory first on `PATH` ([setup](catalog.md)). A running instance keeps the `PATH` it captured; a full stop and start, from the root, picks up a new one.

Teams that use API keys leave the plugin out: a subscription reading can't authorize API spending.

## Cache

Readings are cached in `$XDG_CACHE_HOME/borgchart/seat-v2` (or `~/.cache/borgchart/seat-v2`), private to your user.

- A reading lasts at most 120 seconds, less if a reset comes sooner. `--refresh` skips a cached reading.
- A failed check is not retried for 30 seconds, even with `--refresh`.
- Only normalized quota figures are stored: no responses, tokens or profiles.
- The cache is disposable. Clear it when no check is running.

## Limits

- **Account switches.** A reading is tied to the CLI's identity and the metadata of its credential and settings files. If those change during a check (a token refresh does this), the answer is wait, and the next check starts fresh. A switch that changes only a keychain entry isn't visible, so the previous account's reading can be reused for up to 120 seconds.
- **Stale Claude replies.** The plugin trusts the usage reply as returned. A reply that is silently stale can allow a launch near the real limit.
- **Members may route elsewhere.** The check runs with hooks, MCP configuration and setting sources disabled; a launched member honors user settings that the check didn't see.
- **Tests use fake vendors.** Before relying on it, run both checks from the lead's real environment with the vendor CLI versions you'll use.
