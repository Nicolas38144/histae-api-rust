# Configuration

[Index](README.md) · [Développement](development.md) · [Déploiement](container-deployment.md)

La référence exécutée est [AppConfig](../src/config/loader.rs), complétée par les
[validateurs](../src/config/validation.rs). Une valeur invalide empêche le démarrage avec un code normalisé ;
les valeurs et secrets ne sont pas recopiés dans les logs.

## Chargement et priorité

### Binaires Rust sur l’hôte

`AppConfig::from_env()` charge `.env` avec `dotenvy`, puis lit l’environnement du processus. Une variable
déjà définie dans le terminal garde la priorité. Le fichier `.env` est commun aux deux modes : définir
`ENV` pour une exécution native, ou choisir
l'override Compose correspondant.
Lancer les commandes depuis la racine évite de charger par erreur un `.env` d’un dossier parent.

`ENV` est obligatoire et accepte `development`, `production` et `test`. `ENV=development` derrière
un domaine HTTPS reste du développement, avec ses règles et fournisseurs configurés pour ce mode.

### Docker Compose

| Mécanisme | Effet |
| --- | --- |
| `--env-file .env` | Fournit les valeurs d’interpolation `${…}` des manifests. |
| `env_file: ${HISTAE_ENV_FILE:-.env}` | Injecte le fichier choisi dans les conteneurs applicatifs. |
| `environment:` d’un service | Remplace les mêmes variables reçues via `env_file`. |

Les variables exportées du terminal peuvent remplacer les valeurs d’interpolation.
Les deux piles utilisent le fichier **`.env`** : `compose.dev.yaml` impose `ENV=development` et
`compose.production.yaml` impose `ENV=production`. Les valeurs de chaque machine doivent correspondre à
son usage (fournisseurs, domaines, certificats et secrets). Le dashboard et le reverse proxy ont leurs
propres fichiers d'environnement. Utiliser `--env-file .env` dans toutes les commandes de l'API.

Les fichiers `.env`, `.env.*` et `.secrets/` sont ignorés par Git. Les garder en `0600`.
Les overrides dev/production sélectionnent le rôle PostgreSQL
et retirent les mots de passe des autres rôles de l'environnement applicatif.
Voir [Rôles PostgreSQL](postgres-roles.md).

Après une modification, recréer les conteneurs concernés : `restart` ne recharge pas leur environnement.
Valider sans afficher les secrets avec `docker compose … config --quiet`.

<a id="configuration-locale-minimale"></a>

## Configuration locale minimale

Créer `.env` et remplacer les secrets applicatifs par des valeurs distinctes ; le préparateur génère
séparément les trois mots de passe PostgreSQL.
Pour chaque valeur, `openssl rand -hex 32` produit 32 octets encodés en hexadécimal. Les clés téléphone
acceptent ce format de 64 caractères ; `JWT_SECRET` exige au moins 32 octets.

