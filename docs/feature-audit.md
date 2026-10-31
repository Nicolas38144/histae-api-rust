# Audit fonctionnel — 15 octobre 2026

[Index](README.md) · [Contrat HTTP](http-contract.md) · [Tests](../tests/README.md) · [Livraison](production-readiness.md)

Cette revue porte sur l’API Rust, le dashboard et le site. Elle rapproche les règles documentées,
les accès aux données, les interfaces et les tests. Le dépôt mobile et le serveur distant ne sont pas
modifiés. Les résultats ci-dessous remplacent les anciens rapports de validation des continuations
et de durcissement ; les règles et procédures restent dans leurs guides respectifs.

## Problèmes reproduits et corrigés

| Problème | Correction | Preuve de régression |
| --- | --- | --- |
| Un blocage pouvait terminer avant le commit d’un match en cours de création, laissant ce match actif | Création et blocage partagent un verrou transactionnel sur la paire triée. Les accès mobiles vérifient aussi les blocages pour fermer l’accès aux anciennes lignes incohérentes | Le test PostgreSQL échoue avant correction et réussit après ; messages et historique refusés même après remise artificielle du match à `active` |
| Rejouer un like après création du match renvoyait `404` malgré la décision déjà enregistrée | Le rejeu retrouve le match encore ouvert ; consentements, localisation, préférences et blocages restent contrôlés. Aucun match terminé n’est rouvert | Le test PostgreSQL reproduit le `404` initial, puis vérifie UUID stable, absence de notifications supplémentaires, feed inchangé et refus après déplacement ou clôture |
| Une réponse de session admin tardive pouvait restaurer l’interface après son invalidation | La garde ignore les réponses antérieures à l’invalidation et vide le cache de prénoms. L’API continue de vérifier chaque autorisation | Un test de composant reproduit la course avant correction et vérifie ensuite la redirection vers la connexion |
| Une conversation admin pouvait conserver son état local en changeant de match | Le composant de conversation est identifié par l’UUID du match | Typage, suite de composants et build du dashboard réussis |
| Un test de requêtes tentait une migration avec le rôle applicatif limité | Contrôle de compatibilité sans DDL dans ce test ; migrations exécutées séparément avec le migrateur | Les deux tests de requêtes passent avec `histae_app`, sans élargir ses permissions |

Les deux collections Postman conservent les routes et formats existants. Leurs descriptions expliquent
les garanties de blocage et de rejeu. Un test d’inventaire compare désormais leurs **109 requêtes** au
contrat et vérifie l’usage du cookie admin à la place du JWT mobile.

## Revue par fonctionnalité

« Vérifié localement » décrit les contrôles ci-dessous ; cela ne remplace pas une validation fournisseur
ou une recette sur le serveur de production.

| Fonctionnalité | Contrôles locaux | Validation restante |
| --- | --- | --- |
| OTP et sessions mobiles | Payload Sweego, signatures, délais, états de livraison, rotation/rejeu des refresh, révocation et guards HTTP | Livraison SMS et webhook Sweego réels à travers le tunnel |
| Administration et WebAuthn | Rôles, superadmin unique, bootstrap, sessions/cookies, protection Origin, permissions et invalidation de l’interface | Passkeys des appareils réels et procédure de récupération sur le domaine déployé |
| Consentements et profils | Retraits concurrents, effacement immédiat de la présence/données sensibles, profils, préférences et réponses modérées | Versions et validation juridique réelles |
| Catalogues et plans | Traits/questions, contrats mobile/admin, prix et projections Free/Premium | Correspondance des Prices et réglages du compte Stripe déployé |
| Feed et swipes | Réciprocité des filtres, consentements, exclusions, pagination, présence fraîche, likes immuables et rejeu après match | Nouvelle campagne de débit sur la livraison réelle si sa capacité doit être certifiée |
| Matchs et messages | Participants seuls, révélation mutuelle, idempotence, accusés, pause, clôture, blocage concurrent et absence de réouverture | Recette mobile et reconnexion réseau |
| Continuations et crédits | Fenêtres fixes, choix privés, réservations, débit unique des deux participants Free, plafonds, changements de semaine et d’abonnement | Présentation mobile du coût et des décisions |
| Récompenses publicitaires | Signature P-256, cache borné, altération/rejeu, liaison intention/compte/unité, annulation, concurrence et effacement | AdMob réel, SDK mobile, UMP et textes validés ; fournisseur désactivé par défaut |
| Photos et modération | Codec réel, formats/métadonnées, idempotence, stockage S3, décisions versionnées, visibilité et réconciliation | Fournisseur de modération et URL signées via `storage.histae.com` sur le serveur |
| Stripe et revenus | Checkout/portail avec fournisseur contrôlé, signatures, déduplication, ordre des événements, réconciliation et estimations SQL | Checkout/paiement, webhook et renouvellement réels ; aucune opération Stripe effectuée pendant cette revue |
| Signalements, modération et droits | Transitions, audit d’accès, exports propriétaires, effacement et tâches durables sur PostgreSQL | Recette opérateur, sauvegarde/restauration complète et validation RGPD organisationnelle |
| Notifications, SSE et outbox | Déduplication, éligibilité, claim exclusif, reprise, Redis réel, diffusion entre instances et absence de rejeu SSE | FCM et notifications mobiles réelles |
| Exploitation et schéma | Installation neuve, migration répétée, upgrade des crédits, rôles limités, verrous et traitements bornés | Timer, alertes, certificats et reprise sur le serveur cible |
| Dashboard | Typage, lint, build, composants, navigation et scénarios navigateur | Recette sur le domaine public avec données et appareils représentatifs |
| Site | Export statique, typage, lint, SEO/navigation, règles de crédits et prix dans les quatre langues | Publication, validation juridique et liens vers les applications mobiles |

