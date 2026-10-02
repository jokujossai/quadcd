# QuadCD

[![CI](https://github.com/jokujossai/quadcd/actions/workflows/build.yml/badge.svg?branch=main)](https://github.com/jokujossai/quadcd/actions/workflows/build.yml)
[![License: MIT](.github/badges/license.svg)](LICENSE)

QuadCD deploys Quadlet and systemd unit files from local directories or git repositories and keeps systemd in sync.

Supported files: Quadlet `.container`, `.volume`, `.network`, `.kube`, `.image`, `.build`, `.pod`, `.artifact`; systemd `.service`, `.socket`, `.device`, `.mount`, `.automount`, `.swap`, `.target`, `.path`, `.timer`, `.slice`, `.scope`.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/jokujossai/quadcd/main/install.sh | sudo sh
```

Installs the latest release binary, the generator symlinks and the sync service units, then prints how to enable the service. `BINDIR` (default `/usr/local/bin`) and `PREFIX` (default `/etc/systemd`) change the install locations.

<details>
<summary>Manual install</summary>

```sh
# 1. Download quadcd-linux-<arch> and SHA256SUMS from the latest release, then:
sha256sum -c --ignore-missing SHA256SUMS
sudo install -Dm755 quadcd-linux-$(uname -m) /usr/local/bin/quadcd

# 2. Generator symlinks
sudo ln -sf /usr/local/bin/quadcd /etc/systemd/user-generators/quadcd
sudo ln -sf /usr/local/bin/quadcd /etc/systemd/system-generators/quadcd

# 3. Sync service units (optional)
sudo curl -fsSL -o /etc/systemd/system/quadcd-sync.service \
  https://raw.githubusercontent.com/jokujossai/quadcd/main/dist/quadcd-sync.service
sudo curl -fsSL -o /etc/systemd/user/quadcd-sync.service \
  https://raw.githubusercontent.com/jokujossai/quadcd/main/dist/quadcd-sync-user.service
sudo systemctl daemon-reload
```

</details>

<details>
<summary>Uninstall</summary>

```sh
sudo systemctl disable quadcd-sync.service
sudo systemctl --global disable quadcd-sync.service
sudo rm -f /usr/local/bin/quadcd /etc/systemd/user-generators/quadcd /etc/systemd/system-generators/quadcd
sudo rm -f /etc/systemd/system/quadcd-sync.service /etc/systemd/user/quadcd-sync.service
sudo systemctl daemon-reload
```

</details>

## Quick start

### Local files

Put unit files in any subdirectory of the data directory and reload systemd:

```sh
mkdir -p ~/.local/share/quadcd/local
cat > ~/.local/share/quadcd/local/hello.container <<'EOF'
[Container]
Image=quay.io/podman/hello:latest
EOF
systemctl --user daemon-reload
```

### Git sync

`~/.config/quadcd.toml`:

```toml
[repositories.myapp]
url = "https://github.com/example/myapp.git"
branch = "production"   # optional, default: remote default branch
interval = "30m"        # optional, for --service (s/m/h/d)
```

```sh
quadcd sync --user                                # once
systemctl --user enable --now quadcd-sync.service # continuously
```

The repo is cloned into `~/.local/share/quadcd/myapp/`.

## How sync activates units

After pulling, sync runs `daemon-reload` and handles each changed unit the way a reboot would:

- **Active** (or crash-looping): restarted.
- **Inactive**: started only if a unit that wants it (`WantedBy=`, `RequiredBy=`, `BoundBy=`, `UpheldBy=`) is coming up — active, activating, or with a queued start job. Units stopped by hand stay stopped. `PartOf=`, `Requisite=`, `Conflicts=` and socket/timer/path activation do not count.
- **Already starting**: left to its job; the new config applies on its next restart.
- **Template**: each loaded instance follows the rules above.
- **Deleted**: stopped before `daemon-reload`.

Images are pre-pulled only for units that will be running.

## Unit settings: `[X-QuadCD]`

Optional section in any unit file. systemd and Quadlet ignore it; no variable substitution.

```ini
# app.build
[Build]
ImageTag=localhost/app
SetWorkingDirectory=repo

[X-QuadCD]
StartOnSync=true
Watch=Containerfile
Watch=repo/**
```

| Key | Effect |
|-----|--------|
| `StartOnSync=` | Start the unit when it changes, even if nothing wants it. These units go first and are waited for, so a build finishes before the container using its image. Ignored on templates. |
| `Watch=` | Glob (`*`, `?`, `**`) of repo files, relative to the unit's directory; a change marks the unit changed. Repeatable. Only changes pulled by sync count. |

## Reference

### Commands

```text
quadcd generate [-v] [-no-kmsg-log] [-user] [-dryrun] normal-dir [early-dir] [late-dir]
quadcd sync [--service] [--sync-only] [--force] [--accept-new-host-keys] [-i] [--user] [-v]
quadcd status [--no-fetch] [--json] [--user] [-v]
quadcd version
quadcd help
```

| Option | Description |
|--------|-------------|
| `-dryrun` | Show what `generate` would produce, without changes |
| `--service` | Keep running; sync each repo on its `interval` and reload on config changes |
| `--sync-only` | Pull only; no `daemon-reload`, pre-pull or start/restart |
| `--force` | Allow URL changes; `git reset --hard` instead of `pull --ff-only` |
| `--accept-new-host-keys` | Trust unknown SSH host keys on first connect |
| `-i` | Interactive git/SSH (host key and credential prompts) |
| `--no-fetch` | `status` without network access |
| `--json` | `status` as JSON |
| `--user`, `-user` | Force user mode |
| `-v` | Verbose |

`status` reports repo and service state and exits non-zero on any problem. `generate` also runs automatically when invoked through a generator symlink, or with `SYSTEMD_SCOPE` set and generator-style arguments.

### Paths

| Mode | Data directory | Config |
|------|----------------|--------|
| User | `~/.local/share/quadcd/` | `~/.config/quadcd.toml` |
| System | `/var/lib/quadcd/` | `/etc/quadcd.toml` |

Files are processed in path order; if two produce the same unit name, the later wins with a warning. SSH known hosts are kept in `<data dir>/.known_hosts`.

### Environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `QUADCD_CONFIG` | see [Paths](#paths) | Sync config file |
| `QUADCD_UNIT_DIRS` | all data dir subdirectories | Single source directory |
| `QUADLET_UNIT_DIRS` | auto-detected | Quadlet output directory |
| `QUADLET_DROPINS_UNIT_DIRS` | standard Quadlet directory | Where `*.d/` drop-ins are read from |
| `PODMAN_GENERATOR_PATH` | auto-detected | Podman generator binary |
| `GIT_COMMAND` | `git` | Git binary |
| `GIT_TIMEOUT` | `300` s | Git operation timeout |
| `PODMAN_PULL_TIMEOUT` | `60` s | Image pre-pull timeout |
| `SYSTEMD_SCOPE` | unset | `system` = system mode, other non-empty = user mode |

### Variable substitution

`${VAR}` in unit files is replaced from `<data dir>/.env`; undefined variables are left alone. `${QUADCD_REPO_ROOT}` is always the unit's source directory:

```ini
[Container]
Image=${REGISTRY}/app:${TAG}
Volume=${QUADCD_REPO_ROOT}/configs/app.yaml:/etc/app.yaml:Z,ro
```

### Drop-ins

`*.d/` directories from `~/.config/containers/systemd/` (user) or `/etc/containers/systemd/` (system) are applied, e.g.:

- `foo.container.d/*.conf` — one unit
- `foo-.container.d/*.conf` — units named `foo-*`
- `container.d/*.conf` — all containers

### `[Install]` in plain systemd units

`WantedBy=` and `RequiredBy=` become `.wants`/`.requires` symlinks, like Quadlet does, since generated units cannot be enabled. `Alias=`, `Also=` and `DefaultInstance=` are ignored.

## Development

See [CONTRIBUTING.md](CONTRIBUTING.md).
