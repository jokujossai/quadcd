# Agent Guidelines

QuadCD is a Rust systemd generator and git-sync deployment tool for Quadlet and systemd units.

## Key Files

- `src/main.rs` - entry point
- `src/lib.rs` - library root, signal handlers (`SHUTDOWN` flag)
- `src/cli.rs` - CLI argument parsing
- `src/app.rs` - subcommand dispatch and app orchestration
- `src/config.rs` - env/flag-derived runtime config
- `src/cd_config.rs` - `quadcd.toml` loading
- `src/install.rs` - file discovery, env substitution, installation
- `src/dryrun.rs` - dry-run flow
- `src/generator.rs` - `Generator` trait and `systemd-generator` invocation
- `src/output.rs` - stdout/stderr abstraction
- `src/sync/runner.rs` - sync orchestration
- `src/sync/repo.rs` - per-repo git sync (`sync_repo_inner`)
- `src/sync/vcs.rs` - `Vcs` trait and `GitVcs` implementation
- `src/sync/image.rs` - `ImagePuller` trait for container image pre-pull
- `src/sync/units.rs` - changed-unit detection and activation
- `src/sync/systemd.rs` - systemd operations
- `tests/` - unit and integration tests
- `tests/containerized/` - containerized integration tests
- `.github/pull_request_template.md` - PR body structure (Summary, Related issue, AI usage, Checklist)

## Rules

- Preserve trait-based dependency injection for testability.
- Keep `AGENTS.md`, `README.md`, and `CONTRIBUTING.md` aligned with the codebase.
- Add or update tests when changing behavior.
- Prefer focused changes and avoid unrelated refactors.

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
