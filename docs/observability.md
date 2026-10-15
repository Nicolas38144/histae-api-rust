# Supervision et runbooks

[Index](README.md) · [Exploitation](operations.md) · [Performance SQL](sql-performance.md)

Ce guide couvre l’export Prometheus privé de l’API, la pile locale Prometheus/Alertmanager/Grafana, les seuils
initiaux et la conduite à tenir. Les seuils sont des points de départ à recalibrer avec des mesures de charge
sur la topologie déployée. Ils ne doivent jamais introduire un label utilisateur, un identifiant métier ou une donnée personnelle.

## Architecture et sécurité

L’API ouvre, uniquement lorsque `METRICS_ENABLED=true`, un second serveur HTTP dédié sur `METRICS_HOST:METRICS_PORT`.
Il ne fait pas partie du contrat `/api`, n’accepte que `GET /metrics` et exige `Authorization: Bearer …`. Le secret
`METRICS_TOKEN` fait au moins 32 octets, est distinct des clés JWT/téléphone et n’est ni journalisé ni exposé dans
les métriques. Une collecte PostgreSQL en échec laisse disponibles les compteurs mémoire et publie
`histae_metrics_collection_success 0`.

Le serveur applicatif principal ne publie aucun endpoint Prometheus. En développement Docker, les métriques
sont activées automatiquement sur `api:9091`, sans publication du port sur l’hôte.
Les outils utilisent des volumes persistants et des réseaux privés. Leurs plafonds mémoire cumulés sont de
1,5 Gio ; Prometheus conserve au plus 15 jours ou 2 Go de séries. Les logs Docker sont bornés à cinq fichiers de 10 Mo.

Le compose ne configure aucun destinataire de notification : les alertes restent visibles dans Prometheus et
Alertmanager, ainsi que dans les panneaux de métriques Grafana. Un canal d’astreinte approuvé reste à raccorder
avant une exploitation en production. Ne jamais publier le port 9091 dans Cloudflare ni sur Internet.

<a id="supervision-integree-au-developpement"></a>

## Supervision intégrée au développement

`compose.dev.yaml` réutilise les trois services de `docker-compose.observability.yml` sans publier les ports
3000, 9090 et 9093. Il force `METRICS_ENABLED=true`, `METRICS_HOST=0.0.0.0` et `METRICS_PORT=9091` pour l’API ;
les processus migration, outbox et maintenance gardent leurs métriques désactivées.

### 1. Préparer les secrets

Depuis la racine de l’API, avec `.env` déjà configuré :

```bash
bash scripts/prepare-observability-secrets.sh
```

Le script réutilise un `METRICS_TOKEN` littéral existant d’au moins 32 caractères, ou en génère un de 64 caractères
hexadécimaux dans `.env`. Il synchronise `.secrets/histae_metrics_token` et crée le mot de passe de secours
`.secrets/histae_grafana_admin_password` uniquement s’il n’existe pas. Il n’affiche aucun secret et conserve
le propriétaire du `.env`, y compris lors d’un lancement avec `sudo`.
Un second argument optionnel fixe le dossier des secrets :
`bash scripts/prepare-observability-secrets.sh /chemin/api.env "$PWD/.secrets"`.
La procédure centralisée utilise toujours le dossier `.secrets` de l’API, même lorsque son fichier
d’environnement est placé ailleurs ; les sources des secrets Compose restent ainsi cohérentes.

Le dossier `.secrets` est privé (`0700`). Les fichiers de supervision montés par Docker sont lisibles par les UID
des services (`0444`) : leur protection sur l’hôte repose sur le dossier parent. `.env` et `.secrets` restent
ignorés par Git. Une rotation du mot de passe fichier ne change pas le compte Grafana d’un volume déjà initialisé.

### 2. Démarrer les services

