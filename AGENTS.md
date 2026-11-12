# Working in this repository

These are this repository's own rules. They apply to every agent working here, whichever team it runs in, alongside its team's instructions. `CLAUDE.md` imports this file, so Claude and Codex read the same text.

- **Public repository.** Code, tests, fixtures, docs, commit messages, issues and pull requests use fictional names and data: no machine paths, people's names, account details, usage figures or private notes.
- **Builds.** Run direct cargo builds and test suites as `tools/cargo-slot <command>` from the checkout root. `.githooks/checks` and the git hooks already use it, so don't wrap them. If a build is killed for memory or the cap is unavailable, stop and report it; don't retry uncapped. An implementer runs one cargo command at a time.
- **Checks.** The quick checks are `.githooks/checks fmt clippy fast` plus the tests for your change. The full checks are `.githooks/checks`, which CI runs on every pull request. [CONTRIBUTING.md](CONTRIBUTING.md) lists the tiers and subsets.
- **Current behaviour only.** This project has one user and no compatibility promise. Add no tests or code for retired names, old formats or migration refusals; test current behaviour only.
- **Code style.** Match the existing code: small modules, plain error strings that say what to do next, `//!` module docs.
