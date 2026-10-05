# Roadmap de validation et de mise en production

État de référence : **5 octobre 2026**. Ce document centralise le travail restant après les lots S00–S29, le refactoring et les corrections de la revue de migration. Il ne redéfinit pas l’architecture et ne constitue ni une certification de sécurité ni une validation juridique.

L’API est encore en développement. Le prochain objectif est une première mise en production maîtrisée. Une migration de données utilisateurs existantes ne doit être organisée que si des données à conserver sont effectivement identifiées. À terme, le dépôt NestJS, ses scripts, sa base et ses conteneurs doivent pouvoir être retirés sans dépendance résiduelle du système Rust.

## 1. Acquis, limites et règle de clôture

Les preuves locales sont consignées dans [le bilan des corrections](docs/production-readiness.md). Les comptes rendus [S28](docs/migration/s28-parity.md) et [S29](docs/migration/s29-cutover.md) restent des historiques : une réussite passée ne valide pas une nouvelle image, configuration ou infrastructure.

| Élément | État connu | Preuve restant à obtenir |
| --- | --- | --- |
| Implémentation S00–S29 et organisation par domaines | Livrées ; architecture documentée | Régression sur chaque candidate à la livraison |
| Validation locale du 5 octobre | Formatage et Clippy réussis ; 312 tests réussis, 0 échec/ignoré, 47 cibles | Même validation sur la candidate et environnement Linux cible |
| PostgreSQL, Redis, S3, codec, modération | Intégrations locales exécutées | Configuration réelle, charge, pannes et restauration |
| Inventaire HTTP | 100 couples méthode/chemin documentés | Comparaison comportementale exhaustive sur les applications assemblées |
| Comparateur différentiel | Outil présent ; corpus générique limité à la santé et à une route inconnue | Corpus métier, autorisations et effets persistés |
| Smoke API et métriques | Binaire assemblé testé ; listener privé protégé et compteurs actifs | Proxy/TLS, exposition réseau réelle, réception des alertes |
| Docker, dashboard et restauration locale | Des exercices figurent dans S29 | Rejouer sur la candidate ; restauration complète hors machine et WebAuthn réel |
| Fermeture des processus | Fermeture contrôlée observée localement | SIGTERM/SIGKILL, drainage des requêtes et reprise des workers sur Linux |
| Espace de compilation | Ancien `target/` nettoyé ; profils dev/test allégés | Surveiller les caches, images, logs et volumes pendant l’exploitation |

**Ne pas rouvrir les corrections déjà livrées comme si elles manquaient.** Les tâches ci-dessous demandent de compléter la couverture et d’obtenir les preuves manquantes ; toute divergence découverte devient une correction accompagnée d’un test.

Priorités :

- **B — bloquant avant ouverture publique** : la preuve ou la décision est obligatoire.
- **F — bloquant pour la fonctionnalité concernée** : obligatoire dès que celle-ci est proposée. Son retrait ou sa désactivation exige une décision produit explicite et des tests du comportement résultant.
- **P — suivi après lancement** : récurrent ; son responsable et son dispositif doivent être définis avant lancement.

Toutes les cases ouvertes concernent une preuve ou une décision encore à obtenir. Pour clôturer une tâche, conserver : date, responsable, version exacte des sources et de l’image, configuration expurgée, environnement, commande/protocole, jeu de données, résultat et limites. Ne jamais joindre de `.env`, secret, téléphone, contenu privé ou sortie brute sensible. Les preuves contenant des données sensibles restent dans un espace à accès contrôlé, avec seulement leur référence ici.

Les responsables ci-dessous désignent des **rôles à attribuer**, pas des personnes déjà engagées. Toute dérogation doit préciser son risque, son approbateur, sa durée et sa mesure compensatoire ; elle ne doit pas masquer un contournement d’autorisation, une perte de données ou un effet externe non maîtrisé.

## 2. Ordre de réalisation

Les identifiants R00–R18 sont des lots de validation et d’exploitation, distincts des anciens lots de migration S00–S29.

| Lot | Objectif | Priorité | Prérequis principaux | Responsable proposé |
| --- | --- | --- | --- | --- |
| R00 | Décider périmètre, budgets et responsabilités | B | Aucun | Produit + backend + exploitation |
| R01 | Rendre le dépôt Rust complètement autonome | B | R00 | Backend |
| R02 | Qualifier une livraison reproductible | B | R01 | Backend + exploitation |
| R03 | Terminer la parité HTTP et métier | B | R02 | Backend + QA |
| R04 | Valider les vrais clients et WebAuthn | B | R02, R03 | Mobile + dashboard + QA |
| R05 | Qualifier PostgreSQL, migrations et concurrence | B | R02 | Backend + exploitation PostgreSQL |
| R06 | Valider Sweego, Stripe et FCM | B pour OTP ; F pour billing/push | R00, R02 | Backend + responsables fournisseurs |
| R07 | Qualifier photos, stockage et modération | B | R02, choix S3 R00 | Backend + modération |
| R08 | Prouver la reprise après panne | B | R05, adaptateurs R06/R07 | Backend + exploitation |
| R09 | Revue sécurité et chaîne de dépendances | B | R02 ; campagne finale après corrections | Sécurité + backend |
| R10 | Qualifier l’infrastructure de production | B | R00, R02 | Exploitation |
| R11 | Prouver sauvegarde et restauration hors machine | B | R05, R07, R10, politique R15 | Exploitation + backend |
| R12 | Mesurer charge, endurance et capacité | B | R03, R05, R07, R10, collecte R13 | Performance + backend + exploitation |
| R13 | Valider supervision, alertes et diagnostic | B | R10 ; seuils finaux après R12 | Exploitation + backend |
| R14 | Organiser maintenances et réponse aux incidents | B | R08, R11, R13 | Exploitation + support |
| R15 | Finaliser droit, conservation et gouvernance | B | R00 ; travaux à lancer immédiatement | Produit + DPO/juriste + sécurité |
| R16 | Répéter le déploiement et autoriser l’ouverture | B | R01–R15 applicables clos | Responsable de livraison |
| R17 | Retirer définitivement le système NestJS | B avant suppression, pas avant pilote Rust | R03, R11, R16, observation validée | Backend + exploitation |
| R18 | Pérenniser les contrôles après lancement | P | R16 | Responsables désignés en R00 |

Commencer R00/R01 et les démarches externes R06/R15 sans attendre les tests de charge. Préparer R13 avant R12 pour mesurer la campagne, puis calibrer les alertes sur les résultats. Les lots peuvent avancer indépendamment lorsque leurs prérequis sont disponibles.

## 3. R00 — Décisions qui conditionnent les validations

