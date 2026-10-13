# Continuations, crédits et récompenses publicitaires

[Documentation](README.md) · [Contrat HTTP](http-contract.md#decouverte-matchs-et-messages) · [Postman](../postman/README.md) · [Configuration](configuration.md)

Ce guide possède les règles métier, le parcours des crédits et l’activation AdMob. Le contrat HTTP possède les routes et codes d’erreur. Les pages du site présentent ces règles dans les quatre langues ; le dashboard affiche les compteurs transmis par l’API. L’intégration mobile reste à réaliser et les publicités sont désactivées par défaut.

## 1. Cycle de vie et décisions privées

| État | Échanges | Action |
| --- | --- | --- |
| `active` | Envoi et lecture pendant les 24 premières heures | Discuter, révéler les photos à deux ou terminer le match |
| `awaiting_continuation` | Envoi suspendu ; historique et accusés de lecture accessibles | Pendant les 24 heures suivantes, chacun choisit « Je veux continuer » ou « Mettre fin au match » |
| `confirmed` | Conversation rouverte sans échéance hebdomadaire | Deux accords valides ; chaque personne reste libre de terminer le match |
| `expired` | Messagerie indisponible | Deux accords absents au terme de la fenêtre |
| `ended` | Messagerie indisponible ; match retiré des listes mobiles | Refus, clôture ou blocage |

La fenêtre de décision se termine **48 heures après la création du match**. Une maintenance tardive ou une requête qui attend un verrou ne donne pas 24 heures supplémentaires. La liste mobile calcule l’état à partir de l’horloge même entre les passes de maintenance ; les mutations vérifient l’horloge PostgreSQL.

Les choix sont indépendants. `my_continued` décrit uniquement le choix du demandeur. Le premier accord n’envoie aucun événement à l’autre personne. Deux accords valides déclenchent `match.updated` avec `match_confirmed: true`. Une clôture envoie seulement `status: ended`, sans auteur, motif ni choix individuel. Présenter **« Ce match est terminé »**. Les événements SSE ne constituent pas un historique durable : relire la liste à la reconnexion et après un changement d’état.

La révélation des photos reste un accord distinct. Premium ne dispense jamais de l’accord de l’autre personne.

## 2. Crédits Free et Premium

| Règle | Free | Premium effectif, essai inclus |
| --- | --- | --- |
| Allocation gratuite | 3 crédits chaque lundi à **00:00 UTC**, sans accumulation | Aucun crédit nécessaire |
| Récompense volontaire | 1 vidéo validée = 1 crédit publicitaire | Aucun lancement de publicité |
| Stock publicitaire | 3 crédits maximum, y compris ceux réservés ; conservés entre les semaines | Stock acquis conservé, sans débit |
| Continuations confirmées | 6 maximum par personne et par semaine UTC | Illimitées |
| Coût d’une continuation | 1 crédit **pour chaque participant Free** | 0 |

Le prix Premium reste **5 € par mois, 48 € par an, essai de 30 jours**. Les Prices Stripe réels restent distincts du catalogue ; voir [Configuration](configuration.md#tarifs-du-catalogue-et-stripe).

### Réserver, confirmer et libérer

1. Lire `/api/users/me/continuation-quota` avant de proposer le choix et afficher le coût.
2. « Je veux continuer » envoie `PATCH /api/matches/:id/continue` avec `{"credit_cost":1}` pour Free, `{"credit_cost":0}` pour Premium.
3. Pour Free, l’API réserve en priorité un crédit gratuit, puis un crédit publicitaire. Sans crédit disponible ou sans place dans le plafond hebdomadaire, elle renvoie `403 continuation_quota_reached` et n’enregistre pas l’accord.
4. Le premier accord répond `match_confirmed: false`. Le crédit reste réservé, sans débit.
5. Deux accords valides débitent les participants Free et confirment le match **dans une seule transaction**. Un participant Premium ne paie aucun crédit. Un rejeu ne consomme pas une seconde fois.
6. Refus, expiration, blocage ou effacement rendent les réservations inutilisables immédiatement ; leur nettoyage physique est transactionnel ou borné par la maintenance. Un crédit déjà débité pour un match confirmé n’est pas remboursé à sa clôture.

Le corps annonce le **coût maximal accepté** : `1` autorise un coût nul si le compte est devenu Premium ; `0` ne peut jamais déclencher un débit Free. Un changement vers Free nécessite une nouvelle présentation du coût et un nouvel accord (`409 continuation_plan_changed`). Si c’est l’autre participant qui perd Premium pendant l’attente, son ancien accord sans coût est retiré ; le demandeur reçoit un état pending, sans information sur l’abonnement de l’autre.

Une réservation gratuite prise avant le lundi reste attachée à sa semaine d’origine pendant la fenêtre de décision. Le crédit gratuit est imputé à cette semaine ; la continuation confirmée compte dans le plafond de la semaine de confirmation. Le reste de l’allocation gratuite précédente ne se cumule pas. Le plafond de six est conservé aux changements de semaine.

### Lire le solde

Exemple Free avec une réservation :

```json
{
  "plan": "free",
  "used": 1,
  "weekly_limit": 6,
  "remaining": 3,
  "weekly_credits": 3,
  "free_remaining": 1,
  "advertising_balance": 2,
  "advertising_remaining": 2,
  "reserved": 1,
  "advertising_stock_limit": 3
}
```

- `used` : continuations Free confirmées cette semaine, jamais les accords en attente.
- `remaining` : crédits utilisables pour **de nouveaux accords**, bornés par les réservations et le plafond hebdomadaire.
- `free_remaining` : allocation gratuite non dépensée et non réservée de cette semaine.
- `advertising_balance` : stock total acquis, réservations incluses ; `advertising_remaining` exclut les réservations.
- `reserved` : crédits Free retenus pour des décisions encore valides.
- Premium omet `weekly_limit` et `remaining`, avec `used: 0` ; cela ne supprime pas l’historique Free ni le stock acquis.

Le catalogue `/api/plans` distingue `weekly_included_credits: 3` et `weekly_continuation_limit: 6`. Il expose aussi `advertising_credit_stock_limit`, `ads_removed` et `rewarded_advertising_enabled`. Ce dernier reste `false` tant que le fournisseur est désactivé. La fiche admin auditée expose le même objet `continuation_quota`.

## 3. Parcours d’une vidéo récompensée

Le mobile n’est pas modifié dans cette intervention. Voici le contrat qu’il devra intégrer :

1. Présenter séparément les informations et choix publicitaires adaptés via UMP. Ne pas déduire un consentement publicitaire de l’accord pour continuer, de la localisation ou des données sensibles.
2. Vérifier que les publicités peuvent être demandées. Proposer explicitement la récompense « 1 crédit de continuation », avec fermeture/abandon possibles. Conserver les trois crédits gratuits si la personne refuse les publicités.
3. Appeler `POST /api/users/me/advertising-rewards` avec `{"platform":"android"}` ou `{"platform":"ios"}`. L’API prépare/reprend une intention et répond `200` :

```json
{
  "id": "<UUID v4 de l’intention>",
  "ad_unit": "ca-app-pub-…/…",
  "custom_data": "<le même UUID opaque>",
  "expires_at": "<horodatage UTC>",
  "status": "pending",
  "reward_credits": 1
}
```

4. Charger la publicité avec `ad_unit`, puis renseigner **uniquement** `custom_data` dans les options de vérification serveur. Ne pas renseigner `user_id` avec l’UUID Histae, le téléphone ou un identifiant persistant.
5. Après une vidéo terminée, afficher une validation en cours. Le callback du SDK ne crédite rien : seul le callback AdMob signé ajoute le crédit côté serveur.
6. Relire `GET /api/users/me/advertising-rewards/:id` avec une attente progressive, puis le quota lorsque `status` vaut `credited`. Les autres états sont `pending`, `expired` et `cancelled`. `expired` décrit la fin de la fenêtre de visionnage ; une preuve valide gagnée avant cette échéance peut encore faire évoluer cet état vers `credited` pendant le délai de réception.
7. Si la vidéo est abandonnée ou ne peut être chargée, appeler `DELETE /api/users/me/advertising-rewards/:id` (`204`). Cela annule l’intention et libère sa place. Ne pas annuler une vidéo terminée en attendant sa confirmation : un callback tardif ne pourra plus créditer une intention annulée.

Une intention accepte une vidéo effectuée dans les **15 minutes**. Une preuve valide peut arriver jusqu’à **24 heures après son échéance**, si son horodatage décrit une récompense gagnée dans cette fenêtre. Une seule intention non terminée/non annulée occupe une place ; la reprise conserve son UUID et son échéance. Changer de plateforme exige d’abord son annulation. Une intention expirée peut être annulée pour commencer une nouvelle tentative.

Chaque compte ne peut consulter ou annuler que ses propres intentions. Une intention créditée n’est plus annulable (`404 reward_not_found`). Le stock ne peut jamais dépasser trois, même en cas de callbacks simultanés ou de crédits réservés sur des matchs.

## 4. Configuration AdMob et tunnel Cloudflare

Le `.env` unique contient déjà la configuration de développement, fournisseur désactivé :

```dotenv
ADVERTISING_PROVIDER=disabled
ADMOB_ANDROID_REWARDED_AD_UNIT=ca-app-pub-3940256099942544/5224354917
ADMOB_IOS_REWARDED_AD_UNIT=ca-app-pub-3940256099942544/1712485313
ADMOB_REWARD_ITEM=continuation_credit
```

Ces unités sont les unités officielles de test Google. Les identifiants de test sont refusés lorsque `ENV=production` et que le fournisseur est activé. Aucun secret partagé AdMob ni URL arbitraire de clés n’est accepté : les clés publiques proviennent uniquement du serveur officiel Google.

### Activer et vérifier le serveur

1. Créer/configurer les applications Android et iOS et leurs unités **rewarded** dans AdMob ; renseigner les unités réellement utilisées dans le `.env` du serveur.
2. Pour chaque unité, régler la récompense à **1** et son nom sur `continuation_credit`, ou changer `ADMOB_REWARD_ITEM` de manière cohérente. Les unités de test ne remplacent pas cette configuration ni une validation de callback réel.
3. Activer la vérification côté serveur de l’unité et enregistrer cette URL HTTPS, avec votre domaine API réel : `https://<domaine-api>/api/auth/admob/webhook`.
4. Le reverse proxy/tunnel doit transmettre cette route GET et sa **chaîne de requête originale**, sans en changer l’ordre, l’encodage ou le contenu. Éviter une page d’authentification Cloudflare Access ou un challenge interactif sur ce webhook. Les autres protections restent actives.
5. Mettre `ADVERTISING_PROVIDER=admob`, reconstruire puis recréer l’API avec les fichiers Compose habituels. Les choix SDK/UMP et le parcours mobile doivent être prêts avant d’ouvrir les publicités aux utilisateurs.
6. Utiliser une intention de test réelle, son `custom_data`, l’unité correspondante et l’outil de test SSV AdMob. Vérifier le statut `credited`, un seul crédit ajouté, puis le même callback rejoué sans deuxième crédit. Utiliser les montants/libellés configurés ; ne pas fournir de `user_id` personnel.

Le webhook n’accepte pas de JSON « watched=true » ni de preuve fabriquée depuis Postman. Il vérifie ECDSA P-256/SHA-256 sur les octets signés, l’unité, le montant/libellé, l’intention, les bornes temporelles et l’unicité de la transaction. Une preuve liée à un compte effacé ou banni est refusée. Les erreurs publiques ne dévoilent ni compte ni clé.

Les clés officielles sont mises en cache au plus 24 heures ; la récupération HTTPS est bornée à cinq secondes et 64 Kio. Une indisponibilité de vérification renvoie `503 admob_verification_unavailable`. Les créations d’intention ont une limite utilisateur de 10/min ; le webhook une limite IP de 300/min avec son espace de clés propre. Ces deux politiques reprennent les paramètres de facturation existants.

## 5. Migration, rétention et validation

La migration ajoute les tables wallet, réservation et intention publicitaire ainsi que `free_used_count` au compteur hebdomadaire. Le schéma neuf et l’upgrade réutilisent les sections du SQL consolidé. Les privilèges restent ceux de `histae_migrate` et `histae_app`.

Avant un upgrade de cette règle de coût, suspendre les anciennes instances API et workers d’écriture. Ne pas faire cohabiter les deux modèles de débit pendant une mise à jour progressive ; appliquer la migration puis démarrer les nouvelles images ensemble.

Sur une ancienne base, l’usage historique est conservé comme crédits gratuits déjà consommés ; les matchs confirmés restent confirmés, sans débit rétroactif de l’autre participant. Les accords legacy en attente, qui ne réservaient aucun crédit et annonçaient un coût pour l’initiateur seul, sont remis à zéro lors du premier upgrade pour demander un accord sous le nouveau contrat. Une nouvelle passe du migrateur ne réinitialise pas les nouveaux accords.

Le verrou du match, les verrous de comptes/consentements, puis les verrous ordonnés des abonnements et wallets sérialisent les choix, les débits et les changements Premium. Le stock et les récompenses sont partagés entre les instances via PostgreSQL, sans dépendre d’un compteur en mémoire.

Les exports incluent le wallet, l’usage, les réservations et les intentions du demandeur. Ils n’incluent pas le choix de l’autre personne. L’effacement supprime ces données et libère aussi les réservations du partenaire sur les matchs terminés. La maintenance purge les intentions/preuves et l’usage hebdomadaire au-delà de 90 jours ; un usage lié à une réservation encore présente est conservé jusqu’à sa libération. Les soldes acquis n’expirent pas au changement de semaine.

Les textes juridiques du site sont des **brouillons** actualisés. Avant l’activation des publicités, finaliser/valider les prestataires, données SDK, bases juridiques, transferts et choix UMP ; publier les nouvelles versions puis mettre à jour les versions/URL API et une référence de validation juridique réelle. Cette intervention ne fabrique pas de `LEGAL_REVIEW_REFERENCE` ni d’approbation juridique.

Voir [Tests](../tests/README.md) pour les commandes et [Résultats de validation](continuation-validation.md) pour les preuves locales et leurs limites.

## Références

- [Google — vérification serveur des récompenses](https://developers.google.com/admob/flutter/ssv)
- [Google — publicités récompensées et exemples de test](https://developers.google.com/admob/flutter/rewarded)
- [Google — choix de confidentialité UMP](https://developers.google.com/admob/flutter/privacy)
- [Google — règles applicables aux unités récompensées](https://support.google.com/admob/answer/7313578?hl=fr)
- [CNIL — recommandations pour les applications mobiles](https://www.cnil.fr/fr/recommandations-applications-mobiles)
