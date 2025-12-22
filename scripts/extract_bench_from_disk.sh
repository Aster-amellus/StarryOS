#!/bin/sh
# Host-side helper: extract StarryOS fio bench results from an ext4 rootfs image.
#
# Default image: ./arceos/disk.img
# It copies directories like /root/bench_YYYYMMDD_HHMMSS into a host output folder.
# Note: /tmp is mounted as tmpfs inside StarryOS, so bench outputs written under /tmp
# will NOT be persisted into the ext4 image and thus cannot be extracted after shutdown.
#
# Usage:
#   sh scripts/extract_bench_from_disk.sh
#   sh scripts/extract_bench_from_disk.sh arceos/disk.img bench_out
# Env:
#   DISK_IMG=...   override target image
#   OUT=...        override output directory

set -eu

SCRIPT_DIR="$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)"
REPO_ROOT="$(CDPATH= cd -- "${SCRIPT_DIR}/.." && pwd)"

DISK_IMG="${DISK_IMG:-${1:-${REPO_ROOT}/arceos/disk.img}}"
OUT="${OUT:-${2:-${REPO_ROOT}/bench_out}}"

if [ ! -f "$DISK_IMG" ]; then
  echo "ERROR: disk image not found: $DISK_IMG" >&2
  exit 1
fi

if [ "$(id -u)" -ne 0 ]; then
  SUDO=sudo
else
  SUDO=
fi

TS="$(date +%Y%m%d_%H%M%S 2>/dev/null || echo now)"
OUT_DIR="$OUT/$TS"
mkdir -p "$OUT_DIR"

MNT_DIR="$(mktemp -d -t starryos-mnt.XXXXXX)"
cleanup() {
  set +e
  $SUDO umount "$MNT_DIR" >/dev/null 2>&1 || true
  rmdir "$MNT_DIR" >/dev/null 2>&1 || true
}
trap cleanup EXIT INT TERM

$SUDO mount -o loop "$DISK_IMG" "$MNT_DIR"

copied=0

# Find bench_* directories in the image (keep scope small for speed).
# Typical output is /root/bench_*, but we search a bit wider to be robust.
find "$MNT_DIR" -maxdepth 4 -type d -name 'bench_*' 2>/dev/null | while IFS= read -r d; do
  # Turn /mnt/root/bench_xxx into root_bench_xxx for a stable destination name.
  rel="${d#"$MNT_DIR"/}"
  safe_name="$(printf '%s' "$rel" | tr '/' '_')"
  dest="$OUT_DIR/$safe_name"
  cp -a "$d" "$dest"
  copied=$((copied + 1))
done

if [ "$copied" -eq 0 ]; then
  echo "WARN: no bench_* directories found in the image." >&2
  echo "Tip: in guest, write results under /root (not /tmp), then shutdown cleanly and retry." >&2
fi

echo "OK: extracted $copied bench directories to: $OUT_DIR" >&2
