# S21 — webhooks et réconciliation Stripe

## Analyse de l’existant

Le webhook NestJS ne se contente pas d’accuser réception. Il authentifie exactement le corps brut, refuse un
événement test reçu avec une clé live ou l’inverse, ignore les types inconnus valides et déduplique l’identifiant
Stripe. Pour un événement pris en charge, le reçu, la projection PostgreSQL et l’éventuelle notification sont
committés ensemble. Une facture provoque d’abord une lecture Stripe de son abonnement hors transaction.

Les webhooks constituent le chemin rapide mais ne garantissent ni livraison ni ordre. Deux événements d’outbox à
payload vide complètent donc le système. `billing.subscription.reconcile` relit le Customer et toutes ses
souscriptions ; `billing.customer.reconcile` résout une création Customer dont l’issue réseau était incertaine. Une
tentative sans Customer connu attend 23 heures avant toute recherche et ne déclenche jamais une nouvelle création.

## Mapping NestJS vers Rust

| NestJS | Rust | Responsabilité |
| --- | --- | --- |
| `StripeWebhookController` | `billing::http::stripe_webhook_routes` | Corps brut, signature, IP et quota dédié |
| `StripeWebhookService` | `billing::webhook::StripeWebhookService` | Vérification, mapping, prélecture facture et livraison après commit |
| `StripeProjectionMapper` | fonctions fermées de `billing/webhook.rs` | Validation stricte des objets Subscription et Invoice |
| méthodes webhook de `BillingRepository` | `PgStripeWebhookStore` | Reçu, mapping, projections et notifications atomiques |
| `BillingReconciliationService` | `BillingReconciliationService` et handler outbox | Lecture fournisseur, sélection déterministe et erreurs permanentes/transitoires |
| `BillingReconciliationRepository` | `PgBillingReconciliationStore` | Ordonnancement, contrôle de version, récupération Customer et liste admin |
| timer NestJS | `BillingReconciliationScheduler` | Programmation bornée suivie par `MaintenanceTracker` |
| `MobileDeliveryService` | `BillingRealtimePublisher` | Effet temps réel best-effort après commit ; adaptateur Redis/SSE livré par S22 |

## Contrat HTTP

| Méthode | Route | Auth | Entrée | Réponse |
| --- | --- | --- | --- | --- |
| POST | `/api/billing/stripe/webhook` | Signature Stripe | Corps JSON brut et `Stripe-Signature` | `200 { "received": true }` |
| GET | `/api/admin/billing-reconciliation` | Session WebAuthn admin | `kind=all|subscription|customer_creation`, `limit=1..100`, curseur | `200 { events, next_cursor }` |

Le webhook applique le quota `billing-webhook` par IP et reste exclu uniquement du quota HTTP global. Les erreurs
publiques conservées sont `invalid_stripe_signature`, `invalid_stripe_event`, `stripe_mode_mismatch`,
`billing_webhook_rate_limit_exceeded`, `billing_unavailable`, `rate_limit_unavailable` et
`stripe_request_failed`. La collection admin n’expose ni payload, ni Customer ID, ni Subscription ID.

## Invariants transactionnels

- le reçu Stripe est inséré dans la même transaction que la projection et la notification ;
- un compte effacé committe seulement le reçu et n’est jamais recréé depuis les métadonnées Stripe ;
- une projection webhook est ordonnée par `provider_event_created_at`, avec priorité aux états terminaux à
  timestamp égal ;
- un snapshot de réconciliation vérifie `projection_version` et `provider_snapshot_at` sous verrou avant écriture ;
- plusieurs souscriptions Premium courantes ou une recherche Customer ambiguë deviennent des erreurs permanentes ;
- `customer.deleted` annule la projection locale, ferme les Checkout vivants et désactive le mapping ;
- les notifications de paiement et de fin d’essai réutilisent le prédicat partagé de S14 à la programmation.

## Tests

- `tests/billing_webhook_contract.rs` couvre les octets signés, la réponse HTTP, la signature invalide et le quota ;
- les tests unitaires de `billing::webhook` couvrent la tolérance temporelle, l’altération du corps et le mapping
  strict Product/Price ;
- les tests unitaires de `billing::reconcile` couvrent les curseurs à précision microseconde et la résolution
  exacte d’une tentative Customer ;
- `tests/billing_reconciliation_postgres.rs` prouve qu’un snapshot/version ancien ne peut écraser une projection
  récente, que la liste admin reste minimale et que le watchdog sans résultat est nettoyé sans nouveau POST.

```powershell
cargo test --test billing_webhook_contract
cargo test --features postgres-integration --test billing_reconciliation_postgres -- --test-threads=1
```

Le second test refuse une base autre que `histae-dev` sur loopback. S21 ne requiert aucune nouvelle variable,
migration ou image Docker : la configuration Stripe, les tables et les services locaux existaient déjà dans le
dépôt Rust.

## Risques restant ouverts

L’adaptateur concret Redis/SSE de `BillingRealtimePublisher` est livré par S22 ; l’échec du publisher reste
best-effort après le commit. L’assemblage des binaires `api`, `outbox` et `maintenance` sera activé lorsque tous
leurs handlers requis seront présents, conformément à la séquence de migration existante.
