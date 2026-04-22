# QuadCD

[![CI](https://github.com/jokujossai/quadcd/actions/workflows/build.yml/badge.svg?branch=main)](https://github.com/jokujossai/quadcd/actions/workflows/build.yml)
[![License: MIT](.github/badges/license.svg)](LICENSE)

QuadCD deploys Quadlet and systemd unit files from local directories or git repositories, then keeps systemd in sync.

## Supported Files

- **Quadlet**: `.container`, `.volume`, `.network`, `.kube`, `.image`, `.build`, `.pod`, `.artifact`
- **Systemd**: `.service`, `.socket`, `.device`, `.mount`, `.automount`, `.swap`, `.target`, `.path`, `.timer`, `.slice`, `.scope`

## Usage

Place your unit files in a subdirectory of the data directory and reload systemd. All subdirectories of the data directory are treated as source directories (e.g., `local/` for manually placed files, or named after git repos for synced files).

Source directories are processed in lexicographic order, and files within each source tree are processed in lexicographic path order. If two files produce the same unit filename, the later path wins and quadcd warns about the duplicate.

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

This clones the repository into `~/.local/share/quadcd/myapp/`, installs any supported Quadlet or systemd unit files it contains, reloads systemd, and starts or restarts changed units as needed.

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

Sync pulls unit files from configured git repositories into the data directory, then triggers `systemctl daemon-reload` and restarts changed units with `systemctl restart`.

SSH known hosts are stored in the data directory (`.known_hosts` file) to avoid issues with system SSH config under systemd sandboxing. Use `--accept-new-host-keys` for initial setup to automatically accept host keys on first connect, or `-i` for fully interactive SSH (manual host key approval, credential prompts).

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

QuadCD supports `${VAR}` substitution using a `.env` file in the data directory (`~/.local/share/quadcd/.env` or `/var/lib/quadcd/.env`).

Only variables defined in the `.env` file will be substituted, protecting other shell variables in your unit files.

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

## Drop-in Files

Podman and systemd support drop-in configuration files that let you override or extend base unit files. QuadCD automatically symlinks `*.d/` drop-in directories from the standard Quadlet directory into the generator's working directory, so they are applied when the Podman generator runs.

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

Set `QUADLET_DROPINS_UNIT_DIRS` to override the directory
that QuadCD scans for `*.d/` drop-in directories. By default
it uses the standard Quadlet directory for the current mode
(`~/.config/containers/systemd/` for user mode,
`/etc/containers/systemd/` for system mode).

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/jokujossai/quadcd/main/install.sh | sudo sh
```

This POSIX `sh` installer is fetched from `main`, but it installs the latest
published release binary to `/usr/local/bin/quadcd`, creates symlinks in both
user and system generator directories, installs sync service unit files, and
enables them via `systemctl`. The installer is kept backward compatible with the
latest published release assets and bundled service files.

### Installer Environment Variables

| Variable | Default          | Description              |
|----------|------------------|--------------------------|
| `BINDIR` | `/usr/local/bin` | Binary install location  |
| `PREFIX` | `/etc/systemd`   | Systemd generator prefix |

<details>
<summary>Manual install</summary>

1. Download the binary for your architecture from the
   [latest release](https://github.com/jokujossai/quadcd/releases/latest)
   and install it:

   ```sh
   sudo install -Dm755 quadcd /usr/local/bin/quadcd
   ```

2. Create generator symlinks:

   ```sh
   sudo ln -sf /usr/local/bin/quadcd /etc/systemd/user-generators/quadcd
   sudo ln -sf /usr/local/bin/quadcd /etc/systemd/system-generators/quadcd
   ```

3. Reload systemd:

   ```sh
   sudo systemctl daemon-reload
   systemctl --user daemon-reload
   ```

Optionally install the [sync service units](dist/) for git-based continuous deployment.

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
