#!/usr/bin/env bash
# Push fixture material to a running HTTP minter (binding B).
#
# Requires ani-dl-minter --listen already running with the same XDG_RUNTIME_DIR.
# Config contract: config.toml [minter] transport = "http"
#
# Usage: ./scripts/minter-push.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=minter-env.sh
source "$SCRIPT_DIR/minter-env.sh"

validate_material
command -v curl >/dev/null 2>&1 || die "curl not found"

HANDSHAKE="$RUNTIME_DIR/ani-dl-minter.json"
[[ -f "$HANDSHAKE" ]] || die "no handshake file: $HANDSHAKE (start minter-http-start.sh first)"

read -r PORT TOKEN < <(python3 - "$HANDSHAKE" <<'PY'
import json, sys
hs = json.load(open(sys.argv[1]))
print(hs["port"], hs["token"])
PY
)

PAYLOAD=$(python3 - "$MATERIAL_JSON" <<'PY'
import json, sys
material = json.load(open(sys.argv[1]))
print(json.dumps({"source": "allanime", "material": material}))
PY
)

curl -sf -X POST "http://127.0.0.1:${PORT}/ingest" \
  -H "Authorization: Bearer ${TOKEN}" \
  -H "Content-Type: application/json" \
  -d "$PAYLOAD"
echo
