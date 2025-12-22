#!/bin/sh
# Host-side helper: inject the repo's bench_fio.sh into an ext4 rootfs image.
#
# Default target image: ./arceos/disk.img
# Usage:
#   sh scripts/inject_bench_fio_to_disk.sh
#   sh scripts/inject_bench_fio_to_disk.sh path/to/disk.img
# Env:
#   DISK_IMG=...   override target image
#
# This script uses sudo to mount the image via loopback.

set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"

SRC="${REPO_ROOT}/scripts/bench_fio.sh"
DISK_IMG="${DISK_IMG:-${1:-${REPO_ROOT}/arceos/disk.img}}"

if [ ! -f "$SRC" ]; then
  echo "ERROR: source script not found: $SRC" >&2
  exit 1
fi

if [ ! -f "$DISK_IMG" ]; then
  echo "ERROR: disk image not found: $DISK_IMG" >&2
  echo "Hint: run 'make rootfs' first (it creates arceos/disk.img)." >&2
  exit 1
fi

if [ "$(id -u)" -ne 0 ]; then
  SUDO=sudo
else
  SUDO=
fi

MNT_DIR="$(mktemp -d -t starryos-mnt.XXXXXX)"
cleanup() {
  set +e
  $SUDO umount "$MNT_DIR" >/dev/null 2>&1 || true
  rmdir "$MNT_DIR" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

# ext4 rootfs image is a plain filesystem image; mount it directly via loop.
$SUDO mount -o loop "$DISK_IMG" "$MNT_DIR"

# Install the script in common locations for convenience.
$SUDO mkdir -p "$MNT_DIR/bin"
$SUDO install -m 0755 "$SRC" "$MNT_DIR/bin/bench_fio.sh"
$SUDO install -m 0755 "$SRC" "$MNT_DIR/bench_fio.sh"

# Best-effort sync; not all hosts require it.
$SUDO sync >/dev/null 2>&1 || true

echo "OK: injected bench_fio.sh into: $DISK_IMG" >&2
echo " - guest path: /bin/bench_fio.sh" >&2
echo " - guest path: /bench_fio.sh" >&2