## Résultats de la campagne

| Vérification | Résultat |
| --- | --- |
| Rust, `cargo test --locked --all-targets` | **312 tests réussis**, dont 204 tests unitaires ; les intégrations désactivées par feature ne sont pas comptées comme validées ici |
| PostgreSQL / Redis / S3 isolés | **53 tests réussis** dans 19 exécutables de tests, avec les features d’intégration activées |
| Migration | Installation neuve, réexécution et upgrade depuis l’ancien quota réussis ; usage et nouveaux accords conservés |
| Permissions PostgreSQL | Suites métier avec `histae_app`, DDL avec `histae_migrate` ; fixtures créant bases/schémas séparées sous compte de test administrateur |
| Inventaire Axum, documentation et Postman FR/EN | **109 routes** dans chaque inventaire ; cookies admin contrôlés |
| Clippy, tous targets et toutes features, warnings refusés | Réussi |
| Dashboard : types, lint, build et types des tests | Réussis |
| Dashboard : tests unitaires/composants | **57 tests réussis**, 18 fichiers |
| Dashboard : Playwright avec API simulée | **2 scénarios réussis**, dont passkey virtuelle |
| Dashboard et API réels sur pile jetable | Enrôlement et connexion WebAuthn, cookie `HttpOnly; SameSite=Strict`, CRUD des traits, ouverture des 15 écrans, rejet d’une origine étrangère et déconnexion réussis |
| Parcours HTTP mobile sur pile jetable | Onboarding, consentements, profil/préférences/présence, feed, swipe rejoué, messages privés/idempotents, pause, continuation bilatérale, blocage et retrait de localisation réussis |
| Site : types, lint et build | Réussis : 49 pages statiques produites, 44 pages localisées validées et 28 URL indexables dans le sitemap |
| Audit pnpm des dépendances de production | Aucune vulnérabilité signalée pour le site et le dashboard au moment du contrôle |
| Image API | Construction réussie depuis les sources corrigées sous l’étiquette locale `histae-api:feature-audit` |

Le build Vite signale encore un chunk principal supérieur à 500 Ko. Il s’agit d’un sujet de poids de
chargement du dashboard ; ce n’est pas un échec de build. L’audit pnpm ci-dessus porte sur les dépendances
de production, pas sur tous les outils de développement ni sur les images système.

### Isolation

Les intégrations utilisent des conteneurs temporaires PostgreSQL 18.6/PostGIS 3.6.4, Redis 7.4.2 et
SeaweedFS 4.45, des mots de passe aléatoires et des publications sur loopback uniquement. Les données
PostgreSQL et S3 sont jetables. Les conteneurs, fixtures et secrets temporaires sont supprimés après
les tests. Aucun OTP n’est envoyé, aucun achat n’est réalisé et la base de développement partagée
n’est pas utilisée comme cible de ces tests.

L’image `histae-api:feature-audit` sert à valider cette campagne. Les services déjà démarrés ne sont pas
recréés : la construction d’une image ne constitue pas son déploiement.

## Documentation maintenue

