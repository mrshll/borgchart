# Seating

Before starting each member, check its explicit vendor and model (without the effort):

```sh
brg seat route claude opus
brg seat route codex gpt-6-astra
```

`go: ...` exits 0. `wait: ...` exits 3. Invalid arguments exit 2; help exits 0. A wait or failed check never authorizes another vendor or model. This command checks quota; it does not launch members or change their settings. Leads use `"$BORGCHART_BIN" seat route <vendor> <model>` so the check uses the binary that started them.

brg asks the installed Claude CLI's experimental `get_usage` control request or Codex's `app-server` subscription usage endpoint. It uses the existing vendor login, without opening credential stores. There is no Python adapter, external quota-reader setting, or seating plugin to install.

Shared and applicable model-specific windows warn at 90% and refuse at 100%. An existing warning stays until usage falls below 80%. Backend denial, unknown model scope, missing utilization, malformed data, unavailable CLI, or stale local cache means wait. Claude fable requires an explicit fable scope. Codex class names and versioned model names map to the same class, but unrelated classes do not authorize one another. Retry advice names the latest known hard reset only when every relevant hard limit has a valid future reset; it is not a promise of restored access.

## Cache and configuration

Only normalized quota fields enter the private cache. The default is `$XDG_CACHE_HOME/borgchart/seat-v2`, or `~/.cache/borgchart/seat-v2`. The cache directory must be owned by the current user with mode 0700; files must be owned regular files with mode 0600 and one hard link. A symlink at the cache leaf or a cache file is refused. Existing parent aliases such as macOS's temporary-directory alias are resolved before descriptor-relative operations.

Successful readings last at most 120 seconds, shortened by the earliest future reset. Cached probe failures back off for 30 seconds; `--refresh` bypasses a successful reading but never that backoff. A shared lock serializes readers, with a 22-second lock deadline. Each vendor exchange has a 20-second deadline and separate 4 MiB stdout/stderr limits. Child process groups are killed on completion or failure. Output errors never relay vendor response text.

An optional location belongs in brg's configuration, not a plugin:

```yaml
schema: 1
seat:
  cache_dir: ~/.cache/borgchart/seat-v2
```

Relative paths are relative to the configuration file; `..` components are rejected during configuration validation. Upgrade **every brg binary sharing that file** before adding `seat`: older binaries reject unknown keys. No cache migration or old `seat-reader` setting is supported. Unwritable cache means wait. If a write fails after the probe, no backoff can be stored, so a later invocation may probe again.

File-backed credential and configuration **metadata**, plus the CLI's identity, fence each reading. An identity change during a probe returns wait; the next invocation may probe under the new identity. This includes a routine token refresh or settings rewrite. Credential contents are never read. Raw responses, errors, profiles and tokens are never persisted in the cache.

This fence cannot observe credentials held only in a keychain, keyring or another non-file store. If an account switch does not change the tracked files, a previous account's reading can be reused until the cache TTL expires (at most 120 seconds). Include that account-switch behavior in live qualification.

An identity change resets advisory hysteresis; warnings are not carried between possibly different accounts. Old identity entries are not pruned automatically. The cache is disposable and may be cleared after all seating checks have finished.

## Authentication and qualification limits

`auth: api_key` returns wait before launching a quota probe: subscription headroom cannot authorize API spending. Probes receive the launch environment allowlist without member identity or arbitrary API/backend overrides. Claude probes disable hooks, MCP configuration and setting sources; launched members may honor user settings that route them elsewhere. A subscription result does not authorize a paid API or proxy route.

Claude profile reads are deliberately skipped: no account metadata or persisted-utilization freshness guard is read from either profile. brg trusts the usage reply as returned. A silently stale Claude reply can permit seating near the real limit; a vendor-supported freshness guard is possible future work. Missing, malformed and incompatible replies still mean wait.

Fake tests do not qualify real vendor versions or sandbox access. Before a deployment cutover, check both vendors from the actual lead environment, record CLI versions and cold-start latency, and verify stat-only identity is stable before and after a normal probe. Repeated identity rewrites would cause repeated waits. Confirm the qualified Claude version accepts the empty `--setting-sources` argument. No command here grants filesystem or network access; unavailable login/cache/network remains wait. Removing a deployed team's plugin changes its frozen files and requires an owner-coordinated full restart.