```dotenv
ENV=development
PORT=8080
HISTAE_ENV_FILE=.env
HISTAE_BACKEND_NETWORK=histae-backend
HISTAE_API_BIND_IP=127.0.0.1
HISTAE_API_HOST_PORT=8080

JWT_SECRET=REPLACE_JWT_SECRET
PHONE_ENCRYPTION_KEY=REPLACE_64_HEX_CHARACTERS_ENCRYPTION
PHONE_HASH_KEY=REPLACE_64_HEX_CHARACTERS_HASH
POSTGRES_HOST=127.0.0.1
POSTGRES_PORT=5433
POSTGRES_DB=histae-dev
POSTGRES_ADMIN_PASSWORD=REPLACE_WITH_64_RANDOM_HEX_CHARACTERS
POSTGRES_MIGRATE_PASSWORD=REPLACE_WITH_OTHER_64_RANDOM_HEX_CHARACTERS
POSTGRES_APP_PASSWORD=REPLACE_WITH_ANOTHER_64_RANDOM_HEX_CHARACTERS
POSTGRES_USER=histae_app
POSTGRES_PASSWORD=${POSTGRES_APP_PASSWORD}
POSTGRES_SSLMODE=disable

RATE_LIMIT_STORE=redis
REDIS_ADDR=127.0.0.1:6379
REDIS_PASSWORD=
REDIS_TLS=false
SMS_PROVIDER=disabled
BILLING_PROVIDER=disabled
PUSH_PROVIDER=disabled
PHOTO_MODERATION_PROVIDER=local_http
PHOTO_MODERATION_ENDPOINT=http://127.0.0.1:8090
PHOTO_MODERATION_TOKEN=REPLACE_PHOTO_TOKEN
OBJECT_STORAGE_ENDPOINT=http://storage.histae.localhost:8333
OBJECT_STORAGE_ACCESS_KEY=histae-dev
OBJECT_STORAGE_SECRET_KEY=histae-dev-secret-change-me
OBJECT_STORAGE_BUCKET=histae-photos

ADMIN_WEBAUTHN_ORIGIN=http://localhost:5173
ADMIN_WEBAUTHN_RP_ID=localhost
CORS_ORIGINS=http://localhost:5173
TRUST_PROXY=false
MAINTENANCE_MODE=disabled
METRICS_ENABLED=false
```

`SMS_PROVIDER=disabled` ne livre aucun SMS. Pour une connexion OTP réelle, configurer Sweego.
`python3 scripts/prepare-postgres-roles-env.py .env` prépare les trois mots de passe sans les afficher.
Une base dev existante doit être convertie avant recréation selon [Rôles PostgreSQL](postgres-roles.md).
Les documents juridiques omis en développement reçoivent une version `development-unversioned` et des
URLs `example.invalid` ; les remplacer pour tester un parcours réaliste.

## Groupes de variables

