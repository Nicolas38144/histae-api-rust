# Conteneurisation et déploiement

Ce guide décrit l’image Histae API et les compositions Docker. Il sépare volontairement le développement local
d’un déploiement sur un serveur de 16 Gio : le premier fournit PostgreSQL, Redis, SeaweedFS et la modération ;
le second héberge aussi PostgreSQL TLS, Redis TLS, SeaweedFS server, sa passerelle HTTPS et la modération. Cette topologie de production
mono-nœud accepte un point de panne unique ; elle exige des sauvegardes hors machine et une restauration testée.

## Fichiers et responsabilités

| Fichier | Responsabilité |
| --- | --- |
| `Dockerfile` | image de développement et image d’exécution multi-stage |
| `.dockerignore` | exclusion des secrets, sorties et fichiers inutiles du contexte de build |
| `compose.yaml` | API, migration PostgreSQL, worker outbox et tâche de maintenance |
| `compose.dev.yaml` | stockages locaux, code monté, ports loopback et endpoints Docker |
| `compose.production.yaml` | PostgreSQL TLS à 7 Gio, environnement strict, réseaux externes et aucun port hôte |
| `docker/postgres/` | configuration mémoire et règles TLS/SCRAM du serveur PostgreSQL de production |
| `docker/redis/`, `docker/seaweedfs/`, `docker/storage-gateway/` | Redis TLS, métadonnées S3 persistantes et proxy HTTPS S3 |
| `compose.observability-container.yaml` | raccordement de Prometheus au listener interne `api:9091` |

L’image finale contient les binaires Rust et le runtime Node minimal requis par le codec Sharp/HEIC. Les migrations
SQL sont embarquées dans `db-migrate`. Elle ne contient ni `.env`, ni `.secrets`, ni tests. L’API, le worker et les
commandes d’exploitation utilisent tous cette même image.

## Développement complet

Sur le PC Windows, exécuter Docker et les tests d'infrastructure via WSL, depuis ce dépôt. Par exemple :

```powershell
wsl docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d --build --wait
```

Les commandes Bash ci-dessous s'exécutent directement dans WSL ou sur Debian. Le PC de développement reste
distinct du serveur de production : PostgreSQL y conserve son plafond de 1 Gio.

### Préparer les valeurs locales

Créer puis protéger la configuration locale `.env` directement dans ce dépôt :

```bash
touch .env
chmod 600 .env
```

Renseigner au minimum `POSTGRES_PASSWORD`, `JWT_SECRET`, `PHONE_ENCRYPTION_KEY`, `PHONE_HASH_KEY` et
`PHOTO_MODERATION_TOKEN`. Utiliser une sortie distincte de cette commande pour chaque secret :

```bash
openssl rand -hex 32
```

Les variables applicatives de `.env` restent adaptées aux commandes exécutées depuis l’hôte : PostgreSQL et Redis
utilisent donc des adresses loopback. `compose.dev.yaml` les remplace uniquement dans les conteneurs par les noms
de service `postgres` et `redis`.

### Démarrer

L’exemple publie PostgreSQL Docker sur le port 5433 pour éviter une installation PostgreSQL native qui écoute déjà
sur 5432. `POSTGRES_HOST_PORT` et `POSTGRES_PORT` doivent conserver la même valeur pour l’API et les tests exécutés
depuis l’hôte. Les conteneurs continuent d’utiliser `postgres:5432` sur le réseau interne.

Depuis la racine du dépôt :

```bash
docker compose --env-file .env \
  -f compose.yaml \
  -f compose.dev.yaml \
  up -d --build --wait
```

Cette commande :

1. attend le healthcheck PostgreSQL ;
2. exécute les migrations PostgreSQL dans le service `migrate` ;
3. ne démarre l’API et le worker outbox qu’après la réussite complète des migrations ;
4. attend que l’API réponde sur `/health/live`.

Le conteneur `migrate` terminé avec le code `0` est un état normal :

