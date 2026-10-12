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

Cette première campagne a conduit à relever `nofile`, puis à vérifier les descripteurs, le pool et la CPU
PostgreSQL sous la même charge. La reprise correspondante est décrite ci-dessous. Le débit demandé
et le nombre de clients maintenant une connexion ouverte restent deux dimensions distinctes.

Les deux profils totalisent **171 672 requêtes HTTP**, vérifications initiales comprises.
Les traces, états de santé, preuve de saturation des descripteurs et résumés sont conservés dans
`.load-test/high-rps-2026-10-12/`. Les sessions et conteneurs temporaires ont été supprimés ; l’API habituelle
répond `ready`, avec 5 001 comptes, 5 000 matchs confirmés et 74 996 messages conservés.

## Reprise avec `nofile=16384` — 12 octobre 2026

Le service API de `compose.yaml` fixe désormais les limites souple et dure à **16 384**. La configuration
est commune au développement et à la production ; les deux compositions ont été validées. L’API locale
a été recréée avec son image existante. Les limites du shell et celles du processus API dans `/proc`
confirment `16384`, puis `/health/ready` répond `200`.

L’API temporaire reprend l’image `13d185be5b18` et hérite du même réglage. Les autres conditions de la
campagne précédente sont conservées : deux CPU / un Gio pour l’API et PostgreSQL, pool de 20 connexions,
1 000 utilisateurs virtuels, mélange 60/20/20 et mêmes comptes fictifs. La limite `16384` constitue un
budget initial avec marge, à dimensionner selon les clients connectés et les ressources mesurées.

### Préparation et interruption exclue

Le contrôle préalable a détecté que le `.env` courant indiquait les versions légales `v1`, tandis que
les 4 999 comptes fictifs avaient accepté `development-unversioned`. Seule la configuration légale de
l’API temporaire a été alignée sur les fixtures, après vérification de leurs quatre consentements.
Aucun consentement en base n’a été modifié. Le contrôle préalable incomplet est exclu des mesures.

Une première exécution à 1 000 RPS a subi une interruption de plus de dix heures et ses jetons ont expiré.
Elle a été arrêtée et ses traces sont conservées séparément comme **invalides**. Les anciennes sessions
ont été révoquées, puis ce palier seul a été repris avec des sessions neuves. Le superviseur surveille
également les sauts d’horloge et l’expiration des fixtures, en plus des pannes de conteneurs.

### Mesures valides

Chaque palier ci-dessous comprend **cinq minutes de maintien**, après une montée d’une minute.
Les chiffres du tableau portent uniquement sur le maintien. Les réponses reçues sont toutes `200`
avec des résultats métier valides ; les itérations perdues représentent des requêtes planifiées que
le générateur n’a pas pu démarrer avec ses 1 000 utilisateurs virtuels occupés.

| Cible | Débit servi | p95 global | p99 global | Erreurs HTTP | Itérations perdues pendant le maintien | Résultat |
| --- | --- | --- | --- | --- | --- | --- |
| 200 RPS | 200,00 RPS | 10,1 ms | 15,5 ms | 0 | 0 | Validé |
| 500 RPS | 394,06 RPS | 3 313,7 ms | 3 632,2 ms | 0 | 31 361 | Non validé |
| 1 000 RPS | 377,13 RPS | 3 493,0 ms | 3 703,5 ms | 0 | 186 859 | Non validé |

Le p95 du feed pendant les maintiens est respectivement de **10,5 ms**, **3 465,1 ms** et **3 589,8 ms**.
Les profils à 500 et 1 000 RPS terminent avec le code k6 `99` : les seuils de latence et d’itérations
perdues échouent. Ils ne valident donc pas leurs débits cibles malgré l’absence d’erreurs HTTP.

Les trois profils complets totalisent **354 956 requêtes HTTP**, dont douze vérifications initiales,
sans erreur HTTP ni réponse métier invalide. Ils comptent **231 643 itérations perdues** hors vérifications,
en incluant les rampes. Aucun OOM, redémarrage ou passage de conteneur à `unhealthy` n’est observé.
Tous les snapshots de readiness répondent `200`, sans relevé manquant.

