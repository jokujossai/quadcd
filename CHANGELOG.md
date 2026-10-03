# Changelog

## [Unreleased]

### Added

- `[X-QuadCD]` unit-file section for per-unit sync settings (ignored by systemd):
  - `StartOnSync=true` starts the unit when it changes, even if nothing wants it. These units are started first and waited for, so a build finishes before the container using its image. Ignored on templates.
  - `Watch=<glob>` marks the unit changed when a matching repo file changes (relative to the unit's directory; `*`, `?`, `**`; repeatable).

### Changed

- **Breaking (API):** `UnitChanges` has a new public `other` field (non-unit paths from the diff).
- Shortened the changelog, README and code comments.
- **Breaking (behaviour):** Image pre-pull follows the unit's pull policy (`Pull=` in `.container`, default `missing`; `Policy=` in `.image`, default `always`) via `podman pull --policy`. A `.container` image already present locally is no longer re-pulled unless its policy says so.
- **Breaking (API):** `ImageRef` has a new public `pull_policy` field.

## 0.3.0 - 2026-08-19

### Added

- The generator honours `[Install]` in plain systemd units, creating `.wants`/`.requires` symlinks like Quadlet does. Previously such units could never start at boot.
- `quadcd status [--no-fetch] [--user] [--json] [-v]`: read-only report of repo state and service state (including pending restarts and restart loops). Exits non-zero on any problem.

### Changed

- **Breaking (behaviour):** Sync decides what to start from reverse dependencies instead of `is-enabled`, which reports `generated` for every quadcd unit. A changed unit is restarted if active, and started if inactive only when a unit that wants it is coming up. Units stopped by hand stay stopped.
- `BoundBy=` and `UpheldBy=` (systemd ≥ 249) also count as wanting a unit. `PartOf=`, `Requisite=`, `Conflicts=` and socket/timer/path activation do not.
- Images are pre-pulled only for units that will be running after the sync.
- **Breaking (API):** `SystemdTrait` gained the required `pending_start_jobs` and the defaulted `activation_state`.
- **Breaking (MSRV):** Rust 1.74.

### Fixed

- Sync no longer restarts stopped or failed instances of a changed template; each instance follows the normal rules.
- A unit wanted by a target that is still starting (queued start job) is now started. Previously the first sync on a fresh host started nothing. A dependant in `activating (auto-restart)` does not count.
- Deleted units that are `activating` or reloading are now stopped too, so their containers are not orphaned.
- A changed unit that is itself crash-looping is restarted instead of being left waiting for its next backoff attempt.

## 0.2.0 - 2026-05-23

### Added

- Reserved `${QUADCD_REPO_ROOT}` variable: absolute path of the source directory. Cannot be overridden from `.env`.
- `PodmanArgs=` is forwarded to `podman pull`: all args from `.image` files, only pull-compatible ones from `.container` files.
- After start/restart, sync logs each unit's state and a summary of failed units.

### Changed

- The installer no longer enables `quadcd-sync.service` globally; it prints the `systemctl enable` command instead.

### Fixed

- Units whose files were removed are stopped before `daemon-reload` instead of being left running as orphaned containers.

## 0.1.0

Initial public release.

### Added

- Systemd generator mode (`quadcd generate`) with automatic invocation detection.
- Git-based deployment (`quadcd sync`), one-shot or `--service`; manual syncs can run between service ticks.
- Quadlet files (`.container`, `.volume`, `.network`, `.kube`, `.image`, `.build`, `.pod`, `.artifact`) and native systemd units.
- User and system mode.
- `quadcd.toml` with per-repository `url`, `branch` and `interval` (e.g. `1h30m`).
- `.env`-based `${VAR}` substitution with per-directory overrides.
- Duplicate-unit warnings, drop-in directories, and `-dryrun`.
- Restarting only changed units, with optional image pre-pull (`AuthFile=`, `TLSVerify=`, `Pull=never`).
- `--force`, `--sync-only`, `--accept-new-host-keys` and `-i`; non-interactive git/SSH by default.
- Environment overrides for paths, commands, timeouts and systemd scope.
- Atomic file installation, packaged sync service units, and `install.sh`.
