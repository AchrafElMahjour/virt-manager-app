#!/usr/bin/env bash
# Test approfondi de Virt Manager (lots A à F) contre une instance EN COURS D'EXÉCUTION.
# Chaque effet est vérifié directement dans libvirt avec `virsh`, pas seulement dans l'API.
#
#   ./run.sh test-full                         # recommandé : instance temporaire, tout automatique
#   ADMIN_PASSWORD='…' ./scripts/full-test.sh  # ou contre l'instance déjà lancée
#
# Il crée puis supprime ses propres objets (préfixe « full- ») : VM, volumes, ISO, pool, comptes,
# hyperviseur. Il ne touche à aucune autre VM. Le répertoire /var/lib/libvirt/virtmgr-fulltest
# (vide) peut rester après le test : `sudo rmdir /var/lib/libvirt/virtmgr-fulltest`.
# Dépendances : bash, curl, python3, virsh. Facultatif : ssh (lot E), terraform / packer / ansible,
# python3-jinja2 + python3-yaml (validation des modèles IaC par libvirt).
set -u
BASE="${BASE:-http://127.0.0.1:8080}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA_DIR="${VIRTMGR_DATA_DIR:-$ROOT/data}"
ADMIN_USER="${ADMIN_USER:-admin}"
if [ -z "${ADMIN_PASSWORD:-}" ] && [ -f "$DATA_DIR/admin-initial-password.txt" ]; then
  ADMIN_PASSWORD="$(sed -n 's/^mot de passe : //p' "$DATA_DIR/admin-initial-password.txt")"
fi
: "${ADMIN_PASSWORD:?Définissez ADMIN_PASSWORD (le mot de passe initial a peut-être été changé)}"
POOL="${POOL:-default}"
NET="${NET:-default}"
VIRSH_URI="${LIBVIRT_URI:-qemu:///system}"
command -v virsh >/dev/null || { echo "virsh est requis pour ce test (sudo apt install libvirt-clients)"; exit 2; }
command -v python3 >/dev/null || { echo "python3 est requis"; exit 2; }
STAMP="$(date +%s)"
TMP="$(mktemp -d)"
JAR_ADMIN="$TMP/admin.jar"; JV="$TMP/viewer.jar"; JO="$TMP/operator.jar"
# shellcheck source=scripts/testlib.sh
source "$(dirname "${BASH_SOURCE[0]}")/testlib.sh"
vq() { LC_ALL=C virsh -c "$VIRSH_URI" "$@"; }
vq uri >/dev/null 2>&1 || { echo "libvirt injoignable ($VIRSH_URI)"; exit 2; }

FAKE=18a09112-5827-45e0-ac41-89f8b3f46635
FPOOL=full-test-pool; FPOOL_PATH=/var/lib/libvirt/virtmgr-fulltest
CLEAN_VMS=(); CLEAN_USERS=(full-lock full-pw full-view full-op full-adm2); CLEAN_HVS=(full-ssh full-bad)

pw_of() { echo "Pw-$1-0123456789"; }
session_of() { awk '$6=="vm_session"{print $7}' "$1" 2>/dev/null; }
orphans() { vq vol-list "$POOL" 2>/dev/null | awk '/virtmgr-/{print $1}' | sort; }
wait_state() { # wait_state nom état [secondes]
  local _; for _ in $(seq 1 "${3:-20}"); do [ "$(vq domstate "$1" 2>/dev/null | head -1)" = "$2" ] && return 0; sleep 1; done; return 1; }
mkvm() { # mkvm nom réseau|"" iso|"" [pool]  → VM_UUID, enregistre pour le nettoyage
  local name="$1" net="$2" iso="$3" pool="${4:-$POOL}" netj="null" isoj='"iso_pool":null,"iso_name":null'
  [ -n "$net" ] && netj="\"$net\""
  [ -n "$iso" ] && isoj="\"iso_pool\":\"$POOL\",\"iso_name\":\"$iso\""
  CLEAN_VMS+=("$name")
  call "$JAR_ADMIN" POST /api/v1/vms "{\"name\":\"$name\",\"vcpus\":1,\"memory_mib\":256,\"disk_gib\":1,\"pool\":\"$pool\",\"network\":$netj,$isoj}"
  VM_UUID="$(echo "$BODY" | jf .uuid)"
}
raw_put_status() { # raw_put_status chemin content_length cookie → code HTTP (sans envoyer de corps)
python3 - "$BASE" "$1" "$2" "$3" <<'PY'
import socket, sys, urllib.parse
base, path, cl, cookie = sys.argv[1:5]
u = urllib.parse.urlparse(base)
s = socket.create_connection((u.hostname, u.port or 80), timeout=10)
s.sendall(f"PUT {path} HTTP/1.1\r\nHost: {u.hostname}:{u.port}\r\nCookie: vm_session={cookie}\r\nContent-Length: {cl}\r\nContent-Type: application/octet-stream\r\n\r\n".encode())
s.shutdown(socket.SHUT_WR)   # le client n'enverra jamais le corps annoncé
print(s.recv(200).split(b" ")[1].decode())
PY
}

purge_pool() { local v; for v in $(vq vol-list "$1" 2>/dev/null | awk 'NR>2 && $1!=""{print $1}'); do vq vol-delete --pool "$1" "$v" >/dev/null 2>&1; done; }
cleanup() {
  trap - EXIT
  for n in "${CLEAN_VMS[@]}"; do
    vq destroy "$n" >/dev/null 2>&1
    local files; files="$(vq domblklist "$n" --details 2>/dev/null | awk '$2=="disk"{print $4}')"
    vq undefine "$n" --managed-save --snapshots-metadata >/dev/null 2>&1 || vq undefine "$n" >/dev/null 2>&1
    for f in $files; do vq vol-delete "$f" >/dev/null 2>&1; done
  done
  for v in $(vq vol-list "$POOL" 2>/dev/null | awk '/ full-/{print $1}'); do vq vol-delete --pool "$POOL" "$v" >/dev/null 2>&1; done
  if vq pool-info "$FPOOL" >/dev/null 2>&1; then
    vq pool-start "$FPOOL" >/dev/null 2>&1; purge_pool "$FPOOL"
    vq pool-destroy "$FPOOL" >/dev/null 2>&1; vq pool-undefine "$FPOOL" >/dev/null 2>&1
  fi
  for h in "${CLEAN_HVS[@]}"; do curl -s -o /dev/null -b "$JAR_ADMIN" -X DELETE "$BASE/api/v1/hypervisors/$h"; done
  for u in "${CLEAN_USERS[@]}"; do curl -s -o /dev/null -b "$JAR_ADMIN" -X DELETE "$BASE/api/v1/users/$u"; done
  rm -rf "$TMP"
}
trap cleanup EXIT

# ═══════════════════════════════════════════════════════════════════════════════════════════════
section "Préparation"
call "$JAR_ADMIN" POST /api/v1/auth/login "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASSWORD\"}"; check "connexion admin" 200
ORPHANS_BEFORE="$(orphans)"
call "$JAR_ADMIN" GET /api/v1/resources
HAS_NET=0; echo "$BODY" | jf .networks | grep -q "\"$NET\"" && HAS_NET=1
[ "$HAS_NET" = 1 ] && assert "réseau « $NET » actif (VM avec carte réseau testées)" ok ok || skip "réseau « $NET » inactif (virsh net-start $NET) : tests MAC/réseau ignorés"
dd if=/dev/urandom of="$TMP/small.iso" bs=1M count=3 status=none
ISO="full-$STAMP.iso"
call "$JAR_ADMIN" PUT "/api/v1/storage/pools/$POOL/isos/$ISO" "" --data-binary @"$TMP/small.iso" -H 'Content-Type: application/octet-stream'; check "ISO de test téléversée" 201
for spec in "full-lock viewer" "full-pw viewer" "full-view viewer" "full-op operator" "full-adm2 admin"; do
  set -- $spec; call "$JAR_ADMIN" POST /api/v1/users "{\"username\":\"$1\",\"password\":\"$(pw_of "$1")\",\"role\":\"$2\"}"; check "compte $1 ($2)" 201
