# Tester l’API

[Documentation](../docs/README.md) · [Développement](../docs/development/development.md) · [Livraison](../docs/deployment/production-readiness.md)

Exécuter les commandes depuis la racine du dépôt. Utiliser des données fictives et une cible de
développement isolée. Les suites d’intégration modifient des données ; ne pas les diriger vers la production.

## Choisir les contrôles

| Niveau | Emplacement | Vérifie |
| --- | --- | --- |
| Unitaire | Modules `#[cfg(test)]` de `src/` | Règles métier, configuration, cryptographie et cycles de vie |
| Contrat HTTP | `*_contract.rs`, `http_contract.rs` | Statuts, JSON, validation, authentification et permissions |
| Inventaire | `route_inventory.rs` | Correspondance des routes Axum, du contrat et des collections Postman FR/EN |
| PostgreSQL | Suites activées par `postgres-integration` | Requêtes, contraintes, migrations, verrous et concurrence |
| Redis / S3 | `redis_integration`, `media_integration` | Adaptateurs sur services locaux |
| Codec | `photo_codec.rs`, `fixtures/photos/` | Conversion, formats, taille, métadonnées et gestion du processus Node |
| Comparaison | `contract_harness.rs`, `contract/corpus/` | Corpus explicite contre deux API isolées |
| Smoke | `scripts/smoke-api.sh` | Santé, authentification, validation et route inexistante sur l’API assemblée |

L’inventaire des routes ne remplace pas les tests de permissions ou de réponses JSON.
Les mesures de performance sont décrites dans [Performance SQL](../docs/database/sql-performance.md)
et [Test de charge](../docs/testing/load-testing.md).

## Contrôles sans stockages externes

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo check --locked --all-targets --no-default-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --lib --test http_contract --test route_inventory
```

WebAuthn est activé par défaut via `webauthn-probe`. `--no-default-features` vérifie la compilation
des outils sans WebAuthn ; le démarrage de l’API complète l’exige.
Cibler un domaine avec `--test <nom_du_fichier_sans_rs>`, par exemple :

```bash
cargo test --locked --test matches_contract --test mobile_auth_contract --test profiles_contract
```

Les tests HTTP de fournisseurs utilisent des serveurs simulés locaux. Leur réussite ne prouve pas
la livraison d’un SMS, d’un push ou la validité d’un achat sur appareil.

## PostgreSQL isolé

Le lanceur crée son propre PostgreSQL/PostGIS sur une adresse loopback et un port aléatoire.
Il génère des identifiants temporaires, prépare les rôles séparés et lance le migrateur réel sur
une base neuve. Il exécute les suites applicatives avec
`histae_app`, puis les fixtures qui créent leur propre base avec l’administrateur temporaire.
La base, les conteneurs et les identifiants temporaires sont supprimés à la fin.

Préparer localement l’image PostgreSQL définie dans le dépôt, puis compiler le migrateur :

```bash
sudo docker build --pull -t histae-postgres:18.7-postgis3.7.0 docker/postgres
cargo build --locked --bin db-migrate
sudo -v
python3 scripts/verify-continuation-postgres.py --sudo-docker \
  --native-migrator target/debug/db-migrate
```

Avec un accès Docker déjà administré, omettre `sudo -v` et `--sudo-docker`.
Cargo reste exécuté par l’utilisateur courant. Le lanceur vérifie la présence des images avant
création et refuse de télécharger implicitement une image manquante.

Pour cibler des suites sur un schéma neuf :

```bash
python3 scripts/verify-continuation-postgres.py --sudo-docker \
  --native-migrator target/debug/db-migrate \
  --test matches_postgres --test notifications_postgres
