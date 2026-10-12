# Résultats du test de charge local

[Guide de test](load-testing.md) · [Performance SQL](sql-performance.md)

## Conditions de mesure

Campagne du **11 au 12 octobre 2026**, sur l’ordinateur de développement :

- AMD Ryzen 7 3700U, 4 cœurs / 8 threads, environ 13,6 Gio de RAM.
- API compilée en release depuis le commit `519d876`, avec les optimisations SQL du dépôt actuel ;
  image de test `fcb69b37bb44`.
- API temporaire limitée à **2 CPU / 1 Gio**, PostgreSQL à **2 CPU / 1 Gio**, pool API de 20 connexions.
- Base locale existante : 5 001 comptes, dont 4 999 comptes fictifs actifs utilisables,
  5 000 matchs confirmés et 74 996 messages.
- Les 18 index de performance manquants ont été installés par le migrateur actuel avant la mesure.
- k6 2.3.0 sur le même ordinateur, accès direct à `127.0.0.1:18081`, sans tunnel Cloudflare.
- Redis et clé JWT propres à l’API temporaire ; fournisseurs SMS, paiement et notifications désactivés.
- Quota global temporaire de 120 000 requêtes/minute pour mesurer la capacité au-delà du quota habituel.

Les requêtes HTTP sont authentifiées et passent par les vraies routes : **60 % feed**, **20 % liste de matchs**,
**20 % lecture de messages**, pages de 20 éléments. Chaque itération émet une seule requête, sans retry.
Les réponses doivent être `200` avec des listes non vides ; les vérifications initiales contrôlent aussi l’identité.
Les écritures de préparation se limitent aux sessions temporaires et au rafraîchissement des présences fictives.

## Mesure initiale, avant réécriture du feed

Les latences suivantes portent sur les **plateaux**, sans les montées et descentes de charge.

| Débit visé | Maintien observé | Débit servi | p95 global | p99 global | Erreurs HTTP | Itérations perdues |
| --- | --- | --- | --- | --- | --- | --- |
| 10 RPS | 2 min | 10,00 RPS | 125 ms | 134 ms | 0 | 0 |
| 15 RPS | 5 min | 15,00 RPS | 149 ms | 195 ms | 0 | 0 |
| 20 RPS | 5 min | 20,00 RPS | 408 ms | 905 ms | 0 | 0 |
| 25 RPS | environ 102 s, arrêt anticipé | 19,56 RPS | 7 439 ms | 8 271 ms | 0 | 473 |

Le palier à 25 RPS ne respecte pas les objectifs. La montée a été interrompue manuellement ;
les paliers de 50 et 100 RPS n’ont pas été exécutés. Les 473 itérations perdues correspondent à des
requêtes prévues que k6 n’a pas pu démarrer avec ses 100 utilisateurs virtuels, les réponses étant trop lentes.
Une réponse `200` ne suffit donc pas à valider un débit.
La campagne compte **16 078 requêtes HTTP de benchmark**, dont 16 vérifications initiales.
Aucun `401`, `429`, `5xx`, timeout ni résultat métier invalide n’a été observé.
Les deux maintiens à 15 et 20 RPS ont terminé avec tous les seuils k6 respectés.
Le p95 global de l’ensemble du profil à 20 RPS, montées et descente comprises, est de 339 ms ;
la valeur de 408 ms du tableau mesure uniquement les cinq minutes au débit demandé.

### Latence par route pendant les maintiens de cinq minutes

| Route | p95 à 15 RPS mixtes | p99 à 15 RPS mixtes | p95 à 20 RPS mixtes | p99 à 20 RPS mixtes |
| --- | --- | --- | --- | --- |
| `GET /api/feed` | 158 ms | 215 ms | 652 ms | 1 048 ms |
| `GET /api/matches/me` | 8 ms | 10 ms | 37 ms | 45 ms |
| `GET /api/matches/:id/messages` | 9 ms | 12 ms | 43 ms | 91 ms |

