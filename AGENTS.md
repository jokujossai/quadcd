# Agent Guidelines

QuadCD is a Rust systemd generator and git-sync deployment tool for Quadlet and systemd units.

## Key Files

- `src/main.rs` - entry point
- `src/lib.rs` - library root, signal handlers (`SHUTDOWN` flag)
- `src/cli.rs` - CLI argument parsing
- `src/app.rs` - subcommand dispatch and app orchestration
- `src/config.rs` - env/flag-derived runtime config
- `src/cd_config.rs` - `quadcd.toml` loading
- `src/install/mod.rs` - installation, `[Install]` symlinks, drop-ins
- `src/install/discover.rs` - unit file discovery, generated unit names, duplicates
- `src/install/content.rs` - env substitution, `SourcePath=`
- `src/lock.rs` - sync lock
- `src/dryrun.rs` - dry-run flow
- `src/generator.rs` - `Generator` trait and `systemd-generator` invocation
- `src/output.rs` - stdout/stderr abstraction
- `src/sync/runner/mod.rs` - one-shot sync orchestration
- `src/sync/runner/service.rs` - service loop, intervals, config reload
- `src/sync/repo.rs` - per-repo git sync (`sync_repo_inner`)
- `src/sync/vcs.rs` - `Vcs` trait and `GitVcs` implementation
- `src/sync/image.rs` - `ImagePuller` trait for container image pre-pull
- `src/sync/units/files.rs` - unit file names and detection
- `src/sync/units/plan.rs` - activation planning
- `src/sync/units/execute.rs` - activation and stopping deleted units
- `src/sync/settings.rs` - `[X-QuadCD]` per-unit settings (`StartOnSync=`, `Watch=`)
- `src/sync/watch.rs` - `Watch=` glob matching against non-unit diff paths
- `src/sync/systemd/mod.rs` - systemd operations
- `src/sync/systemd/testing.rs` - `MockSystemd`
- `tests/` - unit and integration tests
- `tests/containerized/` - containerized integration tests
- `.github/pull_request_template.md` - PR body structure (Summary, Related issue, AI usage, Checklist)

## Rules

- Preserve trait-based dependency injection for testability.
- Keep `AGENTS.md`, `README.md`, and `CONTRIBUTING.md` aligned with the codebase.
- Add or update tests when changing behavior.
- Prefer focused changes and avoid unrelated refactors.
- Split a file into logical submodules once it grows large (roughly 1000+
  lines or several unrelated concerns); move its tests with the code.
- Keep writing short: say what and the one non-obvious why, no history or edge-case essays.
  - Commit message: subject line plus at most 3 body lines.
  - CHANGELOG entry: 1–2 sentences.
  - Code comment (`//`): only what the code does not already say.
  - Doc comment (`///`): every function, type and field that had one keeps
    one. Short, but keep the contract: what it returns, error behaviour,
    preconditions (e.g. "before `daemon-reload`") and known gaps.

## Before Committing

Run:

1. `cargo fmt --check`
2. `cargo clippy -- -D warnings`
3. `cargo test`

## Opening a Pull Request

- Read `.github/pull_request_template.md` and fill in every section
  (`## Summary`, `## Related issue`, `## AI usage`, `## Checklist`) in the PR
  body — `gh pr create --body` does not auto-populate the template.
- Tick the correct **AI usage** box honestly. If the change is fully AI-generated
  with minimal human editing, use "Entirely AI-generated (minimal human editing)".
- Tick each **Checklist** item only after the corresponding command has passed
  locally on the branch head.
