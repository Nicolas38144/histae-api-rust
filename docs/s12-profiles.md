# S12 — consentements, profil, préférences et présence

## Analyse de l’existant NestJS

`UsersController` expose deux routes de consentement utilisables pendant l’onboarding et cinq routes qui exigent
une session mobile active avec onboarding complet. `UsersService` distingue la validation structurelle des DTO des
règles métier : une date au mauvais format produit `invalid_profile_payload`, tandis qu’une date calendrier
impossible ou un âge inférieur à 18 ans produit `invalid_profile`.

Le profil est un remplacement complet des champs mobiles autorisés malgré sa méthode `PATCH`. `firstname` et
`birthdate` sont toujours requis. Omettre `sex` ou `bio`, ou envoyer `null`, remet la colonne correspondante à
`NULL`. Le prénom et la bio sont tronqués selon le `trim` ECMAScript puis limités en octets UTF-8, respectivement à
100 et 2 000 octets. Une bio non vide est analysée par les règles locales `text_rules_v1`; le propriétaire continue
de voir son texte et son état de modération.

Les choix juridiques forment un journal ordonné par `event_sequence`. Un rejeu du même état et de la même version
n’ajoute pas d’événement. Les CGU et la notice ne sont pas retirables par cette route. Le retrait du consentement
sensible efface le sexe et les préférences ; celui de la localisation efface la présence.

La protection décisive se trouve dans `UsersRepository`, pas dans le précontrôle du service : le compte est verrouillé
avec `FOR UPDATE`, puis les versions courantes sont relues dans la même transaction que l’écriture. Un retrait
concurrent ne peut donc pas laisser une préférence, un sexe ou une présence réintroduite après l’effacement.

## Contrat HTTP migré

| Méthode | Route | Identité | Résultat principal |
| --- | --- | --- | --- |
| `GET` | `/api/users/me/consents` | session mobile active, onboarding incomplet accepté | `200 { consents, onboarding_complete, required_actions }` |
| `PUT` | `/api/users/me/consents` | session mobile active, onboarding incomplet accepté | état complet des quatre choix |
| `GET` | `/api/users/me` | session mobile active et onboarding complet | profil propriétaire ou `404 profile_not_found` |
| `PATCH` | `/api/users/me/profile` | session mobile active et onboarding complet | `200 { message: "profile updated" }` |
| `GET` | `/api/users/me/preferences` | session mobile active et onboarding complet | préférences ou `404 preferences_not_found` |
| `PATCH` | `/api/users/me/preferences` | session mobile active et onboarding complet | `200 { message: "preferences updated" }` |
| `PATCH` | `/api/users/me/presence` | session mobile active et onboarding complet | `200 { message: "presence updated" }` |

Les routes photo appartiennent à S15. L’émission et la consommation du jeton de suppression appartiennent à S25.

## Mapping NestJS → Rust

| NestJS | Rust | Responsabilité |
| --- | --- | --- |
| `users.models.ts` | `profiles::domain` | Types fermés, projections publiques, consentements et résultats d’écriture. |
| `users.dto.ts` | `profiles::http` | Types de transport Serde stricts et codes d’erreur propres à chaque payload. |
| `UsersController` | `profiles::http::routes` | Routes Axum, identité mobile, IP cliente et user-agent. |
| `UsersService` | `profiles::service::ProfileService` | Validation métier, état des consentements et mapping propriétaire. |
| `UsersRepository` | `profiles::pg::PgProfileRepository` | SQLx, transactions, verrou compte et effacements immédiats. |
| `users.mapper.ts` | construction de `PublicProfile` | Omission de `sex`, `bio` et `photo` quand absents. |
| `TextModerationService` | `moderation::text::TextModerator` | Normalisation NFKD et règles déterministes dans le même ordre. |
| `Date`/horloge globale | `shared::clock::Clock` | Date UTC injectable pour l’âge et instant de présence. |
| `String.trim`/`Buffer.byteLength` | `shared::text` | Trim ECMAScript, y compris BOM, et longueur UTF-8. |

Les traits `ProfileStore`, `Clock` et `ProfilePhotoUrlProvider` représentent uniquement les frontières DB, temps et
stockage réellement substituées dans les tests. Aucun conteneur DI ni modèle de classe NestJS n’est reproduit.

## Invariants conservés

- Les champs inconnus ou les enums invalides sont rejetés avant le service avec le code DTO de la route.
- L’authentification et l’onboarding sont évalués avant la désérialisation du body sur les routes protégées.
- Une date de naissance est une date calendrier stricte `YYYY-MM-DD`; l’âge est calculé sur la date UTC courante.
- Les nombres JSON `25` et `25.0` sont équivalents, puis les préférences exigent des entiers dans les bornes historiques.
- Les quatre consentements sont toujours retournés dans l’ordre historique avec URL et version requise.
- `updated_at` utilise le format JSON de `Date`, à la milliseconde et avec suffixe `Z`.
- Le client ne fournit jamais une version juridique et ne reçoit ni IP ni user-agent d’audit.
- Le retrait et l’écriture sensible partagent l’ordre de verrouillage compte d’abord.
- Une erreur PostgreSQL `P0E01` reste `409 account_unavailable` sans détail du driver.
- Une clé objet photo n’est jamais sérialisée. Seul `ProfilePhotoUrlProvider` peut produire une URL propriétaire.

## Validation

Sans infrastructure :

```powershell
cargo test --locked --test profiles_contract
cargo test --locked --lib profiles::
cargo test --locked --lib moderation::text::
cargo test --locked --lib shared::
```

Avec PostgreSQL local `histae-dev` sur loopback :

```powershell
cargo test --locked --features postgres-integration --test profiles_postgres
```

Le test PostgreSQL utilise un UUID aléatoire et un nettoyage ciblé. Il lance un retrait de consentement sensible en
concurrence avec une réécriture de préférences, vérifie l’absence finale de sexe et de préférences, puis contrôle
l’effacement de présence, le refus des écritures ultérieures et l’absence de nouvel événement lors d’un rejeu.

## Limites du lot et autonomie finale

S12 n’ajoute aucune migration : toutes ses tables et contraintes existent dans la baseline courante. Pendant la
migration incrémentale, la base locale est encore préparée par le migrateur TypeScript décrit en S05. Cette
dépendance est temporaire. Le migrateur, les scripts de préparation et les services Docker nécessaires devront avoir
leur équivalent dans `histae-api-rust` avant la suppression du dépôt NestJS et avant la bascule S29.

La signature d’URL propriétaire reste une frontière sans implémentation réseau jusqu’à S15. Les réponses de profil
sont néanmoins testées avec un fournisseur contrôlé qui prouve qu’une clé objet n’est jamais exposée directement.
