# Achats Apple, Google Play et site

[Index](README.md) · [Contrat HTTP](http-contract.md#abonnement) · [Stripe](billing.md)

## Répartition

| Canal d’achat | Paiement | Gestion / restauration |
| --- | --- | --- |
| Application Android | Google Play Billing | Google Play, bouton **Restaurer mes achats** |
| Application iOS | StoreKit / App Store | App Store, bouton **Restaurer mes achats** |
| Site Histae | Stripe Checkout | Portail Stripe après connexion OTP sur le site |

Les droits Premium appartiennent au compte Histae, pas à l’appareil. Les offres mensuelle et annuelle donnent
les mêmes fonctionnalités. Le catalogue Histae décrit les fonctionnalités ; **les prix proposés dans l’app
viennent du SDK du store**, dans la devise de l’utilisateur. Les prix Stripe restent ceux du site.
Aucun nouvel essai gratuit n’est proposé. Les transactions historiques Stripe restent compatibles.
Les anciennes routes mobiles Stripe Checkout/portail répondent `410 billing_web_only` après authentification.

## Achat natif et preuve serveur

1. L’app lit l’abonnement effectif et `/api/billing/store-products`, puis demande les prix au SDK natif.
2. Après confirmation du prix et du renouvellement, elle prépare une intention authentifiée. Le serveur
   retourne un identifiant opaque distinct du UUID public : `appAccountToken` Apple, ou `obfuscatedAccountId` Google.
3. Le SDK ouvre la feuille de paiement du store. Aucun secret fournisseur n’est embarqué.
4. L’app transmet l’ID de transaction Apple ou le `purchaseToken` Google au serveur. Celui-ci consulte
   les API officielles par le service privé `store-verifier`, vérifie produit, période, environnement et propriétaire.
5. L’API enregistre les droits et l’acquittement Google dans une transaction durable, puis signale une relecture SSE.
   L’app termine la transaction **après** validation serveur. Le worker acquitte aussi Google sans dépendre du téléphone.
6. Une restauration refait la même vérification. Un achat d’un autre compte n’est jamais réattribué.
   Un token Google remplacé par un nouveau token est définitivement exclu des droits, même s’il est rejoué plus tard.

Le vérificateur utilise les SDK officiels Apple/Google. Apple : JWS, chaîne de certificats et contrôles en ligne,
Bundle ID, App ID, environnement, produit et liaison au compte. Google : Developer API `subscriptionsv2.get`,
produit, base plan, environnement test/production et identifiant de compte masqué. Une absence de preuve
ou une indisponibilité n’ouvre aucun nouveau droit. Les achats Google en attente sans dates restent en attente ;
ils ne sont ni accordés ni acquittés. Le partage familial Apple est volontairement exclu ; le désactiver dans la console.

Les projections Stripe et natives sont indépendantes. Une expiration Stripe ne peut pas écraser un achat Apple
encore valable. La fin de période est vérifiée à chaque lecture. Les états natifs `active` et `grace_period`
avec échéance future donnent Premium ; Google `canceled` le conserve jusqu’à cette échéance.
`pending`, `on_hold`, `paused`, `expired`, `revoked` et le recouvrement Apple sans grâce ne donnent pas accès.
Un abonnement non résolu (y compris pause / incident) bloque un nouvel achat : utiliser sa gestion fournisseur.
Ce contrôle limite les doublons connus ; deux achats lancés simultanément sur des stores indépendants
ne constituent pas une transaction distribuée et doivent être surveillés.

## Connexion et achat sur le site

Page : `/fr/billing/subscribe/` (également `en`, `es`, `it`).

- Saisir l’UUID de son compte existant et son téléphone français `+33…`.
- L’API exige une correspondance des deux informations avec un compte actif et non banni avant tout SMS.
  La réponse publique reste générique, y compris pour des coordonnées incorrectes ou un envoi non confirmé.
- Confirmer l’OTP lié à un challenge opaque valable au plus dix minutes et à la durée propre de l’OTP.
  Maximum cinq essais par challenge, en plus des limites partagées par numéro/IP et des protections Sweego.
- La vérification ouvre une session de trente minutes, sans créer de compte et sans JWT dans le navigateur.
- Choisir l’offre ; le prix final est confirmé par Stripe. Le retour succès n’accorde aucun droit : relire la session.

Le code du site ne peut servir à un login mobile ni à un autre challenge. Cookie HTTPS :
`__Host-histae_billing; Secure; HttpOnly; SameSite=Strict; Path=/`, sans Domain. Seul le développement HTTP
local utilise `histae_billing`. Le token aléatoire est uniquement stocké sous forme de hash en base.
Toutes les mutations exigent l’Origin exact `WEB_BILLING_ORIGIN`. Les réponses sont `no-store` et `no-referrer`.
Un logout global mobile invalide aussi les sessions du site. Ban, effacement et consentements sont revérifiés.
Ne pas placer UUID, téléphone, OTP ou cookie dans une URL.

Le site conserve seulement une clé Checkout et sa période dans sessionStorage afin de reprendre une réponse
perdue. Rejouer la même période avec la même clé. Une clé consommée exige une relecture avant une nouvelle
intention ; ne pas supprimer aveuglément cet état pendant une opération incertaine.

Caddy du reverse proxy route `/api/web/billing/*` du domaine du site vers l’API : le cookie reste sur la même origine.
En développement Next, ces chemins sont réécrits vers `HISTAE_API_DEV_UPSTREAM` (défaut `http://127.0.0.1:8080`).
Le site reste exporté statiquement en production, avec l’API derrière Caddy. Bypasser le cache Cloudflare
pour ces chemins ; conserver les en-têtes de confidentialité.

## Configuration et activation

Les secrets restent dans le `.env` **de l’API** ignoré par Git ; `BILLING_ENABLED` du mobile est un interrupteur public.
Les nouvelles variables sont présentes dans le `.env` local avec fournisseurs natifs désactivés.

| Variables | Rôle |
| --- | --- |
| `APP_STORE_ENABLED` | `false` par défaut ; activer après configuration et essais Sandbox |
| `APP_STORE_APP_ID`, `APP_STORE_BUNDLE_ID` | Identifiants numériques / bundle de l’app Apple |
| `APP_STORE_ISSUER_ID`, `APP_STORE_KEY_ID`, `APP_STORE_PRIVATE_KEY` | Clé App Store Server API ; clé PEM avec `\n` littéraux dans une valeur `.env` entre quotes simples |
| `APP_STORE_PREMIUM_MONTHLY_PRODUCT_ID`, `APP_STORE_PREMIUM_ANNUAL_PRODUCT_ID` | Deux produits récurrents du même groupe d’abonnements |
| `APP_STORE_ENVIRONMENT` | Vide : déduit de `ENV` ; `Sandbox` en dev, `Production` en production uniquement |
| `GOOGLE_PLAY_ENABLED` | `false` par défaut ; activer après préparation et tests Play |
| `GOOGLE_PLAY_PACKAGE_NAME`, `GOOGLE_PLAY_PREMIUM_PRODUCT_ID` | Package Android et produit abonnement Premium |
| `GOOGLE_PLAY_MONTHLY_BASE_PLAN_ID`, `GOOGLE_PLAY_ANNUAL_BASE_PLAN_ID` | Deux base plans auto renouvelables, sans essai ni offre promotionnelle |
| `GOOGLE_PLAY_CLIENT_EMAIL`, `GOOGLE_PLAY_PRIVATE_KEY` | Compte de service autorisé dans Play Console ; **pas la configuration publique Firebase** |
| `GOOGLE_PLAY_PUBSUB_AUDIENCE` | URL exacte du push HTTPS Pub/Sub, par exemple `https://api.histae.com/api/billing/google-play/webhook` |
| `GOOGLE_PLAY_PUBSUB_SERVICE_ACCOUNT` | Email du compte de service émettant le JWT OIDC du push |
| `STORE_VERIFIER_TOKEN` | Secret partagé API/worker/vérificateur, au moins 32 caractères aléatoires |
| `STORE_VERIFIER_URL` | `http://store-verifier:8080` ; réseau privé uniquement, aucun port hôte publié |
| `WEB_BILLING_ENABLED` | `false` par défaut de l’application ; `true` dans le `.env` local préparé |
| `WEB_BILLING_ORIGIN` | Défaut dev `http://localhost:3000`, prod `https://www.histae.com` ; pas de slash final |
| Variables `STRIPE_*` et `BILLING_PROVIDER` | Conservées pour le site ; voir [Stripe](billing.md) |

La prise en charge optionnelle de fichiers de clés dans le vérificateur (`APP_STORE_PRIVATE_KEY_FILE`,
`GOOGLE_PLAY_CREDENTIALS_FILE`) exige un montage explicite en lecture seule dans un override Compose ;
elle n’est pas nécessaire avec les valeurs `.env` prévues. Les clés privées natives sont effacées de
l’environnement des conteneurs Rust et transmises uniquement au vérificateur.

Ordre de livraison :

1. Configurer les produits et accès fournisseur (l’utilisateur s’en charge), sans essais, et les prix/renouvellements.
2. Apple Notifications V2 → `https://api.histae.com/api/billing/apple/webhook`, environnements correspondants.
   Google RTDN → topic Pub/Sub → push OIDC vers `/api/billing/google-play/webhook` avec les valeurs exactes ci-dessus.
3. Renseigner les identifiants et clés, activer les fournisseurs en dev, garder les comptes test dans la pile de dev.
4. Déployer API, worker, maintenance et vérificateur, puis site/proxy/dashboard. Le migrateur ajoute les tables sans
   effacer les données Stripe ; il peut être relancé. Les scripts de reset construisent aussi le nouveau service.
5. Tester les achats Sandbox/Play test, renouvellements, refus, grâce, hold, annulation, remboursement, restauration,
   fermeture de l’app, panne réseau et webhook retardé sur de vrais appareils.
6. Activer `BILLING_ENABLED=true` dans une nouvelle compilation Flutter après ces validations ; ne jamais y copier une clé privée.
7. Production : valeurs live séparées, `ENV=production`, même ordre de déploiement et supervision active.

Pas besoin de lancer un nouveau service public dans le tunnel pour le vérificateur. Les webhooks suivent le
routage existant du domaine API. Ne pas imposer de challenge navigateur Cloudflare sur ces deux webhooks,
mais conserver vérification cryptographique, limitation de débit et journalisation sans corps/token.

## Exploitation

La maintenance planifie au plus 100 relectures natives dues par passe, toutes les six heures par abonnement,
jusqu’à 60 jours après expiration, avec suivi séparé des acquittements non terminés.
Les notifications authentifiées sont dédupliquées puis traitées via l’outbox. Elles déclenchent une **relecture
courante** chez le fournisseur, pas une confiance aveugle dans une notification ancienne.
Une observation commencée avant une observation plus récente ne réécrit pas sa projection.

Prometheus : file `store_billing`, `histae_store_billing_unacknowledged`, `histae_store_billing_stale`.
Alertes : acquittement Google en attente quinze minutes, droits non revérifiés depuis douze heures,
travaux en dead letter. Vérifier le service privé, ses credentials et les accès réseau fournisseur,
puis reprendre l’outbox avec la procédure admin après diagnostic. Ne pas ignorer les acquittements :
Google rembourse automatiquement les nouveaux achats non acquittés dans son délai fournisseur.

## Dashboard et rapports financiers

Onglet **Abonnements stores** : état d’activation, abonnements, comptes actifs, acquittements, dernières
vérifications et cinquante abonnements récents. Tous les admins peuvent le consulter.
Seul le superadmin avec authentification récente peut importer un rapport normalisé après confirmation.

CSV UTF-8, au plus 1 000 lignes et 512 Kio dans le dashboard :

```csv
row_id,paid_at,currency,gross_cents,net_cents
```

Fournisseur et identifiant du rapport sont saisis séparément. `row_id` doit rester stable et unique dans ce rapport.
Dates avec fuseau ; devise ISO en majuscules ; montants entiers signés exprimés en **centièmes de l’unité
monétaire** (montant × 100), remboursements négatifs, `net_cents` vide si inconnu.
Ne pas additionner des rapports qui se recouvrent, ni réimporter le même rapport sous un autre nom.
Les exports bruts Apple/Google ont des formats différents : les normaliser d’abord sans inventer les montants.
Un rejeu identique est accepté ; même fournisseur/rapport/ligne avec valeurs différentes → `409 finance_report_conflict`
et rollback de tout l’import. L’import est audité. Le fichier d’exemple fourni n’invente aucun revenu.

Les devises ne sont jamais additionnées ni converties automatiquement. Net inconnu reste inconnu.
L’onglet général conserve le calcul Stripe sur factures payées, clairement étiqueté ; il n’est pas gonflé
par les comptes natifs. Les chiffres natifs proviennent uniquement des rapports effectivement importés.

<a id="donnees-personnelles"></a>

## Données personnelles

Références natives chiffrées en AES-GCM avec séparation de contexte ; token Google indexé par SHA-256,
liaison compte opaque. Aucune carte bancaire, aucun reçu complet dans les logs, Postman ou le dashboard.
L’export RGPD inclut les projections natives, sans token chiffré ni secret. À l’anonymisation, sessions web
supprimées, références chiffrées vidées et liens au compte supprimés ; une restauration ne rattache pas l’achat.
La suppression du compte Histae **ne résilie pas automatiquement l’abonnement du store** : le gérer auprès
d’Apple/Google avant effacement. Les preuves minimales anti rejeu sont purgées 400 jours après la fin de période ;
liaisons orphelines après 400 jours sans abonnement. Notifications traitées : sept jours ; sinon quatre-vingt-dix.
Challenges et sessions expirés sont purgés par la maintenance. Les rapports financiers sont séparés des comptes
utilisateurs ; définir leur durée de conservation comptable avec la validation juridique du projet.

## Vérifications et limites

Tests locaux : normalisation et refus fournisseur simulés ; bridge privé HTTP ; migrations PostgreSQL fraîches
et répétées ; ownership, concurrence, acquittement durable ; parcours OTP et cookie ; Flutter avec achats simulés ;
site avec API simulée ; dashboard et rapports. Voir [tests](../tests/README.md) pour les résultats à jour.
Aucun SMS réel ni paiement réel n’est nécessaire à ces tests. Ils ne remplacent pas les essais Sandbox,
Play test et TestFlight après configuration des consoles et des appareils ; aucune validation fournisseur
ni publication sur les stores n’est revendiquée.

Références : [Google, vérification sécurisée](https://developer.android.com/google/play/billing/security),
[cycle de vie](https://developer.android.com/google/play/billing/lifecycle/subscriptions),
[Apple, bibliothèque serveur officielle](https://github.com/apple/app-store-server-library-node),
[Flutter in_app_purchase](https://pub.dev/packages/in_app_purchase).

## Organisation du code

- API : `billing/web/domain.rs` porte les projections et erreurs ; `service.rs` orchestre le couple
  UUID/téléphone, l’OTP lié au challenge et l’ouverture de session ; `store.rs` expose le contrat de
  persistance ; `pg.rs` possède les transactions. Les DTO, cookies/Origin, quotas HTTP et erreurs
  publiques restent dans `http.rs` et `http/`. L’assemblage des dépendances concrètes reste dans `app/`.
- Les projections administratives sont des structures Rust typées ; la persistance ne construit pas de JSON HTTP.
  Les validations d’import financier sont dans `billing/stores/administration/domain.rs` et les
  écritures/rôles/audits dans son adaptateur PostgreSQL. Le transport ne possède pas ces règles.
- Le vérificateur privé compose deux adaptateurs distincts, `providers/apple.mjs` et `google-play.mjs`.
  Chaque adaptateur possède son SDK, ses vérifications et la normalisation de ses données ; le serveur
  HTTP ne connaît ni la persistance ni les droits Premium.
- Flutter : `BillingController` manipule les valeurs immuables de `BillingGateway`. Seul
  `NativeBillingGateway` connaît les objets et paramètres des SDK. Les widgets affichent l’état et
  émettent les intentions ; le dépôt HTTP valide et traduit les réponses serveur.
- Site : la vue reçoit les textes traduits, le hook pilote les états/actions, et `lib/billing/`
  sépare modèles, protocole HTTP, validation des réponses/URLs et stockage de la clé de reprise.
- Dashboard : la page assemble la lecture et les panneaux ; le hook d’import possède le cycle du
  fichier et de la confirmation. CSV, totaux par devise, DTO HTTP et composants ont des modules séparés.

Dans l’app, l’UUID se trouve dans la section **Mon compte** du profil, avec sélection du texte et
bouton de copie. Le même widget est utilisé dans l’abonnement ; il n’est pas ajouté à l’aperçu public.
Les libellés et confirmations de copie existent dans les quatre langues.
