# S13 — catalogues, traits, questions et réponses de profil

## Périmètre HTTP

| Méthode | Route | Authentification | Résultat nominal |
| --- | --- | --- | --- |
| GET | `/api/plans` | publique | `200 { plans }` |
| GET | `/api/traits` | mobile, onboarding complet | `200 { traits }` |
| GET | `/api/users/me/traits` | mobile, onboarding complet | `200 { traits }` |
| POST | `/api/users/me/traits` | mobile, onboarding complet | `204` |
| DELETE | `/api/users/me/traits/:traitId` | mobile, onboarding complet | `204` |
| POST | `/api/admin/traits` | session admin et Origin | `201` avec le trait |
| PATCH | `/api/admin/traits/:id` | session admin et Origin | `200 { message: "trait updated" }` |
| DELETE | `/api/admin/traits/:id` | session admin et Origin | `204` |
| GET | `/api/profile-questions` | mobile, onboarding complet | `200 { questions }` |
| GET | `/api/users/me/profile-answers` | mobile, onboarding complet | `200 { answers }` |
| PUT | `/api/users/me/profile-answers` | mobile, onboarding complet | `200 { answers }` |
| GET | `/api/admin/profile-questions` | session admin | `200 { questions }` |
| POST | `/api/admin/profile-questions` | session admin et Origin | `201` avec la question |
| PATCH | `/api/admin/profile-questions/:id` | session admin et Origin | `200` avec la question |
| DELETE | `/api/admin/profile-questions/:id` | session admin et Origin | `204` |

Les handlers admin sont compilés avec `webauthn-probe`, comme le socle S10 dont ils réutilisent directement
`AdminIdentity`. Ils ne peuvent donc pas accepter un JWT mobile. Les handlers mobiles utilisent
`OnboardedMobile` et relisent le compte, la famille de session et les versions légales dans PostgreSQL via S08.

## Mapping NestJS vers Rust

| NestJS | Rust |
| --- | --- |
| `PlansController/Service/Repository` | `catalog::http`, `CatalogService::plans`, `PgCatalogRepository::list_plan_rows` |
| `TraitsController/Service/Repository` | handlers traits, règles dans `CatalogService`, SQL dans `CatalogStore` |
| `ProfileQuestionsController/Service/Repository` | handlers questions/réponses, types fermés dans `catalog::domain`, transaction SQLx |
| `TextModerationService` | `moderation::text::TextModerator` partagé avec S12 |
| `JwtActiveGuard` | `OnboardedMobile` |
| `AdminSessionGuard` | `AdminIdentity` |
| `ApiValidationPipe` ciblé | `ValidatedJson`/`ValidatedPath`, `deny_unknown_fields` et validation `ApiDto` |

## Compatibilité conservée

- Les plans actifs sont triés par prix mensuel puis code. Leurs features restent triées par `sort_order` puis code.
  `weekly_continuation_limit` est absent du JSON lorsqu’il vaut `NULL`; `feature_value` conserve son JSON.
- Les traits sont triés par nom puis UUID. Leur nom utilise le trim ECMAScript et une limite de 100 octets UTF-8.
  L’unicité PostgreSQL reste sensible à la casse. L’ajout et le retrait d’une attribution sont idempotents.
- Le body mobile reste `{ "traitId": "..." }`; `{ "trait_id": "..." }` est refusé.
- Les UUID de paramètres et de body suivent `IsUUID('all')`: variante RFC 4122 et versions 1 à 8.
- Une réponse est normalisée en NFKC puis trim ECMAScript. Elle contient 10 à 300 scalaires Unicode, au plus
  1 000 octets UTF-8 et aucun contrôle C0/C1. Au plus trois questions distinctes sont acceptées.
- Le remplacement verrouille le profil vivant, partage les questions, supprime puis recrée réponses et cas de
  modération dans la même transaction. Une question absente laisse donc l’ancienne collection intacte.
- Le propriétaire voit aussi ses réponses `pending`; leur filtrage dans les projections de découverte et de match
  appartient aux lots S17/S19.
- Les prompts admin utilisent NFKC, trim ECMAScript, 3 à 200 scalaires, 500 octets maximum et aucun contrôle C0/C1.
  L’index PostgreSQL conserve l’unicité insensible à la casse.
- `display_order` reproduit la coercition `Number` locale de `class-transformer`, y compris les chaînes numériques,
  puis impose un entier entre 0 et 10 000. Une absence à la création donne 100.
- `answer_count` est calculé en PostgreSQL. La suppression d’une question cascade volontairement ses réponses et
  leurs cas de modération.

## Validation

Validation autonome :

```powershell
$env:CARGO_BUILD_JOBS = "1"
cargo test --all-targets
cargo test --all-targets --features webauthn-probe
cargo clippy --all-targets --features webauthn-probe -- -D warnings
```

Validation PostgreSQL locale, limitée par le test à `ENV=development`, une adresse loopback et la base
`histae-dev` :

```powershell
cargo test --features postgres-integration --test catalog_postgres
```

Le test crée tous ses UUID avec `Uuid::new_v4`, vérifie l’ordre, l’idempotence des traits, les trois positions,
la modération, l’atomicité en présence d’une question inconnue, les compteurs et les deux cascades, puis nettoie ses
propres données.

## Limites restantes

Le routeur S13 est composable mais le binaire `api` ne démarre pas encore l’application complète; l’assemblage final
reste prévu dans les lots d’exploitation. Les migrations, seeds et services Docker sont encore fournis par le dépôt
NestJS pendant la parité. Le dépôt Rust devra recevoir leurs équivalents autonomes avant S29, conformément au
README racine. La confirmation destructive du dashboard admin reste une responsabilité du client; l’API conserve
le `DELETE` et sa cascade existants.

