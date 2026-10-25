#!/usr/bin/env bash
# Virt Manager — installation, compilation et lancement en UNE commande (Debian 13 / Ubuntu).
#
#   ./run.sh             installe ce qui manque, compile, démarre sur http://127.0.0.1:8080
#   ./run.sh --yes       idem sans poser de question (installation automatique)
#   ./run.sh test        test de bout en bout sur une instance temporaire (ne touche pas à vos comptes)
#   ./run.sh test-full   test approfondi (rôles, VM, disques, stockage, SSH, modèles IaC : ≈ 5 min)
#   ./run.sh iso         télécharge une petite ISO de test (Alpine) dans le pool « default »
#   ./run.sh dev         mode développement (rechargement à chaud du front sur :5173)
#   ./run.sh check       vérifie l'environnement sans rien installer ni lancer
#   ./run.sh --rebuild   force la recompilation du front et du back
#
# Variables utiles : API_BIND (défaut 127.0.0.1:8080), LIBVIRT_URI (défaut qemu:///system),
# VIRTMGR_DATA_DIR (défaut ./data), VIRTMGR_ADMIN_PASSWORD (mot de passe admin initial).
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
cd "$ROOT"
ORIG_ARGS=("$@")   # conservés tels quels pour une relance éventuelle via « sg libvirt »
MODE="start"; ASSUME_YES=0; REBUILD=0
for arg in "$@"; do
  case "$arg" in
    start|dev|test|iso|check) MODE="$arg" ;;
    test-full|full) MODE="test-full" ;;
    -y|--yes) ASSUME_YES=1 ;;
    --rebuild) REBUILD=1 ;;
    -h|--help|help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "Argument inconnu : $arg (voir ./run.sh --help)"; exit 2 ;;
  esac
done

export LIBVIRT_URI="${LIBVIRT_URI:-qemu:///system}"
export VIRTMGR_DATA_DIR="${VIRTMGR_DATA_DIR:-$ROOT/data}"
BIND="${API_BIND:-127.0.0.1:8080}"
MIN_RUST="1.89"; MIN_NODE_MAJOR=20; MIN_NODE_MINOR=19
BIN="$ROOT/backend/target/release/virt-manager-api"

if [ -t 1 ]; then B=$'\033[1m'; G=$'\033[32m'; Y=$'\033[33m'; R=$'\033[31m'; N=$'\033[0m'; else B=""; G=""; Y=""; R=""; N=""; fi
step() { printf '\n%s▶ %s%s\n' "$B" "$*" "$N"; }
ok()   { printf ' %s✔%s %s\n' "$G" "$N" "$*"; }
warn() { printf ' %s!%s %s\n' "$Y" "$N" "$*"; }
die()  { printf '\n%s✘ %s%s\n' "$R" "$*" "$N" >&2; exit 1; }

confirm() { # confirm "question"
  [ "$ASSUME_YES" = 1 ] && return 0
  [ -t 0 ] || die "$1 — relancez avec --yes pour accepter automatiquement."
  local a; read -r -p " ? $1 [O/n] " a; [[ -z "$a" || "$a" =~ ^[OoYy] ]]
}
version_ge() { [ "$(printf '%s\n%s\n' "$2" "$1" | sort -V | head -n1)" = "$2" ]; } # version_ge actuelle minimale

# ── Garde-fous ──────────────────────────────────────────────────────────────────────────────
if [ "$(id -u)" = 0 ] && [ "${VIRTMGR_ALLOW_ROOT:-0}" != 1 ]; then
  die "Ne lancez pas ce script en root ni avec sudo : il demandera sudo uniquement quand nécessaire, et l'application doit tourner sous votre compte (groupe libvirt)."
fi
SUDO=""; [ "$(id -u)" = 0 ] || SUDO="sudo"
export PATH="$HOME/.cargo/bin:$PATH"

