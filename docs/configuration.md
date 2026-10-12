# Configuration

[Index](README.md) · [Développement](development.md) · [Déploiement](container-deployment.md)

La référence exécutée est [AppConfig](../src/config/loader.rs), complétée par les
[validateurs](../src/config/validation.rs). Une valeur invalide empêche le démarrage avec un code normalisé ;
les valeurs et secrets ne sont pas recopiés dans les logs.

## Chargement et priorité

### Binaires Rust sur l’hôte

`AppConfig::from_env()` charge `.env` avec `dotenvy`, puis lit l’environnement du processus. Une variable
déjà définie dans le terminal garde la priorité. `.env.production` n’est pas sélectionné automatiquement
par `ENV=production` : préparer l’environnement du processus explicitement ou utiliser Compose.
Lancer les commandes depuis la racine évite de charger par erreur un `.env` d’un dossier parent.

`ENV` est obligatoire et accepte `development`, `production` et `test`. `ENV=development` derrière
un domaine HTTPS reste du développement, avec ses règles et fournisseurs configurés pour ce mode.

### Docker Compose

| Mécanisme | Effet |
| --- | --- |
| `--env-file .env.production` | Fournit les valeurs d’interpolation `${…}` des manifests. |
| `env_file: ${HISTAE_ENV_FILE:-.env}` | Injecte le fichier choisi dans les conteneurs applicatifs. |
| `environment:` d’un service | Remplace les mêmes variables reçues via `env_file`. |

Les variables exportées du terminal peuvent remplacer les valeurs d’interpolation.
Pour la production, écrire **`HISTAE_ENV_FILE=.env.production` dans `.env.production`**, puis utiliser
`--env-file .env.production` dans toutes les commandes. Sans ce réglage, le manifest de base injecte
encore `.env`. Le dashboard et le reverse proxy ont leurs propres fichiers d’environnement.

Le `.env` local et `.secrets/` sont ignorés par Git. Prévoir aussi une exclusion pour `.env.production`
avant d’y stocker des secrets : ce nom n’est pas couvert par le `.gitignore` actuel.

Après une modification, recréer les conteneurs concernés : `restart` ne recharge pas leur environnement.
Valider sans afficher les secrets avec `docker compose … config --quiet`.

<a id="configuration-locale-minimale"></a>

## Configuration locale minimale

Créer `.env` et remplacer les cinq placeholders de secrets par cinq valeurs distinctes.
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
POSTGRES_USER=histae
POSTGRES_PASSWORD=REPLACE_POSTGRES_PASSWORD
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
Les documents juridiques omis en développement reçoivent une version `development-unversioned` et des
URLs `example.invalid` ; les remplacer pour tester un parcours réaliste.

## Groupes de variables

