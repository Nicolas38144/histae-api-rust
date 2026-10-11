# Tester l’API avec Postman

[Documentation](../docs/README.md) · [Contrat HTTP](../docs/http-contract.md) · [Administration](../docs/administration.md)

## Import

Importer l’un des fichiers JSON avec **Import → Files** :

- [Collection française](postman_collection_fr.json).
- [Collection anglaise](postman_collection_en.json).

Chaque collection contient 103 requêtes correspondant au contrat HTTP. Aucun script Python ou générateur
n’est nécessaire pour l’import. Les collections incluent déjà quelques actions après réponse pour stocker
les tokens et challenges ; elles ne fabriquent ni preuve WebAuthn ni signature de fournisseur.

Exécuter les requêtes choisies une par une. Certaines modifient/suppriment des données, envoient des SMS ou
créent des opérations Stripe : les titres marqués `⚠️` signalent ces effets. Ne pas lancer toute la collection
avec Collection Runner sur une base utilisée.

## Variables

Configurer l’onglet **Variables** de la collection importée :

| Variable | Valeur / usage |
| --- | --- |
| `baseUrl` | `http://127.0.0.1:8080` sur le serveur, `http://<IP_LAN_SERVEUR>:8080` sur le LAN, ou domaine HTTPS API ; sans `/api` ni slash final |
| `phoneNumber`, `otp` | Téléphone français E.164 `+33…` et code reçu |
| `accessToken`, `refreshToken` | Remplis après vérification OTP ou refresh réussi |
| `adminCookie` | Cookie admin complet `nom=valeur`, issu d’une session WebAuthn valide |
| `adminOrigin` | Origine exacte du dashboard, par exemple `https://dashboard.histae.com` |
| `bootstrapToken`, `challengeId` | Jeton émis par le serveur et challenge à usage unique ; ne sont pas des credentials WebAuthn |
| `targetUserId`, `matchId`, `photoId`, etc. | UUID réels du jeu de données testé ; remplir uniquement ceux requis par la requête |
| `auditReason` | Motif de consultation/action ; facultatif pour le superadmin selon le contrat |
| `pageSize` | Taille de page ; ne s’applique qu’aux routes paginées |
| Variables de webhook | En-têtes/signatures valides obtenus du fournisseur ou d’une fixture de test |

Un environnement Postman peut masquer une variable de collection portant le même nom : vérifier les valeurs
résolues avant l’envoi. Après les essais, supprimer les tokens/cookies/jetons des variables avant export ou partage.

## Parcours mobile conseillé

1. Tester `/health/live`, puis `/health/ready`.
2. Envoyer l’OTP, vérifier le code et obtenir les deux tokens.
3. Lire les documents de consentement puis accepter leurs versions courantes.
4. Compléter le profil et les consentements sensible/localisation avant les données concernées.
5. Tester catalogues, photo, feed, swipes, matchs et messages avec plusieurs comptes fictifs.
6. Tester ensuite les erreurs, révocations et effacements sur des comptes jetables.

### OTP et idempotence

La collection utilise `{{$guid}}` pour une nouvelle intention d’envoi. `202` signifie demande acceptée,
sans garantie de livraison au téléphone. Un rejeu avec la même clé ne provoque pas un second SMS.
Après `503 otp_delivery_unknown`, récupérer la clé réellement envoyée dans la Console Postman et la
mettre explicitement dans `Idempotency-Key` pour le rejeu. Attendre ; ne pas laisser `{{$guid}}` produire
une nouvelle clé à chaque retry incertain.

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
