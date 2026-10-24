# Tester l’API avec Postman

[Documentation](../docs/README.md) · [Contrat HTTP](../docs/http-contract.md) · [Administration](../docs/administration.md)

## Import

Importer l’un des fichiers JSON avec **Import → Files** :

- [Collection française](postman_collection_fr.json).
- [Collection anglaise](postman_collection_en.json).

Chaque collection contient 131 requêtes correspondant au contrat HTTP. Aucun script Python ou générateur
n’est nécessaire pour l’import. Les collections incluent déjà quelques actions après réponse pour stocker
les tokens et challenges ; elles ne fabriquent ni preuve WebAuthn ni signature de fournisseur.

Exécuter les requêtes choisies une par une. Certaines modifient/suppriment des données, envoient des SMS ou
créent des opérations Stripe : les titres marqués `⚠️` signalent ces effets. Ne pas lancer toute la collection
avec Collection Runner sur une base utilisée.

## Variables

Configurer l’onglet **Variables** de la collection importée :

| Variable | Valeur / usage |
| --- | --- |
| `language` | `fr`, `en`, `es` ou `it` ; en-tête `Accept-Language` sur les réponses localisées ; désactiver cet en-tête pour tester la préférence du compte |
| `baseUrl` | `http://127.0.0.1:8080` sur le serveur, `http://<IP_LAN_SERVEUR>:8080` sur le LAN, ou domaine HTTPS API ; sans `/api` ni slash final |
| `phoneNumber`, `otp` | Téléphone français E.164 `+33…` et code reçu |
| `accessToken`, `refreshToken` | Remplis après vérification OTP ou refresh réussi |
| `adminCookie` | Cookie admin complet `nom=valeur`, issu d’une session WebAuthn valide |
| `adminOrigin` | Origine exacte du dashboard, par exemple `https://dashboard.histae.com` |
| `bootstrapToken`, `challengeId` | Jeton émis par le serveur et challenge à usage unique ; ne sont pas des credentials WebAuthn |
| `targetUserId`, `matchId`, `photoId`, etc. | UUID réels du jeu de données testé ; remplir uniquement ceux requis par la requête |
| `auditReason` | Motif de consultation/action ; facultatif pour le superadmin selon le contrat |
| `continuationCreditCost` | Coût maximal accepté : `1` pour Free, `0` pour Premium ; lire le quota avant le clic |
| `rewardPlatform`, `rewardId` | `android` ou `ios`, puis UUID de l’intention publicitaire préparée ; aucun UUID de compte transmis à AdMob |
| `admobSignedQuery` | Query complète du callback SSV réel, sans `?`, ordre et encodage inchangés ; une chaîne fabriquée ne donne aucun crédit |
| `pageSize` | Taille de page ; ne s’applique qu’aux routes paginées |
| Variables de webhook | En-têtes/signatures valides obtenus du fournisseur ou d’une fixture de test |

Un environnement Postman peut masquer une variable de collection portant le même nom : vérifier les valeurs
résolues avant l’envoi. Après les essais, supprimer les tokens/cookies/jetons des variables avant export ou partage.

## Parcours mobile conseillé

1. Tester `/health/live`, puis `/health/ready`.
2. Envoyer l’OTP, vérifier le code et obtenir les deux tokens.
3. Définir la préférence avec `PATCH /api/users/me/language`, puis lire les documents de consentement puis accepter leurs versions courantes.
4. Compléter le profil et les consentements sensible/localisation avant les données concernées.
5. Tester catalogues, photo, feed, swipes, matchs et messages avec plusieurs comptes fictifs.
6. Tester ensuite les erreurs, révocations et effacements sur des comptes jetables.

### OTP et idempotence

La collection utilise `{{$guid}}` pour une nouvelle intention d’envoi. Le mobile reçoit un `202`
générique avec `resend_after_seconds` après mise en file durable. Le worker outbox doit tourner.
Un numéro déjà connu en inscription (`registration: true`), ou inconnu en connexion, ne reçoit aucun
SMS et garde la même réponse publique. La vérification reste nécessaire pour ouvrir une session.

Après une réponse perdue, reprendre l’UUID réellement envoyé dans la Console Postman et le mettre
explicitement dans `Idempotency-Key`, avec les mêmes téléphone, langue et parcours. Ce rejeu ne
déclenche aucun second SMS et ne prolonge pas le délai. Un nouvel UUID avant le délai partagé
renvoie `429 otp_resend_too_soon` et `Retry-After`. Ne pas laisser `{{$guid}}` changer la clé à chaque
retry incertain. Pour un vrai renvoi, attendre le délai puis créer une nouvelle intention explicite.