- [ ] Définir le périmètre de lancement : plateformes clientes, fonctions Premium, push, régions, nombre initial d’utilisateurs et croissance envisagée.
- [ ] Fixer les volumes de référence : comptes, swipes retenus, matchs, messages, notifications, photos, audits, exports et effacements simultanés ; distinguer stock et trafic de pointe.
- [ ] Fixer les objectifs de service : disponibilité, p95/p99 par parcours, taux d’erreur technique, délai de livraison outbox, délai de modération et d’effacement. Distinguer traitement accepté et effet réellement terminé.
- [ ] Fixer le **RPO** (perte de données maximale admissible) et le **RTO** (temps maximal de rétablissement), ainsi que la durée acceptable d’un mode dégradé.
- [ ] Confirmer la cible actuelle : serveur de 16 Gio, PostgreSQL mono-nœud plafonné à 7 Gio, Redis TLS, SeaweedFS server et modération colocalisés. Accepter explicitement le point de panne unique ou traiter une incompatibilité démontrée avec les objectifs ; ne pas présenter cette topologie comme hautement disponible.
- [ ] Confirmer stockage objet durable, localisation, DNS, domaines API/admin/S3, terminaison TLS, proxy de confiance, région fournisseurs et séparation dev/préproduction/production.
- [ ] Décider si les données des anciennes bases sont des fixtures jetables ou des données à transférer. Identifier aussi les objets S3 et références fournisseurs associés ; une base seule ne suffit pas.
- [ ] Désigner responsables et suppléants : livraison, incidents, sauvegardes, facturation, sécurité, modération et demandes RGPD. Fixer budgets hébergement, SMS, stockage, transfert, supervision et seuils d’alerte de coût.

**Terminé :** décisions écrites, chiffrées lorsque nécessaire et attribuées. Aucun objectif RPS, disponibilité ou délai juridique ne doit être inventé dans le code pour combler une absence de décision.

## 4. R01 — Autonomie du projet Rust

Références : [architecture](docs/architecture.md), [déploiement](docs/container-deployment.md), [tests](tests/README.md), [historique](docs/migration/history.md).

- [ ] Exécuter installation, compilation, initialisation SQL/S3, smoke, outbox, maintenance et bootstrap admin depuis une machine propre disposant uniquement du dépôt Rust et des secrets autorisés.
- [ ] Inventorier chemins absolus, liens vers le dépôt NestJS, scripts, fixtures, catalogues, schémas, modèles et fichiers montés ; éliminer toute dépendance d’exécution ou de validation indispensable à NestJS.
- [ ] Rapatrier et adapter les politiques encore uniquement historiques : rétention, journalisation, droits RGPD, reprise après panne, fournisseurs, sessions et gouvernance de modération. Conserver les sources historiques utiles comme références archivées, sans en dépendre pour exploiter Rust.
- [ ] Documenter toutes les variables réellement acceptées, leurs valeurs par défaut, sensibilité, validation et mode d’injection par environnement. Conserver le `.env` local ignoré ; **ne pas créer de `.env.example`**. La configuration de production doit être provisionnée séparément.
- [ ] Vérifier qu’aucun secret, fixture privée ou sortie de test n’entre dans une image, un paquet de livraison ou une preuve archivée.
- [ ] Garder explicite le runtime Node/Sharp du codec photo : supprimer NestJS ne supprime pas ce besoin. Documenter aussi les dépendances et modèles du service Python de modération.
- [ ] Fournir les procédures d’exploitation Linux équivalentes aux scripts PowerShell nécessaires, puis les exécuter sur une machine vierge. Docker et les tests d’infrastructure locaux Windows passent par WSL.

**Terminé :** une autre personne installe et exploite une pile Rust neuve sans accès au dossier, à la base ni aux conteneurs NestJS. Les seuls éléments externes requis sont inventoriés. **Risque :** un chemin de développement ou une politique non transférée rendrait la suppression prématurée de NestJS irréversible en pratique.

## 5. R02 — Livraison reproductible et tests de base

- [ ] Exécuter sur la candidate les commandes documentées, avec infrastructure isolée et codec installé :

  ```text
  cargo fmt --all -- --check
  cargo clippy --locked --all-targets --all-features -j 2 -- -D warnings
  cargo test --locked --all-targets --all-features --no-fail-fast -j 2
  ```

- [ ] Refaire compilation et tests pertinents sous Linux, puis construire l’image finale et lancer ses vrais binaires. Vérifier la feature `webauthn-probe` nécessaire au moteur WebAuthn et la compatibilité de la toolchain avec le lockfile.
- [ ] Vérifier OpenSSL, certificats CA, bibliothèques natives, Sharp/libvips et décodage HEIC dans l’image ; une réussite MSVC locale ne valide pas ces composants Linux.
- [ ] Tester chaque CLI et son code de sortie : configuration invalide, migration refusée, stockage inaccessible, tâche réussie et interrompue. Ne pas confondre le code de sortie d’un wrapper de terminal avec celui du binaire.
- [ ] Exécuter le smoke sur l’image finale avec la configuration de préproduction : santé, erreurs, garde mobile/admin et métriques privées. Conserver l’image identifiée par digest, ses dépendances et la procédure de reconstruction.
- [ ] Vérifier absence de tests ignorés indispensables, données résiduelles et faux succès dus à un service non lancé. Tester aussi les modes de configuration réellement supportés, pas uniquement `--all-features`.

**Terminé :** commandes reproductibles et preuve sur l’image destinée au déploiement. Le nombre de tests n’est pas un critère suffisant de couverture. Aucun workflow CI n’est ajouté sans demande explicite ; ces contrôles restent exécutables localement et sur un environnement de qualification.

## 6. R03 — Parité complète du contrat HTTP et des règles métier

Références : [contrat HTTP](docs/http-contract.md), [S28](docs/migration/s28-parity.md), binaire `contract-compare`, corpus sous `tests/contract/`.

- [ ] Construire une matrice des **100 endpoints** avec lien vers source NestJS, handler Rust, tests existants et scénarios manquants. Vérifier l’inventaire sur les routeurs réellement assemblés, y compris routes inconnues, méthode non permise et slash final ; l’inventaire textuel ne prouve pas le comportement.
- [ ] Pour chaque endpoint : méthode/URL, path/query, headers, body, authentification, permissions, validation, valeurs par défaut, code HTTP, JSON, content type, erreurs et effets persistés/externes.
- [ ] Étendre le corpus différentiel : succès, entrées invalides, absence de ressource, mauvais propriétaire, compte suspendu/effacé, consentements absents/périmés/retirés, panne de dépendance et rejeu idempotent.
- [ ] Comparer champs absents versus `null`, listes vides, ordre, pagination/cursors/offset compatible, tri stable, dates calendrier, UTC, fractions temporelles, nombres et montants `bigint` sérialisés en chaînes.
- [ ] Caractériser champs inconnus, types JSON incorrects, doublons de paramètres/headers selon le contrat, coercitions numériques, UUID canoniques, limites de longueur en caractères/octets, Unicode et normalisation.
- [ ] Rejouer limites globales et photo, JSON mal formé, type inconnu, multipart incomplet, requêtes sans body et octets bruts des webhooks HMAC. Vérifier que le proxy ne change pas ces contrats.
- [ ] Comparer 401/403/404/409/429, enveloppe `{ "error": { "code", "message" } }`, headers défensifs, CORS, prévols et headers de quota lorsqu’ils sont prévus. Ne pas exposer détails SQL/fournisseur ou erreurs du framework.
- [ ] Prouver qu’un JWT mobile, même associé à un rôle admin, n’autorise jamais une route administrative. Tester chaque route sensible avec identité incorrecte et ressource tierce, pas seulement les extracteurs isolés.
- [ ] Comparer notifications/outbox, audits, transactions et absence d’effets sur requête refusée : une réponse HTTP identique ne suffit pas.
- [ ] Rejouer SSE : framing, content type, heartbeat, reconnexion, expiration, révocation, onboarding, connexion lente et panne Redis. Documenter le relais best-effort et la récupération via les données durables, sans promettre une livraison SSE exactement une fois.
- [ ] Faire valider les différences intentionnelles déjà documentées : runtime admin Rust sans champs Node/V8, export de date sous référence UTC. Toute autre divergence doit être corrigée ou explicitement approuvée.

