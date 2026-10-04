# Histae API Rust

Backend Histae organisé par domaines métier, avec Axum, Tokio et SQLx. PostgreSQL conserve l’état transactionnel, Redis les quotas et le relais SSE, et un stockage compatible S3 les photos privées. L’API, l’outbox et les maintenances s’exécutent dans des processus distincts.

Le dépôt possède sa configuration, ses migrations, son codec photo et ses manifests Docker. Le codec utilise encore un processus Node/Sharp autonome pour préserver la compatibilité JPEG, PNG, WebP, HEIC et HEIF ; il ne charge aucun fichier du dépôt NestJS.

## Organisation

```text
src/
  bin/              # Points d’entrée CLI minces
  app/              # Assemblage des dépendances et cycle de vie des processus
  config/           # Configuration typée, validation, secrets expurgés
  http/             # Extracteurs, erreurs, sécurité et middleware partagés
  infra/            # PostgreSQL, migrations, verrous, Redis, cryptographie
  identity/         # Sessions mobiles, OTP et authentification admin WebAuthn
  profiles/         # Profil et préférences
  catalog/          # Traits, questions et plans
  discovery/        # Feed et décisions de swipe
  matches/          # Matchs, messages et expiration
  media/            # Photos, codec et stockage objet
  moderation/       # Modération de texte et de photos
  billing/          # Stripe, webhooks et réconciliation
  notifications/    # Notifications durables, push et SSE
  privacy/          # Consentements, droits, export et effacement
  reports/          # Signalements
  administration/   # Gestion administrative, photos et métriques métier
  outbox/           # Livraison des effets, retries et dead letters
  operations/       # Logs, métriques privées et suivi des maintenances
  shared/           # Horloge et fonctions texte communes
tests/             # Contrats HTTP et intégrations, nommés par fonctionnalité
db/                # Schéma, catalogues et migrations SQL figés
docker/            # Configuration des services conteneurisés
services/          # Service autonome de modération photo
tools/             # Codec photo et génération de ses fixtures
scripts/           # Smoke et restauration de développement
observability/     # Supervision privée
docs/              # Architecture, contrat HTTP et exploitation
  migration/       # Historique S03–S29 et décisions de migration
```

Les responsabilités et règles de dépendance sont décrites dans [docs/architecture.md](docs/architecture.md). Les tests suivent [tests/README.md](tests/README.md).

## Développement

Prérequis : Rust compatible avec `Cargo.toml`, Node.js 22+, pnpm pour le codec, Docker via WSL sous Windows. WebAuthn utilise OpenSSL ; voir [les prérequis Windows](docs/windows-openssl.md).

Le fichier `.env` de ce dépôt est chargé automatiquement et reste ignoré. Les variables du terminal ont priorité. Les secrets et les valeurs fournisseurs ne doivent pas apparaître dans les logs. La configuration locale et les ports Docker sont détaillés dans [le guide de déploiement](docs/container-deployment.md).

Depuis la racine du dépôt, après préparation de `.env` :

```powershell
wsl.exe docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d --build --wait
./scripts/smoke-api.ps1
```

Pour exécuter les binaires sur l’hôte, avec les services locaux disponibles et sans API concurrente sur le port :

```powershell
pnpm --dir tools/photo-codec install --frozen-lockfile
cargo run --locked --bin db-migrate
cargo run --locked --features webauthn-probe --bin api
```

Le worker `outbox` et la passe `maintenance` sont des binaires séparés. `admin-bootstrap`, `storage-init` et `contract-compare` sont des outils d’exploitation. `--check-config` vérifie la configuration des binaires applicatifs sans lancer leur activité. Le nom de feature `webauthn-probe` est conservé pour compatibilité ; il active le moteur WebAuthn utilisé par l’API.

## Validation

```powershell
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

La dernière commande exige les dépendances locales et le codec installés. Les protections d’isolation et les commandes ciblées sont documentées dans [tests/README.md](tests/README.md).

## Références

- [Contrat HTTP : 100 couples méthode/chemin](docs/http-contract.md)
- [Architecture et conventions](docs/architecture.md)
- [Conteneurisation et déploiement](docs/container-deployment.md)
- [Observabilité privée](docs/observability.md)
- [Historique et décisions de migration](docs/migration/history.md)
- [Bascule et validations externes restantes](docs/migration/s29-cutover.md)

La migration des modules ne remplace pas les validations fournisseurs, les cérémonies WebAuthn réelles, les essais de charge et les contrôles préalables à la production.
