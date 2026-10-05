# S28 — campagne de parité

> Compte rendu historique de ce lot. Pour les commandes et l’arborescence actuelles, consulter le [README](../../README.md) et l’[architecture](../architecture.md).

## Périmètre et méthode

La référence fonctionnelle reste le dépôt NestJS, principalement `routes.md`, les DTO, les services, les
repositories et les tests. Le fichier [http-contract.md](../http-contract.md) en est la copie de travail côté Rust : il
recense les chemins complets, les entrées, les réponses, les statuts, les permissions et les principaux effets de
bord sans rendre le futur dépôt autonome dépendant du code TypeScript.

La campagne utilise trois niveaux de preuve complémentaires :

1. `tests/route_inventory.rs` compare les couples méthode/chemin du contrat aux enregistrements Axum. Il normalise
   les paramètres Axum `{id}` en `:id`, refuse toute route manquante ou non documentée et fige le total à 100.
2. Les tests de contrat appellent les routeurs composables avec des stores contrôlés et vérifient validation,
   authentification, autorisation, format JSON, statuts, erreurs publiques, idempotence et cas limites.
3. Les tests d'intégration exercent les transactions et contraintes sur le vrai schéma PostgreSQL, Redis, le stockage
   S3 local et le codec photo de production. Les fixtures utilisent des UUID v4 générés et nettoient uniquement leurs
   propres données.

Une réponse HTTP seule ne prouve pas les effets : les suites PostgreSQL inspectent aussi les lignes, verrous,
checkpoints, événements outbox et reprises. Les suites S3 vérifient les objets et leur cycle de vie.

## Écart découvert et corrigé

L'inventaire a trouvé deux routes présentes dans NestJS mais absentes des routeurs Rust :
`GET /api/admin/metrics` et `GET /api/admin/revenue`. Elles utilisent maintenant `AdminIdentity`, refusent les
paramètres inconnus, appliquent `month_to_date` par défaut et acceptent les six valeurs historiques de
`revenue_period`.

`PgAdminMetricsRepository` reproduit les agrégats PostgreSQL des comptes, consentements, modération, matchs,
messages, photos, abonnements et revenu estimé. Les bornes calendaires utilisent `Europe/Paris`. Le snapshot
opérationnel joint les compteurs bornés en mémoire aux états PostgreSQL de l'outbox, des OTP et des maintenances. Le
statut d'une dépendance dépend de sa dernière issue, et son dernier code d'erreur public reste normalisé et borné.

## Matrice de preuve

| Domaine | Contrat et règles | Preuve avec dépendances réelles |
| --- | --- | --- |
| Cycle HTTP et santé | `http_contract`, `route_inventory` | `postgres_compatibility` |
| JWT mobile, sessions et appareils | `mobile_auth_contract`, `mobile_identity` | tests transactionnels inclus dans `mobile_identity` |
| OTP et Sweego | `otp_delivery`, `otp_sweego_contract`, `sweego_client`, `sweego_signature` | PostgreSQL local et faux serveur HTTP contrôlé |
| Administration WebAuthn | tests unitaires `identity::admin` avec `webauthn-probe` | primitives OpenSSL/WebAuthn réelles ; cérémonie navigateur reportée ci-dessous |
| Profil et consentements | `profiles_contract` | `profiles_postgres` |
| Traits, questions et plans | règles unitaires du domaine | `catalog_postgres` |
| Photos, S3 et modération | validations HTTP et codec | `media_integration`, `photo_codec`, `moderation_postgres` |
| Matchs et messages | `matches_contract` | `matches_postgres` |
| Découverte | `discovery_contract` | `discovery_postgres` |
| Facturation et Stripe | `billing_contract`, `billing_webhook_contract`, `billing_stripe` | `billing_postgres`, `billing_reconciliation_postgres` |
| Notifications, SSE et Redis | `notifications_contract`, `sse_contract` | `notifications_postgres`, `redis_integration` |
| Blocages, signalements et vues admin | `administration_reports_contract` | `administration_reports_postgres` |
| DSR et export | `privacy_rights_contract` | `privacy_rights_postgres` |
| Effacement reprenable | `account_erasure_contract` | `account_erasure_postgres` |
| Outbox et maintenances | règles unitaires des workers | `outbox_postgres`, `maintenance_outbox_postgres` |
| Métriques et revenu admin | tests unitaires `administration::metrics` et `operations::status` | `admin_metrics_postgres` |

## Commandes de validation

La pile locale requise est démarrée sans exposer PostgreSQL, Redis ou S3 publiquement :

```bash
docker compose -f compose.yaml -f compose.dev.yaml up -d postgres redis object-storage photo-moderation
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets --all-features
```

Le dernier passage S28 a validé les 100 routes, les tests unitaires, toutes les suites de contrat et toutes les suites
réelles PostgreSQL, Redis, S3 et codec photo. Ces résultats historiques doivent être rejoués sur la candidate Debian.

## Divergences expliquées

Le runtime admin expose `{ "runtime": "rust", "uptime_seconds", "memory_rss_bytes" }`. Les champs Node/V8
`heap_used_bytes` et `event_loop_delay_p95_ms` ne sont pas émis, car leur conserver le nom avec une autre sémantique
produirait une métrique trompeuse. Cette décision est cohérente avec les séries Prometheus S27.

Le binaire `api` assemble depuis S29 l’état global, les adaptateurs, les 100 routes et les listeners HTTP et métriques.
Le smoke processus vérifie santé, readiness, authentification mobile/admin, validation JSON et route inconnue. La
procédure de bascule complète et ses validations externes figurent dans [s29-cutover.md](s29-cutover.md).

## Validations externes restantes avant suppression de NestJS

Ces contrôles dépendent d’identifiants ou de fournisseurs externes et restent requis avant suppression de NestJS :

- exécuter `contract-compare` entre deux bases, buckets et espaces Redis isolés ; le corpus générique actuel ne
  contient encore que la santé et la route inconnue, les scénarios métier étant couverts directement par domaine ;
- accomplir inscription et connexion WebAuthn dans le navigateur avec `http://localhost:5173`, RP ID `localhost`
  et proxy Vite `/api` ;
- exécuter les sandboxes Stripe, Sweego et FCM avec des secrets dédiés, sans rejouer automatiquement un POST dont
  l'issue est incertaine ;
- provoquer les coupures réseau et arrêts de processus entre claim, effet externe et checkpoint, puis confirmer la
  reprise idempotente dans PostgreSQL et S3 ;
- rejouer le test de charge et vérifier les budgets de pool, mémoire, lots et cardinalité des métriques.

NestJS ne doit être supprimé qu'après réussite et archivage de ces preuves sur le binaire S29.
