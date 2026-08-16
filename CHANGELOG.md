# Changelog

## [Unreleased]

### Added

- `quadcd status [--no-fetch] [--user] [--json] [-v]` subcommand: read-only diagnostic that reports per-repo state (up-to-date / ahead / behind / diverged / url mismatch / missing / error) and per-service systemd state (active/sub/result, enabled, `NeedDaemonReload`, restart-pending, `NRestarts`, uptime, and a restart-loop heuristic). Default fetches; `--no-fetch` skips the network. Exits non-zero on any problem so it is usable from cron/monitoring. `--json` emits a single structured document; without it, a plain-text table is printed.

### Changed

- Image pre-pull now follows the activation decision: images are pulled only for changed `.container` and `.image` files whose unit sync is about to start or restart. A unit that stays stopped — inactive with no active unit wanting or requiring it — no longer causes a pull of an image nothing is about to run. Verbose runs log the skipped files.
- **Breaking (MSRV):** Minimum Supported Rust Version bumped from 1.70 to 1.74. This matches the toolchain CI builds and tests against, and lets the codebase use `io::Error::other` and other ≥1.74 conveniences. Downstream consumers on Rust 1.70–1.73 will need to upgrade their toolchain.

## 0.2.0 - 2026-05-23

### Added

- Reserved `${QUADCD_REPO_ROOT}` substitution variable resolving to the absolute path of each source directory, allowing repo-relative paths in `Volume=`, `EnvironmentFile=`, and other unit-file directives. User `.env` files cannot override it.
- `PodmanArgs=` values in `.image` files are now forwarded to `podman pull` (all args are pull-valid per the quadlet spec). In `.container` files, only pull-compatible flags are forwarded: `--authfile`, `--tls-verify`, `--creds`, `--cert-dir`, `--os`, `--arch`, `--variant`, `--platform`, and `--decryption-key`; runtime-only flags are silently ignored.
- Post-restart state reporting: after each `systemctl start`/`restart`, sync queries the unit's `ActiveState`/`SubState` via `systemctl show` and logs one line per unit (e.g. `app.service: active (running)` or `app.service: failed (failed)`). When any unit ends up in a non-`active`/`activating` state, an aggregated `N service(s) failed after restart: …` summary is emitted so operators see broken deployments immediately instead of relying on the no-op success of `systemctl restart`.

### Changed

- Installer no longer auto-enables `quadcd-sync.service`; it prints `systemctl enable` instructions for the user to run instead. Previous behavior used `systemctl --global enable`, which enabled the service for every user including future ones.

### Fixed

- Units whose source files are removed from a synced repo are now stopped before `systemctl daemon-reload`. Previously, deletions were lumped into the changed-files list and fed to `systemctl restart` after the reload — by then systemd had forgotten the units, so the underlying Podman containers were left running as orphans. Renames are treated as a delete of the old path plus an add of the new path.

## 0.1.0

Initial public release.

### Added

- Systemd generator mode via `quadcd generate`, including automatic generator invocation detection based on invocation shape and `SYSTEMD_SCOPE`.
- Git-based continuous deployment via `quadcd sync`, with support for one-shot syncs and long-running `--service` mode.
- Concurrent sync coordination: the `--service` loop holds the data-dir lock only while actively syncing, so manual `quadcd sync` invocations run between ticks; contended ticks are skipped and logged with a consecutive-skip counter.
- Support for Quadlet source files: `.container`, `.volume`, `.network`, `.kube`, `.image`, `.build`, `.pod`, and `.artifact`.
- Support for native systemd unit files: `.service`, `.socket`, `.device`, `.mount`, `.automount`, `.swap`, `.target`, `.path`, `.timer`, `.slice`, and `.scope`.
- User and system mode operation, with mode detection based on CLI flags, environment, and effective privileges.
- TOML-based sync configuration in `quadcd.toml`, with per-repository `url`, optional `branch`, and optional `interval`.
- Interval parsing for sync services, including combined durations such as `1h30m`.
- Repository name validation and safe repository path handling for synced checkouts.
- `.env`-based `${VAR}` substitution for unit files, including per-source-directory overrides merged on top of the data directory defaults.
- Lexicographic source processing and duplicate-unit warnings when later files override earlier ones.
- Drop-in directory support for Quadlet and systemd `*.d/` overrides during generator runs.
- Dry-run support via `-dryrun`, including processed file previews and generator output capture.
- Changed-unit detection after sync so only affected services are restarted when possible.
- Optional image pre-pulling for changed `.container` and `.image` files, with support for `AuthFile=`, `TLSVerify=`, and `Pull=never`.
- Sync safety controls including `--force` for remote URL changes and hard resets, plus `--sync-only` to skip reloads and restarts.
- Non-interactive git and SSH behavior by default, with `--accept-new-host-keys` and `-i` / `--interactive` for first-connect and prompt-driven workflows.
- Runtime environment overrides for configuration paths, unit directories, generator path, git command, sync timeouts, and systemd scope.
- Atomic installation of generated files to reduce partial-write risk during updates.
- Packaged system and user sync service units under `dist/` for long-running deployment workflows.
- Shell installer support via `install.sh`, including architecture selection, release download, checksum verification, binary installation, generator symlinks, and sync service installation.