À 20 RPS mixtes, le plateau contient exactement 3 600 lectures de feed, 1 200 de matchs et 1 200 de messages.
La campagne la plus longue utilise successivement 1 386 comptes distincts parmi les 4 999 sessions préparées.

## Ressources et diagnostic

- À 25 RPS demandés, PostgreSQL atteint environ **200 % CPU**, sa limite Docker de deux CPU.
  Le pool de 20 connexions est occupé et jusqu’à **57 acquisitions** attendent dans les snapshots.
- À 20 RPS, PostgreSQL atteint aussi ponctuellement environ **197 % CPU**.
  L’API temporaire utilise en moyenne environ **5 % CPU** sur l’ensemble de ce profil.
- La mémoire ne paraît pas limitante : environ **17 Mio** pour le conteneur API en fin de test,
  **376 Mio** pour PostgreSQL, **15 Mio** pour Redis et plus de **6 Gio** disponibles sur l’hôte.
- Le générateur k6 reste sous 100 Mio de mémoire ; les mesures de CPU/RAM et de pool sont échantillonnées,
  elles ne garantissent pas l’absence de pics entre deux relevés.

Après la charge, six `EXPLAIN (ANALYZE, BUFFERS, SETTINGS, FORMAT JSON)` de la vraie requête
`candidate_batch`, avec un compte fictif et les consentements enregistrés, retournent chacun **84 candidats**.
L’exécution SQL prend **90 à 115 ms**, avec **16 à 21 ms** de planification sur ces sessions de diagnostic neuves.
Le plan traite environ **4 998 candidats géographiques**, puis **4 995 candidats éligibles**, avant le tri et le `LIMIT`.
Les estimations sous-évaluent fortement certains intermédiaires. Les statistiques de présence ont bien été
actualisées automatiquement après le rafraîchissement des données fictives.

Ces observations désignent le **feed SQL et la capacité CPU de PostgreSQL** comme premier goulot de
cette campagne. Les chiffres de planification du diagnostic ne mesurent pas le cache de requêtes du pool API.

## Conclusion de la mesure initiale

**20 RPS mixtes sont validés pendant cinq minutes sur cette configuration.** Le feed approche l’objectif
de 750 ms en p95 et dépasse une seconde en p99 ; **15 RPS conserve davantage de marge**.
25 RPS ne sont pas validés. Cette campagne ne détermine pas une limite maximale universelle.

Priorités :

1. Réduire le coût du feed dans les zones denses : examiner les estimations, les jointures et le calcul/tri
   de distance sur près de 5 000 candidats par requête. Préserver consentements, exclusions et pagination.
2. Comparer la réécriture avec les mêmes données et limites, puis refaire les maintiens à 20 et 25 RPS.
   Augmenter le pool seul ne traite pas la saturation CPU observée.
3. Confirmer le débit retenu pendant 30 minutes, avec un générateur sur une autre machine.
4. Mesurer ensuite le serveur cible en accès direct, puis via un tunnel Cloudflare dédié selon le guide.
   Ajouter séparément les swipes, envois de messages, uploads et traitements de fond.

Ces mesures portent sur des lectures authentifiées et une population fictive concentrée dans une petite zone.
Elles ne prédisent pas le débit en production, les performances du dashboard ni celles des fournisseurs externes.

## Réécriture du feed — 12 octobre 2026

Le feed matérialise la distance exacte une fois par candidat géographique et parcourt l’ordre
`(distance_km, user_id)`. Les comptes, préférences et consentements sont vérifiés à la demande via
une jointure latérale ; les rejets sont sautés jusqu’à obtenir une page complète.
Les détails modérés restent chargés après la sélection de page. La validation ciblée d’un swipe
conserve son traitement des décisions existantes.

Sur les mêmes données, trois mesures SQL par version donnent **97–108 ms avant**, contre **24–31 ms après**,
soit environ **3,7 fois plus rapide sur la moyenne**. La page de 84 candidats est strictement identique,
y compris les distances exactes, traits et contenus modérés. Le nouveau plan parcourt 85 candidats
pour remplir cette page, au lieu de joindre les comptes et consentements de presque toute la zone.
Le calcul des distances et le tri de la fenêtre géographique restent nécessaires.