# ── 1. Paquets système ──────────────────────────────────────────────────────────────────────
ensure_packages() {
  step "Paquets système"
  command -v apt-get >/dev/null || { warn "apt-get introuvable : vérifiez à la main libvirt-dev, libclang-dev, pkg-config, un compilateur C, nodejs ≥ 20.19, npm, libvirt et qemu."; return 0; }
  local want=(build-essential pkg-config libvirt-dev libclang-dev libvirt-clients libvirt-daemon-system
              qemu-system-x86 qemu-utils dnsmasq-base curl ca-certificates python3 openssh-client)
  local missing=() p
  for p in "${want[@]}"; do dpkg -s "$p" >/dev/null 2>&1 || missing+=("$p"); done
  # Node/npm : on teste les commandes (une installation nvm/NodeSource ne passe pas par dpkg).
  command -v node >/dev/null && command -v npm >/dev/null || missing+=(nodejs npm)
  if [ "${#missing[@]}" -eq 0 ]; then ok "tous les paquets requis sont présents"; return 0; fi
  [ "$MODE" = check ] && { warn "paquets manquants : ${missing[*]}"; return 0; }
  echo " Paquets à installer : ${missing[*]}"
  confirm "Installer ces paquets avec apt (sudo) ?" || die "Installation refusée."
  [ -n "$SUDO" ] && ! command -v sudo >/dev/null && die "sudo est requis pour installer les paquets (ou lancez en root : VIRTMGR_ALLOW_ROOT=1)."
  $SUDO apt-get update
  $SUDO env DEBIAN_FRONTEND=noninteractive apt-get install -y "${missing[@]}"
  ok "paquets installés"
}

# ── 2. libvirt : démon, groupe, pool et réseau par défaut ───────────────────────────────────
virsh_s() { LC_ALL=C virsh -c "$LIBVIRT_URI" "$@"; }
virsh_has() { local out; out="$(virsh_s "${@:2}" 2>/dev/null || true)"; grep -Eq "$1" <<<"$out"; } # virsh_has REGEX args…
ensure_libvirt() {
  step "libvirt ($LIBVIRT_URI)"
  command -v virsh >/dev/null || { [ "$MODE" = check ] && { warn "virsh absent"; return 0; } || die "virsh introuvable."; }
  if [ "$LIBVIRT_URI" != "qemu:///system" ]; then ok "URI personnalisée : étapes système ignorées"; return 0; fi

  if [ "$(id -u)" != 0 ] && [[ " $(id -nG) " != *" libvirt "* ]]; then
    if getent group libvirt | grep -qE "[:,]$USER(,|$)"; then
      [ -n "${VIRTMGR_REEXEC:-}" ] && die "Groupe libvirt non pris en compte. Ouvrez une nouvelle session (déconnexion/reconnexion) puis relancez ./run.sh."
      warn "Vous êtes dans le groupe libvirt mais cette session ne le sait pas encore : relance via « sg libvirt »."
    else
      [ "$MODE" = check ] && { warn "l'utilisateur $USER n'est pas dans le groupe libvirt"; return 0; }
      confirm "Ajouter $USER au groupe libvirt (sudo adduser) ?" || die "Le groupe libvirt est nécessaire pour piloter qemu:///system sans root."
      $SUDO adduser "$USER" libvirt >/dev/null 2>&1 || $SUDO usermod -aG libvirt "$USER"
      ok "$USER ajouté au groupe libvirt"
    fi
    [ "$MODE" = check ] && return 0
    command -v sg >/dev/null || die "Reconnectez-vous pour activer le groupe libvirt, puis relancez ./run.sh."
    export VIRTMGR_REEXEC=1
    exec sg libvirt -c "$(printf '%q ' bash "$ROOT/run.sh" "${ORIG_ARGS[@]}")"
  fi
  ok "droits libvirt OK"

  if ! virsh_s uri >/dev/null 2>&1; then
    [ "$MODE" = check ] && { warn "démon libvirt injoignable"; return 0; }
    warn "démon libvirt injoignable : démarrage"
    $SUDO systemctl enable --now libvirtd >/dev/null 2>&1 || $SUDO systemctl enable --now libvirtd.socket >/dev/null 2>&1 || true
    for _ in $(seq 1 15); do virsh_s uri >/dev/null 2>&1 && break; sleep 1; done
    virsh_s uri >/dev/null 2>&1 || die "Impossible de joindre libvirt. Essayez : sudo systemctl enable --now libvirtd"
  fi
  ok "démon libvirt joignable"

  [ "$MODE" = check ] && { virsh_s pool-info default >/dev/null 2>&1 && ok "pool « default » présent" || warn "pool « default » absent"; return 0; }
  if ! virsh_s pool-info default >/dev/null 2>&1; then
    warn "pool « default » absent : création (/var/lib/libvirt/images)"
    virsh_s pool-define-as default dir --target /var/lib/libvirt/images >/dev/null
    virsh_s pool-build default >/dev/null 2>&1 || true
  fi
  virsh_has '^State: +running' pool-info default || virsh_s pool-start default >/dev/null 2>&1 || true
  virsh_has '^State: +running' pool-info default || warn "pool « default » non démarrable"
  virsh_s pool-autostart default >/dev/null 2>&1 || true
  virsh_has '^State: +running' pool-info default && ok "pool « default » actif"

  if ! virsh_s net-info default >/dev/null 2>&1; then
    local xml=/usr/share/libvirt/networks/default.xml
    if [ -f "$xml" ]; then virsh_s net-define "$xml" >/dev/null && warn "réseau « default » créé"; fi
  fi
  if virsh_s net-info default >/dev/null 2>&1; then
    virsh_has '^Active: +yes' net-info default || virsh_s net-start default >/dev/null 2>&1 || true
    virsh_has '^Active: +yes' net-info default || warn "réseau « default » non démarrable (les VM peuvent être créées sans réseau)"
    virsh_s net-autostart default >/dev/null 2>&1 || true
    virsh_has '^Active: +yes' net-info default && ok "réseau « default » prêt"
  else
    warn "pas de réseau « default » : les VM seront créées sans carte réseau"
  fi
  if [ ! -e /dev/kvm ]; then warn "/dev/kvm absent : les VM tourneront en émulation logicielle (lent, mais fonctionnel). Activez la virtualisation dans le BIOS si c'est une machine physique."
  elif [ ! -r /dev/kvm ] || [ ! -w /dev/kvm ]; then warn "/dev/kvm non accessible pour $USER (libvirt s'en charge pour les VM : sans conséquence)"; else ok "accélération KVM disponible"; fi
}

