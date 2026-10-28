# Continuations, crédits et récompenses publicitaires

[Documentation](README.md) · [Contrat HTTP](http-contract.md#decouverte-matchs-et-messages) · [Postman](../postman/README.md) · [Configuration](configuration.md)

Ce guide décrit les règles métier, le parcours des crédits et l’activation AdMob. Le contrat HTTP décrit les routes et codes d’erreur. Les pages du site présentent ces règles dans les quatre langues ; l’app et le dashboard affichent les compteurs transmis par l’API. Les publicités restent désactivées par défaut et nécessitent la configuration du fournisseur.

## 1. Cycle de vie et décisions privées

| État | Échanges | Action |
| --- | --- | --- |
| `active` | Envoi et lecture pendant les 24 premières heures | Discuter, révéler les photos à deux ou terminer le match |
| `awaiting_continuation` | Envoi suspendu ; historique et accusés de lecture accessibles | Pendant les 24 heures suivantes, chacun choisit « Je veux continuer » ou « Mettre fin au match » |
| `confirmed` | Conversation rouverte, soumise au délai d’inactivité ci-dessous | Deux accords valides ; chaque personne reste libre de terminer le match |
| `expired` | Messagerie indisponible | Deux accords absents au terme de la fenêtre |
| `ended` | Messagerie indisponible ; match retiré des listes mobiles | Refus, clôture ou blocage |

La fenêtre de décision se termine **48 heures après la création du match**. Une maintenance tardive ou une requête qui attend un verrou ne donne pas 24 heures supplémentaires. La liste mobile calcule l’état à partir de l’horloge même entre les passes de maintenance ; les mutations vérifient l’horloge PostgreSQL.

Les choix sont indépendants. `my_continued` décrit uniquement le choix du demandeur. Le premier accord n’envoie aucun événement à l’autre personne. Deux accords valides déclenchent `match.updated` avec `match_confirmed: true`. Une clôture envoie seulement `status: ended`, sans auteur, motif ni choix individuel. Présenter **« Ce match est terminé »**. Les événements SSE ne constituent pas un historique durable : relire la liste à la reconnexion et après un changement d’état.

La révélation des photos reste un accord distinct. Premium ne dispense jamais de l’accord de l’autre personne.

### Conversations confirmées : inactivité 10 / 23 / 30 jours

Le délai commence à la confirmation mutuelle, puis repart à zéro uniquement lorsqu’un **nouveau
message est enregistré** par l’un des deux participants. Se connecter, lire, marquer un message
comme lu ou rejouer un envoi ne prolonge rien. La même règle s’applique à Free et Premium.

| Depuis la confirmation ou le dernier nouveau message | Effet |
| --- | --- |
| 10 jours | `archived: true` ; conversation dans « Archivées », toujours accessible |
| 23 jours | Notification `match_expiring` aux deux participants, une seule fois par cycle |
| 30 jours | Accès fermé ; match, messages et notifications associés supprimés par la maintenance, sans 30 jours supplémentaires |

Un nouveau message désarchive le match et invalide les avertissements encore en attente d’envoi.
Reprendre une conversation archivée ne demande aucun nouvel accord de continuation et ne consomme
aucun crédit supplémentaire. Aucun bouton de conservation sans message n’est prévu.

Pour un match confirmé, `expires_at` devient l’échéance d’inactivité ; `inactivity_expires_at`
l’expose explicitement aux clients. L’archivage est calculé à la lecture et n’ajoute pas de statut
SQL : le match reste `confirmed`. Les accès mobiles sont refusés à l’échéance, même si la purge est
en retard. La maintenance traite les suppressions par lots bornés ; elle doit être exécutée
régulièrement. Les signalements suivent leur propre rétention et sont détachés du match supprimé.

Le trigger PostgreSQL `trg_match_inactivity` renouvelle le délai à la confirmation et lors d’un
nouveau message. À la première migration, les matchs déjà confirmés reçoivent un délai complet
à partir du déploiement pour permettre le préavis. Les migrations suivantes ne le renouvellent pas.
Déployer le migrateur avant les nouvelles instances d’écriture ; les anciennes instances ne doivent
pas continuer à créer des notifications sous l’ancien contrat pendant la mise à jour.

## 2. Crédits Free et Premium

| Règle | Free | Premium effectif |
| --- | --- | --- |
| Allocation gratuite | 2 crédits chaque lundi à **00:00 UTC**, sans accumulation | Aucun crédit nécessaire |
| Récompense volontaire | 1 vidéo validée = 1 crédit publicitaire | Aucun lancement de publicité |
| Stock publicitaire | 3 crédits maximum, y compris ceux réservés ; remis à zéro chaque lundi à 00:00 UTC | Aucun débit ; le stock acquis expire aussi le lundi |
| Continuations confirmées | 5 maximum par personne et par semaine UTC | Illimitées |
| Coût d’une continuation | 1 crédit **pour chaque participant Free** | 0 |

Le prix Premium reste **5 € par mois ou 48 € par an, sans période d'essai**. Les Prices Stripe réels restent distincts du catalogue ; voir [Configuration](configuration.md#tarifs-du-catalogue-et-stripe).

### Réserver, confirmer et libérer

1. Lire `/api/users/me/continuation-quota` avant de proposer le choix et afficher le coût.
2. « Je veux continuer » envoie `PATCH /api/matches/:id/continue` avec `{"credit_cost":1}` pour Free, `{"credit_cost":0}` pour Premium.
3. Pour Free, l’API réserve en priorité un crédit gratuit, puis un crédit publicitaire. Sans crédit disponible ou sans place dans le plafond hebdomadaire, elle renvoie `403 continuation_quota_reached` et n’enregistre pas l’accord.
4. Le premier accord répond `match_confirmed: false`. Le crédit reste réservé, sans débit.
5. Deux accords valides débitent les participants Free et confirment le match **dans une seule transaction**. Un participant Premium ne paie aucun crédit. Un rejeu ne consomme pas une seconde fois.
6. Refus, expiration, blocage ou effacement rendent les réservations inutilisables immédiatement ; leur nettoyage physique est transactionnel ou borné par la maintenance. Un crédit déjà débité pour un match confirmé n’est pas remboursé à sa clôture.

Le corps annonce le **coût maximal accepté** : `1` autorise un coût nul si le compte est devenu Premium ; `0` ne peut jamais déclencher un débit Free. Un changement vers Free nécessite une nouvelle présentation du coût et un nouvel accord (`409 continuation_plan_changed`). Si c’est l’autre participant qui perd Premium pendant l’attente, son ancien accord sans coût est retiré ; le demandeur reçoit un état pending, sans information sur l’abonnement de l’autre.

Une réservation gratuite prise avant le lundi reste attachée à sa semaine d’origine pendant la fenêtre de décision. Le crédit gratuit est imputé à cette semaine ; la continuation confirmée compte dans le plafond de la semaine de confirmation. Le reste de l’allocation gratuite précédente ne se cumule pas. Le plafond de cinq est conservé aux changements de semaine.

Les crédits publicitaires, y compris réservés, expirent au lundi à 00:00 UTC. Un accord qui
reposait sur un crédit publicitaire expiré doit être redonné ; aucun nouveau crédit ne peut être
débité à sa place sans ce nouvel accord. `my_continued` reflète cette expiration. Les réservations
gratuites restent attachées à leur semaine d’origine comme décrit ci-dessus.

Le solde publicitaire est interprété à partir de `continuation_wallet.week_start` : une ancienne
semaine vaut immédiatement zéro, sans attendre une maintenance. Une nouvelle récompense ouvre
le solde de la semaine courante à un crédit. Une preuve tardive est enregistrée une seule fois,
mais ne reporte pas dans la nouvelle semaine un crédit gagné la semaine précédente. Dans ce cas,
son statut est `expired`, même si sa preuve a été traitée. `completed_at` enregistre le traitement
de la preuve ; `credited_at` atteste un crédit réellement accordé et produit le statut `credited`.
Un crédit accordé peut ensuite être dépensé ou expirer : seul le quota indique le solde utilisable.

### Lire le solde

Exemple Free avec une réservation :

```json
{
  "plan": "free",
  "used": 1,
  "weekly_limit": 5,
  "remaining": 3,
  "weekly_credits": 2,
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
- `advertising_balance` : stock acquis pendant la semaine UTC courante, réservations incluses ; `advertising_remaining` exclut les réservations.
- `reserved` : crédits Free retenus pour des décisions encore valides.
- Premium omet `weekly_limit` et `remaining`, avec `used: 0` ; cela ne supprime pas l’historique Free ni les crédits publicitaires encore valides de la semaine.

Le catalogue `/api/plans` distingue `weekly_included_credits: 2` et `weekly_continuation_limit: 5`. Il expose aussi `advertising_credit_stock_limit`, `ads_removed` et `rewarded_advertising_enabled`. Ce dernier reste `false` tant que le fournisseur est désactivé. La fiche admin auditée expose le même objet `continuation_quota`.

## 3. Parcours d’une vidéo récompensée

Le parcours mobile suit le contrat suivant :

1. Présenter séparément les informations et choix publicitaires adaptés via UMP. Ne pas déduire un consentement publicitaire de l’accord pour continuer, de la localisation ou des données sensibles.
2. Vérifier que les publicités peuvent être demandées. Proposer explicitement la récompense « 1 crédit de continuation », avec fermeture/abandon possibles. Conserver les deux crédits gratuits si la personne refuse les publicités.
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

Pour le développement, la configuration de référence utilise les unités officielles de test, fournisseur désactivé :

```dotenv
ADVERTISING_PROVIDER=disabled
ADMOB_ANDROID_REWARDED_AD_UNIT=ca-app-pub-3940256099942544/5224354917
ADMOB_IOS_REWARDED_AD_UNIT=ca-app-pub-3940256099942544/1712485313
ADMOB_REWARD_ITEM=continuation_credit
```

Ces unités sont les unités officielles de test Google. Les identifiants de test sont refusés lorsque `ENV=production` et que le fournisseur est activé. Aucun secret partagé AdMob ni URL arbitraire de clés n’est accepté : les clés publiques proviennent uniquement du serveur officiel Google.

Pour activer les récompenses sur le serveur, renseigner les vrais identifiants de blocs dans son `.env` :

```dotenv
ADVERTISING_PROVIDER=admob
ADMOB_ANDROID_REWARDED_AD_UNIT=REPLACE_WITH_ANDROID_REWARDED_AD_UNIT
ADMOB_IOS_REWARDED_AD_UNIT=REPLACE_WITH_IOS_REWARDED_AD_UNIT
ADMOB_REWARD_ITEM=continuation_credit
```

Les deux identifiants sont requis par la configuration actuelle, même si le lancement mobile commence sur une seule plateforme. Ce sont des identifiants de **blocs d’annonces** au format `ca-app-pub-…/…`, distincts de l’identifiant d’application au format `ca-app-pub-…~…`. Le montant de récompense de chaque bloc doit être **1**, avec le libellé `continuation_credit` lorsque cette valeur de `ADMOB_REWARD_ITEM` est conservée.

Le `.env` est local et ignoré par le dépôt : reporter ces valeurs sur le serveur puis recréer l’API. Modifier le fichier local ne configure ni le serveur déployé ni la console AdMob. Le fournisseur peut rester `disabled` en développement tant que le parcours mobile n’est pas prêt ; l’URL SSV répond alors `503 advertising_disabled`.

### Activer et vérifier le serveur

1. Créer/configurer les applications Android et iOS et leurs unités **rewarded** dans AdMob ; renseigner les unités réellement utilisées dans le `.env` du serveur.
2. Pour chaque unité, régler la récompense à **1** et son nom sur `continuation_credit`, ou changer `ADMOB_REWARD_ITEM` de manière cohérente. Les unités de test ne remplacent pas cette configuration ni une validation de callback réel.
3. Activer la vérification côté serveur de chaque unité et enregistrer cette URL HTTPS pour le domaine Histae configuré : `https://api.histae.com/api/auth/admob/webhook`. Pour un autre environnement, remplacer uniquement le domaine par celui de son API.
4. Le reverse proxy/tunnel doit transmettre cette route GET et sa **chaîne de requête originale**, sans en changer l’ordre, l’encodage ou le contenu. Éviter une page d’authentification Cloudflare Access ou un challenge interactif sur ce webhook. Les autres protections restent actives.
5. Mettre `ADVERTISING_PROVIDER=admob`, reconstruire puis recréer l’API avec les fichiers Compose habituels. Les choix SDK/UMP et le parcours mobile doivent être prêts avant d’ouvrir les publicités aux utilisateurs.
6. Dans l’outil SSV de la console AdMob, laisser **ID utilisateur** vide et renseigner exactement **`histae-ssv-validation`** dans **Données personnalisées**. Déclencher la validation, utiliser l’URL validée puis enregistrer. Cette vérification d’URL ne nécessite ni compte Histae ni intention et n’accorde aucun crédit : elle doit produire `200` et le log `admob_callback_url_validated` après vérification de la signature Google.
7. Tester séparément une vraie vidéo en mode de test fournisseur, avec une intention créée par l’API et son `custom_data` UUID transmis par le SDK. Vérifier le statut `credited`, un seul crédit ajouté et l’absence de deuxième crédit au rejeu du même callback. Utiliser les montants/libellés configurés ; ne pas fournir de `user_id` personnel.

Le webhook n’accepte pas de JSON « watched=true » ni de preuve fabriquée depuis Postman. Il vérifie ECDSA P-256/SHA-256 sur les octets signés, l’unité, le montant/libellé, l’intention, les bornes temporelles et l’unicité de la transaction. Une preuve liée à un compte effacé ou banni est refusée. Les erreurs publiques ne dévoilent ni compte ni clé.

Le marqueur `histae-ssv-validation` distingue uniquement le contrôle d’URL de la console : la signature Google, le format, l’interdiction d’un `user_id` non vide et les limites de débit restent vérifiés. Les paramètres de démonstration n’ont pas à correspondre à une intention pour ce contrôle sans effet métier. Ce chemin n’appelle jamais le stockage des récompenses. Tous les callbacks portant un UUID restent soumis au contrôle strict du bloc attendu ; le marqueur ne doit jamais remplacer cet UUID dans Flutter.

Les clés officielles sont mises en cache au plus 24 heures ; la récupération HTTPS est bornée à cinq secondes et 64 Kio. Une indisponibilité de vérification renvoie `503 admob_verification_unavailable`. Les créations d’intention ont une limite utilisateur de 10/min ; le webhook une limite IP de 300/min avec son espace de clés propre. Ces deux politiques reprennent les paramètres de facturation existants.

### Diagnostiquer un HTTP 400 pendant la validation AdMob

Pour valider uniquement l’URL, utiliser le marqueur `histae-ssv-validation`, pas un UUID d’intention : le test de console et le versement d’une récompense sont deux parcours distincts. Une ancienne version du serveur qui ne reconnaît pas ce marqueur renvoie `400 invalid_admob_signature` avec `admob_custom_data_invalid` dans les logs ; déployer la version correspondante avant le test.

Pour un vrai callback de vidéo, l’unité doit correspondre à l’intention et la récompense valoir **1** avec le libellé configuré. Le POST de création d’intention doit viser la même API/base que l’URL publique de rappel. Un rejet `admob_ad_unit_mismatch` prouve que la signature et le format ont été acceptés, mais que le bloc reçu ne correspond pas au bloc lié à l’intention. La console ne doit plus être validée au moyen d’une telle intention.

Le webhook conserve des erreurs publiques génériques (`invalid_admob_signature` pour la preuve, `invalid_admob_reward` pour sa cohérence métier). Les refus de preuve émettent aussi une ligne `admob_callback_rejected error_code=...` dans les logs API. Ces lignes contiennent uniquement un motif technique constant, jamais l’URL, la signature, les paramètres reçus, l’UUID de l’intention ou un identifiant utilisateur.

Après déploiement de cette version, observer les logs pendant un nouvel essai :

```bash
sudo docker logs --since 5m --follow histae-api-production-api-1
```

| Motif dans les logs | Vérification à effectuer |
| --- | --- |
| `admob_user_id_not_allowed` | Laisser le champ ID utilisateur vide dans la console et le SDK |
| `admob_required_field_missing`, `admob_custom_data_invalid` | Dans la console : marqueur exact `histae-ssv-validation` sur la version qui le prend en charge ; dans le SDK : UUID complet de l’intention, sans guillemets ni ajout à l’URL de rappel |
| `admob_reward_amount_mismatch`, `admob_reward_item_mismatch` | Vérifier le montant et le libellé réellement envoyés, ainsi que le `ADMOB_REWARD_ITEM` de l’API déployée |
| `admob_ad_unit_mismatch` | Tester le bloc Android ou iOS exact avec lequel l’intention a été créée |
| `admob_event_before_intent`, `admob_event_after_expiry`, `admob_event_in_future`, `admob_callback_too_late` | Vérifier les horloges et préparer une intention dont la fenêtre de visionnage est encore ouverte |
| `admob_intent_cancelled`, `admob_intent_already_credited`, `admob_transaction_already_used` | Relire l’état de l’intention ; ne pas réutiliser une preuve ou une intention terminée pour une nouvelle récompense |
| `admob_signature_missing`, `admob_signature_parameter_order_invalid`, `admob_signature_encoding_invalid`, `admob_signature_der_invalid`, `admob_signature_mismatch` | Déclencher le véritable outil AdMob et vérifier qu’aucun intermédiaire ne modifie la query signée ; un GET manuel sans preuve n’est pas un test SSV |
| `admob_key_id_missing`, `admob_key_id_invalid`, `admob_key_id_unknown` | Vérifier la réception intacte des paramètres Google ; ne pas remplacer la clé de vérification manuellement |
| `admob_callback_fields_invalid`, `admob_ad_network_invalid`, `admob_transaction_id_invalid`, `admob_timestamp_invalid`, `admob_timestamp_out_of_range`, `admob_reward_amount_invalid`, `admob_query_too_large` | Le format reçu ne respecte pas le contrat attendu ; conserver le motif pour le diagnostic sans publier la query complète |
| `admob_signature_verification_failed` | Vérifier l’environnement cryptographique et les erreurs de vérification sans accepter une preuve non vérifiée |

Une intention accepte une récompense gagnée pendant les 15 minutes suivant sa création. Répéter le POST pendant cette fenêtre reprend la même intention ; cela ne prolonge pas `expires_at`. Si la fenêtre est passée, annuler l’ancienne intention avec sa route DELETE puis créer une nouvelle intention et remplacer le `custom_data` de test. Ne pas annuler une vidéo réellement terminée qui attend encore son callback : une preuve gagnée à temps reste recevable pendant le délai de réception prévu.

Après une vraie récompense, relire le statut de l’intention et le quota. La validation d’URL avec le marqueur ne modifie ni l’intention ni le quota et ne constitue pas une validation du parcours Flutter complet.

## 5. Migration, rétention et validation

La migration ajoute les tables wallet, réservation et intention publicitaire ainsi que `free_used_count` au compteur hebdomadaire. Le schéma neuf et l’upgrade réutilisent les sections du SQL consolidé. Les privilèges restent ceux de `histae_migrate` et `histae_app`.

Avant un upgrade de cette règle de coût, suspendre les anciennes instances API et workers d’écriture. Ne pas faire cohabiter les deux modèles de débit pendant une mise à jour progressive ; appliquer la migration puis démarrer les nouvelles images ensemble.

Sur une ancienne base, l’usage historique est conservé comme crédits gratuits déjà consommés ; les matchs confirmés restent confirmés, sans débit rétroactif de l’autre participant. Les accords legacy en attente, qui ne réservaient aucun crédit et annonçaient un coût pour l’initiateur seul, sont remis à zéro lors du premier upgrade pour demander un accord sous le nouveau contrat. Une nouvelle passe du migrateur ne réinitialise pas les nouveaux accords.

Le verrou du match, les verrous de comptes/consentements, puis les verrous ordonnés des abonnements et wallets sérialisent les choix, les débits et les changements Premium. Le stock et les récompenses sont partagés entre les instances via PostgreSQL, sans dépendre d’un compteur en mémoire.

Les exports incluent le wallet et sa semaine, l’usage, les réservations et les intentions du demandeur. Ils n’incluent pas le choix de l’autre personne. L’effacement supprime ces données et libère aussi les réservations du partenaire sur les matchs terminés. La maintenance purge les intentions/preuves et l’usage hebdomadaire au-delà de 90 jours ; un usage lié à une réservation encore présente est conservé jusqu’à sa libération. Les crédits publicitaires expirent au changement de semaine. Le dernier solde et sa semaine peuvent rester enregistrés jusqu’à une nouvelle récompense ou à l’effacement du compte ; ils ne constituent plus un solde utilisable après cette échéance.

Les textes juridiques du site sont des **brouillons** actualisés. Avant l’activation des publicités, finaliser/valider les prestataires, données SDK, bases juridiques, transferts et choix UMP ; publier les nouvelles versions puis mettre à jour les versions/URL API et une référence de validation juridique réelle. Cette intervention ne fabrique pas de `LEGAL_REVIEW_REFERENCE` ni d’approbation juridique.

Voir [Tests](../tests/README.md) pour les commandes et [Résultats de validation](feature-audit.md) pour les preuves locales et leurs limites.

### Mise à jour de la formule hebdomadaire

Le migrateur ajoute la semaine UTC au wallet et met à jour la formule Free et ses traductions.
Les soldes antérieurs à cette évolution restent utilisables jusqu’au prochain lundi ; ils ne sont
jamais renouvelés par une nouvelle passe du migrateur. Les matchs confirmés et les consommations
historiques restent conservés. Livrer le schéma avant la nouvelle API : le démarrage refuse un
wallet qui ne possède pas la colonne `week_start`, ou des intentions sans `credited_at`.
Le migrateur conserve les anciens résultats en copiant `completed_at` vers `credited_at` une
seule fois. Les preuves antérieures à cette migration ne contiennent pas l’horodatage du gain
permettant de reconstituer une éventuelle expiration hebdomadaire.

## Références

- [Google — vérification serveur des récompenses](https://developers.google.com/admob/flutter/ssv)
- [Google — publicités récompensées et exemples de test](https://developers.google.com/admob/flutter/rewarded)
- [Google — choix de confidentialité UMP](https://developers.google.com/admob/flutter/privacy)
- [Google — règles applicables aux unités récompensées](https://support.google.com/admob/answer/7313578?hl=fr)
- [CNIL — recommandations pour les applications mobiles](https://www.cnil.fr/fr/recommandations-applications-mobiles)

### Disponibilité des clés de vérification

Le cache reste limité à 24 heures, la requête HTTPS à 5 secondes et le document à 64 Kio. Les signatures
DER P-256 malformées sont rejetées avant tout téléchargement. Un seul téléchargement est autorisé à la fois,
avec au plus 32 demandes attendant une clé et une attente de 5 secondes maximum. Ces attentes n’occupent pas
les 32 places de vérification cryptographique : une clé encore valide peut servir pendant une actualisation.
Une clé expirée n’est jamais utilisée comme repli. Échec ou annulation du téléchargement conservent le cooldown
existant de 30 secondes.

La saturation ou l’indisponibilité renvoie `503 admob_verification_unavailable`, sans octroi de crédit. Google
annonce jusqu’à cinq retries espacés d’une seconde : la grâce applicative de 24 heures accepte une preuve tardive,
mais ne garantit pas une nouvelle livraison pendant 24 heures. Vérifier ce parcours avec AdMob avant activation.
Voir le [guide officiel SSV](https://developers.google.com/admob/android/ssv#faq).

## Photo sharing

An approved private profile photo is required for discovery; it never appears in the feed.
Each match has two independent photo agreements. The app exposes the choice from the conversation
header, and `DELETE /api/matches/:id/reveal` withdraws the caller's agreement without spending credits
or removing messages. Both photos and their thumbnails are then hidden. A later agreement can restore
sharing while the other participant's agreement remains valid. Replacements use the current approved
photo in all mutually authorized matches; there is no per-match historical copy.