Validation fonctionnelle : **6 tests de contrat et 3 tests PostgreSQL passent**, ainsi que le Clippy ciblé
avec `-D warnings`. La nouvelle régression vérifie 128 profils proches rejetés, les versions et retraits
de consentement, les préférences réciproques, les comptes indisponibles, les présences périmées,
les blocages dans les deux sens, les matchs terminés, les swipes actuels/expirés, les égalités de distance,
la fin de pagination et le passage de +180° à −180°.

Les maintiens HTTP de comparaison utilisent les mêmes limites de deux CPU pour l’API et PostgreSQL,
le même mélange 60/20/20, les mêmes pages de 20 éléments et le même jeu de données.
La nouvelle image release `1a1ac95a6d2a` contient la réécriture du dépôt de travail.

| Débit visé | p95 global avant | p95 global après | p95 feed avant | p95 feed après | Débit servi après | Maintien après |
| --- | --- | --- | --- | --- | --- | --- |
| 20 RPS | 408 ms | 48 ms | 652 ms | 50 ms | 20,00 RPS | 5 min |
| 25 RPS | 7 439 ms | 52 ms | — | 53 ms | 25,00 RPS | 5 min |

Le plateau précédent à 25 RPS avait été interrompu après environ 102 secondes, avec 473 itérations perdues ;
les deux nouveaux maintiens terminent sans itération perdue, erreur HTTP ni résultat métier invalide.
À 25 RPS après réécriture, le p99 global est de **54 ms** et celui du feed de **55 ms**.
Les deux profils émettent au total **15 591 requêtes HTTP**, dont huit vérifications initiales.

Sur les profils complets, la CPU moyenne échantillonnée de PostgreSQL est d’environ **32 % à 20 RPS**
et **45 % à 25 RPS** ; le maximum échantillonné à 25 RPS est de **61 %**, contre environ 200 % lors
de la saturation initiale. Ces pourcentages Docker correspondent à un CPU à 100 %, avec un plafond de 200 %.
Les snapshots du pool montrent au plus une acquisition en attente dans la nouvelle campagne.

**20 et 25 RPS mixtes sont maintenant validés pendant cinq minutes**, avec davantage de marge sur le feed.
La capacité maximale et l’endurance de trente minutes restent à mesurer ; aucun débit production n’est déduit
de ces tests sur le même ordinateur. Les traces de comparaison sont dans `.load-test/feed-optimization-2026-10-12/`.

## Recherche spatiale et lots adaptatifs — 12 octobre 2026

Cette étape ajoute PostGIS 3.6.4 à PostgreSQL 18.6 bookworm, un index GiST sur l’expression géographique
des coordonnées et une sélection par rayon progressif. Les tables applicatives et leurs colonnes sont
conservées. L’index fournit un préfiltre conservateur ; les distances Haversine exactes, consentements,
préférences et exclusions décident du résultat final dans l’ordre distance/UUID existant.

Pour une page de 20, le service commence avec 21 candidats détaillés. Les lots augmentent si la
revérification des swipes concurrents rejette des candidats, avec le même budget maximal de parcours.
Les prérequis et la première sélection sont réunis ; la vérification de compte, session, révocation et
onboarding reste effectuée à chaque requête par l’authentification. Les catégories d’erreur antérieures
sont conservées par une vérification des prérequis sur les échecs de sélection.

Neuf pages de curseur sur trois comptes fictifs correspondent exactement à la version précédente,
y compris les distances et les contenus modérés. Dans le plan examiné, le GiST retient environ
149 positions, contre près de 5 000 dans la fenêtre précédente, et 21 candidats sont détaillés.
L’exécution SQL sur une session initialisée prend **2,4–3,2 ms**. Les sessions neuves coûtent davantage
pour charger les fonctions spatiales ; ces mesures à chaud ne décrivent pas une première connexion.

Validation : 16 tests ciblés, dont concurrence, pagination, erreurs, base neuve et migration d’une base
peuplée ; compilation release et Clippy ciblé. La restauration de sauvegarde passe avec **46 tables publiques**,
dont la table de référence de l’extension. Une sauvegarde privée a été conservée avant le changement d’image
PostgreSQL, dans le dossier local de cette campagne.

