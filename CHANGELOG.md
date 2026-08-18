# Changelog

## [Unreleased]

### Added

- `quadcd status [--no-fetch] [--user] [--json] [-v]` subcommand: read-only diagnostic that reports per-repo state (up-to-date / ahead / behind / diverged / url mismatch / missing / error) and per-service systemd state (active/sub/result, enabled, `NeedDaemonReload`, restart-pending, `NRestarts`, uptime, and a restart-loop heuristic). Default fetches; `--no-fetch` skips the network. Exits non-zero on any problem so it is usable from cron/monitoring. `--json` emits a single structured document; without it, a plain-text table is printed.

### Changed

- Sync now recognises `BoundBy=` (reverse of `BindsTo=`) and `UpheldBy=` (reverse of `Upholds=`) alongside `WantedBy=`/`RequiredBy=` when deciding whether to start an inactive changed unit. A unit bound to or upheld by a unit that is coming up is one systemd would have running, so it is now started — and its container image pre-pulled — instead of being classified as "stays stopped" and having the image download inline later. Relationships that never propagate a start are deliberately still ignored: `ConsistsOf` (`PartOf=` propagates stop and restart only), `RequisiteOf` (`Requisite=` checks a unit rather than starting it), `ConflictedBy` (`Conflicts=` stops it), and `TriggeredBy` (socket/timer/path activation is on demand, so a reboot leaves the service inactive until the trigger fires — a triggered service that is already running when its file changes is still restarted, as this only affects inactive units). `UpheldBy=` requires systemd 249 or newer; on older systemd it is not reported and contributes nothing, leaving the previous behaviour intact. Reverse dependencies are also deduplicated now, so a unit that both wants and requires the changed unit is reported once in verbose logs.
- Image pre-pull now follows the activation decision: images are pulled only for changed `.container` and `.image` files whose unit will be running after the sync — started or restarted by sync, or pulled in by systemd as a dependency of a unit sync starts (for example an `.image` unit required by a `.container`). A unit that stays stopped — inactive with nothing that would start it coming up — no longer causes a pull of an image nothing is about to run. Verbose runs log the skipped files.
- Template unit instances now follow the same activation policy as every other unit: a running instance is restarted, a stopped one is started only when a unit that would start it is coming up. See **Fixed** below.
- **Breaking (API):** `SystemdTrait` gained a required `pending_start_jobs` method (backed by `systemctl list-jobs`); out-of-tree implementations must add it. It also gained `activation_state`, which reports a unit's `ActiveState`/`SubState` — every activation decision now derives from that one pair, so a unit costs a single `systemctl show` per planning pass instead of one call per question. That one has a default implementation projecting `show_state`, so implementors need not write it.
- **Breaking (MSRV):** Minimum Supported Rust Version bumped from 1.70 to 1.74. This matches the toolchain CI builds and tests against, and lets the codebase use `io::Error::other` and other ≥1.74 conveniences. Downstream consumers on Rust 1.70–1.73 will need to upgrade their toolchain.
- Internal: `SystemdTrait::is_active` now has a default implementation projecting `activation_state`, the same non-breaking pattern `activation_state` already uses for `show_state`; `Systemd`'s override (a separate `systemctl is-active` call) is gone, since the planner has not called `is_active` since `activation_state` was introduced. `Systemd::activation_state` and `Systemd::show_state` now share their `systemctl show` invocation and `KEY=value` parsing through a private `show_properties` helper instead of duplicating both. No behavior change for `SystemdTrait` callers.

### Fixed

- Sync no longer resurrects stopped instances of a changed template unit. Expanding a template (`myapp@.container`) used `systemctl list-units --all`, which reports loaded-but-inactive and failed instances as well as running ones, and every returned instance was passed straight to `systemctl restart`. An instance an operator had stopped by hand came back on the next sync that touched the template file. Instances are now judged individually by the same rule as any other unit — restart when running, start only when something that would start them is coming up, otherwise leave alone — so a failed or stopped instance nothing depends on stays down. Images are pre-pulled for a changed template only when at least one of its instances will actually be running.
- Sync now starts a changed, inactive unit when the unit that would start it is *coming up* — active, activating, or holding a queued start job — instead of requiring it to be fully active. A target implicitly orders itself after the units it wants and only reaches `active` once their jobs have finished, so while those are starting it reads `ActiveState=inactive` with a queued start job — targets never report `activating`. Whether the boot target is still in that state when the first sync runs is a race rather than a guarantee: `quadcd-sync.service` is `Type=simple`, so its own start job completes as soon as the process is forked and the target never waits on the sync itself; it is the other units ordered before the target that usually keep it queued, and on a fast boot it may already be `active`. Both readings have to work, and only the `active` one did before. On a fresh host, where the initial clone marks every unit file as changed, this made every unit look unwanted: nothing was started and (since the pre-pull change above) no image was pulled, so the deployment only came up on the next boot. Units an operator stopped by hand are still left alone: they are `inactive` with no job at all. A dependant that is merely `activating (auto-restart)` — failed and waiting out `Restart=` — does not authorise a start either, so a crash-looping service cannot resurrect a unit that was stopped for maintenance. A changed unit that is itself already coming up is now left to the job systemd has in flight rather than being commanded again, while still being pre-pulled.
- Units whose source file was deleted are now stopped when they are `activating`, or running but mid-`reload`, not only when `systemctl is-active` reported them plainly active. Sync stops deleted units before `daemon-reload`, because afterwards systemd no longer knows the unit and `systemctl stop` cannot reach it — but a unit caught part-way through starting slipped through that check, finished bringing its container up moments later, and left it running with nothing able to stop it. A unit that merely holds a *queued* start job is deliberately not stopped: its job has not run, so there is no container to orphan, and stopping it before `daemon-reload` would cancel the boot transaction's own start job and propagate a stop across the still-live `.requires/` edge to the target that requires it.
- A changed unit that is itself crash-looping (`activating (auto-restart)`) is now restarted into the new configuration instead of being left `AlreadyStarting`. That classification is right for a *dependant* in the same state — a crash loop is not evidence anything it wants belongs running — but wrong for the changed unit's own state: no job is actually in flight (systemd already gave up on the last attempt and is waiting out `Restart=`), so nothing is interrupted by restarting now, and the previous behaviour left the new configuration stranded until whichever future backoff attempt happened to land — or indefinitely, if the crash loop exhausted `StartLimitBurst` first.

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
