# Histae API — contrat HTTP

Ce guide est la référence manuelle du contrat destiné au mobile, au dashboard et aux intégrations. Il ne décrit
pas les mécanismes SQL ou les workers internes. Aucun document OpenAPI ni Swagger n’est exposé. L’inventaire
méthode/chemin est comparé automatiquement aux enregistrements Axum ; les corps et autorisations restent couverts
par leurs tests de contrat dédiés.

## Sommaire

- [Conventions communes](#conventions-communes)
- [Routes publiques et intégrations](#routes-publiques-et-integrations)
- [Client mobile](#client-mobile)
- [Administration](#administration)
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

### Authentification et autorisations

| Public | Justificatif | Règle |
| --- | --- | --- |
| Public | Aucun JWT/cookie | Ne dispense pas d’un OTP, d’une signature de webhook ou d’un secret d’enrôlement lorsqu’ils sont requis. |
| Mobile | `Authorization: Bearer <access_token>` | Compte actif, non banni ; CGU et notice courantes requises pour un rôle utilisateur. |
| Admin | Cookie de session WebAuthn `HttpOnly; SameSite=Strict` | Rôle `admin` ou `superadmin`. Aucun JWT mobile accepté, même avec un rôle administratif. |

Les routes mobiles marquées **onboarding incomplet accepté** restent accessibles avant acceptation des textes. Le contrôle global renvoie sinon `403 onboarding_incomplete`. Le sexe et les préférences exigent aussi le consentement sensible, la présence celui de localisation : `403 required_consent_missing`. Un retrait concurrent empêche une requête préalablement validée de réintroduire les données effacées.

Toute mutation authentifiée par session admin exige l’en-tête `Origin` égal à `ADMIN_WEBAUTHN_ORIGIN`. Pour les routes publiques de connexion/enrôlement, l’origine est vérifiée dans la preuve WebAuthn lors de sa validation, pas par ce guard de session. Les actions marquées **récentes** exigent une authentification WebAuthn récente (moins de dix minutes par défaut). Les consultations sensibles demandent un motif et sont auditées.

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

Une limite globale par IP s’ajoute aux limites dédiées indiquées ci-dessous ; leurs valeurs sont les valeurs par défaut configurables. Dépassement global : `rate_limit_exceeded` ; protection Redis indisponible : `rate_limit_unavailable`, sans laisser passer la requête. Les webhooks Stripe et Sweego ont chacun leur propre limite à la place de la limite globale.

### Pagination

Sauf indication contraire, `limit` vaut 20, entre 1 et 100. Les collections paginées exposent `next_cursor` : le réutiliser avec les mêmes filtres, jusqu’à `null`. Ne pas le décoder ni le reconstruire avec les dates ou distances arrondies affichées.

`offset` est déprécié ; seules les routes qui le mentionnent l’acceptent. Avec un curseur, il doit être nul. Les curseurs évitent les décalages d’offset, mais **ne figent pas un instantané** : un nouveau message, un changement de statut ou de position peut modifier l’ordre ou l’éligibilité pendant le parcours. Dédupliquer les identifiants côté client et rafraîchir la liste si nécessaire.

Les listes non paginées ne doivent pas recevoir arbitrairement `limit` ou `cursor`.

### Idempotence et réponses perdues

Il n’existe pas de mécanisme de retry universel.

| Opération | Comportement attendu du client |
| --- | --- |
| Envoi OTP, photo, message, Checkout | Fournir `Idempotency-Key: <UUID v4>` et conserver la même clé et le même contenu pour la même intention. Les fenêtres et conflits sont propres au parcours. |
| Refresh mobile | Un seul appel à la fois. **Pas de retry aveugle** après perte de réponse ; voir le parcours ci-dessous. |
| Consentements, attribution/retrait de trait | Rejouer un état identique ne doit pas dupliquer l’effet. |
| Effacement du compte | Le jeton est à usage unique. Un `202` indique une acceptation durable, pas la fin des suppressions. |
| Revue admin | Relire après conflit de version ou d’état ; ne pas écraser automatiquement la décision d’un autre opérateur. |

<a id="routes-publiques-et-integrations"></a>
## Routes publiques et intégrations

### Connexion mobile et catalogue

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| POST | `/api/auth/otp/send` | `{ phone_number }` + clé d’idempotence → `202 { message }`. |
| POST | `/api/auth/otp/verify` | `{ phone_number, otp }` → `200 { access_token, refresh_token }`. |
| POST | `/api/auth/refresh` | `{ refresh_token }` → `200 { access_token, refresh_token }`. |
| GET | `/api/plans` | `200 { plans: [...] }`. |

**OTP.** Seuls les téléphones français E.164 `+33…` sont acceptés. La vérification consomme le code une seule fois et crée un compte utilisateur si nécessaire. Envoi et vérification : 5/h par IP et numéro pseudonymisé. La validité du code suit `OTP_TTL`.

L’envoi répond `202 { "message": "Verification code request accepted." }` : cela ne prouve pas la livraison du SMS.
Pendant la rétention de la demande, aucun rejeu identique ne déclenche un second appel fournisseur. Une demande
rejetée renvoie `503 otp_delivery_unavailable`. Un timeout, une réponse inexploitable ou une attente dépassant
le timeout fournisseur augmenté de cinq secondes renvoie `503 otp_delivery_unknown`. Un callback signé peut
confirmer le code initial, jamais réactiver un code consommé, expiré ou remplacé. Attendre puis rejouer la même clé ;
un nouvel envoi exige une nouvelle intention explicite, avec les limites OTP habituelles. Une clé réutilisée avec
un autre numéro renvoie `409 idempotency_key_conflict`. Détails : [suivi Sweego](migration/s09-otp-sweego.md).

**Refresh.** Envoyer le dernier refresh sous la forme opaque reçue `jti:secret`, puis enregistrer atomiquement la nouvelle paire de tokens. Le rejeu d’un ancien token authentique non expiré révoque toute sa famille et renvoie `401 invalid_or_expired_refresh_token`. Il n’y a pas de fenêtre de grâce : deux refresh concurrents ou une réponse perdue peuvent imposer une nouvelle connexion OTP. Limite : 30/15 min/IP.

Erreurs utiles : `authentication_required`, `invalid_or_expired_access_token`, `invalid_or_expired_refresh_token`, `invalid_idempotency_key`, `idempotency_key_conflict`, `otp_delivery_unavailable`, `otp_rate_limit_exceeded`, `refresh_rate_limit_exceeded`. Le format cryptographique, les durées et la rotation des clés sont décrits dans [sessions mobiles](migration/s08-mobile-identity.md).

**Plans.** Chaque plan expose code, nom, prix mensuel/annuel, devise, jours d’essai, limite hebdomadaire éventuelle et fonctionnalités. Le client ne choisit jamais les identifiants ou prix Stripe.

### Webhook Stripe

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| POST | `/api/billing/stripe/webhook` | Corps JSON brut + `Stripe-Signature` → `200 { received: true }`. |

La signature est obligatoire, ainsi que la cohérence test/live. Les Event IDs sont dédupliqués ; les événements non pris en charge mais valides sont acquittés sans effet. Limite dédiée : 300/min/IP.

Événements traités : `checkout.session.completed`, `checkout.session.expired`, `customer.subscription.created|updated|deleted|paused|resumed|trial_will_end`, `invoice.paid`, `invoice.payment_failed`, `invoice.payment_action_required`, `invoice.finalization_failed`, `customer.deleted`. Les événements de facture/abonnement plus anciens que l’état connu sont ignorés. Erreurs : `invalid_stripe_signature`, `stripe_mode_mismatch`, `invalid_stripe_event`, `billing_webhook_rate_limit_exceeded`.

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
Configuration et limites fournisseur : [suivi OTP Sweego](migration/s09-otp-sweego.md).

### Santé

| Méthode | Route | Résultat |
| --- | --- | --- |
| GET | `/health/live` | `200 { status: "ok" }` si le processus répond. |
| GET | `/health/ready` | `200 { status: "ready" }` si les dépendances requises répondent ; sinon `503`. |

La readiness vérifie PostgreSQL, le bucket objet et Redis lorsqu’il est requis. Ces routes n’exigent pas d’authentification.

<a id="client-mobile"></a>
## Client mobile

Toutes les routes de cette section exigent le Bearer mobile.

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
| GET | `/api/users/me/consents` | `200 { consents, onboarding_complete, required_actions }`. |
| PUT | `/api/users/me/consents` | `{ consents: [{ consent_type, granted }] }` → même état enrichi. |

Types : `terms_of_service_acceptance`, `privacy_notice_acknowledgement`, `sensitive_data_consent`, `location_consent`. Aucun choix marketing. Chaque type apparaît au plus une fois. Chaque choix retourné indique sa version acceptée éventuelle, `required_document_version` et `document_url` ; le mobile doit présenter ce texte. Aucune preuve technique IP/user-agent n’est exposée et le client ne choisit pas la version.

Les CGU et la notice ne peuvent pas être retirées via cette route : on peut refuser l’onboarding ou supprimer son compte. Retirer le consentement sensible efface sexe/préférences ; retirer la localisation efface la présence. Un rejeu identique est idempotent.

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
  "moderation_status": "pending",
  "moderation_reasons": ["analysis_unavailable"]
}
```

**Rejouer.** Pendant 24 heures, garder la même clé et les mêmes nom/MIME/octets : pas de nouvelle conversion ni écriture objet. Contenu différent : `409 idempotency_key_conflict` ; résultat remplacé/supprimé : `409 idempotency_key_consumed` ; traitement actif : `409 photo_update_in_progress`. L’URL signée expire après 300 secondes ; ce n’est pas un identifiant durable.

**Retirer.** Le `204` rend la photo immédiatement invisible dans les nouvelles lectures ; l’objet est supprimé en arrière-plan. Une panne ultérieure ne rétablit pas la photo. Les URL déjà émises restent soumises à leur expiration ou à la suppression de l’objet.

Autres erreurs : `400 invalid_idempotency_key`, `400 invalid_photo`, `413 photo_too_large`, `404 profile_not_found`, `409 photo_update_conflict`, `429 photo_rate_limit_exceeded`, `503 photo_storage_unavailable`.

**Modération commune.** Statuts `pending | approved | rejected`. Motifs automatiques : `spam, insult, personal_contact, sexual_content, face_not_detected, multiple_faces, blurry, explicit_image, analysis_unavailable, legacy_unreviewed`. Une panne d’analyse entraîne une revue manuelle, jamais une approbation automatique. L’automatisation approuve ou demande une revue, sans rejeter seule. Dans le feed et les matchs, bio, réponses et photo non approuvées restent masquées ; le propriétaire garde la visibilité de son propre état.

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

### Découverte, matchs et messages

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/users/me/discovery-status` | `200 { ready, required_actions, presence_expires_at }`. |
| GET | `/api/feed` | `limit, cursor` → `200 { profiles, next_cursor }`. |
| POST | `/api/swipes` | `{ target_user_id, decision: like ou pass }` → `201 { decision, matched, match? }`. |
| GET | `/api/matches/me` | `limit, cursor` (offset déprécié) → `200 { matches, next_cursor }`. |
| GET | `/api/users/me/continuation-quota` | `200` : plan effectif, usage, `weekly_limit, remaining` pour un plan limité. |
| PATCH | `/api/matches/:id/reveal` | Aucun corps → `200 { message, photos_revealed }`. |
| PATCH | `/api/matches/:id/continue` | Aucun corps → `200 { message, match_confirmed }`. |
| GET | `/api/matches/:id/messages` | `limit, cursor` (offset déprécié) → `200 { messages, next_cursor }`. |
| POST | `/api/matches/:id/messages` | `{ content }` + clé d’idempotence → `201` avec le message stable. |
| PATCH | `/api/matches/:id/messages/read` | `{ read_through_message_id }` → `200 { updated_count, read_through_message_id }`. |
| PATCH | `/api/matches/:id/messages/:msgId/read` | Aucun corps → `200 { message: "message marked as read" }`. |

**Découverte.** Demandeur et candidats doivent être actifs, avec sexe, préférences, consentements sensible/localisation courants et position de moins d’une heure. Les actions de préparation possibles sont `profile, sex, preferences, sensitive_data_consent, location_consent, fresh_presence`. Les critères âge/sexe/distance s’appliquent dans les deux sens. Blocages, matchs existants et profils déjà swipés sont exclus. Le feed expose `user_id`, prénom, âge, sexe, distance arrondie au dixième, traits, bio et réponses approuvées. Limites : feed 60/min/utilisateur ; swipe 120/min/utilisateur.

Une décision de swipe est immuable pendant sa rétention. Deux likes réciproques créent un seul match ; aucun endpoint ne permet de fabriquer directement un match. Erreurs : `409 discovery_not_ready`, `404 discovery_candidate_not_found`, `409 swipe_already_recorded`, `400 invalid_cursor`, `429 swipe_rate_limit_exceeded`, `429 feed_rate_limit_exceeded`, `503 discovery_unavailable`.

**Matchs.** La liste est triée par activité. Chaque résumé contient l’autre utilisateur (prénom, âge, sexe, bio, traits, `profile_answers`, photo conditionnelle), `my_revealed, photos_revealed, my_continued, unread_count, last_message`. La photo reste `null` avant révélation mutuelle, même approuvée. Les routes mobiles d’un match exigent d’en être participant.

Le match est `active` pendant 24 h, puis `awaiting_continuation` ; les deux accords le rendent `confirmed`, sinon il devient `expired` après la seconde fenêtre. Le quota de l’initiateur est débité au second accord, sans double consommation concurrente ; une limite nulle n’accorde aucune continuation. Une attente de verrou ne prolonge pas l’échéance. Erreurs : `404 match_not_found`, `409 continuation_not_available_yet`, `409 invalid_match_state`, `409 messaging_not_available`, `410 match_expired`, `403 continuation_quota_reached`.

**Messages.** Contenu non vide, 2 000 caractères maximum ; limite 60/min/utilisateur. Rejouer clé/match/contenu identiques renvoie le message existant sans nouvelle notification ; réutiliser la clé pour une autre requête renvoie `409 idempotency_key_conflict`. La lecture par borne marque tous les messages reçus jusqu’à la borne incluse. Un expéditeur ne peut pas marquer son propre message comme lu.

### Appareils et temps réel

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/users/me/devices` | `200 { devices: [...] }`, sans tokens FCM. |
| POST | `/api/users/me/devices` | `{ push_token, platform: ios ou android, app_version? }` → `201` avec l’appareil public. |
| DELETE | `/api/users/me/devices/:id` | UUID d’un appareil du compte → `204` ; `404 device_not_found`. |
| GET | `/api/users/me/events` | Flux SSE authentifié `text/event-stream`. |

L’appareil expose UUID, `session_id`, plateforme, version et dates d’usage ; la session vient du Bearer, pas du corps client. Un token fournisseur déjà connu est réaffecté/rafraîchi de façon idempotente. Une ancienne liaison peut avoir `session_id: null`.

SSE envoie `connected`, un heartbeat toutes les 25 secondes, puis `match.created, match.updated, matches.invalidated, message.created, message.read, subscription.updated`. Le flux ferme à expiration du JWT ou après échec/révocation détecté lors du contrôle de session toutes les 25 secondes. Le client SSE doit pouvoir envoyer le Bearer.

**Après reconnexion, relire les ressources : SSE n’a pas de replay hors ligne.** Le push optionnel n’embarque jamais le texte privé d’un message. Dédupliquer son `notification_id` stable : une réponse fournisseur perdue peut provoquer un doublon externe. Les alertes de paiement/essai devenues obsolètes ne sont pas envoyées. Avec le push désactivé, les tâches consommées ne seront pas rattrapées après activation. Voir [garanties de livraison](migration/s22-sse-push.md).

### Abonnement

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/users/me/subscription` | `200` avec l’état d’abonnement public. |
| POST | `/api/users/me/subscription/checkout` | `{ billing_period: monthly ou annual }` + clé d’idempotence → `201 { session_id, url, expires_at }`. |
| POST | `/api/users/me/subscription/portal` | Aucun corps → `201 { url }`. |

L’état expose `plan, provider, status, access_granted, billing_period`, dates de période/essai/annulation, `cancel_at_period_end, customer_portal_available`. `trialing, active, past_due` ouvrent l’accès uniquement pendant la période connue de l’API.

**Checkout.** Utiliser l’URL retournée (validité 30 minutes). Une seule session peut être créée/ouverte par utilisateur. Le premier abonnement peut bénéficier de l’essai du catalogue ; un essai consommé n’est pas réattribué. Le retour sur l’URL de succès **ne prouve pas à lui seul l’activation Premium** : relire l’abonnement, mis à jour par webhook signé. Ne jamais envoyer Product ID, Price ID, montant, devise, durée d’essai ou Customer ID. Le portail exige un client Stripe déjà lié. Checkout et portail : 10/min/utilisateur. Tant qu’une création Customer reste incertaine, une nouvelle clé est refusée ; seule la clé d’origine peut rejouer le `POST` dans la fenêtre sûre de 23 heures, puis la réconciliation procède uniquement en lecture.

Erreurs : `billing_unavailable`, `stripe_request_failed`, `subscription_already_active`, `checkout_already_in_progress`, `billing_customer_reconciliation_required`, `idempotency_key_reused`, `idempotency_key_consumed`, `billing_customer_not_found`, `billing_rate_limit_exceeded`.

### Droits, effacement et sûreté

Onboarding incomplet accepté pour les jetons/effacement, demandes RGPD et export, mais pas pour les blocages/signalements.

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| POST | `/api/users/me/deletion-token` | Aucun corps → `201 { confirmation_token, expires_at }`. |
| DELETE | `/api/users/me` | `{ confirmation_token }` → `202 { request_id, status: "in_progress" }`. |
| POST | `/api/users/me/data-subject-requests` | `{ type }` → `201` avec la demande. |
| GET | `/api/users/me/data-subject-requests` | `200 { requests: [...] }`. |
| GET | `/api/users/me/data-export` | `200 application/json` en pièce jointe avec l’export portable du demandeur. |
| GET | `/api/users/me/blocks` | `200 { blocks: [...] }`, toujours `photo: null`. |
| POST | `/api/users/me/blocks/:userId` | UUID → `204`, clôt les matchs entre les comptes et empêche leur recréation. |
| DELETE | `/api/users/me/blocks/:userId` | UUID → `204`. |
| POST | `/api/reports` | `{ reported_user_id, match_id?, reason, description? }` → `201` avec le signalement. |

**Effacement.** Demander le jeton dédié, puis le transmettre comme reçu (`uuid:secret`). Il remplace le précédent et expire après dix minutes par défaut (`ACCOUNT_DELETION_TOKEN_TTL`, 1–30 min). Exemple de réponse au DELETE :

```json
{
  "request_id": "11111111-1111-4111-8111-111111111111",
  "status": "in_progress"
}
```

Le `202` désactive immédiatement le compte ; fermer la session mobile. **Ne pas afficher que toutes les données ont déjà été supprimées.** Le nettoyage continue en arrière-plan malgré une panne externe. Jeton invalide/expiré : `401 invalid_or_expired_deletion_token`. Si la réponse est perdue après acceptation, le Bearer devient invalide : un retry n’est pas une route publique de suivi et ne rend pas forcément le même `202`. Voir [effacement et limites de reprise](migration/s25-account-erasure.md).

**Droits.** Types de demande : `access | erasure | portability | rectification | restriction | objection` ; une seule demande ouverte par type/utilisateur. L’export contient profil/réponses, abonnement/factures liés, métadonnées des sessions sans secrets et uniquement les décisions de swipe sortantes. Il n’expose jamais les décisions entrantes d’autrui. Le JSON est préparé par pages dans un fichier temporaire privé, transmis comme flux puis supprimé. Toutes ses données PostgreSQL, swipes compris, partagent un instantané `REPEATABLE READ`. Limite 5/h/utilisateur : `429 data_export_rate_limit_exceeded` ; export au-delà de la borne configurée : `413 data_export_too_large` ; toutes les places de préparation occupées : `503 data_export_busy` avec `Retry-After` ; source indisponible avant envoi : `503 data_export_unavailable`. L’accès réussi est journalisé.

**Signalements.** `reason = inappropriate_content | fake_profile | harassment | spam | other` ; description au plus 2 000 octets ; `match_id` et description peuvent être nuls. Auto-signalement interdit, cible existante et match éventuel reliant les deux comptes. Limite 5/h/utilisateur.

<a id="administration"></a>
## Administration

### Entrée WebAuthn

Ces quatre routes d’entrée ne demandent pas de session existante. La preuve WebAuthn est vérifiée contre l’origine admin ; bootstrap exige en plus le secret d’enrôlement. Limite : 10 requêtes/5 min/IP par défaut. Aucun SSO ni fournisseur d’identité externe.

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| POST | `/api/admin/auth/login/options` | Aucun corps → `200 { challenge_id, options }`. |
| POST | `/api/admin/auth/login/verify` | `{ challenge_id, credential }` → `200` avec session publique et cookie. |
| POST | `/api/admin/auth/bootstrap/options` | `{ bootstrap_token }` → `200 { challenge_id, options }`. |
| POST | `/api/admin/auth/bootstrap/verify` | `{ bootstrap_token, challenge_id, credential, name }` → `201` avec session et cookie. |

En développement, ouvrir **`http://localhost:5173`**, RP ID `localhost`, via le proxy même-origine `/api` du dashboard ; pas `127.0.0.1`. En production : origine HTTPS exacte, cookie `Secure` préfixé `__Host-`. Le RP ID doit correspondre au domaine configuré.

Transmettre les options à l’API WebAuthn du navigateur et renvoyer le credential sérialisé. Passkey découvrable et vérification utilisateur obligatoires. Challenge valable cinq minutes, à usage unique. La session publique expose `user_id, role, authenticated_at, expires_at`, jamais son secret. Bootstrap, créé hors bande, expire après quinze minutes par défaut. Procédure d’enrôlement : [README](migration/s10-admin-auth.md).

### Sessions et passkeys

Toutes les routes suivantes exigent la session admin. Expiration inactive de 30 minutes et absolue de huit heures par défaut.

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/admin/auth/session` | Session courante après vérification du compte et de la passkey. |
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
| GET | `/api/admin/users/:id` | UUID + query `reason` obligatoire → détail administratif audité. |
| PATCH | `/api/admin/users/:id/status` | `{ is_banned, reason? }` → bannissement/débannissement. Motif requis pour bannir. |
| GET | `/api/matches/:userId` | UUID utilisateur + `reason, limit, cursor` (offset déprécié) → `200 { matches, next_cursor }`. **Route admin malgré son chemin.** |
| GET | `/api/admin/matches/:id/messages` | UUID match + `reason, limit, cursor` (offset déprécié) → conversation auditée. |
| GET | `/api/admin/reports` | `status?, limit, cursor` (offset déprécié) → `200 { reports, next_cursor }`. |
| PATCH | `/api/admin/reports/:id` | `{ status }` → `200 { message: "report updated" }` ; `404 report_not_found`. |
| GET | `/api/admin/data-subject-requests` | `status?, limit, cursor` (offset déprécié) → `200 { requests, next_cursor }`, avec progression `erasure` éventuelle. |
| PATCH | `/api/admin/data-subject-requests/:id` | **Récente** ; `{ status, notes? }` → transition contrôlée et auditée. |
| GET | `/api/admin/data-access-logs` | `user_id, limit, cursor` (offset déprécié) → `200 { logs, next_cursor }`. |

Recherche utilisateurs : prénom ou UUID exact ; `status = active | banned`, `role = user | admin | superadmin`. La liste ne signe aucune photo (`photo: null`). Le détail expose compte, profil, préférences, traits, consentements et fraîcheur de présence ; jamais téléphone, empreinte ou coordonnées précises. Les consultations sensibles requièrent un motif de 3–500 caractères ; une conversation est auditée pour les deux participants.

Un bannissement invalide les sessions mobiles. Un admin n’agit que sur un rôle utilisateur ; un superadmin ne peut agir ni sur lui-même ni sur un autre superadmin. Statuts de signalement : `pending | reviewed | dismissed`.

Pour une demande RGPD, la mutation accepte `in_progress | completed | rejected`. Sur un effacement en cours, demander `completed` **programme** le workflow et répond `200 { "message": "account erasure scheduled" }` ; la demande reste `in_progress` jusqu’à réussite réelle. Le rejeu ne duplique pas le travail. Rejet d’un effacement commencé : `409 invalid_data_request_transition`. Autres transitions : `200 { "message": "data subject request updated" }`.

`erasure` peut être nul ; sinon il expose `step, updated_at, event_id, status, attempts, last_error_code`. Les étapes sont `stripe | photos | swipes | postgres | completed`. Après purge d’un événement résolu, `event_id/status` peuvent être nuls. Aucun payload, identifiant Stripe, clé objet ou URL.

### Catalogue et modération

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| POST | `/api/admin/traits` | `{ name }` → `201 { id, name }`. |
| PATCH | `/api/admin/traits/:id` | `{ name }` → `200 { message: "trait updated" }`. |
| DELETE | `/api/admin/traits/:id` | UUID → `204`, suppression définitive. |
| GET | `/api/admin/profile-questions` | `200 { questions: [...] }`, avec dates et `answer_count`. |
| POST | `/api/admin/profile-questions` | `{ prompt, category, display_order? }` → `201` avec la question. |
| PATCH | `/api/admin/profile-questions/:id` | Au moins un de ces trois champs → `200` avec la question. |
| DELETE | `/api/admin/profile-questions/:id` | UUID → `204`, supprime aussi toutes ses réponses. |
| GET | `/api/admin/content-moderation` | `status?, content_type?, limit, cursor` (offset déprécié) → `200 { cases, next_cursor }`. |
| GET | `/api/admin/content-moderation/:id` | UUID + query `reason` (3–500 caractères) → contenu audité. |
| PATCH | `/api/admin/content-moderation/:id` | `{ version, decision, reason, photo_checks? }` → `200 { message: "content moderation decision recorded" }`. |

Traits : nom non vide, au plus 100 octets ; doublon `409 trait_already_exists`. Questions : catégories `daily_life | personality | interests | relationships | conversation`, ordre par défaut 100 ; doublon de libellé insensible à la casse : `409 profile_question_already_exists`. Modifier le libellé affecte aussi les réponses existantes. **Avant suppression, afficher `answer_count` et demander confirmation explicite** : les réponses ne sont pas conservées.

**Revoir un contenu.** Filtrer par `status = pending | approved | rejected` et `content_type = photo | bio | profile_answer`. La liste expose identifiants, utilisateur/prénom, statut/motifs, versions de politique et de revue, signaux/contrôles photo et dates ; pas le texte, la question, la clé objet, l’URL ou l’image. Ouvrir le détail avec un motif fournit `content, question, photo` selon le type ; toute consultation est auditée avant signature photo.

La décision est `approved | rejected`, avec la `version` lue au préalable. Une photo exige les trois booléens `face_detectable, sharp_enough, content_allowed` : tous vrais pour approuver, au moins un faux pour rejeter. Un rejet retire la photo et programme sa suppression. En cas de `409 moderation_case_stale`, recharger plutôt que réappliquer aveuglément. Autres erreurs : `400 invalid_moderation_case_id`, `400 invalid_moderation_request`, `404 moderation_case_not_found`, `409 moderation_review_not_allowed`.

### Exploitation et reprises

| Méthode | Route | Entrée → résultat |
| --- | --- | --- |
| GET | `/api/admin/metrics` | `revenue_period?` → synthèse métier et `operations`. |
| GET | `/api/admin/revenue` | `revenue_period?` → estimation de chiffre d’affaires. |
| GET | `/api/admin/photo-reconciliation` | `status?, limit, cursor` (offset déprécié) → traitements photo à réconcilier. |
| POST | `/api/admin/photo-reconciliation/:id/retry` | UUID photo + `{ reason }` (3–500 caractères) → `202`. |
| GET | `/api/admin/billing-reconciliation` | `kind?, limit, cursor` → dead letters Stripe `200 { events, next_cursor }`. |
| GET | `/api/admin/outbox/dead-letters` | `limit, cursor` → `200 { events, next_cursor }`. |
| POST | `/api/admin/outbox/:id/retry` | **Récente** ; `{ reason }` → `202`, relance auditée. |
| POST | `/api/admin/outbox/:id/discard` | **Récente** ; `{ reason }` → `204`, abandon audité lorsque permis. |

`revenue_period = last_7_days | last_30_days | month_to_date | previous_month | year_to_date | all_time`, défaut `month_to_date`. Le revenu est une estimation : abonnements Premium mis à jour sur la période × tarif mensuel actuel ; **ni encaissements ni bénéfice comptable**.

`operations` expose latences/compteurs HTTP et `401/403/429/5xx`, mémoire/event loop, résultats des dépendances, pool, outbox et maintenances. Les mesures du processus repartent à zéro au redémarrage ; les états outbox/maintenance sont persistants. Chaque maintenance fournit notamment `processed_count, batch_count, duration_ms, work_remaining` ; ce dernier signale qu’une passe a consommé son budget borné et doit être reprise. `operations.outbox.notification_push` détaille `pending, processing, completed, dead_letter, discarded, oldest_pending_at` ; `operations.outbox.billing_reconciliation` fournit les mêmes états utiles sans `discarded`. `completed` signifie tâche acquittée encore conservée, pas réception par un terminal ni validation d’un paiement. Voir [volumes et exports](migration/s24-dsr-data-export.md).

L’export Prometheus n’est pas une route de ce contrat : lorsqu’il est activé, il écoute sur un serveur privé
distinct, authentifié par bearer token. Voir [supervision et runbooks](observability.md).

`operations.sms_delivery` expose `states` (`pending, accepted, sent, failed, unknown`), `awaiting_callback`,
`oldest_unresolved_age_seconds`, `average_acceptance_ms`, `average_sent_callback_ms`, `average_failure_ms`,
`webhook_enabled`, `handset_delivery: "not_confirmed"`, `retention: "otp_expiry"` et `callbacks`
(`applied, ignored, conflict, invalid_signature, invalid_event, unavailable, disabled`). Les états et délais
portent sur les OTP non expirés, pas sur un historique complet ; les compteurs de callbacks sont locaux au processus.

Réconciliation photo : filtre `all | stale_processing | deleting | dead_letter` (défaut `all`). UUID photo/utilisateur, métadonnées techniques, diagnostics et état outbox uniquement ; aucune image ni clé objet. Une photo prête ou un traitement récent refuse la relance : `409 photo_reconciliation_not_allowed` ; worker actif : `409 photo_reconciliation_in_progress` ; photo absente : `404 photo_not_found`.

Réconciliation Stripe : la liste ne contient que les dead letters qui exigent une action humaine ; la file normale reste agrégée dans `operations`. `kind = all | subscription | customer_creation`. Elle expose UUID d’événement/utilisateur, type, tentatives, code d’erreur normalisé et dates ; jamais payload, identifiant fournisseur ou moyen de paiement. Une dead letter peut être relancée par la route outbox commune, après authentification récente, motif et audit. La relance effectue une nouvelle lecture et n’ordonne aucun paiement. Voir [protocole Stripe](migration/s21-stripe-webhooks.md).

Les dead letters exposent type, tentatives et code normalisé, jamais payload/agrégat/clé objet. Une décision devenue obsolète renvoie `409 outbox_event_not_dead_letter`. L’abandon de `account.erase` et des événements `billing.*` est toujours interdit ; celui de `photo.delete` est interdit tant que sa trace existe : `409 outbox_discard_not_allowed`. `notification.push` peut être abandonné sans effacer la notification. Un `202` de reprise ne garantit pas que la dépendance sera disponible lors du prochain essai.

<a id="entretien-du-contrat"></a>
## Entretien du contrat

Modifier ce guide avec toute évolution de route, DTO, réponse, autorisation ou règle de rejeu. Garder les chemins complets dans les tableaux, une seule ligne par couple méthode/chemin.

Le test `tests/route_inventory.rs` compare ces lignes aux déclarations de routes présentes dans les sources Axum. Il détecte les couples méthode/chemin absents ou non documentés ; il ne vérifie pas le montage du routeur final ni les doublons éliminés par son ensemble de comparaison. Les alias HEAD automatiques et le preflight CORS généré ne font pas partie de cet inventaire applicatif.

Ce contrôle ne prouve ni les schémas JSON, ni les droits, ni les garanties métier : les autres tests de contrat et intégrations restent nécessaires. Voir [guide de validation](../tests/README.md), [responsabilités internes](architecture.md) et [validations restantes](migration/s29-cutover.md).