### Descripteurs et goulot restant

Les pics de descripteurs de l’API sont de **1 035 à 200 RPS**, puis **1 036 à 500 et 1 000 RPS**.
Ils retombent à **34 après chaque profil**. Le processus dépasse donc l’ancienne limite de 1 024,
dispose maintenant de marge et libère les sockets à la déconnexion.

À 200 RPS, aucun instantané du pool ne montre d’acquisition en attente. Aux deux paliers supérieurs,
ses 20 connexions sont occupées et les pics échantillonnés atteignent **618**, puis **612 acquisitions
en attente**. Les maxima CPU PostgreSQL atteignent environ **207–208 % d’un CPU**, près du plafond de
deux CPU ; ceux de l’API restent autour de **101 %** et **93 %**, sous son plafond de deux CPU.

La correction élimine la saturation des descripteurs rencontrée précédemment. La CPU PostgreSQL et
les attentes de pool bornent désormais le scénario testé. Les débits observés en saturation, autour
de 380–400 RPS, ne constituent pas des débits validés avec une latence acceptable. **200 RPS sont validés
pendant cinq minutes** dans ces conditions locales. La prochaine mesure doit profiler le coût CPU SQL
et borner les attentes avant une nouvelle hausse de charge ; augmenter le pool doit être évalué avec
le budget CPU et le total de connexions PostgreSQL.

Les traces valides, limites effectives, relevés de sockets et résultats de nettoyage sont dans
`.load-test/nofile-2026-10-12/`. Les sessions des deux préparations ont été révoquées et les fichiers secrets
et conteneurs de benchmark supprimés. L’API locale conserve `nofile=16384` et répond `ready`, avec
5 001 comptes, 5 000 matchs confirmés et 74 996 messages conservés. Ces mesures restent des lectures
locales sans Cloudflare ; elles ne déterminent pas le débit de production ni l’endurance sur trente minutes.

## Pool API de 30 connexions — 12 octobre 2026

Cette campagne modifie uniquement `POSTGRES_POOL_MAX=30` sur l’API temporaire. Les métriques
confirment cette limite sur les trois profils. **L’API habituelle conserve son pool de 20 connexions**,
comme les workers. L’image de benchmark reste `13d185be5b18`, avec `nofile=16384`, deux CPU / un Gio
pour l’API et PostgreSQL, 1 000 utilisateurs virtuels et les mêmes données fictives. Le trafic reste
composé de 60 % de lectures du feed, 20 % de matchs et 20 % de messages, sans Cloudflare.

Les versions légales de l’API temporaire correspondent aux consentements des fixtures ; aucun
consentement en base n’est modifié. Les sessions sont neuves pour cette campagne. La surveillance
des pannes de conteneurs, des sauts d’horloge et de l’expiration des jetons reste active.

Chaque profil comprend une montée d’une minute, **cinq minutes de maintien** et trente secondes
de descente. Le tableau mesure uniquement le maintien ; les itérations perdues sont les requêtes
planifiées que k6 n’a pas pu démarrer, ses utilisateurs virtuels étant occupés.

| Cible | Débit servi | p95 global | p99 global | Erreurs HTTP | Itérations perdues pendant le maintien | Résultat |
| --- | --- | --- | --- | --- | --- | --- |
| 300 RPS | 300,00 RPS | 45,9 ms | 253,4 ms | 0 | 0 | Validé |
| 400 RPS | 387,58 RPS | 3 150,0 ms | 3 491,4 ms | 0 | 3 126 | Non validé |
| 500 RPS | 388,96 RPS | 3 289,3 ms | 3 562,9 ms | 0 | 32 330 | Non validé |

