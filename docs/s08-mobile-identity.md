# S08 — identité mobile et familles de refresh

## Analyse de l’existant NestJS

`TokenService` signe des access tokens HS256 typés `access`, avec `sub`, `sid`, `iat`, `exp`, issuer et audience fixes. Le `kid` de l’en-tête sélectionne exclusivement une clé locale configurée. `JwtActiveGuard` ne fait jamais confiance à un rôle contenu dans le token : il relit à chaque requête le compte, les consentements courants et la famille active dans PostgreSQL. Les routes de ce lot autorisent toutes un onboarding incomplet.

Les refresh tokens ont la forme `<uuid-v4>:<43 caractères base64url>`. Seul le SHA-256 hexadécimal du secret est conservé. `RefreshSessionRepository` garde les ancêtres après rotation jusqu’à leur expiration initiale. Un mauvais secret est rejeté avant le verrou du compte et ne change aucun état. Le rejeu d’un ancêtre authentique, non expiré et déjà roté révoque la famille et ses appareils ; la transaction doit être validée avant le retour `401`.

Toutes les mutations de session verrouillent d’abord `user_account`, puis le token et la famille concernés. Cette séquence les sérialise avec le bannissement, l’effacement et l’enregistrement des appareils. La rotation consomme l’ancien token, insère un unique enfant et étend la famille dans une même transaction. Une erreur d’insertion doit donc restaurer l’ancien token utilisable.

La liste des sessions ne révèle que l’identifiant de famille, trois dates et l’indicateur `current`. Elle trie par `(created_at DESC, id DESC)`, utilise un curseur base64url JSON `{at,id}` et formate les réponses JSON à la milliseconde, même si le curseur PostgreSQL conserve six chiffres de fraction.

La cryptographie téléphone accepte la compatibilité de configuration existante : une clé de 64 caractères hexadécimaux est décodée, sinon la valeur UTF-8 doit faire exactement 32 octets. Le chiffrement AES-256-GCM concatène nonce de 12 octets, ciphertext et tag de 16 octets. Le hash de téléphone est un HMAC-SHA256 hexadécimal.

## Mapping NestJS → Rust

| NestJS | Rust | Responsabilité |
| --- | --- | --- |
| `crypto/phone-crypto.ts` | `src/infra/crypto.rs` | Clé, AES-256-GCM, HMAC-SHA256 et SHA-256 sans exposition de secret |
| `auth/token.service.ts` | `src/identity/mobile/tokens.rs` | Création/parsing refresh, signature et vérification JWT, horloge injectable |
| `auth/auth.models.ts` | `src/identity/mobile/domain.rs` | Identités fermées, lignes de session et curseur |
| `auth/refresh-session.repository.ts` | `src/identity/mobile/pg.rs` | SQL, transactions, verrous, rotation, replay, révocation et appareils |
| `auth/auth.service.ts` | `src/identity/mobile/service.rs` | Cas d’usage, paires de tokens, pagination et erreurs métier |
| `auth/auth.guard.ts` | extracteurs dans `src/identity/mobile/http.rs` | Bearer strict, vérification JWT, relecture compte/famille et onboarding |
| `auth/auth.controller.ts` et DTO | routes et DTO dans `src/identity/mobile/http.rs` | Contrat HTTP, validation, statuts et quotas dédiés |

`MobileSessionStore` est le port d’application vers la persistance. Il ne reproduit pas le conteneur DI NestJS : le routeur reçoit un `MobileAuthState` explicite et partage des valeurs clonables et `Send + Sync`.

## Contrat HTTP livré

| Méthode et route | Auth | Succès | Validation et effets |
| --- | --- | --- | --- |
| `GET /api/auth/me` | JWT mobile actif | `200 {user_id,onboarding_complete}` | Relit compte et famille ; ne publie pas le rôle |
| `POST /api/auth/refresh` | publique | `200 {access_token,refresh_token}` | Corps strict, refresh ≤128 caractères, quota IP, rotation atomique |
| `POST /api/auth/logout` | JWT mobile actif | `204` | Corps strict, refresh de la même famille, `device_id` UUID facultatif, quota utilisateur |
| `GET /api/auth/sessions` | JWT mobile actif | `200 {sessions,next_cursor}` | `limit` 1–100, défaut 20, curseur ≤512, quota utilisateur |
| `DELETE /api/auth/sessions/:id` | JWT mobile actif | `204` | UUID v4, propriétaire obligatoire, révocation idempotente d’une cible déjà révoquée |
| `POST /api/auth/logout-all` | JWT mobile actif | `200 {revoked_sessions}` | Corps exact `{confirm:true}`, révocation de toutes les familles et appareils |

L’extraction axum conserve l’ordre visible de NestJS : l’authentification précède la validation du DTO sur les routes protégées ; le quota dédié intervient après la validation. Le lifecycle S07 injecte l’IP client résolue dans les extensions avant le handler de refresh.

## Erreurs et décisions de sécurité

- En-tête Bearer absent ou mal formé : `401 authentication_required`.
- JWT mal signé, expiré, de mauvais type, avec `kid` inconnu ou `sid` non UUID v4 : `401 invalid_or_expired_access_token`.
- Famille absente/révoquée/expirée ou compte supprimé : `401 authentication_required`.
- Compte banni : `403 account_unavailable`.
- Échec PostgreSQL pendant la relecture du guard : `500 account_check_failed`.
- Refresh mal formé, expiré, déjà consommé ou étranger : `401 invalid_or_expired_refresh_token`.
- Curseur décodable mais invalide : `400 invalid_cursor`; query invalide avant décodage : `400 invalid_session_query`.
- Session absente ou appartenant à un tiers : `404 session_not_found`.

`jsonwebtoken` est utilisé sans backend OpenSSL et limité à HS256 parce que le contrat existant l’exige. `aes-gcm`, `hmac` et `sha2` fournissent des primitives auditées et interopérables avec Node. SQLx reste en requêtes dynamiques dans ce lot, car la compilation hors ligne du repository n’embarque pas les métadonnées de schéma ; les types des tuples de résultat restent explicites.

## Tests

Les tests unitaires vérifient les vecteurs produits par Node pour AES-GCM, HMAC, JWT et hash de refresh, ainsi que la forme opaque et les curseurs. `mobile_auth_contract` couvre le Bearer absent/invalide, la relecture de famille, l’ordre auth/validation, les champs inconnus, les valeurs par défaut, la pagination, les dates, le curseur invalide, l’UUID v4, la ressource absente et `confirm:true`.

```powershell
cargo test --locked --test mobile_auth_contract
```

`mobile_identity`, activé explicitement, refuse une base autre que `histae-dev` et une adresse autre que loopback. Il crée des comptes UUID isolés puis les supprime. Il couvre le faux secret, la rotation, le replay committé, deux rotations concurrentes, le rollback après collision d’enfant, le logout avec ancêtre, le nettoyage des appareils, l’isolation par propriétaire et les révocations ciblée/globale.

```powershell
cargo test --locked --features postgres-integration --test mobile_identity
```

## Limites du lot

`POST /api/auth/otp/verify` consommera `MobileAuthService::issue_token_pair` dans S09. Le routeur S08 est composable et testé, mais le binaire `api` reste volontairement non activé tant que S15 n’a pas fourni la sonde réelle du stockage objet requise pour la readiness. Les sessions administratives WebAuthn restent entièrement séparées et appartiennent à S10.
