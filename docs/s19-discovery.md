# S19 — découverte, feed et swipes

## Périmètre

S19 migre les trois routes mobiles du module NestJS `discovery` :

| Méthode | Route | Entrée | Réponse |
|---|---|---|---|
| `GET` | `/api/users/me/discovery-status` | aucune | `{ ready, required_actions, presence_expires_at }` |
| `GET` | `/api/feed` | `limit` de 1 à 100, `cursor` opaque | `{ profiles, next_cursor }` |
| `POST` | `/api/swipes` | `{ target_user_id, decision }` | `201` avec `{ decision, matched, match? }` |

Les routes exigent une session mobile active et l’acceptation des CGU et de la notice courantes. Le statut ne
consomme aucun quota. Le feed conserve la limite de 60 requêtes par minute et les swipes celle de 120 requêtes par
minute, par utilisateur.

## Mapping NestJS vers Rust

| NestJS | Rust |
|---|---|
| `DiscoveryController` et DTO | `discovery/http.rs` avec extracteurs Axum et DTO Serde fermés |
| `DiscoveryService` | `DiscoveryService` dans `discovery/service.rs` |
| `DiscoveryRepository` | port `DiscoveryRepository` et `PgDiscoveryRepository` dans `discovery/pg.rs` |
| `DiscoveryStore` | port `SwipeStore` et `PgSwipeStore` dans `discovery/pg.rs` |
| `AccountActivityService` | `AccountActivityPool` de S06, partagé par les deux comptes |
| `MatchesService.createFromMutualLike` | port `MatchCreator`, implémenté par `MatchService` de S17 |
| `RateLimitService` | `RateLimiter` de S07 avec les clés historiques `feed` et `swipes` |

## Éligibilité et projection publique

La requête SQLx reprend la requête PostgreSQL NestJS. Le demandeur et le candidat doivent être actifs, avoir un
profil sexué, des préférences, les versions courantes des consentements sensible et localisation, ainsi qu’une
présence marquée fraîche depuis moins d’une heure. Les préférences de sexe, d’âge et de distance s’appliquent dans
les deux sens. Les blocages dans les deux directions et les matchs existants excluent le candidat.

Le feed ne projette une bio et des réponses libres que lorsqu’une décision de modération `approved` existe. Les
traits sont triés par nom. Aucune photo ou URL signée n’est produite. La distance publique est arrondie à un chiffre,
mais le curseur conserve la distance PostgreSQL exacte et l’UUID pour l’ordre `(distance_km, user_id)`.

## Pagination après exclusion des swipes

Les décisions actives sont exclues après lecture de chaque lot de candidats. Pour éviter qu’un lot rempli de profils
déjà swipés masque les suivants, le service lit jusqu’à 20 lots de `max(50, (limit + 1) * 4)` entrées. Comme NestJS,
il peut donc renvoyer une page vide avec un curseur non nul lorsque cette borne est atteinte. Les décisions expirées
ne sont plus exclues.

## Immutabilité, concurrence et fenêtre de reprise

`swipe_decision` reste la source canonique PostgreSQL. Une ligne est immuable pendant exactement 365 jours. Un
replay identique renvoie la décision existante ; une décision différente produit `409 swipe_already_recorded`. Une
ligne expirée est remplacée sous `FOR UPDATE` avec une nouvelle période complète.

L’écriture tient les verrous de session partagés des deux comptes, triés par UUID, et vérifie le lease avant et après
la transaction. Le swipe, la lecture du like réciproque et la création du match restent trois opérations séparées,
comme dans NestJS. Une panne après le commit du swipe peut donc laisser temporairement deux likes sans match. Tant
qu’aucun match n’existe, rejouer le même like reprend la lecture réciproque et la création. La contrainte unique de
la paire dans `match_init` garantit un seul match lors de likes simultanés.

## Erreurs publiques conservées

- `invalid_feed_query`, `invalid_feed_request` et `invalid_cursor` ;
- `invalid_swipe_payload` et `invalid_swipe_request` ;
- `discovery_not_ready` et `discovery_candidate_not_found` ;
- `swipe_already_recorded`, `match_blocked` et `invalid_match_request` ;
- `feed_rate_limit_exceeded` et `swipe_rate_limit_exceeded` ;
- `account_unavailable`, `account_activity_unavailable` et `discovery_unavailable`.

## Validation

```powershell
$env:CARGO_BUILD_JOBS='1'
cargo test --lib
cargo test --test discovery_contract
cargo clippy --all-targets --features postgres-integration -- -D warnings
```

Avec PostgreSQL local :

```powershell
wsl.exe -e bash -lc 'cd /mnt/c/Users/nicol/Nicolas_Germani/Programmation/Histae/histae-api-rust && docker compose --env-file .env -f compose.dev.yaml up -d postgres'
$env:CARGO_BUILD_JOBS='1'
cargo test --features postgres-integration --test discovery_postgres
```

Les tests réels génèrent leurs UUID, refusent une base autre que `histae-dev` sur loopback et nettoient leurs
comptes après chaque scénario.

## Limites du lot

La purge quotidienne bornée des swipes expirés appartient à S26. L’export et l’effacement des décisions sont traités
dans les lots RGPD S24 à S26. S19 n’ajoute aucune variable d’environnement : les limites `RATE_LIMIT_FEED` et
`RATE_LIMIT_SWIPE`, PostgreSQL et les versions légales étaient déjà configurés.