# ── 3. Rust et Node ─────────────────────────────────────────────────────────────────────────
ensure_toolchains() {
  step "Chaînes de compilation"
  local rv=""; command -v rustc >/dev/null && rv="$(rustc --version | awk '{print $2}')"
  if [ -z "$rv" ] || ! version_ge "$rv" "$MIN_RUST"; then
    [ "$MODE" = check ] && { warn "Rust ≥ $MIN_RUST requis (trouvé : ${rv:-aucun})"; } || {
      echo " Rust ≥ $MIN_RUST requis (trouvé : ${rv:-aucun}). Le Rust des dépôts Debian 13 est trop ancien pour le Cargo.lock du projet."
      confirm "Installer Rust via rustup (https://sh.rustup.rs, dans ~/.cargo, sans sudo) ?" || die "Rust requis."
      if command -v rustup >/dev/null; then rustup update stable --no-self-update; else
        curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal; fi
      export PATH="$HOME/.cargo/bin:$PATH"; rv="$(rustc --version | awk '{print $2}')"
    }
  fi
  [ -n "$rv" ] && version_ge "$rv" "$MIN_RUST" && ok "Rust $rv"
  if command -v node >/dev/null; then
    local nv; nv="$(node --version | tr -d v)"; local maj="${nv%%.*}" min; min="$(echo "$nv" | cut -d. -f2)"
    if [ "$maj" -gt "$MIN_NODE_MAJOR" ] || { [ "$maj" -eq "$MIN_NODE_MAJOR" ] && [ "$min" -ge "$MIN_NODE_MINOR" ]; }; then ok "Node $nv"
    else [ "$MODE" = check ] && warn "Node ≥ $MIN_NODE_MAJOR.$MIN_NODE_MINOR requis (trouvé $nv)" || die "Node ≥ $MIN_NODE_MAJOR.$MIN_NODE_MINOR requis (trouvé $nv). Installez une version récente (nvm, NodeSource ou Debian 13)."; fi
  else [ "$MODE" = check ] && warn "node absent" || die "node introuvable."; fi
}

# ── 4. Compilation ──────────────────────────────────────────────────────────────────────────
newer_than() { [ ! -e "$1" ] || [ -n "$(find "${@:2}" -type f -newer "$1" -print -quit 2>/dev/null)" ]; }
build_frontend() {
  step "Interface web (React / Vite)"
  cd "$ROOT/frontend"
  if [ ! -d node_modules ] || newer_than node_modules/.package-lock.json package-lock.json package.json; then
    echo " npm ci (première fois : une à deux minutes)"; npm ci --no-audit --no-fund
  fi
  if [ "$REBUILD" = 1 ] || newer_than dist/index.html src index.html vite.config.js package.json; then
    npm run build >/dev/null && ok "interface compilée (frontend/dist)"
  else ok "interface déjà à jour"; fi
  cd "$ROOT"
}
build_backend() {
  local profile="${1:-release}"; step "Serveur (Rust, $profile)"
  cd "$ROOT/backend"
  [ "$REBUILD" = 1 ] && cargo clean -q 2>/dev/null || true
  if [ "$profile" = release ]; then
    echo " cargo build --release (première fois : 3 à 6 minutes)"; cargo build --release --locked 2>&1 | tail -n 3
    [ -x "$BIN" ] || die "Compilation échouée."
  else cargo build --locked 2>&1 | tail -n 3; fi
  ok "serveur compilé"; cd "$ROOT"
}

