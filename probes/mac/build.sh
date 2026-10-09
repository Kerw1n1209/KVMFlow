#!/bin/bash
set -euo pipefail
cd "$(dirname "$0")"
swift build -c release
BIN="$(pwd)/.build/release/kvmprobe"
echo "built: $BIN"
"$BIN" version
