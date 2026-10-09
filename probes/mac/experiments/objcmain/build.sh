#!/bin/bash
# Builds the ObjC-main process-shape variant of the DDC experiment.
#
# ObjC entry + Foundation runtime around the SAME shim source used by
# kvmprobe ddc-raw and the linkorder binaries - the only variable this
# binary adds to the gradient is the process shape itself. The build
# self-checks that real ObjC codegen is present (objc_msgSend references).
set -euo pipefail
cd "$(dirname "$0")"

SHIM=../../Sources/m1ddcShim
OUT=../../.build/objcmain
mkdir -p "$OUT"

COMMIT="$(git rev-parse --short HEAD 2>/dev/null || echo unknown)"
MIN_VER=13.0   # matches Package.swift platform macOS 13

clang -O2 -Wall -Wextra -fobjc-arc -mmacosx-version-min="$MIN_VER" \
    -I"$SHIM/include" \
    -DSHAPE_DESC="\"objc-main (commit $COMMIT)\"" \
    main.m "$SHIM/m1ddcShim.c" \
    -framework CoreDisplay -framework CoreGraphics -framework IOKit -framework Foundation \
    -o "$OUT/objcmain"

# The whole point of this binary is the ObjC process shape - fail the build
# if the ObjC runtime is not actually wired in.
if ! nm "$OUT/objcmain" 2>/dev/null | grep -q "objc_msgSend"; then
    echo "error: no objc_msgSend reference in $OUT/objcmain - ObjC codegen missing" >&2
    exit 1
fi

echo "built: $(cd "$OUT" && pwd)/objcmain"
echo "  $(file "$OUT/objcmain" | cut -d: -f2- | xargs)"
echo "  linked frameworks: $(otool -L "$OUT/objcmain" | awk '/\.framework/{print $1}' | xargs -n1 basename | grep -v '^objcmain$' | tr '\n' ' ')"
echo "list online displays (index -> CG display id):"
echo "  $OUT/objcmain --list"