done

# ═══════════════════════════════════════════════════════════════════════════════════════════════
section "LOT A1 — Connexion et verrouillage"
for _ in 1 2 3 4 5; do call "$TMP/lock.jar" POST /api/v1/auth/login '{"username":"full-lock","password":"mauvais-mot-de-passe"}'; done
call "$TMP/lock.jar" POST /api/v1/auth/login '{"username":"full-lock","password":"mauvais-mot-de-passe"}'; check "6ᵉ échec : verrouillé" 429
assert "message de verrouillage explicite" "$(echo "$BODY" | jf .error | grep -c 'Réessayez')" "1"
call "$TMP/lock.jar" POST /api/v1/auth/login "{\"username\":\"full-lock\",\"password\":\"$(pw_of full-lock)\"}"; check "le BON mot de passe est refusé pendant le verrouillage" 429
call "$JV" POST /api/v1/auth/login "{\"username\":\"full-view\",\"password\":\"$(pw_of full-view)\"}"; check "un autre compte n'est pas affecté" 200

section "LOT A2 — Changement de mot de passe"
call "$TMP/p1.jar" POST /api/v1/auth/login "{\"username\":\"full-pw\",\"password\":\"$(pw_of full-pw)\"}"; check "session 1" 200
call "$TMP/p2.jar" POST /api/v1/auth/login "{\"username\":\"full-pw\",\"password\":\"$(pw_of full-pw)\"}"; check "session 2 (autre navigateur)" 200
call "$TMP/p1.jar" POST /api/v1/auth/password '{"current_password":"faux-actuel-123456","new_password":"NouveauMotDePasse-1"}'; check "mot de passe actuel erroné refusé" 401
call "$TMP/p1.jar" POST /api/v1/auth/password "{\"current_password\":\"$(pw_of full-pw)\",\"new_password\":\"court\"}"; check "nouveau mot de passe trop court refusé" 400
call "$TMP/p1.jar" POST /api/v1/auth/password "{\"current_password\":\"$(pw_of full-pw)\",\"new_password\":\"$(pw_of full-pw)\"}"; check "identique à l'ancien refusé" 400
call "$TMP/p1.jar" POST /api/v1/auth/password "{\"current_password\":\"$(pw_of full-pw)\",\"new_password\":\"NouveauMotDePasse-1\"}"; check "changement accepté" 200
call "$TMP/p2.jar" GET /api/v1/auth/me; check "l'autre session est fermée" 401
call "$TMP/p1.jar" GET /api/v1/auth/me; check "la session courante reste ouverte" 200
call "$TMP/p3.jar" POST /api/v1/auth/login "{\"username\":\"full-pw\",\"password\":\"$(pw_of full-pw)\"}"; check "ancien mot de passe refusé" 401
call "$TMP/p3.jar" POST /api/v1/auth/login '{"username":"full-pw","password":"NouveauMotDePasse-1"}'; check "nouveau mot de passe accepté" 200

section "LOT A3 — Matrice des rôles (toutes les routes sensibles)"
call "$JO" POST /api/v1/auth/login "{\"username\":\"full-op\",\"password\":\"$(pw_of full-op)\"}"; check "connexion opérateur" 200
ADMIN_ONLY=("POST /api/v1/vms/$FAKE/clone" "PUT /api/v1/vms/$FAKE/hardware" "POST /api/v1/vms/$FAKE/disks" "PUT /api/v1/vms/$FAKE/media"
  "DELETE /api/v1/vms/$FAKE" "POST /api/v1/vms" "POST /api/v1/storage/pools" "POST /api/v1/storage/pools/x/start" "POST /api/v1/storage/pools/x/stop"
  "DELETE /api/v1/storage/pools/x" "PUT /api/v1/storage/pools/x/autostart" "POST /api/v1/storage/pools/x/volumes" "DELETE /api/v1/storage/pools/x/volumes/y"
  "PUT /api/v1/storage/pools/x/isos/y.iso" "POST /api/v1/hypervisors" "DELETE /api/v1/hypervisors/x" "POST /api/v1/users" "GET /api/v1/users" "GET /api/v1/audit")
for r in "${ADMIN_ONLY[@]}"; do set -- $r
  call "$JV" "$1" "$2" '{}'; [ "$CODE" = 403 ] && PASS=$((PASS+1)) || { FAIL=$((FAIL+1)); printf ' %s✘%s lecteur : %s %s → %s (attendu 403)\n' "$c_ko" "$c_off" "$1" "$2" "$CODE"; }
  call "$JO" "$1" "$2" '{}'; [ "$CODE" = 403 ] && PASS=$((PASS+1)) || { FAIL=$((FAIL+1)); printf ' %s✘%s opérateur : %s %s → %s (attendu 403)\n' "$c_ko" "$c_off" "$1" "$2" "$CODE"; }
done
printf ' %s✔%s %d routes administrateur refusées au lecteur ET à l'"'"'opérateur\n' "$c_ok" "$c_off" "${#ADMIN_ONLY[@]}"
for act in start shutdown suspend resume; do
  call "$JV" POST "/api/v1/vms/$FAKE/$act" '{}'; check "lecteur : $act refusé" 403
  call "$JO" POST "/api/v1/vms/$FAKE/$act" '{}'; check "opérateur : $act autorisé (VM inconnue → 404)" 404
done
for p in /api/v1/vms /api/v1/resources /api/v1/storage/pools "/api/v1/storage/pools/$POOL/volumes" /api/v1/hypervisors /api/v1/templates /api-docs/openapi.json; do
  call "$JV" GET "$p"; [ "$CODE" = 200 ] && PASS=$((PASS+1)) || { FAIL=$((FAIL+1)); printf ' %s✘%s lecteur : GET %s → %s (attendu 200)\n' "$c_ko" "$c_off" "$p" "$CODE"; }
done
printf ' %s✔%s 7 routes de lecture autorisées au lecteur\n' "$c_ok" "$c_off"

section "LOT A4 — Effets des changements de compte sur les sessions"
call "$JV" GET /api/v1/vms; check "lecteur connecté" 200
call "$JAR_ADMIN" PUT /api/v1/users/full-view '{"disabled":true}'; check "désactivation du compte" 200
call "$JV" GET /api/v1/vms; check "session du compte désactivé fermée" 401
call "$TMP/d.jar" POST /api/v1/auth/login "{\"username\":\"full-view\",\"password\":\"$(pw_of full-view)\"}"; check "connexion impossible tant que désactivé" 401
call "$JAR_ADMIN" PUT /api/v1/users/full-view '{"disabled":false}'; check "réactivation" 200
call "$JV" POST /api/v1/auth/login "{\"username\":\"full-view\",\"password\":\"$(pw_of full-view)\"}"; check "reconnexion possible" 200
call "$JAR_ADMIN" PUT /api/v1/users/full-view '{"password":"MotDePasseReinitialise-9"}'; check "réinitialisation par l'admin" 200
call "$JV" GET /api/v1/vms; check "réinitialisation : sessions fermées" 401
call "$JV" POST /api/v1/auth/login '{"username":"full-view","password":"MotDePasseReinitialise-9"}'; check "connexion avec le nouveau mot de passe" 200
call "$JAR_ADMIN" PUT /api/v1/users/full-view '{"role":"viewer"}'; check "mise à jour du rôle (inchangé : les sessions sont fermées par précaution)" 200
call "$JV" POST /api/v1/auth/login '{"username":"full-view","password":"MotDePasseReinitialise-9"}'; check "reconnexion du lecteur" 200
call "$JAR_ADMIN" PUT /api/v1/users/admin '{"disabled":true}' ; check "pas d'auto-désactivation" 409
call "$TMP/a2.jar" POST /api/v1/auth/login "{\"username\":\"full-adm2\",\"password\":\"$(pw_of full-adm2)\"}"; check "second administrateur" 200
call "$TMP/a2.jar" PUT /api/v1/users/full-adm2 '{"role":"viewer"}'; check "le second admin ne peut pas se rétrograder" 409

