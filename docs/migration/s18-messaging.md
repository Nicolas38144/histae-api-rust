# S18 — messagerie, lecture et pagination

> Compte rendu historique de ce lot. Pour les commandes et l’arborescence actuelles, consulter le [README](../../README.md) et l’[architecture](../architecture.md).

## Périmètre

S18 ajoute les quatre routes mobiles NestJS suivantes au routeur Axum des matchs :

| Méthode | Route | Entrée | Réponse |
|---|---|---|---|
| `GET` | `/api/matches/:id/messages` | `limit`, `offset`, `cursor` | `{ messages, next_cursor }` |
| `POST` | `/api/matches/:id/messages` | `{ content }` et `Idempotency-Key` | `201` avec le message public |
| `PATCH` | `/api/matches/:id/messages/read` | `{ read_through_message_id }` | `{ updated_count, read_through_message_id }` |
| `PATCH` | `/api/matches/:id/messages/:msgId/read` | aucun corps | `{ message: "message marked as read" }` |

Toutes exigent une session mobile active avec onboarding terminé et la participation au match. L’envoi applique le
quota dédié `RATE_LIMIT_MESSAGE`/`RATE_LIMIT_MESSAGE_WINDOW`, soit 60 requêtes par minute par utilisateur dans la
configuration courante.

## Mapping NestJS vers Rust

| NestJS | Rust |
|---|---|
| `MatchesController` | handlers et DTO fermés dans `matches/http.rs` |
| méthodes de messagerie de `MatchesService` | `MatchService` dans `matches/service.rs` |
| `MatchMessageRepository` | `MatchMessageStore` et `PgMatchMessageRepository` dans `matches/pg.rs` |
| `lockMessagingMatch` | helper transactionnel `lock_available_match` partagé avec S17 |
| `PublicMessage` et mapper | `MessageRecord` → `PublicMessage` dans `matches/domain.rs` |
| `RateLimitService` | `RateLimiter` distribué de S07 |
| `notification-outbox.ts` | `enqueue_notification` de S14 sur la connexion transactionnelle |
| `MobileDeliveryService` | méthodes message de `MatchEventPublisher`, à brancher au transport S22 |

## Validation du contenu et de l’idempotence

Le DTO HTTP exige un champ `content` JSON de type chaîne et refuse tout champ inconnu. Le service applique ensuite
le trim ECMAScript utilisé par JavaScript, refuse un contenu vide et limite le texte à 2 000 points de code. La clé
d’idempotence est trimée, normalisée en minuscules puis validée comme UUID v4 RFC 4122 canonique.

Le repository recherche une clé existante avant de verrouiller le match, comme NestJS. Un replay avec le même
expéditeur, match et contenu renvoie donc le message original, même après la mutation initiale, sans nouvelle
notification ni nouvel événement temps réel. Une autre combinaison renvoie `409 idempotency_key_conflict`.
L’index PostgreSQL `(sender_id, idempotency_key)` et le second contrôle après `ON CONFLICT DO NOTHING` couvrent les
envois concurrents.

## Accès au match et atomicité

Une nouvelle page, un nouvel envoi ou un accusé de lecture verrouille le match. L’horloge est lue après acquisition
du verrou dans le `SELECT` extérieur à la CTE matérialisée. La transaction peut donc ouvrir la fenêtre de
continuation ou persister l’état `expired` avant de renvoyer une erreur publique.

Pour un nouvel envoi, insertion du message, mise à jour de `last_message_at`, notification `new_message`, références
par appareil et jobs outbox partagent la même transaction. Le payload durable contient seulement les UUID du match,
du message et de l’expéditeur. Le texte privé n’est jamais copié dans la notification.

## Pagination et lecture

Les messages sont triés par `(created_at DESC, id DESC)`. Le curseur base64url conserve le JSON `{ at, id }` et les
six chiffres de précision PostgreSQL, ce qui évite de sauter deux messages séparés uniquement par leurs
microsecondes. `offset` reste accepté pour compatibilité, mais il doit être nul lorsqu’un curseur est fourni.

Le read-through vérifie que le message borne appartient au match, puis marque seulement les messages reçus et non
lus dont `(created_at, id)` est antérieur ou égal à la borne. La route historique unitaire refuse qu’un expéditeur
marque son propre message. Une borne absente et un message propre sur la route historique renvoient tous deux
`404 message_not_found`, conformément au comportement actuel.

## Erreurs publiques

S18 conserve notamment :

- `invalid_pagination` pour le DTO de query ;
- `invalid_message_payload` et `invalid_read_payload` pour les corps JSON ;
- `invalid_match_id` ou `invalid_message_id` selon le DTO de route historique ;
- `invalid_message_request` pour les règles de contenu et l’association curseur/offset ;
- `invalid_idempotency_key`, `idempotency_key_conflict` et `message_rate_limit_exceeded` ;
- `match_not_found`, `match_expired`, `messaging_not_available` et `message_not_found`.

## Validation

```bash
export CARGO_BUILD_JOBS=1
cargo test --test matches_contract
cargo clippy --all-targets -- -D warnings
```

Avec PostgreSQL local :

```bash
docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d postgres
export CARGO_BUILD_JOBS=1
cargo test --features postgres-integration --test matches_postgres
```

Les fixtures refusent une base autre que `histae-dev` sur loopback, génèrent tous leurs UUID et suppriment leurs
données après chaque scénario.

## Limites du lot

Le relais SSE/Redis de `message.created` et `message.read` est fourni par S22 à travers `MatchEventPublisher`. Le
push durable est programmé dans la transaction grâce à S14. Les vues administratives de conversation sont livrées
par S23 et la suppression bornée des messages expirés, avant le parent match, par S26.
