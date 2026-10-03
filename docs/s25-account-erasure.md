# S25 — effacement de compte reprenable

## Analyse de l’existant

NestJS sépare l’acceptation HTTP de l’effacement effectif. `POST /api/users/me/deletion-token` remplace tout jeton
précédent par un secret opaque `uuid-v4:base64url`, valable dix minutes par défaut. PostgreSQL ne conserve que le
SHA-256 du jeton complet. `DELETE /api/users/me` verrouille d’abord le compte, consomme le jeton une seule fois,
crée ou reprend la DSR d’effacement, crée le checkpoint et l’événement `account.erase`, désactive le compte et
supprime ses autres jetons dans une transaction unique. Le `202` confirme cet enregistrement durable ; il ne
signifie pas que les fournisseurs et les données locales sont déjà effacés.

Le worker reprend cinq checkpoints persistés : `stripe`, `photos`, `swipes`, `postgres`, puis `completed`. Il prend
un verrou consultatif exclusif de session sur le compte sans conserver de transaction SQL pendant les appels
réseau. Un verrou partagé encore détenu diffère le job de cinq secondes et annule la tentative consommée par sa
revendication. Chaque étape terminée remet le budget de tentatives à zéro et reprogramme le lot suivant.

Stripe traite au plus 50 intentions Customer par passage et conserve la fenêtre sûre de rejeu du POST d’origine.
Les photos sont supprimées par lots de 50 : la ligne passe à `deleting`, l’objet est supprimé, puis la trace
PostgreSQL disparaît seulement après confirmation. Les swipes entrants et sortants sont supprimés par lots de
1 000. La dernière transaction verrouille les matchs avant le compte, vérifie qu’aucune photo ni aucun swipe ne
reste, nettoie les états techniques, appelle `fct_anonymize_user`, termine la DSR et marque le checkpoint
`completed`. L’outbox acquitte ensuite l’événement.

## Mapping NestJS → Rust

| NestJS | Rust | Responsabilité |
| --- | --- | --- |
| `UsersService.issueDeletionToken/confirmAnonymize` | `privacy::erasure::AccountDeletionService` | Secret opaque, hash du jeton complet, durée et erreurs publiques |
| `UsersRepository.replaceDeletionToken/acceptErasure` | `privacy::erasure_pg::PgErasureRepository` | Verrou du compte, consommation unique et acceptation atomique |
| `enqueueAccountErasure` | `privacy::erasure_pg::enqueue_account_erasure` | DSR, checkpoint, outbox et gel du compte partagés avec S24 |
| `ErasureService` | `privacy::erasure::ErasureService` | Orchestration des checkpoints sous verrou exclusif |
| `ErasureRepository` | `privacy::erasure_pg::PgErasureRepository` | Ownership, transitions, lots de swipes, report et anonymisation finale |
| `BillingService.deleteCustomerForAccount` | `billing::service::BillingService` via `CustomerEraser` | Customer lié, intentions incertaines et lot de 50 |
| `PhotosService.deleteForAccount` | `media::service::PhotoService::delete_for_account` via `PhotoEraser` | Suppression objet puis trace PostgreSQL, lot de 50 |
| `AccountActivityService.tryExclusive` | `infra::postgres_locks::AccountActivityPool::try_exclusive` | Fencing de session et report sans transaction réseau |
| `OutboxEventDispatcher` | `outbox::worker::OutboxHandler` | Résultat `Completed`, `Deferred` ou erreur normalisée |

## Contrats HTTP

- `POST /api/users/me/deletion-token` → `201 { confirmation_token, expires_at }` ;
- `DELETE /api/users/me` avec `{ confirmation_token }` →
  `202 { request_id, status: "in_progress" }`.

Les deux routes exigent une famille mobile active, mais autorisent un onboarding incomplet. Le body du `DELETE`
refuse les champs inconnus et exige exactement un UUID v4 canonique minuscule suivi de 43 caractères base64url.
Un payload invalide retourne `invalid_account_deletion_payload`; un secret absent, expiré, remplacé ou déjà consommé
retourne `401 invalid_or_expired_deletion_token`; un compte absent lors de l’émission retourne
`404 account_not_found`.

## Atomicité, reprises et erreurs

Les appels Stripe et stockage sont hors transaction PostgreSQL. Avant chaque checkpoint, le handler relit
l’événement `processing`, son `locked_by` et l’étape courante. Les mises à jour refusent donc un worker qui a perdu
son ownership. Une perte du verrou d’activité avant l’effet suivant ou le checkpoint échoue fermement.

Les seules erreurs persistables sont bornées : `erasure_stripe_unavailable`,
`erasure_stripe_reconciliation_required`, `erasure_photos_unavailable`, `erasure_swipes_unavailable`,
`erasure_postgres_unavailable` et `erasure_invalid_state`. Aucun message fournisseur, clé objet, téléphone, payload
ou stack n’entre dans l’outbox.

Le schéma nécessaire existait déjà dans la baseline et dans `003_postgres_discovery.sql`; S25 n’ajoute donc aucune
migration. `ACCOUNT_DELETION_TOKEN_TTL=10m` était déjà présent dans `.env` et validé entre une et trente minutes.
Aucun service Docker supplémentaire n’est requis.

## Validation

```powershell
cargo test --test s25_contract
cargo test --features postgres-integration --test s25_postgres -- --test-threads=1
cargo check --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets
```

Les tests HTTP couvrent l’authentification, l’onboarding incomplet, les statuts, le JSON, les champs inconnus, les
jetons mal formés, expirés et rejoués ainsi que le compte absent. Le scénario PostgreSQL utilise uniquement des UUID
générés et vérifie le remplacement du jeton, l’absence du secret en base, l’acceptation atomique, le fencing du
worker, le report sous verrou partagé, la reprise après redémarrage, l’ordre objet/photo, les swipes dans les deux
directions, l’anonymisation, l’audit, la clôture DSR et l’acquittement outbox.

## Risques restant ouverts

Le handler `account.erase` est prêt à être injecté dans le dispatcher. S26 doit achever l’assemblage du worker et les
opérations administratives de dead letter, notamment l’interdiction absolue d’abandon de `account.erase`. S28 doit
encore éprouver les coupures réseau réelles, les crashs de processus entre chaque checkpoint et les volumes proches
des bornes de production.
