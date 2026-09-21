# S10 — authentification administrateur WebAuthn

## Analyse de l’existant NestJS

`AdminAuthController` expose quatre routes publiques d’entrée WebAuthn et onze routes protégées par une session administrateur opaque. Les routes publiques appliquent le quota `admin-auth` après la validation DTO. Les routes protégées n’acceptent jamais le JWT mobile : elles lisent exclusivement un cookie de 43 caractères, hachent sa valeur et relisent le compte, la passkey et la session dans PostgreSQL.

`AdminSessionGuard` contrôle l’`Origin` exact avant la session pour chaque mutation. Il renouvelle l’expiration idle à chaque accès sans dépasser l’expiration absolue. Une session absente, expirée, révoquée, liée à une passkey révoquée ou à un compte indisponible renvoie `401 admin_session_invalid` et expire le cookie. `RecentAdminAuthenticationGuard` exige que la cérémonie ayant créé la session date de moins de `ADMIN_RECENT_AUTH_TTL`.

`AdminAuthService` produit des passkeys découvrables avec vérification utilisateur obligatoire. Les challenges et bootstraps sont à usage unique. Le compteur de signature est vérifié puis mis à jour sous verrou avec la nouvelle session et l’événement d’audit. Les listes n’exposent jamais token, hash ou clé publique. La passkey courante, la dernière passkey active et la session courante ne peuvent pas être révoquées par les routes ciblées.

NestJS stocke seulement le hash du challenge parce que SimpleWebAuthn accepte un callback de comparaison. `webauthn-rs-core` exige l’état complet de cérémonie pour appliquer les mêmes vérifications. La migration additive `018_postgres_admin_webauthn_state` ajoute donc `ceremony_state bytea`, nullable pour que NestJS reste compatible. Rust refuse de consommer un challenge sans cet état. Les challenges créés avant la bascule expirent naturellement après cinq minutes ; aucune passkey ni session existante n’est supprimée.

## Mapping NestJS → Rust

| NestJS | Rust | Responsabilité |
| --- | --- | --- |
| `AdminAuthController` | `identity::admin::http` | 15 routes, DTO stricts, statuts, cookies et erreurs publiques. |
| `AdminSessionGuard` | extracteur `AdminIdentity` | Origin mutation, cookie opaque et relecture PostgreSQL. |
| `RecentAdminAuthenticationGuard` | extracteur `RecentAdminIdentity` | Fraîcheur de la cérémonie WebAuthn. |
| `AdminAuthService` | `identity::admin::service::AdminAuthService` | Options, vérifications, sessions, passkeys, pagination et bootstrap. |
| `AdminAuthRepository` | `identity::admin::pg::AdminAuthRepository` | SQLx, transactions, verrous, audits et expiration glissante. |
| `@simplewebauthn/server` | `webauthn_probe::WebauthnProbe` validé en S02 | Vérification spécialisée WebAuthn et conversion COSE existante. |
| `admin-session-cookie.ts` | fonctions de cookie dans `identity::admin::http` | Cookie host-only, `HttpOnly`, `SameSite=Strict`, `Secure` en production. |
| script `create-admin-webauthn-bootstrap.ts` | binaire `admin-bootstrap` avec feature WebAuthn | Secret hors bande affiché une seule fois, hash seul en base et audit atomique. |

## Contrat HTTP conservé

