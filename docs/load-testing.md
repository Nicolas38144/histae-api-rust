# Test de charge de l’API avec k6 et Cloudflare

[Index](README.md) · [Développement](development.md) · [Supervision](observability.md) · [SQL](sql-performance.md)

Ce tutoriel prépare une pile de test, 5 000 comptes fictifs et des sessions temporaires sans OTP.
Il mesure d’abord l’API par une connexion privée, puis le parcours Cloudflare → tunnel → Nginx → API.
Les commandes sont à exécuter manuellement ; aucune charge n’est lancée en lisant ce guide.

Une première mesure sur l’ordinateur de développement est disponible dans
[Résultats du test local](load-testing-results.md), avec les limites et le diagnostic observés.
La [validation approfondie](load-testing-validation.md) décrit les répétitions à débit constant,
les paliers conditionnels, les contrôles de sécurité sous charge et les variantes du feed.

## 1. Choisir les machines et le périmètre

- **A : serveur de test**, avec Docker Compose et le projet à jour.
- **B : générateur**, ton poste Debian par exemple, avec Docker et un accès SSH à A.
- **Production :** conserver ses données, ses secrets et son tunnel existant.

Une VM ou un serveur distinct est préférable pour A. Sur le même serveur physique que la production,
la pile de test partage CPU, RAM et disque : réserver une période sans utilisateurs avant une charge importante.
Un nouveau réseau et une nouvelle base n’isolent pas ces ressources physiques.

Le scénario fourni teste uniquement les lectures mobiles : feed, liste des matchs et conversations.
Il ne représente pas encore les uploads de photos, les connexions SSE persistantes, les écritures ni les
appels aux fournisseurs. La préparation des données utilise les routes de swipe, message et signalement,
mais cette préparation n’est pas une mesure de capacité.

Les limites CPU/RAM de `compose.dev.yaml`, PostgreSQL sans TLS et les fournisseurs désactivés diffèrent de
la production. Les résultats décrivent cette pile. Pour annoncer une capacité de production, reproduire
ses ressources, ses données et ses paramètres PostgreSQL/Redis, puis valider la livraison réelle.

## 2. Vérifier qu’aucune autre pile de développement n’occupe le projet — A

```bash
sudo docker ps -a \
  --filter label=com.docker.compose.project=histae-rust-dev \
  --format 'table {{.Names}}\t{{.Status}}'
sudo docker volume ls \
  --filter label=com.docker.compose.project=histae-rust-dev
```

Le tutoriel utilise le projet `histae-rust-dev`, attendu par les outils de données fictives.
**Si ces commandes listent une pile ou des volumes contenant des données réelles, utiliser une autre machine ou VM.**
Changer uniquement le dossier ne crée pas un nouveau projet Compose et ne protège pas ses volumes.
Pour reprendre une pile de test déjà préparée, conserver son `.env`, ses secrets et ses volumes ;
reprendre à l’étape 5 au lieu de recréer des secrets qui ne correspondraient plus au PostgreSQL existant.

## 3. Préparer un dossier dédié — A

Installer les outils manquants :

```bash
sudo apt-get update
sudo apt-get install python3 rsync curl jq openssl
```

Si le projet à jour se trouve dans `~/Projects/Histae/histae-api-rust`, le recopier en excluant les secrets,
les résultats et les artefacts de compilation :

```bash
install -d -m 700 "$HOME/Projects/Histae/histae-load-test"
rsync -a \
  --exclude='.git/' --exclude='.env' --exclude='.env.*' \
  --exclude='.secrets/' --exclude='.load-test/' \
  --exclude='target/' --exclude='node_modules/' \
  "$HOME/Projects/Histae/histae-api-rust/" \
  "$HOME/Projects/Histae/histae-load-test/"
cd "$HOME/Projects/Histae/histae-load-test"
```

Si le dépôt est sur une autre machine, transférer les mêmes fichiers avec ces exclusions.
La copie doit inclure `scripts/load-test.js` et `scripts/prepare-dev-load-test.py`.

## 4. Créer un environnement de test avec de nouveaux secrets — A