- Suppression des anciens rapports `continuation-validation.md` et `security-hardening.md`, remplacés par cette synthèse.
- Conservation des guides d’intégration, administration, exploitation, crédits, sécurité du tunnel et tests de charge.
- Conservation des mesures de charge datées, avec un avertissement sur leur configuration historique.
- Correction de l’initialisation PostgreSQL, du nombre actuel de tables, des références au backend retiré et des comptes de routes.
- Dashboard : README, sécurité, résumé fonctionnel et roadmap alignés sur l’API Rust, les rôles, les crédits et la supervision.
- Site : export statique, pages légales déjà présentes, Premium annuel à 48 €, crédits et limites actuelles dans la roadmap.

## Rejouer et déployer

Les commandes maintenues se trouvent dans [Tests](../tests/README.md), [Développement](development.md)
et [Déploiement](container-deployment.md). Le lanceur PostgreSQL isolé exige une image construite depuis
les sources à vérifier : une ancienne image peut faire échouer un test de schéma sans incriminer le code actuel.

Pour appliquer les corrections, reconstruire et recréer les services avec les manifests et le mode
**déjà utilisés** sur l’installation. Puis contrôler santé, connexion admin, feed/swipes et blocages.
Les versions, secrets et données de production ne doivent pas être remplacés par ceux des fixtures.

Cette campagne ne relance pas les benchmarks et ne valide pas le tunnel distant, les paiements/SMS/publicités
réels ou la conformité juridique. Ces étapes restent dans la [feuille de route production](../roadmap-production.md).

## Complément — achats multicanaux, 22 octobre 2026

Ce complément couvre aussi `histae-app`, contrairement au périmètre historique du 15 octobre ci-dessus.
Le parcours courant est décrit dans [Achats Apple/Google et site](store-billing.md) : stores natifs dans
l’app, Stripe sur le site après UUID/téléphone/OTP, droits serveur communs au compte et rapports
financiers distincts dans le dashboard. Le contrat et Postman FR/EN comptent désormais 130 routes.

Les contrôles ont corrigé les risques de projection Stripe écrasant des droits natifs, de réattribution
à un autre compte, de rejeu d’un token remplacé, de validation hors environnement et de dates de
période non respectées. Les sessions du site ne sont pas des JWT mobiles ; les OTP sont liés à leur
challenge et les tentatives consommées atomiquement. Les imports contradictoires sont entièrement
annulés dans la transaction, avec relecture du rôle superadmin.