Le p95 du feed pendant ces maintiens est respectivement de **60,2 ms**, **3 297,7 ms** et **3 385,7 ms**.
Le profil à 300 RPS respecte les seuils k6, dont un p95 inférieur à 750 ms et aucune itération perdue.
Les profils à 400 et 500 RPS terminent avec le code `99` : la latence et les itérations perdues dépassent
les seuils. Toutes les réponses reçues sont `200` avec des listes métier non vides.

Les profils complets totalisent **378 127 requêtes HTTP**, dont douze vérifications initiales,
sans erreur HTTP ni réponse métier invalide. Les rampes comprises, **35 972 itérations sont perdues**.
Aucun arrêt, redémarrage, OOM ou passage à `unhealthy` n’est observé ; tous les snapshots de readiness
répondent `200`, sans relevé manquant.

### Ressources et comparaison avec le pool de 20

Les pics échantillonnés d’acquisitions en attente sont de **4**, **609** et **605** aux trois paliers.
Les maxima CPU PostgreSQL atteignent respectivement **159 %**, **208 %** et **204 % d’un CPU**.
Aux deux derniers paliers, PostgreSQL approche donc son plafond de deux CPU ; le maximum API reste
autour de 102 %. Les pics de descripteurs sont de **1 045**, **1 046** et **1 046**, puis retombent à
**44 après chaque profil**, cohérent avec les dix connexions supplémentaires conservées dans le pool.

Le palier à 500 RPS est comparable à celui de la campagne précédente : même image, données,
mélange de lectures, limites CPU/RAM, durée et nombre d’utilisateurs virtuels.

| Mesure du maintien à 500 RPS | Pool de 20 | Pool de 30 |
| --- | --- | --- |
| Débit servi | 394,06 RPS | 388,96 RPS |
| p95 global | 3 313,7 ms | 3 289,3 ms |
| Itérations perdues | 31 361 | 32 330 |
| Erreurs HTTP | 0 | 0 |

Cette comparaison ne montre **aucun gain de débit** avec 30 connexions. La faible différence de latence
ne suffit pas à conclure à une amélioration : il n’y a qu’une exécution par configuration et les mesures
ont lieu successivement sur le même ordinateur. Le résultat est cohérent avec la saturation CPU
PostgreSQL et les longues attentes observées ; augmenter davantage le pool n’est pas justifié par ce test.

**300 RPS sont validés pendant cinq minutes avec le pool de 30.** À ce stade, le palier à 300 RPS
avec le pool de 20 n’avait pas encore été mesuré ; la comparaison complémentaire figure ci-dessous.
Une confirmation de trente minutes et un profilage CPU SQL restent nécessaires avant de retenir
un débit d’exploitation. Ces mesures locales ne déterminent pas la capacité du tunnel ni celle de production.

Les 4 999 sessions temporaires ont été révoquées ; leurs jetons, la configuration secrète et les deux
conteneurs de benchmark sont supprimés. Les vérifications retrouvent 5 001 comptes, 5 000 matchs
confirmés et 74 996 messages. L’API habituelle est inchangée, `healthy`, répond `ready` et conserve
son pool de 20 ainsi que `nofile=16384`. Les traces sont dans `.load-test/pool30-2026-10-12/`.

## Comparaison complétée à 200 et 300 RPS — 12 octobre 2026

Deux nouveaux profils complètent les mesures manquantes : **300 RPS avec 20 connexions**, puis
**200 RPS avec 30 connexions**. Ils utilisent la même image `13d185be5b18`, les mêmes données,
limites CPU/RAM, mélange 60/20/20, pages de 20, 1 000 utilisateurs virtuels et `nofile=16384`.
Chaque profil comprend une minute de montée, cinq minutes de maintien et trente secondes de descente,
en accès local sans Cloudflare. Seul le pool de l’API temporaire change entre les deux profils.

Un premier démarrage de cette API temporaire a été rejeté pour un format de durée de refresh invalide.
Le format a été corrigé avant la création des sessions et avant toute charge ; cette préparation est
exclue des mesures. Les fixtures ont ensuite reçu 4 999 sessions neuves valables une heure, avec les
consentements existants inchangés. La surveillance des pannes et de l’expiration reste active.

