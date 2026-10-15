# Déploiement Docker

[Index](README.md) · [Configuration](configuration.md) · [Exploitation](operations.md)

Les commandes partent de la racine de `histae-api-rust`. Les images contiennent le code compilé ;
aucun mode décrit ici ne monte les sources pour les recharger automatiquement.

## Choisir les manifests

| Installation | Fichiers | Projet Compose | Environnement |
| --- | --- | --- | --- |
| Développement local / LAN | `compose.yaml` + `compose.dev.yaml` | `histae-rust-dev` | `.env`, `ENV=development` imposé |
| Développement derrière le proxy | Les deux précédents + `compose.proxy.yaml` | `histae-rust-dev` | `.env`, reste du développement |
| Production | `compose.yaml` + `compose.production.yaml` | `histae-api-production` | `.env`, `ENV=production` imposé |
| Supervision dev | Incluse dans `compose.dev.yaml`, accès dashboard avec `compose.proxy.yaml` | `histae-rust-dev` | Secrets fichier et `.env` |
| Supervision autonome | `docker-compose.observability.yml`, puis override conteneur si nécessaire | `histae-observability` | Secrets fichier et `.env` choisi |

Ne pas combiner `compose.dev.yaml` et `compose.production.yaml`. Le fichier de base seul ne fournit pas
les services PostgreSQL, Redis, S3 ou de modération. La production n’emploie pas `compose.proxy.yaml` :
son raccordement au proxy est déjà inclus. L’override production réinitialise les ports hôte de l’API
et impose les deux IP de proxy approuvées (`172.30.0.3/32`, `172.30.0.4/32`). Une valeur locale
`TRUST_PROXY=false` ne désactive donc pas la séparation des visiteurs derrière le tunnel en production.

Les deux piles séparent les [rôles PostgreSQL](postgres-roles.md), initialisés automatiquement sur un volume
neuf. Modifier les variables d'un volume existant ne crée pas les rôles ni ne change leurs mots de passe.
Les balises `!override` demandent Docker Compose 2.24.4 ou ultérieur. `healthcheck.start_interval`
demande Docker Engine 25 ou ultérieur : les contrôles s’exécutent toutes les deux secondes pendant
la période de démarrage, puis reprennent leur intervalle habituel. Les tests et nombres de tentatives sont conservés.

<a id="developpement-local"></a>

## Développement local

