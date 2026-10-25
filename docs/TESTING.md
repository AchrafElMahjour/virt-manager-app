# Déployer et tester Virt Manager — guide pas à pas

Ce guide permet de déployer le projet sur une machine neuve et de vérifier qu'il fonctionne, **sans connaître le code**. Durée : ≈ 10 minutes (la première compilation prend 3 à 6 minutes).

## 1. Prérequis

| Élément | Valeur |
|---|---|
| Système | Debian 13 (testé) ou Ubuntu 24.04, avec un compte ayant `sudo` |
| Ressources | 4 Go de RAM, 15 Go de disque libres, accès Internet (apt, rustup, npm, crates.io) |
| Virtualisation | **Non requise** : sans `/dev/kvm` les VM tournent en émulation logicielle (lent mais fonctionnel). Ça marche donc aussi dans une VM sans virtualisation imbriquée. |
| Navigateur | n'importe lequel, sur la même machine (l'application écoute sur `127.0.0.1:8080`) |

## 2. Déploiement depuis le ZIP GitHub

Sur la page GitHub du projet : **Code → Download ZIP**, puis dans un terminal :

```bash
cd ~/Téléchargements
sudo apt install -y unzip            # seulement si « unzip » est absent
unzip virt-manager-app-main.zip
cd virt-manager-app-main             # le nom suit celui du dépôt
./run.sh
```

- Si `./run.sh` répond « Permission denied » (droits d'exécution perdus au téléchargement) : `bash run.sh`. Les autres scripts sont lancés par `run.sh` et n'ont pas besoin de ces droits.
- Le chemin du dossier peut contenir des espaces ou des accents.

`run.sh` détecte ce qui manque et **demande avant d'installer** (réponse `O`) : paquets apt, groupe `libvirt`, Rust via rustup (le Rust de Debian 13 est trop ancien). Il crée le pool et le réseau `default` si besoin, compile, puis affiche :

```
 Ouvrez : http://127.0.0.1:8080
 PREMIER LANCEMENT — compte administrateur créé :
   identifiant  : admin
   mot de passe : ********************
```

Notez le mot de passe (il est aussi dans `data/admin-initial-password.txt`). Pour tout accepter sans question : `./run.sh --yes`. Pour seulement diagnostiquer l'environnement sans rien installer : `./run.sh check`.

> Si le script indique que vous venez d'être ajouté au groupe `libvirt`, il se relance seul ; sinon, déconnectez-vous puis reconnectez-vous et relancez `./run.sh`.

## 3. Tests automatiques (le plus rapide pour valider)

Dans un **second terminal**, serveur lancé ou non :

| Commande | Durée | Résultat attendu |
|---|---|---|
| `cd backend && cargo test --locked` | 1 min | `43 passed; 0 failed` |
| `./run.sh test` | ≈ 1 min | `Résultat : 87 réussis, 0 échoués, 0 ignorés` |
| `./run.sh test-full --yes` | 3 à 5 min | `87 réussis` puis `≈ 310 réussis, 0 échoués`, quelques « ignorés » |

- Ces tests démarrent **leur propre instance** (port 18080, données temporaires) et créent puis suppriment leurs objets (`smoke-*`, `full-*`). Vos comptes et VM ne sont pas touchés.
- Chaque effet est **relu dans libvirt avec `virsh`** : ce n'est pas seulement l'API qui est testée.
- Les « ignorés » sont des contrôles facultatifs dont l'outil n'est pas installé (Terraform/OpenTofu, Packer, Ansible, ou un accès SSH par clé vers localhost). Pour activer le lot « hôte distant par SSH » : `sudo apt install openssh-server`, `ssh-keygen -t ed25519`, `ssh-copy-id localhost`, puis relancer. Désactivez ensuite le service (`sudo systemctl disable --now ssh`) si la machine est sur un réseau partagé.

## 4. Démonstration manuelle (≈ 10 minutes)

Ouvrir `http://127.0.0.1:8080` et se connecter en `admin`.

| # | Action | Résultat attendu |
|---|---|---|
| 1 | Se tromper 5 fois de mot de passe | message « Trop de tentatives… Réessayez dans N s » |
| 2 | Barre latérale → **Mot de passe** : en choisir un nouveau | succès ; `data/admin-initial-password.txt` disparaît |
| 3 | `./run.sh iso` (terminal) puis page **Stockage & ISO** | une ISO Alpine (≈ 60 Mio) apparaît dans le pool `default` (nécessite Internet ; sinon, envoyer n'importe quelle ISO avec le formulaire de la page Stockage) |
| 4 | **Machines virtuelles → + Créer une VM** : 1 vCPU, 512 Mio, 2 Gio, choisir l'ISO | VM « Arrêtée » dans la liste |
| 5 | **▶ Démarrer** puis **Détails → Console VNC** | la console affiche le démarrage d'Alpine ; le clavier fonctionne |
| 6 | **Utilisateurs** : créer `lecteur1` (Lecteur) et `op1` (Opérateur) | comptes créés |
| 7 | Fenêtre privée, connexion `lecteur1` | aucun bouton d'action, pas d'onglet Console, pas de pages d'administration |
| 8 | Fenêtre privée, connexion `op1` | Démarrer / Pause / Éteindre et Console visibles ; Cloner, Supprimer, Créer absents |
| 9 | Éteindre la VM (`poweroff` dans la console, Alpine live ignore l'arrêt ACPI), puis **Détails → Stockage** : éjecter l'ISO, ajouter un disque de 1 Gio | opérations acceptées (elles sont refusées VM en marche) |
| 10 | **Détails → Modèles IaC** : Terraform, Ansible, Packer | fichiers générés avec les bons vCPU/RAM/disques ; boutons Copier/Télécharger |
| 11 | **Journal d'audit** | toutes les actions ci-dessus, sans aucun mot de passe |
| 12 | **Supprimer** la VM (taper son nom, cocher « supprimer le volume ») | VM et disque disparus (`virsh -c qemu:///system list --all`) |

## 5. Nettoyage

```bash
virsh -c qemu:///system list --all           # aucune VM de test ne doit rester
virsh -c qemu:///system vol-list default     # ni volume « virtmgr-* » orphelin
rm -rf data/                                 # supprime comptes et journal de l'application (pas les VM)
```

## 6. Ce que ce guide ne couvre pas

Le chemin KVM matériel, un hôte distant physiquement distinct, `qemu+tls://`, et l'exécution réelle de `terraform apply` / `packer build` n'ont pas été testés (voir `docs/CHANGELOG.md`, tableau de validation). Pour un déploiement permanent derrière HTTPS : `docs/DEPLOY.md`.
