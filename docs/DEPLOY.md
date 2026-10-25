# Déploiement permanent (serveur)

`./run.sh` convient pour un poste de travail ou de démonstration. Pour un service qui démarre au boot, derrière HTTPS, suivre ce guide. L'application reste **un seul exécutable** qui sert l'API et l'interface ; elle pilote libvirt sur la machine où elle tourne (et, en option, des hôtes distants en SSH).

> Statut de validation : la configuration nginx ci-dessous a été testée (smoke test complet, console WebSocket et envoi d'ISO à travers le proxy). L'unité systemd a passé `systemd-analyze verify` mais **n'a pas été démarrée** sur un vrai systemd, et la variante Caddy n'a pas été exécutée : testez-les dans votre environnement.

## 1. Compiler

Sur une machine de build (ou le serveur lui-même) :

```bash
# décompresser le ZIP GitHub (Code → Download ZIP) puis entrer dans le dossier :
unzip virt-manager-app-main.zip && cd virt-manager-app-main
./run.sh check                         # diagnostic ; installe rien
cd frontend && npm ci && npm run build && cd ..
cd backend && cargo build --release --locked && cd ..
```

Rust ≥ 1.89 et Node ≥ 20.19 sont requis (`./run.sh` sait les installer ; voir le README).

## 2. Installer le service

```bash
# Prérequis serveur : libvirt + QEMU, pool et réseau « default » actifs
sudo apt install -y libvirt-daemon-system qemu-system-x86 qemu-utils dnsmasq-base

# Compte système dédié, membre du groupe libvirt (jamais root)
sudo useradd --system --create-home --home-dir /var/lib/virt-manager-app \
     --shell /usr/sbin/nologin --groups libvirt virtmgr

sudo install -d /opt/virt-manager-app
sudo install -m 0755 backend/target/release/virt-manager-api /opt/virt-manager-app/
sudo cp -r frontend/dist /opt/virt-manager-app/dist
sudo cp deploy/virt-manager.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now virt-manager
```

Les modèles Terraform/Ansible/Packer sont embarqués dans l'exécutable : rien d'autre à copier.

**Mot de passe administrateur initial**, au choix :
- le lire : `sudo cat /var/lib/virt-manager-app/admin-initial-password.txt` (ou `sudo journalctl -u virt-manager`) ;
- le fixer avant le premier démarrage dans `/etc/virt-manager-app.env` (propriétaire root, mode 0600) : `VIRTMGR_ADMIN_PASSWORD=…` (10 caractères minimum).

Vérifier : `systemctl status virt-manager` puis `curl -s http://127.0.0.1:8080/api/v1/health`.

## 3. HTTPS avec un reverse-proxy (obligatoire hors de 127.0.0.1)

L'application ne fait pas de TLS elle-même et reste liée à `127.0.0.1:8080`. Sans HTTPS, mots de passe et cookies circuleraient en clair.

1. Installer nginx **ou** Caddy et partir de `deploy/nginx.conf.example` / `deploy/Caddyfile.example`.
2. Ajouter dans `/etc/virt-manager-app.env` : `VIRTMGR_COOKIE_SECURE=1`, puis `sudo systemctl restart virt-manager`.
3. N'ouvrir que le port 443 (et 80 pour la redirection) dans le pare-feu ; **jamais** 8080.

Points d'attention vérifiés par test :

- **`proxy_set_header Host $http_host;`** est indispensable. Avec `$host` (port omis) ou la valeur par défaut de nginx, l'en-tête `Origin` du navigateur ne correspond plus à `Host` et **toutes les actions répondent 403** (protection CSRF). C'est le piège le plus courant.
- Les en-têtes `Upgrade`/`Connection` sont nécessaires pour la console VNC (WebSocket).
- `client_max_body_size 0` et `proxy_request_buffering off` : les ISO sont envoyées en flux (limite côté application : `VIRTMGR_MAX_UPLOAD_GIB`, 16 Gio par défaut). Augmenter les délais (`proxy_read_timeout`) pour les gros fichiers.
- Derrière un proxy, l'application voit l'adresse du proxy : le verrouillage anti-force-brute est alors par couple (proxy, identifiant). Elle ne se fie pas à `X-Forwarded-For` (falsifiable).

## 4. Hôtes distants (facultatif)

Le service tourne sous l'utilisateur `virtmgr` : c'est **son** compte qui se connecte en SSH. Préparer sa clé et `known_hosts` :

```bash
sudo -u virtmgr -H mkdir -p -m 700 /var/lib/virt-manager-app/.ssh
sudo -u virtmgr -H ssh-keygen -t ed25519 -N '' -f /var/lib/virt-manager-app/.ssh/id_ed25519
sudo -u virtmgr -H ssh-copy-id -i /var/lib/virt-manager-app/.ssh/id_ed25519.pub admin@hote-distant
sudo -u virtmgr -H ssh admin@hote-distant true        # accepter l'empreinte une fois
```

Puis, dans l'interface (Hyperviseurs → Ajouter) : `qemu+ssh://admin@hote-distant/system?keyfile=/var/lib/virt-manager-app/.ssh/id_ed25519`. Le compte `admin` distant doit appartenir au groupe `libvirt`.

## 5. Mise à jour, sauvegarde, exploitation

| Tâche | Commande |
|---|---|
| Mise à jour | décompresser la nouvelle version, recompiler (§1), recopier binaire et `dist` (§2), `sudo systemctl restart virt-manager` |
| Journaux | `sudo journalctl -u virt-manager -f` ; audit applicatif dans `/var/lib/virt-manager-app/audit.log` |
| Sauvegarde | `/var/lib/virt-manager-app/` (comptes, hôtes, audit). Les VM et disques sont dans libvirt/le pool, à sauvegarder séparément |
| Mot de passe admin perdu | arrêter le service, supprimer `users.json` du répertoire de données, redémarrer (les autres comptes sont perdus) |
| Test après déploiement | `BASE=http://127.0.0.1:8080 ADMIN_PASSWORD=… ./scripts/smoke-test.sh` (en root ou membre du groupe libvirt ; crée et supprime des objets `smoke-*`) |

## 6. Limites à connaître en production

Comptes locaux uniquement (pas d'annuaire ni de MFA) ; sessions en mémoire (un redémarrage déconnecte tout le monde) ; une seule instance (pas de haute disponibilité) ; chaque requête vers un hôte SSH ouvre une connexion SSH ; pas d'arrêt forcé ni de snapshots dans l'interface. L'application donne, via libvirt, un contrôle étendu de la machine : l'exposer à des utilisateurs de confiance seulement.
