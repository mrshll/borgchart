# Subscription quota reader

The [seating plugin](../borgchart-engineering/teams/dev-team/plugins/seat/README.md)
ships its reader in the same folder. It uses the locally authenticated Claude
and Codex CLIs to request usage metadata without starting a model turn. `uv` and
Python 3.11 or later are required; the reader has no third-party Python packages.
No external checkout or new configuration is needed.

`usage-route <claude|codex> <model>` emits the JSON decision consumed by the
adapter. `seat route <vendor> <model>` returns `go: <reason>` or `wait: <reason>`.
Check the latter from the lead's plugin environment before launching helpers.

## Quota policy

Quota at 90% is advisory; 100%, a backend denial or unknown usage prevents
seating. A 99% reading can still authorize the selected model. Warnings include
the quota scope, observation and reset time. A warning remains held until its
window falls below 80%. A known future reset can be returned as a retry time;
that is not a scheduled retry or a guarantee of recovery.

The reader considers shared and applicable model-specific windows. Codex model
classes are `astra`, `sol`, `terra` and `luna`, including explicit
`gpt-<release>-<class>` names. Claude scopes are `opus`, `sonnet` and explicitly
reported `fable`. Unknown mappings and incomplete applicable windows mean wait.
The reader does not switch models or vendors, spend credits, redeem resets,
manage login or validate model/effort availability in a runtime catalog. brg
remains responsible for model and effort validation.

## Protocol compatibility

Claude's experimental `get_usage` and Codex's `account/rateLimits/read` are
compatibility boundaries. Missing, incompatible, failed or incomplete responses
remain unknown. Claude runs with hooks and MCP disabled for this probe; neither
vendor is asked to start a thread or model turn. Raw CLI errors and responses are
not printed.

Codex protocol reference: [App Server](https://learn.chatgpt.com/docs/app-server).
The client initializes a stdio connection, sends `initialized`, then requests
`account/rateLimits/read`. Claude uses stream-JSON initialize and `get_usage`
control requests. Probe output and duration are bounded, and the reader ends the
probe process group when the read finishes or times out.

A failed probe means to check the vendor CLI's login and compatibility, then retry
after backoff. API-key or alternate-backend environments do not establish the
subscription quota required by this reader. It never changes credentials or
attempts a login itself.

## Cache and configuration

Normalized quota metadata is cached under `$XDG_CACHE_HOME/borgchart/usage`, or
`~/.cache/borgchart/usage`. Successful observations live for at most 120 seconds
(and no later than a relevant upcoming reset). Failures back off for 30 seconds,
including explicit refreshes. A per-identity file lock serializes refreshes;
private files are replaced atomically. No tokens or raw vendor responses are
stored in this cache.

The cache identity includes the vendor configuration directory, executable and
configuration/credential-file metadata. Credential contents are never read.
Token or config rewrites can invalidate the cache and require another probe; a
change during a probe returns unknown rather than reusing capacity across a
possible account change. Stable-account identification over RPC is a possible
future optimization. Claude's profile usage timestamp is checked because its
experimental reader can fall back to persisted data; stale fallback data is not
a fresh observation. The profile is never logged or copied into the cache.

An optional `seat-reader` file next to brg's configuration selects a replacement
executable implementing the same JSON contract. Its location follows
`$BORGCHART_CONFIG` when set, otherwise `~/.config/borgchart/config.yaml`. The file
contains one executable name or absolute path, without flags or shell syntax;
an invalid override does not fall back. This is the only reader configuration
needed for an override. Thresholds and model classes are built-in policy.

If an earlier override used a different `$XDG_CONFIG_HOME`, move `seat-reader`
beside the actual brg configuration before using this version.
