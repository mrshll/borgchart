# A seating plugin

`seat` answers one question for a lead about to launch a member: may a member of this vendor and model start now? Leads ask it before each launch a `create` or `start` causes, and only go ahead on `go`. The policy lives here and in the leads' instructions; brg has no quota logic.

```
seat route <vendor> <model>
```

It prints one line and exits:

| Exit | Line | Meaning |
| --- | --- | --- |
| 0 | `go: <reason>` | launch it |
| 3 | `wait: <reason>` | don't launch it; check again later |
| 2 | `wait: <reason>` | bad arguments |

Any other exit status is an error, which also means wait. A lead never answers a `wait` by launching another vendor or model instead.

## The usage source

`seat` doesn't know any vendor's usage itself. It runs `usage-source` from its own folder, as `./usage-source <vendor> <model>`, which prints one JSON object:

```json
{"used_percent": 42.5, "resets_at": "2026-01-01T06:00:00Z"}
```

- **`used_percent`:** how much of the vendor's allowance for that model is used, from 0 to 100, or `null` when the source can't tell.
- **`resets_at`:** optional; when usage resets, in UTC. A `wait` shows it.

The answer is `go` when `used_percent` is below the threshold: 80 by default, or the number in a file named `threshold` beside `seat` (above 0, at most 100). Everything else is `wait`, with the reason:

- no `usage-source`, or one that can't run;
- a source that exits non-zero or takes over 30 seconds;
- `null` usage;
- output that isn't such an object, or a `used_percent` that isn't a number from 0 to 100;
- an unreadable `threshold` file, including a link whose target is gone.

`usage-source.example` is a stand-in that answers every vendor and model with the same fictional reading. The tests (`tests/`) use their own fake sources.

## Installing it on a machine

Teams name the plugin as `shared:seat`, so each machine supplies its own copy, with its own usage source, in brg's `plugins_dir`:

1. Set `plugins_dir` in brg's configuration (`~/.config/borgchart/config.yaml`), for example `plugins_dir: ~/borgchart-plugins`.
2. Copy this folder there as `seat/`: `cp -R examples/seat ~/borgchart-plugins/seat`.
3. Install the machine's real usage source as `~/borgchart-plugins/seat/usage-source`, an executable that follows the contract above. How it reads usage (a vendor's own status command, an account page, a local log) is the machine's business and stays out of this repository. Until one is installed, every answer is `wait`.
4. Optionally, write a `threshold` file there.
5. Check it: `~/borgchart-plugins/seat/seat route claude opus`.

A running instance freezes where `shared:seat` resolves, not the folder's contents: a new source or threshold takes effect at the next `seat route`.

Run the tests from the repository's root:

```
uv run --no-project --with pytest==8.3.5 pytest examples/seat/tests
```