La montée en charge sur l’image `a74eb2a18143`, avec les mêmes limites de deux CPU pour l’API et PostgreSQL,
le même jeu de données et le mélange 60/20/20, termine avec **45 768 requêtes**, sans erreur ni itération perdue :

| Débit visé | Maintien | Débit servi approximatif | p95 global | p99 global |
| --- | --- | --- | --- | --- |
| 10 RPS | 2 min | 10,00 RPS | 10,4 ms | 22,0 ms |
| 25 RPS | 2 min | 25,01 RPS | 9,8 ms | 11,1 ms |
| 50 RPS | 2 min | 50,00 RPS | 9,9 ms | 11,1 ms |
| 100 RPS | 5 min | 100,00 RPS | 9,7 ms | 10,7 ms |

Pendant le maintien à 100 RPS, le p95 du feed est de **10,1 ms**, celui des matchs de **3,6 ms** et celui
des messages de **4,1 ms**. Les snapshots de l’ensemble du profil donnent au maximum environ
47 % d’un CPU pour PostgreSQL et 30 % pour l’API, sous leurs limites de 200 % chacune.
Ces mesures valident ce scénario local ; elles ne déterminent pas la capacité maximale ni le débit du tunnel.

La confirmation sur l’image finale **`13d185be5b18`**, après le correctif des catégories d’erreur,
termine également avec succès : **34 533 requêtes HTTP**, dont quatre vérifications initiales,
sans erreur HTTP, réponse métier invalide, 429 ni itération perdue.
Le maintien sert exactement **30 000 requêtes en cinq minutes**, soit **100 RPS** :

| Lecture | Part du trafic | p95 | p99 |
| --- | --- | --- | --- |
| Ensemble | 100 % | 9,8 ms | 10,7 ms |
| Feed | 60 % | 10,1 ms | 11,1 ms |
| Matchs | 20 % | 3,5 ms | 4,1 ms |
| Messages | 20 % | 4,0 ms | 4,5 ms |

Sur ce profil final, les maxima CPU échantillonnés sont de **49 % pour PostgreSQL** et **31 % pour l’API**,
avec au plus une acquisition du pool en attente. Tous les contrôles de readiness réussissent.
La montée en charge et la confirmation totalisent **80 301 requêtes HTTP**. Les traces sont dans
`.load-test/spatial-feed-2026-10-12/`. Ces lectures locales n’incluent pas Cloudflare ; la capacité maximale,
les écritures et l’endurance de trente minutes restent à mesurer.

## Paliers 200, 500 et 1 000 RPS — 12 octobre 2026

Cette campagne reprend l’image finale `13d185be5b18`, les mêmes données, le mélange de lectures
60/20/20 et les limites de deux CPU / un Gio pour l’API et PostgreSQL. Le pool reste à 20 connexions.
Chaque palier prévoit une montée d’une minute, cinq minutes de maintien et trente secondes de descente.
Le générateur préalloue **1 000 utilisateurs virtuels**, contre 100 dans la campagne précédente.
Cette différence augmente aussi le nombre de connexions HTTP persistantes ; elle doit être prise en compte
pour comparer les deux campagnes.

Les seuils k6 sont conservés sans arrêt anticipé sur la latence ou les erreurs. Une surveillance des
conteneurs toutes les deux secondes arrête toute la campagne en cas d’arrêt, de redémarrage, d’OOM
ou de passage à `unhealthy`, conformément à la consigne donnée avant le test.

Les mesures ci-dessous portent sur les maintiens, sans les rampes. Le débit réussi et le p95 réussi
comptent uniquement les réponses HTTP `200`, pour ne pas confondre des erreurs rapides avec de bonnes performances.

