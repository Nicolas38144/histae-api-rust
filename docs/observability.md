# Supervision et runbooks

Ce guide couvre l’export Prometheus privé de l’API, la pile locale Prometheus/Alertmanager/Grafana, les seuils
initiaux et la conduite à tenir. Les seuils sont des points de départ à recalibrer avec les mesures de charge de
R12. Ils ne doivent jamais introduire un label utilisateur, un identifiant métier ou une donnée personnelle.

## Architecture et sécurité

L’API ouvre, uniquement lorsque `METRICS_ENABLED=true`, un second serveur HTTP dédié sur `METRICS_HOST:METRICS_PORT`.
Il ne fait pas partie du contrat `/api`, n’accepte que `GET /metrics` et exige `Authorization: Bearer …`. Le secret
`METRICS_TOKEN` fait au moins 32 octets, est distinct des clés JWT/téléphone et n’est ni journalisé ni exposé dans
les métriques. Une collecte PostgreSQL en échec laisse disponibles les compteurs mémoire et publie
`histae_metrics_collection_success 0`.

Le serveur applicatif principal ne publie donc aucun endpoint Prometheus. Deux raccordements locaux sont pris en
charge : Prometheus joint une API exécutée directement sur Debian via `host.docker.internal:9091`, ou l’API
conteneurisée via `api:9091` et le réseau privé `histae-backend`. Dans les deux cas, les trois interfaces de la
pile sont liées à `127.0.0.1` :

| Interface | URL locale | Authentification |
| --- | --- | --- |
| Grafana | `http://localhost:3001` | `histae-admin` et le secret fichier Grafana |
| Prometheus | `http://localhost:9090` | loopback local, ne pas exposer |
| Alertmanager | `http://localhost:9093` | loopback local, ne pas exposer |

Le compose local ne configure aucun destinataire de notification : les alertes restent visibles dans Grafana,
Prometheus et Alertmanager. Avant la production, placer ces interfaces sur un réseau d’administration authentifié,
renouveler les secrets et raccorder Alertmanager à un canal d’astreinte approuvé. Ne jamais publier le port 9091
dans Cloudflare ni sur Internet. Si l’API et Prometheus partagent un réseau privé en production, lier le listener à
l’adresse de ce réseau plutôt qu’à toutes les interfaces.

## Démarrage local

Créer d’abord les secrets Docker basés sur des fichiers :

```bash
install -d -m 700 .secrets
openssl rand -hex 32 | tr -d '\n' > .secrets/histae_metrics_token
openssl rand -base64 32 | tr -d '\n' > .secrets/histae_grafana_admin_password
chmod 600 .secrets/histae_metrics_token .secrets/histae_grafana_admin_password
```

Dans `.env`, définir au minimum `METRICS_TOKEN` avec le contenu exact de
`.secrets/histae_metrics_token` :

```ini
METRICS_ENABLED=true
METRICS_HOST=0.0.0.0
METRICS_PORT=9091
METRICS_TOKEN=<secret-aléatoire-dédié-de-32-octets-minimum>
```

Si l’API s’exécute directement sur l’hôte, la redémarrer puis lancer la pile depuis la racine du dépôt :

```bash
docker compose --env-file .env -f docker-compose.observability.yml up -d --wait
docker compose --env-file .env -f docker-compose.observability.yml ps
```

Si l’API utilise `compose.yaml` et `compose.dev.yaml`, recréer d’abord son conteneur puis appliquer l’override qui
raccorde Prometheus au réseau backend sans publier le port 9091 :

```bash
docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d api
docker compose --env-file .env \
  -f docker-compose.observability.yml \
  -f compose.observability-container.yaml \
  up -d --wait
```

Utiliser les mêmes deux options `-f` dans les commandes `ps`, `exec prometheus` et `down` tant que ce mode est
actif. Le fichier `prometheus.container.yml` cible `api:9091`; la configuration native cible
`host.docker.internal:9091`.

Vérifier ensuite la configuration et les règles dans le conteneur épinglé :

```bash
docker compose --env-file .env -f docker-compose.observability.yml exec prometheus \
  promtool check config /etc/prometheus/prometheus.yml
docker compose --env-file .env -f docker-compose.observability.yml exec prometheus \
  promtool check rules /etc/prometheus/alerts.yml
docker compose --env-file .env -f docker-compose.observability.yml exec prometheus \
  promtool test rules /etc/prometheus/alerts.test.yml
```

Dans Prometheus, `Status > Target health` doit montrer `histae-api` à `UP`. Un `401` indique presque toujours que
le token chargé par Compose diffère de celui de l’API ; un refus de connexion indique l’API arrêtée, les métriques
désactivées, une mauvaise interface d’écoute ou un filtrage réseau sur l’hôte.