```bash
docker compose --env-file .env -f compose.yaml -f compose.dev.yaml ps --all
curl -fsS http://127.0.0.1:8080/health/live
curl -fsS http://127.0.0.1:8080/health/ready
```

Les ports de développement sont liés à `127.0.0.1`. PostgreSQL utilise un volume Docker persistant à l’emplacement
prévu par l’image PostgreSQL 18 ; retirer le conteneur n’efface donc pas la base.

### Développer et exploiter

Les conteneurs utilisent l’image compilée. Un changement Rust, codec ou configuration de compilation exige donc un
rebuild. Afficher des logs ciblés :

```bash
docker compose --env-file .env -f compose.yaml -f compose.dev.yaml logs -f api outbox-worker
```

Relancer explicitement les migrations idempotentes :

```bash
docker compose --env-file .env -f compose.yaml -f compose.dev.yaml run --rm migrate
```

Exécuter une passe de maintenance bornée :

```bash
docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  --profile jobs run --rm --no-deps maintenance
```

Créer le bootstrap de la première passkey administrateur :

```bash
docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  exec api /app/bin/admin-bootstrap <uuid-du-compte-admin>
```

Le token affiché est à usage unique. Il ne doit être ni journalisé ni enregistré dans un fichier du dépôt.

### Arrêter ou reconstruire

```bash
docker compose --env-file .env -f compose.yaml -f compose.dev.yaml down
docker compose --env-file .env -f compose.yaml -f compose.dev.yaml build --pull
```

`down` conserve les volumes. Ne pas ajouter `--volumes` sauf si la suppression de toutes les données locales est
volontaire. Une ancienne base PostgreSQL installée directement sur l’hôte n’est ni importée ni supprimée par cette
composition.

## Nom du stockage objet et URLs signées

`OBJECT_STORAGE_ENDPOINT` vaut en développement :

```ini
OBJECT_STORAGE_ENDPOINT=http://storage.histae.localhost:8333
```

Le suffixe réservé `.localhost` résout vers loopback sur la machine du navigateur. Sur le réseau Docker,
`compose.dev.yaml` donne exactement le même nom comme alias au service SeaweedFS. L’API contacte donc SeaweedFS et
produit une URL signée portant un hôte que le dashboard peut lui aussi joindre.

Ne pas remplacer cette valeur par `http://object-storage:8333` : la signature serait valide dans Docker, mais l’URL
retournée serait inutilisable depuis un navigateur ou l’application mobile. En production, le endpoint doit être
un nom HTTPS stable, résolu de manière cohérente depuis les conteneurs et les clients autorisés.

## Supervision avec l’API conteneurisée

Créer les secrets décrits dans [observability.md](observability.md), puis démarrer la pile avec son override :

```bash
docker compose --env-file .env \
  -f docker-compose.observability.yml \
  -f compose.observability-container.yaml \
  up -d --wait
```

Prometheus rejoint alors `api:9091` sur le réseau privé `histae-backend`. Le port 9091 n’est pas publié sur l’hôte.
Le fichier d’origine sans override reste utilisable lorsque l’API tourne directement sur l’hôte.

## Construire l’image finale

```bash
docker build --pull --target production -t histae-api:local .
docker image inspect histae-api:local --format '{{.Config.User}} {{.Config.WorkingDir}}'
```

L’inspection doit retourner l’utilisateur `node` et `/app`. Pour un déploiement, étiqueter l’image avec une version
immuable ou, de préférence, la référencer par digest. Ne pas utiliser `latest` comme mécanisme de retour arrière.

## Déploiement mono-machine

Le projet Docker de production s'appelle `histae-api-production`, celui de développement `histae-api`.
Ne jamais combiner les deux overrides. Le volume `histae-postgres-production-data` est distinct du volume local.

La composition de production ne publie aucun port. Elle suppose deux réseaux Docker déjà contrôlés :

- `histae-backend`, partagé uniquement avec les stockages TLS et la supervision ;
- `histae-edge`, partagé uniquement avec l’API, la passerelle S3 HTTPS et le reverse proxy ou tunnel Cloudflare.

