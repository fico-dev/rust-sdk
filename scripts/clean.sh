#!/usr/bin/env bash
set -e

###### README ######
# This script runs `cargo clean` when the build artifacts are stale: the last
# clean was more than CLEAN_MAX_AGE_DAYS days ago (default 7), or the target
# directory holds more than CLEAN_MAX_SIZE_GIB GiB (default 10). Otherwise it
# does nothing. The pre-push hooks run it before they build.

# Ensure we are operating from the root of the repository.
REPO_ROOT="$(git rev-parse --show-toplevel)"
cd "$REPO_ROOT"

# Read the limits, which must be positive whole numbers.
: "${CLEAN_MAX_AGE_DAYS:=7}"
: "${CLEAN_MAX_SIZE_GIB:=10}"
for name in CLEAN_MAX_AGE_DAYS CLEAN_MAX_SIZE_GIB; do
    if ! [[ "${!name}" =~ ^[1-9][0-9]*$ ]]; then
        echo "❌ Error: '$name' must be a positive whole number, got '${!name}'." >&2
        exit 1
    fi
done

# `cargo clean` deletes the target directory, and the stamp in it.
TARGET_DIR="${CARGO_TARGET_DIR:-target}"
STAMP="$TARGET_DIR/.last-clean"

if [ ! -d "$TARGET_DIR" ]; then
    echo "✨ Nothing to clean: '$TARGET_DIR' does not exist."
    exit 0
fi

# A missing or malformed stamp starts the count now.
now="$(date +%s)"
last="$(cat "$STAMP" 2>/dev/null || true)"
if ! [[ "$last" =~ ^[0-9]+$ ]]; then
    echo "$now" > "$STAMP"
    last="$now"
fi

age_days=$(( (now - last) / 86400 ))
size_kib="$(du -sk "$TARGET_DIR" | cut -f1)"
size_gib="$(awk "BEGIN { printf \"%.1f\", $size_kib / 1048576 }")"

if (( now - last > CLEAN_MAX_AGE_DAYS * 86400 )); then
    reason="the last clean was $age_days days ago (limit: $CLEAN_MAX_AGE_DAYS)"
elif (( size_kib > CLEAN_MAX_SIZE_GIB * 1048576 )); then
    reason="'$TARGET_DIR' holds $size_gib GiB (limit: $CLEAN_MAX_SIZE_GIB)"
else
    echo "✨ Nothing to clean: '$TARGET_DIR' holds $size_gib GiB, last cleaned $age_days days ago."
    exit 0
fi

echo "⚙️ Cleaning build artifacts: $reason."
cargo clean
echo "✅ Successfully cleaned build artifacts."