Dans le dossier de test, **sans `.env` existant**, exécuter :

```bash
(
set -e
set -o noclobber
test ! -e .env
umask 077
cat > .env <<EOF
ENV=development
PORT=8080
HISTAE_ENV_FILE=.env
HISTAE_API_IMAGE=histae-api:load-test
HISTAE_BACKEND_NETWORK=histae-load-backend
HISTAE_API_BIND_IP=127.0.0.1
HISTAE_API_HOST_PORT=18080

JWT_SECRET=$(openssl rand -hex 32)
JWT_ACTIVE_KID=load-test
JWT_ACCESS_TTL=60m
PHONE_ENCRYPTION_KEY=$(openssl rand -hex 32)
PHONE_HASH_KEY=$(openssl rand -hex 32)
POSTGRES_HOST=127.0.0.1
POSTGRES_PORT=15433
POSTGRES_DB=histae-dev
POSTGRES_USER=histae
POSTGRES_PASSWORD=$(openssl rand -hex 32)
POSTGRES_SSLMODE=disable
POSTGRES_POOL_MAX=20
HISTAE_API_POSTGRES_POOL_MAX=20

RATE_LIMIT_STORE=redis
RATE_LIMIT_GLOBAL=20000
RATE_LIMIT_GLOBAL_WINDOW=1m
REDIS_ADDR=127.0.0.1:6379
REDIS_PASSWORD=
REDIS_TLS=false
SMS_PROVIDER=disabled
BILLING_PROVIDER=disabled
PUSH_PROVIDER=disabled
PHOTO_MODERATION_PROVIDER=local_http
PHOTO_MODERATION_ENDPOINT=http://127.0.0.1:8090
PHOTO_MODERATION_TOKEN=$(openssl rand -hex 32)
OBJECT_STORAGE_ENDPOINT=http://storage.histae.localhost:8333
OBJECT_STORAGE_ACCESS_KEY=histae-load
OBJECT_STORAGE_SECRET_KEY=$(openssl rand -hex 32)
OBJECT_STORAGE_BUCKET=histae-load-photos

ADMIN_WEBAUTHN_ORIGIN=http://localhost:5173
ADMIN_WEBAUTHN_RP_ID=localhost
CORS_ORIGINS=http://localhost:5173
TRUST_PROXY=false
MAINTENANCE_MODE=disabled
METRICS_ENABLED=true
METRICS_TOKEN=$(openssl rand -hex 32)
EOF
chmod 600 .env
)
```

Le sous-shell Bash s’arrête si `.env` existe et interdit son écrasement.
Le listener Prometheus reste dans le réseau Docker ; aucun port 9091 n’est publié ici.

Le quota global normal est 100/minute/IP. Plusieurs comptes sur une machine génératrice partagent cette IP.
20 000/minute permettent ici les paliers jusqu’à 100 RPS, avec une marge pour la préparation.
Les quotas individuels restent actifs : le scénario répartit les requêtes entre 5 000 comptes.
Ne pas appliquer ces valeurs à l’environnement de production.

## 5. Construire et démarrer la pile — A

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml config --quiet
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  up -d --build --wait --wait-timeout 240
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml ps
curl --fail --max-time 5 http://127.0.0.1:18080/health/ready
```

Résultat attendu : `{"status":"ready"}`. En cas d’échec, consulter les logs avant de poursuivre :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  logs --tail=100 migrate api outbox-worker
```

Ne pas supprimer de volumes pour résoudre un incident. Le [guide de déploiement](container-deployment.md)
explique les dépendances, les ports et les diagnostics de démarrage.

## 6. Créer les comptes, matchs et messages — A

```bash
sudo bash scripts/seed-dev-load.sh 5000
```

Cette étape peut durer plusieurs minutes. Elle crée les comptes fictifs dans PostgreSQL puis utilise
les routes existantes pour les swipes, conversations, continuations et signalements. Elle ne demande pas
d’OTP et ne livre aucun SMS. Les fournisseurs SMS, Stripe et push sont désactivés dans cette pile.

