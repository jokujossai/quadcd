#!/bin/sh
set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
cd "$SCRIPT_DIR/.."

IMAGE_NAME="${IMAGE_NAME:-quadcd-test}"

podman build \
  -t "$IMAGE_NAME" \
  --target test \
  -f tests/containerized/image/Containerfile \
  .

podman run \
  --rm \
  -t \
  --privileged \
  --systemd=always \
  --timeout 120 \
  "$IMAGE_NAME"
