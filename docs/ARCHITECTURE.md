# Architecture

```
 Navigateur ──HTTP(S)──▶ [reverse-proxy facultatif] ──▶ virt-manager-api (Rust / Axum, 127.0.0.1:8080)
   React + noVNC                                           │
                                                           ├─ sert l'interface compilée (frontend/dist)
                                                           ├─ guard : origine → session → rôle → audit
                                                           ├─ routes REST /api/v1/…  et WebSocket /vms/{id}/vnc
                                                           │
                                  libvirt (API native, crate « virt ») ──▶ qemu:///system local
                                                           │              └▶ qemu+ssh:// / qemu+tls:// distants
                                                           └─ fichiers de données (0600) : users.json, hypervisors.json, audit.log
```

## Principes de conception

- **Une seule porte vers libvirt** : l'API native (`virt`), jamais de commande shell ni de chemin de fichier fourni par le client. Les volumes, pools et ISO sont désignés par leur nom libvirt ; les chemins sont relus dans le XML.
- **Le serveur décide, l'interface ne fait que masquer** : un seul middleware (`auth::guard`) applique, dans cet ordre, le contrôle d'`Origin` (CSRF), la session, le rôle minimal (table unique `access_for`), puis l'audit. Toute route inconnue est réservée aux administrateurs.
- **Opérations conservatrices** : modifications à froid uniquement, suppression limitée aux VM créées par l'application (marqueur XML + nom de volume vérifiés), refus si un volume est utilisé, nettoyage des volumes créés en cas d'échec, confirmation par saisie du nom.
- **Console sans port ouvert** : le port VNC est lu dans le XML libvirt (127.0.0.1 exigé) ; pour un hôte SSH, il est atteint par `ssh -W` et non par un port exposé.
- **Hyperviseur ciblé par requête** : en-tête `X-Hypervisor` (ou `?hv=`), résolu par un extracteur (`AppState`) ; disjoncteur de 15 s sur les hôtes injoignables.

## Modules du serveur (`backend/src`)

| Fichier | Rôle |
|---|---|
| `main.rs` | routes, OpenAPI, service du front, en-têtes de sécurité, démarrage |
| `auth.rs` | comptes (Argon2id), sessions en mémoire, rôles, verrouillage, garde, audit des connexions |
| `hypervisors.rs` | registre local/distant, validation stricte des URI, extracteur `AppState`, disjoncteur |
| `provisioning.rs` | création, clonage, suppression de VM |
| `management.rs` | fiche VM, CPU/RAM, disques (ajout, agrandissement, détachement) |
| `storage.rs` | pools, volumes, envoi d'ISO en flux, média CD-ROM |
| `console.rs` | relais WebSocket ⇄ VNC (direct ou tunnel SSH) |
| `templates.rs` | catalogue et export Terraform / Ansible / Packer |
| `audit.rs`, `config.rs`, `util.rs` | journal JSONL, configuration par variables d'environnement, utilitaires |

## Interface (`frontend/src`)

`App.jsx` (connexion, navigation, sélecteur d'hyperviseur) ; `api.js` (appels, hyperviseur courant, téléversement avec progression) ; `pages/` : `Vms`, `VmDetails`, `Storage`, `Templates`, `Admin` (hyperviseurs, utilisateurs, audit), `Login`.

## Tests

| Niveau | Où | Contenu |
|---|---|---|
| Unitaires | `cargo test` (43) | validation, XML, droits, hachage, URI, génération IaC |
| Intégration rapide | `./run.sh test` (87) | API réelle contre libvirt |
| Intégration approfondie | `./run.sh test-full` (≈ 310) | scénarios complets relus avec `virsh`, SSH, console, IaC |