| Sujet | Variables principales | Règles |
| --- | --- | --- |
| HTTP | `PORT`, `CORS_ORIGINS`, `TRUST_PROXY` | Origines explicites ; proxies précis en production, jamais `true`. |
| JWT | `JWT_SECRET`, `JWT_ACTIVE_KID`, `JWT_PREVIOUS_KEYS`, `JWT_ACCESS_TTL`, `JWT_REFRESH_TTL` | Clés distinctes des clés téléphone ; anciens `kid` à conserver pendant la durée de validité des tokens concernés. |
| Téléphone | `PHONE_ENCRYPTION_KEY`, `PHONE_HASH_KEY` | Deux clés de 32 octets distinctes. Leur remplacement exige une migration des données. |
| PostgreSQL | `POSTGRES_HOST`, `POSTGRES_PORT`, `POSTGRES_USER`, `POSTGRES_PASSWORD`, `POSTGRES_DB`, `POSTGRES_SSLMODE`, `POSTGRES_POOL_MAX` | Pool par processus ; TLS obligatoire en production. |
| Rôles PostgreSQL Docker | `POSTGRES_ADMIN_PASSWORD`, `POSTGRES_MIGRATE_PASSWORD`, `POSTGRES_APP_PASSWORD` | Trois secrets hexadécimaux de 64 caractères distincts ; Compose fournit le bon rôle et `POSTGRES_PASSWORD` à chaque service. |
| Pool API Docker | `HISTAE_API_POSTGRES_POOL_MAX` | Défaut : 30. Compose transmet cette valeur à l’API dans `POSTGRES_POOL_MAX` ; les autres services utilisent leur `POSTGRES_POOL_MAX`. |
| Timeouts SQL | `POSTGRES_CONNECT_TIMEOUT`, `POSTGRES_IDLE_TIMEOUT`, `POSTGRES_STATEMENT_TIMEOUT`, `POSTGRES_IDLE_TRANSACTION_TIMEOUT` | Défauts : `5s`, `30s`, `15s`, `30s`. |
| Redis | `RATE_LIMIT_STORE`, `REDIS_ADDR`, `REDIS_PASSWORD`, `REDIS_DB`, `REDIS_TLS`, `REDIS_CONNECT_TIMEOUT`, `REDIS_COMMAND_TIMEOUT` | Production : store Redis, TLS et mot de passe. Adresse `hôte:port`, sans schéma. |
| S3 | `OBJECT_STORAGE_ENDPOINT`, `OBJECT_STORAGE_REGION`, `OBJECT_STORAGE_BUCKET`, `OBJECT_STORAGE_ACCESS_KEY`, `OBJECT_STORAGE_SECRET_KEY`, `OBJECT_STORAGE_FORCE_PATH_STYLE` | HTTPS et clés explicites en production ; nom accessible aux serveurs et clients pour les URLs signées. |
| Photos | `PHOTO_MODERATION_PROVIDER`, `PHOTO_MODERATION_ENDPOINT`, `PHOTO_MODERATION_TOKEN`, `PHOTO_MODERATION_TIMEOUT`, `PHOTO_MODERATION_MIN_SHARPNESS`, `PHOTO_MODERATION_NSFW_REVIEW_THRESHOLD` | `disabled` ou `local_http` ; token d’au moins 32 octets avec `local_http`. |
| OTP | `SMS_PROVIDER`, `SWEEGO_API_KEY`, `SWEEGO_SMS_SENDER_ID`, `SWEEGO_SMS_REGION`, `SWEEGO_API_URL`, `SWEEGO_TIMEOUT`, `OTP_TTL`, `SWEEGO_WEBHOOK_SECRET` | Sweego obligatoire en production ; région `FR`. Secret de webhook fourni par Sweego, distinct de la clé API. |
| Stripe | `BILLING_PROVIDER`, `STRIPE_SECRET_KEY`, `STRIPE_WEBHOOK_SECRET`, `STRIPE_PREMIUM_MONTHLY_PRICE_ID`, `STRIPE_PREMIUM_ANNUAL_PRICE_ID` | Clés `sk_test_…` en développement, `sk_live_…` en production ; deux identifiants de prix `price_…` distincts. |
| Redirections Stripe | `STRIPE_CHECKOUT_SUCCESS_URL`, `STRIPE_CHECKOUT_CANCEL_URL`, `STRIPE_PORTAL_RETURN_URL` | HTTPS ; succès contient `{CHECKOUT_SESSION_ID}`. Entrées `/billing/{success,cancel,return}` sur `www.histae.com`, puis langue du navigateur : [configuration et retour dans l'app](billing.md#retours-stripe-sur-le-site-vitrine). |
| Réconciliation Stripe | `STRIPE_RECONCILIATION_INTERVAL`, `STRIPE_RECONCILIATION_FRESHNESS`, `STRIPE_RECONCILIATION_BATCH_SIZE`, `STRIPE_TIMEOUT` | Défauts : `5m`, `1h`, 25 éléments, `10s` ; maintenance pour programmer, outbox pour exécuter. |
| Options Stripe | `STRIPE_AUTOMATIC_TAX`, `STRIPE_ALLOW_PROMOTION_CODES`, `STRIPE_MAX_NETWORK_RETRIES` | Défauts : `false`, `false`, `2` ; respecter les fenêtres d’idempotence du contrat. |
| AdMob | `ADVERTISING_PROVIDER`, `ADMOB_ANDROID_REWARDED_AD_UNIT`, `ADMOB_IOS_REWARDED_AD_UNIT`, `ADMOB_REWARD_ITEM` | `disabled` par défaut ou `admob` ; deux unités rewarded obligatoires si activé, unités Google de test refusées en production ; récompense 1 crédit, libellé `continuation_credit` par défaut. [Guide](continuation-and-rewards.md). |
| Push | `PUSH_PROVIDER`, `FIREBASE_PROJECT_ID`, `FIREBASE_CLIENT_EMAIL`, `FIREBASE_PRIVATE_KEY`, `FIREBASE_TOKEN_URI`, `PUSH_TIMEOUT` | `disabled` ou `fcm` ; compte de service complet pour FCM. |
| Admin | `ADMIN_WEBAUTHN_ORIGIN`, `ADMIN_WEBAUTHN_RP_ID`, `ADMIN_WEBAUTHN_RP_NAME`, `ADMIN_SESSION_IDLE_TTL`, `ADMIN_SESSION_ABSOLUTE_TTL`, `ADMIN_RECENT_AUTH_TTL` | Voir [Administration](administration.md) ; HTTPS en production. |
| Enrôlement | `ADMIN_WEBAUTHN_CHALLENGE_TTL`, `ADMIN_WEBAUTHN_BOOTSTRAP_TTL` | Défauts : `5m` et `15m`, usage unique. |
| Maintenance/export | `MAINTENANCE_MODE`, `MATCH_MAINTENANCE_BATCH_SIZE`, `MATCH_MAINTENANCE_MAX_BATCHES`, `OUTBOX_PURGE_BATCH_SIZE`, `OUTBOX_PURGE_MAX_BATCHES`, `DATA_EXPORT_PAGE_SIZE`, `DATA_EXPORT_MAX_BYTES`, `DATA_EXPORT_MAX_CONCURRENCY` | Traitements bornés ; voir [Exploitation](operations.md). |
| Effacement | `ACCOUNT_DELETION_TOKEN_TTL` | Défaut `10m`, jeton à usage unique ; workflow asynchrone. |
| Métriques | `METRICS_ENABLED`, `METRICS_HOST`, `METRICS_PORT`, `METRICS_TOKEN` | Listener privé, port `9091` ; token distinct d’au moins 32 octets. La pile dev active la collecte ; préparer les secrets avec `bash scripts/prepare-observability-secrets.sh`. |

Les limites dédiées `RATE_LIMIT_*` possèdent une variable de fenêtre suffixée `_WINDOW`.
Le [contrat HTTP](http-contract.md#erreurs-et-limites-de-debit) décrit les quotas et codes publics.
Les durées utilisent une unité, par exemple `5s`, `1m` ou `8h`.

### Tarifs du catalogue et Stripe

Le catalogue initial propose Premium à **5 EUR par mois** ou **48 EUR par an**, sans période d'essai.

Free reçoit **trois crédits gratuits chaque lundi à 00:00 UTC**, sans accumulation. Il peut aussi
conserver un stock de **trois crédits publicitaires maximum**, réservations comprises, obtenu par les
récompenses lorsqu’elles sont activées. Ce stock se conserve entre les semaines. Le plafond est de
**six continuations confirmées par personne Free et par semaine UTC** : chaque participant Free réserve
son propre crédit, débité seulement après les deux accords. Un refus ou une expiration libère la
réservation. Premium permet des continuations illimitées sans débit de crédit. Voir les
[règles de continuation et de récompenses](continuation-and-rewards.md) pour le détail des réservations.

Le [catalogue SQL](../db/002_insert_postgres.sql) initialise une base neuve. Sur une base existante, le
migrateur remplace l'ancien prix annuel Premium de 3 000 centimes EUR par 4 800 centimes, sans écraser un
autre tarif personnalisé. Un second passage ne modifie ni le prix ni sa date de mise à jour.

Le montant réellement facturé dépend du Price Stripe référencé par `STRIPE_PREMIUM_ANNUAL_PRICE_ID`.
Créer un Price récurrent annuel à 48 EUR avant d'activer cette offre, puis renseigner son identifiant dans
le `.env` du serveur. Modifier le catalogue ne modifie pas les Prices Stripe ni les abonnements existants.
Aligner également le catalogue statique du site (`histae-site/config/site.json`) et le reconstruire.
L'app et le dashboard lisent les trois offres via `GET /api/plans` : `offers` contient Free,
Premium Mensuel et Premium Annuel, avec un prix, une devise et une périodicité par carte.
Les fonctionnalités et quotas sont rattachés aux deux niveaux `plans` par `plan_code`.
Le catalogue initial expose 0 EUR, 5 EUR/mois et 48 EUR/an ; les deux offres Premium héritent des
mêmes fonctionnalités, sans période d'essai. Le catalogue HTTP ne contient plus `trial_days`.
Le contrat impose `plans` et `offers` ; l'app et le dashboard rejettent une réponse sans `offers`,
sans reconstruire de cartes depuis les prix de `plans`. Une liste `offers` explicitement vide
reste vide ; aucune donnée invalide n'est remplacée par un tarif local.
Le dashboard exige aussi les quatre compteurs `subscription_offers` des métriques d'administration.
Déployer l'API, le dashboard et la nouvelle compilation mobile avec le même contrat. Aucun nouvel
identifiant produit Stripe ni troisième niveau de droits n'est nécessaire.

Une réponse `/api/plans` qui n'a toujours pas `offers` après déploiement indique une ancienne
version de l'API en service. Vérifier les sources présentes sous `HISTAE_ROOT`, puis reconstruire
et recréer l'API et le dashboard avec `reset-api.sh --production --full` et
`reset-dashboard.sh --production --full`. Les scripts conservent les volumes. Le schéma initial et le seed n’ont plus de colonne `subscription_plan.trial_days`. Sur une base
existante, le migrateur retire cette colonne si elle est présente, avec sa contrainte locale, sans
modifier les abonnements ni leurs échéances. Le champ `trial_days` est aussi retiré du catalogue HTTP. Les nouvelles créations Checkout
sont sans essai ; les sessions Stripe déjà créées avant la mise à jour conservent leurs conditions
jusqu'à expiration. Pour retirer immédiatement une ancienne session ouverte, l'expirer dans Stripe. Effacer la base
n'ajoute pas ce champ : les trois offres sont calculées par le code à partir des deux plans.

`STRIPE_PREMIUM_MONTHLY_PRICE_ID` et `STRIPE_PREMIUM_ANNUAL_PRICE_ID` contiennent les identifiants des **prix**
Stripe (`price_…`), pas les identifiants des abonnements clients (`sub_…`). Aucun identifiant de
produit n'est demandé dans le `.env`. Les webhooks et la réconciliation reconnaissent Premium
à partir de ces deux prix autorisés pour les nouvelles ventes.
Les contrôles de signature, de mode test/live et d'unicité de l'abonnement restent appliqués.
Après rotation de ces prix, un ancien prix peut encore renouveler son abonnement déjà connu : son
association exacte au client et à l'abonnement est contrôlée en base. Il ne devient pas un prix
supplémentaire autorisé pour les nouveaux achats. Voir le [parcours Stripe](billing.md).

### Documents juridiques

En production, les neuf valeurs suivantes sont obligatoires :

```dotenv
TERMS_OF_SERVICE_VERSION=<version-publiee>
TERMS_OF_SERVICE_URL=<url-https>
PRIVACY_POLICY_VERSION=<version-publiee>
PRIVACY_POLICY_URL=<url-https>
SENSITIVE_DATA_CONSENT_VERSION=<version-publiee>
SENSITIVE_DATA_CONSENT_URL=<url-https>
LOCATION_CONSENT_VERSION=<version-publiee>
LOCATION_CONSENT_URL=<url-https>
LEGAL_REVIEW_REFERENCE=<reference-de-validation-reelle>
```

L’API expose ces versions et URLs pour les consentements. Une nouvelle version peut imposer une nouvelle
acceptation aux comptes existants. La présence d’une chaîne dans `LEGAL_REVIEW_REFERENCE` ne prouve pas
qu’une revue juridique/DPO a eu lieu : renseigner sa référence réelle.

### TLS et nom de variable historique

`POSTGRES_SSLMODE=disable` désactive TLS. Toute autre valeur sélectionne actuellement **VerifyFull**
dans SQLx ; utiliser explicitement `verify-full` pour éviter toute ambiguïté avec les modes de libpq.
`NODE_EXTRA_CA_CERTS` fournit le chemin du certificat racine PEM aux adaptateurs Rust PostgreSQL **et Redis**.
Ce nom est conservé pour compatibilité ; ces connexions ne passent pas par un client PostgreSQL Node.
En production Docker, le certificat est monté à `/run/secrets/postgres_ca`.

### Variables réservées à Compose

`HISTAE_API_IMAGE`, `HISTAE_API_POSTGRES_POOL_MAX`, `HISTAE_POSTGRES_IMAGE`, `HISTAE_PHOTO_MODERATION_IMAGE`, `HISTAE_ENV_FILE`, `HISTAE_API_BIND_IP`,
`HISTAE_API_HOST_PORT`, `HISTAE_API_DNS_SERVER`, `HISTAE_BACKEND_NETWORK`, `HISTAE_EDGE_NETWORK` et
`HISTAE_STORAGE_HOST` pilotent l’orchestration. Les overrides sont décrits dans [Déploiement](container-deployment.md).

Pour le réglage actuel des pools Docker :

```dotenv
POSTGRES_POOL_MAX=20
HISTAE_API_POSTGRES_POOL_MAX=30
```

`POSTGRES_POOL_MAX` borne le pool de chaque processus qui le reçoit, notamment outbox et maintenance.
Compose remplace cette valeur uniquement pour le service API par `HISTAE_API_POSTGRES_POOL_MAX`.
L’API reçoit donc `POSTGRES_POOL_MAX=30` dans son conteneur. Hors Compose, le binaire Rust lit
`POSTGRES_POOL_MAX` directement ; il ne lit pas `HISTAE_API_POSTGRES_POOL_MAX`.

## Vérifier une configuration

Sur l’hôte :

```bash
cargo run --locked --bin api -- --check-config
MAINTENANCE_MODE=worker cargo run --locked --bin outbox -- --check-config
MAINTENANCE_MODE=worker cargo run --locked --bin maintenance -- --check-config
```

Ces contrôles valident les valeurs, sans prouver la connectivité ni les fournisseurs. Les outils
`admin-bootstrap`, `db-migrate`, `storage-init` et `contract-compare` ont leurs propres arguments ; ne pas
leur ajouter arbitrairement `--check-config`.

### Crédits Free et publicités

Le fournisseur est laissé désactivé dans le `.env` local. Les identifiants de test Android/iOS y sont présents pour préparer une intégration, avec le libellé `continuation_credit`. Les publicités nécessitent la configuration AdMob SSV, les choix UMP et le parcours mobile décrits dans le [guide](continuation-and-rewards.md). Aucun secret AdMob partagé n’est requis. Les clés ECDSA sont téléchargées uniquement depuis l’URL HTTPS officielle Google, qui n’est pas configurable.

Le catalogue SQL utilise encore `subscription_plan.weekly_continuation_limit` comme allocation de crédits gratuits (3 pour Free, `NULL` pour Premium). La projection HTTP distingue cette allocation dans `weekly_included_credits` du plafond `weekly_continuation_limit` de 6 continuations Free confirmées. Le wallet et les verrous font autorité pour les réservations et débits. Les paramètres de pool PostgreSQL restent inchangés.

## URL des documents traduits

Les langues API sont `fr`, `en`, `es`, `it`. Pour les URL juridiques Histae, les chemins localisés sont
sélectionnés automatiquement. Pour un hébergement personnalisé, les suffixes `_EN`, `_ES`, `_IT` sont
acceptés sur `TERMS_OF_SERVICE_URL`, `PRIVACY_POLICY_URL`, `SENSITIVE_DATA_CONSENT_URL` et
`LOCATION_CONSENT_URL` (douze valeurs facultatives, HTTPS en production). Sans traduction configurée,
l’URL française est conservée et `document_language` vaut `fr`.
Les versions et `LEGAL_REVIEW_REFERENCE` gardent leurs exigences existantes.
Voir [Internationalisation](internationalization.md) pour la préférence du compte, les appareils et les consentements.
