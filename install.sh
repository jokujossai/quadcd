#!/bin/sh
set -eu

# Require root privileges
if [ "$(id -u)" -ne 0 ]; then
  echo "Error: must run as root" >&2
  exit 1
fi

REPO="jokujossai/quadcd"
BINDIR="${BINDIR:-/usr/local/bin}"
PREFIX="${PREFIX:-/etc/systemd}"
QUADCD_BIN="${BINDIR}/quadcd"

arch="$(uname -m)"
case "$arch" in
  x86_64|aarch64) ;;
  *) echo "Unsupported architecture: $arch" >&2; exit 1 ;;
esac

# Track installed files for cleanup on failure
INSTALLED_FILES=""
TMPDIR=""
cleanup() {
  status=$?

  if [ -n "$TMPDIR" ] && [ -d "$TMPDIR" ]; then
    rm -rf "$TMPDIR"
  fi

  if [ "$status" -ne 0 ] && [ -n "$INSTALLED_FILES" ]; then
    echo "Installation failed. The following files were partially installed:" >&2
    echo "$INSTALLED_FILES" | tr ' ' '\n' | sed 's/^/  /' >&2
    echo "You may want to remove them manually." >&2
  fi

  return "$status"
}
trap cleanup 0
trap 'exit 1' HUP INT TERM

TMPDIR="$(mktemp -d)"

# NOTE TO MAINTAINERS: README.md pipes this script from the main branch, but the
# script installs the latest published release. Keep changes here backward
# compatible with the latest published release assets and dist/ service files.
# Fetch latest release tag
RELEASE_URL="https://github.com/${REPO}/releases"
tag="$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" \
  | sed -n 's/.*"tag_name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')"
if [ -z "$tag" ]; then
  echo "Failed to fetch latest release" >&2
  exit 1
fi

binary_name="quadcd-linux-${arch}"
url="${RELEASE_URL}/download/${tag}/${binary_name}"

# Fetch and validate checksums before downloading the binary
echo "Fetching checksums..."
if ! curl -fsSL -o "${TMPDIR}/SHA256SUMS" "${RELEASE_URL}/download/${tag}/SHA256SUMS"; then
  echo "Error: SHA256SUMS not found for release ${tag}" >&2
  exit 1
fi
if ! grep -q "${binary_name}" "${TMPDIR}/SHA256SUMS"; then
  echo "Error: SHA256SUMS does not contain entry for ${binary_name}" >&2
  exit 1
fi

echo "Downloading quadcd ${tag} for ${arch}..."
curl -fsSL -o "${TMPDIR}/${binary_name}" "$url"

# Verify checksum
(cd "${TMPDIR}" && sha256sum -c --ignore-missing SHA256SUMS)
echo "Checksum verified"

install -Dm755 "${TMPDIR}/${binary_name}" "${QUADCD_BIN}"
INSTALLED_FILES="${INSTALLED_FILES} ${QUADCD_BIN}"
echo "Installed ${QUADCD_BIN}"

mkdir -p "${PREFIX}/user-generators"
ln -sf "${QUADCD_BIN}" "${PREFIX}/user-generators/quadcd"
INSTALLED_FILES="${INSTALLED_FILES} ${PREFIX}/user-generators/quadcd"
echo "Linked user generator"

mkdir -p "${PREFIX}/system-generators"
ln -sf "${QUADCD_BIN}" "${PREFIX}/system-generators/quadcd"
INSTALLED_FILES="${INSTALLED_FILES} ${PREFIX}/system-generators/quadcd"
echo "Linked system generator"

# Install sync service units
raw="https://raw.githubusercontent.com/${REPO}/${tag}"

mkdir -p "${PREFIX}/system"
curl -fsSL -o "${PREFIX}/system/quadcd-sync.service" "${raw}/dist/quadcd-sync.service"
INSTALLED_FILES="${INSTALLED_FILES} ${PREFIX}/system/quadcd-sync.service"
echo "Installed system sync service"

mkdir -p "${PREFIX}/user"
curl -fsSL -o "${PREFIX}/user/quadcd-sync.service" "${raw}/dist/quadcd-sync-user.service"
INSTALLED_FILES="${INSTALLED_FILES} ${PREFIX}/user/quadcd-sync.service"
echo "Installed user sync service"

# Patch sync service binary paths if BINDIR was overridden
if [ "${BINDIR}" != "/usr/local/bin" ]; then
  for file in "${PREFIX}/system/quadcd-sync.service" "${PREFIX}/user/quadcd-sync.service"; do
    tmp=$(mktemp)
    awk -v old="/usr/local/bin/quadcd" -v new="${QUADCD_BIN}" '{ gsub(old, new); print }' "$file" > "$tmp" && mv "$tmp" "$file"
  done
fi

if [ -d /run/systemd/system ]; then
  systemctl daemon-reload
  systemctl enable quadcd-sync.service
  echo "Enabled system sync service"

  systemctl --global enable quadcd-sync.service
  echo "Enabled user sync service"
else
  echo "Note: systemd not detected, skipping service enablement" >&2
fi