| Sujet | Variables principales | Règles |
| --- | --- | --- |
| HTTP | `PORT`, `CORS_ORIGINS`, `TRUST_PROXY` | Origines explicites ; proxies précis en production, jamais `true`. |
| JWT | `JWT_SECRET`, `JWT_ACTIVE_KID`, `JWT_PREVIOUS_KEYS`, `JWT_ACCESS_TTL`, `JWT_REFRESH_TTL` | Clés distinctes des clés téléphone ; anciens `kid` à conserver pendant la durée de validité des tokens concernés. |
| Téléphone | `PHONE_ENCRYPTION_KEY`, `PHONE_HASH_KEY` | Deux clés de 32 octets distinctes. Leur remplacement exige une migration des données. |
| PostgreSQL | `POSTGRES_HOST`, `POSTGRES_PORT`, `POSTGRES_USER`, `POSTGRES_PASSWORD`, `POSTGRES_DB`, `POSTGRES_SSLMODE`, `POSTGRES_POOL_MAX` | Pool par processus ; TLS obligatoire en production. |
| Timeouts SQL | `POSTGRES_CONNECT_TIMEOUT`, `POSTGRES_IDLE_TIMEOUT`, `POSTGRES_STATEMENT_TIMEOUT`, `POSTGRES_IDLE_TRANSACTION_TIMEOUT` | Défauts : `5s`, `30s`, `15s`, `30s`. |
| Redis | `RATE_LIMIT_STORE`, `REDIS_ADDR`, `REDIS_PASSWORD`, `REDIS_DB`, `REDIS_TLS`, `REDIS_CONNECT_TIMEOUT`, `REDIS_COMMAND_TIMEOUT` | Production : store Redis, TLS et mot de passe. Adresse `hôte:port`, sans schéma. |
| S3 | `OBJECT_STORAGE_ENDPOINT`, `OBJECT_STORAGE_REGION`, `OBJECT_STORAGE_BUCKET`, `OBJECT_STORAGE_ACCESS_KEY`, `OBJECT_STORAGE_SECRET_KEY`, `OBJECT_STORAGE_FORCE_PATH_STYLE` | HTTPS et clés explicites en production ; nom accessible aux serveurs et clients pour les URLs signées. |
| Photos | `PHOTO_MODERATION_PROVIDER`, `PHOTO_MODERATION_ENDPOINT`, `PHOTO_MODERATION_TOKEN`, `PHOTO_MODERATION_TIMEOUT`, `PHOTO_MODERATION_MIN_SHARPNESS`, `PHOTO_MODERATION_NSFW_REVIEW_THRESHOLD` | `disabled` ou `local_http` ; token d’au moins 32 octets avec `local_http`. |
| OTP | `SMS_PROVIDER`, `SWEEGO_API_KEY`, `SWEEGO_SMS_SENDER_ID`, `SWEEGO_SMS_REGION`, `SWEEGO_API_URL`, `SWEEGO_TIMEOUT`, `OTP_TTL`, `SWEEGO_WEBHOOK_SECRET` | Sweego obligatoire en production ; région `FR`. Secret de webhook fourni par Sweego, distinct de la clé API. |
| Stripe | `BILLING_PROVIDER`, `STRIPE_SECRET_KEY`, `STRIPE_WEBHOOK_SECRET`, `STRIPE_PREMIUM_PRODUCT_ID`, `STRIPE_PREMIUM_MONTHLY_PRICE_ID`, `STRIPE_PREMIUM_ANNUAL_PRICE_ID` | Identifiants complets ; clés `sk_test_…` en développement, `sk_live_…` en production. |
| Redirections Stripe | `STRIPE_CHECKOUT_SUCCESS_URL`, `STRIPE_CHECKOUT_CANCEL_URL`, `STRIPE_PORTAL_RETURN_URL` | HTTPS ; succès contient `{CHECKOUT_SESSION_ID}`. |
| Réconciliation Stripe | `STRIPE_RECONCILIATION_INTERVAL`, `STRIPE_RECONCILIATION_FRESHNESS`, `STRIPE_RECONCILIATION_BATCH_SIZE`, `STRIPE_TIMEOUT` | Défauts : `5m`, `1h`, 25 éléments, `10s` ; maintenance pour programmer, outbox pour exécuter. |
| Options Stripe | `STRIPE_AUTOMATIC_TAX`, `STRIPE_ALLOW_PROMOTION_CODES`, `STRIPE_MAX_NETWORK_RETRIES` | Défauts : `false`, `false`, `2` ; respecter les fenêtres d’idempotence du contrat. |
| Push | `PUSH_PROVIDER`, `FIREBASE_PROJECT_ID`, `FIREBASE_CLIENT_EMAIL`, `FIREBASE_PRIVATE_KEY`, `FIREBASE_TOKEN_URI`, `PUSH_TIMEOUT` | `disabled` ou `fcm` ; compte de service complet pour FCM. |
| Admin | `ADMIN_WEBAUTHN_ORIGIN`, `ADMIN_WEBAUTHN_RP_ID`, `ADMIN_WEBAUTHN_RP_NAME`, `ADMIN_SESSION_IDLE_TTL`, `ADMIN_SESSION_ABSOLUTE_TTL`, `ADMIN_RECENT_AUTH_TTL` | Voir [Administration](administration.md) ; HTTPS en production. |
| Enrôlement | `ADMIN_WEBAUTHN_CHALLENGE_TTL`, `ADMIN_WEBAUTHN_BOOTSTRAP_TTL` | Défauts : `5m` et `15m`, usage unique. |
| Maintenance/export | `MAINTENANCE_MODE`, `MATCH_MAINTENANCE_BATCH_SIZE`, `MATCH_MAINTENANCE_MAX_BATCHES`, `OUTBOX_PURGE_BATCH_SIZE`, `OUTBOX_PURGE_MAX_BATCHES`, `DATA_EXPORT_PAGE_SIZE`, `DATA_EXPORT_MAX_BYTES`, `DATA_EXPORT_MAX_CONCURRENCY` | Traitements bornés ; voir [Exploitation](operations.md). |
| Effacement | `ACCOUNT_DELETION_TOKEN_TTL` | Défaut `10m`, jeton à usage unique ; workflow asynchrone. |
| Métriques | `METRICS_ENABLED`, `METRICS_HOST`, `METRICS_PORT`, `METRICS_TOKEN` | Listener privé, port `9091` ; token distinct d’au moins 32 octets. |

Les limites dédiées `RATE_LIMIT_*` possèdent une variable de fenêtre suffixée `_WINDOW`.
Le [contrat HTTP](http-contract.md#erreurs-et-limites-de-debit) décrit les quotas et codes publics.
Les durées utilisent une unité, par exemple `5s`, `1m` ou `8h`.

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

`HISTAE_API_IMAGE`, `HISTAE_POSTGRES_IMAGE`, `HISTAE_PHOTO_MODERATION_IMAGE`, `HISTAE_ENV_FILE`, `HISTAE_API_BIND_IP`,
`HISTAE_API_HOST_PORT`, `HISTAE_API_DNS_SERVER`, `HISTAE_BACKEND_NETWORK`, `HISTAE_EDGE_NETWORK` et
`HISTAE_STORAGE_HOST` pilotent l’orchestration. Les overrides sont décrits dans [Déploiement](container-deployment.md).

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