Le dashboard provisionné `Histae / Histae Operations` affiche disponibilité, alertes, HTTP, dépendances, pool
PostgreSQL, outbox et maintenance. Les compteurs HTTP/dépendances repartent à zéro après un redémarrage de l’API ;
les fonctions Prometheus `rate` et `increase` savent gérer ces remises à zéro.

## Seuils initiaux

| Signal | Fenêtre et seuil | Sévérité |
| --- | --- | --- |
| Cible ou collecte persistante indisponible | 2 minutes | critique |
| Réponses `5xx` | plus de 2 % avec au moins 20 requêtes/10 min, pendant 5 min | critique |
| Latence HTTP p95 agrégée | plus de 750 ms/10 min, au moins 20 requêtes, pendant 5 min | avertissement |
| `401` + `403` | plus de 50/10 min, pendant 2 min | avertissement |
| `429` | plus de 20/10 min, pendant 2 min | avertissement |
| Dernière opération d’une dépendance | échec pendant 5 min après au moins un appel | critique |
| Erreurs d’une dépendance | plus de 20 %, au moins 5 appels/10 min, pendant 5 min | avertissement |
| Pool PostgreSQL | attente pendant 2 min ou occupation supérieure à 80 % pendant 5 min | avertissement |
| Event loop p95 | plus de 100 ms pendant 5 min | avertissement |
| OTP Sweego non résolu | plus de 80 % de sa durée de vie pendant 2 min | avertissement |
| Dead letter | au moins une pendant 1 min | critique |
| Plus ancien événement outbox | 30 min ; push 2 min ; réconciliation Stripe 15 min | critique ou avertissement |
| Maintenance | absente 10 min, en retard/bloquée 5 min, travail restant 15 min | critique ou avertissement |

Une alerte indique un symptôme, pas nécessairement sa cause. Ne pas augmenter un seuil ou une taille de pool avant
d’avoir identifié la saturation, la requête ou la dépendance concernée.

## Escalade et acquittement

En développement, la personne qui injecte l’incident acquitte et résout l’alerte dans Alertmanager. Pour un
environnement exploité :

1. une alerte critique doit être examinée sous 15 minutes ; sécuriser d’abord les données et éviter tout rejeu
   aveugle d’un effet externe ;
2. sans acquittement ou sans diagnostic sous 15 minutes supplémentaires, escalader au responsable de la plateforme ;
3. les avertissements sont examinés le jour ouvré courant, sauf hausse simultanée des `5xx`, dead letters ou retards ;
4. conserver l’heure, le nom de l’alerte, la cause, les actions et le retour à la normale, sans payload ni donnée
   personnelle ;
5. ne marquer résolu qu’après disparition du signal et vérification du workflow métier affecté.

Le propriétaire nominatif, le calendrier d’astreinte et le canal de notification doivent être décidés avant la
production. Cette décision opérationnelle ne doit pas être codée sous forme d’adresse personnelle dans le dépôt.

<a id="metrics-unavailable"></a>
## Cible de métriques indisponible

Vérifier `/health/live`, le processus API, `METRICS_ENABLED`, l’interface/port, puis la cible Prometheus. Si la cible
répond `401`, réaligner le même `METRICS_TOKEN` côté API et dans `.secrets/histae_metrics_token`, puis recréer le
conteneur Prometheus ; ne jamais afficher le token. Un échec du seul port 9091 n’autorise pas à rendre `/metrics`
public sur le port métier.

<a id="collection-failed"></a>
## Collecte persistante impossible

`histae_metrics_collection_success=0` signifie que la cible répond mais qu’au moins une lecture PostgreSQL de
l’outbox, des OTP ou de la maintenance a échoué. Vérifier `/health/ready`, PostgreSQL, le pool et le dernier code
d’erreur sûr. Restaurer la dépendance, puis confirmer que la jauge revient à 1 ; ne pas masquer l’alerte en retirant
les métriques persistantes.

<a id="http-errors"></a>
## Erreurs HTTP

Comparer les routes normalisées, le moment du déploiement, la disponibilité des dépendances et les événements de
logs sûrs. Un pic global avec dépendance en échec appelle d’abord le runbook de cette dépendance. Ne jamais activer
de logs contenant corps, query string, téléphone, token ou stack pour diagnostiquer.

<a id="http-latency"></a>
## Latence HTTP

Comparer débit, event loop, pool PostgreSQL et histogrammes de dépendance. Identifier une route normalisée, puis
utiliser les requêtes documentées dans `sql-performance.md`. Ne pas relever les timeouts avant d’avoir vérifié
verrous, plans SQL, saturation CPU/mémoire et taille des lots.

<a id="authentication-failures"></a>
## Échecs d’authentification et d’autorisation

Séparer `401` (session/credential invalide ou expiré) de `403` (origine, consentement ou droit refusé). Vérifier si
le trafic est attendu et si un déploiement mobile/admin vient d’avoir lieu. En cas d’abus probable, conserver les
limites, vérifier le proxy de confiance et bloquer en amont sans journaliser les identifiants de victime.

<a id="rate-limit"></a>
## Limitation de débit