Après `logout-all`, refaire une connexion avec un nouvel OTP ; le logout n’interdit pas un nouvel envoi.
Le refresh consomme le dernier token et le remplace : ne pas exécuter deux refresh concurrents ni rejouer
aveuglément une réponse perdue. Voir [Idempotence](../docs/http-contract.md#idempotence-et-reponses-perdues).

## Parcours administrateur

Enrôler puis connecter la passkey dans le navigateur selon [Administration](../docs/administration.md).
Pour les requêtes Postman, renseigner localement le cookie de cette session et `adminOrigin` ; le nom du
cookie est `__Host-histae_admin_session` en HTTPS et `histae_admin_session` en HTTP local.
Les actions sensibles peuvent demander une connexion WebAuthn récente.

Les requêtes d’options/verify sont documentées pour intégrer WebAuthn ; Postman seul ne remplace pas
la cérémonie du navigateur. Un JWT mobile ne donne aucun droit sur `/api/admin`.
Une réponse `401` sur les traits admin doit être diagnostiquée avec le cookie, pas avec le rôle du JWT.

## Photos, webhooks et effets réels

- Photo : sélectionner un vrai fichier dans le corps multipart ; conserver sa clé pour rejouer la même intention.
- URL S3 signée : l’endpoint doit être joignable depuis la machine cliente ; voir [Accès LAN](../docs/container-deployment.md#acces-lan).
- Sweego et Stripe : une signature valide porte sur les octets exacts du corps. Les placeholders de collection
  ne constituent pas un test d’acceptation signé. Voir le [contrat des intégrations](../docs/http-contract.md#routes-publiques-et-integrations).
- Effacement, suppression de catalogue, retrait de rôle et abandon outbox : relire la description et la cible
  avant l’envoi ; réserver ces essais aux données de test.

En cas de timeout LAN, vérifier d’abord IP active, port publié et readiness depuis le serveur.
Les codes API doivent être interprétés par `error.code`, pas par le texte du message.

## Continuations et crédits

Lire le quota, puis utiliser `PATCH /api/matches/:id/continue` avec le coût explicite configuré. Deux comptes Free consomment chacun un crédit après leurs deux accords ; la première acceptation réserve sans débit. Les messages sont suspendus dans la seconde fenêtre de 24 heures, avec historique accessible. Un refus ou une expiration libère les réservations. Le stock publicitaire persiste entre les semaines, avec un plafond de trois ; Free confirme au plus six continuations par semaine UTC.

Le dossier de récompenses prépare, lit et annule une intention propriétaire. La requête webhook nécessite une vraie preuve AdMob ; les collections ne génèrent aucune signature. Le fournisseur désactivé renvoie `503 advertising_disabled`, comme le `.env` local par défaut. Voir [Activation et tests](../docs/continuation-and-rewards.md).

## Parcours multilingue

Voir [Internationalisation](../docs/internationalization.md). Vérifier les quatre langues du catalogue,
les champs `translations` des traits/questions admin, les URL documentaires et la langue acceptée,
l’exception `language` des appareils, puis Checkout/portail. Les prix, UUID, catégories, réponses personnelles
et curseurs ne changent pas. Un changement de langue avec une même clé OTP est un conflit ; une reprise
Checkout conserve la langue de la tentative originale.

## Stores et paiement du site

La section correspondante couvre les intentions/vérifications natives, webhooks Apple/Google, rapports
admin et session web OTP. Renseigner `storeProvider`, IDs/receipts réels de test et signatures valides.
Les placeholders ne donnent jamais Premium. Ne pas exporter les purchaseTokens ni cookies.
Pour le site, configurer `webBillingOrigin` exact, deux clés UUIDv4 distinctes `webBillingOtpKey` et
`webBillingCheckoutKey`, puis `webBillingCookie` (ou le gestionnaire de cookies Postman). Le challenge
est mémorisé après 202. Le cookie web est distinct du cookie admin et du Bearer mobile.
Les anciennes routes Stripe mobiles renvoient 410. Le template financier du JSON est synthétique : ne pas
l’importer comme chiffre réel. Voir [le guide](../docs/store-billing.md).
