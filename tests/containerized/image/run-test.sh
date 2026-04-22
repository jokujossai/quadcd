#!/bin/bash
set -euo pipefail

EXIT_CODE=1

cleanup() {
    if [ "$EXIT_CODE" -eq 0 ]; then
        echo "RESULT: PASS"
    else
        echo "RESULT: FAIL"
    fi
    systemctl exit "$EXIT_CODE" 2>/dev/null || systemctl poweroff --no-block || true
}
trap cleanup EXIT

echo "=== QuadCD System Integration Tests ==="
/usr/local/bin/quadcd-test --ignored --test-threads=1

echo "=== QuadCD User Integration Tests ==="

# Ensure the user's runtime dir exists and the user systemd instance is running
uid=$(id -u quadcd-test)
mkdir -p "/run/user/${uid}"
chown quadcd-test:quadcd-test "/run/user/${uid}"
systemctl start "user@${uid}.service"

su - quadcd-test -c "export XDG_RUNTIME_DIR=/run/user/${uid} && /usr/local/bin/quadcd-test --ignored --test-threads=1"

EXIT_CODE=0
