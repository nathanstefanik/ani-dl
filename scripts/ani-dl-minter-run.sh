#!/usr/bin/env bash
# Primary entrypoint — stdio minter binding with fixture material.
#
# Requires config.toml [minter] transport = "stdio" (warns if mismatched).
# Saves material to ~/.config/ani-dl/material.json (see capture-material.js).
#
# Usage: ./scripts/ani-dl-minter-run.sh [ani-dl args…]

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=minter-env.sh
source "$SCRIPT_DIR/minter-env.sh"

validate_material
ensure_config_stdio
export ANI_DL_MINTER_FIXTURE

exec ani-dl "$@"
