# API REST — Virt Manager v1.0.0

Écoute par défaut sur `http://127.0.0.1:8080`. Spécification OpenAPI : `GET /api-docs/openapi.json` (session requise).
Réponse d'erreur : `{"error":"…"}`.

## Authentification et contexte

Toutes les routes sauf `GET /api/v1/health` et `POST /api/v1/auth/login` exigent une session (cookie `vm_session`, `HttpOnly`).
Avec `curl`, utilisez un fichier de cookies :

```bash
B=http://127.0.0.1:8080
curl -s -c jar -X POST $B/api/v1/auth/login -H 'Content-Type: application/json' \
     -d '{"username":"admin","password":"…"}'
curl -s -b jar $B/api/v1/vms | python3 -m json.tool
curl -s -b jar -c jar -X POST $B/api/v1/vms/$UUID/start -H 'Content-Type: application/json' -d '{}'
```

- **Hyperviseur ciblé** : en-tête `X-Hypervisor: <id>` (défaut `local`) ; `?hv=<id>` pour le WebSocket.
- **Corps JSON** obligatoire sur les POST d'action (`{}`) : refuse les soumissions de formulaires tiers.
- **CSRF** : une requête modifiante dont l'`Origin` n'est pas celui du serveur est refusée (403).
- Les noms de pool/volume sont à encoder en URL (`Téléchargements` → `T%C3%A9l%C3%A9chargements`).

## Rôles

`viewer` < `operator` < `admin`. Colonne « Min. » = rôle minimal.

## Routes

| Méthode | Chemin | Min. | Description |
|---|---|---|---|
| GET | `/api/v1/health` | public | état et version |
| POST | `/api/v1/auth/login` | public | `{username,password}` → cookie de session ; 429 après trop d'échecs |
| POST | `/api/v1/auth/logout` | tout | ferme la session (`{}`) |
| GET | `/api/v1/auth/me` | tout | utilisateur courant |
| POST | `/api/v1/auth/password` | tout | `{current_password,new_password}` ; ferme les autres sessions |
| GET/POST | `/api/v1/users` | admin | lister / créer `{username,password,role}` |
| PUT/DELETE | `/api/v1/users/{username}` | admin | modifier `{role?,disabled?,password?}` / supprimer (jamais le dernier admin ni soi-même) |
| GET | `/api/v1/audit?limit=` | admin | journal d'audit (plus récent d'abord) |
| GET | `/api/v1/hypervisors` | viewer | liste (URI visible des admins seulement) |
| POST | `/api/v1/hypervisors` | admin | `{id?,name,uri}` |
| DELETE | `/api/v1/hypervisors/{id}` | admin | retire de la liste (pas `local`) |
| POST | `/api/v1/hypervisors/{id}/test` | admin | test de connexion (`{}`) |
| GET | `/api/v1/hypervisors/{id}/info` | viewer | nom d'hôte, type, URI |
| GET | `/api/v1/vms` | viewer | inventaire |
| POST | `/api/v1/vms` | admin | créer (arrêtée) |
| GET | `/api/v1/vms/{uuid}` | viewer | fiche détaillée |
| DELETE | `/api/v1/vms/{uuid}` | admin | `{confirm_name,delete_disk}` — VM gérée arrêtée |
| POST | `/api/v1/vms/{uuid}/start\|shutdown\|suspend\|resume` | operator | cycle de vie (`{}`) |
| POST | `/api/v1/vms/{uuid}/clone` | admin | `{name}` — clone à froid |
| PUT | `/api/v1/vms/{uuid}/hardware` | admin | `{vcpus,memory_mib}` à froid |
| POST | `/api/v1/vms/{uuid}/disks` | admin | `{pool,disk_gib}` |
| POST | `/api/v1/vms/{uuid}/disks/{target}/resize` | admin | `{disk_gib}` (agrandissement) |
| DELETE | `/api/v1/vms/{uuid}/disks/{target}` | admin | `{delete_disk}` |
| PUT | `/api/v1/vms/{uuid}/media` | admin | `{iso_pool,iso_name}` ; `null`/`null` pour éjecter |
| GET | `/api/v1/vms/{uuid}/console` | viewer | état de la console |
| GET (WS) | `/api/v1/vms/{uuid}/vnc` | operator | relais RFB binaire ; `Origin` obligatoire |
| GET | `/api/v1/vms/{uuid}/export/{terraform\|ansible\|packer}` | viewer | `{filename,content}` |
| GET | `/api/v1/resources` | viewer | pools actifs, réseaux, ISO |
| GET/POST | `/api/v1/storage/pools` | viewer / admin | lister / créer `{name,path}` |
| DELETE | `/api/v1/storage/pools/{pool}` | admin | `{confirm_name}` — retire de libvirt, fichiers conservés |
| POST | `/api/v1/storage/pools/{pool}/start\|stop\|refresh` | admin | (`{}`) |
| PUT | `/api/v1/storage/pools/{pool}/autostart` | admin | `{autostart}` |
| GET/POST | `/api/v1/storage/pools/{pool}/volumes` | viewer / admin | lister / créer `{name,size_gib,format}` |
| DELETE | `/api/v1/storage/pools/{pool}/volumes/{volume}` | admin | `{confirm_name}` — refusé si utilisé |
| PUT | `/api/v1/storage/pools/{pool}/isos/{name}` | admin | corps = octets de l'ISO, `Content-Length` obligatoire |
| GET | `/api/v1/templates`, `/api/v1/templates/{id}` | viewer | catalogue de modèles IaC |

## Exemples

```bash
# Téléverser une ISO (streaming, 16 Gio max par défaut)
curl -b jar -X PUT -H 'Content-Type: application/octet-stream' \
     --data-binary @debian.iso "$B/api/v1/storage/pools/default/isos/debian.iso"

# Créer une VM avec cette ISO
curl -b jar -X POST $B/api/v1/vms -H 'Content-Type: application/json' -d '{
  "name":"lab-01","vcpus":2,"memory_mib":2048,"disk_gib":20,"pool":"default",
  "network":"default","iso_pool":"default","iso_name":"debian.iso"}'

# Cibler un hyperviseur distant
curl -b jar -H 'X-Hypervisor: labo' $B/api/v1/vms
```

## Codes HTTP

| Code | Sens |
|---|---|
| 200 / 201 | succès / création |
| 400 | paramètre ou nom invalide, confirmation incorrecte |
| 401 | session absente ou expirée |
| 403 | rôle insuffisant, origine refusée, VM non gérée par l'application |
| 404 | VM, pool, volume, hyperviseur ou route inconnu |
| 409 | état incompatible (VM non arrêtée, volume utilisé, doublon…) |
| 411 / 413 / 507 | `Content-Length` manquant / ISO trop grosse / pool plein |
| 429 | trop de tentatives de connexion |
| 502 | erreur libvirt ou hyperviseur injoignable |
