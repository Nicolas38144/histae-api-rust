# Abonnements et droits Premium

[Index](../README.md) · [Achats Apple/Google](store-billing.md) · [Contrat HTTP](../api/http-contract.md#abonnement)

## Paiements pris en charge

Les achats et renouvellements passent par l’App Store et Google Play. L’API vérifie les
preuves côté serveur via le service privé `store-verifier`. Le [guide des stores](store-billing.md) décrit la configuration,
l’acquittement Google, les webhooks, les restaurations et les essais sur appareil.

`GET /api/plans` expose les fonctionnalités et les prix indicatifs du catalogue Histae. Les prix
réellement proposés à l’achat proviennent du SDK du store et de ses produits/base plans configurés.
Une modification du catalogue SQL ne modifie pas les prix du store.

## Lecture et gestion de l’abonnement

`GET /api/users/me/subscription` retourne les droits effectifs du compte. Les achats natifs,
les attributions internes, les Premium offerts par Histae et l’accès du compte de revue restent
séparés en base. La projection commune sélectionne les droits valables à la date de lecture.
Un webhook retardé ne prolonge pas une échéance expirée. Une attribution gratuite ne crée ni achat
ni revenu et ne remplace pas un historique fournisseur.

Pour un achat natif, `provider` vaut `app_store` ou `google_play` et `management_url` ouvre la gestion
des abonnements chez le fournisseur. Pour Free et les droits offerts, ces champs peuvent être `null`.
Après la suppression d’un compte Histae, la résiliation de l’abonnement se fait depuis le store.

## Revenus administratifs

`GET /api/admin/revenue` et les métriques générales utilisent les lignes de rapports
financiers Apple/Google importées avec la route administrative dédiée. La base de calcul vaut
`store_financial_reports`. Les champs sont `financial_rows`, `gross_revenue_cents`, `currency` et
`excluded_currency_rows`. Aucun revenu n’est déduit du nombre d’abonnés ou du prix catalogue.

Seules les lignes de la devise du plan Premium sont additionnées ; les autres sont comptées
séparément. Les remboursements négatifs diminuent le total. Sans rapport importé, le revenu est zéro.
Les bornes calendaires utilisent Europe/Paris. `premium_subscriptions` compte les abonnements natifs
éligibles à la fin de période ; il ne mesure ni les encaissements ni les comptes Premium offerts.
Les montants bruts ne sont ni un bénéfice ni les versements nets des stores.

## Session du site pour les données personnelles

Le parcours UUID + téléphone + OTP est conservé pour `/api/web/account/*` et `/api/web/privacy/*`.
Il sert uniquement à l’accès aux données personnelles et à l’effacement, sans créer de compte.
`WEB_ACCOUNT_ORIGIN` contrôle l’origine exacte des mutations. Le cookie HTTPS est
`__Host-histae_account` (`histae_account` en HTTP de développement), HttpOnly et SameSite=Strict.
Les quotas OTP, la protection Sweego, l’expiration de session, le contrôle d’origine et la révocation
du compte restent appliqués. Voir [Contrat HTTP](../api/http-contract.md) et [Achats natifs](store-billing.md).
