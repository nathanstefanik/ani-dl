#!/usr/bin/env bash
# Start HTTP minter companion (binding B).
#
# Writes handshake to $XDG_RUNTIME_DIR/ani-dl-minter.json; pair with minter-push.sh.
# Config contract: config.toml [minter] transport = "http"
#
# Usage: ./scripts/minter-http-start.sh [addr]
#   addr defaults to 127.0.0.1:8765

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=minter-env.sh
source "$SCRIPT_DIR/minter-env.sh"

validate_material
preflight_binaries

mkdir -p "$RUNTIME_DIR"

ADDR="${1:-127.0.0.1:8765}"
exec ani-dl-minter --listen "$ADDR"
