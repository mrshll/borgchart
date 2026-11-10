# Running the teams in this repository

`teams/` holds three team definitions (`borgchart-engineering`, `dev-team` and `dev-design-team`) and a `shared/` folder of includes they all use. Each `team.yaml` and its role files are the only description of that team.

The three aren't self-contained, so they travel together:

- **Child teams resolve by name.** `team: dev-team` is looked up in the `teams:` search paths of brg's configuration.
- **Shared includes sit beside the teams.** `catalog:shared/...` points at `teams/shared/`, which has no `team.yaml` and is not a team.
- **The [seating plugin](../plugins/seat/README.md), `brg-seat`, is found on `PATH`,** as it was when the instance was created or fully started.

## Setup

Point brg's configuration at the folder that contains `teams/`. With this `config.yaml` beside it:

```yaml
schema: 1
auth: login
teams: [./teams]
```

set `BORGCHART_CONFIG` to that file's absolute path, put `brg-seat` on `PATH`, and validate: `brg validate borgchart-engineering`, `brg validate dev-team`, `brg validate dev-design-team`. If another configured directory provides one of these names, validation refuses the ambiguity.

Then create an instance: `brg create borgchart-engineering <folder>`, where `<folder>/lead` is a checkout of the repository, or `brg create dev-team <folder>`, where `<folder>/repo` is.

## Pinned installation

A team that works on this repository shouldn't run from the checkout it edits: a half-finished change to brg or to a role file would take effect at the next start. Run it from a copy fixed at one revision instead. From a clean checkout at that revision:

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

Put the `config.yaml` above in `$prefix`. Use a new prefix for each revision, and keep its `bin` first on `PATH` when creating or fully starting an instance.
