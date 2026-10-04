# S24 — DSR et export utilisateur

> Compte rendu historique de ce lot. Pour les commandes et l’arborescence actuelles, consulter le [README](../../README.md) et l’[architecture](../architecture.md).

## Analyse de l’existant

Les demandes RGPD sont accessibles avec une session mobile active même lorsque l’onboarding n’est pas terminé. Les
types publics sont `access`, `erasure`, `portability`, `rectification`, `restriction` et `objection`. PostgreSQL
garantit une seule demande `pending` ou `in_progress` par type et par utilisateur. La liste personnelle n’expose ni
notes administratives ni détails du workflow d’effacement.

Les listes administratives utilisent uniquement la session WebAuthn, une pagination décroissante par
`requested_at/id` et une projection bornée de la progression d’effacement. Une mutation exige une authentification
WebAuthn récente. Les transitions sont verrouillées, auditées et atomiques. Demander `completed` pour un effacement
en cours maintient la DSR à `in_progress`, désactive le compte, crée son checkpoint et écrit `account.erase` dans
l’outbox. Le rejeu rend le même résultat sans dupliquer le workflow. Le handler reprenable appartient à S25.

L’export est préparé intégralement avant le début de la réponse. Toutes les collections, y compris les swipes
sortants, partagent une transaction PostgreSQL `REPEATABLE READ, READ ONLY`. Les données sont écrites page par page
dans un fichier temporaire privé. La taille et le nombre de préparations simultanées sont bornés. Le fichier et la
place de concurrence sont conservés jusqu’à la fin, l’erreur ou l’abandon du body HTTP, puis libérés ensemble.

## Mapping NestJS → Rust

| NestJS | Rust | Responsabilité |
| --- | --- | --- |
| `PrivacyService/PrivacyRepository` pour les DSR | `privacy::{rights,rights_pg,rights_http}` | Types fermés, transitions, audits et contrats HTTP |
| `DataExportService` | `privacy::export::DataExportService` | Quota de concurrence, fichier privé, signature photo et nettoyage |
| `DataExportRepository` | `privacy::export::pg::PgDataExportStore` | Instantané unique et pagination de toutes les collections |
| `JsonExportWriter` | `privacy::export::JsonExportWriter` | Écriture incrémentale et borne stricte en octets |
| `JwtActiveGuard` avec `AllowIncompleteOnboarding` | `AuthenticatedMobile` | Session mobile active sans obligation d’onboarding terminé |
| `AdminSessionGuard` / `RecentAdminAuthenticationGuard` | `AdminIdentity` / `RecentAdminIdentity` | Session WebAuthn et fraîcheur des mutations |
| `StreamableFile` | `Body::from_stream(PreparedDataExport)` | Flux borné, longueur connue et nettoyage à la fermeture |

## Contrats livrés

- `POST /api/users/me/data-subject-requests` → `201` ;
- `GET /api/users/me/data-subject-requests` → `200 { requests }` ;
- `GET /api/users/me/data-export` → pièce jointe JSON `200` ;
- `GET /api/admin/data-subject-requests` → page administrative ;
- `PATCH /api/admin/data-subject-requests/:id` → transition récente et auditée ;
- `GET /api/admin/data-access-logs` → journal paginé pour un utilisateur.

Les DTO refusent les champs inconnus. Les notes acceptent `null` ou une chaîne de 2 000 caractères JavaScript au
maximum. Les limites et erreurs restent `data_request_already_open`, `data_request_not_found`,
`invalid_data_request_transition`, `data_export_rate_limit_exceeded`, `data_export_too_large`, `data_export_busy`
avec `Retry-After: 30`, et `data_export_unavailable`.

## Contenu et sûreté de l’export

Le document contient le compte, le profil et sa photo signée au dernier moment, les préférences, traits, réponses de
profil, consentements, matchs, messages rédigés, signalements soumis, blocages sortants, abonnement, factures,
métadonnées des familles mobiles et `discovery_actions.outgoing`. Il ne lit jamais les décisions entrantes de tiers.
Les secrets de session, téléphones, hashes, payloads fournisseurs et clés objet ne sont pas projetés.

Les valeurs `DATA_EXPORT_PAGE_SIZE`, `DATA_EXPORT_MAX_BYTES`, `DATA_EXPORT_MAX_CONCURRENCY`,
`RATE_LIMIT_DATA_EXPORT` et `RATE_LIMIT_DATA_EXPORT_WINDOW` étaient déjà présentes dans `.env` et validées par la
configuration Rust. S24 n’ajoute aucune migration ni ressource Docker.

L’observation HTTP reconnaît désormais les réponses `Content-Disposition: attachment` comme des flux. La durée est
enregistrée à la fin ou à l’abandon du body, comme pour SSE, plutôt qu’au retour du handler.

## Validation

```powershell
cargo test --lib
cargo test --test privacy_rights_contract
cargo test --features postgres-integration --test privacy_rights_postgres -- --test-threads=1
cargo check --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Les UUID de test sont générés. Le scénario PostgreSQL force une page de taille 1, vérifie qu’un swipe entrant n’est
pas exporté, valide l’audit du téléchargement et prouve qu’un rejeu de programmation d’effacement ne crée qu’un
checkpoint et un événement outbox.

## Risques restant ouverts

Le téléchargement occupe une connexion PostgreSQL pendant sa préparation complète, conformément à NestJS. Les
budgets doivent être calibrés avec des volumes représentatifs avant la production. Le handler `account.erase` est
maintenant fourni par S25 ; S28/S29 assembleront les routeurs dans le binaire final et exécuteront la comparaison
différentielle complète avec NestJS.
