# S23 — Blocages, signalements et vues administratives

## Analyse de l’existant

Les blocages exigent un compte mobile actif et un onboarding terminé. Un blocage est idempotent, refuse
l’auto-blocage, vérifie que la cible existe et clôt dans la même transaction tous les matchs encore utilisables entre
les deux comptes. Les matchs terminés reçoivent une échéance de purge à 30 jours. La liste ne signe jamais de photo
et retourne donc toujours `photo: null`. Après commit, un événement SSE `matches.invalidated` est envoyé aux deux
comptes en best-effort.

Un signalement accepte les motifs `inappropriate_content`, `fake_profile`, `harassment`, `spam` et `other`. Sa
description optionnelle est limitée à 2 000 octets. L’auto-signalement est interdit ; la cible doit exister et le
match optionnel doit relier exactement le déclarant et la cible. PostgreSQL garantit un seul signalement `pending`
par paire déclarant/cible. La création est limitée à cinq requêtes par heure et par utilisateur par défaut.

Les vues administratives utilisent uniquement la session WebAuthn. Les listes de comptes ne signent aucune photo.
Le détail d’un compte, ses matchs et ses messages exigent un motif de 3 à 500 caractères et inscrivent leur audit dans
la transaction de lecture. Un détail ne signe sa photo qu’après le commit de cet audit. Un administrateur ne peut
agir que sur un utilisateur ; un superadministrateur ne peut agir ni sur lui-même ni sur un autre
superadministrateur. Un bannissement révoque les familles mobiles, les refresh tokens et les appareils dans la même
transaction.

## Mapping NestJS → Rust

| NestJS | Rust | Responsabilité |
| --- | --- | --- |
| `PrivacyService/Repository` pour les blocs | `privacy::{service,pg,http}` | Blocage atomique, liste sans photo et invalidation SSE |
| `ReportsService/Repository` | `reports::{service,pg,http}` | Création, liste admin, transitions et audit |
| `AdminService/Repository` | `administration::{service,pg,http}` | Comptes, bannissement, matchs et messages administratifs |
| `JwtActiveGuard` | `OnboardedMobile` | Compte, famille, CGU et notice courantes |
| `AdminSessionGuard` | `AdminIdentity` | Session WebAuthn opaque et origine exacte des mutations |
| `recordAdminAudit` | écritures SQL dans les transactions appelantes | Audit des accès et mutations sensibles |

## Contrats livrés

- `GET /api/users/me/blocks` ;
- `POST /api/users/me/blocks/:userId` ;
- `DELETE /api/users/me/blocks/:userId` ;
- `POST /api/reports` ;
- `GET /api/admin/reports` ;
- `PATCH /api/admin/reports/:id` ;
- `GET /api/admin/me` ;
- `GET /api/admin/users` ;
- `GET /api/admin/users/:id` ;
- `PATCH /api/admin/users/:id/status` ;
- `GET /api/matches/:userId`, route administrative malgré son chemin ;
- `GET /api/admin/matches/:id/messages`.

Les collections utilisent les mêmes valeurs par défaut `limit=20`, `offset=0`, l’ordre décroissant
date/identifiant et les curseurs base64url de la version NestJS. `offset` avec un curseur reste refusé. Les champs
optionnels `match_id` et `description` d’un signalement sont omis de la réponse lorsqu’ils sont absents.

## Sécurité et stockage

Les DTO refusent les champs inconnus. Aucun téléphone, hash, token, clé objet ou position précise n’est projeté. La
liste admin et la liste des blocages n’appellent jamais le service de signature. Les conversations administratives
sont auditées pour les deux participants avant leur lecture. Les transitions de signalement et leur audit partagent
une transaction.

S23 réutilise le schéma PostgreSQL existant, le rate limiting de S07, les sessions mobiles de S12, WebAuthn de S10 et
le relais SSE de S22. Aucune variable `.env`, migration ou ressource Docker supplémentaire n’est nécessaire.

## Validation

```powershell
cargo test --lib
cargo test --test s23_contract
cargo test --features postgres-integration --test s23_postgres -- --test-threads=1
cargo check --all-targets --all-features
cargo clippy --all-targets --all-features -- -D warnings
```

Les UUID des fixtures sont générés. Les tests couvrent authentification et onboarding, champs inconnus, projections
optionnelles, auto-blocage, cible absente, idempotence, absence de photo signée, ressource inexistante, permissions de
bannissement, révocation mobile, audits et violations d’unicité PostgreSQL.

## Risques restant ouverts

La suppression d’un blocage ne restaure pas un match terminé, conformément à NestJS. Les invalidations SSE restent
best-effort et le client doit relire ses ressources. Les routes sont fournies sous forme de routeurs composables ;
leur assemblage dans le binaire final reste couvert par les jalons de bascule S28/S29.
