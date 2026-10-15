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