Les responsabilités sont séparées entre services/ports Rust, adaptateurs PostgreSQL/HTTP,
vérificateur privé officiel et SDK natifs Flutter. Les résultats datés, scénarios et limites sont dans
[Tests](../tests/README.md#achats-multicanaux). Les essais sur appareils et fournisseurs, la publication
juridique et le déploiement final restent ouverts dans [la feuille de route](../roadmap-production.md#prod-08--achats-natifs-et-stripe-sur-le-site).

## 28 octobre 2026 — inactivité des conversations confirmées

Politique vérifiée : archivage accessible à J10, avertissement des deux participants à J23,
fermeture et suppression à J30 sans rétention supplémentaire. Un nouveau message renouvelle le
délai et désarchive sans nouveau crédit ; lecture, connexion et rejeu ne le font pas.
Free et Premium suivent le même cycle.

- **235 tests unitaires**, **38 tests de contrat/inventaire** et **37 tests PostgreSQL** réussis.
- Migrations sur bases neuve et ancienne, préavis initial complet et absence de prolongation au redéploiement vérifiés.
- Concurrence avec l’envoi d’un message, lecture et rejeu sans prolongation, avertissements uniques,
  push obsolètes refusés, suppression par lots et conservation séparée des signalements vérifiés.
- Formatage et Clippy toutes cibles/features avec warnings refusés passent.
- Dashboard : typage, lint, build et 12 tests ciblés réussis. Site : typage, lint, build statique
  et 7 tests marketing multilingues réussis.
- Flutter : traductions ARB et fichiers Dart associés vérifiés statiquement ; trois tests ajoutés,
  non exécutés faute de SDK Flutter/Dart sur ce poste.

Le PostgreSQL utilisé était jetable et a été supprimé avec ses fixtures et secrets temporaires.
Aucun SMS ni push réel n’a été envoyé. La livraison FCM sur appareil reste à vérifier ; les
avertissements et la purge nécessitent l’exécution régulière de la maintenance existante.

### Refactoring de l’API après cette vérification

Les transactions et requêtes bornées de maintenance sont regroupées dans `matches/pg/maintenance.rs` ;
le service conserve le pilotage, les compteurs et le suivi opérationnel. Les avertissements utilisent
des résultats SQL typés dans `matches/pg/inactivity.rs`. Les verrous et l’ordre des suppressions
restent conservés dans la même transaction.

Les projections mobile et administration partagent la conversion d’un match et reçoivent une date
explicite issue de l’horloge du service. Le calcul d’archivage ne lit plus l’horloge système dans le
domaine. Les régressions couvrent cette injection et la frontière exacte de J23, sans modifier les
règles 10/23/30 jours, les crédits ou les réponses HTTP.

Après refactoring : **236 tests unitaires**, **35 tests de contrat/inventaire** (matchs,
découverte, notifications, routes) et **25 tests PostgreSQL** (matchs, inactivité, notifications,
maintenance/outbox) réussis. Formatage et Clippy toutes cibles/features avec warnings refusés
passent. Les migrations ont été rejouées sur le PostgreSQL jetable, supprimé à la fin du contrôle.

## 28 octobre 2026 — accès de revue Google Play

Ajout d’un accès mobile de revue activé explicitement côté serveur, avec numéro fictif réservé
et code réutilisable privés. Les identifiants réels ne figurent ni dans Git, ni dans Flutter,
ni dans Postman. Le Compose les transmet uniquement à l’API. Les autres comptes conservent leur
OTP habituel, et les limitations IP/numéro restent appliquées au compte de revue.

La création et la session du compte dédié sont atomiques. Aucun compte normal existant n’est
adopté ; une contrainte PostgreSQL et la route de promotion interdisent l’administration.
Bannissement, suppression et tombstone restent bloquants, y compris si une suppression se termine
pendant l’insertion. Le code fixe n’est pas utilisable pour le parcours d’achat web.

Vérifications : **237 tests unitaires**, **26 tests de contrat/inventaire** et **15 tests PostgreSQL**
réussis, soit **278 tests**. Clippy toutes cibles/features avec warnings refusés, formatage,
configuration Compose de développement et absence des identifiants privés dans les changements
publics vérifiés. Migration d’une base ancienne, réexécution et maintien des marqueurs de revue
vérifiés sur PostgreSQL jetable, supprimé à la fin.

Les [consignes de préparation et de publication](google-play-review.md) indiquent l’inscription initiale puis la connexion,
le profil à compléter et le texte anglais à fournir à Google. Le déploiement sur le serveur et la
connexion sur appareil restent à effectuer ; aucun SMS ni achat fournisseur réel n’a été déclenché.

### Ajustement — création par le bouton d’inscription

La première création du compte de revue exige désormais une intention d’inscription encore valide,
enregistrée avec `registration: true`. Une vérification isolée, une connexion pour un compte absent
ou une intention expirée ne crée plus de compte. Après inscription, le code fixe reste réutilisable
pour retrouver ce même compte par « Connexion ».

L’intention temporaire conserve le délai de renvoi, les quotas et l’idempotence, avec seulement le
hash du numéro, sans téléphone chiffré ni événement SMS/outbox. Les formulaires Flutter existants
collectent déjà profil, préférences et consentements avant l’étape téléphone ; leur enregistrement
reste celui du parcours normal. Le guide et les descriptions Postman FR/EN reflètent cet ordre.

Après cet ajustement : **25 tests de contrat/inventaire** et **17 tests PostgreSQL** réussis,
formatage et Clippy toutes cibles/features sans avertissement. Le PostgreSQL isolé a été supprimé.
Le numéro et le code privés du `.env` ne sont pas modifiés.

### Premium offert au compte de revue

La projection centrale `effective_subscription_at` accorde Premium sans expiration aux comptes
de revue actifs de rôle `user`, y compris ceux déjà inscrits. Aucun achat, facture ou projection
fournisseur n’est créé ; un historique Stripe/store existant est conservé. Les continuations et
le dashboard lisent les mêmes droits. Les consentements et protections d’authentification restent
appliqués. Aucune variable supplémentaire ni modification Flutter n’est nécessaire.

La sélection SQL distingue l’éligibilité de revue, les trois sources de droits et leur priorité.
Les tests de facturation de revue sont isolés dans `tests/play_review/premium.rs` et partagent les
fixtures d’identité et de nettoyage du test PostgreSQL existant.

Vérifications : **27 tests de contrat/inventaire** et **45 tests PostgreSQL** réussis, soit **72 tests**.
Les migrations sur base neuve, mise à niveau et réexécution passent sur PostgreSQL jetable,
supprimé avec ses fixtures et secrets temporaires. Aucun fournisseur de paiement n’a été contacté.

### Premium offert depuis le dashboard

Ajout de `PATCH /api/admin/users/:id/complimentary-premium` et de la table indépendante
`complimentary_premium_grant`. Le superadmin peut offrir ou retirer Premium sans expiration ;
les droits sont revalidés en base et chaque changement est journalisé atomiquement. Les rejeux
n’ajoutent pas d’audit. Les comptes de revue restent en lecture seule pour cette action.
Le détail administratif expose l’offre manuelle et le marqueur de revue séparément.

L’offre ne crée aucun achat ou revenu et n’annule aucun abonnement fournisseur. Les mutations
partagent les verrous de continuation ; l’anonymisation et la suppression retirent l’offre.
Le dashboard propose confirmation, retrait et erreurs accessibles dans la boîte de dialogue,
avec les traductions françaises, anglaises, espagnoles et italiennes des actions et de l’audit.
Les collections Postman et l’inventaire des **140 routes** sont alignés. Aucune variable `.env`
supplémentaire n’est nécessaire. Déployer les migrations/API avant le dashboard.

Vérifications : **239 tests unitaires Rust**, **27 tests de contrat/inventaire** et **53 tests PostgreSQL**
réussis, soit **319 tests API**. **144 tests dashboard**, typage, lint et build de production réussis.
Les migrations neuves, historiques et répétées conservent les offres ; le PostgreSQL jetable a été
supprimé avec ses fixtures et secrets temporaires. Aucun SMS ou paiement réel n’a été envoyé.

## Parcours web de suppression — vérification du 29 octobre 2026

Les gardes HTTP du parcours web vérifient l'origine exacte, le cookie actif et le budget par compte
avant la validation des champs JSON. Le parseur global conserve ses contrôles de format, syntaxe et
taille. Les retraits partiels sont orchestrés dans `privacy/web/service.rs`, sans SQL ni règles de
consentement dupliquées dans le transport. L'effacement reprend le service et la file existants.
Le client HTTP partagé du site omet `Content-Type` en l'absence de corps ; il n'envoie donc plus du
JSON vide pour demander un jeton, se déconnecter ou ouvrir le portail.

| Vérification locale | Résultat |
| --- | --- |
| Rust, tous les targets avec fonctionnalités par défaut | 373 tests réussis |
| PostgreSQL isolé : parcours web, stores, RGPD, effacement, profils | 17 tests réussis, dont six nouveaux scénarios web |
| Migrations neuves, upgrades et répétition | Réussis ; base et secrets temporaires supprimés |
| Inventaire et collections Postman FR/EN | 140 routes, cookies web et origine contrôlés |
| Formatage et Clippy, tous les targets et fonctionnalités | Réussis, avertissements refusés |
| Client du site : typage, lint ciblé et tests avec `fetch` simulé | Réussis, deux tests |

Les tests couvrent les cookies expirés et ambigus, les comptes bannis/supprimés, les tentatives visant
un autre compte, les budgets partagés, les retraits idempotents et leur audit, les jetons de confirmation
et l'invalidation des sessions. La connexion reste disponible sans facturation ni onboarding terminé ;
Checkout et portail restent désactivés lorsque le paiement web est coupé. Aucun fournisseur réel ni pile
déployée n'a été utilisé. Le parcours navigateur sur le serveur cible reste à vérifier après livraison
conjointe de l'API, du site et du routage proxy. Aucun changement de `.env` ou de schéma n'est nécessaire.

## Lecture administrative des messages — vérification du 31 octobre 2026

Le dernier contrôle exige un signalement conservé liant exactement la conversation et ses deux
participants, pour admin comme superadmin, quel que soit son statut. L'indicateur `can_view_messages`
reste administratif ; la lecture réévalue l'autorisation sur chaque page. Les lectures et les deux
audits restent transactionnels. Le refactoring isole les requêtes et projections dans
`administration/pg/conversations.rs` et ne mémorise que le texte SQL, sans mettre les droits en cache.

Les cinq accès privés au pool dans les nouvelles fixtures ont été remplacés par l'acquisition publique
d'une connexion. Le scénario utilise désormais de vrais messages et des acteurs de chaque rôle :
profil/autre conversation/participants incohérents refusés, deux orientations et trois statuts acceptés,
contenu et pagination isolés, curseur refusé après suppression du signalement, absence d'audit
de consultation pour les refus et transaction annulée si l'audit échoue.

Validation locale : **373 tests Rust par défaut et 29 tests PostgreSQL réussis** (administration,
matchs, modération et Premium offert), formatage et Clippy sans avertissement. Les migrations neuves,
les upgrades et leur répétition passent ; la base et les secrets temporaires sont supprimés. Le contrat,
le guide admin et Postman FR/EN décrivent la règle ; l'inventaire reste à 140 routes. Aucun changement
de `.env` ni migration supplémentaire nécessaire. La validation du dashboard déployé reste à faire.
