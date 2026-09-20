# S05 — PostgreSQL et historique des migrations

## Frontière retenue

SQLx fournit le pool asynchrone et les codecs PostgreSQL sans introduire d’ORM. Les futurs repositories conserveront leur SQL spécialisé et recevront une connexion ou transaction explicite. `Database::transaction` commence, committe ou annule la transaction sur une même connexion ; une erreur d’annulation remplace l’erreur de l’opération, comme dans `DatabaseService` NestJS.

Le pool reprend `POSTGRES_POOL_MAX`, les délais de connexion et d’inactivité, `statement_timeout`, `idle_in_transaction_session_timeout` et `application_name`. Son compteur d’attente est protégé par une garde RAII : une future d’acquisition annulée ne laisse pas un compteur fantôme. Les requêtes SQLx ne sont jamais journalisées par le driver.

Lorsque TLS est activé, SQLx utilise `verify-full`. Le certificat supplémentaire déjà fourni au runtime Node par `NODE_EXTRA_CA_CERTS` est transmis à SQLx comme racine privée. Sans cette variable, les racines publiques intégrées à rustls restent utilisées. Aucun chemin ou contenu de certificat n’est journalisé.

Le pool ne force pas une timezone de session différente de PostgreSQL. Les requêtes existantes qui exigent UTC continuent à l’exprimer dans leur SQL. Les types Rust retenus sont `Uuid`, `NaiveDate`, `DateTime<Utc>`, `Decimal`, `i64`, `serde_json::Value`, `Vec<u8>` et `f64` selon les colonnes existantes.

## Historique

Le migrateur TypeScript reste la source de vérité. Rust vérifie seulement :

| Version | SHA-256 normalisé par `migration-catalog.ts` |
| --- | --- |
| `001_baseline_20260905` | `7d33ff78d8094576acc30af275e1426f2feb6333911283ef1f1aadf2f9b8e111` |
| `017_postgres_discovery` | `f2e656a133d64a08873c86cb9a4dddbc84c4c1704e590851ed63a3a2ac6d1006` |

Un historique absent, incomplet, inconnu, sans checksum ou divergent fait échouer le démarrage. Rust exige aussi `user_account` et `swipe_decision`, ce qui empêche un historique fabriqué sans les objets finaux. Une nouvelle migration PostgreSQL devra être appliquée par le migrateur TypeScript puis ajoutée à cette liste dans le même changement Rust.

## Erreurs

Les erreurs SQLx sont immédiatement réduites à des variants sans message, requête, paramètre ou détail du serveur. Les SQLSTATE conservés sont :

| SQLSTATE | Variant Rust | Usage |
| --- | --- | --- |
| `P0E01` | `AccountUnavailable` | future réponse HTTP 409 `account_unavailable` |
| `23502` | `Constraint(NotNull)` | violation NOT NULL |
| `23503` | `Constraint(ForeignKey)` | violation de clé étrangère |
| `23505` | `Constraint(Unique)` | concurrence/idempotence métier |
| `23514` | `Constraint(Check)` | invariant SQL |
| `40001` | `SerializationFailure` | échec sérialisable potentiellement rejouable par une commande conçue pour cela |
| `40P01` | `Deadlock` | deadlock potentiellement rejouable par une commande conçue pour cela |

Une issue inconnue lors du commit reste `TransactionCommitFailed`, car le résultat peut être incertain. Aucun retry automatique n’est ajouté par ce lot.

## Tests

Les tests unitaires couvrent l’historique vide, les versions inconnues ou manquantes, les checksums absents ou modifiés et le mapping SQLSTATE. Le test PostgreSQL réel crée son schéma de contrôle dans une transaction annulée et vérifie :

- l’historique de la base locale déjà migrée ;
- les paramètres de timeout de chaque connexion ;
- les codecs UUID, date, timestamptz à la microseconde, numeric, bigint, jsonb, bytea et double precision ;
- le rollback réel avec un pool limité à une connexion ;
- la remontée réelle du SQLSTATE `P0E01` sans son message privé ;
- les refus d’un historique absent, inconnu ou altéré et d’objets terminaux manquants.

Le test exige `ENV=development`, `POSTGRES_DB=histae-dev` et un hôte loopback. Il ne lance aucun reset et n’altère pas le schéma public. Le test NestJS `postgres.baseline.integration.spec.ts` reste la preuve de création sur base vide, sérialisation du migrateur, refus d’un schéma non vide et protection des checksums.

## Hors périmètre

Les advisory session locks et le pool dédié d’activité appartiennent à S06. Les repositories métier arrivent dans leurs lots respectifs. Le listener HTTP et la traduction de `P0E01` en enveloppe JSON seront raccordés en S07.
