# S16 — modération et administration photo

> Compte rendu historique de ce lot. Pour les commandes et l’arborescence actuelles, consulter le [README](../../README.md) et l’[architecture](../architecture.md).

## Contrat migré

| Méthode | Route | Comportement |
| --- | --- | --- |
| `GET` | `/api/admin/content-moderation` | Filtres `status?`, `content_type?`, pagination `limit=20`, `offset=0`, `cursor?`; répond `{ cases, next_cursor }`. |
| `GET` | `/api/admin/content-moderation/:id` | UUID et `reason` de 3 à 500 caractères ; audite la lecture puis signe la photo privée si le cas en contient une. |
| `PATCH` | `/api/admin/content-moderation/:id` | Version, décision, motif et contrôles photo éventuels ; répond `{ "message": "content moderation decision recorded" }`. |
| `GET` | `/api/admin/photo-reconciliation` | Filtres `all`, `stale_processing`, `deleting`, `dead_letter` et pagination ; ne renvoie aucune clé objet ni URL. |
| `POST` | `/api/admin/photo-reconciliation/:id/retry` | Motif opérateur, audit et remise en file atomique ; répond `202 { "message": "photo reconciliation queued" }`. |

Les DTO refusent les champs inconnus. Les curseurs conservent le JSON base64url NestJS avec timestamp UTC à trois
à six décimales et UUID RFC 4122 versions 1 à 8. Un curseur non vide est incompatible avec `offset`. Les dates de
réponse sont des chaînes ISO UTC à la milliseconde, comme la sérialisation des `Date` NestJS.

## Mapping NestJS vers Rust

| NestJS | Rust |
| --- | --- |
| `ModerationController` | `moderation::http` |
| `ModerationService` | `moderation::service::ModerationService` |
| `ModerationRepository` | `moderation::pg::PgModerationRepository` |
| `PhotoModerationService` | `moderation::photo::HttpPhotoModerator` |
| `AdminPhotoRepository` et méthodes `AdminService` | `administration::photos` |
| FastAPI/OpenCV/ONNX | `services/photo-moderation` |
| `PhotosService.upload` | `media::service::PhotoService` avec `PhotoModerator` injecté |

Les rôles admin vivent dans `identity::admin_role`, indépendamment de la feature WebAuthn, afin que repositories et
tests PostgreSQL n’aient pas à compiler le moteur cryptographique. Les handlers HTTP restent conditionnés par
`webauthn-probe` et réutilisent les extracteurs S10, le cookie opaque, le contrôle d’origine et la réauthentification
récente des mutations.

## Décision automatique

`HttpPhotoModerator` envoie le WebP normalisé avec `Content-Type: image/webp`, une taille explicite et un bearer
token, dans le timeout configuré. La réponse doit contenir un nombre de visages entier entre 0 et 100, une netteté
finie positive et un score NSFW fini entre 0 et 1.

Une photo est automatiquement `approved` uniquement avec un visage, une netteté au-dessus du seuil et un score
NSFW sous le seuil. Les codes restent ordonnés : `face_not_detected`, `multiple_faces`, `blurry`, `explicit_image`.
Tout code conserve la photo en `pending`. Une panne réseau, un timeout, un statut HTTP non réussi ou un JSON
invalide donne `pending/analysis_unavailable`. Aucun chemin automatique ne produit `rejected`.

L’analyse a lieu après la conversion bornée et avant l’écriture des métadonnées/S3. Sa décision est insérée avec
l’activation de la photo et l’achèvement de la demande idempotente dans la même transaction. Un replay réutilise la
décision persistée sans nouvelle conversion, analyse ou écriture objet.

## Revue et réconciliation transactionnelles

Le détail charge le contenu et écrit `view_moderation_content` dans une transaction. L’URL photo n’est produite
qu’après le commit de cet audit. Une panne de signature devient `503 photo_storage_unavailable` sans annuler la
trace d’accès.

La revue verrouille le cas, compare `version` et refuse une photo qui n’est plus `ready`. Une approbation photo
exige `face_detectable`, `sharp_enough` et `content_allowed` à `true`; un rejet exige au moins une valeur `false`.
Le rejet passe la photo à `deleting`, réinitialise ou crée `photo.delete`, incrémente la version et écrit
`admin_review_content` dans une seule transaction.

La réconciliation refuse les photos `ready`, les traitements de moins de 30 minutes et les événements détenus
depuis moins de 5 minutes par un worker. Une reprise valide passe la photo à `deleting`, réinitialise l’événement et
écrit `admin_reconcile_photo` dans la transaction verrouillée.

## Service local autonome

Le service `services/photo-moderation` utilise Python 3.12, FastAPI, OpenCV et `opennsfw-onnx`. Son image embarque
les modèles, s’exécute avec l’UID `10001`, sans accès en écriture hors `/tmp`, sans OpenAPI ni logs d’accès. Compose
limite CPU, mémoire, processus et exposition réseau. Les variables `PHOTO_MODERATION_*` sont déjà présentes dans le
`.env` Rust ; aucun fichier du dépôt NestJS n’est lu.

```bash
docker compose --env-file .env -f compose.dev.yaml up --build photo-moderation
```

## Validation

- analyse sûre, signaux de revue, provider désactivé, réseau indisponible et réponse malformée ;
- validation stricte des trois contrôles, motifs NFKC/ECMAScript et curseurs microseconde ;
- ordre audit puis signature et absence de signature pour une ressource inexistante ;
- PostgreSQL réel : audit de détail, conflit de version, rejet atomique, outbox, reprise d’un traitement ancien et
  refus d’un worker actif ;
- régression PostgreSQL/S3 S15 après ajout de la décision automatique ;
- build du service Docker, healthcheck et analyse réelle d’une fixture WebP publique.

## Divergence connue

Les contrôleurs NestJS `ModerationController` et `AdminController` n’attachent pas explicitement
`RecentAdminAuthenticationGuard` à ces mutations, alors que les invariants du dépôt exigent une authentification
admin récente pour les décisions sensibles et les reprises de dead letter. Les routes Rust appliquent
`RecentAdminIdentity`. Une comparaison différentielle utilisant une ancienne session doit donc attendre `401` côté
Rust et révèle ce défaut restant dans la référence NestJS.

## Limites reportées

Le branchement des routeurs dans le binaire API complet reste lié au bootstrap final. La reprise automatique des
photos anciennes et la purge sont livrées dans S26 ; l’état de maintenance persistant conserve leurs compteurs. La calibration des seuils, les
recours opérateur et la validation indépendante du modèle avant production restent des dépendances de la roadmap,
pas des constantes à inventer dans ce lot.