Le réseau `storage`, interne au projet, relie uniquement SeaweedFS à sa passerelle HTTPS. Les ports master,
volume et filer ne sont accessibles ni à l'API ni au tunnel. Tout s'exécute sur le même serveur ; les réseaux
Docker assurent la séparation des interfaces.

Les créer une fois si l’orchestrateur du tunnel ne les a pas déjà créés :

```bash
docker network create histae-backend
docker network create histae-edge
```

Créer `.env.production` avec `ENV=production`, `HISTAE_ENV_FILE=.env.production`, une image immuable dans
`HISTAE_API_IMAGE` et toutes les valeurs exigées par `ConfigService`. Les contrôles de production refusent notamment
PostgreSQL sans TLS, Redis sans TLS/mot de passe, un endpoint S3
HTTP, Sweego ou Stripe incomplets et un proxy globalement approuvé.

### PostgreSQL local et certificats

Compose impose aux quatre processus applicatifs `POSTGRES_HOST=postgres`, `POSTGRES_PORT=5432`,
`POSTGRES_SSLMODE=verify-full` et `NODE_EXTRA_CA_CERTS=/run/secrets/postgres_ca`.
Le client `pg` reçoit `ssl: true` et valide la chaîne de confiance et le nom serveur avec Node TLS.
Le serveur refuse les connexions TCP non chiffrées et exige SCRAM sur les connexions TLS.

Avant le premier démarrage, préparer dans `.secrets/postgres/` :

- `server.crt` : certificat serveur PEM avec SAN `DNS:postgres`, suivi des intermédiaires éventuels ;
- `server.key` : clé privée correspondante, sans passphrase pour le démarrage non interactif ;
- `ca.crt` : certificat public PEM de l'autorité de confiance, fourni aux clients Node.

Utiliser une autorité privée administrée et conserver sa clé privée hors du serveur et du dépôt. Aucun certificat
de production n'est généré automatiquement. Sur Debian, `server.key` doit appartenir à l'UID PostgreSQL de
l'image et être en mode `0600` (ou root, groupe PostgreSQL, mode `0640`). Vérifier les UID/GID avec
`docker run --rm --entrypoint id postgres:18.6-bookworm postgres` ; les répertoires parents doivent être traversables
par cet utilisateur. `ca.crt` doit aussi être lisible par l'UID 1000 des conteneurs applicatifs.
Les montages refusent de créer silencieusement un dossier manquant.

`POSTGRES_USER`, `POSTGRES_PASSWORD` et `POSTGRES_DB` proviennent du même `.env.production` pour PostgreSQL et
l'application. Sur un volume existant, changer ces variables ne change ni les rôles ni leurs mots de passe :
effectuer une rotation SQL coordonnée. L'utilisateur créé par l'image est administrateur PostgreSQL ; la séparation
future du rôle de migration et du rôle applicatif demande des droits dédiés.

