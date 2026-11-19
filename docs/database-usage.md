# Utilisation des tables PostgreSQL

[Index](README.md) · [Schéma et migrations](database.md)

Audit du code du 19 novembre 2026 : les **64 tables applicatives déclarées dans
`db/001_schema_postgres.sql` ont chacune un accès SQL dans le code exécuté**. Les points
ci-dessous excluent les tests, les migrations et les simples références de clés étrangères.
Aucune table applicative sans usage n’a été identifiée.

Cet inventaire décrit les chemins du code ; il ne mesure pas leur fréquence en production.
Une table peut rester vide tant que la fonctionnalité correspondante n’est pas utilisée.
Les jobs de rétention, les projections SQL et les triggers apportent des usages supplémentaires.

| Table | Utilisation | Point d’entrée SQL représentatif |
| --- | --- | --- |
| `user_account` | Identité, rôles, bannissement, pause et cycle de vie du compte. | [identity/mobile/account.rs](../src/identity/mobile/account.rs) |
| `swipe_decision` | Likes/passes, exclusions du feed et rétention des décisions. | [discovery/pg.rs](../src/discovery/pg.rs) |
| `account_tombstone` | Empreintes des comptes supprimés, contrôlées avant une réinscription. | [identity/mobile/account.rs](../src/identity/mobile/account.rs) |
| `sms_send_control` | Budgets SMS et suspension des envois, pilotables par l’administration. | [identity/mobile/sms_safety/pg.rs](../src/identity/mobile/sms_safety/pg.rs) |
| `sms_send_attempt` | Réservations d’envoi et comptabilité des plafonds SMS. | [identity/mobile/sms_safety/pg.rs](../src/identity/mobile/sms_safety/pg.rs) |
| `mobile_otp_request` | Intentions OTP durables, idempotence et traitement par le worker. | [identity/mobile/otp/requests/pg.rs](../src/identity/mobile/otp/requests/pg.rs) |
| `otp_verification` | Vérification et consommation des codes, suivi de livraison Sweego. | [identity/mobile/otp/pg.rs](../src/identity/mobile/otp/pg.rs) |
| `refresh_token_family` | Sessions mobiles, révocation et échéances de session. | [identity/mobile/pg.rs](../src/identity/mobile/pg.rs) |
| `refresh_tokens` | Rotation des refresh tokens, détection de rejeu et récupération. | [identity/mobile/pg/rotation.rs](../src/identity/mobile/pg/rotation.rs) |
| `device_token` | Appareils destinataires des notifications et association aux sessions. | [notifications/pg.rs](../src/notifications/pg.rs) |
| `user_profile` | Profil personnel, lectures publiques autorisées et modifications. | [profiles/pg.rs](../src/profiles/pg.rs) |
| `user_preferences` | Critères de recherche et filtrage du feed. | [profiles/pg.rs](../src/profiles/pg.rs) |
| `user_presence` | Présence et localisation minimisée utilisées pour le feed géographique. | [discovery/pg.rs](../src/discovery/pg.rs) |
| `user_consent` | Consentements, versions juridiques et autorisations d’accès. | [profiles/pg.rs](../src/profiles/pg.rs) |
| `trait` | Catalogue des traits proposés aux profils. | [catalog/pg.rs](../src/catalog/pg.rs) |
| `user_trait` | Traits sélectionnés par l’utilisateur et projection du profil. | [catalog/pg.rs](../src/catalog/pg.rs) |
| `profile_question` | Catalogue des questions de profil. | [catalog/pg.rs](../src/catalog/pg.rs) |
| `user_profile_answer` | Réponses personnelles et projection du profil. | [catalog/pg.rs](../src/catalog/pg.rs) |
| `user_photo` | Photos du profil, stockage privé, présentation et suppression. | [media/pg.rs](../src/media/pg.rs) |
| `photo_upload_request` | Idempotence, état et réconciliation des uploads synchrones. | [media/pg.rs](../src/media/pg.rs) |
| `content_moderation_case` | Modération manuelle des contenus et visibilité des photos. | [moderation/pg.rs](../src/moderation/pg.rs) |
| `subscription_plan` | Catalogue des formules et paramètres des droits. | [catalog/pg.rs](../src/catalog/pg.rs) |
| `subscription_plan_feature` | Avantages des formules exposés dans le catalogue. | [catalog/pg.rs](../src/catalog/pg.rs) |
| `user_subscription` | Attributions internes conservées, projection des droits et verrous de continuation. | [matches/pg/continuation.rs](../src/matches/pg/continuation.rs) |
| `complimentary_premium_grant` | Premium offert ou retiré par le superadmin. | [administration/pg/premium.rs](../src/administration/pg/premium.rs) |
| `billing_store_account` | Liaison d’un compte Histae avec ses achats Apple/Google. | [billing/stores/pg.rs](../src/billing/stores/pg.rs) |
| `billing_store_subscription` | Preuves, état et réconciliation des abonnements natifs. | [billing/stores/pg.rs](../src/billing/stores/pg.rs) |
| `billing_store_notification` | Réception durable et déduplication des événements Apple/Google. | [billing/stores/pg.rs](../src/billing/stores/pg.rs) |
| `billing_store_finance` | Import des rapports financiers et calcul des revenus administratifs. | [billing/stores/administration/pg.rs](../src/billing/stores/administration/pg.rs) |
| `web_account_challenge` | OTP du site lié à la paire UUID/téléphone. | [identity/web/pg.rs](../src/identity/web/pg.rs) |
| `web_account_session` | Session privée du site pour les actions sur les données personnelles. | [identity/web/pg.rs](../src/identity/web/pg.rs) |
| `match_init` | Cycle de vie du match, continuation et échéances d’inactivité. | [matches/pg/matches.rs](../src/matches/pg/matches.rs) |
| `match_state` | Choix et consentements propres à chaque participant. | [matches/pg/continuation.rs](../src/matches/pg/continuation.rs) |
| `user_notification_preference` | Préférences générales d’envoi des notifications. | [notifications/preferences/queries.rs](../src/notifications/preferences/queries.rs) |
| `match_notification_preference` | Mise en sourdine d’une conversation et contrôle des destinataires. | [notifications/preferences/queries.rs](../src/notifications/preferences/queries.rs) |
| `chat_message` | Messages, pagination, lecture et effacement des conversations. | [matches/pg/messages.rs](../src/matches/pg/messages.rs) |
| `continuation_usage` | Décompte hebdomadaire des continuations et sources consommées. | [matches/pg/credits.rs](../src/matches/pg/credits.rs) |
| `continuation_wallet` | Solde et renouvellement des crédits publicitaires. | [matches/pg/credits.rs](../src/matches/pg/credits.rs) |
| `continuation_reservation` | Réservation et consommation atomiques des crédits lors de l’accord mutuel. | [matches/pg/credits.rs](../src/matches/pg/credits.rs) |
| `advertising_reward_intent` | Intentions publicitaires, preuves SSV et crédit du portefeuille. | [advertising/pg.rs](../src/advertising/pg.rs) |
| `user_block` | Blocages entre utilisateurs et exclusions des interactions. | [privacy/pg.rs](../src/privacy/pg.rs) |
| `user_report` | Signalements et traitement administratif. | [reports/pg.rs](../src/reports/pg.rs) |
| `notification` | Notifications persistées et déduplication de leur création. | [notifications/enqueue.rs](../src/notifications/enqueue.rs) |
| `outbox_event` | File durable des effets externes, reprises et dead letters. | [outbox/pg.rs](../src/outbox/pg.rs) |
| `notification_push_delivery` | Références d’envoi par appareil, liées au job et à la notification. | [notifications/delivery/pg.rs](../src/notifications/delivery/pg.rs) |
| `outbox_operator_action` | Audit des relances et abandons décidés par un administrateur. | [outbox/admin/pg.rs](../src/outbox/admin/pg.rs) |
| `maintenance_job_status` | Dernier résultat et suivi des traitements bornés. | [operations/maintenance.rs](../src/operations/maintenance.rs) |
| `data_subject_request` | Demandes RGPD et traitement des réponses. | [privacy/rights/pg.rs](../src/privacy/rights/pg.rs) |
| `account_erasure` | Progression et reprise de l’effacement d’un compte. | [privacy/erasure/pg.rs](../src/privacy/erasure/pg.rs) |
| `account_deletion_token` | Jetons à usage unique pour confirmer la suppression du compte. | [privacy/erasure/pg.rs](../src/privacy/erasure/pg.rs) |
| `data_access_log` | Audit des consultations et mutations administratives sensibles. | [administration/pg.rs](../src/administration/pg.rs) |
| `admin_webauthn_credential` | Clés WebAuthn des administrateurs. | [identity/admin/pg.rs](../src/identity/admin/pg.rs) |
| `admin_session` | Sessions administratives et authentification récente. | [identity/admin/pg.rs](../src/identity/admin/pg.rs) |
| `admin_webauthn_bootstrap` | Enrôlement initial des clés administratives. | [identity/admin/pg.rs](../src/identity/admin/pg.rs) |
| `admin_webauthn_challenge` | Challenges d’inscription et d’authentification WebAuthn. | [identity/admin/pg.rs](../src/identity/admin/pg.rs) |
| `admin_auth_event` | Journal des accès administratifs et rétention. | [identity/admin/pg.rs](../src/identity/admin/pg.rs) |
| `trait_translation` | Traductions du catalogue de traits. | [localization/pg.rs](../src/localization/pg.rs) |
| `profile_question_translation` | Traductions des questions de profil. | [localization/pg.rs](../src/localization/pg.rs) |
| `subscription_plan_translation` | Traductions des formules. | [localization/pg.rs](../src/localization/pg.rs) |
| `subscription_plan_feature_translation` | Traductions des avantages et descriptions des formules. | [localization/pg.rs](../src/localization/pg.rs) |
| `phone_blocklist_control` | Empreinte de contrôle de la clé HMAC de la politique SMS. | [identity/mobile/phone_blocklist/queries.rs](../src/identity/mobile/phone_blocklist/queries.rs) |
| `phone_blocklist_entry` | Empreintes des numéros et exceptions décidées par les administrateurs. | [identity/mobile/phone_blocklist/queries.rs](../src/identity/mobile/phone_blocklist/queries.rs) |
| `phone_blocklist_source` | Fraîcheur, état et résultats des sources de numéros publics. | [identity/mobile/phone_blocklist/queries.rs](../src/identity/mobile/phone_blocklist/queries.rs) |
| `phone_blocklist_observation` | Observations importées par source, conservées séparément des décisions manuelles. | [identity/mobile/phone_blocklist/queries.rs](../src/identity/mobile/phone_blocklist/queries.rs) |

`user_subscription` reste utile après le retrait de Stripe : la projection SQL
`effective_subscription_at` lit les attributions internes, et la continuation verrouille ces lignes.
Les achats natifs et les Premium offerts utilisent leurs tables distinctes. Une absence d’écriture
par une route actuelle ne suffit donc pas à considérer cette table comme inutilisée.

PostGIS ajoute sa table d’extension `spatial_ref_sys`, utilisée pour les référentiels spatiaux.
Elle porte le total à **65 tables dans `public`** sur une installation standard ; elle ne fait pas
partie des 64 tables applicatives et doit rester gérée par PostGIS.

Les tables Stripe et le ledger `schema_migrations` ont été retirés du schéma applicatif.
Les index et colonnes demandent leur propre analyse d’usage ; cet inventaire concerne les tables.
