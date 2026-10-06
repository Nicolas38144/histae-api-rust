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
db/                # Schéma PostgreSQL consolidé, données initiales et script de suppression
docker/            # Configuration des services conteneurisés
services/          # Service autonome de modération photo
tools/             # Codec photo et génération de ses fixtures
scripts/           # Smoke et restauration de développement
  install-debian-13.sh # Prérequis de développement Debian 13
observability/     # Supervision privée
docs/              # Architecture, contrat HTTP et exploitation
  migration/       # Historique S03–S29 et décisions de migration
```

Les responsabilités et règles de dépendance sont décrites dans [docs/architecture.md](docs/architecture.md). Les tests suivent [tests/README.md](tests/README.md).

## Développement

Sur **Debian 13 (trixie)**, préparer une machine de développement avec :

```bash
bash scripts/install-debian-13.sh
source "$HOME/.profile"
```

Le script demande `sudo` pour les paquets Debian et Docker, installe Rust 1.88 avec Cargo, rustfmt et Clippy, les outils de compilation d’OpenSSL, Node.js 22.22.1, pnpm 10.28.2, Docker Engine/Compose, `curl`, `jq` et `openssl`. Il vérifie l’archive Node avec le manifeste SHA-256 officiel. Il ne crée aucun secret, ne modifie aucune base/volume et n’ajoute pas l’utilisateur au groupe `docker` (qui donne des privilèges équivalents à root). Préparer d’abord un compte non-root ayant accès à `sudo` ; en cas de paquets Docker incompatibles déjà installés, le script s’arrête avant de les retirer. Pour travailler avec Docker, utiliser `sudo docker …` ou un mode d’accès administré séparément. Le script suit les procédures officielles de [Docker pour Debian](https://docs.docker.com/engine/install/debian/), [Rustup](https://www.rust-lang.org/tools/install) et la [distribution Node.js 22.22.1](https://nodejs.org/download/release/v22.22.1/).

La machine de production exécute l’image Docker construite par ce dépôt ; l’installation de Rust, Node ou pnpm sur l’hôte n’y est pas nécessaire. Le service de modération Python est également construit en conteneur.

Le fichier `.env` de ce dépôt est chargé automatiquement et reste ignoré. Les variables du terminal ont priorité. Les secrets et les valeurs fournisseurs ne doivent pas apparaître dans les logs. La configuration locale et les ports Docker sont détaillés dans [le guide de déploiement](docs/container-deployment.md).

`db/001_schema_postgres.sql` contient le schéma complet. Le migrateur l'installe sur une base vide et vérifie les objets attendus sur une base déjà initialisée ; il retire l'ancienne table `schema_migrations` si elle existe. `db/drop_postgres.sql` supprime tous les objets du schéma `public` de la base ciblée : sauvegarder les données avant de l'exécuter manuellement.

Depuis la racine du dépôt, après préparation de `.env` :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d --build --wait
bash scripts/smoke-api.sh
```

Pour exécuter les binaires sur l’hôte, avec les services locaux disponibles et sans API concurrente sur le port :

```bash
pnpm --dir tools/photo-codec install --frozen-lockfile
cargo run --locked --bin db-migrate
cargo run --locked --features webauthn-probe --bin api
```

Le worker `outbox` et la passe `maintenance` sont des binaires séparés. `admin-bootstrap`, `storage-init` et `contract-compare` sont des outils d’exploitation. `--check-config` vérifie la configuration des binaires applicatifs sans lancer leur activité. Le nom de feature `webauthn-probe` est conservé pour compatibilité ; il active le moteur WebAuthn utilisé par l’API.

## Validation

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

La dernière commande exige les dépendances locales et le codec installés. Les protections d’isolation et les commandes ciblées sont documentées dans [tests/README.md](tests/README.md).
Le [workflow GitHub Actions](.github/workflows/rust-checks.yml) exécute le formatage, Clippy, la compilation sans feature, les tests unitaires et les contrats HTTP sans démarrer ces dépendances.

## Références

- [Corrections et prérequis de mise en production](docs/production-readiness.md)
- [Contrat HTTP : 100 couples méthode/chemin](docs/http-contract.md)
- [Architecture et conventions](docs/architecture.md)
- [Conteneurisation et déploiement](docs/container-deployment.md)
- [Observabilité privée](docs/observability.md)
- [Historique et décisions de migration](docs/migration/history.md)
- [Bascule et validations externes restantes](docs/migration/s29-cutover.md)

La migration des modules ne remplace pas les validations fournisseurs, les cérémonies WebAuthn réelles, les essais de charge et les contrôles préalables à la production.

## Corrections de parité et espace de compilation

Le suivi des corrections de la revue se trouve dans [production-readiness.md](docs/production-readiness.md).
Les prérequis fournisseur, juridiques et d’exploitation y sont distingués des validations locales.

Les profils de développement et de test utilisent des symboles réduits (`debug = 1`) et désactivent
le cache incrémental. Le dossier `target/` contient uniquement des artefacts reconstructibles ;
`cargo clean` les supprime, puis Cargo recompile au prochain lancement. Cela inclut les dépendances
natives comme OpenSSL : réserver ce nettoyage complet aux besoins d’espace, pas à chaque test.