**Protocole :** deux applications et jeux de données équivalents mais isolés, UUID générés, bases/buckets/espaces Redis distincts et fournisseurs simulés ou dédiés. Normaliser seulement les valeurs dynamiques justifiées ; ne pas masquer statut, champ manquant, ordre ou effet métier. Ne jamais rejouer des écritures de production sur deux systèmes actifs pour comparer.

**Terminé :** chaque endpoint possède des preuves positives et négatives, toute divergence est résolue ou approuvée, et le corpus est rejouable avant suppression de NestJS. **Risque :** conclure à la parité à partir du seul nombre de routes ou des tests unitaires.

## 7. R04 — Clients mobiles, dashboard et administration réelle

- [ ] Valider sur les clients effectivement distribués : inscription OTP, majorité et date de naissance, textes juridiques courants, profil, préférences, présence, découverte, match, messages, blocages, signalements, Premium, export et suppression.
- [ ] Tester réseau intermittent, délai de réponse, retour arrière, reprise après fermeture de l’application, double clic et retry. Le mobile ne doit pas lancer de refresh concurrents ni réessayer aveuglément un effet incertain.
- [ ] Vérifier rotation et rejeu des refresh tokens, familles, révocation ciblée/globale, suppression des appareils push, compte désactivé et expiration pendant une connexion SSE.
- [ ] Exécuter de vraies cérémonies WebAuthn avec navigateur et authentificateur : enrôlement, connexion, vérification utilisateur, credential découvrable, challenge/bootstrap réutilisé ou expiré, origine/RP incorrects et compteur.
- [ ] Rejouer réauthentification récente, gestion des passkeys et sessions : interdiction de retirer la passkey courante, la dernière active et la session courante par révocation ciblée ; historique sans secrets.
- [ ] Vérifier en local `http://localhost:5173`, RP `localhost` et proxy Vite `/api` ; puis sur le domaine HTTPS final, cookie `__Host-…; Secure; HttpOnly; SameSite=Strict`, origine exacte et absence de cookie trop largement partagé.
- [ ] Préparer deux passkeys distinctes par administrateur, accès de secours contrôlé, perte d’appareil, départ d’un administrateur et compromission. Le bootstrap hors bande doit être audité sans enregistrer son secret.
- [ ] Tester le dashboard réel : contenus masqués dans les listes, motif obligatoire avant détail/signature, audit avant accès, revue photo avec trois contrôles, confirmation de suppression d’une question avec ses réponses.

**Terminé :** parcours validés sur une matrice explicite de clients/navigateurs supportés et sur l’origine finale. Les authentificateurs virtuels restent utiles aux régressions mais ne remplacent pas toutes les cérémonies réelles.

## 8. R05 — PostgreSQL, migrations, concurrence et volume

- [ ] Qualifier la chaîne `001_baseline_20260905` → `017_postgres_discovery` → `018_postgres_admin_webauthn_state` depuis une base vide et depuis chaque état encore supporté. Vérifier interruption/reprise et refus d’une version inconnue, d’un checksum divergent ou d’un schéma sans historique reconnu.
- [ ] Vérifier que les catalogues nécessaires sont présents et qu’aucun compte, secret ou fixture de développement n’est installé en production. Ne jamais fabriquer l’historique pour adopter une base incompatible.
- [ ] Tester contraintes, index, clés étrangères, nullabilité, cascades voulues, triggers contre écritures tardives, timestamps et types numériques. Examiner les plans des requêtes critiques sur données représentatives avant d’ajouter des index.
- [ ] Rejouer concurrences : retrait de consentement versus profil/préférences/présence ; deux likes réciproques ; quotas et continuation de match ; message idempotent ; activation photo concurrente ; effacement versus upload/Checkout/swipe ; webhook versus réconciliation Stripe.
- [ ] Vérifier lecture de l’horloge après verrou de match, expiration pendant attente, limite de continuation à zéro et ordre stable des verrous. Tester deadlock/timeout sans retry d’une opération non idempotente.
- [ ] Tester exports paginés sous un même instantané `REPEATABLE READ`, absence de swipes entrants et absence de données tierces. Mesurer l’impact d’un long instantané sur vacuum et croissance disque.
- [ ] Rejouer âge/date bissextile, changement de jour, UTC et changements d’heure pour les agrégats concernés ; montants au-delà de la précision JavaScript et conversions numériques extrêmes.
- [ ] Calculer le budget total de connexions : API, workers, maintenances, supervision, migrations et pools d’activité dédiés de quatre connexions maximum par processus concerné. Réserver la capacité d’administration.
- [ ] Confirmer le pooling de session requis par les advisory locks et le verrou de leader ; ne pas faire passer ces pools dans un pooling transactionnel incompatible. Tester perte de connexion et libération des verrous.
- [ ] Mesurer vacuum, index, bloat, rétention, historique outbox/audit et pagination à fort volume. Vérifier nettoyage borné des messages avant parent match, purges outbox et effacement par checkpoints.

**Terminé :** migrations répétables, invariants prouvés par tests concurrents réels, plans et budget de connexions documentés. Tests en schémas isolés ; aucun reset ou SQL destructif sur cible non vérifiée. Toute évolution du schéma suit une nouvelle migration, sans modifier la baseline figée.

## 9. R06 — Fournisseurs réels et effets externes

Utiliser des comptes de test dédiés, plafonds de dépenses et destinataires autorisés. Les secrets restent hors des rapports. Distinguer validation sandbox et validation limitée du compte de production.