Les chiffres ci-dessous portent uniquement sur les maintiens de cinq minutes. Les lignes « précédente »
reprennent les campagnes documentées plus haut ; les deux autres correspondent aux nouveaux profils.

| Pool API | Cible | Débit servi | p95 global | p99 global | Erreurs HTTP | Itérations perdues | Mesure |
| --- | --- | --- | --- | --- | --- | --- | --- |
| 20 connexions | 200 RPS | 200,00 RPS | 10,1 ms | 15,5 ms | 0 | 0 | Précédente |
| 20 connexions | 300 RPS | 299,99 RPS | 67,2 ms | 176,3 ms | 0 | 0 | Nouvelle |
| 30 connexions | 200 RPS | 200,00 RPS | 11,3 ms | 203,0 ms | 0 | 0 | Nouvelle |
| 30 connexions | 300 RPS | 300,00 RPS | 45,9 ms | 253,4 ms | 0 | 0 | Précédente |

**Les quatre paliers respectent les seuils configurés**, dont un p95 inférieur à 750 ms et aucune
itération perdue. Le maintien à 300 RPS avec 20 connexions compte 89 998 réponses ; l’écart de deux
réponses à la frontière de la fenêtre explique son débit arrondi à 299,99, sans itération perdue.
Les deux nouveaux profils terminent avec le code k6 `0`, toutes les réponses étant `200` et les listes
métier non vides. Ils totalisent **172 566 requêtes HTTP**, dont huit vérifications initiales,
sans erreur HTTP, réponse métier invalide ni itération perdue, rampes comprises.

Le p95 du feed atteint **85,7 ms** à 300 RPS avec 20 connexions et **12,5 ms** à 200 RPS avec 30.
Les pics de descripteurs sont respectivement de **1 035** et **1 045**, puis retombent à **34** et **44**.
Les pics échantillonnés d’acquisitions en attente sont de **27** et **2**. PostgreSQL atteint au maximum
environ **208 %** et **92 % d’un CPU** ; les maxima de l’API sont d’environ **109 %** et **52 %**.
Aucun arrêt, redémarrage, OOM ou passage à `unhealthy` n’est observé pendant les mesures ; tous les
snapshots de readiness répondent `200`, sans relevé manquant.

Le pool de **20 connexions valide donc aussi 300 RPS** sur cinq minutes. À 300 RPS, le p95 mesuré est
plus bas avec 30 connexions, mais le p99 est plus élevé ; à 200 RPS, le pool de 30 n’améliore aucun
des deux percentiles. Il n’y a qu’une exécution par combinaison, à des moments différents sur un
ordinateur partagé avec le générateur : ces écarts ne prouvent pas un effet causal du pool.
Le maximum du nouveau maintien à 200 RPS atteint **2,45 s**, malgré son p95 bas ; les réponses les plus
lentes restent à examiner. Les résultats ne justifient pas de changer le pool habituel de 20 connexions.
Une confirmation prolongée et le profilage SQL restent les prochaines mesures utiles.

Les 4 999 sessions temporaires sont révoquées, les secrets et conteneurs de benchmark supprimés.
La vérification conserve 5 001 comptes, 5 000 matchs confirmés et 74 996 messages. L’API habituelle
est inchangée, `healthy`, répond `ready`, conserve son pool de 20 et `nofile=16384`.
Les traces et preuves de nettoyage sont dans `.load-test/pool-comparison-2026-10-12/`.

### Configuration appliquée après les mesures

Après cette comparaison, le pool de l’API habituelle a été réglé à **30 connexions**, à la demande
de l’utilisateur. `HISTAE_API_POSTGRES_POOL_MAX=30` dans `.env` et le défaut du service API dans
`compose.yaml` rendent ce réglage persistant. La recréation locale conserve l’image existante ;
le conteneur est `healthy`, reçoit `POSTGRES_POOL_MAX=30` et répond `ready`. Les autres pools restent
à 20. Les résultats ci-dessus décrivent les configurations utilisées pendant les mesures.

