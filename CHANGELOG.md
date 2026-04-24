# Changelog

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
