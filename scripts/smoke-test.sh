#!/usr/bin/env bash
# Test de bout en bout de Virt Manager contre une instance EN COURS D'EXÉCUTION.
#
#   ./scripts/smoke-test.sh                      # lit le mot de passe initial dans data/
#   ADMIN_PASSWORD='…' ./scripts/smoke-test.sh   # ou le fournir explicitement
#
# Il crée puis supprime ses propres objets (préfixe « smoke- ») : VM, volumes, ISO, comptes.
# Il ne touche à aucune autre VM. Dépendances : bash, curl, python3 (et virsh, facultatif,
# pour forcer l'arrêt de la VM de test à la fin).
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
STAMP="$(date +%s)"
VM_NAME="smoke-vm-$STAMP"
ISO_NAME="smoke-$STAMP.iso"
VOL_NAME="smoke-vol-$STAMP"
TMP="$(mktemp -d)"
JAR_ADMIN="$TMP/admin.jar"; JAR_VIEW="$TMP/view.jar"
trap 'rm -rf "$TMP"' EXIT

# shellcheck source=scripts/testlib.sh
source "$(dirname "${BASH_SOURCE[0]}")/testlib.sh"
section "Disponibilité et protection par authentification"
call "$JAR_ADMIN" GET /api/v1/health; check "health public" 200
call "$JAR_ADMIN" GET /api/v1/vms; check "liste des VM sans session refusée" 401
call "$JAR_ADMIN" POST /api/v1/vms/18a09112-5827-45e0-ac41-89f8b3f46635/start '{}'; check "démarrage sans session refusé" 401
call "$JAR_ADMIN" GET /api-docs/openapi.json; check "OpenAPI sans session refusée" 401
call "$JAR_ADMIN" POST /api/v1/auth/login '{"username":"admin","password":"mauvais-mot-de-passe"}'; check "mauvais mot de passe refusé" 401
call "$JAR_ADMIN" POST /api/v1/auth/login "{\"username\":\"$ADMIN_USER\",\"password\":\"$ADMIN_PASSWORD\"}"; check "connexion admin" 200
assert "rôle renvoyé" "$(echo "$BODY" | jf .role)" "admin"
call "$JAR_ADMIN" GET /api/v1/auth/me; check "session valide" 200
grep -q HttpOnly "$JAR_ADMIN" 2>/dev/null && assert "cookie de session HttpOnly" "oui" "oui" || assert "cookie de session HttpOnly" "non" "oui"

section "Protection CSRF (en-tête Origin)"
call "$JAR_ADMIN" POST /api/v1/auth/logout '{}' -H 'Origin: http://evil.example'; check "POST depuis une origine étrangère refusé" 403
call "$JAR_ADMIN" GET /api/v1/auth/me; check "la session survit à la tentative CSRF" 200

section "Lecture : hyperviseurs, ressources, stockage"
call "$JAR_ADMIN" GET /api/v1/hypervisors; check "liste des hyperviseurs" 200
assert "hyperviseur local présent" "$(echo "$BODY" | jf .0.id)" "local"
call "$JAR_ADMIN" GET /api/v1/hypervisors/local/info; check "infos hyperviseur local" 200
call "$JAR_ADMIN" GET /api/v1/vms; check "liste des VM" 200
call "$JAR_ADMIN" GET /api/v1/resources; check "ressources (pools/réseaux/ISO)" 200
call "$JAR_ADMIN" GET /api/v1/storage/pools; check "liste détaillée des pools" 200
call "$JAR_ADMIN" GET "/api/v1/storage/pools/$POOL/volumes"; check "volumes du pool $POOL" 200
call "$JAR_ADMIN" GET /api-docs/openapi.json; check "spécification OpenAPI" 200
call "$JAR_ADMIN" GET /api/v1/templates; check "catalogue de modèles IaC" 200
call "$JAR_ADMIN" GET /api/v1/templates/terraform-vm; check "modèle Terraform statique" 200