| Fournisseur | Campagne restante | Critère de réussite |
| --- | --- | --- |
| Sweego | Envoi réel, callback HMAC, signature invalide, duplicata, ordre inversé, retard, OTP consommé/remplacé/expiré, timeout avant/après acceptation, quota et indisponibilité | États `pending/accepted/sent/failed/unknown` cohérents ; aucun nouveau POST aveugle ; callback ne réactive jamais un code ; `sent` n’est pas assimilé à reçu |
| Stripe Customer | Intention et watchdog avant POST, réponse perdue, clé d’origine avant 23 h, recherche après 23 h, métadonnées invalides, plusieurs résultats, Customer supprimé | Aucun nouveau Customer sur issue incertaine ; ambiguïté en dead letter ; rattachement et clôture de relation corrects |
| Stripe Premium | Checkout, SCA, renouvellement, échec de paiement, annulation immédiate/fin de période, remboursement et statut d’accès attendu | Projection, factures, droits et notifications conformes à la décision produit et au fournisseur |
| Stripe événements | Signature sur body brut, rejeu, retard, désordre, perte de webhook, snapshot ancien et reprise par réconciliation | Versions empêchent les régressions ; effet métier durable non dupliqué ; rattrapage sans intervention SQL |
| FCM, si activé | Véritables appareils, token renouvelé/invalide, OAuth expiré, quota, panne, appareil révoqué entre programmation et envoi | Pas de texte privé ; erreurs normalisées ; contrôle d’éligibilité ; aucun secret conservé dans les preuves |

- [ ] Vérifier timeouts, TLS, confiance CA, limites de réponse, retry/backoff et classification permanent/transitoire/incertain pour chaque client HTTP.
- [ ] Vérifier notifications et tâches push écrites dans la transaction métier, `notification_id` stable et filtres de facturation à la programmation comme à l’envoi.
- [ ] Documenter la possibilité de doublon externe FCM après résultat incertain ; tester la déduplication cliente prévue, sans promettre une sémantique fournisseur non garantie.
- [ ] Contrôler domaines de callbacks, credentials test/live, droits minimum, rotation et procédure de panne fournisseur. Aucun numéro, payload fournisseur, token ou justification sensible dans les logs.

**Terminé :** comptes rendus des scénarios, références fournisseurs expurgées, coûts maîtrisés et procédure de réconciliation utilisable par un opérateur. Les simulations locales ne clôturent pas ce lot à elles seules.

## 10. R07 — Photos, stockage objet et modération

- [ ] Tester la cible S3 de production par l’interface générique `OBJECT_STORAGE_*` : PUT/HEAD/GET/DELETE, TLS, permissions privées, signature, URL résoluble depuis conteneurs et clients, expiration et refus d’accès public.
- [ ] Vérifier extensions, MIME, signature, dimensions, 500 000 octets en entrée et sortie, conversion WebP sans métadonnées, JPEG/PNG/WebP/HEIC/HEIF et fichiers corrompus/tronqués. Mesurer les cas coûteux ou à dimensions extrêmes et les borner avant épuisement mémoire.
- [ ] Tester quota dédié, concurrence du codec, délai et arrêt du processus enfant, sortie invalide, indisponibilité du codec et nettoyage de ses fichiers. Contrôler l’absence de blocage du runtime async sous charge.
- [ ] Rejouer l’idempotence photo sur 24 heures : même clé/mêmes octets sans conversion ni écriture supplémentaires ; autre contenu, ancienne photo remplacée et requête concurrente refusés selon le contrat.
- [ ] Injecter panne entre `processing`, métadonnées, PUT S3, activation et outbox. Une issue S3 incertaine conserve une trace réconciliable ; l’ancienne photo n’est pas perdue avant activation atomique.
- [ ] Vérifier qu’aucune URL n’est persistée ; seules les photos `ready` et `approved` sont projetées hors propriétaire. Les listes admin/blocages ne signent pas d’objets ; le détail autorisé audite avant signature.
- [ ] Vérifier refus de réconciliation sur photo active/récente ou verrou de worker actif ; suppression confirmée avant retrait de la trace SQL ; inventaire des objets orphelins sans exposer les clés aux clients.
- [ ] Tester panne/timeout/réponse mal formée de l’analyseur : `pending` avec `analysis_unavailable`, jamais approbation par défaut ni rejet autonome. Rejet humain d’une photo ready et `photo.delete` restent atomiques.
- [ ] Évaluer règles texte et modèle photo sur un corpus représentatif et autorisé : faux positifs/négatifs, biais, version du modèle, politique et seuils. Vérifier checksum, provenance et licence des modèles.
- [ ] Définir reviewers habilités, délai de traitement, formation, exposition minimale, explications et recours utilisateurs. Mesurer âge de file et décisions infirmées sans labels personnels dans Prometheus.

**Terminé :** essais sur stockage réel et image Linux, politique de modération approuvée, limites mesurées et procédure de reprise. SeaweedFS mini demeure réservé au développement ; la durabilité de SeaweedFS server dépend du déploiement et de ses sauvegardes, pas de son nom.

## 11. R08 — Résilience et arrêts de processus

Compléter la couverture existante avec des campagnes contre les vrais binaires et conteneurs Linux. Utiliser schémas/buckets dédiés, UUID générés et relais réseau de test ; ne jamais arrêter les conteneurs partagés pour injecter une panne.

| Point de panne | Injection | Invariant à prouver après reprise |
| --- | --- | --- |
| Outbox | Arrêt après claim, pendant effet, après effet avant ack ; deux workers concurrents | Revendication bornée, reprise propriétaire, aucun double effet métier ; incertitudes externes traitées selon leur contrat |
| Effacement | Arrêts Stripe → photos → lots de swipes → anonymisation → ack | Compte désactivé dès 202, token consommé une fois, DSR non terminée trop tôt, reprise sans perte de checkpoint |
| Verrou d’activité | Connexion PostgreSQL rompue pendant upload/Checkout/swipe | Perte de protection détectée ; aucun effet suivant lancé comme si le verrou existait encore |
| Base SQL | Indisponibilité, redémarrage, délai, connexion coupée, commit incertain | Pas de réponse de succès inventée, erreur sûre, atomicité et reprise idempotente |
| Redis | Timeout, partition, redémarrage | Rate limit échoue selon le contrat sans ouverture silencieuse ; SSE ne compromet pas la durabilité métier |
| S3 | PUT/DELETE réussi mais réponse perdue, stockage inaccessible | Objet et état SQL réconciliables, aucune trace supprimée prématurément |
| Export | Déconnexion client, annulation, disque plein, limite fichier atteinte, processus tué | Pas de gros objet RAM, aucun flux partiel avant préparation, quota libéré et fichiers privés nettoyés ou purgés au redémarrage |
| Maintenance | Arrêt au milieu d’un lot, exécutions concurrentes, leader perdu | Lots déjà validés conservés, verrous libérés, travail restant repris et suivi exact |
| API | SIGTERM avec upload/export/SSE actif, puis SIGKILL de test | Drainage borné, pools et enfants fermés, requêtes interrompues rejouables selon leur idempotence |
| Ressources hôte | Espace/inodes insuffisants, mémoire bornée, descripteurs épuisés | Échec observable, pas de corruption ni fuite durable ; retour à l’état sain vérifié |