Pour une API locale :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d --build --wait
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml ps --all
```

Pour accéder aux outils depuis `https://dashboard.histae.com`, utiliser aussi `compose.proxy.yaml`.
Le reverse proxy doit démarrer en premier : il crée `histae-edge` et le réseau interne `histae-monitoring`
(`172.29.0.0/24`, sous-réseau à réserver). Son `.env` doit sélectionner `HISTAE_API_UPSTREAM=http://histae-api-dev:8080`.
Dans le `.env` de l’API, définir `ADMIN_WEBAUTHN_ORIGIN=https://dashboard.histae.com`,
`ADMIN_WEBAUTHN_RP_ID=dashboard.histae.com` et l’origine CORS correspondante.

La procédure complète est automatisée depuis `histae-scripts` :

```bash
bash reset-docker-compose.sh --dev
```

Le reset prépare les secrets, valide les configurations, reconstruit les images et démarre les services modifiés.
Il affiche la durée de chaque étape. `--full` force la recréation de tous les conteneurs ; la commande normale
laisse les services inchangés en place. Il exécute aussi une passe de maintenance.
Après modification de fichiers de configuration montés des outils ou rotation des secrets, utiliser
`--full` pour que tous les processus rechargent ces fichiers ; Compose ne détecte pas leur contenu.

Pour démarrer manuellement après le proxy :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml -f compose.proxy.yaml \
  up -d --build --wait
```

### 3. Ouvrir les outils

Se connecter au dashboard avec la passkey du **superadmin**, puis ouvrir **Supervision**.
Les trois boutons ouvrent un nouvel onglet :

| Outil | URL | Usage |
| --- | --- | --- |
| Grafana | `https://dashboard.histae.com/monitoring/grafana/` | Tableau `Histae / Histae Operations` |
| Prometheus | `https://dashboard.histae.com/monitoring/prometheus/` | PromQL, règles et santé des cibles |
| Alertmanager | `https://dashboard.histae.com/monitoring/alertmanager/` | Alertes et silences |

Aucun nouveau domaine ni changement d’ingress Cloudflare n’est nécessaire. Les chemins restent sur l’origine
du dashboard. Le serveur Vite seul ne fournit pas ces routes : les liens nécessitent Caddy et l’override proxy.

Caddy vérifie **chaque requête** avec `GET /api/admin/auth/monitoring`. L’API revalide la session, la passkey,
l’état du compte et le rôle courant : absence/expiration/révocation → `401`, rôle admin → `403`.
Les mutations exigent l’`Origin` exacte du dashboard. Caddy supprime les identités entrantes et le cookie Histae
avant de transmettre la requête aux outils. Grafana accepte uniquement l’identité fournie par Caddy depuis
`172.29.0.4` et attribue le rôle d’organisation `Admin`, sans créer de session de connexion indépendante.
Le superadmin n’a donc pas à saisir un second mot de passe. Le compte de secours Grafana reste inaccessible
par formulaire dans ce mode. Prometheus conserve ses API d’administration et de rechargement HTTP désactivées.

Une indisponibilité de l’API ferme l’accès aux trois interfaces. Leur collecte continue séparément.
Les liens du dashboard ne démarrent aucun service. L’intégration production se configure avec l’override décrit ci-dessous.

### 4. Vérifier la collecte

Depuis la racine de l’API ; ajouter `-f compose.proxy.yaml` si ce mode est utilisé :

```bash
OBSERVABILITY=(sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml)
"${OBSERVABILITY[@]}" exec -T prometheus promtool check config /etc/prometheus/prometheus.yml
"${OBSERVABILITY[@]}" exec -T prometheus promtool test rules /etc/prometheus/alerts.test.yml
```

Dans Prometheus, **Status → Target health** doit montrer `histae-api` à **UP**.
Un `401` de collecte indique généralement un désaccord entre le token de l’API et le secret fichier.
Un refus de connexion indique l’API arrêtée, les métriques désactivées, une mauvaise interface ou un réseau absent.
Après rotation du token, recréer API et Prometheus pour recharger les deux valeurs.

