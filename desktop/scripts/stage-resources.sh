#!/bin/bash
# Prepare Tauri bundle resources. The Rust runtime is linked into the app.
set -euo pipefail
exec node "$(dirname "$0")/prepare-tauri.js"
