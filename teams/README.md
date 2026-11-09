# Team definitions

`borgchart-engineering/`, `dev-team/` and `dev-design-team/` are sibling definitions. Their `team:` roles refer to configured team names. Runtime instances still form a tree with nested working folders and independent stores.

`shared/` holds the common instructions once; it has no `team.yaml` and is not a team. Team definitions use `catalog:shared/...` for these assets. Other team files remain confined to their own definition folder.

## Pinned installation

Copy this entire `teams/` directory into `brg-<sha>/teams/`, alongside the stable brg built from that revision. Do not copy only the department folder. Put `config.yaml` in `brg-<sha>/` with:

```yaml
schema: 1
auth: login
teams: [./teams]
```

Set `BORGCHART_CONFIG` to that file's absolute path, then validate all three names before creating instances: `brg validate borgchart-engineering`, `brg validate dev-team`, `brg validate dev-design-team`. No source checkout or global `plugins_dir` is needed to resolve the shared definitions/assets. Built-in [seating](../docs/seating.md) uses the installed vendor CLIs and subscription login; no seating plugin or external reader is needed.

The configured search paths are authoritative. If another configured directory provides the same name, validation refuses the ambiguity; it does not silently prefer this snapshot.