Identifier les routes normalisées concernées et contrôler Redis si le mode distribué est actif. Un `429` attendu
sous attaque prouve que la protection agit ; une hausse sur trafic légitime demande de corriger le client ou de
mesurer le budget, pas de supprimer immédiatement la limite.

<a id="dependency-down"></a>
<a id="dependency-errors"></a>
## Dépendance indisponible ou instable

Le label `dependency` appartient à la liste fermée `postgres`, `redis`, `object_storage`, `sweego`,
`stripe`. Vérifier son healthcheck, réseau, TLS, credentials et quotas sans imprimer de secret. Pour S3 et Stripe,
préserver les traces PostgreSQL et laisser l’outbox/réconciliation reprendre ; ne pas relancer un POST dont l’issue
est incertaine. Pour Redis, vérifier les mécanismes de reconnexion avant tout redémarrage partagé.

<a id="postgres-pool"></a>
## Pool PostgreSQL sous pression

Contrôler connexions actives/attente, transactions longues, verrous, durée des requêtes et processus concurrents.
Le pool d’activité de compte ajoute jusqu’à quatre sessions par processus. Corriger d’abord les transactions ou la
concurrence ; augmenter `POSTGRES_POOL_MAX` sans vérifier la capacité serveur peut aggraver la panne.

<a id="sweego"></a>
## Livraison Sweego en retard

Vérifier les erreurs de dépendance, l’état des callbacks signés et les états `pending/accepted/unknown`. `sent` ne
prouve pas la réception au téléphone. Ne jamais rejouer automatiquement un POST après une issue incertaine ; le
callback ou une nouvelle demande utilisateur selon les règles OTP doit résoudre le parcours.

<a id="dead-letters"></a>
## Dead letters

Ouvrir la liste protégée `GET /api/admin/outbox/dead-letters`, sans chercher de payload dans les logs. Corriger la
cause puis utiliser la reprise avec authentification récente, motif et audit. L’abandon de `account.erase` et des
événements Stripe est interdit ; `photo.delete` reste non abandonnable tant que la photo existe. Une dead letter
push peut être abandonnée selon le contrat. Confirmer ensuite consommation et résultat métier.

<a id="outbox-delay"></a>
## Retard outbox

Vérifier qu’un worker est actif, ses claims/renouvellements, la dépendance visée et `work_remaining`. Lancer une
passe contrôlée si aucun worker ne tourne ; ne jamais modifier directement le statut ni exécuter l’effet externe
hors du dispatcher idempotent.

<a id="billing-reconciliation"></a>
## Réconciliation Stripe en retard

Vérifier maintenance `billing`, worker outbox et erreurs Stripe. Pour une création Customer incertaine, rejouer la
même clé uniquement dans la fenêtre autorisée ; après 23 heures effectuer uniquement les recherches documentées.
Toute ambiguïté reste une dead letter soumise à décision humaine.

<a id="maintenance"></a>
## Maintenance absente, bloquée ou bornée

Vérifier `MAINTENANCE_MODE`, le planificateur et l’état persistant du job. `work_remaining=1` signifie que la passe
a atteint son budget : relancer de façon contrôlée et mesurer avant d’augmenter les lots. Un job `running` ancien
peut être repris par les règles existantes ; ne pas supprimer son état à la main.

## Test contrôlé des alertes

`alerts.test.yml` injecte trois séries synthétiques et vérifie le délai, les labels et le runbook des alertes cible
indisponible, collecte persistante impossible et dead letter. Exécuter `promtool test rules` après toute modification.

Pour vérifier toute la chaîne locale, arrêter volontairement **l’API seulement** pendant plus de deux minutes,
observer `HistaeMetricsUnavailable` dans Prometheus puis Alertmanager, suivre le runbook ci-dessus, redémarrer l’API
et vérifier la résolution. Ne jamais arrêter PostgreSQL, Redis ou S3 partagés pour ce test. Archiver seulement
le nom de l’alerte et les horodatages, sans capture contenant une configuration ou un secret.

## Instrumentation des adaptateurs Rust

Les compteurs sont branchés sur les clients utilisés par l’API : requêtes Stripe, envois Sweego,
opérations S3, commandes Redis, acquisition des connexions PostgreSQL, sondes SQL et commandes de transaction (BEGIN/COMMIT/ROLLBACK).
Les durées incluent les reprises internes du client. PostgreSQL compte des opérations d’accès et
commandes de transaction, pas chaque instruction SQL individuelle. Les refus métier ne sont pas comptés comme des pannes de stockage.
Ces compteurs ne doivent donc pas être interprétés comme un nombre de requêtes SQL ni comparés directement
aux volumes du driver NestJS. Une erreur HTTP conserve aussi ses compteurs et un événement sûr
`http_request_failed`. Les labels restent bornés et aucun payload, SQL, secret ou identifiant client
n’est enregistré dans les métriques des dépendances.
