# brg-seat

Optional Rust quota plugin; see [seating](../../docs/seating.md) and the [catalog setup](../../docs/catalog.md).

`brg-seat route <claude|codex> <model> [--refresh]` returns go/0, wait/3 or usage/2. Declare and grant it through the existing team plugin mechanism. API-key teams omit it; the plugin never reads core auth configuration or authorizes API spending. It shares the existing borgchart library's bounded processes, UTC, paths and environment helpers. There is no Python or compatibility wrapper.

Tests use fake vendor protocols with owned homes and private caches. They do not qualify live vendor access or silently stale Claude replies; profile reads remain deliberately absent.