Le succès doit se terminer par `API load seed complete: 5000 users, 5000 matches.`
Une relance reprend les données existantes. Le script réessaie certaines erreurs pendant le peuplement :
sa durée ne permet donc pas de calculer la capacité de l’API.

## 7. Préparer les sessions du benchmark — A

```bash
install -d -m 700 .load-test
sudo python3 scripts/prepare-dev-load-test.py 5000
sudo chown -R "$(id -u):$(id -g)" .load-test
chmod 600 .load-test/accounts.json
```

L’outil vérifie le projet Docker, `ENV=development`, la base `histae-dev` et les fournisseurs désactivés.
Il crée des familles de sessions temporaires pour les UUID fictifs, choisit une conversation confirmée
contenant des messages par compte et rafraîchit les présences fictives.
Les comptes supprimés, bannis ou devenus administrateurs sont exclus, avec au moins 1 000 comptes actifs requis.
Les arguments `--api-service load-api --compose-file CHEMIN` permettent de cibler une API temporaire
du même projet de développement ; ses clés JWT peuvent ainsi être distinctes de celles de l’API habituelle.

`.load-test/accounts.json` contient des **jetons d’accès sensibles**, jamais la clé de signature JWT.
La durée est celle de `JWT_ACCESS_TTL`, ici 60 minutes. Le dossier est ignoré par Git.
Ne pas afficher le JSON dans un terminal partagé ni l’envoyer dans une collection Postman publique.

## 8. Préparer le générateur et k6 — B

Remplacer l’adresse SSH avant d’exécuter les commandes. Les chemins distants ci-dessous sont relatifs
au dossier personnel du compte SSH :

```bash
LOAD_SERVER='walagaine@IP_PRIVEE_DU_SERVEUR_TEST'
install -d -m 700 "$HOME/histae-load-client"
cd "$HOME/histae-load-client"
install -d -m 700 scripts .load-test
scp "$LOAD_SERVER:Projects/Histae/histae-load-test/scripts/load-test.js" scripts/load-test.js
scp "$LOAD_SERVER:Projects/Histae/histae-load-test/.load-test/accounts.json" .load-test/accounts.json
chmod 600 .load-test/accounts.json

sudo docker pull grafana/k6
K6_IMAGE=$(sudo docker image inspect grafana/k6 --format '{{index .RepoDigests 0}}')
sudo docker run --rm "$K6_IMAGE" version
```

