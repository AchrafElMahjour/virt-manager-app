# Virt Manager — v1.0.0


Gestion de machines virtuelles **KVM/QEMU via libvirt** : back-end Rust (Axum) + interface React/Vite, servie par le même exécutable.
Conçu pour **Debian 13** (fonctionne aussi sur Ubuntu).

## Démarrage rapide (depuis le ZIP GitHub)

1. Sur la page GitHub du projet : bouton vert **Code → Download ZIP**.
2. Dans un terminal, sur une machine **Debian 13 ou Ubuntu 24.04** :

```bash
cd ~/Téléchargements
sudo apt install -y unzip          # seulement si « unzip » est absent
unzip virt-manager-app-main.zip
cd virt-manager-app-main           # le nom du dossier suit celui du dépôt
./run.sh                           # si « Permission denied » : bash run.sh
```

3. Ouvrir **http://127.0.0.1:8080** et se connecter avec `admin` et le mot de passe affiché dans le terminal.
4. *(facultatif)* Test automatique, dans un second terminal : `./run.sh test` (≈ 1 min ; résultat attendu : `87 réussis, 0 échoués`). Sinon, utiliser librement l'interface.

Guide détaillé avec les résultats attendus : **[docs/TESTING.md](docs/TESTING.md)** · déploiement permanent derrière HTTPS : [docs/DEPLOY.md](docs/DEPLOY.md) · architecture : [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

C'est tout. Au premier lancement, `run.sh` vérifie ce qui manque et **demande avant** d'installer (paquets apt, groupe `libvirt`, Rust via rustup si nécessaire), crée le pool et le réseau `default` s'ils n'existent pas, compile (3 à 6 minutes la première fois) puis démarre :

```
 Ouvrez : http://127.0.0.1:8080
 PREMIER LANCEMENT — compte administrateur créé :
   identifiant  : admin
   mot de passe : ********************
```

Connectez-vous avec ce compte (le mot de passe est aussi dans `data/admin-initial-password.txt`, supprimé dès que vous le changez dans l'interface).

| Commande | Effet |
|---|---|
| `./run.sh` | installe / compile / démarre |
| `./run.sh --yes` | idem sans aucune question |
| `./run.sh check` | diagnostic de l'environnement, rien n'est installé ni lancé |
| `./run.sh test` | test rapide (≈ 90 vérifications, 1 min) sur une instance temporaire ; crée puis supprime ses propres objets `smoke-*` |
| `./run.sh test-full` | test **approfondi** (≈ 400 vérifications, 3 à 5 min) : rôles, sessions, VM, disques, ISO, clonage, pools, suppression, hôte distant par SSH, console, modèles IaC. Chaque effet est relu dans libvirt avec `virsh`. Crée puis supprime ses objets `full-*` ; ne touche pas à vos données |
| `./run.sh iso` | télécharge une petite ISO Alpine (≈ 60 Mio, SHA-256 vérifié) dans le pool `default` pour tester une VM avec console |
| `./run.sh dev` | développement : front à rechargement à chaud sur `:5173` + API |

Si vous venez d'être ajouté au groupe `libvirt`, le script se relance seul via `sg libvirt` : pas besoin de vous reconnecter.

### Premier essai conseillé (5 minutes)

1. `./run.sh iso` (ou « Stockage & ISO » → envoyer votre propre ISO).
2. « Machines virtuelles » → **+ Créer une VM** : 1 vCPU, 512 Mio, 2 Gio, choisir l'ISO.
3. **Détails → Stockage** pour changer/éjecter l'ISO, **▶ Démarrer**, puis **Détails → Console VNC**.
4. **Détails → Modèles IaC** : export Terraform / Ansible / Packer de cette VM.
5. Pour nettoyer : éteindre la VM puis **Supprimer** (avec disque).

## Fonctionnalités

- **Authentification** : comptes locaux, mots de passe Argon2id, session par cookie `HttpOnly` + `SameSite=Strict`, verrouillage progressif après 5 échecs, protection CSRF par contrôle de l'`Origin`, journal d'audit.
- **Rôles** appliqués à chaque requête par le serveur (l'interface ne fait que masquer les boutons) :

  | Rôle | Droits |
  |---|---|
  | Lecteur | consulter VM, stockage, modèles |
  | Opérateur | + démarrer / pause / reprendre / arrêt ACPI, console VNC |
  | Administrateur | + créer, cloner, supprimer, CPU/RAM, disques, stockage, hyperviseurs, utilisateurs, audit |

- **VM** : inventaire, cycle de vie, création (QCOW2, ISO facultative), clonage à froid, suppression contrôlée (VM créées par l'application seulement), CPU/RAM à froid, disques supplémentaires (ajout, agrandissement, détachement), insertion/éjection d'ISO.
- **Stockage** : pools (démarrer, arrêter, rafraîchir, créer un pool `dir`, retirer), volumes (créer, supprimer — jamais un volume utilisé par une VM), **téléversement d'ISO** avec barre de progression, écrit par libvirt (aucun droit root côté application).
- **Console web** (noVNC) : relais WebSocket authentifié ; le port VNC est lu dans le XML libvirt, jamais fourni par le client.
- **Hyperviseurs distants** : `qemu+ssh://` (console par tunnel SSH, rien d'exposé) et `qemu+tls://`. Sélecteur dans la barre latérale.
- **Modèles IaC** : catalogue Terraform / Ansible / Packer (dossier `templates/`) et export généré à partir d'une VM existante.
- **KVM ou émulation** : détection automatique ; sans `/dev/kvm`, les VM tournent en émulation logicielle (lent mais fonctionnel).

## Configuration (variables d'environnement)

| Variable | Défaut | Rôle |
|---|---|---|
| `API_BIND` | `127.0.0.1:8080` | adresse d'écoute |
| `LIBVIRT_URI` | `qemu:///system` | hyperviseur « local » |
| `VIRTMGR_DATA_DIR` | `./data` (via `run.sh`) | comptes, hôtes, audit (permissions 0600) |
| `VIRTMGR_ADMIN_PASSWORD` | aléatoire | mot de passe initial de `admin` (≥ 10 caractères), utilisé uniquement à la création |
| `VIRTMGR_SESSION_HOURS` | `12` | durée d'inactivité d'une session |
| `VIRTMGR_MAX_UPLOAD_GIB` | `16` | taille maximale d'une ISO téléversée |
| `VIRTMGR_POOL_ROOTS` | `/var/lib/libvirt,/srv,/mnt,/media,/opt,/data,/home` | racines autorisées pour créer un pool |
| `VIRTMGR_ALLOWED_ORIGINS` | `http://127.0.0.1:5173,http://localhost:5173` | origines navigateur supplémentaires (mode `dev`) |
| `VIRTMGR_COOKIE_SECURE` | désactivé | `1` derrière HTTPS : ajoute `Secure` au cookie |
| `VIRTMGR_STATIC_DIR` | `frontend/dist` | dossier de l'interface compilée |

## Hyperviseurs distants

Administration → Hyperviseurs → **Ajouter un hôte distant**, par exemple `qemu+ssh://admin@192.168.1.20/system?keyfile=/home/vous/.ssh/id_ed25519`.

Prérequis : le serveur Virt Manager doit pouvoir se connecter **sans interaction** (clé SSH sans mot de passe, hôte déjà accepté une fois dans `~/.ssh/known_hosts` via `ssh admin@192.168.1.20`), et l'utilisateur distant doit pouvoir utiliser libvirt (groupe `libvirt`). Utilisez le bouton **Tester**. Un hôte injoignable est mis en pause 15 s pour ne pas bloquer l'interface ; pensez à `ConnectTimeout` dans `~/.ssh/config`.

Seuls `qemu+ssh://` et `qemu+tls://` sont acceptés, avec une liste blanche de paramètres (`keyfile`, `pkipath`, `no_tty`, `no_verify`, `known_hosts_verify`). Les paramètres capables d'exécuter une commande (`command`, `proxy`, `netcat`…) sont refusés.

## Sécurité — à lire avant d'exposer l'application

- Par défaut tout reste sur **127.0.0.1**. Si vous écoutez ailleurs, placez un **reverse-proxy HTTPS** devant (sinon mots de passe et cookies circulent en clair) et définissez `VIRTMGR_COOKIE_SECURE=1`. Le serveur affiche un avertissement quand l'écoute n'est pas en boucle locale.
- **Ne lancez pas l'application avec `sudo`** : elle utilise le groupe `libvirt`. Être dans ce groupe équivaut à de très gros droits sur la machine (comme avec virt-manager).
- Pas de MFA, pas d'annuaire (LDAP/OIDC) : comptes locaux uniquement. Les sessions sont en mémoire : un redémarrage du serveur déconnecte tout le monde.
- Mot de passe admin perdu : arrêter le serveur, supprimer `data/users.json`, relancer (un nouveau compte `admin` est créé ; les autres comptes sont perdus).
- Ne supprimez pas à la main des volumes `virtmgr-*` : ils portent les disques des VM gérées.

## Limites connues

- Clonage : VM BIOS arrêtée, un seul disque fichier dans un pool `dir` actif, sans TPM/UEFI/snapshots. Un clone garde l'identité interne de l'OS (hostname, machine-id, clés SSH) : à adapter avant usage simultané.
- Les modifications (CPU/RAM, disques, ISO) se font **à froid** ; pas d'arrêt forcé dans l'interface.
- Pas de snapshots, de migration, de réseaux virtuels (création) ni de pools autres que `dir` en écriture.
- Console : VNC uniquement (les VM SPICE s'ouvrent avec `virt-manager`/`remote-viewer`) ; indisponible en `qemu+tls://`.
- Chaque requête vers un hôte distant ouvre une connexion SSH (pas de cache de connexion) : l'interface est plus lente que sur l'hôte local.
- Modèles Terraform/Packer : syntaxe HCL vérifiée seulement (pas d'`apply`/`build` réel) ; Terraform vise `dmacvicar/libvirt` 0.8.x. Le playbook Ansible, lui, a été exécuté pour de vrai (voir `docs/CHANGELOG.md`).

## Prérequis détaillés (si vous préférez installer à la main)

```bash
sudo apt install -y build-essential pkg-config libvirt-dev libclang-dev libvirt-clients libvirt-daemon-system \
  qemu-system-x86 qemu-utils dnsmasq-base curl python3 nodejs npm openssh-client
sudo adduser "$USER" libvirt        # puis nouvelle session
# Rust >= 1.89 (le Rust de Debian 13 est plus ancien) :
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal
cd frontend && npm ci && npm run build && cd ../backend && cargo build --release
VIRTMGR_STATIC_DIR=../frontend/dist ./target/release/virt-manager-api
```

Node ≥ 20.19 est requis (Debian 13 fournit 20.19).

## Service permanent (facultatif)

Pour un vrai déploiement (compte dédié, durcissement systemd, HTTPS) : **[docs/DEPLOY.md](docs/DEPLOY.md)**. Version minimale pour un usage personnel :

`~/.config/systemd/user/virt-manager.service` :

```ini
[Service]
WorkingDirectory=%h/Projects/virt-manager-app
Environment=VIRTMGR_DATA_DIR=%h/Projects/virt-manager-app/data
Environment=VIRTMGR_STATIC_DIR=%h/Projects/virt-manager-app/frontend/dist
ExecStart=%h/Projects/virt-manager-app/backend/target/release/virt-manager-api
Restart=on-failure
[Install]
WantedBy=default.target
```

puis `systemctl --user enable --now virt-manager` (et `loginctl enable-linger $USER` pour qu'il démarre sans session ouverte).

## Dépannage

| Symptôme | Piste |
|---|---|
| `Impossible de joindre libvirt` | `sudo systemctl enable --now libvirtd` ; `virsh -c qemu:///system list --all` |
| « Aucun pool de stockage dir actif » | `virsh -c qemu:///system pool-start default` (ou page Stockage) |
| VM très lente | pas de `/dev/kvm` : activer VT-x/AMD-V dans le BIOS |
| 403 « Origine non autorisée » | accès via une autre adresse/port que celle du serveur : ajoutez-la à `VIRTMGR_ALLOWED_ORIGINS` |
| `rustc 1.85 is not supported` | Rust trop ancien : `./run.sh` installe rustup, sinon voir « Prérequis détaillés » |
| Hôte distant « injoignable » | `ssh utilisateur@hote true` doit réussir sans question depuis le compte qui lance le serveur |

## Structure

```
run.sh                   lancement en une commande
backend/src/             main (routes), auth, hypervisors, provisioning, management, storage, console, templates, audit, config
frontend/src/            App, api.js, pages/{Vms,VmDetails,Storage,Templates,Admin,Login}
templates/               modèles Terraform / Ansible / Packer (embarqués dans le binaire)
scripts/smoke-test.sh    test rapide (./run.sh test)
scripts/full-test.sh     test approfondi (./run.sh test-full) ; scripts/testlib.sh : fonctions communes
docs/API.md              référence de l'API ; spécification OpenAPI sur /api-docs/openapi.json
docs/TESTING.md          guide de déploiement et de test pas à pas
docs/DEPLOY.md           service permanent, HTTPS, mise à jour
docs/ARCHITECTURE.md     conception et modules
docs/CHANGELOG.md        historique et état de validation
deploy/                  unité systemd, exemples nginx et Caddy
```

## Feuille de route

- [x] 1 — Lecture des VM · [x] 2 — Cycle de vie · [x] 3 — Création, clonage, disques
- [x] 4 — Détails VM, CPU/RAM, disques, **pools / volumes / ISO**
- [x] 5 — Console VNC avec relais WebSocket **authentifié**
- [x] 6 — **Hyperviseurs distants, authentification, rôles**, audit
- [x] 7 — **Modèles Terraform, Ansible, Packer**
