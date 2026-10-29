# Histae API — contrat HTTP

[Index](README.md) · [Postman](../postman/README.md) · [Configuration](configuration.md)

Ce guide est la référence manuelle du contrat destiné au mobile, au dashboard et aux intégrations. Il ne décrit
pas les mécanismes SQL ou les workers internes. Aucun document OpenAPI ni Swagger n’est exposé. L’inventaire
méthode/chemin est comparé automatiquement aux enregistrements Axum ; les corps et autorisations restent couverts
par leurs tests de contrat dédiés. L’inventaire courant contient **140 couples méthode/chemin**.

## Sommaire

- [Conventions communes](#conventions-communes)
- [Routes publiques et intégrations](#routes-publiques-et-integrations)
  - [OTP et catalogue](#connexion-mobile-et-catalogue)
  - [Webhooks Stripe](#webhook-stripe) et [Sweego](#webhook-sweego)
- [Client mobile](#client-mobile)
  - [Sessions](#session-et-deconnexion), [consentements](#consentements), [profil](#profil-et-preferences)
  - [Photo](#photo-unique), [traits et questions](#traits-et-questions-de-profil)
  - [Découverte, matchs et messages](#decouverte-matchs-et-messages)
  - [Temps réel](#appareils-et-temps-reel), [abonnement](#abonnement), [droits et effacement](#droits-effacement-et-surete)
- [Administration](#administration)
  - [WebAuthn](#entree-webauthn), [sessions et passkeys](#sessions-et-passkeys)
  - [Comptes et droits](#comptes-signalements-et-droits), [catalogue et modération](#catalogue-et-moderation)
  - [Exploitation et reprises](#exploitation-et-reprises)
- [Entretien du contrat](#entretien-du-contrat)

<a id="conventions-communes"></a>

## Conventions communes

### Chemins, entrées et réponses

Les tableaux donnent les **chemins complets**, préfixe `/api` inclus. Seules les routes `/health/live` et `/health/ready` sont hors de ce préfixe. `:id`, `:userId`, etc. représentent des paramètres à remplacer, généralement des UUID.

Les corps sont JSON, sauf l’upload photo multipart. Les champs inconnus sont refusés. Dans les tableaux, `?` signifie « facultatif », `A | B` indique les valeurs admises et une liste de noms représente les champs d’un objet, pas un exemple JSON littéral. Une réponse `204` n’a pas de corps.

- Corps JSON : **1 Mio** maximum ; les limites métier peuvent être plus petites.
- Photos : **500 000 octets** maximum en entrée et après conversion WebP.
- Dates de naissance : date calendrier stricte `YYYY-MM-DD`, jamais un timestamp.
- Horodatages : ISO 8601 avec fuseau ; traiter les curseurs comme des chaînes opaques.
- Prix du catalogue : centimes, avec devise explicite. Distances : kilomètres.
- En-têtes communs : `Cache-Control: no-store`, protections navigateur et `X-Request-ID`. Un UUID v4 fourni dans `X-Request-ID` est conservé ; sinon un identifiant est généré. HSTS est ajouté en production.

### Langues

`Accept-Language` accepte `fr`, `en`, `es`, `it`, les variantes régionales et les priorités `q`.
Il prime pour la requête sur la langue du compte ; sans en-tête, le catalogue public utilise le français.
Les réponses localisées exposent `Content-Language` et `Vary: Accept-Language`.
Les traductions manquantes se replient sur le français, sans changer UUID, codes, ordre ou curseurs.
Voir [internationalisation](internationalization.md) pour les règles complètes, les champs et les migrations.

### Authentification et autorisations

| Public | Justificatif | Règle |
| --- | --- | --- |
| Public | Aucun JWT/cookie | Ne dispense pas d’un OTP, d’une signature de webhook ou d’un secret d’enrôlement lorsqu’ils sont requis. |
| Mobile | `Authorization: Bearer <access_token>` | Compte actif, non banni ; CGU et notice courantes requises pour un rôle utilisateur. |
| Admin | Cookie de session WebAuthn `HttpOnly; SameSite=Strict` | Rôle `admin` ou `superadmin`. Aucun JWT mobile accepté, même avec un rôle administratif. |

Les routes mobiles marquées **onboarding incomplet accepté** restent accessibles avant acceptation des textes. Le contrôle global renvoie sinon `403 onboarding_incomplete`. Le sexe et les préférences exigent aussi le consentement sensible, la présence celui de localisation : `403 required_consent_missing`. Un retrait concurrent empêche une requête préalablement validée de réintroduire les données effacées.

Toute mutation authentifiée par session admin exige l’en-tête `Origin` égal à `ADMIN_WEBAUTHN_ORIGIN`. Pour les routes publiques de connexion/enrôlement, l’origine est vérifiée dans la preuve WebAuthn lors de sa validation, pas par ce guard de session. Les actions marquées **récentes** exigent une authentification WebAuthn récente (moins de dix minutes par défaut). Les consultations sensibles sont auditées ; le motif est obligatoire pour un admin et facultatif pour le superadmin. Sans motif fourni, l’API inscrit un libellé automatique dans l’audit.

Lorsque `ADMIN_WEBAUTHN_ORIGIN` est en HTTPS, le cookie s'appelle `__Host-histae_admin_session` et contient `Secure; HttpOnly; SameSite=Strict; Path=/`, sans `Domain`, dans tous les environnements. Seul le développement HTTP sur `localhost` conserve le cookie non sécurisé. Le durcissement invalide les sessions HTTPS antérieures : se reconnecter avec la passkey existante, sans nouvel enrôlement.

<a id="erreurs-et-limites-de-debit"></a>

### Erreurs et limites de débit

```json
{
  "error": {
    "code": "required_consent_missing",
    "message": "The required consent has not been granted."
  }
}
```

Le client doit interpréter `error.code`, pas le texte de `message`. Aucune stack n’est exposée.

| Statut | Interprétation habituelle |
| --- | --- |
| 400 | Corps, paramètre, curseur ou clé d’idempotence invalide. |
| 401 | Authentification absente, expirée ou invalide. |
| 403 | Autorisation, onboarding, consentement ou quota insuffisant. |
| 404 | Ressource absente ou non accessible au demandeur. |
| 409 | Conflit d’état, de version ou d’idempotence. |
| 410 | Match expiré. |
| 413 | Taille du fichier ou du corps dépassée. |
| 429 | Limite de débit dépassée ; respecter `Retry-After` lorsqu’il est fourni. |
| 503 | Dépendance ou protection temporairement indisponible. |

Une limite globale par IP s’ajoute aux limites dédiées indiquées ci-dessous ; leurs valeurs sont les valeurs par défaut configurables. Dépassement global : `rate_limit_exceeded` ; protection Redis indisponible : `rate_limit_unavailable`, sans laisser passer la requête. Les webhooks Stripe, Sweego et AdMob ont chacun leur propre limite à la place de la limite globale.

### Pagination

Sauf indication contraire, `limit` vaut 20, entre 1 et 100. Les collections paginées exposent `next_cursor` : le réutiliser avec les mêmes filtres, jusqu’à `null`. Ne pas le décoder ni le reconstruire avec les dates ou distances arrondies affichées. Le curseur du feed est chiffré, lié à l'utilisateur et expire après 15 minutes ; en cas de `invalid_cursor`, recommencer sans curseur. Les anciens curseurs du feed sont invalidés par le déploiement du format chiffré et par la rotation de la clé JWT active.

`offset` est déprécié ; seules les routes qui le mentionnent l’acceptent. Avec un curseur, il doit être nul. Les curseurs évitent les décalages d’offset, mais **ne figent pas un instantané** : un nouveau message, un changement de statut ou de position peut modifier l’ordre ou l’éligibilité pendant le parcours. Dédupliquer les identifiants côté client et rafraîchir la liste si nécessaire.

Les listes non paginées ne doivent pas recevoir arbitrairement `limit` ou `cursor`.

<a id="idempotence-et-reponses-perdues"></a>

### Idempotence et réponses perdues

Il n’existe pas de mécanisme de retry universel.

| Opération | Comportement attendu du client |
| --- | --- |
| Envoi OTP, photo, message, Checkout | Fournir `Idempotency-Key: <UUID v4>` et conserver la même clé et le même contenu pour la même intention. Les fenêtres et conflits sont propres au parcours. |
| Refresh mobile | Un seul appel à la fois. **Pas de retry aveugle** après perte de réponse ; voir le parcours ci-dessous. |
| Consentements, attribution/retrait de trait | Rejouer un état identique ne doit pas dupliquer l’effet. |
| Swipe | Rejouer le même choix conservé est accepté si la cible reste éligible. Un like retrouve le même match encore ouvert ; un match terminé ou expiré n’est pas rouvert. |
| Effacement du compte | Le jeton est à usage unique. Un `202` indique une acceptation durable, pas la fin des suppressions. |
| Revue admin | Relire après conflit de version ou d’état ; ne pas écraser automatiquement la décision d’un autre opérateur. |

<a id="routes-publiques-et-integrations"></a>

## Routes publiques et intégrations

### Connexion mobile et catalogue

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| POST | `/api/auth/otp/send` | `{ phone_number, registration?: boolean }` + clé d’idempotence → `202 { message, resend_after_seconds }`. `registration` vaut `false` par défaut. |
| POST | `/api/auth/otp/verify` | `{ phone_number, otp }` → `200 { access_token, refresh_token }`. |
| POST | `/api/auth/refresh` | `{ refresh_token }` → `200 { access_token, refresh_token }`. |
| GET | `/api/plans` | `200 { plans: [...], offers: [...] }` ; trois offres commerciales et leurs plans de droits, avec quotas et fonctionnalités. |
| GET | `/api/legal/documents` | `200 { documents: [{ consent_type, required_document_version, document_url, document_language, document_date }] }` ; quatre documents courants, publics et localisés selon `Accept-Language`, sans compte ni preuve de consentement. `Cache-Control: no-store`. |

`document_date` est la date publique de la version courante requise (`required_document_version`),
au format `YYYY-MM-DD` ou `null` si elle n’est pas configurée. La même métadonnée est exposée dans
les consentements authentifiés, y compris lorsqu’une version plus ancienne a été acceptée.
Elle est indépendante de `updated_at`, qui date l’accord/retrait de l’utilisateur ; le catalogue
public n’expose aucune preuve de consentement ni date utilisateur.

**Inscription avant téléphone.** Le mobile prépare le profil et les préférences en mémoire, puis présente les documents de cette route. Aucun profil ni consentement n’est enregistré avant la vérification OTP. Après authentification, il compare versions, URL et langues aux documents propriétaires avant de sauvegarder les choix explicites, puis le profil et les préférences autorisées. Un changement de document impose une nouvelle présentation et un nouveau choix. La route publique ne crée aucun compte et ne modifie aucun consentement.

**OTP.** Seuls les téléphones français E.164 `+33…` sont acceptés. La vérification consomme le code une seule fois et crée un compte utilisateur si nécessaire après une demande d’inscription. Envoi et vérification : 5/h par IP et numéro pseudonymisé. La validité du code suit `OTP_TTL`.

Exception mobile explicite : le [compte de revue Google Play](google-play-review.md), désactivé par défaut, utilise un code réutilisable pour un seul numéro fictif configuré. Son `/send` conserve le délai de renvoi et enregistre une intention sans téléphone chiffré ni SMS/outbox. La première création exige `registration: true` dans la dernière intention encore valide ; une simple connexion ne crée aucun compte. Les quotas et validations restent actifs ; cette exception n’authentifie ni un compte réel existant ni un administrateur.

Pour une inscription, le client envoie `registration: true`. Les deux parcours répondent de manière générique : un numéro déjà enregistré en inscription, ou inconnu en connexion, ouvre le même écran de code, mais ne reçoit aucun SMS. Aucun renseignement sur l’existence du compte ni sur la livraison n’est exposé par cette route.

L’envoi répond `202 { "message": "Verification code request accepted.", "resend_after_seconds": 60 }`. Il confirme uniquement l’écriture durable de la demande et de son événement outbox, dans la même transaction. La route HTTP ne recherche aucun compte et ne contacte pas Sweego. Le worker outbox vérifie l’éligibilité puis traite le SMS éventuel avec l’UUID initial. Une demande déjà remplacée ou expirée est acquittée sans envoi. Le téléphone chiffré est effacé après traitement ; les métadonnées expirent selon `OTP_TTL`, avec nettoyage borné et maintenance RGPD.

Un nouvel UUID pour le même téléphone avant `OTP_RESEND_COOLDOWN` reçoit `429 otp_resend_too_soon` avec `Retry-After`. Ce délai est partagé entre instances et parcours. Un rejeu du même UUID conserve le délai initial sans nouvel envoi et renvoie le nombre de secondes restant. Un autre téléphone, parcours ou langage avec le même UUID renvoie `409 idempotency_key_conflict`. Les quotas IP/téléphone s’appliquent également aux demandes supprimées et aux rejeux. Le mobile distingue cette attente de renvoi des limites de vérification : saisir le code reçu reste possible pendant le compte à rebours.

Le worker conserve les états de livraison et ne répète jamais un appel fournisseur incertain avec la même intention. Un callback signé peut confirmer le code initial, jamais réactiver un code consommé, expiré ou remplacé. Une réponse HTTP perdue se reprend avec le même UUID et les mêmes paramètres ; un nouvel envoi exige une action explicite après le délai. Configuration : [variables OTP](configuration.md#groupes-de-variables) ; diagnostic : [Exploitation](operations.md#diagnostic-de-demarrage).

**Refresh.** Envoyer le dernier refresh sous la forme opaque reçue `jti:secret`, puis enregistrer atomiquement la nouvelle paire de tokens. Le rejeu d’un ancien token authentique non expiré révoque toute sa famille et renvoie `401 invalid_or_expired_refresh_token`. Il n’y a pas de fenêtre de grâce : deux refresh concurrents ou une réponse perdue peuvent imposer une nouvelle connexion OTP. Limite : 30/15 min/IP.

Erreurs utiles : `authentication_required`, `invalid_or_expired_access_token`, `invalid_or_expired_refresh_token`, `invalid_idempotency_key`, `idempotency_key_conflict`, `otp_resend_too_soon`, `otp_rate_limit_exceeded`, `refresh_rate_limit_exceeded`. Les durées et clés sont décrites dans [Configuration](configuration.md#groupes-de-variables).

**Plans et offres.** `plans` expose les niveaux de droits `free` et `premium`, avec prix du catalogue
mensuel/annuel, devise, fonctionnalités et quotas (`weekly_included_credits`,
`weekly_continuation_limit`, `advertising_credit_stock_limit`, `ads_removed`, `rewarded_advertising_enabled`).
`offers` expose les trois choix commerciaux, dans cet ordre lorsque les deux plans sont actifs :

| `code` | `display_name` | `plan_code` | `billing_period` | `price_cents` initial |
| --- | --- | --- | --- | --- |
| `free` | Free | `free` | `null` | 0 |
| `premium_monthly` | Premium Mensuel | `premium` | `monthly` | 500 |
| `premium_annual` | Premium Annuel | `premium` | `annual` | 4800 |

Chaque offre expose aussi `currency`. Les prix et la devise sont projetés depuis
le catalogue PostgreSQL actif ; aucun tarif de secours n'est ajouté aux clients. Les fonctionnalités et
quotas viennent du plan référencé par `plan_code`. Une offre n'est pas publiée si son plan est absent
ou inactif. Le contrat exige `plans` et `offers` ; les réponses ne contenant que `plans` sont rejetées
par les clients. Le champ `trial_days` est supprimé des plans et des offres ; aucun essai n'est proposé.

Le client affiche une carte par offre. Checkout reçoit uniquement `billing_period: monthly` ou `annual`,
avec sa clé d'idempotence ; ni `premium_monthly` ni `premium_annual` ne deviennent un niveau de droits.
Les deux abonnements donnent les droits `premium` selon la projection serveur. Le catalogue ne prouve
pas la disponibilité des paiements, et aucun identifiant ou prix Stripe n'est choisi par le client.

### Webhook Stripe

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| POST | `/api/billing/stripe/webhook` | Corps JSON brut + `Stripe-Signature` → `200 { received: true }`. |

La signature est obligatoire, ainsi que la cohérence test/live. Les Event IDs sont dédupliqués ; les événements non pris en charge mais valides sont acquittés sans effet. Limite dédiée : 300/min/IP.

Les nouveaux abonnements Premium sont reconnus par les prix `price_…` configurés dans
`STRIPE_PREMIUM_MONTHLY_PRICE_ID` et `STRIPE_PREMIUM_ANNUAL_PRICE_ID`. Un abonnement traité doit contenir
un seul article portant un prix autorisé ; aucun identifiant produit n'est configuré.

Les périodes historiques au niveau de l’abonnement et les références historiques de facture `subscription` sont aussi acceptées. Les anciens prix restent reconnus uniquement pour l’abonnement et le client déjà liés à ce prix en base, après rotation des prix des nouvelles ventes. Un événement dépassé par un état plus récent est acquitté sans annoncer un changement d’état obsolète.

Événements traités : `checkout.session.completed`, `checkout.session.expired`, `customer.subscription.created|updated|deleted|paused|resumed`, `invoice.paid`, `invoice.payment_failed`, `invoice.payment_action_required`, `invoice.finalization_failed`, `customer.deleted`. Les événements de facture/abonnement plus anciens que l’état connu sont ignorés. Erreurs : `invalid_stripe_signature`, `stripe_mode_mismatch`, `invalid_stripe_event`, `billing_webhook_rate_limit_exceeded`.

### Protection des envois SMS

Les plafonds concernent les **tentatives d’appel fournisseur** : 5/heure et 10/24 h par numéro, et 100/heure et 500/24 h au total par défaut, en fenêtres glissantes. PostgreSQL sérialise les réservations entre instances. Une réponse perdue ou un échec fournisseur ne rembourse pas de tentative. Les quotas HTTP IP/téléphone restent appliqués avant la mise en file.

Pour le mobile, les refus de budget, la suspension et les états Sweego restent internes au worker : la route conserve son `202` générique. Seuls les quotas HTTP et le délai de renvoi produisent un `429` public. Les autres clients du service OTP, notamment le parcours web, conservent leurs propres codes d’erreur. Aucun OTP déjà accepté n’est invalidé par un refus local d’envoi.

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/admin/sms-safety` | Session admin → `200` état global, limites par numéro et globales, compteurs agrégés 5 min/1 h/24 h, pic de trafic et `version`. Aucun numéro ni hash exposé. |
| PATCH | `/api/admin/sms-safety` | **Superadmin, récente** ; `{ suspended: boolean, version: integer >= 0 }` → `204`, modification globale auditée. `409 resource_changed` si la version a changé. |

La reprise conserve les compteurs. `SMS_SENDING_SUSPENDED=true` impose l’arrêt même si la suspension
manuelle est levée. Les autorisations déjà accordées peuvent encore aboutir. Après une réponse de
mutation perdue, relire le GET avant toute nouvelle décision ; ne pas rejouer automatiquement la mutation.
Les métriques et le dashboard ne sont pas nécessaires à l’application des protections.
Voir [configuration et exploitation](sms-safety.md).

### Webhook Sweego

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| POST | `/api/auth/sweego/webhook` | Corps JSON brut + `webhook-id`, `webhook-timestamp`, `webhook-signature` → `200 { received: true }`. |

HMAC-SHA256 obligatoire avec le secret Sweego décodé en base64 ; aucune session mobile/admin. Signature sur
`id.timestamp.corps-brut`, envoi datant de moins de cinq minutes et au plus une minute dans le futur.
Charge signée limitée à 16 Kio. Limite dédiée : 300/min/IP, à la place de la limite globale.
Les DTO des événements pris en charge refusent les champs inconnus ; les métadonnées fournisseur, notamment
le téléphone éventuellement reçu, sont immédiatement écartées après validation et ne sont pas journalisées.

`sms_sent` confirme l’envoi fournisseur, **pas une réception au téléphone démontrée**. `sms_undelivered` marque
l’échec et prévaut sur `sms_sent`, quel que soit l’ordre d’arrivée. Rejeux et callbacks anciens ne réactivent
aucun OTP consommé, expiré ou remplacé. Événements non pris en charge, simulations, autres campagnes/senders
et demandes déjà purgées sont acquittés sans effet. La corrélation utilise `campaign_id` et `swg_uid`.

Erreurs : `401 invalid_sweego_signature`, `400 invalid_sweego_event`, `409 sweego_delivery_conflict`,
`429 sms_webhook_rate_limit_exceeded`, `503 sweego_webhook_unavailable` (configuration absente ou stockage indisponible).
Configuration : [variables OTP](configuration.md#groupes-de-variables).

### Santé

| Méthode | Route | Résultat |
| --- | --- | --- |
| GET | `/health/live` | `200 { status: "ok" }` si le processus répond. |
| GET | `/health/ready` | `200 { status: "ready" }` si les dépendances requises répondent ; sinon `503`. |

La readiness vérifie PostgreSQL, le bucket objet et Redis lorsqu’il est requis. Ces routes n’exigent pas d’authentification.

<a id="client-mobile"></a>

## Client mobile

Toutes les routes de cette section exigent le Bearer mobile.

<a id="session-et-deconnexion"></a>

### Session et déconnexion

Onboarding incomplet accepté pour toutes les routes de ce tableau. La gestion des sessions, logout compris, partage une limite de 30/15 min/utilisateur.

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/auth/me` | `200 { user_id, onboarding_complete }`, sans rôle admin. |
| POST | `/api/auth/logout` | `{ refresh_token, device_id? }` → `204`. |
| GET | `/api/auth/sessions` | `limit, cursor` → `200 { sessions, next_cursor }`. Pas d’offset. |
| DELETE | `/api/auth/sessions/:id` | UUID v4 d’une famille du compte → `204`. |
| POST | `/api/auth/logout-all` | `{ confirm: true }` → `200 { revoked_sessions }`. |

Une session listée expose `id, created_at, last_refreshed_at, expires_at, current`, jamais token, hash, IP ou user-agent. Seules les familles actives sont listées.

Logout révoque la famille du Bearer et ses appareils push ; le refresh doit appartenir à cette famille, un prédécesseur authentique non expiré étant accepté. L’appareil facultatif doit appartenir au compte. La révocation ciblée peut viser la session courante ; elle est idempotente pour une famille déjà révoquée encore référencée. Une famille absente/étrangère renvoie `404 session_not_found`. Logout-all révoque toutes les familles et supprime tous les appareils push. Autres erreurs : `invalid_session_id`, `invalid_session_query`, `session_rate_limit_exceeded`.

### Consentements

Onboarding incomplet accepté.

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/users/me/language` | `200 { language }` (`fr`, `en`, `es`, `it`) ; onboarding incomplet accepté. |
| PATCH | `/api/users/me/language` | `{ language }` → même préférence ; onboarding incomplet accepté, aucun consentement modifié. |
| GET | `/api/users/me/consents` | `200 { consents, onboarding_complete, required_actions }`. |
| PUT | `/api/users/me/consents` | `{ consents: [{ consent_type, granted }] }` → même état enrichi. |

Types : `terms_of_service_acceptance`, `privacy_notice_acknowledgement`, `sensitive_data_consent`, `location_consent`. Aucun choix marketing. Chaque type apparaît au plus une fois. Chaque choix retourné indique sa version acceptée éventuelle, `required_document_version`, `document_url` et `document_date` de la version courante requise ; le mobile doit présenter ce texte. Aucune preuve technique IP/user-agent n’est exposée et le client ne choisit pas la version.

Les CGU et la notice ne peuvent pas être retirées via cette route : on peut refuser l’onboarding ou supprimer son compte. Retirer le consentement sensible efface sexe/préférences ; retirer la localisation efface la présence. Un rejeu identique est idempotent.

<a id="profil-et-preferences"></a>

### Profil et préférences

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/users/me` | `200` avec le profil propriétaire ; `404 profile_not_found` s’il n’est pas complété. |
| PATCH | `/api/users/me/profile` | `{ firstname, birthdate, sex?, bio? }` → `200 { message: "profile updated" }`. |
| GET | `/api/users/me/preferences` | `200 { min_age, max_age, max_distance_km, looking_for }` ; `404 preferences_not_found`. |
| PATCH | `/api/users/me/preferences` | Ces quatre champs requis → `200 { message: "preferences updated" }`. |
| PATCH | `/api/users/me/presence` | `{ latitude, longitude }` → `200 { message: "presence updated" }`. |

Le profil expose `user_id, firstname, birthdate, profile_answers, moderation`, et `sex, bio, photo` lorsqu’ils existent. Le propriétaire voit son contenu même non approuvé. `moderation.bio` et `moderation.photo` contiennent `status, reasons` ; les réponses ont `moderation_status, moderation_reasons`. Une URL photo propriétaire n’est fournie que si la photo est techniquement prête.

Pour le profil, prénom et naissance sont requis même avec PATCH ; prénom limité à 100 octets, âge minimal 18 ans. `sex` accepte `male | female | other | null`, `bio` accepte du texte ou `null` et au plus 2 000 octets. **Omettre `sex` ou `bio` les remet à `null`** : ce PATCH ne conserve pas automatiquement ces valeurs absentes. Cette route n’accepte aucune photo. Une bio modifiée est réanalysée.

Préférences : âges entiers 18–99, distance entière 1–500 km, `looking_for = male | female | both | other`. Présence : latitude entre -90 et 90, longitude entre -180 et 180.

### Photo unique

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| PUT | `/api/users/me/photo` | Multipart `photo` + clé d’idempotence → `200` avec URL propriétaire et modération. |
| DELETE | `/api/users/me/photo` | Aucun corps → `204` dès acceptation durable du retrait. |

**Envoyer.** Un seul fichier, aucun champ additionnel. Extensions admises : `.jpg .jpeg .png .heic .heif .webp`. Nom, MIME déclaré et signature doivent correspondre. L’image ne peut dépasser 40 Mpx ; entrée et résultat sont limités chacun à 500 000 octets. Le résultat est un WebP privé sans métadonnées, au plus 2 048 px. Limite dédiée : 10 tentatives/h/utilisateur.

Exemple de réponse (URL illustrative) :

```json
{
  "message": "photo updated",
  "photo": "https://storage.example.test/signed-photo.webp",
  "moderation_status": "approved",
  "moderation_reasons": []
}
```

**Rejouer.** Pendant 24 heures, garder la même clé et les mêmes nom/MIME/octets : pas de nouvelle conversion ni écriture objet. Contenu différent : `409 idempotency_key_conflict` ; résultat remplacé/supprimé : `409 idempotency_key_consumed` ; traitement actif : `409 photo_update_in_progress`. L’URL signée expire après 60 secondes et impose `Cache-Control: private, no-store`, y compris pour les anciens objets ; ce n’est pas un identifiant durable. Recharger la ressource autorisée pour obtenir une nouvelle URL.

**Capacité de conversion.** Par instance : une conversion active, quatre requêtes en attente au maximum, cinq secondes d’attente et trente-cinq secondes de traitement au maximum. Saturation, délai dépassé ou convertisseur indisponible : `503 photo_processing_unavailable`, `Retry-After: 5`. Une photo réellement invalide reste un `400 invalid_photo`.

**Retirer.** Le `204` rend la photo immédiatement invisible dans les nouvelles lectures ; l’objet est supprimé en arrière-plan. Une panne ultérieure ne rétablit pas la photo. Les URL déjà émises restent soumises à leur expiration ou à la suppression de l’objet.

Autres erreurs : `400 invalid_idempotency_key`, `400 invalid_photo`, `413 photo_too_large`, `404 profile_not_found`, `409 photo_update_conflict`, `429 photo_rate_limit_exceeded`, `503 photo_processing_unavailable`, `503 photo_storage_unavailable`.

**Modération commune.** Les textes conservent leurs contrôles et motifs automatiques `spam, insult, personal_contact, sexual_content` ; `legacy_unreviewed` identifie un contenu historique. Les photos ne subissent aucune analyse automatique du visage, de la netteté ou du contenu : dès un upload valide, elles sont disponibles (`moderation_status: approved`, motifs vides). Ce statut ne certifie pas une inspection du contenu. La modération manuelle reste possible dans le dashboard ; un rejet retire la photo et programme sa suppression. Les statuts `pending | approved | rejected` restent communs aux contenus.

### Traits et questions de profil

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/traits` | `200 { traits: [{ id, name }] }`, catalogue. |
| GET | `/api/users/me/traits` | Même forme, traits attribués au demandeur. |
| POST | `/api/users/me/traits` | `{ traitId }` → `204` ; `404 trait_not_found`. Le champ snake_case `trait_id` est refusé. |
| DELETE | `/api/users/me/traits/:traitId` | UUID → `204`, même si non attribué. |
| GET | `/api/profile-questions` | `200 { questions: [...] }`, catalogue ordonné. |
| GET | `/api/users/me/profile-answers` | `200 { answers: [...] }`, réponses propriétaires ordonnées. |
| PUT | `/api/users/me/profile-answers` | `{ answers: [{ question_id, answer }] }` → même collection remplacée. |

L’attribution d’un trait existant est idempotente. Une question expose `id, code, prompt, category, display_order` ; ordre par `display_order` puis UUID. Une réponse propriétaire expose `question_id, code, question, answer, position, moderation_status, moderation_reasons`.

**Enregistrer les réponses.** Envoyer toute la liste, de zéro à trois questions distinctes ; l’ordre du tableau définit l’ordre d’affichage. Chaque texte est normalisé sur une ligne, sans caractère de contrôle, entre 10 et 300 caractères et au plus 1 000 octets. Un tableau vide efface les réponses. Le remplacement est atomique et relance leur modération. Erreurs : `404 profile_not_found`, `404 profile_question_not_found`. Si une question est supprimée par l’administration, ses réponses sont supprimées aussi : recharger le catalogue et l’état propriétaire.

<a id="decouverte-matchs-et-messages"></a>

### Découverte, matchs et messages

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/users/me/discovery-status` | `200 { ready, required_actions, presence_expires_at }`. |
| GET | `/api/users/me/discovery-settings` | `200 { paused: boolean }`, visibilité persistée du compte. |
| PATCH | `/api/users/me/discovery-settings` | `{ paused: boolean }` strict → même état confirmé ; `false` reprend la découverte. |
| GET | `/api/feed` | `limit, cursor` → `200 { profiles, next_cursor }`. |
| POST | `/api/swipes` | `{ target_user_id, decision: like ou pass }` → `201 { decision, matched, match? }`. |
| GET | `/api/matches/me` | `limit, cursor` (offset déprécié) → `200 { matches, next_cursor }`. |
| GET | `/api/users/me/continuation-quota` | `200` : plan effectif, `used, weekly_credits, free_remaining, advertising_balance, advertising_remaining, reserved, advertising_stock_limit`, `weekly_limit, remaining` pour Free. |
| PATCH | `/api/matches/:id/reveal` | Aucun corps → `200 { message, photos_revealed }`. |
| DELETE | `/api/matches/:id/reveal` | Aucun corps → `200 { message, photos_revealed: false }`. Retrait idempotent de son accord, photos masquées pour les deux participants. |
| PATCH | `/api/matches/:id/continue` | `{ credit_cost: 0 ou 1 }` : coût maximal accepté → `200 { message, match_confirmed }`. |
| PATCH | `/api/matches/:id/end` | Aucun corps → `200 { message: "match ended", status: "ended" }`. |
| GET | `/api/matches/:id/messages` | `limit, cursor` (offset déprécié) → `200 { messages, next_cursor }`. |
| POST | `/api/matches/:id/messages` | `{ content }` + clé d’idempotence → `201` avec le message stable. |
| PATCH | `/api/matches/:id/messages/read` | `{ read_through_message_id }` → `200 { updated_count, read_through_message_id }`. |
| PATCH | `/api/matches/:id/messages/:msgId/read` | Aucun corps → `200 { message: "message marked as read" }`. |

**Découverte.** Demandeur et candidats doivent être actifs, avec sexe, préférences, consentements sensible/localisation courants et position de moins d’une heure. Les actions de préparation possibles sont `profile, sex, preferences, sensitive_data_consent, location_consent, fresh_presence, resume_discovery`. Les critères âge/sexe/distance s’appliquent dans les deux sens. Blocages, matchs existants et profils déjà swipés sont exclus. Le feed expose `user_id`, prénom, âge, sexe, distance approximative par paliers de 5 km (minimum 5), traits, bio et réponses approuvées. Limites : feed 60/min/utilisateur ; swipe 120/min/utilisateur.

Les coordonnées reçues sont réduites à un centre de zone d’environ 5 km avant stockage. Filtres et tri utilisent ces centres ; `max_distance_km` est donc approximatif aux frontières des zones. Les coordonnées exactes de l’appareil ne sont pas conservées. Le curseur reste chiffré, lié au lecteur, et utilise la distance interne entre centres : deux profils dans le même palier public restent paginés correctement. Le changement de zone est limité à un toutes les cinq minutes, sous verrou transactionnel ; une actualisation dans la même zone reste possible et ne repousse pas cette échéance. Un changement trop rapide renvoie `429 presence_update_limited`, `Retry-After: 300`, sans modifier la présence. Le serveur ne peut pas attester qu’une position déclarée correspond au GPS réel. Voir [Confidentialité et capacité photo](privacy-and-photo-security.md).

**Mode pause.** `paused` vaut `false` à la création du compte et persiste entre sessions/appareils. Avec `true`, le profil est exclu des nouvelles lectures du feed et ne peut plus envoyer ni recevoir de nouveaux swipes ou créer de nouveaux matchs. Les écritures de swipe et de création du match recontrôlent la pause sous verrou de ligne ; une carte chargée avant la pause ne suffit pas à autoriser un nouveau like. Une lecture déjà reçue ne peut pas être effacée à distance. Le diagnostic ajoute `resume_discovery` aux actions requises ; `ready` devient faux. La reprise réévalue les autres prérequis, notamment la fraîcheur de la position. Matchs, conversations, consentements, photo et abonnement existants sont conservés. Le réglage apparaît dans l’export propriétaire sous `account.discovery_paused`.

Les deux routes de réglage exigent le compte mobile avec onboarding terminé, utilisent le même budget dédié de 60 requêtes/min/utilisateur et n’acceptent aucun identifiant de compte en entrée. Le PATCH est une affectation idempotente, sans rejeu réseau automatique. Erreurs : `400 invalid_discovery_settings`, `429 discovery_settings_rate_limit_exceeded`, erreurs communes de session/base. Le schéma et le migrateur doivent être mis à jour avant de déployer cette API. Les collections Postman FR/EN couvrent ces routes. Les vérifications de compilation, de concurrence et de migration sont décrites dans le [guide de tests](../tests/README.md#mode-pause).

Les likes et les profils passés restent valides **30 jours à partir du choix initial**. Une décision de swipe est immuable pendant cette période ; un rejeu ne renouvelle pas son échéance. Après expiration, elle n’exclut plus le profil du feed et un ancien like ne peut plus créer de match. Les autres critères restent appliqués, notamment l’exclusion des matchs déjà enregistrés. Deux likes réciproques valides créent un seul match ; aucun endpoint ne permet de fabriquer directement un match. Erreurs : `409 discovery_not_ready`, `404 discovery_candidate_not_found`, `409 swipe_already_recorded`, `400 invalid_cursor`, `429 swipe_rate_limit_exceeded`, `429 feed_rate_limit_exceeded`, `503 discovery_unavailable`.

Le migrateur ramène également les décisions existantes à `swiped_at + 30 jours`, sans modifier les dates des choix ni les matchs existants. Les anciennes décisions de plus de 30 jours expirent dès cette migration ; leur suppression physique reste confiée à la maintenance. Arrêter les anciennes instances API, appliquer le migrateur puis démarrer la nouvelle version : les anciennes instances calculent encore 365 jours et ne sont pas compatibles avec la nouvelle contrainte PostgreSQL. L’API vérifie cette contrainte au démarrage.

Après perte de réponse, le rejeu du même like conservé retrouve le match `active`, `awaiting_continuation`
ou `confirmed` encore accessible, sans nouveau match ni notification. Les consentements, positions,
préférences et blocages restent vérifiés. Un choix différent reste interdit ; un match terminé ou expiré
reste exclu. Cette exception de rejeu ne réintroduit jamais le profil dans le feed.

**Matchs.** La liste est triée par activité. Chaque résumé contient l’autre utilisateur (prénom, âge, sexe, bio, traits, `profile_answers`, photo conditionnelle), `my_revealed, photos_revealed, my_continued, unread_count, last_message`. La photo reste `null` avant révélation mutuelle, même approuvée. Les routes mobiles d’un match exigent d’en être participant.

Le match est `active` pendant 24 h, puis `awaiting_continuation` pendant les 24 heures suivantes. **L’envoi est suspendu pendant la décision ; l’historique et les accusés de lecture restent accessibles.** Deux accords valides le rendent `confirmed`, sinon il expire. Les délais partent de l’échéance d’origine, même lorsque la maintenance est tardive. La liste expose l’état calculé selon l’horloge.

Après confirmation, `archived` passe à `true` après 10 jours sans nouveau message. La conversation
reste lisible et utilisable. À J23, les deux participants reçoivent un push `match_expiring` ; à J30,
le match et ses messages sont supprimés par la maintenance sans délai additionnel. `inactivity_expires_at`
indique cette échéance, également portée par `expires_at` pour un match confirmé. Un nouveau message
relance le délai et désarchive, sans crédit supplémentaire ; une lecture, une connexion ou un rejeu
ne le relance pas. Le match reste `confirmed` pendant l’archivage. À J30 il disparaît des listes mobiles
et les accès sont refusés même avant le passage du job. Même règle Free/Premium.

Chaque personne Free réserve son propre crédit après acceptation explicite du coût `credit_cost: 1`. Les débits sont atomiques après deux accords ; Premium annonce `credit_cost: 0` et ne consomme aucun crédit. Deux crédits gratuits sont renouvelés chaque lundi à 00:00 UTC, sans cumul. Le stock publicitaire est limité à trois et revient à zéro au même instant ; les crédits doivent être regagnés par vidéo. Free est limité à cinq continuations confirmées par semaine. `remaining` tient compte du stock, des réservations et du plafond ; `weekly_limit` et `remaining` sont omis pour Premium.

Le premier accord reste privé et ne publie aucun événement au partenaire. Seul `my_continued` expose le choix du demandeur. Refus, expiration ou clôture libèrent les réservations ; l’événement terminal ne nomme pas l’auteur. Un rejeu d’un match déjà confirmé renvoie `match_confirmed: true` sans nouveau débit ni événement. Erreurs : `400 invalid_continuation_request`, `404 match_not_found`, `409 continuation_not_available_yet`, `409 continuation_plan_changed`, `409 invalid_match_state`, `409 messaging_not_available`, `410 match_expired`, `403 continuation_quota_reached`. Le client doit relire le quota avant de représenter le coût après un changement de plan. Voir [Parcours et crédits](continuation-and-rewards.md) pour les changements de semaine et la migration.

**Mettre fin à un match.** Chaque participant peut clôturer son match, y compris un match expiré, sans accord de l’autre et sans créer de blocage. Le compte doit être authentifié, actif et avoir terminé son onboarding. Un UUID inconnu, un match d’autrui ou un participant supprimé renvoie `404 match_not_found`. Un second appel renvoie le même `200` sans repousser la purge ni répéter l’événement temps réel.

Le match disparaît des listes mobiles des deux participants. Lecture, envoi et marquage des messages, révélation et continuation deviennent indisponibles (`409 messaging_not_available` ou `409 invalid_match_state`), y compris le rejeu d’un ancien envoi. Les messages et états existants sont conservés jusqu’à la purge après 30 jours ; une échéance de purge déjà définie est conservée. Aucun quota de continuation n’est remboursé et les swipes existants restent inchangés.

Après le commit, les deux participants reçoivent `match.updated` avec `{ match_id, status: "ended" }` : retirer la conversation côté client. Une opération déjà validée avant la clôture peut avoir abouti. Après reconnexion, relire les ressources conformément au contrat SSE.

**Messages.** Contenu non vide, 2 000 caractères maximum ; limite 60/min/utilisateur. Tant que le match reste accessible, rejouer clé/match/contenu identiques renvoie le message existant sans nouvelle notification ; réutiliser la clé pour une autre requête renvoie `409 idempotency_key_conflict`. La lecture par borne marque tous les messages reçus jusqu’à la borne incluse. Un expéditeur ne peut pas marquer son propre message comme lu.

<a id="recompenses-publicitaires"></a>

### Récompenses publicitaires

Les routes propriétaires exigent Bearer, compte actif et onboarding courant. Elles sont destinées aux vidéos volontairement demandées par un compte Free ; elles ne représentent pas un consentement au suivi publicitaire.

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| POST | `/api/users/me/advertising-rewards` | `{ platform: android ou ios }` → `200 { id, ad_unit, custom_data, expires_at, status, reward_credits }`, intention préparée/reprise. |
| GET | `/api/users/me/advertising-rewards/:id` | Même projection propriétaire ; `status: pending ou credited ou expired ou cancelled`. |
| DELETE | `/api/users/me/advertising-rewards/:id` | Aucun corps → `204`, annulation d’une intention non terminée, idempotente. |
| GET | `/api/auth/admob/webhook` | Public ; query AdMob signée inchangée → `200` vide après crédit/déduplication, ou validation d’URL sans crédit avec le marqueur dédié. |

`credited` signifie qu’un crédit a effectivement été accordé. Une preuve d’une vidéo gagnée la
semaine précédente puis traitée après le lundi à 00:00 UTC reste consommée contre le rejeu,
mais renvoie `expired` sans augmenter le solde courant. Le quota reste la référence pour les
crédits encore disponibles, après consommation ou expiration.

Une intention est opaque, limitée à 15 minutes et à une récompense d’un crédit. Une seule intention en attente occupe une place ; une preuve gagnée avant l’échéance peut arriver dans les 24 heures suivantes. Annuler une intention abandonnée libère cette place mais invalide les preuves ultérieures. L’API crédite uniquement après vérification serveur ECDSA ; le callback client du SDK ne donne aucun crédit. Le rejeu d’une transaction liée à une autre intention est refusé.

Pour **valider l’URL dans la console AdMob**, renseigner `custom_data=histae-ssv-validation` et laisser l’ID utilisateur vide. Ce marqueur est réservé : après vérification de la signature Google et du format des paramètres, l’API répond `200` sans consulter d’intention ni modifier un solde. Aucun UUID d’intention ni compte de test n’est nécessaire pour cette validation. Pour une vraie vidéo, le SDK transmet exclusivement le `custom_data` UUID de l’intention ; les contrôles d’unité, de récompense, de temps et de transaction restent obligatoires. Ne jamais utiliser le marqueur de validation dans le SDK pour obtenir un crédit.

Erreurs : `400 invalid_reward_request`, `400 invalid_reward_id`, `400 invalid_admob_signature`, `400 invalid_admob_reward`, `404 reward_not_found`, `409 advertising_stock_full`, `409 advertising_not_eligible`, `429 advertising_rate_limit_exceeded`, `429 admob_webhook_rate_limit_exceeded`, `503 advertising_disabled`, `503 admob_verification_unavailable`. Les créations reprennent `RATE_LIMIT_BILLING`, les callbacks `RATE_LIMIT_BILLING_WEBHOOK` ; espaces de clés distincts. Comme les autres webhooks de fournisseur, AdMob est exclu de la limite globale IP et dispose de sa limite dédiée. Tant que le fournisseur est désactivé, le webhook renvoie `503 advertising_disabled` avant toute récupération de clés.

[Configuration et vérification AdMob](continuation-and-rewards.md#4-configuration-admob-et-tunnel-cloudflare).

<a id="appareils-et-temps-reel"></a>

### Appareils et temps réel

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/users/me/devices` | `200 { devices: [...] }`, sans tokens FCM. |
| POST | `/api/users/me/devices` | `{ push_token, platform: ios ou android, app_version?, language?: fr, en, es, it ou null }` → `201` avec l’appareil public. |
| DELETE | `/api/users/me/devices/:id` | UUID d’un appareil du compte → `204` ; `404 device_not_found`. |
| GET | `/api/users/me/events` | Flux SSE authentifié `text/event-stream`. |

L’appareil expose UUID, `session_id`, plateforme, version et dates d’usage ; la session vient du Bearer, pas du corps client. Un token fournisseur déjà connu est réaffecté/rafraîchi de façon idempotente. Une ancienne liaison peut avoir `session_id: null`.

SSE envoie `connected`, un heartbeat toutes les 25 secondes, puis `match.created, match.updated, matches.invalidated, message.created, message.read, subscription.updated`. Le flux ferme à expiration du JWT ou après échec/révocation détecté lors du contrôle de session toutes les 25 secondes. Le client SSE doit pouvoir envoyer le Bearer.

**Après reconnexion, relire les ressources : SSE n’a pas de replay hors ligne.** Le push optionnel n’embarque jamais le texte privé d’un message. Dédupliquer son `notification_id` stable : une réponse fournisseur perdue peut provoquer un doublon externe. Les alertes de paiement devenues obsolètes ne sont pas envoyées. Les types push pris en charge sont `new_match`, `new_message`, `match_expiring` et `billing_payment_failed`. Le push d’inactivité ne contient que les identifiants techniques ; il est abandonné si un nouveau message a repoussé l’échéance ou si le match est devenu inaccessible. Avec le push désactivé, les tâches consommées ne seront pas rattrapées après activation.

### Abonnement

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/users/me/subscription` | `200` état effectif commun Stripe / Apple / Google. |
| POST | `/api/users/me/subscription/checkout` | Route retirée : `410 billing_web_only` après authentification mobile. |
| POST | `/api/users/me/subscription/portal` | Route retirée : `410 billing_web_only` après authentification mobile. |
| GET | `/api/billing/store-products` | Query `provider=app_store|google_play` → `200 { provider, offers: [{ billing_period, product_id, base_plan_id }] }` ; public, aucun prix natif. |
| POST | `/api/users/me/subscription/purchase-intents` | `{ provider: app_store|google_play }` → `200 { provider, account_binding, offers }`, compte onboardé. |
| POST | `/api/users/me/subscription/apple/verify` | `{ reference: transactionId }` → `204` après preuve serveur ; relire l’abonnement. |
| POST | `/api/users/me/subscription/google-play/verify` | `{ reference: purchaseToken }` → `204` après preuve serveur ; relire l’abonnement. |
| POST | `/api/billing/apple/webhook` | `{ signedPayload }` JWS Notifications V2 Apple → `200 { received: true }`. |
| POST | `/api/billing/google-play/webhook` | Enveloppe Pub/Sub + `Authorization: Bearer <JWT OIDC>` → `200 { received: true }`. |

État : `plan, provider, status, access_granted, billing_period`, dates de période/essai/annulation,
`cancel_at_period_end, customer_portal_available, management_url`. `provider` : `stripe|app_store|google_play|null`.
`management_url` : lien officiel du store ou `null`. `plan` vaut Free si aucun droit courant, même si un historique
Premium existe. Ne jamais déduire les droits du SDK, d’un webhook reçu, d’un redirect ou de `status` seul.
États natifs : `pending, active, grace_period, past_due, on_hold, paused, canceled, expired, revoked`.
Les états Stripe historiques sont conservés. Les protections de consentement et la limite 10/min/compte s’appliquent.
Un compte de revue Google Play actif dispose de Premium offert sans expiration : `access_granted: true`,
`provider: null` et `current_period_ends_at: null`, sauf si un abonnement fournisseur actif est prioritaire.
Ce droit est réservé au marqueur serveur de revue ; aucune route utilisateur ne peut le définir.
Un achat possède un compte immuable ; un reçu d’un autre compte renvoie `409 store_purchase_account_conflict`.
Autres erreurs : `503 store_unavailable`, `422 invalid_store_purchase`, `409 subscription_already_active`.
Voir [achats stores](store-billing.md) pour les états, acquittements, restauration et notifications.

### Paiement du site

Session distincte des JWT mobiles et du cookie administrateur. Origin exact obligatoire sur toutes les mutations.
Cookie HttpOnly host-only, Secure en HTTPS, SameSite Strict ; session trente minutes. Réponses `no-store`.

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| POST | `/api/web/billing/otp/send` | `{ user_id: UUIDv4, phone_number }` + `Idempotency-Key: UUIDv4` → `202 { challenge_id, expires_at, message }` générique ; SMS seulement si les deux correspondent à un compte actif. |
| POST | `/api/web/billing/otp/verify` | `{ challenge_id, phone_number, otp: six chiffres }` → `204` + cookie ; cinq essais maximum, code lié au challenge. |
| GET | `/api/web/billing/session` | Cookie web → `200 { user_id, onboarding_complete, subscription }` ; sinon `401 web_authentication_failed`. |
| DELETE | `/api/web/billing/session` | Cookie web + Origin → `204` et cookie expiré. |
| GET | `/api/web/billing/plans` | Catalogue public du site, localisé, même forme que `/api/plans`. |
| POST | `/api/web/billing/checkout` | Cookie web, Origin, clé UUIDv4, `{ billing_period: monthly|annual }` → `201 { session_id, url, expires_at }`. |
| POST | `/api/web/billing/portal` | Cookie web + Origin → `201 { url }` du portail Stripe. |

Aucun compte créé par ce parcours. Limites OTP par IP et téléphone partagées avec le mobile et les protections
Sweego. Les réponses d’envoi restent génériques même en cas d’envoi non confirmé. Le code ne donne pas de JWT mobile.
Avant paiement : compte actif, non banni et consentements courants ; `403 onboarding_incomplete` sinon.
`403 invalid_web_request_origin` si Origin incorrect. `503 web_billing_unavailable` bloque Checkout
et le portail si le paiement du site est désactivé ; la connexion et la gestion des données restent disponibles.
Une réponse Checkout perdue se reprend avec la même clé et la même période. Expiration à 35 minutes à la préparation ;
respecter `expires_at`. Clé consommée : relire les droits avant de préparer une autre intention.
Les états non terminés connus d’un fournisseur bloquent un second achat, même sans accès Premium.
Changer de périodicité remplace uniquement une session Checkout ouverte et connue : l’API confirme
son expiration chez Stripe avant de créer la nouvelle. Les sessions terminées, les créations en
cours et les tentatives incertaines restent protégées. La clé de la formule remplacée devient
consommée ; après relecture des droits, le site peut utiliser une nouvelle clé pour cette formule.
Les erreurs et procédures Stripe existantes restent décrites dans [le guide Stripe](billing.md).

### Gestion des données depuis le site

La page publique `/{fr,en,es,it}/account/data/`, liée au footer, présente les conséquences de chaque
suppression et une adresse d’assistance pour les personnes sans accès à l’app. Les actions automatiques
réutilisent le cookie web de trente minutes ; aucun JWT mobile n’est émis. La session peut être partagée
avec « Mon abonnement ». Origin exact `WEB_BILLING_ORIGIN` obligatoire sur toutes les mutations,
y compris si `WEB_BILLING_ENABLED=false`. Toutes les réponses sont `no-store` et `no-referrer`.

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| POST | `/api/web/account/otp/send` | Même UUID/téléphone, clé d’idempotence et réponse générique que le parcours de connexion du paiement. |
| POST | `/api/web/account/otp/verify` | Même challenge/code SMS → `204` et cookie HttpOnly. |
| GET | `/api/web/account/session` | Cookie valide → `{ user_id, onboarding_complete }`, sans charger la facturation. |
| DELETE | `/api/web/account/session` | Déconnexion du navigateur et expiration du cookie → `204`. |
| DELETE | `/api/web/privacy/data` | `{ category: photo|location|sensitive_profile }` → `204` ; le compte et les autres catégories sont conservés. |
| POST | `/api/web/privacy/deletion-token` | Émet un jeton à usage unique lié au compte connecté → `201 { confirmation_token, expires_at }`. |
| DELETE | `/api/web/privacy/account` | `{ confirmation_token }` → `202 { request_id, status: in_progress }` et cookie expiré. |

L’identité cible vient uniquement de la session ; aucun `user_id` cible n’est accepté dans une
suppression. Les consentements requis pour l’onboarding et les droits Premium ne conditionnent pas
ces démarches. Le compte doit être actif et non banni pour le parcours automatique ; sinon l’assistance
publique reste accessible. Les mutations sont limitées par compte (`RATE_LIMIT_PHOTO`, espace `web-privacy`).
Pour `/api/web/privacy/*`, l'origine, la session et le débit précèdent la validation des champs
du JSON. Le parseur global vérifie d'abord le format, la syntaxe et la taille du corps. Une requête
avec des champs invalides ne contourne pas les gardes. Origine absente, dupliquée ou incorrecte : `403 invalid_web_request_origin` ; cookie
absent, ambigu, expiré ou compte inactif : `401 web_authentication_failed` ; budget épuisé :
`429 privacy_rate_limit_exceeded` avec `Retry-After`. Un corps invalide après ces contrôles reçoit
`400 invalid_request_body`. Un jeton de confirmation bien formé mais expiré ou lié à un autre compte
reçoit `401 invalid_or_expired_deletion_token`.
Pour les appels sans corps (jeton d'effacement, déconnexion, portail), omettre `Content-Type`
ou envoyer `{}` avec `application/json`. Déclarer du JSON avec un corps vide donne `400`,
conformément au parseur HTTP commun.

`photo` réutilise le retrait de photo et son nettoyage S3 différé. `location` retire le consentement de
localisation et efface la présence ; `sensitive_profile` retire le consentement sensible et efface le sexe
et les préférences. Les historiques de choix sont conservés selon les règles de rétention existantes.
La fermeture du compte utilise exactement le même service d’effacement que l’app : désactivation
transactionnelle, invalidation des sessions puis traitement outbox reprenable. `202` n’atteste pas la
fin du nettoyage. Une réponse réseau perdue n’est pas présentée comme une réussite ni rejouée
automatiquement par le site. Aucune table, migration ou remise à zéro n’est ajoutée.

### Administration des achats natifs

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/admin/store-billing` | Cookie admin ; `period` selon les périodes de revenus → `200 { enabled, sources, period, finance, recent, pending_notifications, dead_letters, finance_basis }`. |
| POST | `/api/admin/store-billing/finance` | Cookie superadmin récent + Origin, `{ provider, report_id, rows: [{ row_id, paid_at, currency, gross_cents, net_cents }] }` → `204`, import atomique et audité. |

Rapports normalisés, au plus 1 000 lignes, IDs stables 1–128 caractères alphanumériques/`_.:-`, dates avec fuseau non
futures, monnaie en majuscules, montants signés entiers bornés à 10¹² centièmes par ligne. Net inconnu : `null`.
Un rejeu identique est idempotent ; même clé avec d’autres valeurs → `409 finance_report_conflict`, rollback global.
Aucune estimation à partir des abonnés ; devises séparées ; Stripe conserve ses factures payées séparément.

<a id="droits-effacement-et-surete"></a>

### Droits, effacement et sûreté

Onboarding incomplet accepté pour les jetons/effacement, demandes RGPD et export, mais pas pour les blocages/signalements.

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| POST | `/api/users/me/deletion-token` | Aucun corps → `201 { confirmation_token, expires_at }`. |
| DELETE | `/api/users/me` | `{ confirmation_token }` → `202 { request_id, status: "in_progress" }`. |
| POST | `/api/users/me/data-subject-requests` | `{ type, details? }` → `201` avec la demande. `rectification` exige une précision non vide ; `restriction` et `objection` sont retirés. |
| GET | `/api/users/me/data-subject-requests` | `200 { requests: [...] }`. |
| GET | `/api/users/me/data-export` | `200 application/json` en pièce jointe avec l’export portable du demandeur. |
| GET | `/api/users/me/blocks` | `200 { blocks: [...] }`, toujours `photo: null`. |
| POST | `/api/users/me/blocks/:userId` | UUID → `204`, clôt les matchs entre les comptes et empêche leur recréation. |
| DELETE | `/api/users/me/blocks/:userId` | UUID → `204`. |
| POST | `/api/reports` | `{ reported_user_id, match_id?, reason, description? }` → `201` avec le signalement. |

Les champs `details` (message utilisateur) et `response` (réponse visible par l’utilisateur) sont indépendants des notes internes admin. Maximum : 2 000 caractères Unicode, après normalisation des espaces extérieurs ; retours à la ligne autorisés. Les anciennes lignes peuvent avoir ces deux champs à `null`. La clôture d’une rectification enregistre un traitement manuel et sa réponse ; elle ne modifie pas automatiquement les données métier. Les types `restriction` et `objection` restent lisibles pour l’historique uniquement.

**Effacement.** Demander le jeton dédié, puis le transmettre comme reçu (`uuid:secret`). Il remplace le précédent et expire après dix minutes par défaut (`ACCOUNT_DELETION_TOKEN_TTL`, 1–30 min). Exemple de réponse au DELETE :

```json
{
  "request_id": "11111111-1111-4111-8111-111111111111",
  "status": "in_progress"
}
```

Le `202` désactive immédiatement le compte ; fermer la session mobile. **Ne pas afficher que toutes les données ont déjà été supprimées.** Le nettoyage continue en arrière-plan malgré une panne externe. Jeton invalide/expiré : `401 invalid_or_expired_deletion_token`. Si la réponse est perdue après acceptation, le Bearer devient invalide : un retry n’est pas une route publique de suivi et ne rend pas forcément le même `202`. Voir [suivi et reprises](operations.md#outbox-et-reprises-operateur).

**Droits.** Types de nouvelle demande : `access | erasure | portability | rectification` ; une seule demande ouverte par type/utilisateur. L’export contient profil/réponses, messages et réponses des demandes de données (sans les notes admin), abonnement/factures liés, métadonnées des sessions sans secrets et uniquement les décisions de swipe sortantes. Il n’expose jamais les décisions entrantes d’autrui. Le JSON est préparé par pages dans un fichier temporaire privé, transmis comme flux puis supprimé. Toutes ses données PostgreSQL, swipes compris, partagent un instantané `REPEATABLE READ`. Limite 5/h/utilisateur : `429 data_export_rate_limit_exceeded` ; export au-delà de la borne configurée : `413 data_export_too_large` ; toutes les places de préparation occupées : `503 data_export_busy` avec `Retry-After` ; source indisponible avant envoi : `503 data_export_unavailable`. L’accès réussi est journalisé.

**Signalements.** `reason = inappropriate_content | fake_profile | harassment | spam | other` ; description au plus 2 000 octets ; `match_id` et description peuvent être nuls. Auto-signalement interdit, cible existante et match éventuel reliant les deux comptes. Limite 5/h/utilisateur.

<a id="administration"></a>

## Administration

<a id="entree-webauthn"></a>

### Entrée WebAuthn

Ces quatre routes d’entrée ne demandent pas de session existante. La preuve WebAuthn est vérifiée contre l’origine admin ; bootstrap exige en plus le secret d’enrôlement. Limite : 10 requêtes/5 min/IP par défaut. Aucun SSO ni fournisseur d’identité externe.

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| POST | `/api/admin/auth/login/options` | Aucun corps → `200 { challenge_id, options }`. |
| POST | `/api/admin/auth/login/verify` | `{ challenge_id, credential }` → `200` avec session publique et cookie. |
| POST | `/api/admin/auth/bootstrap/options` | `{ bootstrap_token }` → `200 { challenge_id, options }`. |
| POST | `/api/admin/auth/bootstrap/verify` | `{ bootstrap_token, challenge_id, credential, name }` → `201` avec session et cookie. |

En développement, ouvrir **`http://localhost:5173`**, RP ID `localhost`, via le proxy même-origine `/api` du dashboard ; pas `127.0.0.1`. En production : origine HTTPS exacte, cookie `Secure` préfixé `__Host-`. Le RP ID doit correspondre au domaine configuré.

Transmettre les options à l’API WebAuthn du navigateur et renvoyer le credential sérialisé. Passkey découvrable et vérification utilisateur obligatoires. Challenge valable cinq minutes, à usage unique. La session publique expose `user_id, role, authenticated_at, expires_at`, jamais son secret. Bootstrap, créé hors bande, expire après quinze minutes par défaut. Procédure d’enrôlement : [Administration](administration.md#enregistrer-la-premiere-passkey).

### Sessions et passkeys

Toutes les routes suivantes exigent la session admin. Expiration inactive de 30 minutes et absolue de huit heures par défaut.

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/admin/auth/session` | Session courante après vérification du compte et de la passkey. |
| GET | `/api/admin/auth/monitoring` | **Superadmin** ; contrôle du proxy → `204` avec `X-Histae-Monitoring-User`. Exige `X-Forwarded-Method` et `X-Forwarded-Uri` sous `/monitoring/grafana/`, `/monitoring/prometheus/` ou `/monitoring/alertmanager/`. Toute Origin fournie doit être exacte ; obligatoire pour une méthode transférée non sûre. |
| GET | `/api/admin/me` | `200 { user_id, role }`, alias métier ; préférer la route session. |
| POST | `/api/admin/auth/logout` | Aucun corps → `204`, expire aussi le cookie. |
| GET | `/api/admin/auth/credentials` | Passkeys actives, marqueur de la passkey courante, sans clé publique. |
| PATCH | `/api/admin/auth/credentials/:id` | **Récente** ; `{ name }` → `200`, renommage audité. |
| POST | `/api/admin/auth/credentials/options` | **Récente** ; `201`, options d’une passkey supplémentaire. |
| POST | `/api/admin/auth/credentials/verify` | **Récente** ; `{ challenge_id, credential, name }` → `201`. |
| DELETE | `/api/admin/auth/credentials/:id` | **Récente** ; UUID → `204`. |
| GET | `/api/admin/auth/sessions` | Sessions actives du compte avec passkey, dates et `current`, sans token/hash. |
| DELETE | `/api/admin/auth/sessions/:id` | **Récente** ; UUID → `204`, sauf session courante. |
| POST | `/api/admin/auth/sessions/revoke-others` | **Récente** ; `201 { revoked_sessions }`, garde la session courante. |
| GET | `/api/admin/auth/events` | `limit, cursor` → `200 { events, next_cursor }`, récents d’abord. |

Un nom de passkey normalisé contient 1–100 caractères, au plus 200 octets. Interdit de révoquer la passkey courante ou la dernière active ; révoquer une autre passkey invalide les sessions qui en dépendent. Pour fermer la session courante, utiliser logout, pas la révocation ciblée.

Erreurs : `invalid_or_expired_admin_bootstrap`, `invalid_or_expired_webauthn_challenge`, `webauthn_registration_failed`, `webauthn_authentication_failed`, `admin_session_invalid`, `admin_reauthentication_required`, `invalid_admin_request_origin`, `last_admin_credential`, `current_admin_credential`, `current_admin_session`, `admin_session_not_found`, `admin_credential_not_found`, `admin_auth_rate_limit_exceeded`.

### Comptes, signalements et droits

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/admin/users` | `status, role, search, limit, cursor` (offset déprécié) → `200 { users, next_cursor }`. |
| GET | `/api/admin/user-names` | `ids` : 1 à 100 UUID séparés par des virgules → `200 { users: [{ user_id, firstname }] }`. Comptes actifs uniquement. |
| GET | `/api/admin/users/:id` | UUID + query `reason?` → détail administratif audité. |
| PATCH | `/api/admin/users/:id/status` | `{ is_banned, reason? }` → bannissement/débannissement. Motif requis pour bannir par un admin. |
| PATCH | `/api/admin/users/:id/role` | Superadmin uniquement : `{ role: "admin" | "user", reason? }` → promotion ou retrait des droits admin. |
| PATCH | `/api/admin/users/:id/complimentary-premium` | Superadmin uniquement : `{ enabled: boolean, reason? }` → `200 { message }` ; active ou retire un Premium offert sans expiration. Idempotent et audité. |
| GET | `/api/matches/:userId` | UUID utilisateur + `reason?, limit, cursor` (offset déprécié) → `200 { matches, next_cursor }`. **Route admin malgré son chemin.** |
| GET | `/api/admin/matches/:id/messages` | UUID match + `reason?, limit, cursor` (offset déprécié) → conversation auditée. |
| GET | `/api/admin/reports` | `status?, limit, cursor` (offset déprécié) → `200 { reports, next_cursor }`. |
| PATCH | `/api/admin/reports/:id` | `{ status }` → `200 { message: "report updated" }` ; `404 report_not_found`. |
| GET | `/api/admin/data-subject-requests` | `status?, limit, cursor` (offset déprécié) → `200 { requests, next_cursor }`, avec progression `erasure` éventuelle. |
| PATCH | `/api/admin/data-subject-requests/:id` | **Récente** ; `{ status, notes?, response? }` → transition contrôlée et auditée. Pour clôturer/refuser une rectification, `response` est obligatoire ; les catégories retirées sont en lecture seule. |
| GET | `/api/admin/data-access-logs` | `user_id, limit, cursor` (offset déprécié) → `200 { logs, next_cursor }`. |

Recherche utilisateurs : prénom ou UUID exact ; `status = active | banned`, `role = user | admin | superadmin`. La liste ne signe aucune photo (`photo: null`). Le détail expose compte, profil, préférences, traits, consentements et fraîcheur de présence ; jamais téléphone, empreinte ou coordonnées précises. Il expose aussi `complimentary_premium` (offre Histae enregistrée) et `is_review_account` (compte de revue automatique), deux booléens en lecture seule sur le détail. Les motifs fournis mesurent 3–500 caractères ; une conversation est auditée pour les deux participants.

L’offre Histae est indépendante du rôle et du marqueur de revue. Une attribution ne crée ni achat,
facture ni revenu, n’annule aucun abonnement fournisseur et utilise la projection commune des droits.
Le retrait ne supprime que cette offre : un abonnement payant actif continue de donner Premium.
Un compte banni ne peut pas recevoir une offre mais peut se la voir retirer. Un compte supprimé
renvoie `404 account_not_found`. Un administrateur ordinaire, un acteur n’étant plus superadmin
en base ou une modification de l’offre automatique d’un compte de revue renvoie `403 admin_action_forbidden`.
Le motif est facultatif pour le superadmin ; l’API fournit un motif d’audit par défaut.
Seuls les changements d’état créent les événements `admin_grant_premium` / `admin_revoke_premium`.

Un bannissement invalide les sessions mobiles. Un admin n’agit que sur un rôle utilisateur ; un superadmin ne peut agir ni sur lui-même ni sur un autre superadmin. Statuts de signalement : `pending | reviewed | dismissed`.

Le rôle `superadmin` ne peut pas être attribué par l’API et un index PostgreSQL limite la base à un seul superadmin. Le retrait du rôle admin révoque les sessions, passkeys et jetons d’enrôlement administrateur ; le compte utilisateur reste actif. La première passkey d’un nouvel admin nécessite ensuite un jeton émis par `admin-bootstrap`.

Pour une demande RGPD, la mutation accepte `in_progress | completed | rejected`. Sur un effacement en cours, demander `completed` **programme** le workflow et répond `200 { "message": "account erasure scheduled" }` ; la demande reste `in_progress` jusqu’à réussite réelle. Le rejeu ne duplique pas le travail. Rejet d’un effacement commencé : `409 invalid_data_request_transition`. Autres transitions : `200 { "message": "data subject request updated" }`.

`erasure` peut être nul ; sinon il expose `step, updated_at, event_id, status, attempts, last_error_code`. Les étapes sont `stripe | photos | swipes | postgres | completed`. Après purge d’un événement résolu, `event_id/status` peuvent être nuls. Aucun payload, identifiant Stripe, clé objet ou URL.

<a id="catalogue-et-moderation"></a>

### Catalogue et modération

Les traits et questions admin ajoutent une carte `translations` (`fr`, `en`, `es`, `it`).
POST/PATCH acceptent ses traductions facultatives ; clé absente = conservation, `null` = suppression,
français non supprimable. `name` / `prompt` reste la source française. Voir [les règles de traduction](internationalization.md#catalogues-et-dashboard).

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/admin/traits` | `200 { traits: [...] }`, lecture du catalogue avec session administrateur. |
| POST | `/api/admin/traits` | `{ name }` → `201 { id, name }`. |
| PATCH | `/api/admin/traits/:id` | `{ name }` → `200 { message: "trait updated" }`. |
| DELETE | `/api/admin/traits/:id` | UUID → `204`, suppression définitive. |
| GET | `/api/admin/profile-questions` | `200 { questions: [...] }`, avec dates et `answer_count`. |
| POST | `/api/admin/profile-questions` | `{ prompt, category, display_order? }` → `201` avec la question. |
| PATCH | `/api/admin/profile-questions/:id` | Au moins un de ces trois champs → `200` avec la question. |
| DELETE | `/api/admin/profile-questions/:id` | UUID → `204`, supprime aussi toutes ses réponses. |
| GET | `/api/admin/content-moderation` | `status?, content_type?, limit, cursor` (offset déprécié) → `200 { cases, next_cursor }`. |
| GET | `/api/admin/content-moderation/:id` | UUID + query `reason?` → contenu audité. |
| PATCH | `/api/admin/content-moderation/:id` | `{ version, decision, reason? }` → `200 { message: "content moderation decision recorded" }`. |

Traits : nom non vide, au plus 100 octets ; doublon `409 trait_already_exists`. Questions : catégories `daily_life | personality | interests | relationships | conversation`, ordre par défaut 100 ; doublon de libellé insensible à la casse : `409 profile_question_already_exists`. Modifier le libellé affecte aussi les réponses existantes. **Avant suppression, afficher `answer_count` et demander confirmation explicite** : les réponses ne sont pas conservées.

**Revoir un contenu.** Filtrer par `status = pending | approved | rejected` et `content_type = photo | bio | profile_answer`. La liste expose identifiants, utilisateur/prénom, statut/motifs, versions de politique et de revue, signaux/contrôles photo et dates ; pas le texte, la question, la clé objet, l’URL ou l’image. Ouvrir le détail fournit `content, question, photo` selon le type ; toute consultation est auditée avant signature photo.

La décision est `approved | rejected`, avec la `version` lue au préalable. Aucune liste de contrôles photo n’est requise. Un rejet retire la photo et programme sa suppression. En cas de `409 moderation_case_stale`, recharger plutôt que réappliquer aveuglément. Autres erreurs : `400 invalid_moderation_case_id`, `400 invalid_moderation_request`, `404 moderation_case_not_found`, `409 moderation_review_not_allowed`.

### Exploitation et reprises

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/admin/metrics` | `revenue_period?` → synthèse métier et `operations`. |
| GET | `/api/admin/revenue` | `revenue_period?` → estimation de chiffre d’affaires. |
| GET | `/api/admin/photo-reconciliation` | `status?, limit, cursor` (offset déprécié) → traitements photo à réconcilier. |
| POST | `/api/admin/photo-reconciliation/:id/retry` | UUID photo + `{ reason? }` → `202`. |
| GET | `/api/admin/billing-reconciliation` | `kind?, limit, cursor` → dead letters Stripe `200 { events, next_cursor }`. |
| GET | `/api/admin/outbox/dead-letters` | `limit, cursor` → `200 { events, next_cursor }`. |
| POST | `/api/admin/outbox/:id/retry` | **Récente** ; `{ reason? }` → `202`, relance auditée. |
| POST | `/api/admin/outbox/:id/discard` | **Récente** ; `{ reason? }` → `204`, abandon audité lorsque permis. |

`revenue_period = last_7_days | last_30_days | month_to_date | previous_month | year_to_date | all_time`, défaut `month_to_date`. Les bornes calendaires utilisent `Europe/Paris`, début inclus et fin exclue.

`GET /api/admin/metrics` expose `subscriptions: [{ plan, users }]` par niveau de droits et
`subscription_offers: [{ offer, users }]` avec quatre compteurs, toujours présents :
`free`, `premium_monthly`, `premium_annual`, `premium_unspecified`. Ce dernier regroupe les
accès Premium sans périodicité enregistrée ; il ne représente pas une quatrième offre vendue.
La ventilation suit `user_subscription.billing_period` et les mêmes règles d'accès effectif que
`subscriptions` : Stripe `trialing`, `active` ou `past_due` avec fin de période future, ou accès
manuel non expiré. Un abonnement expiré ou Stripe résilié compte comme Free. Les comptes supprimés
sont exclus ; les comptes bannis restent inclus comme dans les totaux existants. Les compteurs
représentent les accès actuels, indépendamment de `revenue_period`, et ne modifient aucun abonnement.

`revenue` expose `period, period_start, period_end, premium_subscriptions, paid_invoices,
estimated_revenue_cents, currency, excluded_currency_invoices, basis`. La base est
`stripe_paid_invoices` : somme de `billing_invoice.amount_paid` pour les factures d’abonnement
`paid`, de montant positif, dont `paid_at` appartient à la période et dont la devise correspond
au catalogue Premium. `paid_invoices` compte ces factures ; `premium_subscriptions` compte leurs
abonnements Stripe distincts, pas les comptes Premium actuels. `price_per_subscription_cents`
a été supprimé : aucun tarif unique ne permet de calculer ce revenu.

Achats annuels, remises et renouvellements utilisent leurs montants historiques ; les essais à 0,
factures impayées, dates de paiement absentes et factures sans abonnement sont exclus. Les factures
d’une autre devise ne sont jamais additionnées : `excluded_currency_invoices` permet de les signaler.
Une annulation ou un effacement ultérieurs ne retirent pas les factures historiques enregistrées.
Ce total brut inclut les taxes facturées, sans déduire frais et remboursements ; des webhooks manquants
peuvent laisser des factures absentes. **Ce n’est ni un MRR, ni un bénéfice, ni un versement bancaire.**
Voir [Abonnements et revenus](billing.md).

`operations` expose latences/compteurs HTTP et `401/403/429/5xx`, mémoire/uptime du processus Rust, résultats des dépendances, pool, outbox et maintenances. Les mesures du processus repartent à zéro au redémarrage ; les états outbox/maintenance sont persistants. Chaque maintenance fournit notamment `processed_count, batch_count, duration_ms, work_remaining` ; ce dernier signale qu’une passe a consommé son budget borné et doit être reprise. `operations.outbox.notification_push` détaille `pending, processing, completed, dead_letter, discarded, oldest_pending_at` ; `operations.outbox.billing_reconciliation` fournit les mêmes états utiles sans `discarded`. `completed` signifie tâche acquittée encore conservée, pas réception par un terminal ni validation d’un paiement. Voir [Exploitation](operations.md) et [bornes de configuration](configuration.md#groupes-de-variables).

L’export Prometheus n’est pas une route de ce contrat : lorsqu’il est activé, il écoute sur un serveur privé
distinct, authentifié par bearer token. Voir [supervision et runbooks](observability.md).

`operations.sms_delivery` expose `states` (`pending, accepted, sent, failed, unknown`), `awaiting_callback`,
`oldest_unresolved_age_seconds`, `average_acceptance_ms`, `average_sent_callback_ms`, `average_failure_ms`,
`webhook_enabled`, `handset_delivery: "not_confirmed"`, `retention: "otp_expiry"` et `callbacks`
(`applied, ignored, conflict, invalid_signature, invalid_event, unavailable, disabled`). Les états et délais
portent sur les OTP non expirés, pas sur un historique complet ; les compteurs de callbacks sont locaux au processus.

Réconciliation photo : filtre `all | stale_processing | deleting | dead_letter` (défaut `all`). UUID photo/utilisateur, métadonnées techniques, diagnostics et état outbox uniquement ; aucune image ni clé objet. Une photo prête ou un traitement récent refuse la relance : `409 photo_reconciliation_not_allowed` ; worker actif : `409 photo_reconciliation_in_progress` ; photo absente : `404 photo_not_found`.

Réconciliation Stripe : la liste ne contient que les dead letters qui exigent une action humaine ; la file normale reste agrégée dans `operations`. `kind = all | subscription | customer_creation`. Elle expose UUID d’événement/utilisateur, type, tentatives, code d’erreur normalisé et dates ; jamais payload, identifiant fournisseur ou moyen de paiement. Une dead letter peut être relancée par la route outbox commune, après authentification récente et avec audit. La relance effectue une nouvelle lecture et n’ordonne aucun paiement.

Les dead letters exposent type, tentatives et code normalisé, jamais payload/agrégat/clé objet. Une décision devenue obsolète renvoie `409 outbox_event_not_dead_letter`. L’abandon de `account.erase` et des événements `billing.*` est toujours interdit ; celui de `photo.delete` est interdit tant que sa trace existe : `409 outbox_discard_not_allowed`. `notification.push` peut être abandonné sans effacer la notification. Un `202` de reprise ne garantit pas que la dépendance sera disponible lors du prochain essai.

<a id="entretien-du-contrat"></a>

## Entretien du contrat

Modifier ce guide avec toute évolution de route, DTO, réponse, autorisation ou règle de rejeu. Garder les chemins complets dans les tableaux, une seule ligne par couple méthode/chemin.

Le test `tests/route_inventory.rs` compare ces lignes aux déclarations de routes présentes dans les sources Axum. Il détecte les couples méthode/chemin absents ou non documentés ; il ne vérifie pas le montage du routeur final ni les doublons éliminés par son ensemble de comparaison. Les alias HEAD automatiques et le preflight CORS généré ne font pas partie de cet inventaire applicatif.

Ce contrôle ne prouve ni les schémas JSON, ni les droits, ni les garanties métier : les autres tests de contrat et intégrations restent nécessaires. Voir [guide de validation](../tests/README.md), [responsabilités internes](architecture.md) et [validations de livraison](production-readiness.md).

### Données de crédits dans la fiche administrateur

`GET /api/admin/users/:userId` conserve ses guards, motif/audit et données existants, avec `continuation_quota` supplémentaire. Cet objet présente les compteurs du compte ciblé et aucune décision individuelle de son partenaire. Le dashboard affiche ses soldes et réservations depuis cette projection. `GET /api/plans` indique si les récompenses publicitaires sont activées ; il décrit les règles sans démarrer de publicité.

### Photo requirements and per-match withdrawal

Discovery requires a current `ready` photo not rejected by manual moderation for the viewer and every candidate. New uploads are immediately available without content analysis.
`GET /api/users/me/discovery-status` includes `photo` in `required_actions` when that requirement is
missing. Feed and targeted swipe eligibility enforce it on the server; no photo or URL is added to feed DTOs.
Removing or replacing a photo with one awaiting approval suspends discovery until approval.

`PATCH /api/matches/:id/reveal` records the caller's agreement only when both participants have
approved photos (`409 photo_required` otherwise). `DELETE` on the same route withdraws the caller's
agreement under the same match lock and leaves messages, continuation and the other person's agreement
intact. The two photos disappear from subsequent authorized projections; either person's missing or
unapproved photo also prevents the asymmetric issue of a partner photo URL. Withdrawal never requires
retaining a photo. Both commands publish the current `photos_revealed` state through `match.updated`.

Agreement belongs to the match, not to a photo version. Replacing a photo does not reset agreements:
the newly approved current photo is used on subsequent reads of every mutually authorized match.
Previously issued signed storage URLs retain their existing maximum 60-second validity; a withdrawal
cannot recall downloaded bytes. Clients evict displayed images on privacy changes and realtime invalidation.