section "LOT A5 — Cookie, en-têtes, origine, écoute, fichiers"
HDRS="$(curl -si -X POST "$BASE/api/v1/auth/login" -H 'Content-Type: application/json' -d "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASSWORD\"}")"
SC="$(echo "$HDRS" | grep -i '^set-cookie:' | head -1)"
assert "cookie HttpOnly" "$(echo "$SC" | grep -ci 'HttpOnly')" "1"
assert "cookie SameSite=Strict" "$(echo "$SC" | grep -ci 'SameSite=Strict')" "1"
assert "cookie non lisible hors session (pas de jeton dans le corps)" "$(echo "$HDRS" | tail -1 | grep -c "$(echo "$SC" | sed 's/.*vm_session=\([^;]*\).*/\1/')")" "0"
HH="$(curl -sI "$BASE/api/v1/health")"
for h in x-frame-options x-content-type-options content-security-policy referrer-policy cache-control; do assert "en-tête $h présent" "$(echo "$HH" | grep -ci "^$h:")" "1"; done
call "$JAR_ADMIN" POST /api/v1/hypervisors/local/test '{}' -H "Origin: $BASE"; check "POST avec Origin identique accepté" 200
call "$JAR_ADMIN" POST /api/v1/hypervisors/local/test '{}' -H 'Origin: http://evil.example'; check "POST avec Origin étranger refusé" 403
call "$JAR_ADMIN" POST /api/v1/hypervisors/local/test '{}' -H 'Origin: null'; check "POST avec Origin « null » refusé" 403
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -b "$JAR_ADMIN" -H 'Content-Type: text/plain' -d '{}' "$BASE/api/v1/hypervisors/local/test")"; check "POST non-JSON refusé (formulaire tiers)" 415
CODE="$(curl -s -o /dev/null -w '%{http_code}' -X POST -b "$JAR_ADMIN" -H 'Content-Type: application/x-www-form-urlencoded' -d 'a=b' "$BASE/api/v1/hypervisors/local/test")"; check "POST formulaire classique refusé" 415
call "$JAR_ADMIN" GET /api/v1/route-inexistante; check "route inconnue → 404 JSON" 404
PORT="${BASE##*:}"
if command -v ss >/dev/null && [[ "$BASE" == http://127.0.0.1:* ]]; then
  assert "écoute uniquement sur 127.0.0.1" "$(ss -ltn 2>/dev/null | awk -v p=":$PORT" '$4 ~ p"$"{print $4}' | grep -vc '^127\.0\.0\.1:')" "0"
else skip "vérification d'écoute (ss absent ou hôte non local)"; fi
if [ -r "$DATA_DIR/users.json" ]; then
  for f in users.json audit.log hypervisors.json; do [ -e "$DATA_DIR/$f" ] && assert "permissions 600 : $f" "$(stat -c %a "$DATA_DIR/$f")" "600"; done
  assert "aucun mot de passe en clair dans users.json" "$(grep -c "$ADMIN_PASSWORD" "$DATA_DIR/users.json")" "0"
else skip "permissions des fichiers de données ($DATA_DIR illisible)"; fi

# ═══════════════════════════════════════════════════════════════════════════════════════════════
section "LOT B1 — Configuration d'une VM arrêtée"
A="full-a-$STAMP"
mkvm "$A" "" "$ISO"; check "création de la VM A (ISO insérée)" 201; UA="$VM_UUID"
call "$JAR_ADMIN" PUT "/api/v1/vms/$UA/hardware" '{"vcpus":0,"memory_mib":512}'; check "0 vCPU refusé" 400
call "$JAR_ADMIN" PUT "/api/v1/vms/$UA/hardware" '{"vcpus":2,"memory_mib":100}'; check "RAM < 256 Mio refusée" 400
call "$JAR_ADMIN" PUT "/api/v1/vms/$UA/hardware" '{"vcpus":2,"memory_mib":384}'; check "CPU/RAM : 2 vCPU, 384 Mio" 200
assert "libvirt : 2 vCPU" "$(vq dominfo "$A" | awk -F': +' '/^CPU\(s\)/{print $2}')" "2"
assert "libvirt : 384 Mio (393216 KiB)" "$(vq dominfo "$A" | awk -F': +' '/^Max memory/{print $2}')" "393216 KiB"
call "$JAR_ADMIN" POST "/api/v1/vms/$UA/disks" "{\"pool\":\"$POOL\",\"disk_gib\":1}"; check "ajout d'un disque (vdb)" 200
assert "libvirt : vdb attaché" "$(vq domblklist "$A" | awk '$1=="vdb"{print "oui"}')" "oui"
call "$JAR_ADMIN" POST "/api/v1/vms/$UA/disks" "{\"pool\":\"$POOL\",\"disk_gib\":1}"; check "ajout d'un second disque (vdc)" 200
call "$JAR_ADMIN" POST "/api/v1/vms/$UA/disks" "{\"pool\":\"inexistant\",\"disk_gib\":1}"; check "pool inexistant refusé" 400
call "$JAR_ADMIN" POST "/api/v1/vms/$UA/disks" "{\"pool\":\"$POOL\",\"disk_gib\":0}"; check "taille 0 refusée" 400
VDB="$(vq domblklist "$A" | awk '$1=="vdb"{print $2}')"
call "$JAR_ADMIN" POST "/api/v1/vms/$UA/disks/vdb/resize" '{"disk_gib":2}'; check "agrandissement de vdb à 2 Gio" 200
assert "libvirt : capacité 2 Gio" "$(vq vol-info "$VDB" | awk -F': +' '/^Capacity/{print $2}')" "2.00 GiB"
call "$JAR_ADMIN" POST "/api/v1/vms/$UA/disks/vdb/resize" '{"disk_gib":1}'; check "rétrécissement refusé" 409
call "$JAR_ADMIN" POST "/api/v1/vms/$UA/disks/vda/resize" '{"disk_gib":2}'; check "agrandissement du disque système" 200
call "$JAR_ADMIN" POST "/api/v1/vms/$UA/disks/sda/resize" '{"disk_gib":2}'; check "agrandir le CD-ROM refusé" 400

section "LOT B2 — ISO : éjection, insertion, refus"
call "$JAR_ADMIN" PUT "/api/v1/vms/$UA/media" '{"iso_pool":null,"iso_name":null}'; check "éjection" 200
assert "libvirt : lecteur vide" "$(vq domblklist "$A" | awk '$1=="sda"{print $2}')" "-"
call "$JAR_ADMIN" PUT "/api/v1/vms/$UA/media" "{\"iso_pool\":\"$POOL\",\"iso_name\":\"$ISO\"}"; check "insertion" 200
assert "libvirt : ISO insérée" "$(vq domblklist "$A" | awk '$1=="sda"{print $2}' | sed 's|.*/||')" "$ISO"
call "$JAR_ADMIN" PUT "/api/v1/vms/$UA/media" "{\"iso_pool\":\"$POOL\",\"iso_name\":\"inconnue.iso\"}"; check "ISO inconnue refusée" 400
call "$JAR_ADMIN" PUT "/api/v1/vms/$UA/media" "{\"iso_pool\":\"$POOL\",\"iso_name\":\"$(basename "$VDB")\"}"; check "volume non-ISO refusé" 400
call "$JAR_ADMIN" PUT "/api/v1/vms/$UA/media" "{\"iso_pool\":\"$POOL\",\"iso_name\":null}"; check "paramètres incohérents refusés" 400

section "LOT B3 — Clonage"
B="full-b-$STAMP"; NETARG=""; [ "$HAS_NET" = 1 ] && NETARG="$NET"
mkvm "$B" "$NETARG" "$ISO"; check "création de la VM B (source du clone)" 201; UB="$VM_UUID"
call "$JAR_ADMIN" POST "/api/v1/vms/$UB/clone" "{\"name\":\"full-c-$STAMP\"}"; check "clone avec ISO insérée refusé" 400
assert "message : retirer l'ISO" "$(echo "$BODY" | jf .error | grep -c 'ISO')" "1"
call "$JAR_ADMIN" PUT "/api/v1/vms/$UB/media" '{"iso_pool":null,"iso_name":null}'; check "éjection de l'ISO de B" 200
call "$JAR_ADMIN" POST "/api/v1/vms/$UB/clone" '{"name":"../mauvais"}'; check "nom de clone invalide refusé" 400
call "$JAR_ADMIN" POST "/api/v1/vms/$UB/clone" "{\"name\":\"$B\"}"; check "nom de clone déjà pris refusé" 409
CLN="full-c-$STAMP"; CLEAN_VMS+=("$CLN")
call "$JAR_ADMIN" POST "/api/v1/vms/$UB/clone" "{\"name\":\"$CLN\"}"; check "clone de B" 201; UC="$(echo "$BODY" | jf .uuid)"
assert "UUID du clone différent" "$([ "$UC" != "$UB" ] && echo oui)" "oui"
assert "libvirt : UUID du clone ≠ source" "$([ "$(vq domuuid "$CLN")" != "$(vq domuuid "$B")" ] && echo oui)" "oui"
if [ "$HAS_NET" = 1 ]; then
  MB="$(vq domiflist "$B" | awk 'NR>2{print $5}' | head -1)"; MC="$(vq domiflist "$CLN" | awk 'NR>2{print $5}' | head -1)"
  assert "libvirt : adresse MAC du clone régénérée" "$([ -n "$MB" ] && [ -n "$MC" ] && [ "$MB" != "$MC" ] && echo oui)" "oui"
fi
assert "libvirt : disque du clone distinct" "$([ "$(vq domblklist "$CLN" | awk '$1=="vda"{print $2}')" != "$(vq domblklist "$B" | awk '$1=="vda"{print $2}')" ] && echo oui)" "oui"
call "$JAR_ADMIN" POST "/api/v1/vms/$UA/clone" "{\"name\":\"full-d-$STAMP\"}"; check "clone d'une VM à plusieurs disques refusé" 400
assert "aucun volume orphelin après les refus" "$(vq vol-list "$POOL" | grep -c "full-d-")" "0"

section "LOT B4 — Détachement de disques"
call "$JAR_ADMIN" DELETE "/api/v1/vms/$UA/disks/vda" '{"delete_disk":true}'; check "détacher le disque système refusé" 403
call "$JAR_ADMIN" DELETE "/api/v1/vms/$UA/disks/vdb" '{"delete_disk":true}'; check "détacher + supprimer vdb" 200
assert "libvirt : volume vdb supprimé" "$(vq vol-list "$POOL" | grep -c "$(basename "$VDB")")" "0"
VDC="$(vq domblklist "$A" | awk '$1=="vdc"{print $2}')"
call "$JAR_ADMIN" DELETE "/api/v1/vms/$UA/disks/vdc" '{"delete_disk":false}'; check "détacher vdc en gardant le volume" 200
assert "libvirt : vdc détaché" "$(vq domblklist "$A" | awk '$1=="vdc"{print "oui"}')" ""
assert "libvirt : volume vdc conservé" "$(vq vol-list "$POOL" | grep -c "$(basename "$VDC")")" "1"
call "$JAR_ADMIN" DELETE "/api/v1/storage/pools/$POOL/volumes/$(basename "$VDC")" "{\"confirm_name\":\"$(basename "$VDC")\"}"; check "suppression du volume orphelin" 200

section "LOT B5 — VM en marche : restrictions, pause, console"
call "$JO" POST "/api/v1/vms/$UA/start" '{}'; check "démarrage par l'opérateur" 200
assert "libvirt : VM en marche" "$(wait_state "$A" running 10 && echo oui)" "oui"
call "$JO" POST "/api/v1/vms/$UA/start" '{}'; check "démarrer une VM déjà démarrée refusé" 409
call "$JAR_ADMIN" PUT "/api/v1/vms/$UA/hardware" '{"vcpus":1,"memory_mib":256}'; check "CPU/RAM à chaud refusé" 409
call "$JAR_ADMIN" POST "/api/v1/vms/$UA/disks" "{\"pool\":\"$POOL\",\"disk_gib\":1}"; check "ajout de disque à chaud refusé" 409
call "$JAR_ADMIN" PUT "/api/v1/vms/$UA/media" '{"iso_pool":null,"iso_name":null}'; check "éjection à chaud refusée" 409
call "$JAR_ADMIN" POST "/api/v1/vms/$UA/clone" "{\"name\":\"full-e-$STAMP\"}"; check "clone à chaud refusé" 409
call "$JAR_ADMIN" DELETE "/api/v1/vms/$UA" "{\"confirm_name\":\"$A\",\"delete_disk\":true}"; check "suppression à chaud refusée" 409
call "$JV" POST "/api/v1/vms/$UA/shutdown" '{}'; check "lecteur : arrêt refusé" 403
call "$JO" POST "/api/v1/vms/$UA/suspend" '{}'; check "pause" 200
assert "libvirt : VM en pause" "$(vq domstate "$A")" "paused"
call "$JAR_ADMIN" GET "/api/v1/vms/$UA/console"; check "console d'une VM en pause" 200
assert "console indisponible en pause" "$(echo "$BODY" | jf .available)" "False"
call "$JO" POST "/api/v1/vms/$UA/resume" '{}'; check "reprise" 200
assert "libvirt : VM reprise" "$(vq domstate "$A")" "running"
call "$JO" POST "/api/v1/vms/$UA/shutdown" '{}'; check "demande d'arrêt ACPI acceptée" 200
call "$JV" GET "/api/v1/vms/$UA/console"; check "lecteur : état de la console visible" 200
assert "console VNC disponible" "$(echo "$BODY" | jf .available)" "True"
SA="$(session_of "$JAR_ADMIN")"; SO="$(session_of "$JO")"; SV="$(session_of "$JV")"
assert "WS admin : bannière RFB" "$(ws_probe "$UA" "$BASE" "$SA")" "RFB:RFB 003.008"
assert "WS opérateur : bannière RFB" "$(ws_probe "$UA" "$BASE" "$SO")" "RFB:RFB 003.008"
assert "WS lecteur refusé" "$(ws_probe "$UA" "$BASE" "$SV")" "403"
assert "WS sans session refusé" "$(ws_probe "$UA" "$BASE" -)" "401"
assert "WS origine étrangère refusé" "$(ws_probe "$UA" http://evil.example "$SA")" "403"
assert "WS hyperviseur inconnu refusé" "$(ws_probe "$UA" "$BASE" "$SA" inconnu)" "404"
vq destroy "$A" >/dev/null 2>&1; assert "arrêt forcé (virsh) puis état « shutoff »" "$(wait_state "$A" "shut off" 10 && echo oui)" "oui"
sleep 1; call "$JAR_ADMIN" GET /api/v1/vms
assert "l'API reflète l'arrêt" "$(echo "$BODY" | python3 -c "import sys,json; print(next((v['state'] for v in json.load(sys.stdin) if v['name']=='$A'),''))")" "shutoff"

# ═══════════════════════════════════════════════════════════════════════════════════════════════
section "LOT C1 — Pools"
call "$JAR_ADMIN" DELETE "/api/v1/storage/pools/$FPOOL" "{\"confirm_name\":\"$FPOOL\"}" >/dev/null 2>&1
call "$JAR_ADMIN" POST /api/v1/storage/pools "{\"name\":\"$FPOOL\",\"path\":\"/etc/libvirt\"}"; check "chemin système refusé" 400
call "$JAR_ADMIN" POST /api/v1/storage/pools "{\"name\":\"$FPOOL\",\"path\":\"/var/lib/libvirt\"}"; check "racine elle-même refusée" 400
call "$JAR_ADMIN" POST /api/v1/storage/pools "{\"name\":\"-mauvais\",\"path\":\"$FPOOL_PATH\"}"; check "nom de pool invalide refusé" 400
call "$JAR_ADMIN" POST /api/v1/storage/pools "{\"name\":\"$FPOOL\",\"path\":\"$FPOOL_PATH\"}"; check "création du pool" 201
purge_pool "$FPOOL"   # résidus éventuels d'une exécution précédente interrompue
assert "libvirt : pool actif" "$(vq pool-info "$FPOOL" | awk -F': +' '/^State/{print $2}')" "running"
assert "libvirt : démarrage automatique" "$(vq pool-info "$FPOOL" | awk -F': +' '/^Autostart/{print $2}')" "yes"
call "$JAR_ADMIN" POST /api/v1/storage/pools "{\"name\":\"$FPOOL\",\"path\":\"$FPOOL_PATH-2\"}"; check "nom déjà pris refusé" 409
call "$JAR_ADMIN" POST /api/v1/storage/pools "{\"name\":\"full-autre\",\"path\":\"$FPOOL_PATH\"}"; check "répertoire déjà utilisé refusé" 409
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$FPOOL/refresh" '{}'; check "rafraîchissement" 200
call "$JAR_ADMIN" PUT "/api/v1/storage/pools/$FPOOL/autostart" '{"autostart":false}'; check "autostart désactivé" 200
assert "libvirt : autostart « no »" "$(vq pool-info "$FPOOL" | awk -F': +' '/^Autostart/{print $2}')" "no"
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$FPOOL/stop" '{}'; check "arrêt du pool" 200
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$FPOOL/stop" '{}'; check "second arrêt refusé" 409
call "$JAR_ADMIN" GET "/api/v1/storage/pools/$FPOOL/volumes"; check "lister les volumes d'un pool arrêté refusé" 409
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$FPOOL/volumes" '{"name":"x","size_gib":1,"format":"qcow2"}'; check "créer un volume dans un pool arrêté refusé" 409
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$FPOOL/start" '{}'; check "démarrage du pool" 200
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$FPOOL/start" '{}'; check "second démarrage refusé" 409
call "$JAR_ADMIN" GET /api/v1/storage/pools; check "liste des pools" 200
assert "le pool de test est listé actif" "$(echo "$BODY" | python3 -c "import sys,json; print(next((p['active'] for p in json.load(sys.stdin) if p['name']=='$FPOOL'),''))")" "True"
call "$JAR_ADMIN" POST "/api/v1/storage/pools/inconnu/start" '{}'; check "pool inconnu → 404" 404

section "LOT C2 — Volumes"
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$FPOOL/volumes" '{"name":"full-data","size_gib":1,"format":"qcow2"}'; check "volume qcow2 (extension ajoutée)" 201
assert "libvirt : full-data.qcow2 existe" "$(vq vol-list "$FPOOL" | grep -c 'full-data.qcow2')" "1"
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$FPOOL/volumes" '{"name":"full-raw","size_gib":1,"format":"raw"}'; check "volume raw (.img)" 201
assert "libvirt : full-raw.img existe" "$(vq vol-list "$FPOOL" | grep -c 'full-raw.img')" "1"
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$FPOOL/volumes" '{"name":"full-data","size_gib":1,"format":"qcow2"}'; check "doublon refusé" 409
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$FPOOL/volumes" '{"name":"a b","size_gib":1,"format":"qcow2"}'; check "nom avec espace refusé" 400
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$FPOOL/volumes" '{"name":"../x","size_gib":1,"format":"qcow2"}'; check "traversée de chemin refusée" 400
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$FPOOL/volumes" '{"name":"big","size_gib":3000,"format":"qcow2"}'; check "taille > 2048 Gio refusée" 400
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$FPOOL/volumes" '{"name":"f","size_gib":1,"format":"vmdk"}'; check "format inconnu refusé" 400
call "$JAR_ADMIN" DELETE "/api/v1/storage/pools/$FPOOL/volumes/full-data.qcow2" '{"confirm_name":"autre"}'; check "confirmation erronée refusée" 400
call "$JAR_ADMIN" DELETE "/api/v1/storage/pools/$FPOOL/volumes/full-data.qcow2" '{"confirm_name":"full-data.qcow2"}'; check "suppression d'un volume libre" 200
assert "libvirt : volume supprimé" "$(vq vol-list "$FPOOL" | grep -c 'full-data.qcow2')" "0"

section "LOT C3 — Téléversement d'ISO (limites et interruption)"
dd if=/dev/urandom of="$TMP/big.iso" bs=1M count=20 status=none
call "$JAR_ADMIN" PUT "/api/v1/storage/pools/$FPOOL/isos/full-20m.iso" "" --data-binary @"$TMP/big.iso" -H 'Content-Type: application/octet-stream'; check "ISO de 20 Mio" 201
assert "libvirt : taille exacte (20 Mio)" "$(vq vol-info --pool "$FPOOL" full-20m.iso | awk -F': +' '/^Capacity/{print $2}')" "20.00 MiB"
assert "contenu identique (SHA-256 relu depuis le pool)" "$([ "$(sha256sum "$TMP/big.iso" | cut -d' ' -f1)" = "$(vq vol-download --pool "$FPOOL" full-20m.iso "$TMP/back.iso" >/dev/null 2>&1; sha256sum "$TMP/back.iso" 2>/dev/null | cut -d' ' -f1)" ] && echo oui)" "oui"
call "$JAR_ADMIN" PUT "/api/v1/storage/pools/$FPOOL/isos/full-20m.iso" "" --data-binary @"$TMP/big.iso" -H 'Content-Type: application/octet-stream'; check "même nom : refusé" 409
call "$JAR_ADMIN" PUT "/api/v1/storage/pools/$FPOOL/isos/x.iso" "" -H 'Transfer-Encoding: chunked' --data-binary @"$TMP/small.iso"; check "sans Content-Length : refusé" 411
assert "Content-Length gigantesque : refusé" "$(raw_put_status "/api/v1/storage/pools/$FPOOL/isos/huge.iso" 999999999999 "$SA")" "413"
assert "aucun volume « huge » créé" "$(vq vol-list "$FPOOL" | grep -c huge)" "0"
call "$JAR_ADMIN" PUT "/api/v1/storage/pools/$FPOOL/isos/virtmgr-x.iso" "" --data-binary @"$TMP/small.iso"; check "préfixe réservé refusé" 400
dd if=/dev/urandom of="$TMP/huge.iso" bs=1M count=80 status=none
curl -s -m 3 --limit-rate 3M -X PUT -b "$JAR_ADMIN" -H 'Content-Type: application/octet-stream' --data-binary @"$TMP/huge.iso" "$BASE/api/v1/storage/pools/$FPOOL/isos/full-partial.iso" >/dev/null 2>&1
gone=0; for _ in $(seq 1 10); do [ "$(vq vol-list "$FPOOL" | grep -c full-partial)" = 0 ] && { gone=1; break; }; sleep 1; done
assert "envoi interrompu à mi-parcours : volume partiel supprimé" "$gone" "1"
assert "le serveur reste utilisable après l'interruption" "$(curl -s -o /dev/null -w '%{http_code}' -b "$JAR_ADMIN" "$BASE/api/v1/storage/pools")" "200"

section "LOT C4 — Pool utilisé par une VM"
CV="full-p-$STAMP"
mkvm "$CV" "" "" "$FPOOL"; check "VM dont le disque est dans le pool de test" 201; UCV="$VM_UUID"
call "$JAR_ADMIN" POST "/api/v1/vms/$UCV/start" '{}'; check "démarrage" 200
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$FPOOL/stop" '{}'; check "arrêt du pool refusé (VM en marche)" 409
call "$JAR_ADMIN" DELETE "/api/v1/storage/pools/$FPOOL" "{\"confirm_name\":\"$FPOOL\"}"; check "retrait du pool refusé (volume utilisé)" 409
vq destroy "$CV" >/dev/null 2>&1; wait_state "$CV" "shut off" 10
call "$JAR_ADMIN" GET "/api/v1/storage/pools/$FPOOL/volumes"
assert "le volume de la VM est marqué « utilisé par »" "$(echo "$BODY" | grep -c "\"in_use_by\":\[\"$CV\"\]")" "1"
call "$JAR_ADMIN" DELETE "/api/v1/storage/pools/$FPOOL/volumes/virtmgr-$UCV.qcow2" "{\"confirm_name\":\"virtmgr-$UCV.qcow2\"}"; check "suppression d'un volume utilisé refusée" 409
call "$JAR_ADMIN" DELETE "/api/v1/vms/$UCV" "{\"confirm_name\":\"$CV\",\"delete_disk\":true}"; check "suppression de la VM et de son disque" 200
call "$JAR_ADMIN" DELETE "/api/v1/storage/pools/$FPOOL" '{"confirm_name":"faux"}'; check "retrait : confirmation erronée refusée" 400
call "$JAR_ADMIN" DELETE "/api/v1/storage/pools/$FPOOL" "{\"confirm_name\":\"$FPOOL\"}"; check "retrait du pool" 200
assert "libvirt : pool retiré" "$(vq pool-list --all | grep -c "$FPOOL")" "0"
call "$JAR_ADMIN" POST /api/v1/storage/pools "{\"name\":\"$FPOOL\",\"path\":\"$FPOOL_PATH\"}"; check "recréation du pool sur le même répertoire" 201
call "$JAR_ADMIN" GET "/api/v1/storage/pools/$FPOOL/volumes"; check "volumes du pool recréé" 200
assert "les fichiers ont été CONSERVÉS au retrait du pool (full-20m.iso retrouvée)" "$(echo "$BODY" | grep -c 'full-20m.iso')" "1"
purge_pool "$FPOOL"
call "$JAR_ADMIN" DELETE "/api/v1/storage/pools/$FPOOL" "{\"confirm_name\":\"$FPOOL\"}"; check "retrait définitif du pool de test" 200

# ═══════════════════════════════════════════════════════════════════════════════════════════════
section "LOT D — Suppression de VM"
D1="full-d1-$STAMP"; mkvm "$D1" "" ""; check "VM D1" 201; UD1="$VM_UUID"
call "$JAR_ADMIN" DELETE "/api/v1/vms/$UD1" '{"confirm_name":"faux","delete_disk":false}'; check "confirmation erronée refusée" 400
call "$JAR_ADMIN" DELETE "/api/v1/vms/$UD1" "{\"confirm_name\":\"$D1\",\"delete_disk\":false}"; check "suppression SANS le disque" 200
assert "libvirt : VM disparue" "$(vq list --all --name | grep -cx "$D1")" "0"
assert "libvirt : le disque est conservé" "$(vq vol-list "$POOL" | grep -c "virtmgr-$UD1.qcow2")" "1"
call "$JAR_ADMIN" DELETE "/api/v1/storage/pools/$POOL/volumes/virtmgr-$UD1.qcow2" "{\"confirm_name\":\"virtmgr-$UD1.qcow2\"}"; check "nettoyage du disque orphelin par l'interface" 200
D2="full-d2-$STAMP"; mkvm "$D2" "" ""; check "VM D2" 201; UD2="$VM_UUID"
call "$JAR_ADMIN" DELETE "/api/v1/vms/$UD2" "{\"confirm_name\":\"$D2\",\"delete_disk\":true}"; check "suppression AVEC le disque" 200
assert "libvirt : disque supprimé" "$(vq vol-list "$POOL" | grep -c "virtmgr-$UD2.qcow2")" "0"
D3="full-d3-$STAMP"; mkvm "$D3" "" ""; UD3="$VM_UUID"
call "$JAR_ADMIN" POST "/api/v1/vms/$UD3/disks" "{\"pool\":\"$POOL\",\"disk_gib\":1}"; check "D3 : disque supplémentaire" 200
call "$JAR_ADMIN" DELETE "/api/v1/vms/$UD3" "{\"confirm_name\":\"$D3\",\"delete_disk\":true}"; check "suppression refusée tant qu'un disque supplémentaire est attaché" 409
call "$JAR_ADMIN" DELETE "/api/v1/vms/$UD3/disks/vdb" '{"delete_disk":true}'; check "détachement du disque supplémentaire" 200
call "$JAR_ADMIN" DELETE "/api/v1/vms/$UD3" "{\"confirm_name\":\"$D3\",\"delete_disk\":true}"; check "suppression ensuite acceptée" 200
EXT="full-ext-$STAMP"; CLEAN_VMS+=("$EXT")
vq define /dev/stdin >/dev/null 2>&1 <<EOF
<domain type='qemu'><name>$EXT</name><memory unit='MiB'>256</memory><vcpu>1</vcpu><os><type arch='x86_64'>hvm</type></os></domain>
EOF
call "$JAR_ADMIN" GET /api/v1/vms
assert "VM externe listée comme non gérée" "$(echo "$BODY" | python3 -c "import sys,json; print(next((v['managed'] for v in json.load(sys.stdin) if v['name']=='$EXT'),'absente'))")" "False"
UEXT="$(vq domuuid "$EXT")"
call "$JAR_ADMIN" DELETE "/api/v1/vms/$UEXT" "{\"confirm_name\":\"$EXT\",\"delete_disk\":false}"; check "VM externe non supprimable" 403
call "$JAR_ADMIN" POST "/api/v1/vms/$UEXT/disks" "{\"pool\":\"$POOL\",\"disk_gib\":1}"; check "VM externe : ajout de disque refusé" 403
call "$JAR_ADMIN" PUT "/api/v1/vms/$UEXT/hardware" '{"vcpus":1,"memory_mib":512}'; check "VM externe : CPU/RAM modifiable à froid" 200
assert "libvirt : VM externe passée à 512 Mio" "$(vq dominfo "$EXT" | awk -F': +' '/^Max memory/{print $2}')" "524288 KiB"
call "$JAR_ADMIN" GET "/api/v1/vms/$UEXT"; check "VM externe : fiche lisible" 200
assert "aucun volume virtmgr orphelin après le lot D" "$(orphans | grep -c "$UD1\|$UD2\|$UD3")" "0"

# ═══════════════════════════════════════════════════════════════════════════════════════════════
section "LOT E — Hyperviseur distant par SSH (sur cette machine)"
SSH_USER="$(id -un)"
if command -v ssh >/dev/null && ssh -o BatchMode=yes -o ConnectTimeout=5 "$SSH_USER@localhost" true >/dev/null 2>&1; then
  call "$JAR_ADMIN" POST /api/v1/hypervisors "{\"id\":\"full-ssh\",\"name\":\"Test SSH\",\"uri\":\"qemu+ssh://$SSH_USER@localhost/system\"}"; check "ajout de l'hôte SSH" 201
  call "$JV" GET /api/v1/hypervisors
  assert "lecteur : URI de l'hôte masquée" "$(echo "$BODY" | python3 -c "import sys,json; u=next((h['uri'] for h in json.load(sys.stdin) if h['id']=='full-ssh'),'absent'); print('' if u is None else u)")" ""
  call "$JAR_ADMIN" POST /api/v1/hypervisors/full-ssh/test '{}'; check "test de connexion" 200
  assert "connexion SSH réussie" "$(echo "$BODY" | jf .ok)" "True"
  assert "nombre de VM identique à virsh" "$(echo "$BODY" | jf .domains)" "$(vq list --all --name | grep -c .)"
  call "$JAR_ADMIN" GET /api/v1/vms "" -H 'X-Hypervisor: full-ssh'; check "inventaire via SSH" 200
  assert "même inventaire que le local" "$(echo "$BODY" | python3 -c "import sys,json; print(sorted(v['name'] for v in json.load(sys.stdin)))")" "$(curl -s -b "$JAR_ADMIN" "$BASE/api/v1/vms" | python3 -c "import sys,json; print(sorted(v['name'] for v in json.load(sys.stdin)))")"
  call "$JAR_ADMIN" GET /api/v1/storage/pools "" -H 'X-Hypervisor: full-ssh'; check "pools via SSH" 200
  S1="full-s-$STAMP"; CLEAN_VMS+=("$S1")
  call "$JAR_ADMIN" POST /api/v1/vms "{\"name\":\"$S1\",\"vcpus\":1,\"memory_mib\":256,\"disk_gib\":1,\"pool\":\"$POOL\",\"network\":null,\"iso_pool\":null,\"iso_name\":null}" -H 'X-Hypervisor: full-ssh'; check "création de VM via SSH" 201; US1="$(echo "$BODY" | jf .uuid)"
  assert "libvirt : la VM existe" "$(vq list --all --name | grep -cx "$S1")" "1"
  call "$JAR_ADMIN" POST "/api/v1/vms/$US1/start" '{}' -H 'X-Hypervisor: full-ssh'; check "démarrage via SSH" 200
  call "$JAR_ADMIN" GET "/api/v1/vms/$US1/console" "" -H 'X-Hypervisor: full-ssh'; check "état de la console via SSH" 200
  assert "console disponible via SSH" "$(echo "$BODY" | jf .available)" "True"
  assert "TUNNEL SSH : bannière RFB reçue" "$(ws_probe "$US1" "$BASE" "$SA" full-ssh)" "RFB:RFB 003.008"
  vq destroy "$S1" >/dev/null 2>&1; wait_state "$S1" "shut off" 10
  call "$JAR_ADMIN" DELETE "/api/v1/vms/$US1" "{\"confirm_name\":\"$S1\",\"delete_disk\":true}" -H 'X-Hypervisor: full-ssh'; check "suppression via SSH" 200
  call "$JAR_ADMIN" POST /api/v1/hypervisors "{\"id\":\"full-bad\",\"name\":\"Hôte mort\",\"uri\":\"qemu+ssh://nobody@127.0.0.1:1/system\"}"; check "ajout d'un hôte injoignable (validation seulement)" 201
  call "$JAR_ADMIN" POST /api/v1/hypervisors/full-bad/test '{}'; check "test d'un hôte injoignable" 200
  assert "échec signalé proprement" "$(echo "$BODY" | jf .ok)" "False"
  call "$JAR_ADMIN" GET /api/v1/vms "" -H 'X-Hypervisor: full-bad'; check "hôte injoignable → 502" 502
  call "$JAR_ADMIN" GET /api/v1/vms "" -H 'X-Hypervisor: full-bad'; check "2ᵉ essai immédiat → 502 (disjoncteur)" 502
  assert "message du disjoncteur" "$(echo "$BODY" | grep -c 'nouvel essai')" "1"
  call "$JAR_ADMIN" GET /api/v1/vms; check "l'hôte local n'est pas affecté" 200
  call "$JAR_ADMIN" DELETE /api/v1/hypervisors/full-bad; check "retrait de l'hôte mort" 200
  call "$JAR_ADMIN" DELETE /api/v1/hypervisors/full-ssh; check "retrait de l'hôte SSH" 200
else
  skip "SSH vers localhost non utilisable sans mot de passe — pour activer ce lot : sudo apt install openssh-server ; ssh-keygen -t ed25519 ; ssh-copy-id localhost ; ssh localhost true"
fi

# ═══════════════════════════════════════════════════════════════════════════════════════════════
section "LOT F — Modèles IaC : contenu et validité"
F="full-f-$STAMP"; mkvm "$F" "$NETARG" ""; check "VM modèle (avec disque supplémentaire à venir)" 201; UF="$VM_UUID"
call "$JAR_ADMIN" PUT "/api/v1/vms/$UF/hardware" '{"vcpus":3,"memory_mib":1536}'; check "3 vCPU / 1536 Mio" 200
call "$JAR_ADMIN" POST "/api/v1/vms/$UF/disks" "{\"pool\":\"$POOL\",\"disk_gib\":2}"; check "disque de 2 Gio" 200
for tool in terraform ansible packer; do
  call "$JAR_ADMIN" GET "/api/v1/vms/$UF/export/$tool"; check "export $tool" 200
  echo "$BODY" | jf .filename > "$TMP/$tool.name"; echo "$BODY" | jf .content > "$TMP/$tool.out"
done
assert "Terraform : 3 vCPU et 1536 Mio" "$(grep -c 'vcpu   = 3\|memory = 1536' "$TMP/terraform.out")" "2"
assert "Terraform : 2 volumes (1 Gio + 2 Gio)" "$(grep -c '^resource "libvirt_volume"' "$TMP/terraform.out")" "2"
assert "Ansible : 2 disques déclarés" "$(grep -c 'size_gib:' "$TMP/ansible.out")" "2"
assert "Packer : disque système 1 Gio" "$(grep -c 'disk_size      = "1G"' "$TMP/packer.out")" "1"
assert "aucun secret dans les exports (mot de passe admin)" "$(cat "$TMP"/*.out | grep -c "$ADMIN_PASSWORD")" "0"
assert "nom de fichier d'export sûr" "$(cat "$TMP"/*.name | grep -vc '^[A-Za-z0-9._-]*$')" "0"
mkdir -p "$TMP/iac"; cp "$TMP/terraform.out" "$TMP/iac/main.tf"; cp "$TMP/packer.out" "$TMP/iac/vm.pkr.hcl"; cp "$TMP/ansible.out" "$TMP/iac/play.yml"
mkdir -p "$TMP/static"
for id in terraform-vm ansible-playbook ansible-xml packer-debian packer-preseed; do
  call "$JAR_ADMIN" GET "/api/v1/templates/$id"; check "modèle statique $id" 200
  echo "$BODY" | jf .filename > "$TMP/static/$id.name"; echo "$BODY" | jf .content > "$TMP/static/$(cat "$TMP/static/$id.name")"
done
TF="$(command -v terraform || command -v tofu || true)"
if [ -n "$TF" ]; then
  for d in iac static; do mkdir -p "$TMP/tf-$d"; [ "$d" = iac ] && cp "$TMP/iac/main.tf" "$TMP/tf-$d/" || cp "$TMP/static/main.tf" "$TMP/tf-$d/"
    "$TF" fmt -check -diff "$TMP/tf-$d" >/dev/null 2>&1; rc=$?; [ $rc = 0 ] || [ $rc = 3 ] && assert "$(basename "$TF") fmt : syntaxe HCL valide ($d)" ok ok || assert "$(basename "$TF") fmt : syntaxe HCL valide ($d)" "code $rc" ok; done
else skip "Terraform / OpenTofu non installé (https://developer.hashicorp.com/terraform/install)"; fi
if command -v packer >/dev/null; then
  packer validate -syntax-only "$TMP/iac/vm.pkr.hcl" >/dev/null 2>&1 && assert "packer validate -syntax-only (export)" ok ok || assert "packer validate -syntax-only (export)" échec ok
  packer validate -syntax-only "$TMP/static/debian13.pkr.hcl" >/dev/null 2>&1 && assert "packer validate -syntax-only (modèle Debian)" ok ok || assert "packer validate -syntax-only (modèle Debian)" échec ok
else skip "Packer non installé"; fi
if python3 -c 'import yaml' 2>/dev/null; then
  python3 -c 'import yaml,sys; [yaml.safe_load(open(f)) for f in sys.argv[1:]]' "$TMP/iac/play.yml" "$TMP/static/create-vm.yml" 2>/dev/null && assert "YAML valide (export + modèle Ansible)" ok ok || assert "YAML valide (export + modèle Ansible)" invalide ok
else skip "python3-yaml absent"; fi
if python3 -c 'import yaml, jinja2' 2>/dev/null; then
  DT=qemu; [ -e /dev/kvm ] && DT=kvm
  python3 - "$TMP" "$STAMP" "$DT" <<'PY'
import sys, yaml, jinja2
tmp, stamp, dt = sys.argv[1:4]
play = yaml.safe_load(open(f"{tmp}/iac/play.yml"))[0]
v = dict(play["vars"]); v["vm_name"] = f"full-render-{stamp}"
open(f"{tmp}/export.xml", "w").write(jinja2.Environment().from_string(v["vm_xml"]).render(**v))
tpl = open(f"{tmp}/static/vm.xml.j2").read()
base = dict(vm_name=f"full-static-{stamp}", vm_memory_mib=64, vm_vcpus=1, vm_images_dir="/var/lib/libvirt/images", vm_network="default", vm_domain_type=dt)
open(f"{tmp}/static-noiso.xml", "w").write(jinja2.Environment().from_string(tpl).render(**base, vm_iso=""))
open(f"{tmp}/static-iso.xml", "w").write(jinja2.Environment().from_string(tpl).render(**base, vm_iso="/var/lib/libvirt/images/x.iso"))
PY
  for x in export static-noiso static-iso; do
    n="$(sed -n 's|.*<name>\(.*\)</name>.*|\1|p' "$TMP/$x.xml" | head -1)"; CLEAN_VMS+=("$n")
    out="$(vq define "$TMP/$x.xml" 2>&1)" && assert "libvirt accepte le XML généré ($x)" ok ok || assert "libvirt accepte le XML généré ($x) : ${out:0:120}" échec ok
    vq undefine "$n" >/dev/null 2>&1
  done
else skip "validation des XML générés par libvirt (sudo apt install python3-jinja2 python3-yaml)"; fi
if command -v ansible-playbook >/dev/null && ansible-galaxy collection list 2>/dev/null | grep -q community.libvirt; then
  printf '[hypervisors]\nlocalhost ansible_connection=local ansible_python_interpreter=/usr/bin/python3\n' > "$TMP/inv.ini"
  cp "$TMP/static/vm.xml.j2" "$TMP/iac/"
  ansible-playbook --syntax-check -i "$TMP/inv.ini" "$TMP/iac/play.yml" >/dev/null 2>&1 && assert "ansible --syntax-check (export)" ok ok || assert "ansible --syntax-check (export)" échec ok
  ansible-playbook --syntax-check -i "$TMP/inv.ini" "$TMP/static/create-vm.yml" >/dev/null 2>&1 && assert "ansible --syntax-check (modèle statique)" ok ok || assert "ansible --syntax-check (modèle statique)" échec ok
  if /usr/bin/python3 -c 'import libvirt, lxml' 2>/dev/null && { [ "$(id -u)" = 0 ] || sudo -n true 2>/dev/null; }; then
    AB=""; [ "$(id -u)" = 0 ] && AB="-e ansible_become=false"
    AN="full-ans-$STAMP"; CLEAN_VMS+=("$AN")
    run_play() { ansible-playbook -i "$TMP/inv.ini" "$1" -e "vm_name=$AN" -e vm_memory_mib=64 $AB 2>&1; }
    OUT="$(run_play "$TMP/iac/play.yml")"
    assert "ansible : playbook EXPORTÉ exécuté sans erreur" "$(echo "$OUT" | grep -o 'failed=[0-9]*' | head -1)" "failed=0"
    assert "ansible : la 1re exécution a bien créé des objets (changed > 0)" "$(echo "$OUT" | grep -c 'changed=[1-9]')" "1"
    assert "libvirt : VM créée par Ansible, en marche" "$(vq domstate "$AN" 2>/dev/null)" "running"
    assert "libvirt : 2 disques attachés (comme l'original)" "$(vq domblklist "$AN" 2>/dev/null | grep -c 'qcow2')" "2"
    OUT2="$(run_play "$TMP/iac/play.yml")"
    assert "ansible : 2ᵉ exécution idempotente (changed=0)" "$(echo "$OUT2" | grep -o 'changed=[0-9]*' | head -1)" "changed=0"
    vq destroy "$AN" >/dev/null 2>&1; vq pool-refresh "$POOL" >/dev/null 2>&1
    for v in $(vq vol-list "$POOL" | awk -v n="$AN" '$1 ~ n {print $2}'); do vq vol-delete "$v" >/dev/null 2>&1; done
  else skip "exécution réelle des playbooks (python3-libvirt + python3-lxml système, et root ou sudo sans mot de passe requis)"; fi
else skip "ansible + collection community.libvirt non installés (ansible-galaxy collection install community.libvirt)"; fi

# ═══════════════════════════════════════════════════════════════════════════════════════════════
section "Journal d'audit et nettoyage"
call "$JAR_ADMIN" GET "/api/v1/audit?limit=1000"; check "journal d'audit" 200
for needle in "POST /api/v1/vms" "PUT /api/v1/users" "PUT /api/v1/storage/pools"; do
  m="${needle%% *}"; p="${needle#* }"
  assert "audit : $needle enregistré" "$(echo "$BODY" | python3 -c "import sys,json; print(any(e['method']=='$m' and e['path'].startswith('$p') for e in json.load(sys.stdin)))")" "True"
done
assert "audit : tentatives de connexion refusées enregistrées" "$(echo "$BODY" | python3 -c "import sys,json; print(any(e['path']=='/api/v1/auth/login' and e['status'] in (401,429) for e in json.load(sys.stdin)))")" "True"
assert "audit : refus 403 enregistrés (tentatives des rôles insuffisants)" "$(echo "$BODY" | python3 -c "import sys,json; print(any(e['status']==403 and e['user']=='full-op' for e in json.load(sys.stdin)))")" "True"
assert "audit : aucun mot de passe" "$(echo "$BODY" | grep -c "$ADMIN_PASSWORD\|MotDePasse")" "0"
cleanup_early() { for n in "${CLEAN_VMS[@]}"; do vq destroy "$n" >/dev/null 2>&1; done; }
cleanup_early; cleanup
assert "plus aucune VM « full- » dans libvirt" "$(vq list --all --name | grep -c '^full-')" "0"
assert "plus de volume « full- » dans le pool" "$(vq vol-list "$POOL" | grep -c ' full-')" "0"
assert "aucun volume virtmgr orphelin créé par le test" "$(comm -13 <(echo "$ORPHANS_BEFORE") <(orphans) | grep -c .)" "0"
assert "pool de test retiré" "$(vq pool-list --all | grep -c "$FPOOL")" "0"
summary