section "Permissions par rôle"
call "$JAR_ADMIN" POST /api/v1/users "{\"username\":\"smoke-viewer\",\"password\":\"viewer-password-123\",\"role\":\"viewer\"}"; check "création d'un compte lecteur" 201
call "$JAR_ADMIN" POST /api/v1/users "{\"username\":\"smoke-viewer\",\"password\":\"viewer-password-123\",\"role\":\"viewer\"}"; check "doublon refusé" 409
call "$JAR_ADMIN" POST /api/v1/users '{"username":"smoke-weak","password":"court","role":"viewer"}'; check "mot de passe trop court refusé" 400
call "$JAR_VIEW" POST /api/v1/auth/login '{"username":"smoke-viewer","password":"viewer-password-123"}'; check "connexion lecteur" 200
call "$JAR_VIEW" GET /api/v1/vms; check "lecteur : lecture autorisée" 200
call "$JAR_VIEW" GET /api/v1/hypervisors; check "lecteur : liste des hyperviseurs" 200
assert "lecteur : URI masquée" "$(echo "$BODY" | jf .0.uri)" ""
call "$JAR_VIEW" POST /api/v1/vms "{\"name\":\"smoke-forbidden\",\"vcpus\":1,\"memory_mib\":512,\"disk_gib\":1,\"pool\":\"$POOL\",\"network\":null,\"iso_pool\":null,\"iso_name\":null}"; check "lecteur : création refusée" 403
call "$JAR_VIEW" POST /api/v1/vms/18a09112-5827-45e0-ac41-89f8b3f46635/start '{}'; check "lecteur : démarrage refusé" 403
call "$JAR_VIEW" GET /api/v1/users; check "lecteur : liste des comptes refusée" 403
call "$JAR_VIEW" GET /api/v1/audit; check "lecteur : audit refusé" 403
call "$JAR_ADMIN" PUT /api/v1/users/smoke-viewer '{"role":"operator"}'; check "promotion en opérateur" 200
call "$JAR_VIEW" GET /api/v1/vms; check "promotion : sessions fermées par sécurité" 401
call "$JAR_VIEW" POST /api/v1/auth/login '{"username":"smoke-viewer","password":"viewer-password-123"}'; check "reconnexion opérateur" 200
call "$JAR_VIEW" POST /api/v1/vms/18a09112-5827-45e0-ac41-89f8b3f46635/start '{}'; check "opérateur : démarrage autorisé (VM inconnue → 404)" 404
call "$JAR_VIEW" DELETE /api/v1/vms/18a09112-5827-45e0-ac41-89f8b3f46635 '{"confirm_name":"x","delete_disk":false}'; check "opérateur : suppression refusée" 403
call "$JAR_ADMIN" PUT /api/v1/users/admin '{"role":"viewer"}'; check "pas d'auto-rétrogradation" 409
call "$JAR_ADMIN" DELETE /api/v1/users/admin; check "pas d'auto-suppression" 409

