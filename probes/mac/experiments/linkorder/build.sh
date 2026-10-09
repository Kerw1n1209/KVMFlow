#!/bin/bash
# Builds the two link-order variants of the standalone DDC experiment and
# self-verifies their recorded LC_LOAD_DYLIB order with otool.
#
# No SwiftPM/Xcode needed - plain clang against the same shim source the
# kvmprobe ddc-raw path uses (../../Sources/m1ddcShim/m1ddcShim.c).
set -euo pipefail
cd "$(dirname "$0")"

SHIM=../../Sources/m1ddcShim
OUT=../../.build/linkorder
mkdir -p "$OUT"

COMMIT="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
MIN_VER=13.0   # matches Package.swift platform macOS 13

build_variant() {
    local name="$1" desc="$2" expected="$3"
    shift 3
    clang -O2 -Wall -Wextra -mmacosx-version-min="$MIN_VER" \
        -I"$SHIM/include" \
        -DLINKORDER_DESC="\"$desc (commit $COMMIT)\"" \
        main.c "$SHIM/m1ddcShim.c" \
        "$@" -framework CoreFoundation \
        -o "$OUT/$name"
    # Fail the build if the linker did not record the intended relative
    # order of the three frameworks under test.
    local recorded
    recorded=$(otool -l "$OUT/$name" \
        | awk '/ cmd / && /LC_LOAD_DYLIB/ {grab=1} grab && /name / {print $2; grab=0}' \
        | xargs -n1 basename \
        | grep -E '^(CoreDisplay|CoreGraphics|IOKit)$' \
        | paste -sd' ' -)
    if [ "$recorded" != "$expected" ]; then
        echo "error: $name LC_LOAD_DYLIB order mismatch" >&2
        echo "  recorded: $recorded" >&2
        echo "  expected: $expected" >&2
        exit 1
    fi
    echo "built: $(cd "$OUT" && pwd)/$name"
    echo "  LC_LOAD_DYLIB framework order: $recorded"
}

# m1ddc order (otool on /opt/homebrew/bin/m1ddc, 2026-09-13)
build_variant linkorder-cd "CoreDisplay->CoreGraphics->IOKit" \
    "CoreDisplay CoreGraphics IOKit" \
    -framework CoreDisplay -framework CoreGraphics -framework IOKit

# kvmprobe order (Package.swift linkerSettings, otool-verified 2026-09-13)
build_variant linkorder-io "IOKit->CoreGraphics->CoreDisplay" \
    "IOKit CoreGraphics CoreDisplay" \
    -framework IOKit -framework CoreGraphics -framework CoreDisplay

echo
echo "list online displays (index -> CG display id):"
echo "  $OUT/linkorder-cd --list"