### Alignement de l’image et statistiques SQL

L’API, le worker outbox et le migrateur locaux utilisent désormais l’image de benchmark
**`13d185be5b18`**, sous l’étiquette `histae-api:local`. L’ancienne image API `60443dc2c5a9` est conservée
sous `histae-api:before-alignment-2026-10-12`. Le pool API reste à 30, celui du worker à 20.

PostgreSQL conserve son image et son volume. `pg_stat_statements` **1.12** est préchargé et installé,
avec `compute_query_id=auto`, suivi des commandes utilitaires désactivé et suivi de planification désactivé.
Les premiers relevés confirment l’enregistrement de requêtes et d’appels. Le migrateur termine avec
le code 0, `/health/ready` répond `200` et le smoke test passe. Les 5 001 comptes, 5 000 matchs confirmés
et 74 996 messages sont conservés.

Une base temporaire contenant seulement l’extension de statistiques a aussi été initialisée par le
migrateur de cette image : 46 tables publiques, aucun compte fictif, PostGIS 3.6.4 et statistiques lisibles.
Cette base a été supprimée. Une sauvegarde privée créée avant l’activation et les preuves de vérification
sont dans `.load-test/image-stats-2026-10-12/`. Aucun nouveau test de charge n’a été exécuté pendant
cette mise à jour ; les mesures précédentes n’incluaient pas le préchargement de `pg_stat_statements`.

## Profilage SQL et calculs du feed — 12 octobre 2026

Cette comparaison utilise `pg_stat_statements` activé, un pool API de **30 connexions**,
`nofile=16384` et **1 000 utilisateurs virtuels**. L’API et PostgreSQL conservent chacun leur limite
de **deux CPU / un Gio**. Les données, pages de 20, accès loopback et mélange de lectures 60/20/20
restent identiques. Les deux profils comprennent une minute de montée, **cinq minutes à 300 RPS**,
puis trente secondes de descente. L’image initiale est `13d185be5b18`, l’image optimisée `e2ba350009c7`.
Les mêmes sessions, clé JWT et Redis temporaires servent aux deux profils ; les fournisseurs externes
sont désactivés et les routes OTP ne sont pas appelées.

### Requêtes dominantes

Un rôle PostgreSQL temporaire distingue les requêtes du benchmark des autres services de la base.
Il ne dispose d’aucun privilège de superutilisateur ou de création d’objets. Deux snapshots de ses
statistiques isolent chaque maintien, sans remise à zéro des compteurs partagés et sans publication
du texte SQL ou de paramètres personnels.

Les cinq requêtes les plus coûteuses avant optimisation sont :

| Requête | Appels | Exécution cumulée | Moyenne par appel | Part du temps SQL |
| --- | --- | --- | --- | --- |
| Sélection du feed | 53 971 | 409,74 s | 7,592 ms | 93,23 % |
| Liste des matchs | 17 990 | 11,92 s | 0,662 ms | 2,71 % |
| Contrôle de compte et session | 89 918 | 7,32 s | 0,081 ms | 1,67 % |
| Disponibilité du match et verrou | 17 991 | 5,91 s | 0,329 ms | 1,35 % |
| Lecture des messages | 17 991 | 2,40 s | 0,134 ms | 0,55 % |

Les fenêtres SQL durent respectivement **299,630 s** et **301,188 s** : le déclenchement et la réponse
des snapshots prennent un peu de temps. Les moyennes par appel sont donc plus comparables que les
totaux bruts. Les compteurs SQL incluent les appels achevés à leurs propres frontières et ne sont pas
les compteurs HTTP. Les durées d’exécution incluent les attentes, ne mesurent pas la CPU seule et
ne fournissent aucun percentile SQL ; le suivi de planification reste désactivé.

