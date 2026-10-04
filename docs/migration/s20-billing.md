# S20 — parcours client Stripe

> Compte rendu historique de ce lot. Pour les commandes et l’arborescence actuelles, consulter le [README](../../README.md) et l’[architecture](../architecture.md).

## Analyse de l’existant

Le lot couvre les trois routes mobiles de consultation d’abonnement, de création d’une session Checkout et
d’ouverture du portail Stripe. Les webhooks, la projection des événements fournisseur et la réconciliation
périodique sont livrés par S21.

Le comportement NestJS ne se limite pas à appeler Stripe. Avant tout `POST /customers`, PostgreSQL conserve
l’intention de création et programme `billing.customer.reconcile` dans la même transaction. La même tentative et
la même clé Stripe peuvent être rejouées pendant moins de 23 heures. Une intention plus ancienne ne provoque
jamais un nouveau POST : elle exige une réconciliation en lecture afin d’éviter deux Customers pour un compte.

Checkout utilise le prix, la durée d’essai et les URLs configurés côté serveur. Un seul Checkout peut être en cours
par utilisateur. Une clé d’idempotence UUID v4 rejouée avec la même période restitue la session encore ouverte ;
un changement de période, une session consommée ou une autre tentative vivante produit un conflit stable. Le
verrou d’activité de compte est contrôlé immédiatement avant chaque effet externe du parcours Checkout.

## Mapping NestJS vers Rust

| NestJS | Rust | Responsabilité |
| --- | --- | --- |
| `BillingController` | `billing/http.rs` | Routes Axum, auth mobile, DTO stricts, statuts et quota dédié |
| `BillingService` | `billing/service.rs` | Accès Premium, orchestration Checkout/portail et compensations |
| `BillingRepository` | `billing/pg.rs` | Transactions, verrous de ligne, idempotence et watchdog Customer |
| `StripeGateway` | `billing/stripe.rs` | Requêtes Stripe bornées, formulaires, version API et retries |
| `billing.models.ts` | `billing/domain.rs` | Types fermés et représentations JSON publiques |
| `AccountActivityService` | `AccountActivityPool` S06 | Verrou partagé et preuve de lease avant effet externe |
| exceptions NestJS | `BillingError` puis `ApiError` | Codes, messages et statuts publics stables |

## Contrat HTTP

| Méthode | Route | Auth | Entrée | Résultat |
| --- | --- | --- | --- | --- |
| GET | `/api/users/me/subscription` | Mobile onboardé | Aucune | `200 SubscriptionView` |
| POST | `/api/users/me/subscription/checkout` | Mobile onboardé | `{ billing_period }` et `Idempotency-Key` UUID v4 | `201 { session_id, url, expires_at }` |
| POST | `/api/users/me/subscription/portal` | Mobile onboardé | Aucun corps requis | `201 { url }` |

Le DTO Checkout refuse les champs inconnus. Le client ne peut fournir ni Customer ID, ni Product ID, ni Price ID,
ni montant, ni devise, ni essai. Checkout et portail partagent le quota `billing` configuré, 10 requêtes par minute
et par utilisateur par défaut.

## Décisions techniques

### Client Stripe HTTP explicite

Le crate n’ajoute pas un SDK Rust dont le modèle ou la version d’API divergerait du SDK Node de référence. Le
client `reqwest` envoie les mêmes formulaires `application/x-www-form-urlencoded`, la version Stripe
`2026-07-29.dahlia`, les mêmes clés d’idempotence et des délais/retries bornés par `BillingConfig`. Les réponses
sont limitées à 65 536 octets et ne sont jamais journalisées.

Cette approche rend les champs réellement envoyés testables avec un faux serveur TCP. Elle impose de maintenir
explicitement les opérations supplémentaires introduites en S21.

### Frontière repository/service

Le repository décide atomiquement si une demande est une création, un retry, un replay ou un conflit. Le service
ne reconstruit pas cet état en mémoire. Il orchestre les appels réseau et les compensations : expiration d’une
session non persistée et suppression confirmée d’un Customer créé mais non rattaché.

### Réutilisation du schéma et de la configuration

Le schéma autonome Rust contient déjà `subscription_plan`, `user_subscription`, `billing_customer` et
`billing_checkout_session`, ainsi que l’outbox S11. `BillingConfig`, `.env` et `compose.dev.yaml` contiennent déjà
les variables et le PostgreSQL nécessaires. S20 n’ajoute donc ni migration, ni variable, ni conteneur.

## Tests

- `tests/billing_contract.rs` vérifie auth, réponses publiques, DTO strict, `201`, erreurs et rate limit.
- `tests/billing_stripe.rs` compare méthode, chemin, headers, version Stripe, idempotence et formulaires réellement
  envoyés à un faux serveur.
- `tests/billing_postgres.rs` vérifie l’intention Customer et son watchdog avant réseau, leur suppression après
  rattachement, le replay, les conflits, la ressource absente et l’interdiction absolue d’un nouveau POST après la
  fenêtre sûre de 23 heures.

Commandes ciblées :

```powershell
cargo test --test billing_contract --test billing_stripe
cargo test --features postgres-integration --test billing_postgres -- --test-threads=1
```

Le second test exige le PostgreSQL local `histae-dev` initialisé par `compose.dev.yaml`. L’exécution séquentielle
évite de multiplier les pools au-delà du budget de la pile locale.

## Points de parité vérifiés

- projection `free`/`premium`, statuts Stripe ouvrant l’accès et fin de période stricte ;
- disponibilité du portail uniquement avec Stripe activé et un Customer actif ;
- validation UUID v4 canonique et distinction des conflits d’idempotence ;
- une seule session `creating/open` par utilisateur ;
- essai attribué une seule fois et prix choisi par la période côté serveur ;
- intention et watchdog durables avant le premier POST Customer ;
- aucune nouvelle création après une issue incertaine vieille de 23 heures ;
- mêmes URLs, métadonnées, clés Stripe, statuts HTTP et enveloppes d’erreur ;
- aucune donnée fournisseur ou contenu de réponse Stripe dans les logs.

## Risques et suite

La projection d’abonnement, les événements signés et la réconciliation fournisseur sont désormais fournis par S21.
Les métriques détaillées de dépendance seront reliées au socle d’observabilité dans son lot prévu ; les appels sont
déjà bornés et utilisent des codes de log à cardinalité fixe.

