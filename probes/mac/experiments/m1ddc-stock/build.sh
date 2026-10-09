#!/bin/bash
# Builds the STOCK m1ddc v1.2.0 (tag 2549fec) from its own unmodified
# sources with its own Makefile - zero source changes, zero shim code.
#
# Product-decision experiment for the KVM-1 DDC read defect: the brew
# bottle reads correct values on the target machine while every same-source
# reimplementation we built returns canned frames. If THIS self-built
# binary reads 15/7 on the target machine, the "sidecar spawns a vendored
# self-built m1ddc subprocess" path is viable for the MVP; if it also
# returns canned frames, the bottle differs from its own sources in some
# unknown build-time way (sealed-cause note).
set -euo pipefail
cd "$(dirname "$0")"

VENDORED=m1ddc-2549fec
OUT=../../.build/m1ddc-stock
mkdir -p "$OUT"

# Their Makefile, their flags (-Wall -Werror -Wextra -fmodules, link
# -framework CoreDisplay; the @import directives pull the rest).
make -C "$VENDORED"

cp "$VENDORED/m1ddc" "$OUT/m1ddc-selfbuilt"
chmod +x "$OUT/m1ddc-selfbuilt"

echo "built: $(cd "$OUT" && pwd)/m1ddc-selfbuilt"
echo "  source: m1ddc v1.2.0 tag 2549fec (vendored, unmodified)"
echo "  sha256: $(shasum -a 256 "$OUT/m1ddc-selfbuilt" | awk '{print $1}')"
echo "  signature: $(codesign -dv "$OUT/m1ddc-selfbuilt" 2>&1 | grep -m1 'Signature=' || echo unknown)"
echo "  linked frameworks: $(otool -L "$OUT/m1ddc-selfbuilt" | awk '/\.framework/{print $1}' | xargs -n1 basename | grep -v '^m1ddc-selfbuilt$' | tr '\n' ' ')"
echo
echo "sanity (built-in display machine):"
echo "  $OUT/m1ddc-selfbuilt display list"
