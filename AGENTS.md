# Agent Guidelines

QuadCD is a Rust systemd generator and git-sync deployment tool for Quadlet and systemd units.

## Key Files

- `src/main.rs` - entry point
- `src/app.rs` - CLI dispatch and app orchestration
- `src/config.rs` - env/flag-derived runtime config
- `src/cd_config.rs` - `quadcd.toml` loading
- `src/install.rs` - file discovery, env substitution, installation
- `src/dryrun.rs` - dry-run flow
- `src/output.rs` - stdout/stderr abstraction
- `src/sync/runner.rs` - sync orchestration
- `src/sync/systemd.rs` - systemd operations
- `tests/` - unit and integration tests
- `tests/containerized/` - containerized integration tests

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
