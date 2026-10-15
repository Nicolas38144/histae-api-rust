# Validation locale

[Documentation](../docs/README.md) · [Développement](../docs/development.md) · [Livraison](../docs/production-readiness.md)

Exécuter depuis la racine du dépôt. Les tests ne chargent pas le `.env` du projet NestJS.
Utiliser des fixtures fictives et des secrets de développement, jamais une base de production.

## Niveaux et limites de preuve

| Niveau | Emplacement | Vérifie |
| --- | --- | --- |
| Unitaire | Modules `#[cfg(test)]` de `src/` | Règles, configuration, transports, mocks et cycles de vie |
| Contrat | `*_contract.rs`, `http_contract.rs` | Statuts, JSON, validation, guards et permissions |
| Inventaire | `route_inventory.rs` | Les 109 couples méthode/chemin de `docs/http-contract.md` correspondent aux déclarations Axum |
| PostgreSQL | `*_postgres.rs`, `postgres_compatibility`, `postgres_locks` | SQL, contraintes, verrous, snapshots et concurrence réels |
| Redis / S3 | `redis_integration`, `media_integration` | Adaptateurs sur services locaux |
| Codec | `photo_codec.rs`, `fixtures/photos/` | Formats, taille, métadonnées, corruption et arrêt du processus Node |
| Comparaison | `contract_harness.rs`, `contract/corpus/` | Corpus explicite contre deux API isolées |
| Smoke | `scripts/smoke-api.sh` | Binaire HTTP assemblé : santé, auth, validation et route inexistante |

L’inventaire analyse les sources, pas le montage final du routeur, les schémas JSON ni les autorisations.
Une intégration désactivée par feature n’est pas une preuve de validation de sa dépendance.

## Contrôles sans stockages externes

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo check --locked --all-targets --no-default-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --lib --test http_contract --test route_inventory
```

WebAuthn est activé par défaut via `webauthn-probe`, avec ses prérequis natifs.
`--no-default-features` contrôle la compilation des outils/tests sans WebAuthn ; l’API complète l’exige.
Les tests de contrat par domaine peuvent également être ciblés avec `--test <nom_du_fichier_sans_rs>`.

## Suite complète

Préparer [la pile dev](../docs/container-deployment.md#developpement-local), avec les adresses **hôte**
dans `.env`, puis installer le codec :

```bash
pnpm --dir tools/photo-codec install --frozen-lockfile
cargo test --locked --all-targets --all-features
```

Exemples ciblés :

```bash
cargo test --locked --features postgres-integration --test matches_postgres --test query_performance_postgres
cargo test --locked --features redis-integration --test redis_integration
cargo test --locked --test photo_codec
```

`postgres-integration` et `redis-integration` activent les suites concernées. Les tests codec doivent
pouvoir lire le store pnpm et lancer Node. Vérifier le résultat de chaque suite, pas seulement la compilation.

## Isolation

Les fixtures PostgreSQL contrôlent la cible de développement et utilisent des schémas/données isolés.
Le claim outbox parcourt toute une file : son test crée `histae_outbox_test_<UUID>`, applique le migrateur
réel et supprime cette base après fermeture, y compris si une assertion échoue. Le rôle local doit pouvoir
créer une base ; la file de la base de développement partagée n’est pas nettoyée par ce test.

Redis utilise sa base logique et des clés de test ; S3 des objets dédiés. Ne pas arrêter les dépendances
partagées pour simuler une panne ni remplacer le nettoyage ciblé par une purge Docker globale.
Pour les garde-fous précis, consulter les fixtures et le [guide de base de données](../docs/database.md).

Les comptes applicatifs Docker dev et production sont limités. Les fixtures qui créent des schémas ou
bases nécessitent un compte administrateur de développement ; l'API utilise toujours `histae_app`.
`sudo bash scripts/verify-postgres-roles.sh` valide l'installation neuve avec le migrateur réel et les
permissions applicatives dans un conteneur temporaire. Voir [Rôles PostgreSQL](../docs/postgres-roles.md).

## Régressions couvertes

| Sujet | Exemples |
| --- | --- |
| Auth / transport | JWT aux bornes temporelles, claims invalides, IP socket/X-Forwarded-For falsifié, coercition numérique, corps JSON invalides ou trop grands |
| Consentements / administration | Onboarding des appareils/SSE, données sensibles, sessions admin, rôles et consultations auditées |
| Cycle de vie | Drain après échec, worker terminé inopinément, annulation au drop, SSE bloqué, expiration prioritaire du JWT et scrape interrompu |
| Abonnements Stripe / revenus | Requêtes Checkout figées, reprises bornées, concurrence, abonnement distant sans webhook, prix historiques, projections dépassées et montants des factures payées |
| Fournisseurs / exports | Durées OAuth hors plage, diagnostics Stripe préservés pendant effacement, date UTC et bigint exportés sans perte, nettoyage des fichiers temporaires |
| SQL | Filtres et pages admin, compte sans profil, compteurs, curseurs, fiche/audit et équivalence des agrégats de files |
| Clôture de match | Participants uniquement, rejeux, fermeture concurrente, messages en attente refusés, événement SSE et rétention/quota conservés |
| Continuations / récompenses | Envoi suspendu, choix privés, coût explicite, réservations concurrentes, double débit évité, deux participants, changement de semaine/plan/consentement, signature ECDSA et récompense unique plafonnée |
| Migrateur | Base neuve/peuplée, idempotence, index concurrents et réparation d’un index invalide |
| Confidentialité | `Debug` expurgé de configuration, erreurs/labels bornés et absence de valeurs sensibles |

Les mesures de performance restent séparées des assertions fonctionnelles : voir [Performance SQL](../docs/sql-performance.md).
Les [contrôles sous charge](../docs/load-testing-validation.md) testent aussi les refus de l’API
assemblée et les variantes du feed. Ils utilisent uniquement les comptes fictifs d’une API temporaire dev.

<a id="comparaison-de-deux-api"></a>

## Comparaison de deux API

Le binaire `contract-compare` exige deux URLs et namespaces d’état distincts. Préparer également leurs
bases, clés Redis et préfixes S3 isolés. Exemple sur deux déploiements locaux déjà disponibles :

```bash
cargo run --locked --bin contract-compare -- \
  --corpus tests/contract/corpus/smoke.json \
  --reference-url http://127.0.0.1:8080/ --reference-state reference-test \
  --candidate-url http://127.0.0.1:8081/ --candidate-state candidate-test