L’image est sélectionnée par son digest pour utiliser la même version pendant toutes les comparaisons.
La [documentation officielle k6](https://grafana.com/docs/k6/latest/set-up/install-k6/) décrit aussi
l’installation native si tu préfères éviter Docker sur B.

Créer cette fonction dans **le même terminal Bash** :

```bash
k6_test() {
  sudo docker run --rm --network host \
    --user "$(id -u):$(id -g)" \
    --mount "type=bind,src=$PWD,dst=/work" --workdir /work \
    "$K6_IMAGE" run "$@" scripts/load-test.js
}
```

Le mode `--network host` de Docker Linux permet au conteneur k6 d’utiliser le tunnel SSH local.
Le montage donne accès au scénario, aux jetons et au dossier où enregistrer les résumés.
Recréer la fonction et `K6_IMAGE` après l’ouverture d’un nouveau terminal.

## 9. Ouvrir l’accès privé à l’API — B

Dans **un second terminal sur B**, remplacer la même adresse puis laisser cette commande tourner :

```bash
LOAD_SERVER='walagaine@IP_PRIVEE_DU_SERVEUR_TEST'
ssh -N -o ExitOnForwardFailure=yes \
  -L 127.0.0.1:18080:127.0.0.1:18080 "$LOAD_SERVER"
```

Dans le terminal du générateur :

```bash
curl --fail --max-time 5 http://127.0.0.1:18080/health/ready
```

Le port sur B redirige vers le port loopback de A. Aucun accès public direct à l’API n’est nécessaire.
Le trajet SSH ajoute du chiffrement et de la latence ; surveiller aussi son CPU et le réseau.
Si ce trajet sature, refaire la mesure sur un LAN/VPN privé suffisamment rapide avant de conclure à une
limite du serveur. Pour une vérification locale sur A, k6 peut utiliser directement le même URL, mais
le générateur consomme alors les ressources du serveur.

## 10. Vérifier le scénario à très faible charge — B

```bash
k6_test \
  --env BASE_URL=http://127.0.0.1:18080 \
  --env PROFILE=smoke \
  --summary-export=.load-test/direct-smoke.json
```

Le scénario commence par vérifier l’identité du premier compte, un feed non vide, ses matchs et sa
conversation. Il envoie ensuite **1 requête/seconde pendant 20 secondes**.
Une redirection, une erreur d’authentification ou une page HTML Cloudflare ne compte pas comme une réussite.
Les réponses JSON doivent contenir la collection attendue et au moins un élément.

Le JSON de résumé contient des métriques, sans jetons ni corps de réponse.

## 11. Ouvrir la supervision avant la montée en charge — A

Dans un autre terminal sur A :

```bash
sudo docker stats
```

Regarder les conteneurs `histae-rust-dev-*`, le générateur sur B et les autres services du serveur si
la machine est partagée. CPU, mémoire et entrées/sorties doivent être suivis pendant chaque palier.

Prendre aussi un instantané des métriques privées avant et après chaque test :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  exec -T api node -e '
fetch("http://127.0.0.1:9091/metrics", {
  headers: { Authorization: "Bearer " + process.env.METRICS_TOKEN }
}).then(async r => {
  if (!r.ok) throw new Error("HTTP " + r.status);
  console.log(await r.text());
}).catch(e => { console.error(e.message); process.exitCode = 1; });
' > .load-test/metrics-before.prom
```

Rejouer avec `metrics-after.prom` comme nom après le test. Le secret reste dans le conteneur.
Pour des courbes continues de latence, pool SQL et outbox, suivre le [guide de supervision](observability.md)
avec le réseau backend **`histae-load-backend`** et le secret de cette pile.

Pour un diagnostic SQL pendant un palier :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  exec -T postgres sh -c 'psql -X -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "
SELECT state, wait_event_type, count(*)
FROM pg_stat_activity WHERE datname = current_database()
GROUP BY state, wait_event_type ORDER BY count(*) DESC;
"'
```

Pour vérifier l’état de l’outbox après le peuplement :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  exec -T postgres sh -c 'psql -X -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "
SELECT status, count(*) FROM outbox_event GROUP BY status ORDER BY status;
SELECT now() - min(created_at) AS oldest_pending
FROM outbox_event WHERE status IN ('\''pending'\'', '\''processing'\'');
"'
```

## 12. Mesurer la capacité en lecture — B

```bash
k6_test \
  --env BASE_URL=http://127.0.0.1:18080 \
  --env PROFILE=capacity \
  --env TARGET_RPS=100 \
  --summary-export=.load-test/direct-capacity.json
```

Le profil dure **13 minutes 30 secondes**, hors vérifications et fin des requêtes en cours :

| Palier | Montée | Maintien |
| --- | --- | --- |
| 10 RPS | 30 s | 2 min |
| 25 RPS | 30 s | 2 min |
| 50 RPS | 30 s | 2 min |
| 100 RPS | 30 s | 5 min |
| Retour à zéro | 30 s | — |

Une itération envoie exactement une requête métier. Le mélange est **60 % feed, 20 % matchs, 20 % messages**.
Il est réparti sur les comptes, sans retries automatiques. Le débit de scénario correspond donc au RPS
visé hors des quatre requêtes de vérification initiale. Le débit atteint reste à lire dans les résultats.
Le [modèle d’arrivée k6](https://grafana.com/docs/k6/latest/using-k6/scenarios/executors/ramping-arrival-rate/)
continue à proposer la charge lorsque les réponses ralentissent, tant que des utilisateurs virtuels sont disponibles.

Pour mesurer uniquement le feed, refaire une préparation fraîche puis utiliser :

```bash
k6_test \
  --env BASE_URL=http://127.0.0.1:18080 \
  --env PROFILE=capacity --env WORKLOAD=feed \
  --env TARGET_RPS=100 \
  --summary-export=.load-test/direct-feed.json
```

Le scénario arrête les profils longs si, après 30 secondes, les erreurs HTTP atteignent 1 % ou les
vérifications échouent trop souvent, ou si des itérations sont perdues. Il s’arrête aussi si le p95
dépasse l’objectif après une minute. `Ctrl+C` reste disponible pour arrêter immédiatement une dégradation.
Les seuils initiaux sont **p95 < 750 ms** et **p99 < 1 000 ms**, pour l’ensemble et chaque route
du mélange. `--env P95_MS=500 --env P99_MS=800` permet de choisir d’autres objectifs explicites.
Les réponses métier invalides doivent être absentes ; les codes d’erreur sont comptés dans
`histae_error_codes`, avec des labels bornés et sans corps de réponse ni identifiant personnel.

`--env ABORT_ON_THRESHOLD_FAILURE=false` laisse terminer un profil qui dépasse les seuils,
mais son résultat reste un échec. Réserver cette option à une campagne supervisée avec un watchdog
indépendant qui arrête la charge en cas de panne de conteneur. Les seuils ne sont pas retirés.

## 13. Lire les résultats et décider du palier suivant

| Mesure k6 | Interprétation |
| --- | --- |
| `http_reqs` | Requêtes réellement envoyées ; la moyenne inclut les montées et descentes. |
| `http_req_duration` p95/p99 | Temps sous lequel passent 95 % / 99 % des requêtes ; vérifier aussi chaque route dans la supervision. |
| `http_req_failed` | Erreurs HTTP et réseau. |
| `checks` | Vérification du statut, de l’identité et des collections JSON non vides. |
| `dropped_iterations` | Charge prévue qui n’a pas pu démarrer ; elle doit être nulle pour valider le débit demandé. |
| `histae_http_429` | Requêtes limitées ; vérifier les quotas API et Cloudflare. |
| `histae_http_5xx` | Erreurs serveur, éventuellement émises par un intermédiaire. |
| `histae_cloudflare_challenge` | Réponses portant `cf-mitigated: challenge`. |
| `histae_invalid_payload` | Réponses qui ne correspondent pas au résultat métier attendu. |

Un test terminé avec un seuil échoué renvoie un code de sortie non nul. Un `200` avec un feed vide
n’est pas une preuve de performance représentative. Des `401` indiquent notamment une session expirée,
une mauvaise instance ou des secrets différents, et doivent être corrigés avant toute conclusion sur le RPS.

Si `dropped_iterations` est non nul, vérifier la saturation du générateur et la latence.
Le scénario réserve 100 utilisateurs virtuels : augmenter cette réserve si nécessaire avec
`--env PREALLOCATED_VUS=200`, puis répéter le test. Une itération perdue peut aussi être le symptôme
d’un serveur qui répond trop lentement ; l’ajout de VU ne résout pas cette cause.

Ne pas annoncer « 100 RPS supportés » à partir de la moyenne globale d’un test en paliers.
Confirmer ce débit pendant un plateau suffisamment long, sans pertes d’itérations, avec les objectifs
de latence et d’erreur respectés et les ressources surveillées.

## 14. Recréer des sessions fraîches entre les campagnes — A, puis B

Sur A :

```bash
sudo python3 scripts/prepare-dev-load-test.py --cleanup
sudo python3 scripts/prepare-dev-load-test.py 5000
sudo chown -R "$(id -u):$(id -g)" .load-test
```

Sur B, recopier le fichier avant de relancer k6 :

```bash
scp "$LOAD_SERVER:Projects/Histae/histae-load-test/.load-test/accounts.json" .load-test/accounts.json
chmod 600 .load-test/accounts.json
```

La préparation remet aussi les présences fictives à jour. Elles expirent au bout d’une heure ; les
anciens jetons et les anciens fichiers de B ne doivent plus être utilisés après nettoyage sur A.
Le scénario refuse de démarrer si les jetons ne couvrent pas la durée du profil avec deux minutes de marge.

## 15. Préparer un tunnel Cloudflare réservé au test

Créer **un tunnel nommé distinct** et un sous-domaine, par exemple `api-charge.histae.com`, dans
Cloudflare. Utiliser le parcours de création d’un connecteur `cloudflared`, puis choisir Docker.
Conserver le token du nouveau tunnel pour l’étape suivante.

La configuration de l’application publiée doit être :

- Nom d’hôte : `api-charge.histae.com`.
- Type de service : HTTP.
- URL du service : `load-proxy:80`.

Ce nom Docker désignera le Nginx de test. Le tunnel, le DNS et le sous-domaine de production restent
associés à leurs services existants. Les libellés du tableau de bord peuvent évoluer : la
[procédure officielle](https://developers.cloudflare.com/tunnel/get-started/) décrit la création d’un tunnel nommé.

## 16. Ajouter Nginx et le connecteur de test — A

Créer le fichier du token dans le dossier privé, sans l’inscrire dans l’historique du shell :

```bash
read -rsp 'Token du tunnel Cloudflare de test : ' LOAD_TUNNEL_TOKEN
printf '\n'
printf '%s' "$LOAD_TUNNEL_TOKEN" > .load-test/tunnel-token
unset LOAD_TUNNEL_TOKEN
chmod 444 .load-test/tunnel-token
```

Le fichier doit être lisible par l’utilisateur du conteneur Cloudflare. Le dossier `.load-test` reste
en mode 700 sur l’hôte ; le secret est monté uniquement dans ce connecteur, en lecture seule.

Créer le proxy :

```bash
cat > .load-test/nginx.conf <<'NGINX'
set_real_ip_from 172.31.240.5/32;
real_ip_header CF-Connecting-IP;
real_ip_recursive off;
map "$realip_remote_addr:$http_x_forwarded_proto" $load_forwarded_proto {
    default $scheme;
    "172.31.240.5:https" https;
}
resolver 127.0.0.11 valid=30s ipv6=off;
server_tokens off;
server {
    listen 80;
    server_name api-charge.histae.com;
    location = /healthz { access_log off; return 204; }
    location / {
        set $load_api_upstream "http://api:8080";
        proxy_pass $load_api_upstream;
        proxy_set_header Host $proxy_host;
        proxy_set_header X-Real-IP $remote_addr;
        proxy_set_header X-Forwarded-Host $host;
        proxy_set_header X-Forwarded-For $remote_addr;
        proxy_set_header X-Forwarded-Proto $load_forwarded_proto;
        proxy_http_version 1.1;
        proxy_set_header Connection "";
        proxy_connect_timeout 5s;
        proxy_read_timeout 60s;
    }
}
NGINX

cat > .load-test/compose.public.yaml <<'YAML'
services:
  api:
    environment:
      TRUST_PROXY: "172.31.240.4/32"
    networks:
      backend:
      load-edge:
  load-proxy:
    image: nginx:1.28-alpine
    volumes:
      - ./.load-test/nginx.conf:/etc/nginx/conf.d/default.conf:ro
    depends_on:
      api: { condition: service_healthy }
    networks:
      load-edge:
        ipv4_address: 172.31.240.4
    restart: unless-stopped
  load-cloudflared:
    image: cloudflare/cloudflared:latest
    command: [tunnel, --no-autoupdate, run, --token-file, /run/secrets/load_tunnel_token]
    secrets:
      - load_tunnel_token
    depends_on:
      - load-proxy
    networks:
      load-edge:
        ipv4_address: 172.31.240.5
    restart: unless-stopped
secrets:
  load_tunnel_token:
    file: ./.load-test/tunnel-token
networks:
  load-edge:
    name: histae-load-edge
    ipam:
      config:
        - subnet: 172.31.240.0/24
          ip_range: 172.31.240.128/25
YAML

sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  -f .load-test/compose.public.yaml config --quiet
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  -f .load-test/compose.public.yaml up -d --wait --wait-timeout 240
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  -f .load-test/compose.public.yaml logs --tail=50 load-cloudflared
```

Nginx utilise ici la version de l’image du reverse proxy du projet. Les chemins de montage du troisième
manifest sont résolus depuis le premier manifest `compose.yaml`, donc depuis la racine du dossier de test.
Si Docker signale un chevauchement de sous-réseau, choisir un réseau libre et modifier ensemble son CIDR,
les deux IP fixes, `set_real_ip_from`, la clé de `map` et `TRUST_PROXY`.

Le connecteur doit apparaître connecté dans Cloudflare. `--wait` ne vérifie pas à lui seul la connexion
du tunnel : les logs et la requête HTTPS suivante sont nécessaires.
Le paramètre `--token-file` est décrit dans les [options du connecteur](https://developers.cloudflare.com/tunnel/reference/run-parameters/).
À partir de cette étape, conserver le troisième `-f` pour toute commande de recréation ou d’arrêt de la pile.
Les outils de préparation utilisent seulement `exec`/`ps` sur le même projet ; ils restent compatibles.

## 17. Vérifier les règles Cloudflare et le chemin public — B

```bash
curl --fail --max-time 10 https://api-charge.histae.com/health/ready
k6_test \
  --env BASE_URL=https://api-charge.histae.com \
  --env PROFILE=smoke \
  --summary-export=.load-test/cloudflare-smoke.json
```

Vérifier que les règles de cache n’enregistrent pas les réponses personnalisées du sous-domaine.
Pour ce test, créer si nécessaire une règle de cache **Bypass cache** avec le nom d’hôte
`api-charge.histae.com`. Un cache devant les réponses authentifiées invaliderait la mesure du serveur
et pourrait exposer des données. Consulter les [règles de cache](https://developers.cloudflare.com/cache/how-to/cache-rules/).

Si le smoke est bloqué, consulter les événements de sécurité Cloudflare pour l’IP du générateur et ce
nom d’hôte. Un `429` peut venir de Cloudflare ou de l’API ; le JSON API contient notamment
`rate_limit_exceeded` ou `feed_rate_limit_exceeded`.
Une réponse avec `cf-mitigated: challenge` est un challenge Cloudflare,
[identifiable par cet en-tête](https://developers.cloudflare.com/cloudflare-challenges/challenge-types/challenge-pages/detect-response/).

Si une protection bloque le trafic légitime, créer une règle temporaire `Skip`, limitée à :

```text
(http.host eq "api-charge.histae.com" and ip.src eq IP_PUBLIQUE_DU_GENERATEUR)
```

Remplacer l’IP par l’adresse **publique** de B, vue dans les événements Cloudflare, pas par son IP LAN.
Sélectionner seulement les protections responsables, puis supprimer cette exception après la campagne.
Les options dépendent de ton offre. **Bot Fight Mode ne peut pas être sauté par une règle WAF Skip** ;
ne pas supposer qu’une exception couvre tous les mécanismes de protection.
Voir les [options Cloudflare](https://developers.cloudflare.com/waf/custom-rules/skip/).

Conserver deux observations distinctes : le comportement avec les protections habituelles, puis,
si une exception est nécessaire, les performances du chemin public avec cette exception documentée.

## 18. Comparer le même scénario via le tunnel — B

Préparer et recopier des sessions fraîches selon l’étape 14, puis lancer :

```bash
k6_test \
  --env BASE_URL=https://api-charge.histae.com \
  --env PROFILE=capacity --env TARGET_RPS=100 \
  --summary-export=.load-test/cloudflare-capacity.json
```

Suivre aussi CPU/RAM de `load-proxy` et `load-cloudflared` avec `docker stats` sur A.
Comparer les erreurs, le débit atteint, les latences et les métriques API pour le même jeu de données.
Les différences incluent le réseau, SSH pour le test direct, TLS public, Cloudflare, Nginx et le tunnel :
elles ne mesurent pas uniquement le coût de `cloudflared`.

Cloudflare ne publie pas de plafond RPS universel pour les tunnels nommés dans ses
[limites de compte](https://developers.cloudflare.com/cloudflare-one/account-limits/).
La capacité du connecteur et les protections configurées peuvent néanmoins limiter le trafic.
Si des protections bloquent la charge, les requêtes bloquées ne constituent pas une mesure de la capacité Rust/SQL.

## 19. Confirmer un débit stable pendant 30 minutes — B

Choisir le dernier débit validé. Après une nouvelle préparation/copie des sessions :

```bash
k6_test \
  --env BASE_URL=https://api-charge.histae.com \
  --env PROFILE=soak --env TARGET_RPS=50 --env SOAK_MINUTES=30 \
  --summary-export=.load-test/cloudflare-soak-50.json
```

Ce profil monte à 50 RPS en une minute, maintient 30 minutes, puis descend pendant 30 secondes.
Utiliser l’URL privée pour confirmer d’abord le serveur seul. Surveiller une hausse progressive de la
mémoire, de la latence, des attentes SQL ou du retard de l’outbox. Ne pas dépasser la fraîcheur des
présences : le profil borne le maintien à 45 minutes et refuse des jetons trop proches de leur expiration.

Pour tester un débit supérieur à 100, changer `TARGET_RPS`, vérifier la réserve de VU et le quota global
avant de lancer. Le plafond du scénario est 1 000 RPS ; 20 000 requêtes/minute ne suffisent pas au-delà
d’environ 333 RPS moyens par IP. Dimensionner la limite avec une marge, sans la modifier en production.

Vérifier également la limite `nofile` effective : le service API commun la fixe à 16 384 pour les limites
souple et dure. Une configuration modifiée demande une recréation du conteneur, selon le
[guide Docker](container-deployment.md#developpement-local). Les sockets des utilisateurs virtuels
et les connexions aux dépendances partagent ce budget. Relever leur pic pendant la charge et leur
libération à la déconnexion ; le nombre de clients connectés constitue une dimension distincte du RPS.

## 20. Nettoyer les accès temporaires

Sur **A**, révoquer uniquement les sessions de cette campagne :

```bash
cd "$HOME/Projects/Histae/histae-load-test"
sudo python3 scripts/prepare-dev-load-test.py --cleanup
```

Sur **B**, supprimer la copie des jetons puis fermer le terminal SSH avec `Ctrl+C` :

```bash
rm -- .load-test/accounts.json
```

Les comptes, matchs et messages fictifs restent disponibles pour les campagnes suivantes.
La commande de nettoyage ne supprime pas les utilisateurs et ne révoque pas leurs autres sessions.
Si une préparation est interrompue brutalement avant d’écrire le fichier, les sessions restantes
expirent au plus tard après leur durée bornée ; ne pas utiliser une purge globale de comptes pour les nettoyer.

Sur **A**, arrêter la pile avec sa sélection complète de manifests :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  -f .load-test/compose.public.yaml down
rm -- .load-test/tunnel-token
```

Si la partie Cloudflare n’a pas été exécutée, utiliser les deux premiers manifests seulement et ne pas
supprimer un fichier token absent. Ne pas ajouter `--volumes` : les données de test sont conservées.
Supprimer dans Cloudflare l’exception temporaire, le sous-domaine/DNS de test et le tunnel de test.
Le tunnel de production n’est pas concerné.

## 21. Conserver une conclusion reproductible

Garder les résumés JSON, les instantanés de métriques et une note contenant :

- Commit testé, versions/digests Docker, CPU/RAM, emplacement du générateur et trajet réseau.
- Nombre de comptes, matchs/messages et paramètres SQL/Redis/pool.
- Scénario, proportions des routes, quotas, paliers et durée du plateau validé.
- Débit atteint, p95/p99, erreurs, itérations perdues, ressources et protections Cloudflare appliquées.

Une conclusion utile ressemble à : « Sur cette pile, le scénario de lecture 60/20/20 a tenu X RPS
pendant 30 minutes, avec p95 Y ms, Z % d’erreurs et aucune itération perdue. »
Elle ne prédit pas encore la capacité en upload photo, en écriture ou en connexions SSE.
Ajouter ces scénarios séparément, avec des cibles de swipe uniques et des clés d’idempotence distinctes,
puis valider un mélange correspondant à l’usage mobile réel.