**Terminé :** protocole rejouable avec assertions SQL/objet, absence de fuite de secrets et temps de reprise mesuré. Les tests doivent distinguer « effet jamais lancé », « effet confirmé » et « issue inconnue » ; une boucle de retry ne constitue pas une preuve de résilience.

## 12. R09 — Sécurité applicative et dépendances

- [ ] Faire une revue indépendante puis un test d’intrusion authentifié couvrant utilisateur, autre utilisateur, utilisateur banni/effacé, modérateur/admin selon les rôles présents et client anonyme. Relier chaque constat à un chemin reproductible et corriger avec un test.
- [ ] Revoir autorisations objet, énumération, mass assignment, SQL paramétré, champs de privilège/prix Stripe refusés, export tiers, sessions révoquées, consentements et accès administratif par JWT mobile.
- [ ] Tester JWT HS256 : `kid` local seulement, type/issuer/audience/sid/exp, signature, bornes d’expiration, token altéré et famille inactive. Vérifier que rôles/état sont relus en base.
- [ ] Tester cookies, origine exacte des mutations admin, CSRF, CORS et proxy : IP socket, chaîne de proxies explicite, `X-Forwarded-For` forgé, headers dupliqués et accès direct au backend.
- [ ] Tester limites requête/upload, connexions lentes, abonnements SSE, bruteforce OTP, quotas distribués, coûts SMS et abus de génération d’exports/URLs. Vérifier que les erreurs de dépendance n’ouvrent pas un chemin sans limite.
- [ ] Examiner les chemins réseau et les URL réellement consommées : possibilité de SSRF, redirection vers réseau interne, endpoints configurables, permissions S3 et appels fournisseurs. Ne déclarer une vulnérabilité que si un chemin d’exploitation est démontré.
- [ ] Auditer logs, métriques, traces, dumps et messages CLI : pas de body privé, téléphone, token, hash de session, object key, URL signée, query ou exception brute. Contrôler aussi les logs proxy, Docker, codec et modération.
- [ ] Scanner dépendances Rust, Node du codec, Python/modèles, OpenSSL, images et paquets OS ; produire inventaire/SBOM, licences, provenance et versions verrouillées. Traiter les vulnérabilités exploitables et documenter les autres avec réévaluation datée.
- [ ] Chercher secrets dans sources, fichiers livrés, images et artefacts ; si un secret a été exposé, le révoquer/renouveler. Supprimer le fichier seul ne révoque pas le secret.
- [ ] Réviser `unwrap`, `expect`, `panic`, indexation, durées et casts numériques sur chemins de production ; cibler tests de propriétés/fuzzing sur parsers, curseurs, signature, Unicode, multipart et entrées extrêmes.
- [ ] Écrire et tester la rotation des secrets JWT, webhook, fournisseur, Redis/S3 et admin. Distinguer clés de chiffrement téléphone et clé de pseudonymisation : les changer sans migration peut rendre les données illisibles ou casser leur rapprochement.

**Terminé :** aucun constat critique/élevé exploitable non traité dans le périmètre livré, preuves des corrections et approbation de sécurité. Un scan de dépendances sans analyse ne vaut pas un audit ; aucune absence universelle de vulnérabilité n’est revendiquée.

## 13. R10 — Infrastructure, réseau et image finale

Référence : [guide de déploiement](docs/container-deployment.md) et manifests Compose du dépôt.

- [ ] Déployer sur une préproduction représentative du serveur cible ; calculer mémoire/CPU/disque pour API, PostgreSQL 7 Gio, Redis, S3, modération, codec, supervision et OS. Garder une marge mesurée ; ne pas appliquer le budget production à la pile locale PostgreSQL 1 Gio.
- [ ] Vérifier image non-root, système de fichiers en lecture seule, capacités minimales, limites de processus/fichiers/mémoire et seuls volumes/tmpfs nécessaires en écriture. Tester leur dimensionnement pour photos et exports.
- [ ] Vérifier TLS PostgreSQL/Redis/S3 : certificat, CA, nom d’hôte, date d’expiration, refus du clair et renouvellement. Tester récupération après rotation sans désactivation de vérification TLS.
- [ ] Vérifier réseau depuis l’extérieur : seuls les accès publics prévus sont ouverts ; DB, Redis, stockage brut, modération, metrics, Prometheus, Alertmanager et Grafana restent privés. Seule la passerelle S3 HTTPS rejoint le réseau proxy prévu.
- [ ] Qualifier DNS, reverse proxy/CDN éventuel, limites de body, timeouts, headers et non-bufferisation SSE. Exclure du cache les données privées, les erreurs sensibles et les flux authentifiés ; ne jamais publier `/metrics` via la passerelle publique.
- [ ] Vérifier droits des volumes, persistance PostgreSQL/S3, renouvellement des certificats, synchronisation horaire, accès administrateur hôte, pare-feu, correctifs OS et accès sortants requis.
- [ ] Vérifier readiness/liveness selon leur contrat et l’ordre de démarrage migration → API/worker. Une panne de dépendance ne doit pas entraîner une boucle de redémarrages empêchant la reprise.
- [ ] Vérifier signaux Docker, délai de grâce cohérent avec le drain applicatif, redémarrage complet du serveur, reprise des services et montage correct des volumes avant démarrage.
- [ ] Définir espace libre minimal, croissance des données, logs Docker, exports temporaires et rétention des images/caches. Séparer explicitement caches reconstruisibles et volumes métier avant toute purge.

**Terminé :** audit réseau réel, redémarrage du serveur répété, image/configuration identifiées et budgets compatibles avec R12. L’existence de `compose.production.yaml` ne prouve pas cette qualification.

## 14. R11 — Sauvegarde, restauration et reprise après sinistre

Le script [verify-dev-backup-restore.ps1](scripts/verify-dev-backup-restore.ps1) constitue une preuve locale utile : restauration dans une base temporaire et contrôles de structure/historique. Il ne prouve pas une reprise complète du service, la cohérence métier ni l’atteinte d’un RPO/RTO hors machine.