| Méthode | Route | Succès | Protection |
| --- | --- | --- | --- |
| POST | `/api/admin/auth/login/options` | `200 { challenge_id, options }` | Publique, quota admin-auth. |
| POST | `/api/admin/auth/login/verify` | `200` session + cookie | Publique, quota admin-auth. |
| POST | `/api/admin/auth/bootstrap/options` | `200 { challenge_id, options }` | Bootstrap syntaxiquement valide, quota admin-auth. |
| POST | `/api/admin/auth/bootstrap/verify` | `201` session + cookie | Bootstrap/challenge uniques, quota admin-auth. |
| GET | `/api/admin/auth/session` | `200` session publique | Session admin. |
| POST | `/api/admin/auth/logout` | `204` + cookie expiré | Session admin + Origin. |
| GET | `/api/admin/auth/credentials` | `200` tableau | Session admin. |
| POST | `/api/admin/auth/credentials/options` | `201` options | Session récente + Origin. |
| POST | `/api/admin/auth/credentials/verify` | `201 { message }` | Session récente + Origin. |
| PATCH | `/api/admin/auth/credentials/:id` | `200 { message }` | Session récente + Origin. |
| DELETE | `/api/admin/auth/credentials/:id` | `204` | Session récente + Origin. |
| GET | `/api/admin/auth/sessions` | `200` tableau | Session admin. |
| DELETE | `/api/admin/auth/sessions/:id` | `204` | Session récente + Origin. |
| POST | `/api/admin/auth/sessions/revoke-others` | `201 { revoked_sessions }` | Session récente + Origin. |
| GET | `/api/admin/auth/events` | `200 { events, next_cursor }` | Session admin, limite 1..100, défaut 20. |

Les corps refusent les champs inconnus. Les UUID de challenge, passkey et session sont des UUID v4 canoniques. Le nom passe d’abord la contrainte DTO de 1 à 100 unités UTF-16 puis la normalisation NFKC, le trim, le refus des contrôles et la limite de 200 octets.

## Transactions et invariants

- Le challenge est consommé par un `UPDATE ... RETURNING` avant la vérification, comme dans NestJS ; un échec WebAuthn ne le rend pas réutilisable.
- Bootstrap, insertion de la première passkey, création de session et audit sont atomiques.
- Authentification, contrôle optimiste du compteur sous `FOR UPDATE`, mise à jour de la passkey, session et audit sont atomiques.
- Ajouter, renommer ou révoquer une passkey écrit l’audit dans la même transaction.
- Révoquer une passkey révoque toutes les autres sessions en conservant la session courante, après le contrôle de dernière passkey.
- Les dates publiques utilisent la précision milliseconde et le suffixe `Z` du contrat NestJS.
- La pagination des événements conserve l’ordre `(created_at DESC, id DESC)` et un curseur base64url opaque.
- Les rôles sont relus dans PostgreSQL ; aucun rôle du client, JWT mobile ou ancien cookie n’est accepté.

## Fichiers

- `src/identity/admin/domain.rs` : rôles, lignes métier, curseurs et sérialisation des dates.
- `src/identity/admin/pg.rs` : store abstrait pour tests et repository SQLx transactionnel.
- `src/identity/admin/service.rs` : orchestration WebAuthn, sessions, passkeys et bootstrap.
- `src/identity/admin/http.rs` : routes Axum, extracteurs, DTO, cookie et mapping `ApiError`.
- `src/bin/admin_bootstrap.rs` : émission hors bande du bootstrap.
- `src/webauthn_probe.rs` : moteur S02 désormais utilisé par S10.
- `db/018_postgres_admin_webauthn_state.sql` dans le dépôt NestJS : état opaque de cérémonie.

## Validation

Le build courant sans WebAuthn reste vérifiable sur Windows :

```powershell
cargo check --locked --all-targets
cargo test --locked
```

Le module S10 et son binaire exigent OpenSSL/Perl et se valident avec :

```powershell
cargo test --locked --features webauthn-probe
cargo clippy --locked --all-targets --features webauthn-probe,postgres-integration -- -D warnings
```

La procédure Windows complète est décrite dans [windows-openssl.md](windows-openssl.md). Le projet compile une copie vendored d’OpenSSL : il faut installer Strawberry Perl et les outils C++ Microsoft, pas une DLL OpenSSL globale.

La migration est appliquée par le migrateur TypeScript conservé :

```powershell
pnpm run db:migrate
```

## Limites du lot

Le routeur S10 reste composable comme les lots S07 à S09 ; le binaire API final sera assemblé lors de la phase de composition prévue par le plan. La validation cryptographique complète ne peut pas être exécutée sur le poste Windows tant qu’un Perl Windows compatible MSVC n’est pas installé. Aucun moteur factice n’est activé dans le code livré.