Après renouvellement du certificat, recharger PostgreSQL avec `SELECT pg_reload_conf()` via une session
administrative et vérifier la nouvelle connexion TLS. Si la CA change, recréer aussi API, migrations et workers :
Node charge `NODE_EXTRA_CA_CERTS` au démarrage. Voir les règles de
[TLS PostgreSQL](https://www.postgresql.org/docs/18/ssl-tcp.html).

### Budget mémoire du serveur de 16 Gio

| Service | Plafond mémoire |
| --- | --- |
| PostgreSQL | 7 Gio, sans swap |
| API | 1 Gio |
| Outbox | 768 Mio |
| Redis | 256 Mio (données bornées à 128 Mio) |
| SeaweedFS server | 1 Gio |
| Passerelle HTTPS S3 | 128 Mio |
| Modération photo | 384 Mio |
| Maintenance ponctuelle | 1 Gio |
| Migration ponctuelle | 1 Gio |
| Initialisation S3 ponctuelle | 256 Mio |
| Prometheus + Alertmanager + Grafana, si installés | 1,5 Gio au total |

La pile permanente avec supervision représente 12 Gio de plafonds cumulés. En comptant même les trois jobs
simultanément, elle atteint 14,25 Gio. Il reste alors 1,75 Gio sur 16 Gio pour Linux, Docker, le tunnel/proxy et
les autres processus ; en régime permanent, la marge est de 4 Gio. Éviter builds et gros traitements de sauvegarde
pendant les pics. Les sauvegardes doivent disposer d'une destination hors serveur malgré cette colocalisation.
Les plafonds ne réservent pas toute cette mémoire. Docker utilise ici des unités binaires (`7g` = 7 Gio) ;
vérifier la RAM réellement disponible sur le serveur et dans la VM Docker Desktop/WSL en local.

Le plafond PostgreSQL couvre buffers, connexions, mémoire partagée et cache de fichiers imputé au conteneur.
`memswap_limit: 7g`, égal à `mem_limit`, interdit de dépasser le budget via le swap.
Les réglages initiaux de `docker/postgres/postgresql.conf` sont `shared_buffers=1792MB`, `work_mem=4MB`,
`maintenance_work_mem=128MB`, `autovacuum_work_mem=64MB` et `max_connections=100`.
`effective_cache_size=4GB` est une estimation pour le planificateur, pas une allocation.
`work_mem` se multiplie par les opérations et workers actifs ; ces valeurs ne garantissent pas l'absence d'OOM.
Chaque processus applicatif peut ouvrir `POSTGRES_POOL_MAX + 4` connexions : recalculer avant toute réplication.
Sur le PC de développement, PostgreSQL reste limité à 1 Gio et conserve les réglages SQL de l'image.
Le budget de 7 Gio et le fichier SQL de configuration concernent uniquement le serveur de production.
Références : [limites Compose](https://docs.docker.com/reference/compose-file/services/#memswap_limit) et
[mémoire PostgreSQL](https://www.postgresql.org/docs/18/runtime-config-resource.html).

### Valider et démarrer

Préparer également les services colocalisés :

- Redis : `.secrets/redis/server.crt` et `server.key`, SAN `DNS:redis`, signés par la même CA que PostgreSQL.
  La clé doit être lisible par l'UID Redis de l'image et inaccessible aux autres utilisateurs. Renseigner
  `REDIS_PASSWORD` ; le port non TLS est désactivé. Redis ne persiste que des compteurs éphémères.
- S3 : `.secrets/seaweedfs/s3.json` contient une identité avec les mêmes clés que `OBJECT_STORAGE_ACCESS_KEY`
  et `OBJECT_STORAGE_SECRET_KEY`. Le schéma minimal est ci-dessous ; ne jamais placer de vraies clés dans Git.
- Passerelle S3 : `.secrets/storage-gateway/server.crt` (chaîne publique complète) et `server.key`, pour le
  domaine public `HISTAE_STORAGE_HOST` sans schéma ni port. La clé doit être lisible par l'UID 101 du proxy,
  par exemple root:groupe 101 et `0640`. Utiliser un certificat reconnu par les navigateurs.
- Modération : renseigner `PHOTO_MODERATION_TOKEN`, puis construire l'image avec
  `docker compose --env-file .env.production -f compose.yaml -f compose.production.yaml build photo-moderation`.
  Pour une livraison reproductible, définir `HISTAE_PHOTO_MODERATION_IMAGE` avec une étiquette immuable.

```json
{"identities":[{"name":"histae","credentials":[{"accessKey":"REPLACE_ACCESS_KEY","secretKey":"REPLACE_SECRET_KEY"}],"actions":["Admin","Read","Write","List","Tagging"]}]}
```

Cette identité admin S3 permet l'initialisation du bucket. Elle est réservée aux processus serveur et ne doit
jamais être fournie au client mobile. `storage-init` vérifie/crée le bucket via S3 sans ajouter de politique
publique, puis se termine avec le code 0 ; l'API attend sa réussite. `OBJECT_STORAGE_BUCKET` et les clés doivent
correspondre au fichier d'identité. Un échec d'authentification bloque le démarrage, sans création alternative.
SeaweedFS utilise `server` et persiste objets et métadonnées filer dans `histae-object-storage-production-data`.
Cela fournit une persistance mono-machine, pas de haute disponibilité ni de sauvegarde automatique.

Compose donne `HISTAE_STORAGE_HOST` comme alias privé à la passerelle et impose
`OBJECT_STORAGE_ENDPOINT=https://<HISTAE_STORAGE_HOST>` aux clients serveur. Le même nom doit être publié via
le tunnel/reverse proxy pour les navigateurs et mobiles, en conservant exactement le Host et le chemin des
requêtes signées. Router ce domaine vers `https://storage-gateway:443`, avec le nom TLS d'origine réglé sur
`HISTAE_STORAGE_HOST` et vérification du certificat activée. L'API reste routée vers `http://api:8080`.
Ne publier aucune interface SeaweedFS brute. Les SMS, le push et Stripe restent des fournisseurs externes.

Valider sans afficher la configuration résolue :

```bash
docker compose --env-file .env.production \
  -f compose.yaml \
  -f compose.production.yaml \
  config --quiet
```

Puis déployer :

```bash
docker compose --env-file .env.production \
  -f compose.yaml \
  -f compose.production.yaml \
  up -d --wait
```

Le tunnel doit cibler `http://api:8080` depuis `histae-edge` et la passerelle HTTPS pour le domaine S3. PostgreSQL, Redis, SeaweedFS et le listener
9091 ne doivent pas rejoindre ce réseau. Un tunnel ne remplace ni WebAuthn, ni les guards, ni la configuration
précise de `TRUST_PROXY`.

### Planifier la maintenance sans composant supplémentaire

Le service `maintenance` est volontairement une commande à exécution unique. Sur Debian, un timer `systemd` peut
l’appeler toutes les cinq minutes sans introduire de scheduler applicatif supplémentaire. Le service doit exécuter
depuis le dossier de déploiement :

```ini
[Service]
Type=oneshot
WorkingDirectory=/opt/histae-api
ExecStart=/usr/bin/docker compose --env-file .env.production -f compose.yaml -f compose.production.yaml --profile jobs run --rm --no-deps maintenance
```

Le timer associé peut utiliser `OnCalendar=*:0/5` et `Persistent=true`. Tester manuellement la commande, les logs et
les métriques de retard avant d’activer le timer.

## PostgreSQL : persistance, sauvegarde et mise à niveau

Les volumes `histae-postgres-data` (développement) et `histae-postgres-production-data` (production) protègent
seulement contre la recréation d’un conteneur. Ils ne protègent pas contre
une panne de disque, une suppression de volume, une corruption, un chiffrement malveillant ou la perte de la
machine.

Avant toute production :

1. fixer les objectifs de perte et de reprise ;
2. sauvegarder vers un autre support ou une autre machine ;
3. chiffrer et contrôler l’accès aux sauvegardes ;
4. restaurer réellement dans un environnement isolé ;
5. ajouter l’archivage WAL/PITR si un simple dump périodique perdrait trop de données ;
6. tester toute montée de version majeure avec `pg_upgrade` ou export/restauration avant de changer l’image.

Ne jamais sauvegarder un volume PostgreSQL actif par une copie de fichiers arbitraire. Utiliser les outils
PostgreSQL prévus et conserver les preuves de restauration sans donnée personnelle dans les logs du dépôt.

## Limites restantes

Cette livraison fournit le packaging et l’orchestration, pas la haute disponibilité. Une seule machine demeure un
point de panne unique. La cible S3 durable, les sauvegardes PostgreSQL restaurées, la rotation réelle des
secrets, le canal d’alertes et les tests de charge/sécurité restent suivis dans [les critères de bascule](migration/s29-cutover.md).