```

`--postgres-image` sélectionne une autre image locale compatible. Pour tester le migrateur d’une
image API reconstruite depuis les sources, omettre `--native-migrator` et préciser `--api-image`.
Le test PostgreSQL de `media_integration` est inclus ; son test S3 nécessite le stockage objet séparément.

`outbox_postgres` vérifie la stabilité du schéma après deux réexécutions, les identifiants des événements
et la réparation d’un index invalide.

Pour les privilèges des rôles, suivre [Rôles PostgreSQL](../docs/database/postgres-roles.md#verifier-la-separation).

## Redis, S3 et codec

Préparer la [pile dev](../docs/deployment/container-deployment.md#developpement-local), son bucket privé et
les adresses accessibles depuis l’hôte dans `.env`. Installer les dépendances du codec :

```bash
pnpm --dir tools/photo-codec install --frozen-lockfile
cargo test --locked --test photo_codec
cargo test --locked --features redis-integration --test redis_integration
cargo test --locked --features postgres-integration --test media_integration
```

Les fixtures doivent être locales. Si une suite utilise la file outbox de la pile dev, arrêter son
worker pour qu’il ne consomme pas les événements de test ; préférer le lanceur isolé pour les suites SQL.
Vérifier les prérequis de chaque suite avant d’exécuter toutes les fonctionnalités :

```bash
cargo test --locked --all-targets --all-features
```

## Vérificateur des achats et supervision

Le service privé Apple/Google possède ses propres tests et dépendances :

```bash
npm --prefix services/store-verifier ci
npm --prefix services/store-verifier test
```

Voir son [guide](../services/store-verifier/README.md), les [achats natifs](../docs/features/store-billing.md)
et les procédures de [supervision](../docs/operations/observability.md#test-controle-des-alertes) pour les règles
Prometheus, leur validation avec `promtool` et les essais contrôlés d’Alertmanager.

## Scénarios par fonctionnalité

| Fonctionnalité | Suites représentatives | Guide |
| --- | --- | --- |
| OTP et plafonds SMS | `otp_sweego_contract`, `otp_delivery`, `sms_safety_postgres` | [Protection SMS](../docs/features/sms-safety.md) |
| Numéros publics et exceptions | `phone_blocklist_postgres` | [Politique SMS](../docs/features/phone-blocklist.md) |
| Sessions et reprise de refresh | `mobile_auth_contract`, `mobile_identity` | [Reprise des sessions](../docs/features/refresh-recovery.md) |
| Profil, consentements et langues | `profiles_contract`, `profiles_postgres`, `localization_postgres` | [Internationalisation](../docs/api/internationalization.md) |
| Feed, pause et exclusions | `discovery_contract`, `discovery_postgres` | [Contrat HTTP](../docs/api/http-contract.md#decouverte-matchs-et-messages) |
| Matchs, messages et crédits | `matches_contract`, `matches_postgres`, `matches_inactivity_postgres` | [Continuations](../docs/features/continuation-and-rewards.md) |
| Notifications et préférences | `notifications_postgres`, `sse_contract` | [Notifications push](../docs/features/push-notifications.md) |
| Achats et revenus | `store_billing_postgres`, `admin_metrics_postgres`, `complimentary_premium_postgres` | [Abonnements](../docs/features/billing.md) |
| Revue Google Play | `play_review_postgres` | [Compte de revue](../docs/features/google-play-review.md) |
| Droits et effacement | `privacy_rights_postgres`, `account_erasure_postgres`, `web_privacy_postgres` | [Contrat HTTP](../docs/api/http-contract.md#droits-effacement-et-surete) |
| SQL et outbox | `query_performance_postgres`, `outbox_postgres`, `maintenance_outbox_postgres` | [Base de données](../docs/database/database.md) |

<a id="mode-pause"></a>

### Vérifier la pause de découverte

`discovery_postgres` couvre le refus du feed et du swipe pendant la pause, les replays, les règles
de confidentialité et les exclusions après reprise. Exécuter cette suite avec le lanceur isolé ;
`discovery_contract` vérifie les réponses HTTP et l’ordre des contrôles d’accès.

Les fournisseurs sont simulés dans ces scénarios. Les tests de récompenses publicitaires gardent
la vérification des signatures avec une clé de test ; ils ne désactivent pas les contrôles de production.
Les refus d’autorisation sous charge et variantes du feed sont décrits dans
[Validation approfondie](../docs/testing/load-testing-validation.md).

<a id="comparaison-de-deux-api"></a>

## Comparaison de deux API

Préparer deux déploiements et namespaces d’état distincts, avec bases, clés Redis et préfixes S3 isolés :

```bash
cargo run --locked --bin contract-compare -- \
  --corpus tests/contract/corpus/smoke.json \
  --reference-url http://127.0.0.1:8080/ --reference-state reference-test \
  --candidate-url http://127.0.0.1:8081/ --candidate-state candidate-test
```

Sortie : `0` parité, `1` différences, `2` configuration, corpus ou transport invalide.
`--allow-isolated-mutations` autorise les scénarios correspondants mais ne crée pas leur isolation.

## Vérifications sur une pile démarrée

```bash
bash scripts/smoke-api.sh
```

Le smoke accepte seulement une URL loopback. Pour le LAN et les parcours authentifiés, utiliser
[Postman](../postman/README.md). Pour les proxys, suivre [Vérifier Caddy et le tunnel](../docs/deployment/container-deployment.md#verifier-caddy-et-le-tunnel).

Les tests locaux ne couvrent pas à eux seuls WebAuthn dans le navigateur, les stores sur appareil,
la livraison FCM/Sweego réelle, la capacité en charge ou la restauration complète hors machine.
Suivre [Mise en production](../docs/deployment/production-readiness.md) pour ces validations.
