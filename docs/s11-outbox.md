# S11 — moteur d’outbox et suivi de maintenance

## Analyse de l’existant NestJS

`OutboxRepository` reçoit la transaction métier lors de l’insertion. L’unicité `(event_type, aggregate_id)` rend
la programmation idempotente. Le worker réclame jusqu’à 50 événements dus ou abandonnés avec
`FOR UPDATE SKIP LOCKED`, incrémente `attempts` lors du claim et conserve l’identité du worker dans
`locked_by`. Un claim devient reprenable après cinq minutes.

Avant chaque effet, le worker renouvelle le claim. Il lance au plus cinq handlers simultanément et n’acquitte
l’événement qu’après la réussite de l’effet. Un handler reprenable peut répondre `deferred` : la ligne reste alors
`processing` et sera reprise à l’expiration de sa lease. Les erreurs transitoires utilisent un backoff exponentiel
de 1 à 60 secondes et dix tentatives ; une anomalie explicitement permanente devient une dead letter dès la
première tentative. Seul un code d’erreur borné est persisté.

Les événements `completed` et `discarded` sont conservés sept jours. Leur purge horaire est bornée par
`OUTBOX_PURGE_BATCH_SIZE × OUTBOX_PURGE_MAX_BATCHES`, avec un commit par requête. Le suivi
`maintenance_job_status` est best-effort : sa panne ne transforme jamais une maintenance métier réussie en échec.
Le `run_id` empêche une exécution plus ancienne de terminer l’état d’une exécution plus récente.

Les cinq types persistés sont `photo.delete`, `notification.push`, `account.erase`,
`billing.subscription.reconcile` et `billing.customer.reconcile`. Le schéma autorise techniquement d’autres noms :
Rust les conserve comme types inconnus et ne les acquitte jamais par défaut.

## Mapping NestJS → Rust

| NestJS | Rust | Responsabilité |
| --- | --- | --- |
| `outbox.models.ts` | `outbox::types` | Types, états, résultats de dispatch et compteurs bornés. |
| `OutboxRepository` | `outbox::pg::PgOutboxRepository` | Enqueue/requeue transactionnels, claim, ownership, retry et purge SQLx. |
| `OutboxWorkerService` | `outbox::worker::OutboxWorker` | Lots de 50, concurrence 5, backoff, dead letter, purge et annulation Tokio. |
| `OutboxEventDispatcher` | `OutboxEventDispatcher` + traits `OutboxHandler` | Routage fermé et frontière testable des cinq effets métier. |
| `MaintenanceStatusRepository` | `operations::maintenance::PgMaintenanceStatusRepository` | État persistant de la dernière exécution. |
| `MaintenanceTrackerService` | `operations::maintenance::MaintenanceTracker` | Succès, échec normalisé, skip et progression best-effort. |
| `AbortSignal` et timer Node | `CancellationToken` et `tokio::select!` | Arrêt coopératif sans nouveau poll après annulation. |

Les traits portent uniquement les frontières substituées dans les tests. Il n’y a ni conteneur DI, ni timer partagé,
ni mutex dans les chemins de production.

## Invariants conservés

- `enqueue` utilise la transaction de l’agrégat appelant ; aucun commit caché n’est ouvert.
- `requeue` réinitialise l’état existant sans remplacer son payload, comme la requête NestJS.
- Deux workers ne réclament pas la même ligne disponible grâce à `SKIP LOCKED`.
- Les lignes `processing` dont `locked_at <= stale_before` sont récupérables, frontière incluse.
- Une perte d’ownership avant le dispatch interdit l’effet et l’acquittement.
- Une réussite externe suivie d’un échec d’acquittement reste retryable ; un doublon externe demeure donc possible.
- `deferred` ne complète ni ne reprogramme la ligne.
- Tous les handlers d’un groupe sont attendus même si une autre transition PostgreSQL échoue.
- La purge fusionne les candidats `completed` et `discarded`, ordonne par date/UUID et reste bornée.
- `processed_count`, `batch_count` et `work_remaining` ne contiennent aucun identifiant personnel.
- Les erreurs et logs ne conservent ni payload, ni message d’exception, ni cause technique.

## Activation progressive

Le dispatcher exige cinq handlers concrets lors de sa construction. S11 n’en fournit volontairement aucun : ils
arrivent avec les lots notifications, photos, Stripe et effacement. Le binaire `outbox` conserve donc son échec sûr
`component_not_implemented` et ne doit pas être lancé contre la file de développement. Cette barrière évite qu’un
handler incomplet acquitte un événement ou qu’un type absent consomme son budget de tentatives.

Une fois les cinq handlers livrés, le binaire pourra construire `PgOutboxRepository`, `MaintenanceTracker` et
`OutboxWorker`, puis appeler `run_until_cancelled`. Aucun changement de schéma n’est nécessaire pour S11.

## Validation

Sans infrastructure :

```powershell
cargo test --locked --lib outbox::
cargo test --locked --lib operations::maintenance::
cargo test --locked
cargo clippy --locked --all-targets --features postgres-integration -- -D warnings
```

Avec PostgreSQL local `histae-dev`, préparé et migré par le dépôt NestJS :

```powershell
cargo test --locked --features postgres-integration --test outbox_postgres
```

Le test réel refuse toute base autre que `histae-dev`, tout environnement autre que `development` et tout hôte non
loopback. Il crée uniquement des UUID aléatoires, place ses événements dans le futur pour les isoler d’un worker
local éventuel, puis effectue un nettoyage ciblé.

## Limites du lot

Les handlers externes et leurs règles d’éligibilité appartiennent aux lots métier ultérieurs. Les routes de liste,
retry et discard des dead letters, leur authentification récente et leur audit transactionnel restent dans S26.
S11 ne modifie aucun contrat HTTP.

