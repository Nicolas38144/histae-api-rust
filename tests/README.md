# Validation locale

Exécuter les commandes depuis la racine de `histae-api-rust`. Les tests ne chargent pas le `.env` du dépôt NestJS.
Les UUID de fixtures sont générés ; ne pas introduire d’identifiants personnels ou de secrets fournisseurs réels.

## Niveaux de tests

| Niveau | Emplacement | Preuve |
| --- | --- | --- |
| Unitaire | Modules `#[cfg(test)]` dans `src/` | Règles isolées, configuration, codecs de transport et cycle de vie |
| Contrat | `tests/*_contract.rs`, `http_contract.rs` | Statuts, JSON, validation, authentification et autorisation |
| Inventaire | `route_inventory.rs` | 100 couples méthode/chemin documentés et enregistrements présents dans les sources |
| Intégration | `*_postgres.rs`, `postgres_*`, `redis_integration`, `media_integration`, etc. | SQL réel, contraintes, concurrence et dépendances locales |
| Codec | `photo_codec.rs` et `fixtures/photos/` | JPEG/PNG/WebP/HEIC/HEIF, taille, métadonnées, corruption et arrêt du processus |
| Comparaison | `contract_harness.rs` et `contract/corpus/` | Comparaison explicite de deux API avec états isolés |

L’inventaire analyse les déclarations de routes ; il ne prouve pas à lui seul le montage du routeur final, les
schémas JSON ou les permissions. Les tests de contrat et le smoke du binaire assemblé complètent cette preuve.

## Commandes

Contrôles statiques et tests unitaires sans stockages externes :

```powershell
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --lib --all-features
```

Suite complète après préparation de PostgreSQL, Redis, S3 et du codec :

```powershell
pnpm --dir tools/photo-codec install --frozen-lockfile
cargo test --locked --all-targets --all-features
```

Exemples ciblés :

```powershell
cargo test --locked --test http_contract --test route_inventory
cargo test --locked --features postgres-integration --test matches_postgres
cargo test --locked --features redis-integration --test redis_integration
cargo test --locked --test photo_codec
```

Les features `postgres-integration` et `redis-integration` activent les suites concernées. WebAuthn exige la feature
`webauthn-probe` et ses prérequis natifs. Une exécution sans ces features ne valide pas les suites désactivées.

## Isolation et infrastructure

Utiliser uniquement la pile locale décrite dans [le guide Docker](../docs/container-deployment.md).
Les fixtures PostgreSQL contrôlent la cible de développement et utilisent leurs schémas ou données isolés.
Le test de claim outbox parcourt une file entière : il crée donc une base `histae_outbox_test_<UUID>` sur le serveur
local vérifié, applique le véritable migrateur et la supprime après fermeture du pool, même si une assertion échoue.
Le rôle PostgreSQL local utilisé pour ce test doit pouvoir créer une base. La base de développement partagée et
ses événements ne sont pas nettoyés par ce test.
Redis utilise sa base logique de test et des clés isolées ; S3 utilise les objets de test dédiés. Ne pas arrêter
les conteneurs partagés pour simuler une panne et ne pas remplacer le nettoyage ciblé par un reset global.

Sous Windows, gérer Docker via WSL. Les tests du codec doivent pouvoir lire les hardlinks du store pnpm ;
une restriction de sandbox peut provoquer `EPERM` alors que Node et le codec sont installés.
Les avertissements de lien OpenSSL `LNK4099` ne constituent pas une réussite ou un échec des tests : vérifier
le code de sortie et les résultats des suites.

Les tests de cycle de vie couvrent le drain après échec d’une tâche, la sortie imprévue du worker, l’annulation
au drop, la fermeture d’un SSE bloqué sur une lecture de session et l’arrêt d’un scrape de métriques bloqué.
L’expiration du JWT ferme aussi le SSE pendant une lecture de session bloquée ; un contrôle de révocation
prêt est traité avant un heartbeat ou un événement prêt simultanément. Ces bornes ont des tests dédiés.
La configuration a aussi un test empêchant l’exposition des clés et valeurs d’environnement par `Debug`.

Les régressions de migration couvrent aussi les JWT signés aux limites temporelles et avec des claims
absents/mal formés, les IP réelles sur socket TCP malgré un X-Forwarded-For falsifié, la validation des
corps sur les routes sans extracteur JSON, les conversions numériques des DTO, l’onboarding des appareils
et du SSE, ainsi que les règles Unicode de modération. Les suites PostgreSQL vérifient le format de l’export
(date UTC et bigint en chaînes), la conservation du diagnostic de réconciliation Stripe pendant l’effacement
et l’accès aux mutations de modération avec une session admin valide mais non récente. Les tests des clients
vérifient l’instrumentation agrégée sans changer les résultats et les durées OAuth hors plage sont refusées.

Les fournisseurs réels, les cérémonies WebAuthn dans le navigateur, la charge et la restauration ne sont pas
prouvés par des doubles de test. Leurs critères restent dans [le guide de bascule](../docs/migration/s29-cutover.md).