Préparer `.env` selon la [configuration minimale](configuration.md#configuration-locale-minimale), puis :

```bash
python3 scripts/prepare-postgres-roles-env.py .env
bash scripts/prepare-observability-secrets.sh
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml config --quiet
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d --build --wait
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml ps --all
curl --max-time 5 http://127.0.0.1:8080/health/ready
bash scripts/smoke-api.sh
```

`migrate` doit terminer avec le code 0 ; `api` et les dépendances possédant un healthcheck doivent être
sains, y compris les trois services de supervision. Le worker outbox est un processus permanent ; vérifier ses logs et sa progression.
Le contrôle Docker de l’API utilise `/health/live`, et ne remplace pas la vérification `/health/ready`.

Le service API définit `ulimits.nofile` à **16 384** pour les limites souple et dure dans
`compose.yaml`, en développement comme en production. Les sockets HTTP, PostgreSQL et Redis partagent
ce budget avec les fichiers du processus. Une modification de cette limite nécessite de recréer le
conteneur ; un simple redémarrage ne suffit pas. Après recréation, vérifier par exemple en développement :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml exec -T api \
  sh -c 'printf "soft="; ulimit -Sn; printf "hard="; ulimit -Hn'
```

Les deux valeurs attendues sont `16384`. Surveiller aussi les descripteurs ouverts du processus API
pendant la charge et leur libération lorsque les clients se déconnectent.
Ce budget initial donne une marge pour plusieurs milliers de connexions ; son dimensionnement définitif
dépend du nombre de clients connectés et des mesures de ressources, distincts du débit en RPS.

PostgreSQL utilise l’image construite depuis `docker/postgres/Dockerfile` : PostgreSQL 18.6 bookworm
et PostGIS 3.6.4. Le migrateur installe l’extension et l’index spatial avant la nouvelle API.
Pour la première mise à jour d’un volume existant, suivre la [procédure PostgreSQL/PostGIS](database.md#postgis-et-mise-a-jour-dune-installation).

Les deux piles préchargent aussi `pg_stat_statements`. Sur un volume neuf, son script d’initialisation
est automatique ; sur un volume existant, suivre la [procédure d’activation SQL](sql-performance.md#pg-stat-statements).

### Ports et overrides

| Service | Depuis l’hôte, par défaut | Depuis les conteneurs |
| --- | --- | --- |
| API | `127.0.0.1:8080` | `api:8080` |
| PostgreSQL | `127.0.0.1:5433` si `POSTGRES_PORT=5433` | `postgres:5432` |
| Redis | `127.0.0.1:6379` | `redis:6379` |
| S3 | `127.0.0.1:8333` | `storage.histae.localhost:8333` |
| Modération | `127.0.0.1:8090` | `photo-moderation:8080` |
| Métriques API | Aucun port hôte publié | `api:9091`, activées en dev |
| Prometheus / Grafana / Alertmanager | Aucun port hôte publié | Réseaux privés, accès superadmin via Caddy |

L’override dev impose les adresses internes, PostgreSQL sans TLS, Redis sans TLS ni mot de passe et le S3
HTTP local. Changer `POSTGRES_HOST` ou `REDIS_PASSWORD` dans `.env` ne remplace donc pas ces valeurs dans
les conteneurs. Pour PostgreSQL, `POSTGRES_PORT` règle le port **hôte** publié ; il n’existe pas de variable
`POSTGRES_HOST_PORT` dans ces manifests. L’API utilise `HISTAE_API_HOST_PORT` et `HISTAE_API_BIND_IP`.

Le réseau backend vaut `histae-rust-backend` par défaut en dev. Prometheus y joint directement l’API ;
le réseau interne `observability` relie les trois outils. `compose.dev.yaml` impose `METRICS_ENABLED=true`
et le port interne `9091`, même si les métriques sont désactivées pour les processus natifs dans `.env`.
La pile autonome reste disponible pour une API native ; voir [Supervision](observability.md).

<a id="acces-lan"></a>

## Accès LAN avec Postman

Dans `.env`, publier l’API sur l’interface souhaitée, par exemple toutes les interfaces du serveur :

```dotenv
HISTAE_API_BIND_IP=0.0.0.0
HISTAE_API_HOST_PORT=8080
```

Recréer l’API, puis vérifier les deux chemins d’accès :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d --force-recreate api
ip -br -4 addr
curl --max-time 5 http://127.0.0.1:8080/health/live
curl --max-time 5 http://<IP_LAN_SERVEUR>:8080/health/live
```

Dans Postman sur l’autre machine : `baseUrl=http://<IP_LAN_SERVEUR>:8080`. Utiliser l’adresse de l’interface
LAN active, pas celle de Docker, de Tailscale ou d’un ancien réseau Wi-Fi. Si l’accès local fonctionne et le
client distant expire, vérifier routage, isolation Wi-Fi et filtrage du port. Garder cette publication sur le LAN.
Le smoke accepte uniquement une URL loopback ; tester le LAN avec `curl` ou Postman.

Cette modification publie l’API seule. Les photos signées utilisent toujours l’endpoint S3 configuré :
`storage.histae.localhost` vise normalement la machine cliente, et ne rend pas le stockage du serveur
accessible depuis une autre machine. Pour tester les images à distance, prévoir un nom et un endpoint S3
accessibles aux conteneurs **et** aux clients, et adapter l’override dev ainsi que la politique CSP du dashboard.
Ne pas remplacer simplement le domaine d’une URL déjà signée. Pour un dashboard distant et les passkeys,
utiliser l’origine HTTPS du proxy décrite ci-dessous.

<a id="developpement-derriere-cloudflare-et-le-reverse-proxy"></a>

## Développement derrière Cloudflare et le reverse proxy

La pile voisine [reverse-proxy](../../histae-reverse-proxy/README.md) crée `histae-edge` avec son IPAM.
Le sous-réseau prévu est `172.30.0.0/24` ; il doit être libre sur le serveur. Cette pile réserve :

| Service | Adresse edge | Rôle dans la confiance |
| --- | --- | --- |
| Dashboard | `172.30.0.3` | Proxy approuvé par l’API |
| Caddy | `172.30.0.4` | Proxy approuvé par l’API |
| Cloudflared | `172.30.0.5` | Source approuvée des en-têtes Cloudflare par Caddy |
| API / passerelle S3 production | Allocation automatique | Aucun ajout à `TRUST_PROXY` nécessaire |

Configurer `../histae-reverse-proxy/.env` avec `HISTAE_API_UPSTREAM=http://histae-api-dev:8080` et le même
`HISTAE_EDGE_NETWORK`. Démarrer cette pile depuis son dossier avant l’API ; ne pas précréer son réseau edge.
Elle crée aussi `histae-monitoring`, interne, sur `172.29.0.0/24` : ce second sous-réseau doit être libre.
`compose.proxy.yaml` y raccorde les trois outils sans ajouter de port publié ni de domaine Cloudflare.
Le [guide de supervision](observability.md#supervision-integree-au-developpement) décrit la préparation et l’accès superadmin.
Dans `.env` de l’API, définir l’origine réelle du dashboard, par exemple :

```dotenv
ADMIN_WEBAUTHN_ORIGIN=https://dashboard.histae.com
ADMIN_WEBAUTHN_RP_ID=dashboard.histae.com
CORS_ORIGINS=https://dashboard.histae.com
HISTAE_EDGE_NETWORK=histae-edge
```

Puis, depuis la racine de l’API :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml -f compose.proxy.yaml \
  up -d --build --wait
sudo docker network inspect histae-edge \
  --format '{{range .IPAM.Config}}{{println "Sous-réseau :" .Subnet}}{{end}}{{range .Containers}}{{println .Name .IPv4Address}}{{end}}'
sudo docker exec reverse-proxy-container sh -c 'wget -qO- "$HISTAE_API_UPSTREAM/health/ready"'
```

L’API porte l’alias `histae-api-dev`. `compose.proxy.yaml` impose
`TRUST_PROXY=172.30.0.3/32,172.30.0.4/32`. Ajouter les trois fichiers à **toutes** les commandes de cette pile,
notamment lors d’une recréation de l’API, pour conserver son réseau edge.

Le tunnel cible `http://reverse-proxy:80` pour les domaines site, dashboard et API. Caddy accepte
`CF-Connecting-IP` uniquement depuis le connecteur attendu et remplace `X-Forwarded-For`. L’API ne doit
pas approuver arbitrairement toutes les IP ou toute la plage Docker. Si l’IPAM change, adapter ensemble
le proxy, l’override et les listes de confiance. Voir [Administration](administration.md) pour WebAuthn.

## Production

Pour une installation neuve, préparer `.env` selon [Configuration](configuration.md), compléter
les valeurs de production et préparer les trois mots de passe avec
`python3 scripts/prepare-postgres-roles-env.py .env`. Les [rôles PostgreSQL](postgres-roles.md)
sont créés automatiquement sur le volume vide ; aucun identifiant administrateur n'est injecté dans l'API.

### Environnement et images

Préparer `.env` à partir des groupes de [Configuration](configuration.md). Inclure :

```dotenv
ENV=production
HISTAE_ENV_FILE=.env
HISTAE_API_IMAGE=histae-api:REPLACE_RELEASE_TAG
HISTAE_POSTGRES_IMAGE=histae-postgres:18.6-postgis3.6.4
HISTAE_PHOTO_MODERATION_IMAGE=histae-photo-moderation:REPLACE_RELEASE_TAG
HISTAE_BACKEND_NETWORK=histae-backend
HISTAE_EDGE_NETWORK=histae-edge
HISTAE_STORAGE_HOST=<domaine-public-s3>
TRUST_PROXY=172.30.0.3/32,172.30.0.4/32
```

Choisir une étiquette propre à la livraison. Définir explicitement `HISTAE_API_IMAGE` garantit que les
services applicatifs et `storage-init` utilisent la même image pour une livraison identifiée. Les valeurs de repli des processus applicatifs sont également alignées.
Le serveur exécute Docker ; Rust, Node et pnpm ne sont pas nécessaires sur son hôte.

La configuration impose TLS PostgreSQL/Redis, authentification Redis, S3 HTTPS, Sweego, Stripe live,
origine WebAuthn HTTPS, documents juridiques versionnés et référence de revue réelle. Compléter ces valeurs
avant de lancer les conteneurs : une configuration dev ne doit pas être présentée comme une production.

### Certificats et stockage

Le préparateur centralisé contrôle le `.env` existant, les fichiers requis et leurs permissions,
et crée seulement les secrets S3/supervision manquants, sans remplacer les fichiers existants :

```bash
bash ../histae-scripts/prepare-production.sh
bash ../histae-scripts/prepare-production.sh --check
```

Le bilan indique les variables et fichiers à corriger sans afficher les secrets. Les certificats
restent à fournir selon le tableau ci-dessous ; le script vérifie leur correspondance avec les clés,
les SAN, la CA commune PostgreSQL/Redis et la confiance publique du S3 sur l'hôte.
Voir [les options et limites du préparateur](../../histae-scripts/README.md#préparer-la-production).
La confiance TLS depuis les images clientes doit encore être validée sur la cible.

Préparer les fichiers hors Git, avec les permissions nécessaires aux utilisateurs des images :

Les commandes pour générer les clés S3, signer les certificats PostgreSQL/Redis, obtenir le
certificat public S3 et installer les fichiers sont dans [Secrets de production](production-secrets.md).

| Emplacement hôte | Contenu / contraintes |
| --- | --- |
| `.secrets/postgres/server.crt` | Certificat PEM, SAN `DNS:postgres`, avec intermédiaires éventuels |
| `.secrets/postgres/server.key` | Clé sans passphrase, propriétaire PostgreSQL en `0600`, ou root:groupe PostgreSQL en `0640` |
| `.secrets/postgres/ca.crt` | Autorité publique lisible par les clients applicatifs UID 1000 et Redis |
| `.secrets/redis/server.crt` et `server.key` | SAN `DNS:redis`, même CA ; clé lisible par l’UID Redis, accès restreint |
| `.secrets/seaweedfs/s3.json` | Identité S3 correspondant exactement aux clés applicatives |
| `.secrets/storage-gateway/server.crt` et `server.key` | Certificat du domaine `HISTAE_STORAGE_HOST`, chaîne reconnue par les clients ; clé lisible par UID/GID 101 |

Vérifier les UID avant d’attribuer les permissions, par exemple :

```bash
sudo docker run --rm --entrypoint id postgres:18.6-bookworm postgres
sudo docker run --rm --entrypoint id redis:7.4.2-alpine redis
```

Les répertoires parents doivent être traversables. Garder la clé privée de l’autorité hors du serveur.
Les montages de certificats refusent la création automatique d’un dossier manquant.

Exemple de structure `s3.json`, avec des placeholders à remplacer :

```json
{"identities":[{"name":"histae","credentials":[{"accessKey":"REPLACE_ACCESS_KEY","secretKey":"REPLACE_SECRET_KEY"}],"actions":["Admin","Read","Write","List","Tagging"]}]}
```

Cette identité serveur initialise le bucket privé ; ne jamais la fournir au mobile. `storage-init` vérifie ou
crée le bucket via S3, puis termine avec le code 0. SeaweedFS persiste objets et métadonnées ; cela ne fournit
ni sauvegarde automatique ni haute disponibilité.

Compose impose `OBJECT_STORAGE_ENDPOINT=https://<HISTAE_STORAGE_HOST>` et donne ce nom comme alias privé
à `storage-gateway`. Le tunnel publie le même nom vers `http://reverse-proxy:80`. La route S3 de Caddy
transmet à `https://storage-gateway:443`, vérifie le certificat avec `tls_server_name storage.histae.com`
et conserve explicitement le Host public ainsi que le chemin et les paramètres des requêtes signées.
Pour un autre domaine, adapter ensemble `HISTAE_STORAGE_HOST`, le matcher et le nom TLS de Caddy.
`storage.histae.com` est réservé à la production : il ne suit pas `HISTAE_API_UPSTREAM` et n'utilise
jamais le stockage dev, même lorsque le proxy dirige les requêtes API vers la pile dev.
Les interfaces brutes de SeaweedFS restent sur le réseau `storage` interne.

PostgreSQL utilise SQLx avec vérification du certificat et du nom serveur. Redis utilise également la CA
montée : le nom historique `NODE_EXTRA_CA_CERTS` est lu par les adaptateurs Rust. Après changement de CA,
recréer les clients ; après renouvellement du certificat serveur, recharger le service et vérifier une nouvelle connexion.
Sur un volume PostgreSQL existant, changer `POSTGRES_PASSWORD` ne change pas le mot de passe du rôle :
effectuer une rotation SQL coordonnée. Voir [Base de données](database.md).

### Réseaux et démarrage

La pile reverse proxy crée le réseau edge. Régler son upstream sur `http://histae-api-production:8080`,
puis recréer dashboard et Caddy depuis leur dossier. Le backend de production est externe et doit exister :

```bash
sudo docker network inspect histae-backend >/dev/null 2>&1 || sudo docker network create histae-backend
sudo docker compose --env-file .env -f compose.yaml -f compose.production.yaml config --quiet
sudo docker compose --env-file .env -f compose.yaml -f compose.production.yaml up -d --build --wait
sudo docker compose --env-file .env -f compose.yaml -f compose.production.yaml ps --all
sudo docker exec reverse-proxy-container sh -c 'wget -qO- "$HISTAE_API_UPSTREAM/health/ready"'
```

Adapter le nom si `HISTAE_BACKEND_NETWORK` est personnalisé. Si dev et production coexistent, leur donner
chacun un backend distinct. Aucun port hôte API, PostgreSQL, Redis ou S3 brut n’est publié en production.
Le listener métriques reste privé ; voir [Supervision](observability.md).

La procédure centralisée se lance depuis `../histae-scripts` :

```bash
bash reset-docker-compose.sh --production
```

Elle sélectionne les manifests production et ajoute `compose.production-observability.yaml`
pour les interfaces protégées du dashboard. Elle prépare les secrets de supervision, vérifie les
montages/réseaux, construit avant redémarrage et contrôle Caddy, Cloudflare, S3, readiness et règles
Prometheus. `--check` utilise les fichiers et images existants sans redémarrer les services.
Voir le [guide des scripts](../../histae-scripts/README.md) pour les modes et la maintenance systemd.

### Ressources

La pile vise initialement un serveur de 16 Gio. Les plafonds permanents sont PostgreSQL 7 Gio, API 1 Gio,
outbox 768 Mio, Redis 256 Mio, SeaweedFS 1 Gio, passerelle S3 128 Mio et modération 384 Mio.
La supervision ajoute 1,5 Gio ; maintenance/migration peuvent ajouter 1 Gio chacune et `storage-init` 256 Mio.
Les plafonds ne réservent pas la RAM : prévoir aussi Linux, Docker, le tunnel, les builds et les sauvegardes.

PostgreSQL production utilise [postgresql.conf](../docker/postgres/postgresql.conf), notamment
`max_connections=100` et `shared_buffers=1792MB`. Chaque pool applicatif et pool de verrous compte dans ce
budget ; le recalculer avant réplication ou hausse de `POSTGRES_POOL_MAX`. Le PostgreSQL dev est limité à
1 Gio avec les réglages de l’image. La modération dev possède un plafond différent : 2 Gio.

<a id="mise-a-jour"></a>

## Mise à jour

Pour une pile déjà démarrée, sauvegarder puis sélectionner sa commande Compose. Exemple dev avec proxy :

```bash
COMPOSE=(sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml -f compose.proxy.yaml)
"${COMPOSE[@]}" config --quiet
"${COMPOSE[@]}" build api photo-moderation
"${COMPOSE[@]}" run --rm --no-deps migrate
"${COMPOSE[@]}" up -d --no-deps --force-recreate --wait api outbox-worker photo-moderation
```

Sans proxy, retirer uniquement `-f compose.proxy.yaml`. En production, sélectionner
`--env-file .env -f compose.yaml -f compose.production.yaml`, avec une nouvelle étiquette d’image.
Pour des images déjà publiées, utiliser `pull` à la place du build. Attendre la réussite du migrateur avant
la recréation de l’API ; il installe les index d’une base existante sans rejouer le schéma complet.

Une simple modification de `.env` demande la recréation des services concernés, sans build si le code n’a pas
changé. La recréation interrompt les connexions/SSE ; les données des volumes sont conservées. Après mise à
jour, vérifier readiness, logs, flux métier et [maintenance](operations.md). Un rollback vers une ancienne image
n’est sûr que si elle reste compatible avec le schéma et les états déjà écrits.

## Arrêt et reprise

Utiliser les mêmes fichiers et le même environnement que lors du démarrage. `stop` arrête les processus ;
`up -d` les reprend. `down` retire les conteneurs/réseaux gérés par la pile mais conserve les volumes nommés
par défaut. **`down -v` et les scripts de purge Docker détruisent les données persistantes.**
Après une suppression totale de Docker, restaurer les sauvegardes et reconstruire les piles ; les données ne
reviennent pas en lançant `up`. Voir [Sauvegarde/restauration](database.md#sauvegarde-et-restauration).

Les diagnostics de démarrage sont centralisés dans [Exploitation](operations.md#diagnostic-de-demarrage).
Les validations avant exposition réelle sont dans [Mise en production](production-readiness.md).
