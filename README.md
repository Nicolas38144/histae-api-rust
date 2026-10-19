# Histae API Rust

Backend Histae construit avec Axum, Tokio et SQLx. PostgreSQL conserve les données transactionnelles,
Redis les quotas et le relais SSE, et un stockage compatible S3 les photos privées. L’API, le worker
outbox et les maintenances sont exécutés séparément.

Le dépôt contient son schéma PostgreSQL, ses binaires, son image Docker et ses services locaux.
Le codec photo utilise un processus Node/Sharp autonome, livré dans l’image API.

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
db/                # Schéma PostgreSQL consolidé, données initiales et script de suppression
docker/            # Configuration des services conteneurisés
services/          # Service autonome de modération photo
tools/             # Codec photo et génération de ses fixtures
scripts/           # Smoke, restauration et données de charge de développement
  install-debian-13.sh # Prérequis de développement Debian 13
observability/     # Supervision privée
docs/              # Architecture, contrat HTTP et exploitation
  migration/       # Historique S03–S29 et décisions de migration
```

Les responsabilités et règles de dépendance sont décrites dans [docs/architecture.md](docs/architecture.md). Les tests suivent [tests/README.md](tests/README.md).

## Démarrer en développement

Toutes les commandes s’exécutent depuis la racine du dépôt, sauf indication contraire.
Sur Debian 13, installer les prérequis puis préparer `.env` selon le
[guide de développement](docs/development.md) et la [configuration](docs/configuration.md).

```bash
bash scripts/install-debian-13.sh
source "$HOME/.profile"
bash scripts/prepare-observability-secrets.sh
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d --build --wait
bash scripts/smoke-api.sh
```

L’API locale écoute par défaut sur `http://127.0.0.1:8080`. Rust 1.88.0 et son rust-analyzer sont
fixés par [rust-toolchain.toml](rust-toolchain.toml). WebAuthn est activé par défaut dans Cargo.
La pile dev inclut Prometheus, Grafana et Alertmanager sans publier leurs ports.
L’accès superadmin depuis le dashboard est décrit dans le [guide de supervision](docs/observability.md).

## Documentation

L’[index des guides](docs/README.md) permet de choisir une procédure par usage.

| Besoin | Guide |
| --- | --- |
| Développer, compiler, créer des données fictives | [Développement](docs/development.md) |
| Comprendre `.env`, les secrets et les overrides | [Configuration](docs/configuration.md) |
| Déployer localement, sur le LAN ou derrière Cloudflare | [Déploiement Docker](docs/container-deployment.md) |
| Initialiser et sauvegarder PostgreSQL | [Base de données](docs/database.md) |
| Enrôler une passkey et gérer les administrateurs | [Administration](docs/administration.md) |
| Exécuter les workers et planifier la maintenance | [Exploitation](docs/operations.md) |
| Intégrer les 111 routes et leurs autorisations | [Contrat HTTP](docs/http-contract.md) |
| Importer les collections française ou anglaise | [Postman](postman/README.md) |
| Diagnostiquer les alertes et les requêtes lentes | [Supervision](docs/observability.md), [SQL](docs/sql-performance.md) |
| Tester la charge et le tunnel Cloudflare | [Test de charge](docs/load-testing.md) |
| Suivre les blocages avant le lancement | [Feuille de route production](roadmap-production.md) |
| Valider une livraison | [Tests](tests/README.md), [Mise en production](docs/production-readiness.md) |
| Comprendre les modules et leurs responsabilités | [Architecture](docs/architecture.md) |

## Validation rapide

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --lib --test http_contract --test route_inventory
```

La suite complète exige PostgreSQL, Redis, S3 et le codec installés : voir les
[niveaux de validation et règles d’isolation](tests/README.md). La réussite des tests locaux ne remplace
pas les essais des fournisseurs, de WebAuthn dans le navigateur, de charge et de restauration.

## Continuations et récompenses

Le [guide des crédits](docs/continuation-and-rewards.md) décrit la pause de la messagerie, les décisions privées, les réservations des deux participants et la vérification serveur AdMob. Le fournisseur publicitaire est désactivé par défaut ; les interfaces mobiles et les callbacks réels doivent être validés avant son activation. [Résultats des tests](docs/feature-audit.md).