```

Sortie : `0` parité, `1` différences, `2` configuration/corpus/transport invalide.
`--allow-isolated-mutations` autorise les scénarios correspondants mais ne crée pas leur isolation.

## Validations externes

Les tests locaux ne prouvent ni les cérémonies WebAuthn du navigateur, ni la livraison réelle des fournisseurs,
ni la capacité en charge, ni une restauration complète hors machine. Exécuter et conserver ces preuves selon
[Mise en production](../docs/production-readiness.md).

## Crédits et récompenses

Le [guide](../docs/continuation-and-rewards.md) décrit les règles et la validation AdMob restante. Les tests `matches_postgres` couvrent les wallets, récompenses et consentements avec le rôle applicatif, sans appel OTP et sans publicité réelle. Les tests ECDSA utilisent une clé de test générée en mémoire ; ils ne désactivent jamais la vérification de production. Les résultats et limites sont regroupés dans [Validation des continuations](../docs/continuation-validation.md).

Pour ces suites, utiliser le lanceur isolé afin que le worker de la pile dev ne récupère pas les événements des fixtures :

```bash
# L’image API doit d’abord être construite depuis le code actuel.
sudo -v
python3 scripts/verify-continuation-postgres.py --sudo-docker
```

Cargo reste exécuté par l’utilisateur courant ; `sudo` s’applique uniquement aux commandes Docker. Avec un accès Docker déjà disponible, omettre `sudo -v` et `--sudo-docker`. Le lanceur crée un PostgreSQL avec une publication sur une adresse loopback et un port aléatoire, teste le migrateur livré sur base neuve et ancienne, puis exécute les suites avec `histae_app`. Ses fixtures, conteneur, tmpfs et mots de passe temporaires sont supprimés à la fin. Il ne se connecte pas au PostgreSQL de la pile dev.

## Vérifications HTTP derrière Caddy

Le [guide Caddy](../docs/caddy-migration.md) décrit les commandes de validation des configurations et les
contrôles après déploiement : authentification, erreurs HTTP, SSE et photos signées. Les résultats de la
campagne locale de migration sont conservés dans ce guide.

## Signature des récompenses publicitaires

Les tests `advertising::ssv::tests` couvrent la signature, le cache périmé, l’actualisation concurrente et
les limites d’admission, avec des clés générées en mémoire et sans appel à Google.