- [ ] Choisir la stratégie PostgreSQL selon RPO/RTO : fréquence des sauvegardes, éventuels WAL/PITR, chiffrement, vérification d’intégrité, rétention et surveillance des échecs. Tester les mécanismes effectivement retenus.
- [ ] Sauvegarder les objets privés et les métadonnées S3 nécessaires, pas seulement PostgreSQL. Documenter la cohérence temporelle entre base, objets, outbox et fournisseurs ; identifier objets manquants, supplémentaires ou déjà supprimés après restauration.
- [ ] Conserver une copie hors serveur et hors domaine de panne, protégée contre effacement accidentel/compromission selon le risque retenu. Tester accès et téléchargement sans dépendre de la machine perdue.
- [ ] Sauvegarder/provisionner séparément configuration, certificats, clés de chiffrement et pseudonymisation, accès fournisseurs, catalogue de versions et procédures ; limiter l’accès aux secrets de secours et en tester la récupération.
- [ ] Définir le traitement de Redis après restauration : quotas, reconnexion SSE et données reconstructibles. Ne pas considérer un Redis vide comme neutre vis-à-vis des abus.
- [ ] Restaurer sur une machine vierge isolée, à partir des copies hors machine, avec uniquement le dépôt/image Rust et les moyens de secours. Mesurer perte effective de données et temps jusqu’au rétablissement vérifié, pas uniquement jusqu’à la fin de `pg_restore`.
- [ ] Contrôler comptes, consentements, sessions, droits Premium, matchs/messages, photos, audits, checkpoints, historique de migration et contraintes ; exécuter smoke et parcours métier avec données synthétiques.
- [ ] Empêcher SMS/push/paiements réels pendant l’exercice. Après restauration, traiter les divergences fournisseurs en lecture/réconciliation avant de relancer des effets ; une sauvegarde peut avoir oublié un effet pourtant livré après sa prise.
- [ ] Tester la reprise des intentions Stripe de plus de 23 heures, suppressions S3 et outbox restaurées. Ne pas émettre de nouveau POST sous une autre clé pour « réparer » un état incertain.
- [ ] Définir avec le DPO comment réappliquer les effacements intervenus après la sauvegarde et empêcher la réapparition des données supprimées. Protéger le registre minimal nécessaire et sa propre rétention.
- [ ] Exécuter au moins un exercice complet de perte de machine sur environnement dédié, incluant DNS/certificats/secrets, démarrage des workers et validation client. Tester aussi une archive invalide et l’indisponibilité d’une copie.

**Terminé :** RPO/RTO mesurés respectés, sauvegardes déchiffrables et procédure réalisable par une autre personne. La sauvegarde, la haute disponibilité et le rollback applicatif répondent à des problèmes différents. Ne jamais restaurer une vieille base pour simplement annuler un déploiement si cela écrase des écritures récentes.

## 15. R12 — Tests de charge, endurance et capacité

### Préparer une campagne mesurable

- [ ] Choisir un outil de charge capable des parcours HTTP/SSE retenus et versionner ses scénarios dans Rust. Installer le générateur hors du serveur mesuré et vérifier qu’il n’est pas le goulot d’étranglement.
- [ ] Créer des données synthétiques avec UUID générés, distribution réaliste, utilisateurs actifs/inactifs, comptes à forte activité, décisions réciproques, historiques longs et tailles d’objets variées. Aucun contenu personnel réel nécessaire.
- [ ] Fixer dans R00 les seuils par parcours et volumes visés avant mesure. Les valeurs ci-dessous décrivent un protocole proposé, pas une capacité démontrée.
- [ ] Désactiver les livraisons payantes/réelles dans cette campagne et simuler les fournisseurs de façon contrôlée ; évaluer séparément leurs limites officielles/contractuelles et les essais R06.
- [ ] Mesurer à la fois charge offerte, débit réellement servi, concurrence, requêtes abandonnées et latence de bout en bout. Éviter qu’un générateur qui ralentit lui-même masque la saturation.

### Scénarios minimaux

| Parcours | Charge et cas défavorable | Mesures spécifiques |
| --- | --- | --- |
| Authentification | OTP, refresh, rejeux et attaques refusées sur comptes/IP variés | p95/p99, 401/409/429 attendus, contention/verrous, coût SMS simulé |
| Lecture et feed | Profils, pagination, filtres, grand historique et zone dense | Plans SQL, lignes examinées, pool, latence et taille des réponses |
| Swipes et matchs | Likes réciproques simultanés, utilisateur très sollicité, continuation/expiration | Quotas, conflits, temps de verrou et unicité des matchs |
| Messagerie | Envoi concurrent, même clé idempotente, long historique | Débit durable, ordre contractuel, duplication et volume outbox |
| SSE | Connexions longues, clients lents, expirations et reconnexions groupées | Mémoire par connexion, descripteurs, heartbeat, Redis, rattrapage durable |
| Photos | Uploads maximaux, HEIC coûteux, replay identique et modération lente | CPU/RSS API + Node/Python, queue, timeouts, débit S3 |
| Export | Plusieurs comptes à historique maximal et clients interrompus | Espace temporaire, taille fichier, snapshot SQL, mémoire et nettoyage |
| Effacement et maintenance | Lots importants en présence de trafic normal | Verrous, I/O, durée des lots, checkpoints et délai de traitement |
| Billing et notifications | Rafale de webhooks désordonnés, retard fournisseur, backlog outbox | Éligibilité, non-régression des projections, âge du plus ancien événement |
| Administration et supervision | Listes, agrégats et scrape pendant pic | Coût SQL, pagination, absence de dégradation disproportionnée du trafic métier |

### Déroulement proposé et critères

1. Établir une référence à faible trafic ; vérifier fonctionnellement les résultats.
2. Monter par paliers à 25 %, 50 % et 100 % de la cible décidée ; tenir la cible au moins une heure après stabilisation.
3. Appliquer une pointe courte à 2× la cible comme scénario initial à ajuster ; mesurer admission, refus attendus, reprise et absence d’emballement.
4. Faire une endurance de 12 à 24 heures, incluant maintenances, renouvellement de sessions, expirations et croissance des files.
5. Rejouer une panne de dépendance puis le rattrapage sous trafic ; mesurer la durée de résorption et les effets des retries.
6. Mesurer un redémarrage/déploiement avec connexions longues. Si plusieurs instances sont prévues, tester alors le rate limit distribué, le Pub/Sub et la concurrence des workers.

- [ ] Capturer p50/p95/p99 par parcours, taux de 5xx/timeouts, 4xx attendus séparés, CPU/RSS de tous les processus, I/O, disque/inodes, descripteurs, connexions/attentes SQL, verrous, Redis, S3, outbox et maintenances.
- [ ] Vérifier bornes de mémoire, buffers, processus enfants, concurrence et files. Rechercher N+1 ou contention à partir de traces/plans/mesures ; ne pas optimiser sur une supposition.
- [ ] Documenter la granularité actuelle des métriques PostgreSQL : acquisition/ping/commandes transactionnelles, **pas toutes les requêtes SQL**. Compléter le diagnostic de charge avec statistiques/plans SQL et instrumentation ciblée sûre si nécessaire.
- [ ] Vérifier après chaque essai cohérence SQL/S3, unicité métier, absence de fuite, fichiers temporaires et backlog résorbé. Conserver configuration, données et résultats comparables avant/après correction.

**Terminé :** tous les budgets approuvés sont respectés à la charge cible, sans perte de données, OOM ou croissance non bornée ; seuil de saturation et capacité de reprise connus ; marge et plan d’extension documentés. Les 429 attendus ne sont pas des 5xx mais doivent rester compatibles avec l’usage prévu. Pas de test de saturation sur la production ou une pile locale partagée.