Les plans du feed sont examinés avec `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)`, y compris le passage
automatique des plans personnalisés aux plans génériques. La configuration de cache des plans
de l’application reste inchangée. Le feed concentre le coût : les contrôles de compte/session,
les exclusions et les verrous n’ont pas été allégés.

### Modification retenue et vérifications de sécurité

Le CTE du lecteur calcule une seule fois les bornes de naissance et le cosinus de sa latitude.
L’ancien préfiltre numérique latitude/longitude, redondant avec PostGIS pour le feed, est retiré.
Les coordonnées valides et les présences fraîches restent exigées. `ST_DWithin` et le GiST
présélectionnent une fenêtre conservatrice ; **la distance Haversine exacte et les préférences
réciproques décident toujours de l’éligibilité**. La validation ciblée d’un swipe garde également
ce contrôle exact. Ce changement corrige aussi les omissions de profils proches aux pôles.

Les vérifications de session/révocation, compte actif, versions et retraits des consentements,
blocages dans les deux sens, swipes, matchs, modération et pagination restent appliquées.
Aucun cache d’autorisation ou de consentement, relèvement de timeout ou augmentation du pool
ne contribue au gain. Les détails modérés restent chargés après la sélection des profils éligibles.

Une tentative de regrouper les traits et réponses de toute la page coûtait davantage dans les plans
mesurés ; elle n’est pas retenue. Un index expérimental sur les consentements actifs n’était pas
utilisé et n’apportait pas de gain ; il a été supprimé. Aucun nouvel index applicatif n’est ajouté
par cette étape.

Validation : **18 tests passent** dans `discovery_postgres`, `discovery_contract` et
`mobile_auth_contract`. La nouvelle régression polaire contrôle le rayon réciproque, puis le
bannissement, l’ancienne version du consentement sensible et le retrait du consentement de
localisation, sur le feed et le swipe ciblé. Les régressions existantes couvrent notamment
concurrence, exclusions, curseurs chiffrés, limites de page et passage de ±180°.
**32 comparaisons de résultats complets** entre les deux SQL, sur huit comptes fictifs avec trois
rayons ou une mauvaise version de consentement, sont identiques, distances et contenus compris.

En complément, 24 exécutions de plan par version, intercalées, donnent une médiane de **2,970 ms
avant** et **2,650 ms après**. Ce diagnostic de requêtes seules confirme une réduction de calculs ;
il ne remplace pas la comparaison HTTP concurrente ci-dessous.

### Comparaison à 300 RPS

Les latences du tableau portent uniquement sur les maintiens de cinq minutes.

| Mesure | Avant | Après |
| --- | --- | --- |
| Débit servi pendant le maintien | 300,00 RPS | 299,91 RPS |
| p95 HTTP global | 76,184 ms | 26,352 ms |
| p99 HTTP global | 268,735 ms | 222,679 ms |
| p95 HTTP du feed | 97,474 ms | 32,436 ms |
| Maximum HTTP global | 699,167 ms | 1 779,306 ms |
| Erreurs HTTP / réponses métier invalides | 0 / 0 | 0 / 0 |
| Itérations perdues | 0 | 0 |
| Exécution SQL moyenne du feed | 7,592 ms | 5,481 ms |

Chaque profil complet émet exactement **103 533 requêtes HTTP**, dont quatre vérifications initiales.
Le total est donc **207 066**, sans erreur HTTP, réponse métier invalide ni itération perdue, rampes
comprises. Le maintien contient 89 999 réponses avant et 89 974 après ; l’écart à 90 000 provient
des réponses achevées aux frontières de la fenêtre, pas d’itérations que le générateur n’aurait pas
pu lancer. Les deux exécutions terminent avec le code k6 `0` et respectent les seuils configurés.

