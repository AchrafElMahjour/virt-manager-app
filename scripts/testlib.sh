#!/usr/bin/env bash
# Bibliothèque commune aux scripts de test (source, ne pas exécuter).
# Attend : BASE, TMP (dossier temporaire) ; fournit call/check/assert/section/jf/ws_probe/summary.
PASS=0; FAIL=0; SKIP=0
c_ok=$'\033[32m'; c_ko=$'\033[31m'; c_dim=$'\033[2m'; c_off=$'\033[0m'
[ -t 1 ] || { c_ok=""; c_ko=""; c_dim=""; c_off=""; }

jf() { # jf '.a.b.0' < json
  python3 -c '
import sys, json
d = json.load(sys.stdin)
for k in [p for p in sys.argv[1].split(".") if p]:
    d = d[int(k)] if isinstance(d, list) else d.get(k)
    if d is None: break
print("" if d is None else (json.dumps(d) if isinstance(d, (dict, list)) else d))' "$1" 2>/dev/null
}

CODE=""; BODY=""
call() { # call JAR METHOD PATH [JSON] [extra curl args…]
  local jar="$1" method="$2" path="$3" data="${4:-}"; shift 4 2>/dev/null || shift $#
  local args=(-s -o "$TMP/body" -w '%{http_code}' -X "$method" -b "$jar" -c "$jar" "$BASE$path")
  [ -n "$data" ] && args+=(-H 'Content-Type: application/json' -d "$data")
  CODE="$(curl "${args[@]}" "$@")"; BODY="$(cat "$TMP/body" 2>/dev/null)"
}
check() { # check "description" expected_code
  if [ "$CODE" = "$2" ]; then PASS=$((PASS+1)); printf ' %s✔%s %s %s(%s)%s\n' "$c_ok" "$c_off" "$1" "$c_dim" "$CODE" "$c_off"
  else FAIL=$((FAIL+1)); printf ' %s✘%s %s — attendu %s, reçu %s\n   %s\n' "$c_ko" "$c_off" "$1" "$2" "$CODE" "${BODY:0:300}"; fi
}
assert() { # assert "description" "valeur" "attendue"
  if [ "$2" = "$3" ]; then PASS=$((PASS+1)); printf ' %s✔%s %s %s(%s)%s\n' "$c_ok" "$c_off" "$1" "$c_dim" "$2" "$c_off"
  else FAIL=$((FAIL+1)); printf ' %s✘%s %s — attendu « %s », reçu « %s »\n' "$c_ko" "$c_off" "$1" "$3" "$2"; fi
}
section() { printf '\n%s\n' "── $1"; }


skip() { SKIP=$((SKIP+1)); printf ' %s–%s %s %s(ignoré)%s\n' "$c_dim" "$c_off" "$1" "$c_dim" "$c_off"; }

# ws_probe UUID ORIGIN COOKIE [HV] → code HTTP (si pas 101) ou « RFB:<bannière> »
ws_probe() {
python3 - "$BASE" "$1" "$2" "$3" "${4:-}" <<'PY'
import base64, os, socket, sys, urllib.parse
base, uuid, origin, cookie, hv = sys.argv[1:6]
u = urllib.parse.urlparse(base)
s = socket.create_connection((u.hostname, u.port or 80), timeout=15)
key = base64.b64encode(os.urandom(16)).decode()
path = f"/api/v1/vms/{uuid}/vnc" + (f"?hv={hv}" if hv else "")
lines = [f"GET {path} HTTP/1.1", f"Host: {u.hostname}:{u.port}", "Upgrade: websocket",
         "Connection: Upgrade", f"Sec-WebSocket-Key: {key}", "Sec-WebSocket-Version: 13",
         "Sec-WebSocket-Protocol: binary"]
if origin != "-": lines.append(f"Origin: {origin}")
if cookie != "-": lines.append(f"Cookie: vm_session={cookie}")
s.sendall(("\r\n".join(lines) + "\r\n\r\n").encode())
buf = b""
while b"\r\n\r\n" not in buf:
    chunk = s.recv(4096)
    if not chunk: break
    buf += chunk
head, _, rest = buf.partition(b"\r\n\r\n")
code = head.split(b" ")[1].decode()
if code != "101":
    print(code); sys.exit()
while len(rest) < 2: rest += s.recv(4096)
n = rest[1] & 0x7F
while len(rest) < 2 + n: rest += s.recv(4096)
print("RFB:" + rest[2:2+n].decode(errors="replace").strip()[:11])
PY
}

summary() {
  printf '\n%s\n' "══ Résultat : $PASS réussis, $FAIL échoués, $SKIP ignorés"
  [ "$FAIL" -eq 0 ]
}
