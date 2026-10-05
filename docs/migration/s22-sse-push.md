# S22 — SSE et push FCM

> Compte rendu historique de ce lot. Pour les commandes et l’arborescence actuelles, consulter le [README](../../README.md) et l’[architecture](../architecture.md).

## Analyse de l’existant

Le contrôleur mobile NestJS expose `GET /api/users/me/events` derrière `JwtActiveGuard`. Le flux envoie immédiatement
`connected`, émet un heartbeat toutes les 25 secondes, filtre les événements par destinataire et se ferme à
l’expiration de l’access token. La famille de refresh est relue immédiatement puis toutes les 25 secondes : une
révocation sur une autre instance ferme donc le flux même si le message Redis correspondant est perdu. Redis
Pub/Sub relaie les événements entre instances, tandis qu’un `Subject` local sert lorsque Redis est désactivé. Il
n’existe aucun replay hors ligne.

Les jobs `notification.push` ne contiennent qu’un identifiant de livraison. Avant chaque appel réseau, PostgreSQL
relit la notification, l’appareil, la famille, le compte et l’objet métier. Une notification expirée, lue, bloquée,
révoquée ou devenue obsolète est acquittée sans envoi. Le payload FCM est une allowlist de métadonnées et ne contient
jamais le texte d’un message. OAuth Google utilise une assertion RS256 et met le token d’accès en cache avec une
marge de 60 secondes. Seul le code FCM explicite `UNREGISTERED` supprime un appareil ; un simple HTTP 404 reste une
erreur réessayable.

## Mapping NestJS → Rust

| NestJS | Rust | Responsabilité |
| --- | --- | --- |
| `RealtimeService` | `notifications::sse::RealtimeService` | Publication locale/Redis, validation du relais et fan-out borné |
| `RealtimeService.stream` | stream Axum dans `notifications::sse` | `connected`, événements ciblés, heartbeat, révocation et expiration |
| `MobileDeliveryService` | `notifications::delivery::MobileDeliveryService` | Adaptateurs `MatchEventPublisher` et `BillingRealtimePublisher` |
| `NotificationPushRepository` | `PgNotificationDeliveryStore` | Éligibilité finale et suppression d’un token explicitement invalide |
| `NotificationPushService` | `NotificationPushHandler` | Allowlist du payload et handler `OutboxHandler` |
| `PushService` | `notifications::push::PushService` | OAuth RS256, cache, requêtes FCM bornées et erreurs normalisées |
| `RedisService` | `infra::redis::RedisService` | Connexions, publication et abonnement dédiés |

## Contrat HTTP et SSE

`GET /api/users/me/events` exige un JWT mobile actif et un onboarding complet, comme NestJS et répond `200 text/event-stream`.

- `connected` ne porte pas d’identifiant et contient `{ "server_time": <date ISO UTC> }` ;
- `heartbeat` a le même payload et arrive toutes les 25 secondes ;
- les événements métier portent un UUID généré, leur type exact et un objet contenant `occurred_at` puis les
  métadonnées métier ;
- les types acceptés sont `match.created`, `match.updated`, `matches.invalidated`, `message.created`,
  `message.read` et `subscription.updated` ;
- une reconnexion ne rejoue aucun événement antérieur.

Le buffer local est borné à 128 événements par récepteur. Un client trop lent est déconnecté dès qu’il accuse du
retard ; le mobile relit alors les ressources métier. Le canal Redis entrant est lui aussi borné. Cette politique
évite qu’un flux SSE fasse croître la mémoire sans limite et ne transforme pas le signal best-effort en stockage.

## Livraison durable FCM

Le handler relit l’éligibilité avant le réseau en réutilisant le prédicat Stripe de S14. Pour les matchs et messages,
il vérifie encore le statut et l’expiration du match, les comptes participants, les blocages, l’existence du message,
son destinataire et son état de lecture. Un appareil réaffecté ou lié à une autre session ne reçoit pas une ancienne
tâche.

FCM reçoit uniquement :

- `type` et le `notification_id` stable ;
- `match_id` pour `new_match` et `new_message` ;
- `message_id` et `sender_id` pour `new_message`.

Les réponses réseau sont limitées à 64 Kio. Les détails fournisseur, assertions OAuth et tokens d’appareil ne sont
ni persistés ni journalisés. `PUSH_PROVIDER=disabled` acquitte la tâche sans réseau, conformément au comportement
NestJS. Une erreur OAuth, réseau ou FCM devient `push_delivery_unavailable` et suit les reprises S11. Une issue
incertaine peut donc produire un doublon externe portant le même `notification_id`.

## Infrastructure et configuration

S22 réutilise les variables déjà présentes dans `.env` : `PUSH_PROVIDER`, `FIREBASE_PROJECT_ID`,
`FIREBASE_CLIENT_EMAIL`, `FIREBASE_PRIVATE_KEY`, `FIREBASE_TOKEN_URI` et `PUSH_TIMEOUT`. Aucun secret ni nouvelle
valeur n’a été ajouté. `compose.dev.yaml` contient désormais le Redis local éphémère sur `127.0.0.1:6379`, limité à
192 Mio avec `noeviction`, sans sauvegarde ni AOF.

## Validation

```bash
cargo test --lib notifications::
cargo test --test sse_contract
cargo test --features postgres-integration --test notifications_postgres -- --test-threads=1
cargo test --features redis-integration --test redis_integration -- --test-threads=1
cargo clippy --all-targets --all-features -- -D warnings
```

Les tests couvrent l’authentification HTTP, le format initial SSE, le ciblage, la déduplication des destinataires,
la révocation, l’expiration du JWT, le consommateur lent, les messages Redis invalides, le relais réel entre deux
instances, le cache OAuth, `401`, `404`, `UNREGISTERED`, le provider désactivé, l’allowlist sans contenu privé et
les alertes Stripe devenues obsolètes. Tous les UUID de fixtures sont générés.

## Risques restant ouverts

Le flux SSE ne remplace pas une lecture métier après reconnexion. L’acceptation FCM ne prouve pas la réception par
le téléphone et une réponse perdue reste susceptible d’être rejouée. L’assemblage des routeurs et des cinq handlers
dans les binaires de production suit la séquence d’intégration existante ; S22 fournit désormais les composants
concrets attendus par S17, S18 et S21.