Le p95 global baisse d’environ **65 %** et la moyenne SQL du feed d’environ **28 %**. Le feed reste
responsable de **92 %** du temps SQL après modification. Ses lectures de blocs partagés restent
autour de 837 par appel ; le gain vient surtout des calculs retirés. Aucun fichier temporaire SQL
n’est lu ou écrit. Le cache est plus chaud après, avec zéro bloc lu contre 183 avant sur les deux
requêtes principales ; ce facteur doit être pris en compte dans l’interprétation.

Les ressources suivantes sont échantillonnées sur les profils complets, rampes comprises :

| Mesure | Avant | Après |
| --- | --- | --- |
| CPU PostgreSQL moyenne | 107,09 % | 86,15 % |
| CPU PostgreSQL maximale | 215,11 % | 137,44 % |
| CPU API moyenne | 42,15 % | 40,00 % |
| Acquisitions du pool en attente, maximum | 49 | 24 |
| Descripteurs API, maximum / fin | 1 045 / 44 | 1 045 / 44 |

Les pourcentages Docker expriment un CPU à 100 % et peuvent dépasser ponctuellement la limite
sur un relevé ; ils ne constituent pas un profilage CPU par requête. Aucun conteneur surveillé
n’a été arrêté, redémarré, tué par OOM ou déclaré `unhealthy`. Tous les relevés de readiness réussissent,
sans échantillon manquant ni erreur de collecte SQL.

**300 RPS mixtes sont validés pendant cinq minutes dans les deux conditions.** Il n’y a qu’une
exécution avant/après sur un ordinateur partagé avec k6 : les écarts de cache, de charge de l’hôte
et de contention empêchent de garantir exactement ce gain en production. Le p99 reste à environ
223 ms et le maximum augmente à 1,78 s ; les réponses les plus lentes restent à examiner.
Les débits supérieurs n’ont pas été retestés. Le test d’endurance de trente minutes et le scénario
de charge avec écritures sont exclus de cette intervention, à la demande de l’utilisateur.

### Application locale et nettoyage

Les 4 999 sessions temporaires, le rôle SQL et ses droits, les conteneurs de benchmark et leurs
fichiers secrets sont supprimés. La vérification retrouve **5 001 comptes, 5 000 matchs confirmés,
74 996 messages et 46 tables publiques** ; l’index expérimental est absent.

L’API, le worker outbox et le migrateur locaux utilisent l’image optimisée **`e2ba350009c7`** sous
`histae-api:local`. La version précédente reste disponible sous `histae-api:before-sql-profile-2026-10-12`.
PostgreSQL conserve son image et son volume. Le migrateur termine avec le code `0`, l’API est `healthy`,
le worker fonctionne et le smoke test passe. Le pool reste à **30 pour l’API**, **20 pour le worker**,
et le processus API confirme `nofile=16384`. Aucun déploiement distant n’a été effectué.
Les mesures, plans, résumés et preuves de nettoyage sont dans `.load-test/sql-profile-2026-10-12/`.

## Nettoyage et traces

Les sessions temporaires sont révoquées et les fichiers de jetons sont supprimés après chaque campagne.
Les conteneurs API/Redis de benchmark et leur fichier d’environnement secret sont retirés.
Les index installés et le rafraîchissement des présences fictives sont conservés ; les comptes, matchs et messages
sont conservés. L’API habituelle reste disponible après le test.

Les résumés k6, points de mesure, plans SQL et relevés de ressources sont conservés localement dans
`.load-test/local-2026-10-11/`, `.load-test/feed-optimization-2026-10-12/` et
`.load-test/spatial-feed-2026-10-12/`, ainsi que `.load-test/high-rps-2026-10-12/`,
`.load-test/nofile-2026-10-12/`, `.load-test/pool30-2026-10-12/` et
`.load-test/pool-comparison-2026-10-12/`, `.load-test/image-stats-2026-10-12/` et
`.load-test/sql-profile-2026-10-12/`, dossiers exclus de Git et du contexte de build Docker.
L’ACL ajoutée manuellement au socket Docker doit être retirée par l’utilisateur avec la commande fournie
à la fin de la campagne.