Le tableau `Histae / Histae Operations` affiche disponibilité, alertes, HTTP, dépendances, pool PostgreSQL,
outbox et maintenance. Les compteurs repartent à zéro après un redémarrage de l’API ; `rate` et `increase`
savent gérer ces remises à zéro. Les métriques et panneaux ne portent aucun identifiant utilisateur.

<a id="supervision-integree-a-la-production"></a>

## Supervision intégrée à la production

`compose.production-observability.yaml` s’ajoute à `compose.yaml` et `compose.production.yaml`.
Il active le listener privé `api:9091`, réutilise les trois services de supervision, leurs secrets
et volumes, puis les raccorde aux alias `histae-production-*` sur le réseau privé monitoring.
La configuration Prometheus porte `environment: production`. Aucun port de supervision n’est publié.
Les volumes restent dans le projet production et ne reprennent pas ceux du projet dev.

Le proxy doit sélectionner les destinations production du modèle `.env.example`, et les deux
environnements doivent partager le nom `HISTAE_MONITORING_NETWORK`.
Le script centralisé impose les bonnes destinations avec son choix de mode :

```bash
# Depuis histae-scripts, après préparation de l’environnement API et des certificats :
bash reset-docker-compose.sh --production
```

Pour démarrer manuellement depuis la racine API, après démarrage du proxy et création du backend :

```bash
bash scripts/prepare-observability-secrets.sh
sudo docker compose --env-file .env \
  -f compose.yaml -f compose.production.yaml -f compose.production-observability.yaml \
  up -d --build --wait
```

L’accès reprend le contrôle WebAuthn du superadmin pour chaque requête, la suppression du cookie
avant transmission aux outils et l’identité Grafana fournie uniquement par l’API via Caddy.
Les contrôles de collecte et les tests `promtool` ci-dessus s’appliquent en gardant ces trois manifests.
Le provisioning nommé `provisioning-dev` est réutilisé pour son URL Prometheus avec sous-chemin ;
les données, les services et les labels de cette pile sont bien ceux de la production.

Le receiver Alertmanager fourni reste sans canal de notification. Un destinataire réel et une
validation de réception/résolution doivent être configurés avant ouverture. Ne pas lancer en parallèle
la pile autonome sur cette même API si l’intégration répond au besoin.

## Pile autonome : API native ou supervision distincte

La pile indépendante reste disponible pour une API exécutée directement sur Debian ou un déploiement séparé.
Elle utilise les mêmes secrets, mais publie les interfaces uniquement sur loopback :

| Interface | URL locale | Authentification |
| --- | --- | --- |
| Grafana | `http://localhost:3001` | `histae-admin` et le mot de passe fichier Grafana |
| Prometheus | `http://localhost:9090` | Accès local uniquement |
| Alertmanager | `http://localhost:9093` | Accès local uniquement |

Préparer les secrets avec le script ci-dessus, puis définir dans `.env` :

```ini
METRICS_ENABLED=true
METRICS_HOST=0.0.0.0
METRICS_PORT=9091
```

Un listener natif lié à `127.0.0.1` n’est pas accessible par le bridge Docker : limiter le port 9091
au réseau de supervision avec le filtrage de l’hôte. Redémarrer l’API native, puis :

```bash
sudo docker compose --env-file .env -f docker-compose.observability.yml up -d --wait
```

Prometheus cible alors `host.docker.internal:9091`. Pour une API conteneurisée de production, ajouter
`-f compose.observability-container.yaml` et utiliser le même `HISTAE_BACKEND_NETWORK` que la pile API.
La configuration `prometheus.container.yml` cible `api:9091`. Activer les métriques de cette API et la recréer.
Conserver la même sélection de fichiers Compose pour les commandes suivantes.