port_free() { ! (exec 3<>"/dev/tcp/${1%:*}/${1##*:}") 2>/dev/null; }
wait_health() { for _ in $(seq 1 40); do curl -sf "http://$1/api/v1/health" >/dev/null 2>&1 && return 0; sleep 0.5; done; return 1; }

# ── Modes ───────────────────────────────────────────────────────────────────────────────────
case "$MODE" in
  check)
    ensure_packages; ensure_libvirt; ensure_toolchains
    printf '\n%sVérification terminée (rien n'"'"'a été installé).%s\n' "$B" "$N"; exit 0 ;;

  iso)
    ensure_libvirt
    step "ISO de test (Alpine Linux « virt », ~60 Mio)"
    base="https://dl-cdn.alpinelinux.org/alpine/latest-stable/releases/x86_64"
    file="$(curl -fsSL "$base/latest-releases.yaml" | grep -o 'alpine-virt-[0-9.]*-x86_64\.iso' | head -n1)"
    [ -n "$file" ] || die "Impossible de déterminer la dernière ISO Alpine."
    virsh_s vol-info --pool default "$file" >/dev/null 2>&1 && { ok "$file déjà présente dans le pool default"; exit 0; }
    tmp="$(mktemp)"; trap 'rm -f "$tmp"' EXIT
    curl -fL --progress-bar -o "$tmp" "$base/$file"
    expected="$(curl -fsSL "$base/$file.sha256" | awk '{print $1}')"
    [ -z "$expected" ] || [ "$(sha256sum "$tmp" | awk '{print $1}')" = "$expected" ] || die "Somme SHA-256 incorrecte : téléchargement corrompu."
    virsh_s vol-create-as default "$file" "$(stat -c%s "$tmp")" --format raw >/dev/null
    virsh_s vol-upload --pool default "$file" "$tmp"
    ok "$file ajoutée au pool default — choisissez-la à la création d'une VM (console VNC : un shell Alpine, identifiant root sans mot de passe)"
    exit 0 ;;

  test|test-full)
    ensure_packages; ensure_libvirt; ensure_toolchains; build_backend release
    step "Test de bout en bout (instance temporaire sur 127.0.0.1:18080)"
    TMPD="$(mktemp -d)"; PW="test-$(head -c 12 /dev/urandom | od -An -tx1 | tr -d ' \n')"
    VIRTMGR_DATA_DIR="$TMPD" VIRTMGR_ADMIN_PASSWORD="$PW" VIRTMGR_STATIC_DIR="" API_BIND=127.0.0.1:18080 "$BIN" >"$TMPD/server.log" 2>&1 &
    SPID=$!; trap 'kill $SPID 2>/dev/null || true; rm -rf "$TMPD"' EXIT
    wait_health 127.0.0.1:18080 || { cat "$TMPD/server.log"; die "Le serveur de test ne démarre pas."; }
    export BASE=http://127.0.0.1:18080 ADMIN_PASSWORD="$PW" VIRTMGR_DATA_DIR="$TMPD" LIBVIRT_URI="$LIBVIRT_URI"
    rc=0; bash "$ROOT/scripts/smoke-test.sh" || rc=$?
    if [ "$MODE" = test-full ]; then bash "$ROOT/scripts/full-test.sh" || rc=$?; fi
    exit $rc ;;

  dev)
    ensure_packages; ensure_libvirt; ensure_toolchains
    step "Mode développement"
    cd "$ROOT/frontend"; [ -d node_modules ] || npm ci --no-audit --no-fund
    cd "$ROOT/backend"; VIRTMGR_STATIC_DIR="" cargo run --locked &
    BPID=$!; trap 'kill $BPID 2>/dev/null || true' EXIT INT TERM
    cd "$ROOT/frontend"; echo " Interface : http://127.0.0.1:5173  (API : $BIND)"; npm run dev ;;

  start)
    ensure_packages; ensure_libvirt; ensure_toolchains; build_frontend; build_backend release
    port_free "$BIND" || die "Le port $BIND est déjà utilisé. Libérez-le ou lancez : API_BIND=127.0.0.1:8081 ./run.sh"
    step "Démarrage"
    printf ' %sOuvrez : http://%s%s   (Ctrl+C pour arrêter)\n' "$B" "$BIND" "$N"
    export API_BIND="$BIND" VIRTMGR_STATIC_DIR="$ROOT/frontend/dist"
    exec "$BIN" ;;
esac