## 16. R13 — Observabilité et alertes réellement exploitables

Référence : [observabilité et runbooks](docs/observability.md).

- [ ] Rejouer la protection du listener distinct : 401 sans bearer, collecte autorisée, `/api/metrics` absent, réseau privé et rotation du token dédié sans exposition.
- [ ] Valider configuration et règles Prometheus avec les tests fournis, puis déclencher chaque alerte critique et sa résolution. Raccorder Alertmanager à un canal approuvé et vérifier réception par le responsable et son suppléant.
- [ ] Provoquer une panne de l’API seule alors que l’exporteur/Prometheus fonctionnent ; distinguer disponibilité applicative, collecte, dépendances et absence de trafic. Prévoir un signal extérieur à la machine pour sa perte totale.
- [ ] Couvrir DB/pool, Redis, S3, erreurs fournisseur, backlog/âge outbox, dead letters, DSR bloquées, maintenance absente, file de modération, espace disque, mémoire/OOM, certificats et sauvegardes périmées.
- [ ] Recalibrer les seuils initiaux du guide après R12. Vérifier que les dashboards n’attendent pas des métriques Node/V8 absentes en Rust et que leur agrégation ne masque pas un parcours dégradé.
- [ ] Vérifier request IDs/corrélation selon le contrat, codes d’événement stables, chemins de route génériques et cardinalité bornée. Aucun identifiant utilisateur/métier dans les labels.
- [ ] Définir collecte des logs, accès, chiffrement, rotation, volume et rétention approuvée. Distinguer logs techniques, audit métier et preuve d’incident ; tester un disque de logs presque plein.

**Terminé :** chaque alerte critique a un propriétaire, un canal testé et un runbook exécuté. Un dashboard vert sans trafic ou un service Prometheus démarré ne clôture pas ce lot.

## 17. R14 — Maintenances, astreinte et procédures d’incident

- [ ] Installer un ordonnancement explicite de `maintenance` : le profil Compose `jobs` ne constitue pas un scheduler. Définir fréquence, budget, verrouillage, timeout, code de sortie, logs sûrs et alerte d’exécution manquante.
- [ ] Vérifier expiration/purge OTP et sessions, matchs/messages, notifications, demandes photo idempotentes, audits et autres données selon les politiques retenues ; conserver traitements bornés et `processed_count`, `batch_count`, `work_remaining` pour les maintenances concernées.
- [ ] Vérifier que l’outbox tourne indépendamment de l’API, reprend après redémarrage et que ses lots/concurrences ne saturent pas les pools. Documenter arrêt et relance sans double worker historique.
- [ ] Écrire les gestes pour dead letters : authentification récente, motif, audit transactionnel, interdiction d’abandon d’`account.erase` et de `photo.delete` quand la ligne existe. Pas de modification SQL improvisée pour contourner ces garanties.
- [ ] Préparer runbooks : perte DB/Redis/S3, quota fournisseur, intention Stripe incertaine, file bloquée, photo orpheline, effacement bloqué, clé compromise, accès admin perdu, expiration de certificat et saturation disque.
- [ ] Définir niveau de service support, escalade, communication d’incident et conservation minimale des preuves. Les délais/notifications réglementaires sont validés en R15.
- [ ] Former au moins un suppléant et réaliser un exercice où il suit seul un runbook sans connaître les secrets du développeur initial.
- [ ] Établir une politique de nettoyage pour `target/`, caches et images obsolètes, séparée de la rétention des sauvegardes et volumes métier. Vérifier les chemins absolus et les versions de rollback à conserver avant suppression.

**Terminé :** maintenances planifiées et observées, opérateurs formés, incidents simulés et moyens de secours disponibles hors du serveur principal.

## 18. R15 — Juridique, protection des données et gouvernance produit

Ce lot requiert des décisions du produit et du DPO/juriste. Il ne doit pas être clôturé par une valeur de configuration arbitraire ou par la seule réussite des tests.

- [ ] Faire approuver CGU, notice, consentements sensibles/localisation, finalités, bases légales, destinataires, droits et versions affichées. Vérifier que les changements de version sont présentés par les clients.
- [ ] Évaluer l’obligation d’AIPD et la réaliser si nécessaire ; inclure mise en relation, localisation, données sensibles et modération. Documenter les risques résiduels et responsables.
- [ ] Rapatrier puis faire approuver la matrice de rétention Rust : swipes, OTP, sessions/familles/hash de refresh consommé, notifications, outbox, signalements, audits, consentements, DSR, tombstones, logs, exports temporaires et sauvegardes.
- [ ] Définir les comptes durablement inactifs, préavis éventuels, purge et exclusions légitimes ; ne pas ajouter une purge automatique avant cette décision.
- [ ] Valider exercice des droits, contrôle d’identité proportionné, protection des tiers dans l’export, délais de réponse, effacement asynchrone et explication des copies externes/URLs signées encore valides jusqu’à expiration.
- [ ] Documenter sous-traitants, régions, transferts, contrats et données minimales envoyées à Sweego, Stripe, FCM, hébergeur et stockage ; approuver les politiques de sauvegarde et restauration après effacement.
- [ ] Faire approuver questions de profil, information sur suppression en cascade de leurs réponses, modération humaine/automatique, recours, habilitations et exposition des reviewers.
- [ ] Valider procédure d’incident de données, rôles, délais applicables et communication ; prévoir retrait d’habilitation et revue régulière des accès administrateurs.
- [ ] Conserver chaque approbation avec auteur, date, périmètre, version et support durable. Renseigner `LEGAL_REVIEW_REFERENCE` avec une véritable référence vérifiable et vérifier les garde-fous de configuration de production.

**Terminé :** décisions et textes approuvés, implémentation/maintenance alignées et preuves traçables. Une durée héritée de NestJS est un état technique à examiner, pas une justification juridique. Toute modification de durée met à jour politique, migrations si nécessaires, maintenance et tests.

## 19. R16 — Répétition du déploiement, rollback et ouverture

### Répétition sur préproduction

