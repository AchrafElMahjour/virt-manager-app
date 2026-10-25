# Historique

## 1.0.0 — révision 4
- Livraison par **ZIP GitHub** : démarrage rapide (`Code → Download ZIP`, `unzip`, `./run.sh`) dans le README et `docs/TESTING.md`.
- `run.sh` lance désormais les scripts internes via `bash` : le déploiement et les tests fonctionnent même si le ZIP ou l'envoi sur GitHub a fait perdre les droits d'exécution (`bash run.sh` en repli). Vérifié depuis un ZIP produit par `git archive`, sans aucun bit exécutable, dans un dossier à espaces et accents : compilation à froid ≈ 4 min 30, serveur prêt, connexion admin, `./run.sh test` 87/87.
- `.gitattributes` : fins de ligne LF forcées pour les scripts.

## 1.0.0 — révision 3
- Documentation : `docs/TESTING.md` (déploiement et tests pas à pas), `docs/DEPLOY.md`, `docs/ARCHITECTURE.md`, `deploy/` (unité systemd, nginx, Caddy).
- Corrigé : après un refus précoce d'un envoi d'ISO (nom existant, trop grosse…), le serveur lit une quantité bornée du corps avant de répondre ; derrière un reverse-proxy le message exact (409, 413…) arrive au lieu d'un 502. L'interface vérifie aussi le nom avant d'envoyer.
- Corrigé : `run.sh` relancé via `sg libvirt` perdait ses arguments (`test-full` redevenait un simple lancement).
- Scripts : `shellcheck` propre.

## 1.0.0 — révision 2
- Nouveau : `./run.sh test-full` (scripts/full-test.sh, ≈ 400 vérifications), `scripts/testlib.sh`.
- `run.sh` : suppression de faux avertissements « pool/réseau non démarrable » (sortie de `virsh` analysée en locale française et `grep -q` dans un pipe sous `pipefail`) ; détection de Node/npm par les commandes plutôt que par dpkg.
- Interface : l'état de la VM s'affiche en français dans la fiche de détails.

## 1.0.0
- **Authentification** : comptes locaux, Argon2id, sessions par cookie HttpOnly/SameSite=Strict, rôles viewer/operator/admin, anti-force-brute, contrôle d'`Origin`, journal d'audit, gestion des utilisateurs.
- **Hyperviseurs distants** (`qemu+ssh://`, `qemu+tls://`) avec liste blanche des paramètres d'URI, disjoncteur de 15 s, console par tunnel SSH.
- **Stockage** : pools (démarrer/arrêter/rafraîchir/créer/retirer), volumes, téléversement d'ISO en flux, insertion/éjection d'ISO.
- **Console VNC** : WebSocket authentifié (rôle opérateur) ; plus d'origine codée en dur.
- **Modèles IaC** : catalogue + export Terraform/Ansible/Packer par VM.
- **Détection KVM / émulation** par les capacités libvirt.
- Le serveur sert l'interface compilée (un seul processus) ; `run.sh` automatise installation, compilation et lancement.
- Changements incompatibles : toute l'API exige une session ; `GET /api/v1/hypervisors/local` est remplacé par `GET /api/v1/hypervisors/{id}/info`.

## 0.4.0 — 0.3.x
Détails VM, CPU/RAM à froid, disques QCOW2, console noVNC locale ; création, clonage, suppression contrôlée ; correctifs `<pm>` et redirections USB SPICE.

## État de validation de la 1.0.0

Exécuté sur Ubuntu 24.04 avec un vrai libvirt 10 et QEMU 8.2 (émulation logicielle, sans KVM) ; `./run.sh test-full` : 87 + 319 vérifications réussies.

| Élément | Validé |
|---|---|
| 43 tests unitaires Rust | oui |
| `./run.sh test` (87) et `./run.sh test-full` (319) : rôles et sessions, CSRF, cookies, VM (création, CPU/RAM, disques, ISO, clonage, pause, suppression avec/sans disque, VM externe), pools et volumes, envoi d'ISO (intégrité SHA-256, interruption, limites), console VNC par WebSocket, audit, verrouillage | oui, chaque effet relu avec `virsh` |
| Hôte distant `qemu+ssh://` (ici vers localhost) : test de connexion, inventaire, création/démarrage/suppression de VM, **tunnel SSH de la console**, disjoncteur sur hôte injoignable | oui |
| Ansible : playbook modèle **et** playbook exporté exécutés réellement (VM créée avec ses disques, 2ᵉ exécution idempotente), `--syntax-check` | oui |
| XML générés (export Ansible, gabarit `vm.xml.j2`) acceptés par libvirt | oui |
| Terraform / Packer : syntaxe HCL (via `tofu fmt`, OpenTofu 1.13) | syntaxe seulement ; `terraform init/apply` et `packer build` **non exécutés** |
| Rendu de l'interface (8 scénarios jsdom : connexion, toutes les pages, rôles, détails, export, 401) | oui, sans navigateur réel ; la console et la connexion ont ensuite été vérifiées par l'utilisateur sur Debian 13 (captures) |
| Chemin KVM matériel | **non** (testé en émulation ; l'utilisateur n'a pas de `/dev/kvm`) |
| Hôte distant physiquement distinct, `qemu+tls://` | **non** |
| `./run.sh iso`, `./run.sh dev` | **non** (réseau/navigateur indisponibles) |