section "Stockage : volume vierge, ISO téléversée"
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$POOL/volumes" "{\"name\":\"$VOL_NAME\",\"size_gib\":1,\"format\":\"qcow2\"}"; check "création d'un volume qcow2" 201
call "$JAR_ADMIN" POST "/api/v1/storage/pools/$POOL/volumes" "{\"name\":\"virtmgr-evil\",\"size_gib\":1,\"format\":\"qcow2\"}"; check "préfixe réservé refusé" 400
call "$JAR_ADMIN" POST /api/v1/storage/pools '{"name":"smoke-evil","path":"/etc/libvirt"}'; check "pool hors racines autorisées refusé" 400
call "$JAR_ADMIN" POST /api/v1/storage/pools '{"name":"smoke-evil","path":"/srv/../etc"}'; check "traversée de chemin refusée" 400
dd if=/dev/urandom of="$TMP/test.iso" bs=1M count=3 status=none
call "$JAR_ADMIN" PUT "/api/v1/storage/pools/$POOL/isos/$ISO_NAME" "" --data-binary @"$TMP/test.iso" -H 'Content-Type: application/octet-stream'; check "téléversement d'une ISO de 3 Mio" 201
call "$JAR_ADMIN" PUT "/api/v1/storage/pools/$POOL/isos/$ISO_NAME" "" --data-binary @"$TMP/test.iso" -H 'Content-Type: application/octet-stream'; check "ISO existante : écrasement refusé" 409
call "$JAR_ADMIN" PUT "/api/v1/storage/pools/$POOL/isos/mauvais.txt" "" --data-binary @"$TMP/test.iso"; check "extension non .iso refusée" 400
call "$JAR_ADMIN" GET "/api/v1/storage/pools/$POOL/volumes"; check "relister les volumes" 200
assert "taille de l'ISO conservée (octets)" "$(echo "$BODY" | python3 -c "import sys,json; print(next((v['capacity_bytes'] for v in json.load(sys.stdin) if v['name']=='$ISO_NAME'), ''))")" "3145728"

section "Machine virtuelle : création, configuration, export"
call "$JAR_ADMIN" POST /api/v1/vms "{\"name\":\"$VM_NAME\",\"vcpus\":1,\"memory_mib\":256,\"disk_gib\":1,\"pool\":\"$POOL\",\"network\":null,\"iso_pool\":\"$POOL\",\"iso_name\":\"$ISO_NAME\"}"; check "création d'une VM avec ISO" 201
VM_UUID="$(echo "$BODY" | jf .uuid)"
call "$JAR_ADMIN" GET "/api/v1/vms/$VM_UUID"; check "fiche de la VM" 200
call "$JAR_ADMIN" DELETE "/api/v1/storage/pools/$POOL/volumes/$ISO_NAME" "{\"confirm_name\":\"$ISO_NAME\"}"; check "ISO utilisée : suppression refusée" 409
call "$JAR_ADMIN" PUT "/api/v1/vms/$VM_UUID/hardware" '{"vcpus":2,"memory_mib":384}'; check "modification CPU/RAM à froid" 200
call "$JAR_ADMIN" POST "/api/v1/vms/$VM_UUID/disks" "{\"pool\":\"$POOL\",\"disk_gib\":1}"; check "ajout d'un disque" 200
call "$JAR_ADMIN" POST "/api/v1/vms/$VM_UUID/disks/vdb/resize" '{"disk_gib":2}'; check "agrandissement du disque" 200
call "$JAR_ADMIN" PUT "/api/v1/vms/$VM_UUID/media" '{"iso_pool":null,"iso_name":null}'; check "éjection de l'ISO" 200
call "$JAR_ADMIN" PUT "/api/v1/vms/$VM_UUID/media" "{\"iso_pool\":\"$POOL\",\"iso_name\":\"$ISO_NAME\"}"; check "réinsertion de l'ISO" 200
for tool in terraform ansible packer; do
  call "$JAR_ADMIN" GET "/api/v1/vms/$VM_UUID/export/$tool"; check "export $tool" 200
  case "$tool" in
    terraform) echo "$BODY" | jf .content > "$TMP/export.tf" ;;
    ansible)   echo "$BODY" | jf .content > "$TMP/export.yml" ;;
    packer)    echo "$BODY" | jf .content > "$TMP/export.pkr.hcl" ;;
  esac
done
call "$JAR_ADMIN" GET "/api/v1/vms/$VM_UUID/export/pulumi"; check "outil d'export inconnu refusé" 400
if python3 -c 'import yaml' 2>/dev/null; then
  python3 -c 'import yaml,sys; yaml.safe_load(open(sys.argv[1]))' "$TMP/export.yml" && assert "export Ansible = YAML valide" ok ok || assert "export Ansible = YAML valide" invalide ok
fi

section "Cycle de vie et console VNC"
call "$JAR_ADMIN" POST "/api/v1/vms/$VM_UUID/start" '{}'; check "démarrage" 200
sleep 2
call "$JAR_ADMIN" GET "/api/v1/vms/$VM_UUID/console"; check "état de la console" 200
assert "console VNC disponible" "$(echo "$BODY" | jf .available)" "True"
call "$JAR_ADMIN" PUT "/api/v1/vms/$VM_UUID/hardware" '{"vcpus":1,"memory_mib":256}'; check "modification à chaud refusée" 409
call "$JAR_ADMIN" DELETE "/api/v1/vms/$VM_UUID" "{\"confirm_name\":\"$VM_NAME\",\"delete_disk\":true}"; check "suppression d'une VM en marche refusée" 409
# Poignée de main WebSocket + bannière RFB, avec le cookie de session.
SESSION="$(awk '$6=="vm_session"{print $7}' "$JAR_ADMIN")"
assert "WebSocket sans cookie refusé" "$(ws_probe "$VM_UUID" "$BASE" -)" "401"
assert "WebSocket depuis une origine étrangère refusé" "$(ws_probe "$VM_UUID" http://evil.example "$SESSION")" "403"
assert "WebSocket sans Origin refusé" "$(ws_probe "$VM_UUID" - "$SESSION")" "403"
assert "WebSocket authentifié : bannière RFB reçue" "$(ws_probe "$VM_UUID" "$BASE" "$SESSION")" "RFB:RFB 003.008"

