# Changelog

## [Unreleased]

### Added

- `quadcd status [--no-fetch] [--user] [--json] [-v]` subcommand: read-only diagnostic that reports per-repo state (up-to-date / ahead / behind / diverged / url mismatch / missing / error) and per-service systemd state (active/sub/result, enabled, `NeedDaemonReload`, restart-pending, `NRestarts`, uptime, and a restart-loop heuristic). Default fetches; `--no-fetch` skips the network. Exits non-zero on any problem so it is usable from cron/monitoring. `--json` emits a single structured document; without it, a plain-text table is printed.

### Changed

- Sync now recognises `BoundBy=` (reverse of `BindsTo=`) and `UpheldBy=` (reverse of `Upholds=`) alongside `WantedBy=`/`RequiredBy=` when deciding whether to start an inactive changed unit. A unit bound to or upheld by an active unit is one systemd would have running, so it is now started — and its container image pre-pulled — instead of being classified as "stays stopped" and having the image download inline later. Relationships that never propagate a start are deliberately still ignored: `ConsistsOf` (`PartOf=` propagates stop and restart only), `RequisiteOf` (`Requisite=` checks a unit rather than starting it), `ConflictedBy` (`Conflicts=` stops it), and `TriggeredBy` (socket/timer/path activation is on demand, so a reboot leaves the service inactive until the trigger fires — a triggered service that is already running when its file changes is still restarted, as this only affects inactive units). `UpheldBy=` requires systemd 249 or newer; on older systemd it is not reported and contributes nothing, leaving the previous behaviour intact. Reverse dependencies are also deduplicated now, so a unit that both wants and requires the changed unit is reported once in verbose logs.
- Image pre-pull now follows the activation decision: images are pulled only for changed `.container` and `.image` files whose unit will be running after the sync — started or restarted by sync, or pulled in by systemd as a dependency of a unit sync starts (for example an `.image` unit required by a `.container`). A unit that stays stopped — inactive with no active unit wanting or requiring it — no longer causes a pull of an image nothing is about to run. Verbose runs log the skipped files.
- Template unit instances now follow the same activation policy as every other unit: a running instance is restarted, a stopped one is started only when an active unit wants or requires it. See **Fixed** below.
- **Breaking (MSRV):** Minimum Supported Rust Version bumped from 1.70 to 1.74. This matches the toolchain CI builds and tests against, and lets the codebase use `io::Error::other` and other ≥1.74 conveniences. Downstream consumers on Rust 1.70–1.73 will need to upgrade their toolchain.

### Fixed

- Sync no longer resurrects stopped instances of a changed template unit. Expanding a template (`myapp@.container`) used `systemctl list-units --all`, which reports loaded-but-inactive and failed instances as well as running ones, and every returned instance was passed straight to `systemctl restart`. An instance an operator had stopped by hand came back on the next sync that touched the template file. Instances are now judged individually by the same rule as any other unit — restart when active, start only when some active unit wants or requires them, otherwise leave alone — so a failed or stopped instance nothing depends on stays down. Images are pre-pulled for a changed template only when at least one of its instances will actually be running.

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
