# Parcours des abonnements Stripe

[Index](README.md) · [Configuration](configuration.md#tarifs-du-catalogue-et-stripe) · [Contrat HTTP](http-contract.md#abonnement)

## Configuration et Product ID

`STRIPE_PREMIUM_PRODUCT_ID` est inutile et n’est plus lu par l’API. Chaque Price Stripe référence déjà
son produit. L’API choisit et reconnaît les nouveaux abonnements à partir de deux prix récurrents :

- `STRIPE_PREMIUM_MONTHLY_PRICE_ID` : prix mensuel ;
- `STRIPE_PREMIUM_ANNUAL_PRICE_ID` : prix annuel.

Ils doivent être distincts, appartenir au mode test/live de la clé API et correspondre aux offres du
catalogue : par défaut 5 EUR/mois, 48 EUR/an et 30 jours d’essai. L’essai est transmis par l’API ; modifier
le catalogue n’altère pas le montant d’un Price Stripe existant. Le portail Stripe doit être configuré
pour les offres et les changements effectivement souhaités.

L’activation exige `BILLING_PROVIDER=stripe`, `STRIPE_SECRET_KEY`, `STRIPE_WEBHOOK_SECRET`, ces deux prix
et trois URL HTTPS : succès avec `{CHECKOUT_SESSION_ID}`, annulation et retour du portail. Publier de
vraies pages de retour ou des liens applicatifs vérifiés. La redirection de succès ne constitue pas
une preuve d’achat. Une configuration `BILLING_PROVIDER=disabled` refuse Checkout/portail avec
`503 billing_unavailable`, tout en laissant lire le catalogue et l’état local.

## 1. Choix de l’offre et création de Checkout

Le client lit `GET /api/plans`, puis envoie avec son Bearer et une clé UUID v4 :

```http
POST /api/users/me/subscription/checkout
Idempotency-Key: <uuid-v4-de-cette-tentative>
Content-Type: application/json

{"billing_period":"annual"}
```

Le compte doit être authentifié, actif et avoir terminé l’onboarding requis. Le client ne transmet
aucun prix, produit, montant, Customer ID ni nombre de jours d’essai. Les quotas dédiés restent à
10 requêtes/minute/utilisateur pour Checkout et le portail.

L’API prend un verrou de compte qui protège l’effacement et un verrou propre à Checkout. Ce second
verrou sérialise les créations, y compris si une requête Stripe est lente, tout en laissant les autres
activités partagées du compte possibles. Un compteur de génération clôt aussi les droits d’écriture
et de nettoyage d’un ancien propriétaire : une reprise ne peut pas être annulée par sa réponse tardive. PostgreSQL vérifie ensuite la clé, les sessions ouvertes et
les abonnements connus. Pour un Customer déjà lié, l’API vérifie aussi chez Stripe qu’aucun abonnement
non terminé n’existe : un webhook absent ne doit jamais permettre un second abonnement facturé.

`incomplete`, `trialing`, `active`, `past_due`, `unpaid` et `paused` bloquent un nouvel achat ; `canceled`
et `incomplete_expired` permettent une nouvelle tentative si aucune session Checkout ne reste active.
Une session ouverte peut être reprise avec sa clé d’origine. Pour un abonnement existant, privilégier
le portail, même si le compte dispose actuellement des droits Free. Le code historique
`subscription_already_active` décrit également ces états Stripe non terminés.

## 2. Client Stripe et requête durable

Si nécessaire, l’API crée un Customer avec une intention durable et la clé Stripe
`histae-customer-<tentative>`, puis le lie au compte. Les métadonnées contiennent les UUID du compte et
de la tentative ; aucun téléphone ni message privé n’est transmis.

Avant le POST Checkout, l’API persiste dans `billing_checkout_session.stripe_checkout_request` les
paramètres exacts : Customer, Price, période, essai, URLs, taxes, promotions et expiration. Le premier
envoi et les reprises utilisent la même clé `histae-checkout-<tentative>` et les mêmes paramètres,
même si le catalogue ou la configuration ont changé. Cette trace ne contient ni secret Stripe ni
donnée de carte bancaire.

L’expiration est calculée **après** la création/recherche du Customer et fixée à **35 minutes**, afin de
conserver une marge sur le minimum Stripe de 30 minutes lors des reprises réseau bornées. L’échéance
renvoyée par Stripe et publiée dans `expires_at` reste celle à respecter. Les paramètres ne sont pas
recalculés pour prolonger une tentative incertaine.

Checkout est créé en mode `subscription`, avec un seul Price et une quantité de 1. L’essai du catalogue
est ajouté uniquement si `billing_customer.trial_used_at` est vide. Si les taxes automatiques sont
activées, l’adresse collectée est enregistrée chez Stripe pour permettre le calcul fiscal.

L’API persiste la session puis renvoie `201 { session_id, url, expires_at }`. Le client ouvre `url`.
Les informations bancaires, authentifications de paiement et prélèvements sont traités chez Stripe.

## 3. Activation Premium

Stripe appelle `POST /api/billing/stripe/webhook`. Cette route est publique mais exige la signature
HMAC du corps brut ; le timestamp doit être dans la fenêtre de cinq minutes. Le mode test/live doit
correspondre à la clé. Chaque Event ID est traité au plus une fois en transaction.

- `checkout.session.completed` clôture la tentative ; il n’accorde pas seul Premium.
- Les événements `customer.subscription.*` projettent l’abonnement et sa période en PostgreSQL.
- `invoice.paid` enregistre la facture et relit l’abonnement chez Stripe ; le statut de l’abonnement
  reste déterminant pour les droits.
- Le premier essai reconnu inscrit `trial_used_at` ; annuler puis racheter ne réattribue pas l’essai.

Le Customer, l’abonnement, le prix et les métadonnées sont contrôlés pour rattacher l’état au bon
compte. Les anciens formats de périodes et de références de facture restent compatibles. Après
rotation des prix, seul l’ancien prix de l’abonnement déjà connu pour ce Customer reste accepté ;
un prix inconnu ne permet pas de créer des droits Premium.

Une projection plus ancienne qu’un webhook ou une réconciliation déjà appliqués est ignorée et
acquittée sans publier de statut SSE obsolète. Une facture déjà payée ne déclenche pas une notification
de paiement échoué à partir d’un événement dépassé.

Premium est accordé pour `trialing`, `active` ou `past_due`, avec une fin de période connue et future.
`past_due` conserve donc les droits pendant cette période selon la politique actuelle ; il ne prouve
pas un encaissement. Une période absente/expirée ou un autre statut Stripe donne les droits Free.
Les fiches/listes administratives et les compteurs de plans appliquent les mêmes conditions.

L’API publie `subscription.updated` via SSE lorsque la projection est appliquée. Le client doit relire
`GET /api/users/me/subscription`, notamment après le retour Stripe, pour confirmer `access_granted`.

## 4. Renouvellement, annulation et incident

Stripe renouvelle et prélève l’abonnement selon son Price ; l’API ne lance pas de tâche de prélèvement.
Les événements de facture et d’abonnement mettent à jour les droits. En cas d’échec ou d’action bancaire
requise, une notification durable peut demander à l’utilisateur de corriger le paiement.

`POST /api/users/me/subscription/portal` renvoie une URL du portail pour le Customer lié : les actions
disponibles dépendent de sa configuration Stripe. Une annulation en fin de période conserve Premium
jusqu’à l’échéance tant que le statut y ouvre droit. Une annulation effective le retire.

Les maintenances programment les réconciliations dans l’outbox. Le worker relit Stripe et répare les
états locaux sans déclencher de nouveau paiement. Une ambiguïté ou plusieurs abonnements non terminés
restent des erreurs soumises à revue opérateur. Voir [Exploitation](operations.md).

Après une création Customer incertaine, la clé originale peut être rejouée uniquement pendant la
fenêtre sûre de 23 heures ; ensuite la récupération est en lecture seule. Après une création Checkout
incertaine, seule la tentative originale peut être reprise jusqu’à son échéance. Une nouvelle clé
est bloquée pendant cette fenêtre et la clé expirée n’est pas recyclée.

## 5. Revenus du dashboard

`GET /api/admin/revenue` et le champ `revenue` de `/api/admin/metrics` partagent le calcul
`stripe_paid_invoices`. Le total additionne les montants payés des factures d’abonnement enregistrées,
selon leur `paid_at`, dans la devise du catalogue Premium. Les périodes calendaires suivent Paris ;
le début est inclus et la fin exclue. Un annuel payé 48 EUR ajoute 48 EUR à sa période de paiement,
pas 5 EUR ni 4 EUR de MRR. Les remises et renouvellements sont pris pour leurs montants payés.

- `paid_invoices` : factures payées de montant positif retenues ;
- `premium_subscriptions` : abonnements distincts associés à ces factures ;
- `estimated_revenue_cents` et `currency` : montant brut dans la devise indiquée ;
- `excluded_currency_invoices` : factures d’autres devises exclues, sans conversion.

Les essais à zéro, impayés, dates de paiement absentes et factures sans abonnement n’entrent pas
au total. Un Premium accordé manuellement ne crée pas de revenu. Une annulation ou un effacement
ultérieurs n’effacent pas les factures historiques anonymisées. Les réconciliations ne déplacent
pas leur période de paiement.

Le dashboard affiche les montants, les compteurs et l’avertissement de devise. Les taxes facturées
sont incluses, mais les commissions et remboursements ne sont pas déduits. Des webhooks manquants
peuvent laisser des factures absentes : ce total brut est une estimation opérationnelle, ni un MRR,
ni un bénéfice, ni le solde des versements Stripe. Il ne remplace pas le suivi comptable.

## Mise à jour d’une installation existante

Le migrateur ajoute les colonnes de requête durable et de génération sans supprimer les données. Pour les anciennes
tentatives en cours/échouées susceptibles d’avoir déjà envoyé un Checkout, dont les paramètres
originaux n’étaient pas stockés, il interdit de reconstruire un POST sous l’ancienne clé. Une nouvelle
tentative attend leur échéance. Les sessions déjà ouvertes restent rejouables et les intentions
Customer encore non résolues conservent leur récupération dédiée.

L’index des factures payées est créé sans bloquer les écritures sur une base existante.
Reconstruire les images de l’API et du migrateur ensemble, déployer le dashboard compatible, puis démarrer avec la sélection Compose
du déploiement. Aucun ALTER TABLE manuel n’est nécessaire.

## Preuves et limites de l’audit du 15 octobre 2026

Les tests ciblés couvrent signature/mode, paramètres figés, expiration, création incertaine, reprise et
verrouillage concurrent, abonnement distant sans webhook, plans effectifs, prix historiques, formats
historiques et notifications/projections dépassées. Les migrations sont testées sur une base neuve,
une ancienne base et une migration répétée avec le rôle migrateur restreint. Les régressions des droits
et du cycle de vie des matchs sont également exécutées dans un PostgreSQL jetable.

Résultats locaux :

| Vérification | Résultat |
| --- | --- |
| `cargo fmt`, `cargo check --all-targets --all-features`, Clippy avec `-D warnings` | Réussis |
| Bibliothèque Rust | 204 tests réussis |
| Contrats facturation/HTTP, transport Stripe et inventaire des routes | 25 tests réussis |
| PostgreSQL jetable avec rôle applicatif : facturation 9, réconciliation 2, revenus 2, matchs 16, verrous 1 | 30 tests réussis |
| Migrateur restreint : base neuve, anciennes colonnes/index absents, migrations répétées | Réussis |
| Dashboard : TypeScript, ESLint et build Vite | Réussis |
| Dashboard : tests unitaires et composants, dont les 3 tests des revenus | 56 tests réussis |

Les tests de revenus couvrent montant annuel, renouvellement remisé, essais à zéro, impayés,
date absente, autre devise, facture hors période, facture anonymisée et annulation ultérieure.
Ils valident le même calcul dans la route dédiée et la synthèse métier.

Les fournisseurs sont simulés : aucun paiement, Customer ni abonnement réel n’est créé par ces tests.
Les montants des Prices, le portail, les pages de retour et la livraison réelle des webhooks doivent
être validés en mode test Stripe avant activation en production. Les revenus affichés restent limités aux factures payées enregistrées ; les tests ne prouvent pas
la livraison de vrais webhooks Stripe.

Références : [Checkout et expiration](https://docs.stripe.com/api/checkout/sessions/create),
[idempotence](https://docs.stripe.com/api/idempotent_requests),
[abonnements et webhooks](https://docs.stripe.com/billing/subscriptions/webhooks),
[taxes avec Checkout](https://docs.stripe.com/tax/checkout/page),
[montants des factures](https://docs.stripe.com/api/invoices/object).
