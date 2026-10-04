# S26 — maintenances métier et administration de l’outbox

> Compte rendu historique de ce lot. Pour les commandes et l’arborescence actuelles, consulter le [README](../../README.md) et l’[architecture](../architecture.md).

## Analyse de l’existant

NestJS distingue la boucle longue de l’outbox de la commande de maintenance bornée. L’outbox revendique les
événements PostgreSQL avec ownership, renouvelle le claim avant le dispatch et ne considère un effet terminé
qu’après le retour explicite de son handler. Elle distribue exactement cinq types : `photo.delete`,
`notification.push`, `account.erase`, `billing.subscription.reconcile` et `billing.customer.reconcile`.

La maintenance générale exécute en parallèle quatre travaux indépendants. Les matchs conservent un advisory lock
de session pendant toute l’exécution, mais valident chaque lot dans sa propre transaction. Les messages sont
supprimés et les signalements détachés avant le parent. La privacy applique dix-huit politiques de rétention avec
un advisory lock transactionnel par lot. Les photos récupèrent les traitements anciens et les suppressions à
reprendre, puis effacent l’objet avant la trace PostgreSQL. La facturation programme ses réconciliations sans
réinventer le protocole Stripe de S21.

L’administration expose une liste minimale des dead letters et deux mutations. La liste ne contient jamais le
payload, l’aggregate ID ou une clé objet. Retry et discard verrouillent la ligne, vérifient encore son état, écrivent
l’audit et effectuent la transition dans la même transaction. `account.erase` et tous les événements de facturation
ne peuvent jamais être abandonnés. `photo.delete` ne peut l’être que si la ligne `user_photo` a déjà disparu.

## Mapping NestJS → Rust

| NestJS | Rust | Responsabilité |
| --- | --- | --- |
| `OutboxAdminController` | `outbox::http` | Contrats axum, DTO stricts, session admin et fraîcheur WebAuthn |
| `OutboxAdminService` | `outbox::admin::OutboxAdminService` | Curseur, normalisation NFKC du motif et erreurs publiques |
| `OutboxAdminRepository` | `outbox::admin::PgOutboxAdminRepository` | Verrou, garde d’abandon, audit et transition atomique |
| `OutboxEventDispatcher` | `outbox::worker::OutboxEventDispatcher` | Dispatch fermé des cinq types d’événement |
| worker CLI | `outbox::runtime` et binaire `outbox` | Assemblage réel PostgreSQL/Redis/S3/FCM/Stripe et arrêt borné |
| `MatchMaintenanceService/Repository` | `matches::maintenance` | Leader de session, transactions par lot et nettoyage des enfants |
| `PrivacyMaintenanceService` | `privacy::maintenance` | Dix-huit rétentions bornées et leader transactionnel |
| `PhotosMaintenanceService` | `media::maintenance` | Reprise processing/deleting et suppression objet avant ligne |
| commande de maintenance | `operations::runtime` et binaire `maintenance` | Exécution concurrente bornée et compteurs sûrs |
| `MaintenanceTrackerService` | `operations::maintenance::MaintenanceTracker` | Running/succeeded/failed/skipped, progression et code normalisé |

## Contrat HTTP

- `GET /api/admin/outbox/dead-letters` exige une session WebAuthn admin active. `limit` vaut 20 par défaut et doit
  être un entier entre 1 et 100 ; `cursor` est base64url et limité à 512 unités UTF-16. Réponse `200` :
  `{ events, next_cursor }`, avec seulement `event_id`, `event_type`, `attempts`, `last_error_code`, `created_at`
  et `dead_lettered_at` ;
- `POST /api/admin/outbox/:id/retry` exige une authentification admin récente, un UUID v4 et
  `{ "reason": string }` sans champ inconnu, entre 3 et 500 unités UTF-16. Réponse `202` :
  `{ "message": "outbox event queued" }` ;
- `POST /api/admin/outbox/:id/discard` applique les mêmes guards et validations, puis répond `204` sans corps.

Les erreurs stables sont `invalid_outbox_request` (`400`), `invalid_cursor` (`400`),
`outbox_event_not_found` (`404`), `outbox_event_not_dead_letter` (`409`) et
`outbox_discard_not_allowed` (`409`). Le motif est normalisé NFKC, trimé comme JavaScript et refuse les caractères
de contrôle. Il reste exclusivement dans l’audit et l’état de résolution, jamais dans les logs.

## Lots, ownership et observabilité

La maintenance des matchs garde la clé consultative historique `37142581` sur une connexion dédiée entre les
lots. Chaque lot ouvre et commit sa propre transaction ; une panne n’annule donc pas le travail déjà validé. La
taille et le nombre maximum de lots utilisent `MATCH_MAINTENANCE_BATCH_SIZE` et
`MATCH_MAINTENANCE_MAX_BATCHES`. La privacy conserve 1 000 lignes × 100 lots et la photo 100 × 100, comme NestJS.
La purge résolue de l’outbox reste configurée par `OUTBOX_PURGE_BATCH_SIZE` et `OUTBOX_PURGE_MAX_BATCHES`.

`maintenance_job_status` ne conserve que `processed_count`, `batch_count`, `work_remaining` et un code d’erreur
normalisé. La commande écrit uniquement des compteurs autorisés par la liste blanche de logs. Elle ferme son pool
PostgreSQL même lorsqu’une maintenance échoue. Le worker coopère avec le `CancellationToken`, draine au plus trente
secondes, puis ferme le pool d’activité et le pool principal.

Le binaire `outbox` est une boucle longue. Le binaire `maintenance` effectue un passage borné ; sa cadence reste à
la charge du planificateur d’exploitation. Les deux exigent `MAINTENANCE_MODE=worker`. Aucune nouvelle variable
d’environnement, migration ou dépendance Docker n’est nécessaire dans S26 : `.env`, le schéma et
`compose.dev.yaml` contenaient déjà leurs prérequis.

## Validation

```powershell
cargo test --lib --all-features
cargo test --features postgres-integration --test maintenance_outbox_postgres -- --test-threads=1
cargo check --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-targets --all-features
```

Les tests unitaires couvrent la pagination, la normalisation du motif, la projection minimale, les DTO, les codes
HTTP, l’agrégation de progression et les reprises photo. Le scénario PostgreSQL refuse toute cible autre que
`histae-dev` sur loopback, génère tous ses UUID et nettoie ses lignes. Il vérifie le leader de session, les commits
par lots, l’ordre message/signalement/match, les rétentions présence et jeton, le retry, le discard autorisé, les
trois interdictions critiques et les audits transactionnels. Ses dates de maintenance sont antérieures à 1961 afin
qu’aucune donnée Histae existante ne puisse entrer dans ses requêtes de rétention.

## Risques restant ouverts

Le routeur d’administration reste composable et sera monté avec l’ensemble de l’API lors de l’assemblage final.
S27 doit encore produire l’image applicative non-root/read-only, le listener Prometheus privé et les commandes
d’exploitation finales. S28 doit provoquer les crashs et coupures réseau entre claims, effets externes et
checkpoints, puis comparer les cinq handlers à NestJS sous charge et concurrence réelles.
