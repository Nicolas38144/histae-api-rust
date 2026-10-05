# S14 — Appareils mobiles et création transactionnelle des notifications

> Compte rendu historique de ce lot. Pour les commandes et l’arborescence actuelles, consulter le [README](../../README.md) et l’[architecture](../architecture.md).

## Périmètre NestJS analysé

S14 migre `MobileController`, `MobileService`, `MobileRepository`, les DTO d’appareil,
`notification-outbox.ts` et le prédicat partagé de `notification-billing.ts`. Le SSE, OAuth Google,
l’éligibilité finale et l’envoi FCM sont désormais livrés par S22.

| NestJS | Rust | Responsabilité |
| --- | --- | --- |
| `MobileController` | `notifications::http` | Routes, auth mobile, validation et statuts HTTP |
| `RegisterDeviceDto` / `DeviceIdParamDto` | DTO serde + `ApiDto` | Rejet des champs inconnus, longueurs validator.js, plateforme et UUID |
| `MobileService` | `notifications::devices::DeviceService` | Trim ECMAScript, projection publique et erreurs métier |
| `MobileRepository` | `notifications::pg::PgNotificationRepository` | Compte/session verrouillés, upsert, liste et suppression propriétaire |
| `enqueueNotification` | `notifications::enqueue::enqueue_notification` | Notification, deliveries et jobs dans la transaction de l’appelant |
| `isBillingNotificationEligibleSql` | `notifications::eligibility` | Même prédicat réutilisé lors de la livraison S22 |

## Contrat HTTP

| Méthode | Route | Authentification | Réponse |
| --- | --- | --- | --- |
| `GET` | `/api/users/me/devices` | JWT mobile actif, onboarding complet requis | `200 { "devices": [...] }` |
| `POST` | `/api/users/me/devices` | JWT mobile actif, onboarding complet requis | `201` avec l’appareil public |
| `DELETE` | `/api/users/me/devices/{id}` | JWT mobile actif, onboarding complet requis | `204` sans corps |

Le corps POST accepte uniquement `push_token`, `platform` (`ios` ou `android`) et
`app_version` facultatif ou `null`. La longueur est validée avant trim, comme class-validator :
20 à 4 096 caractères pour le token et au plus 50 pour la version. Le service applique ensuite
le trim ECMAScript ; une version vide devient `null`.

La projection n’expose jamais `token` ni `user_id`. Elle conserve `id`, `session_id`,
`platform`, `app_version`, `created_at` et `last_used_at`, avec des dates UTC en millisecondes.
La liste reste triée par `last_used_at DESC NULLS LAST, id`.

Les erreurs propres au module sont :

- `400 invalid_device_payload` pour un corps invalide ;
- `400 invalid_device_id` pour un paramètre qui n’est pas un UUID RFC 4122 version 1 à 8 ;
- `401 authentication_required` si le compte ou la famille disparaît entre le guard et l’écriture ;
- `404 device_not_found` pour une ressource absente ou appartenant à un tiers.

## Invariants transactionnels

L’enregistrement verrouille d’abord le compte actif puis vérifie que la famille de refresh du JWT
est encore active. L’unicité du token fournisseur conserve l’identifiant et `created_at`, mais
réattribue atomiquement le token au compte et à la famille courants. Les révocations de familles
de S08 suppriment déjà leurs appareils par la clé étrangère et le repository de session.

`enqueue_notification` reçoit un `&mut PgConnection` fourni par la mutation métier. Il ne démarre
ni transaction ni appel réseau. Une CTE unique :

1. verrouille le destinataire actif en `FOR SHARE` ;
2. crée une notification dédupliquée par SHA-256 de la forme JSON compacte
   `[type, source_id, user_id]` ;
3. filtre les événements Stripe par l’état local courant ;
4. cible les appareils sans famille historique ou liés à une famille active ;
5. crée un UUID par livraison, réutilisé comme identifiant et agrégat du job
   `notification.push` puis comme identifiant de `notification_push_delivery`.

Le payload est une liste blanche : `new_match` contient `match_id`, `new_message` contient
`match_id`, `message_id` et `sender_id`, et les deux notifications de facturation ont `{}`.
Le texte privé d’un message et les objets fournisseur ne sont jamais copiés.

## PostgreSQL local autonome

`compose.dev.yaml` fournit désormais le PostgreSQL de développement du dépôt Rust sur le port
configuré dans `.env` (5433 actuellement). Un volume Rust distinct est initialisé par les assets
de `db/` avec la baseline, les migrations 017/018 et leurs checksums attendus.

```bash
docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d postgres
```

Cette initialisation s’applique à un volume neuf. Le binaire de migration incrémentale pour une
base déjà existante reste à livrer avant la bascule finale. Redis est ajouté par S22 ; les autres
services Docker sont ajoutés avec les lots qui les utilisent.

## Validation

```bash
export CARGO_BUILD_JOBS=1
cargo test --locked --test notifications_contract
cargo test --locked --features postgres-integration --test notifications_postgres
```

Le contrat couvre création, projection, validation, refus des opérations avant onboarding, suppression,
ressource absente et panne DB. Le test réel couvre l’upsert, les sessions révoquées, les appareils
historiques, le rollback, la déduplication concurrente, les prédicats Stripe, l’absence de contenu
privé et les cascades delivery/appareil.