| Cible | Maintien observé | Réponses HTTP/s, tous statuts | Réponses réussies/s | p95 réussi | Erreurs HTTP | Itérations perdues |
| --- | --- | --- | --- | --- | --- | --- |
| 200 RPS | 5 min | 200,00 | 199,88 | 10,2 ms | 35 / 60 000, soit 0,058 % | 0 |
| 500 RPS | 2 min 55 s, arrêt automatique | 499,96 | 277,82 | 54,0 ms | 38 841 / 87 416, soit 44,43 % | 0 |
| 1 000 RPS | Non exécuté après l’arrêt demandé | — | — | — | — | — |

Le maintien à 200 RPS termine avec un p95 feed tous statuts de **10,9 ms** et un p99 global de **17,8 ms**.
Ses erreurs sont 27 réponses `500` et huit `503`. Il respecte les seuils k6 configurés à moins de 1 %
d’erreurs, mais ne constitue pas une validation sans erreur.

À 500 RPS, le watchdog interrompt k6 le **12 octobre à 01:59:59, heure de Paris** : le conteneur API de test
est encore en cours d’exécution, mais son état de santé devient **`unhealthy`**. Aucun OOM ni redémarrage
n’est observé. Le palier à 1 000 RPS n’est donc pas lancé. Sur le profil complet à 500 RPS, les codes relevés
comprennent `account_check_failed`, `internal_error` et `discovery_unavailable` ; les requêtes concernées
échouent dans les contrôles de compte ou les lectures métier.

### Limite de connexions constatée

La limite souple du processus API est de **1 024 descripteurs de fichiers**, sa limite dure de 524 288.
Pendant le test, une lecture de `/proc/7/fd` compte exactement **1 024 descripteurs ouverts**.
Les sockets HTTP, connexions PostgreSQL/Redis et autres fichiers partagent ce budget. Cette saturation
explique une limite de connexions que la campagne précédente à 100 utilisateurs virtuels n’avait pas atteinte.
Elle empêche d’attribuer l’échec à 500 RPS au seul coût des requêtes SQL.

Sur le profil à 500 RPS, les maxima CPU échantillonnés sont d’environ **210 % pour PostgreSQL**,
proche de son plafond de deux CPU, et **88 % pour l’API**. La mémoire disponible de l’hôte reste d’environ
sept Gio au minimum. Sur chaque profil, le client ne fournit pas de snapshot exploitable à six reprises :
leur état de readiness reste inconnu. Les snapshots complets enregistrent uniquement des réponses `200`.
L’état Docker `unhealthy`, lui, est confirmé par l’inspection du conteneur.

Avant de reprendre 500 puis 1 000 RPS, relever explicitement la limite `nofile` des services applicatifs
dans Compose, puis vérifier les descripteurs, le pool et la CPU PostgreSQL sous la même charge.
La capacité maximale en RPS reste à établir après cette correction. La comparaison devra aussi distinguer
le débit demandé du nombre de clients maintenant une connexion ouverte.

Les deux profils totalisent **171 672 requêtes HTTP**, vérifications initiales comprises.
Les traces, états de santé, preuve de saturation des descripteurs et résumés sont conservés dans
`.load-test/high-rps-2026-10-12/`. Les sessions et conteneurs temporaires ont été supprimés ; l’API habituelle
répond `ready`, avec 5 001 comptes, 5 000 matchs confirmés et 74 996 messages conservés.

## Nettoyage et traces

Les 4 999 sessions temporaires sont révoquées et le fichier de jetons est supprimé après la campagne.
Les conteneurs API/Redis de benchmark et leur fichier d’environnement secret sont retirés.
Les index installés et le rafraîchissement des présences fictives sont conservés ; les comptes, matchs et messages
sont conservés. L’API habituelle reste disponible après le test.

Les résumés k6, points de mesure, plans SQL et relevés de ressources sont conservés localement dans
`.load-test/local-2026-10-11/`, `.load-test/feed-optimization-2026-10-12/` et
`.load-test/spatial-feed-2026-10-12/`, ainsi que `.load-test/high-rps-2026-10-12/`,
dossiers exclus de Git et du contexte de build Docker.
L’ACL ajoutée manuellement au socket Docker doit être retirée par l’utilisateur avec la commande fournie
à la fin de la campagne.