section "Hyperviseurs distants (validation, sans connexion réelle)"
call "$JAR_ADMIN" POST /api/v1/hypervisors '{"name":"x","uri":"qemu+ssh://h/system?command=/bin/sh"}'; check "paramètre d'URI dangereux refusé" 400
call "$JAR_ADMIN" POST /api/v1/hypervisors '{"name":"x","uri":"qemu+tcp://h/system"}'; check "schéma non chiffré refusé" 400
call "$JAR_ADMIN" POST /api/v1/hypervisors '{"id":"smoke-lab","name":"Labo smoke","uri":"qemu+ssh://nobody@192.0.2.1/system"}'; check "ajout d'un hyperviseur valide" 201
call "$JAR_ADMIN" GET /api/v1/vms "" -H 'X-Hypervisor: inconnu'; check "hyperviseur inconnu → 404" 404
call "$JAR_ADMIN" DELETE /api/v1/hypervisors/local; check "l'hyperviseur local n'est pas supprimable" 409
call "$JAR_ADMIN" DELETE /api/v1/hypervisors/smoke-lab; check "retrait de l'hyperviseur de test" 200

section "Nettoyage"
command -v virsh >/dev/null && virsh -c "$VIRSH_URI" destroy "$VM_NAME" >/dev/null 2>&1
sleep 1
call "$JAR_ADMIN" DELETE "/api/v1/vms/$VM_UUID/disks/vdb" '{"delete_disk":true}'; check "détachement + suppression du disque ajouté" 200
call "$JAR_ADMIN" DELETE "/api/v1/vms/$VM_UUID" "{\"confirm_name\":\"$VM_NAME\",\"delete_disk\":true}"; check "suppression de la VM et de son disque" 200
call "$JAR_ADMIN" DELETE "/api/v1/storage/pools/$POOL/volumes/$ISO_NAME" "{\"confirm_name\":\"$ISO_NAME\"}"; check "suppression de l'ISO de test" 200
call "$JAR_ADMIN" DELETE "/api/v1/storage/pools/$POOL/volumes/$VOL_NAME.qcow2" "{\"confirm_name\":\"$VOL_NAME.qcow2\"}"; check "suppression du volume de test" 200
call "$JAR_ADMIN" DELETE /api/v1/users/smoke-viewer; check "suppression du compte de test" 200

section "Audit et anti-force-brute"
call "$JAR_ADMIN" GET "/api/v1/audit?limit=500"; check "journal d'audit (admin)" 200
assert "l'audit contient la création de la VM" "$(echo "$BODY" | python3 -c "import sys,json; print(any(e['method']=='POST' and e['path']=='/api/v1/vms' and e['status']==201 for e in json.load(sys.stdin)))")" "True"
assert "l'audit ne contient aucun mot de passe" "$(echo "$BODY" | grep -c "$ADMIN_PASSWORD")" "0"
for _ in 1 2 3 4 5; do call "$TMP/brute.jar" POST /api/v1/auth/login '{"username":"smoke-nobody","password":"faux-faux-faux"}'; done
call "$TMP/brute.jar" POST /api/v1/auth/login '{"username":"smoke-nobody","password":"faux-faux-faux"}'; check "6ᵉ tentative échouée : verrouillage" 429
call "$JAR_ADMIN" POST /api/v1/auth/logout '{}'; check "déconnexion" 200
call "$JAR_ADMIN" GET /api/v1/auth/me; check "session fermée côté serveur" 401

summary
