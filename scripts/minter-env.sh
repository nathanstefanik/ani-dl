# Shared minter fixture setup — sourced by scripts/minter-*.sh
#
# Config contract:
#   stdio  — ani-dl-minter-run.sh; config.toml [minter] transport = "stdio"
#   http   — minter-http-start.sh + minter-push.sh; transport = "http"
#
# Fixture: ~/.config/ani-dl/material.json (see docs/minter-protocol.md)

CONFIG_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/ani-dl"
MATERIAL_JSON="$CONFIG_DIR/material.json"
RUNTIME_DIR="${XDG_RUNTIME_DIR:-$HOME/.cache/ani-dl/runtime}"

export ANI_DL_MINTER_FIXTURE="$MATERIAL_JSON"
export XDG_RUNTIME_DIR="$RUNTIME_DIR"

die() {
  echo "error: $*" >&2
  exit 1
}

validate_material() {
  [[ -f "$MATERIAL_JSON" ]] || die "missing fixture: $MATERIAL_JSON"

  python3 - "$MATERIAL_JSON" <<'PY'
import base64, binascii, json, sys, time

path = sys.argv[1]
required = [
    "epoch", "partB", "mask", "buildId",
    "referer", "apiBase", "cdnBase", "expiresAt",
]

with open(path) as f:
    try:
        data = json.load(f)
    except json.JSONDecodeError as e:
        print(f"invalid JSON: {e}", file=sys.stderr)
        sys.exit(1)

if not isinstance(data, dict):
    print("material.json must be a JSON object", file=sys.stderr)
    sys.exit(1)

missing = [k for k in required if k not in data]
if missing:
    print(f"missing required keys: {', '.join(missing)}", file=sys.stderr)
    sys.exit(1)

try:
    part_b = base64.b64decode(data["partB"], validate=True)
except Exception as e:
    print(f"partB: invalid base64: {e}", file=sys.stderr)
    sys.exit(1)
if len(part_b) != 32:
    print(f"partB: expected 32 bytes, got {len(part_b)}", file=sys.stderr)
    sys.exit(1)

try:
    mask = binascii.unhexlify(data["mask"])
except Exception as e:
    print(f"mask: invalid hex: {e}", file=sys.stderr)
    sys.exit(1)
if len(mask) != 32:
    print(f"mask: expected 32 bytes, got {len(mask)}", file=sys.stderr)
    sys.exit(1)

grace_ms = data.get("graceMs", 300000)
if grace_ms > 300000:
    print(f"warning: graceMs={grace_ms} > 300000; consider 300000", file=sys.stderr)

now_ms = int(time.time() * 1000)
expires_at = int(data["expiresAt"])
if now_ms >= expires_at - grace_ms:
    print("warning: material may be stale (now >= expiresAt - graceMs)", file=sys.stderr)
PY
  [[ $? -eq 0 ]] || die "fixture validation failed: $MATERIAL_JSON"
}

ensure_config_stdio() {
  local cfg="$CONFIG_DIR/config.toml"
  [[ -f "$cfg" ]] || return 0

  local transport
  transport=$(awk '
    /^\[minter\]/ { in_minter=1; next }
    /^\[/ { in_minter=0 }
    in_minter && /^[[:space:]]*transport[[:space:]]*=/ {
      gsub(/.*=[[:space:]]*"/, ""); gsub(/".*/, ""); print; exit
    }
  ' "$cfg")

  if [[ -n "$transport" && "$transport" != "stdio" ]]; then
    echo "warning: minter.transport=$transport in config.toml; this script expects stdio" >&2
  fi
}

preflight_binaries() {
  command -v ani-dl >/dev/null 2>&1 || die "ani-dl not found on PATH"
  command -v ani-dl-minter >/dev/null 2>&1 || die "ani-dl-minter not found on PATH"
}
