# QuadCD

[![CI](https://github.com/jokujossai/quadcd/actions/workflows/build.yml/badge.svg?branch=main)](https://github.com/jokujossai/quadcd/actions/workflows/build.yml)
[![License: MIT](.github/badges/license.svg)](LICENSE)

QuadCD deploys Quadlet and systemd unit files from local directories or git repositories, then keeps systemd in sync.

## Supported Files

- **Quadlet**: `.container`, `.volume`, `.network`, `.kube`, `.image`, `.build`, `.pod`, `.artifact`
- **Systemd**: `.service`, `.socket`, `.device`, `.mount`, `.automount`, `.swap`, `.target`, `.path`, `.timer`, `.slice`, `.scope`

## Usage

Put unit files in any subdirectory of the data directory (e.g. `local/`, or one per synced repo) and reload systemd. Files are processed in path order; if two produce the same unit name, the later wins with a warning.

| Mode   | Data Directory             |
|--------|----------------------------|
| User   | `~/.local/share/quadcd/`   |
| System | `/var/lib/quadcd/`         |

```sh
# User mode
systemctl --user daemon-reload

# System mode
sudo systemctl daemon-reload
```

## Examples

### Local Mode Example

Create a local source directory and add a Quadlet file:

```sh
mkdir -p ~/.local/share/quadcd/local
cat > ~/.local/share/quadcd/local/hello.container <<'EOF'
[Container]
Image=quay.io/podman/hello:latest
EOF
systemctl --user daemon-reload
```

QuadCD installs the source file into the generator working directory and Podman generates the corresponding user unit on reload.

### Sync Mode Example

Create a sync config:

```toml
[repositories.myapp]
url = "https://github.com/example/myapp.git"
branch = "production"
interval = "30m"
```

Store it at `~/.config/quadcd.toml`, then run:

```sh
quadcd sync --user
```

This clones the repo into `~/.local/share/quadcd/myapp/`, reloads systemd and activates changed units.

## Command Line Options

### Generate (systemd generator mode)

```sh
quadcd generate [-v] [-no-kmsg-log] [-user] [-dryrun] normal-dir [early-dir] [late-dir]
```

| Option | Description |
|--------|-------------|
| `-v` | Verbose output |
| `-no-kmsg-log` | Disable kmsg logging (for quadlet compatibility) |
| `-user` | Force user mode |
| `-dryrun` | Dry-run mode (no changes, implies -v) |

Generator mode is also activated automatically when:
- The binary is invoked via a symlink whose basename is not `quadcd` (e.g., `podman-user-generator` or `podman-system-generator`).
- `SYSTEMD_SCOPE` is set and the positional arguments look like a generator invocation (1 or 3 args, first is an existing directory).

#### `[Install]` Sections in Plain Systemd Units

Generated units cannot be enabled, so the generator turns `WantedBy=` and `RequiredBy=` into `<target>.wants/<unit>` and `<target>.requires/<unit>` symlinks, like Quadlet does. `Alias=`, `Also=` and `DefaultInstance=` are ignored.

```ini
# backup.timer → timers.target.wants/backup.timer
[Install]
WantedBy=timers.target
```

### Sync (git-based continuous deployment)

```sh
quadcd sync [--service] [--sync-only] [--force] [--accept-new-host-keys] [-i] [--user] [-v]
```

| Option | Description |
|--------|-------------|
| `-v` | Verbose output |
| `--service` | Long-running service mode with file watching and interval-based syncing |
| `--sync-only` | Pull changes but skip `daemon-reload`, image pre-pulls, and service start/restart |
| `--force` | Allow URL changes and use `git reset --hard` instead of `git pull --ff-only` |
| `--accept-new-host-keys` | Accept unknown SSH host keys on first connect (TOFU) |
| `-i`, `--interactive` | Enable interactive mode (allows SSH prompts for host keys, credentials) |
| `--user` | Force user mode |

Sync pulls the configured repositories, runs `daemon-reload` and activates changed units the way a reboot would:

- **Active** (or crash-looping) unit: restarted.
- **Inactive** unit: started only if a unit that wants it (`WantedBy=`, `RequiredBy=`, `BoundBy=`, `UpheldBy=`) is coming up — active, activating, or with a queued start job. Units stopped by hand stay stopped. `PartOf=`, `Requisite=`, `Conflicts=` and socket/timer/path activation do not count.
- **Already starting** unit: left to its job; the new config applies on its next restart.
- **Template**: each loaded instance follows the rules above.
- **Deleted** unit: stopped before `daemon-reload`.

Images are pre-pulled only for units that will be running. `UpheldBy=` needs systemd ≥ 249.

#### `[X-QuadCD]` Settings

Per-unit sync settings, read from the source file without variable substitution. systemd and Quadlet ignore the section.

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

| Key | Description |
|-----|-------------|
| `StartOnSync=` | Start the unit when it changes, even if nothing wants it. For builds that should not run at boot. |
| `Watch=` | Glob of repo files (relative to the unit's directory) whose change marks the unit changed. Repeatable; empty value clears. |

- `StartOnSync=` units are started (or restarted) first and waited for, so a build finishes before the container using its image (`Pull=never`). A unit stopped by hand is started again when it changes. Ignored on templates.
- `Watch=` supports `*`, `?` and `**`; paths outside the repository are ignored. Only changes pulled by sync are seen, not a manual `git pull`.

SSH known hosts are kept in `<data dir>/.known_hosts`. Use `--accept-new-host-keys` on first connect, or `-i` for interactive SSH.

### Version

```sh
quadcd version
```

Print the version and exit. The `-version` flag also works anywhere in the command line for backwards compatibility.

### Help

```sh
quadcd help
```

Print usage information and exit.

#### Configuration

Create a config file at `~/.config/quadcd.toml` (user) or `/etc/quadcd.toml` (system):

```toml
[repositories.myapp]
url = "https://github.com/example/myapp.git"
branch = "production"    # optional, defaults to remote default
interval = "30m"         # optional, for --service mode (s/m/h/d)
```

Override the config path with `QUADCD_CONFIG`.

## Runtime Environment Variables

These environment variables override quadcd's default behavior:

| Variable | Default | Description |
|----------|---------|-------------|
| `QUADCD_CONFIG` | `~/.config/quadcd.toml` or `/etc/quadcd.toml` | Path to the sync configuration file |
| `QUADCD_UNIT_DIRS` | (all subdirectories of data dir) | Override the source directory (single path) |
| `QUADLET_UNIT_DIRS` | (auto-detected) | Override the quadlet output directory |
| `QUADLET_DROPINS_UNIT_DIRS` | mode-specific standard Quadlet drop-ins dir | Override the directory scanned for `*.d/` Quadlet drop-ins |
| `PODMAN_GENERATOR_PATH` | (auto-detected) | Override the podman generator binary path |
| `GIT_COMMAND` | `git` | Override the git binary path |
| `GIT_TIMEOUT` | `300` seconds | Timeout for git operations |
| `PODMAN_PULL_TIMEOUT` | `60` seconds | Timeout for pre-pulling container images in sync mode |
| `SYSTEMD_SCOPE` | (unset) | Systemd scope detection (`system` = system mode, any other non-empty value = user mode) |

## Variable Substitution

`${VAR}` is substituted from `<data dir>/.env`. Variables not defined there are left alone.

### Example

**~/.local/share/quadcd/.env**:

```sh
REGISTRY=docker.io
IMAGE_TAG=latest
```

**~/.local/share/quadcd/local/myapp.container**:

```ini
[Container]
Image=${REGISTRY}/myimage:${IMAGE_TAG}
```

### Reserved Variables

`${QUADCD_REPO_ROOT}` is the absolute path of the unit's source directory and cannot be overridden. Use it to mount files committed next to the unit:

**~/.local/share/quadcd/<repo>/myapp.container**:

```ini
[Container]
Image=ghcr.io/me/app:latest
Volume=${QUADCD_REPO_ROOT}/configs/app.yaml:/etc/app.yaml:Z,ro
```

## Drop-in Files

QuadCD symlinks `*.d/` drop-in directories from the standard Quadlet directory into the generator's working directory, so Podman applies them.

### Drop-in Directories

| Mode   | Quadlet Drop-ins | Systemd Drop-ins |
|--------|------------------|------------------|
| User   | `~/.config/containers/systemd/*.d/` | `~/.config/systemd/user/*.d/` |
| System | `/etc/containers/systemd/*.d/` | `/etc/systemd/system/*.d/` |

### How Drop-ins Work

For a unit file `foo.container`, create drop-in files in:

1. `foo.container.d/*.conf` - Specific to this unit
2. `foo-.container.d/*.conf` - For units starting with `foo-`
3. `container.d/*.conf` - Global defaults for all containers

Drop-ins are merged in alphabetical order, with more specific paths taking precedence.

### Example: Global Container Defaults

**~/.config/containers/systemd/container.d/10-defaults.conf**:

```ini
[Container]
LogDriver=journald

[Service]
Restart=always
```

This applies to all `.container` units.

**Note:** Drop-in values override values defined in the base quadlet files.

### Example: Unit-Specific Override

**~/.config/containers/systemd/nginx.container.d/20-volumes.conf**:

```ini
[Container]
Volume=/data/nginx:/usr/share/nginx/html:ro
```

This only applies to `nginx.container`.

### Overriding the Drop-in Source Directory

Set `QUADLET_DROPINS_UNIT_DIRS` to scan another directory for `*.d/` drop-ins.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/jokujossai/quadcd/main/install.sh | sudo sh
```

Installs the latest release binary, generator symlinks and sync service units, then prints how to enable the service.

### Installer Environment Variables

| Variable | Default          | Description              |
|----------|------------------|--------------------------|
| `BINDIR` | `/usr/local/bin` | Binary install location  |
| `PREFIX` | `/etc/systemd`   | Systemd generator prefix |

<details>
<summary>Manual install</summary>

1. Download the binary and `SHA256SUMS` for your architecture from the
   [latest release](https://github.com/jokujossai/quadcd/releases/latest)
   and verify the checksum:

   ```sh
   sha256sum -c --ignore-missing SHA256SUMS
   ```

2. Install the binary:

   ```sh
   sudo install -Dm755 quadcd-linux-$(uname -m) /usr/local/bin/quadcd
   ```

3. Create generator symlinks:

   ```sh
   sudo ln -sf /usr/local/bin/quadcd /etc/systemd/user-generators/quadcd
   sudo ln -sf /usr/local/bin/quadcd /etc/systemd/system-generators/quadcd
   ```

4. Install the sync service units (optional, for git-based continuous deployment):

   ```sh
   sudo curl -fsSL -o /etc/systemd/system/quadcd-sync.service \
     https://raw.githubusercontent.com/jokujossai/quadcd/main/dist/quadcd-sync.service
   sudo curl -fsSL -o /etc/systemd/user/quadcd-sync.service \
     https://raw.githubusercontent.com/jokujossai/quadcd/main/dist/quadcd-sync-user.service
   ```

5. Reload systemd:

   ```sh
   sudo systemctl daemon-reload
   systemctl --user daemon-reload
   ```

6. Enable the sync service if installed in step 4:

   ```sh
   # System mode
   sudo systemctl enable --now quadcd-sync.service

   # User mode (per user)
   systemctl --user enable --now quadcd-sync.service
   ```

</details>

### Uninstall

```sh
sudo systemctl disable quadcd-sync.service
sudo systemctl --global disable quadcd-sync.service
sudo rm -f /usr/local/bin/quadcd
sudo rm -f /etc/systemd/user-generators/quadcd
sudo rm -f /etc/systemd/system-generators/quadcd
sudo rm -f /etc/systemd/system/quadcd-sync.service /etc/systemd/user/quadcd-sync.service
sudo systemctl daemon-reload
```

## Testing

Run the standard checks with:

```sh
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

Run the containerized integration suite with the helper script:

```sh
./scripts/run-containerized-tests.sh
```

This builds `tests/containerized/image/Containerfile` with Podman and then runs the resulting test image with `--privileged` and `--systemd=always`.
