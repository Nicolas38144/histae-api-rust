# S15 — photos privées et stockage objet

> Compte rendu historique de ce lot. Pour les commandes et l’arborescence actuelles, consulter le [README](../../README.md) et l’[architecture](../architecture.md).

## Contrat migré

`PUT /api/users/me/photo` exige une session mobile active avec onboarding terminé. La clé `Idempotency-Key` est
normalisée par trim et minuscules puis doit être un UUID v4 canonique. Cette validation précède le contrôle
multipart et la lecture des octets. La limite dédiée est de 10 requêtes par heure et par utilisateur.

Le multipart accepte exactement un fichier nommé `photo`, aucun champ ni second fichier. L’entrée est limitée à
500 000 octets. Les extensions, MIME, signatures, formats, pages, pixels, orientation, dimensions, métadonnées et
sortie WebP suivent le corpus S03. La réponse `200` conserve `message`, `photo`, `moderation_status` et
`moderation_reasons`. `DELETE /api/users/me/photo` répond `204` après la transition durable vers `deleting` et la
création transactionnelle du job outbox.

## Mapping NestJS vers Rust

| NestJS | Rust |
| --- | --- |
| `UsersController` multipart | `media::http`, extraction différée après clé et quota |
| `PhotosService` | `media::service::PhotoService` |
| `PhotosRepository` | `media::pg::PgPhotoRepository` |
| `ObjectStorageService` AWS SDK | `media::s3::S3ObjectStorage`, API S3 et SigV4 |
| `PhotoProcessorService` | `PhotoCodec` + package Node autonome `tools/photo-codec` |
| handler `photo.delete` | `PhotoDeletionHandler` implémentant `OutboxHandler` |
| provider d’URL profil | implémentation `ProfilePhotoUrlProvider` par `PhotoService` |
| `HeadBucket` de readiness | implémentation `DependencyProbe` par `S3ObjectStorage` |

## Protocole et idempotence

La première transaction verrouille le profil, purge la même clé expirée, arbitre replay/conflit/consommation et
crée la photo `processing` avec sa demande valable 24 heures. Le hash reprend les trois valeurs préfixées par leur
longueur big-endian : nom exact, MIME trimé en minuscules et octets source.

Après conversion, les métadonnées vérifiées sont enregistrées avant le `PUT` S3. La preuve de verrou d’activité
est relue immédiatement avant cet effet externe. Une panne de stockage laisse la ligne `processing` et sa clé objet
pour réconciliation. L’activation verrouille de nouveau le profil, passe l’ancienne photo à `deleting`, programme
`photo.delete`, active la nouvelle photo, crée la décision produite par l’analyse S16 et termine la demande dans une
transaction unique. Lorsque l’analyseur est désactivé ou indisponible, cette décision reste
`pending/analysis_unavailable`. Un replay identique signe seulement l’objet existant.

Le handler de suppression relit uniquement une photo `deleting`, supprime l’objet puis consomme les demandes et
retire la ligne PostgreSQL. DELETE S3 et l’absence de ligne sont idempotents. Une panne entre S3 et PostgreSQL est
rejouable sans restaurer la visibilité de la photo.

## Stockage local autonome

`compose.dev.yaml` fournit `chrislusf/seaweedfs:4.45` en mode `mini`, publié uniquement sur
`127.0.0.1:8333`, avec le volume `histae-rust-object-storage-data`. L’alias réseau
`storage.histae.localhost` correspond à `OBJECT_STORAGE_ENDPOINT`, ce qui préserve l’hôte utilisé dans les
signatures entre conteneurs et navigateur. SeaweedFS reste une cible de développement ; l’application ne dépend
d’aucun type propre à SeaweedFS.

## Validation

- tests du codec autonome sur JPEG, PNG, WebP, HEIC/HEIF, EXIF, animation, pixels, corruption et timeout ;
- tests unitaires du hash NestJS, des UUID v4 et de la restriction de namespace avant signature ;
- intégration PostgreSQL réelle : création, métadonnées, activation, replay, conflit, retrait, outbox et résultat
  consommé ;
- intégration S3 réelle : `HeadBucket`, PUT privé, URL signée lisible, double DELETE et objet ensuite absent ;
- suite Rust complète et Clippy strict.

## Limites reportées

La récupération automatique des traitements anciens et la purge bornée sont livrées dans S26 en conservant la
trace PostgreSQL jusqu’à la suppression confirmée de l’objet. L’image multi-stage et l’installation de production
du codec autonome appartiennent à S27.
