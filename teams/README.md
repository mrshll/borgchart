# Team definitions

`borgchart-engineering/`, `dev-team/` and `dev-design-team/` are sibling definitions. Their `team:` roles refer to configured team names. Runtime instances still form a tree with nested working folders and independent stores.

`shared/` holds the common instructions once; it has no `team.yaml` and is not a team. Team definitions use `catalog:shared/...` for these assets. Other team files remain confined to their own definition folder.

## Pinned installation

The pinned installation includes core and plugin binaries in `bin/`. From a clean checkout at the chosen revision, build the workspace and copy the catalog:

```sh
revision=$(git rev-parse HEAD)
prefix="$HOME/.local/lib/borgchart/brg-$revision"
tools/cargo-slot cargo build --release --workspace --bins --locked
mkdir -p "$prefix/bin"
cp target/release/brg target/release/borgchart target/release/brg-seat "$prefix/bin/"
cp -R teams "$prefix/teams"
export PATH="$prefix/bin:$PATH"
export BORGCHART_CONFIG="$prefix/config.yaml"
```

Choose a new, unused prefix for each revision. Keep that `bin` directory first on PATH whenever creating or fully starting an instance: no-path plugins resolve through the launch PATH frozen by brg. Merely invoking an absolute brg does not pair it with the sibling plugin. This recipe does not start or migrate any instance.

Copy this entire `teams/` directory into `brg-<sha>/teams/`, alongside the stable brg built from that revision. Do not copy only the department folder. Put `config.yaml` in `brg-<sha>/` with:

```yaml
schema: 1
auth: login
teams: [./teams]
```

Set `BORGCHART_CONFIG` to that file's absolute path, then validate all three names before creating instances: `brg validate borgchart-engineering`, `brg validate dev-team`, `brg validate dev-design-team`. No source checkout or global `plugins_dir` is needed to resolve the shared definitions/assets. The declared [seating plugin](../docs/seating.md) uses the pinned `brg-seat` binary, installed vendor CLIs and subscription login. API-key teams omit its declaration and grant.

The configured search paths are authoritative. If another configured directory provides the same name, validation refuses the ambiguity; it does not silently prefer this snapshot.