- [ ] Préparer une fiche de livraison : digest image, schéma supporté, changements de configuration, catalogue de secrets référencés sans valeurs, tests, sauvegarde vérifiée, ordre des opérations et personnes joignables.
- [ ] Déployer depuis un état connu : vérifier volumes/réseaux/certificats, exécuter une seule migration contrôlée, démarrer API/worker, ordonnancer maintenance, valider readiness puis trafic client.
- [ ] Tester interruption de déploiement, migration refusée et image défectueuse. Définir les critères d’arrêt/rollback à partir des budgets : erreurs, latence, retard de file, autorisations ou intégrité ; toute atteinte à l’intégrité/sécurité prime sur le maintien du trafic.
- [ ] Pendant la période de coexistence de référence, répéter les cycles NestJS → Rust → NestJS → Rust prévus par S29 sur données synthétiques, uniquement après vérification de compatibilité des schémas et credentials. Ne jamais lancer les deux générations de workers sur les mêmes effets.
- [ ] Prévoir arrêt/drainage, claims en cours, verrous, connexions SSE et reprises clientes. Vérifier la compatibilité réelle des passkeys/sessions après retour de version ; ne pas la déduire de routes identiques.
- [ ] Tester le rollback vers l’image Rust précédente sur le schéma courant. Pour les futures migrations, documenter leur compatibilité avec les deux versions ou le plan de correction en avant ; ne pas supposer un downgrade SQL automatique.
- [ ] Vérifier qu’un rollback conserve écritures déjà validées, clés d’idempotence, notifications, checkpoints et effets fournisseurs incertains. Ne pas utiliser une restauration ancienne comme raccourci.

### Première ouverture publique

- [ ] Confirmer explicitement données de démarrage, absence de fixtures, comptes admin de secours, credentials live, domaines et références juridiques.
- [ ] Refaire smoke depuis le chemin public autorisé et parcours admin/mobile ; tester callback fournisseurs et URL photo depuis l’extérieur. Vérifier que les interfaces privées sont toujours inaccessibles.
- [ ] Ouvrir progressivement selon le plan produit et observer une fenêtre définie en R00 avec responsable présent. Garder les mécanismes de retour utilisables et surveiller coûts/quotas en plus des erreurs.
- [ ] Consigner la décision de go/no-go et les preuves ci-dessous. En cas d’anomalie bloquante, fermer le parcours concerné selon une procédure approuvée et corriger avant reprise.

**Terminé :** répétition complète réussie et décision nominative de lancement. Le pilote ne dispense ni de la sécurité, ni des sauvegardes, ni des décisions juridiques nécessaires aux données traitées.

## 20. R17 — Suppression définitive de NestJS et de son infrastructure

La demande de retrait couvre à terme dépôt, scripts, base, conteneurs et volumes historiques. Leur suppression est une étape séparée, pas une conséquence automatique du premier démarrage Rust.

- [ ] Clore la comparaison R03 et archiver les preuves de référence nécessaires dans un support autonome. Conserver les contrats et politiques utiles dans Rust.
- [ ] Vérifier absence de chemins/mounts/imports/script de démarrage, jobs planifiés, callbacks, monitoring ou DNS encore dirigés vers NestJS.
- [ ] Si des données doivent être préservées, effectuer une répétition puis un transfert cohérent vers l’infrastructure Rust : SQL, clés de chiffrement, objets privés et références fournisseurs. Comparer intégrité et parcours métier avant toute destruction de la source.
- [ ] Si tout est jetable, consigner cette décision ; reconstruire la base Rust avec ses migrations et catalogues sans déplacer de fixtures privilégiées ou de secrets historiques.
- [ ] Identifier chaque conteneur, projet Compose, volume, base et bucket par cible exacte. Vérifier qu’aucun volume ou service n’est partagé avec Rust et que les sauvegardes à conserver sont récupérables.
- [ ] Fixer la fin de la fenêtre de retour NestJS après observation et approbation. Conserver ensuite une image Rust précédente exploitable, ses configurations compatibles et des sauvegardes vérifiées ; la capacité de rollback ne doit plus dépendre de NestJS.
- [ ] Retirer credentials, clés, accès fournisseurs, callbacks et tâches devenus inutiles sans révoquer ceux encore employés par Rust. Vérifier les services après retrait.
- [ ] Exécuter seulement alors le plan de suppression validé et enregistrer les ressources retirées sans secret. Faire une nouvelle installation/restauration Rust avec le dossier NestJS indisponible.

**Terminé :** système Rust indépendant, absence de perte de données utile, coût des ressources historiques supprimé et restauration possible sans NestJS. Aucun `DROP`, reset ou effacement de volume n’est autorisé implicitement par ce document.

## 21. R18 — Contrôles récurrents après lancement

Les cadences suivantes sont à formaliser selon risques et objectifs ; désigner le responsable et la prochaine échéance avant l’ouverture.

| Déclencheur | Contrôle à maintenir |
| --- | --- |
| Chaque livraison | Formatage/Clippy/tests, contrats touchés, migrations, scan dépendances/image, smoke, compatibilité rollback et documentation |
| Chaque changement réseau/secret/modèle/fournisseur | Test ciblé TLS/rotation, origine WebAuthn, signature, permissions, qualité de modération et procédures de reprise |
| Surveillance continue avec revue régulière | Sauvegardes, capacité disque, disponibilité, backlog, coûts, erreurs, échéances certificats, maintenance et alertes |
| Revue périodique planifiée | Dépendances et correctifs, habilitations/passkeys, règles modération, demandes de droits, rétention et accès aux preuves |
| Exercice périodique et après évolution importante | Restauration hors machine, récupération admin, incident de sécurité, charge et panne de dépendance |
| Croissance ou changement d’offre | Budgets R12, plans SQL, quotas fournisseur, stockage, marge machine et réévaluation du risque mono-nœud |
| Changement produit/juridique | Textes, versions de consentement, conservation, sous-traitants, transfert et besoin de nouvelle analyse de risques |

Éviter les extensions d’architecture sans mesure justifiant leur coût. Si une CI est souhaitée ultérieurement, sa mise en place fait l’objet d’une demande explicite ; elle doit réutiliser les commandes reproductibles et respecter l’isolation des tests.

## 22. Checklist finale de go/no-go

- [ ] R00 : périmètre, propriétaires, SLO, RPO/RTO et capacité cible décidés.
- [ ] R01–R02 : dépôt autonome, candidate Linux reproductible, tests et smoke passés sur l’image livrée.
- [ ] R03–R05 : parité endpoint par endpoint, clients/admin réels, schéma et concurrence validés ; différences intentionnelles approuvées.
- [ ] R06–R07 : fournisseurs applicables, stockage privé et modération qualifiés.
- [ ] R08–R09 : pannes maîtrisées et constats de sécurité bloquants corrigés puis retestés.
- [ ] R10–R11 : infrastructure privée/TLS et restauration hors machine respectant RPO/RTO.
- [ ] R12–R14 : charge/endurance conformes, alertes reçues, maintenances et runbooks opérables.
- [ ] R15 : approbations juridiques et gouvernance nécessaires effectivement obtenues.
- [ ] R16 : déploiement et rollback répétés, sauvegarde exploitable, responsable de lancement et surveillance prévus.
- [ ] R17 : plan de retrait NestJS prêt ; suppression différée tant que ses propres conditions ne sont pas réunies.
- [ ] R18 : responsables et échéances des contrôles récurrents enregistrés.

Une case ne devient acquise que par une preuve rattachée à son périmètre. Une modification ultérieure invalide les preuves qu’elle affecte ; les autres restent réutilisables avec leurs limites explicites.
