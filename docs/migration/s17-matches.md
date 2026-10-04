# S17 — matchs, reveal et continuation

> Compte rendu historique de ce lot. Pour les commandes et l’arborescence actuelles, consulter le [README](../../README.md) et l’[architecture](../architecture.md).

## Périmètre

S17 migre les quatre routes mobiles NestJS suivantes :

| Méthode | Route | Authentification | Réponse |
|---|---|---|---|
| `GET` | `/api/matches/me` | session mobile active et onboarding terminé | `{ matches, next_cursor }` |
| `PATCH` | `/api/matches/:id/reveal` | session mobile active et onboarding terminé | consentement de reveal et état mutuel |
| `PATCH` | `/api/matches/:id/continue` | session mobile active et onboarding terminé | consentement de continuation et confirmation mutuelle |
| `GET` | `/api/users/me/continuation-quota` | session mobile active et onboarding terminé | plan, consommation et quota restant |

La création interne d’un match réciproque est également disponible pour S19. Elle trie les UUID des participants,
crée les deux états individuels et programme les deux notifications `new_match` dans la transaction métier.

## Mapping NestJS vers Rust

| NestJS | Rust |
|---|---|
| `MatchesController` et DTO de pagination | `matches/http.rs`, extracteurs Axum et DTO Serde fermés |
| `MatchesService` | `MatchService` dans `matches/service.rs` |
| `MatchesRepository` | trait `MatchStore` et `PgMatchRepository` dans `matches/pg.rs` |
| modèles et projections publiques | types fermés dans `matches/domain.rs` |
| `JwtActiveGuard` | extracteur `OnboardedMobile` de S08 |
| `MobileDeliveryService` | trait `MatchEventPublisher`; l’adaptateur SSE sera branché par S22 |
| `PhotoStorageService.getSignedUrl` | `ProfilePhotoUrlProvider` de S12/S15 |
| `notification-outbox.ts` | `enqueue_notification` de S14 sur la transaction appelante |

## Invariants transactionnels

- Le match est verrouillé avant toute décision de reveal ou de continuation.
- `clock_timestamp()` est lu dans le `SELECT` extérieur après l’acquisition du verrou. Une attente ne prolonge donc
  pas l’ancienne fenêtre de 24 heures.
- Un match actif expiré passe d’abord en `awaiting_continuation` avec une nouvelle fenêtre de 24 heures.
- Une fenêtre de continuation expirée passe en `expired` avec une purge planifiée à 30 jours.
- Le premier consentement désigne l’initiateur sans consommer son quota. Le second consentement consomme le quota
  de cet initiateur et confirme le match dans la même transaction.
- L’upsert de `continuation_usage` est conditionnel. Des confirmations concurrentes sur plusieurs matchs ne peuvent
  pas dépasser la limite hebdomadaire.
- Une limite hebdomadaire de zéro autorise le premier consentement mais refuse la confirmation sans créer de ligne
  de consommation.
- Le début de semaine est le lundi en UTC.

## Projection et confidentialité

La liste exclut les matchs terminés, les comptes supprimés et les paires bloquées. Elle est triée par dernière
activité puis UUID décroissant et accepte l’offset historique ou le curseur opaque NestJS. Le curseur conserve le
format base64url JSON `{ at, id }` et refuse les timestamps ou UUID non canoniques.

Une bio, une réponse libre ou une photo n’est projetée que si sa modération courante est `approved`. La photo doit
en plus être `ready` et les deux participants doivent avoir consenti au reveal. PostgreSQL ne renvoie que la clé
objet privée ; l’URL courte est signée après la requête, au dernier moment. La projection conserve aussi les traits,
le dernier message et le nombre de messages non lus attendus par le contrat NestJS.

## Erreurs publiques

Les erreurs restent enveloppées sous `{ "error": { "code", "message" } }`. S17 conserve notamment
`invalid_pagination`, `invalid_match_id`, `invalid_match_request`, `invalid_cursor`, `match_not_found`,
`match_blocked`, `discovery_candidate_not_found`, `invalid_match_state`,
`continuation_not_available_yet`, `continuation_quota_reached`, `match_expired` et
`photo_storage_unavailable`, avec les statuts NestJS correspondants.

## Validation

Sans infrastructure :

```powershell
$env:CARGO_BUILD_JOBS='1'
cargo test --test matches_contract
cargo test --lib
cargo clippy --all-targets -- -D warnings
```

Avec le PostgreSQL de développement Rust :

```powershell
wsl.exe -e bash -lc 'cd /mnt/c/Users/nicol/Nicolas_Germani/Programmation/Histae/histae-api-rust && docker compose --env-file .env -f compose.dev.yaml up -d postgres'
$env:CARGO_BUILD_JOBS='1'
cargo test --features postgres-integration --test matches_postgres
```

Le test réel refuse une base autre que `histae-dev` sur loopback. Toutes ses lignes utilisent des UUID générés et
sont supprimées après le scénario.

## Limites du lot

Le routeur reste composable et sera assemblé dans le binaire API lors du lot d’intégration prévu. Le transport SSE
best-effort et son relais Redis implémentent désormais `MatchEventPublisher` dans S22. Les routes de messages,
la décision réciproque de découverte et les écrans administratifs sont livrés par S18, S19 et S23. S26 fournit la
maintenance d’expiration et de purge avec leader conservé entre les lots.