Cette pile autonome n’est pas raccordée aux routes `/monitoring/*` du dashboard. Prévoir un accès
administratif authentifié et un canal de notification avant son usage en production. Éviter de lancer une
seconde collecte autonome sur l’API dev si les trois services intégrés répondent déjà au besoin.

## Validation locale du 13 octobre 2026

- Configurations Compose dev, dev avec proxy, production, supervision autonome et reverse proxy valides.
- API reconstruite ; API et trois outils sains, aucun port de supervision publié ; cible `histae-api` à `UP`.
- 304 tests Rust, Clippy avec avertissements interdits et formatage : réussis.
- Dashboard : typecheck, lint, build et 53 tests unitaires réussis.
- Caddy : refus `401`/`403`, remplacement d’une identité forgée, retrait du cookie Histae, corps POST
  conservé et contrôle d’origine vérifiés dans des conteneurs isolés avec une autorisation fictive.
- Grafana réel dans cette isolation : identité fournie par le proxy, rôle d’organisation `Admin` et accès
  anonyme/identité forgée refusés. L’API réelle rejette aussi l’accès anonyme au nouvel endpoint.
- Tests `promtool` des règles d’alerte : réussis. Préparation des secrets : valeurs vides/commentaires,
  relance, permissions et refus de l’interpolation ou des liens symboliques vérifiés.

Le reset complet, le tunnel du serveur et la connexion avec une vraie passkey sur le domaine public
restent à vérifier après copie et démarrage sur le serveur. Aucun compte réel ni panne de dépendance
n’a été utilisé pour simuler une autorisation ou une alerte.

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

Comparer débit, CPU/mémoire du conteneur, pool PostgreSQL et histogrammes de dépendance. Identifier une route normalisée, puis
suivre le [guide SQL](sql-performance.md). Ne pas relever les timeouts avant d’avoir vérifié
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
cause puis utiliser la reprise avec authentification récente et audit ; motif requis pour un admin, facultatif
pour le superadmin. L’abandon de `account.erase` et des
événements Stripe est interdit ; `photo.delete` reste non abandonnable tant que la photo existe. Une dead letter
push peut être abandonnée selon le contrat. Confirmer ensuite consommation et résultat métier.

<a id="outbox-delay"></a>

## Retard outbox

Vérifier qu’un worker est actif, ses claims/renouvellements, la dépendance visée et `work_remaining`. Lancer une
worker outbox si aucun ne tourne ; ne jamais modifier directement le statut ni exécuter l’effet externe
hors du dispatcher idempotent. Voir [Exploitation](operations.md#outbox-et-reprises-operateur).

<a id="billing-reconciliation"></a>

## Réconciliation Stripe en retard

Vérifier maintenance `billing`, worker outbox et erreurs Stripe. Pour une création Customer incertaine, rejouer la
même clé uniquement dans la fenêtre autorisée ; après 23 heures effectuer uniquement les recherches documentées.
Toute ambiguïté reste une dead letter soumise à décision humaine.

<a id="maintenance"></a>

## Maintenance absente, bloquée ou bornée

Vérifier `MAINTENANCE_MODE`, le planificateur et l’état persistant du job. `work_remaining=1` signifie que la passe
a atteint son budget : relancer de façon contrôlée et mesurer avant d’augmenter les lots. Un job `running` ancien
peut être repris par les règles existantes ; ne pas supprimer son état à la main. Vérifier aussi si Stripe est
désactivé : son job ne s’exécute alors pas. Voir [ordonnancement et statut](operations.md#maintenance-reguliere).

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
Le runtime expose son identité Rust, sa durée de fonctionnement et sa mémoire RSS.
Ces compteurs ne doivent pas être interprétés comme un nombre de requêtes SQL. Une erreur HTTP conserve aussi ses compteurs et un événement sûr
`http_request_failed`. Les labels restent bornés et aucun payload, SQL, secret ou identifiant client
n’est enregistré dans les métriques des dépendances.
