# Histae API — feuille de route de production

Dernière mise à jour : **14 octobre 2026**, après intégration des dépôts `histae-scripts` et
`histae-reverse-proxy` et correction des procédures de déploiement.

Cette feuille de route suit les raccordements, décisions et preuves qui restent nécessaires avant le
déploiement final de Histae et son ouverture aux utilisateurs. Les guides liés décrivent les procédures ;
ce fichier sert à suivre leur réalisation sur la livraison et le serveur retenus.

Les connecteurs et une grande partie des contrôles sont déjà implémentés. Une case ouverte signifie
que la configuration ou la validation finale reste à confirmer, pas que la fonctionnalité manque dans
le code. L'état du serveur et des comptes fournisseurs n'a pas été audité pour rédiger ce document.
Les validations locales documentées restent utiles, mais ne clôturent pas une validation sur la cible.

## Règles de suivi

- **Démarrage** : bloque le démarrage ou la disponibilité de la pile de production actuelle.
- **Ouverture** : doit être terminé avant d'accueillir des utilisateurs réels.
- **Conditionnel** : bloque l'activation de la fonctionnalité concernée ; elle peut rester désactivée
  si ce choix est cohérent avec le périmètre annoncé aux utilisateurs.
- Cocher une tâche après vérification et conserver une preuve datée : livraison/image, environnement,
  scénario, résultat et emplacement du compte rendu expurgé de secrets et de données personnelles.
- Après changement de code, secrets, certificat ou topologie, rejouer les validations concernées.
- Aucun secret, token, mot de passe, clé privée ou donnée utilisateur ne doit être copié dans ce fichier.

## Synthèse des blocages

| Lot | Résultat attendu | Blocage | Dépendances principales |
| --- | --- | --- | --- |
| PROD-01 | Configuration et livraison reproductibles | Démarrage | Secrets, serveur, choix des images |
| PROD-02 | PostgreSQL/PostGIS, rôles, TLS et migrations | Démarrage | PROD-01 |
| PROD-03 | Redis authentifié et chiffré | Démarrage | PROD-01 |
| PROD-04 | Photos privées sur un S3 HTTPS persistant | Démarrage / ouverture | PROD-01, PROD-06 |
| PROD-05 | Modération automatique et revue humaine utilisables | Démarrage / ouverture | PROD-04, PROD-09 |
| PROD-06 | Domaines, tunnel et confiance proxy corrects | Ouverture | PROD-01 |
| PROD-07 | OTP Sweego et callbacks réels | Démarrage / ouverture | PROD-06 |
| PROD-08 | Stripe live et parcours Premium complets | Démarrage / ouverture | PROD-06, PROD-10 |
| PROD-09 | Administration WebAuthn et récupération | Ouverture | PROD-02, PROD-06, PROD-07 |
| PROD-10 | Outbox permanente et maintenance planifiée | Ouverture | PROD-02, PROD-03, PROD-04 |
| PROD-11 | Supervision production et alertes reçues | Ouverture | PROD-06, PROD-09, PROD-10 |
| PROD-12 | Sauvegardes hors machine et restauration complète | Ouverture | PROD-02, PROD-04 |
| PROD-13 | Textes publiés, consentements et procédures de droits | Démarrage / ouverture | Produit, site, validation juridique |
| PROD-14 | Clients réels, sécurité, capacité et procédure de livraison | Ouverture | PROD-01 à PROD-13 |
| PROD-15 | Notifications mobiles Firebase/FCM | Conditionnel | Mobile, Firebase, PROD-10 |
| PROD-16 | Récompenses publicitaires AdMob | Conditionnel | Mobile, AdMob, PROD-06, PROD-13 |

## Corrections réalisées dans les dépôts

Ces cases concernent les fichiers et les vérifications locales ; elles ne clôturent pas les lots
sur le serveur final. Aucun fournisseur réel ni serveur distant n'a été utilisé pour ces corrections.

- [x] Corriger les chemins vers `histae-reverse-proxy` et centraliser reset/mise à jour dans `histae-scripts`.
- [x] Prévoir la mise à jour des cinq dépôts, avec refus des modifications locales et des fusions implicites.
  Le script de mise à jour a été relu et sa syntaxe vérifiée ; aucune commande Git n'a été exécutée.
- [x] Partager la préparation des secrets de supervision entre reset et redémarrage après mise à jour.
- [x] Ajouter un lancement explicite `--production`, ses upstreams et les contrôles de fichiers/réseaux.
- [x] Construire les images avant redémarrage et masquer les sorties de validation réussies en conservant les erreurs.
- [x] Aligner l'image de `storage-init` sur celle des autres processus API, y compris la valeur de repli.
- [x] Fournir une supervision production privée via `compose.production-observability.yaml`, avec
  alias dédiés, accès WebAuthn du superadmin et label Prometheus `production`.
- [x] Fournir des unités systemd configurables et une maintenance avec verrou partagé avec le reset.
- [x] Ajouter le modèle d'environnement et l'exclusion des secrets du contexte de build du proxy ;
  empêcher Compose de créer silencieusement les chemins montés manquants dans cette pile.
- [x] Normaliser les scripts Bash en LF et prévoir leur conservation avec `.gitattributes`.

Preuves locales du 14 octobre 2026 : dix tests d'orchestration avec Docker simulé réussis ; syntaxe
Bash/LF vérifiée sur vingt scripts ; cinq topologies acceptées par Compose 2.40.3 avec des valeurs
fictives (dev/proxy, production, production avec supervision, supervision autonome et reverse proxy).
Les contrôles vérifient aussi l'accord des images API, l'absence de ports de supervision publiés et
la séparation des projets dev/production. Ils ne prouvent pas le démarrage des images, les permissions
TLS, la connexion du tunnel, une cible Prometheus à UP ou un enrôlement WebAuthn réel.

Références : [Scripts et vérifications](../histae-scripts/README.md),
[Supervision production](docs/observability.md#supervision-integree-a-la-production).

## PROD-01 — Configuration, secrets et images

- [ ] Préparer l'environnement de production avec `ENV=production`, des valeurs adaptées à la cible
  et des secrets distincts pour JWT, téléphone, Redis, S3, SMS, Stripe et supervision.
- [ ] Préserver les clés téléphone si des comptes existants sont repris ; documenter toute migration
  de clés et la rotation JWT avec les anciens `kid` nécessaires.
- [ ] Utiliser `compose.yaml` et `compose.production.yaml`, avec le même fichier d'environnement pour
  l'interpolation Compose et l'injection des processus applicatifs.
- [ ] Fixer les images de la livraison, notamment `HISTAE_API_IMAGE` et
  `HISTAE_PHOTO_MODERATION_IMAGE`, et conserver leurs identifiants exacts. API, migration,
  initialisation du stockage, outbox et maintenance doivent utiliser la livraison attendue.
- [ ] Fixer aussi `HISTAE_CLOUDFLARED_IMAGE` sur une version/digest validé ; le repli `latest`
  du modèle n'est pas une livraison reproductible. Conserver les références des cinq dépôts.
- [ ] Préparer les réseaux et volumes persistants de production, distincts de ceux du développement.
- [ ] Valider la configuration Compose et celle des binaires API, outbox et maintenance ; protéger
  l'accès aux secrets et définir la procédure de renouvellement des certificats.
- [x] Fournir la procédure de lancement production dans
  `../histae-scripts/reset-docker-compose.sh --production`, avec `compose.yaml`,
  `compose.production.yaml` et `compose.production-observability.yaml`.
- [ ] Exécuter cette procédure sur la cible finale avec les secrets, certificats et images retenus ;
  vérifier ensuite les parcours, la maintenance et la collecte. Sans option, le reset reste en mode dev.

**Clôture :** une livraison identifiée peut être construite et démarrée avec une configuration validée,
sans valeurs de démonstration ni dépendance à la pile dev.

Références : [Configuration](docs/configuration.md), [Déploiement](docs/container-deployment.md#production),
[Reverse proxy](../histae-reverse-proxy/README.md), [Scripts](../histae-scripts/README.md).

## PROD-02 — PostgreSQL, rôles et migrations

- [ ] Préparer la base et son volume persistant ; décider explicitement quelles données sont reprises.
- [ ] Configurer les mots de passe distincts des rôles `histae_admin`, `histae_migrate` et `histae_app`.
  Sur un volume existant, appliquer la procédure de conversion/rotation des rôles appropriée.
- [ ] Installer les certificats sous `.secrets/postgres/`, avec le SAN `DNS:postgres`, la CA et les
  permissions attendues ; vérifier une connexion SQLx avec `POSTGRES_SSLMODE=verify-full`.
- [ ] Exécuter le migrateur livré, puis vérifier PostGIS, les contraintes et les index de performance.
  Vérifier aussi un second passage du migrateur.
- [ ] Vérifier que le rôle applicatif possède les accès nécessaires aux parcours métier et ne dispose
  pas des privilèges d'administration ou de migration.
- [ ] Dimensionner les pools de tous les processus, y compris les connexions de verrous, dans la
  capacité PostgreSQL du serveur. Vérifier les timeouts et le budget mémoire sous charge.

**Clôture :** migrations réussies, connexion TLS vérifiée et parcours SQL exécutés avec le rôle
applicatif sur la base de production ou son environnement de validation équivalent.

Références : [Base de données](docs/database.md), [Rôles](docs/postgres-roles.md),
[Performance SQL](docs/sql-performance.md).

## PROD-03 — Redis

- [ ] Préparer un mot de passe dédié, les certificats sous `.secrets/redis/` avec le SAN `DNS:redis`
  et la CA attendue par les clients.
- [ ] Vérifier `RATE_LIMIT_STORE=redis`, `REDIS_TLS=true` et la connexion authentifiée de l'API et
  des workers ; la pile production impose ces réglages.
- [ ] Vérifier les quotas distribués et le relais des événements SSE entre processus.
- [ ] Vérifier, dans un environnement isolé, le refus des requêtes protégées lorsque le contrôle de
  quota Redis est indisponible et la reprise après rétablissement.

**Clôture :** Redis est privé, authentifié et chiffré ; quotas et temps réel fonctionnent avec une
reprise documentée. Les clients relisent les données après reconnexion SSE.

Références : [Configuration](docs/configuration.md), [Supervision](docs/observability.md).

## PROD-04 — Stockage S3 et photos privées

Le fournisseur des manifests actuels est SeaweedFS. Un remplacement par RustFS reste un choix
d'architecture possible ; s'il est retenu, rejouer ces validations et préparer une migration des objets
par S3 en conservant leurs clés et métadonnées. Ce changement n'est pas une tâche imposée au lancement.

- [ ] Préparer les clés S3 dédiées et, avec SeaweedFS, l'identité correspondante dans
  `.secrets/seaweedfs/s3.json`.
- [ ] Configurer le volume persistant et initialiser le bucket privé avec `storage-init` ; vérifier
  que les accès anonymes aux photos et aux listes d'objets sont refusés.
- [ ] Configurer `HISTAE_STORAGE_HOST`, la passerelle HTTPS, son certificat et l'ingress Cloudflare
  du même domaine, avec vérification du certificat d'origine.
- [ ] Vérifier que le domaine S3 est accessible à l'API et aux vrais clients, et que Host, chemin et
  paramètres des requêtes signées sont conservés de bout en bout.
- [ ] Vérifier upload, métadonnées `Content-Type`/`Cache-Control`, lecture par URL signée de cinq
  minutes, refus d'une signature invalide ou expirée et répétition d'une suppression.
- [ ] Vérifier qu'après traitement de la suppression par l'outbox, la photo n'est plus accessible à
  l'origine ; documenter les rétentions, purges et sauvegardes associées.
- [ ] Conserver le bucket applicatif sans versioning ni Object Lock tant que l'effacement de toutes
  les versions et la gestion des rétentions ne sont pas intégrés au parcours de suppression.

**Clôture :** les photos restent privées, les lectures autorisées fonctionnent derrière le tunnel et
les suppressions reprenables sont vérifiées avec le fournisseur et la version retenus.

Références : [Déploiement S3](docs/container-deployment.md#certificats-et-stockage),
[Contrat photo](docs/http-contract.md#photo-unique), [Tests](tests/README.md).

## PROD-05 — Modération et traitement humain

- [ ] Construire et démarrer le service de modération photo prévu par la pile, vérifier son warmup
  et la correspondance entre `PHOTO_MODERATION_TOKEN` et son `MODERATION_TOKEN`.
- [ ] Valider des photos fictives représentatives : approbation, mise en revue, format invalide,
  timeout et indisponibilité de l'analyse.
- [ ] Vérifier que les contenus non approuvés restent masqués et qu'une panne laisse la photo en
  attente de revue conformément au contrat.
- [ ] Vérifier dans le dashboard le détail justifié, les URL courtes, les décisions avec version
  optimiste, les conflits et l'audit des consultations/décisions.
- [ ] Définir les responsables, délais, motifs, explications aux utilisateurs et canal de recours ;
  mettre en place le traitement réellement prévu au lancement.

**Clôture :** l'analyse automatique et la revue humaine sont utilisables ; une demande ou un recours
possède un responsable et un parcours de traitement documenté.

Références : [Service photo](services/photo-moderation/README.md),
[Contrat](docs/http-contract.md#catalogue-et-moderation), [Dashboard](../histae-dashboard/roadmap.md).

## PROD-06 — Domaines, HTTPS et tunnel

- [ ] Vérifier les domaines du site, du dashboard, de l'API et du S3, leurs routes Cloudflare et les
  certificats nécessaires sur la topologie finale.
- [ ] Configurer l'upstream du dashboard et du reverse proxy sur
  `http://histae-api-production:8080`, puis recréer les services concernés.
- [ ] Vérifier les réseaux backend/edge et les IP fixes des proxys en accord avec `TRUST_PROXY` ;
  contrôler l'IP visiteur et les quotas depuis le véritable tunnel.
- [ ] Vérifier l'absence de ports publics pour PostgreSQL, Redis, SeaweedFS brut, modération et
  métriques ; contrôler les en-têtes, erreurs, redirections et routes SPA sur les domaines réels.
- [ ] Vérifier que les webhooks fournisseurs restent accessibles sans challenge interactif et que
  leurs signatures restent valides après passage par les intermédiaires.
- [ ] Vérifier les journaux effectifs du proxy, de l'API et du stockage : aucune URL signée,
  téléphone, cookie, preuve publicitaire ou secret ne doit y apparaître.

**Clôture :** les parcours HTTP, SSE, photos et webhooks fonctionnent depuis l'extérieur avec les
protections et la confiance proxy attendues.

Références : [Reverse proxy](../histae-reverse-proxy/README.md), [Caddy](docs/caddy-migration.md),
[Durcissement](docs/security-hardening.md).

## PROD-07 — Sweego et connexion mobile

Sweego est imposé par la configuration actuelle lorsque `ENV=production`.

- [ ] Vérifier le compte fournisseur, l'accès SMS, `SWEEGO_API_KEY`, le sender et la région `FR`.
- [ ] Configurer le webhook POST `/api/auth/sweego/webhook` et `SWEEGO_WEBHOOK_SECRET` avec le
  secret fourni par Sweego, distinct de la clé API.
- [ ] Vérifier un OTP réellement reçu sur un numéro autorisé et le parcours de création/connexion.
- [ ] Vérifier signature, rejeu et suivi des callbacks, refus d'un OTP expiré/consommé, limites
  d'essais et absence de réactivation par un callback tardif.
- [ ] Documenter la surveillance des erreurs de livraison, quotas et consommation fournisseur.

**Clôture :** connexion mobile et suivi de livraison validés avec Sweego, depuis le domaine final,
sans téléphone ni OTP dans les traces conservées.

Références : [Configuration OTP](docs/configuration.md#groupes-de-variables),
[Webhook Sweego](docs/http-contract.md#webhook-sweego).

## PROD-08 — Stripe et Premium

Le validateur actuel impose `BILLING_PROVIDER=stripe` et une clé live en production. Un lancement avec
la facturation désactivée nécessiterait une évolution explicite de cette règle et de ses tests.

- [ ] Configurer le compte Stripe, `STRIPE_SECRET_KEY`, `STRIPE_WEBHOOK_SECRET`, le produit Premium
  et les Prices mensuel/annuel correspondant au catalogue : 5 EUR/mois, 48 EUR/an, essai de 30 jours.
- [ ] Vérifier l'accord entre catalogue PostgreSQL, catalogue du site, dashboard et paramètres Stripe.
- [ ] Configurer Checkout et le portail client, les retours HTTPS succès/annulation/portail et leur
  traitement dans le client ; le retour de succès contient `{CHECKOUT_SESSION_ID}`.
- [ ] Enregistrer POST `/api/billing/stripe/webhook` et les événements pris en charge par le contrat,
  en préservant le corps brut et `Stripe-Signature`.
- [ ] Valider d'abord en sandbox les paiements, essai, renouvellement, échec, résiliation, rejeu,
  ordre des événements, réconciliation et effacement ; terminer par la validation live contrôlée
  du périmètre retenu et documenter les résultats.
- [ ] Vérifier que le client relit l'état serveur et que les droits Premium ne reposent pas uniquement
  sur la redirection de paiement.

**Clôture :** produit/prix et parcours clients sont cohérents ; les événements Stripe mettent à jour
les droits, la réconciliation reprend les écarts et l'effacement conserve ses checkpoints.

Références : [Tarifs et Stripe](docs/configuration.md#tarifs-du-catalogue-et-stripe),
[Contrat Stripe](docs/http-contract.md#webhook-stripe), [Livraison](docs/production-readiness.md).

## PROD-09 — Administration WebAuthn

- [ ] Vérifier `ADMIN_WEBAUTHN_ORIGIN` et `ADMIN_WEBAUTHN_RP_ID` sur le domaine HTTPS réel.
- [ ] Initialiser le superadmin identifié, émettre le jeton avec `admin-bootstrap` et terminer
  l'enrôlement dans le dashboard depuis cette origine.
- [ ] Enregistrer au moins deux passkeys distinctes et tester le moyen de secours.
- [ ] Vérifier le cookie `__Host-`, `Secure`, `HttpOnly`, `SameSite=Strict`, l'origine des mutations,
  la réauthentification récente et les refus de session/role invalide.
- [ ] Tester promotion/retrait d'un admin, révocation de passkey et de session, puis documenter la
  récupération hors bande et le traitement d'un compte compromis.

**Clôture :** les opérateurs peuvent administrer et modérer depuis le domaine final ; le retrait des
droits et la récupération ont une procédure répétable et auditée.

Référence : [Administration](docs/administration.md).

## PROD-10 — Outbox et maintenance régulière

- [ ] Faire tourner `outbox-worker` en permanence avec la livraison et les dépendances de production.
- [x] Fournir le service/timer configurable, l'installateur et le verrou évitant un chevauchement
  entre reset et maintenance planifiée sur le même hôte.
- [ ] Installer et activer un ordonnanceur pour le service `maintenance`, avec les manifests
  production. Adapter `/etc/histae/maintenance.env`, tester le service puis activer le timer ;
  l'installateur n'active pas automatiquement les unités. L'intervalle initial est de cinq minutes.
- [ ] Vérifier la progression des échéances de matchs, nettoyages photo, rétentions et programmation
  des réconciliations Stripe, y compris après redémarrage du serveur.
- [ ] Vérifier leases, retries, dead letters et reprises administratives sur des données de validation.
- [ ] Dimensionner les lots et traiter `work_remaining` ; raccorder les retards et échecs à la supervision.

**Clôture :** les traitements futurs progressent sans lancement manuel du reset, avec une reprise
observable après un échec et sans rejouer arbitrairement les effets fournisseurs.

Références : [Exploitation](docs/operations.md), [Maintenance](../histae-scripts/README.md#maintenance-planifiee).

## PROD-11 — Supervision et alertes de production

L'intégration `/monitoring/*` peut maintenant cibler les services production avec
`compose.production-observability.yaml` et les upstreams `histae-production-*` du proxy.
La configuration est fournie ; déploiement, collecte et alertes réelles restent à valider.

- [ ] Activer le listener privé de l'API avec `METRICS_ENABLED=true` et un `METRICS_TOKEN` dédié ;
  préparer et synchroniser le secret lu par Prometheus.
- [x] Fournir les manifests de supervision production, les alias du proxy et les labels d'environnement corrigés.
- [ ] Déployer la supervision pour le backend production et vérifier la cible `api:9091` à `UP`.
- [ ] Vérifier l'accès aux interfaces par le dashboard avec une session WebAuthn réelle ; refuser
  les accès anonymes, les admins sans rôle superadmin et les identités forgées. Si une pile autonome
  est retenue à la place, prévoir son accès administratif authentifié.
- [ ] Vérifier Grafana, les règles Prometheus et leurs tests, les refus d'accès anonyme et les
  permissions des opérateurs.
- [ ] Configurer un destinataire réel dans Alertmanager : le receiver `local-dashboard` fourni ne
  contient aucun canal de notification.
- [ ] Désigner un responsable d'exploitation et tester réception, résolution et traitement d'une
  alerte simulée dans un environnement isolé ; inclure l'expiration des certificats dans le suivi.

**Clôture :** la production est collectée, les opérateurs accèdent aux outils de manière protégée et
une alerte exploitable est effectivement reçue par le destinataire choisi.

Références : [Supervision](docs/observability.md),
[Prometheus conteneur](observability/prometheus/prometheus.container.yml),
[Alertmanager](observability/alertmanager/alertmanager.yml).

## PROD-12 — Sauvegardes, restauration et retour arrière

- [ ] Définir la perte de données admissible et le délai de reprise attendu, puis les fréquences et
  rétentions des sauvegardes correspondantes.
- [ ] Automatiser des sauvegardes PostgreSQL cohérentes et conserver une copie chiffrée hors machine.
- [ ] Sauvegarder les objets S3 et leurs métadonnées, ainsi que les éléments de configuration,
  secrets, certificats et rôles nécessaires à la restauration, avec des accès restreints.
- [ ] Restaurer l'ensemble dans une installation isolée et vérifier comptes, consentements, matchs,
  messages, photos privées et traitements de fond. Le script de restauration dev ne couvre pas
  une reprise complète de production et ne conserve pas de sauvegarde.
- [ ] Répéter la procédure de reprise après perte du serveur et le retour arrière compatible avec
  le schéma, les réservations de crédits et les effets fournisseurs déjà écrits.

**Clôture :** une restauration complète réussit depuis les copies hors machine et respecte les
objectifs de reprise retenus ; la procédure et ses résultats sont conservés.

Référence : [Sauvegarde et restauration](docs/database.md#sauvegarde-et-restauration).

## PROD-13 — Textes, consentements et droits

- [ ] Finaliser les CGU, la confidentialité, le consentement sensible et le consentement localisation,
  leurs traductions et les informations propres au site ; retirer les champs de travail des textes publiés.
- [ ] Faire valider les textes, durées de conservation, fournisseurs et parcours prévus ; renseigner
  une référence réelle dans `LEGAL_REVIEW_REFERENCE`.
- [ ] Publier les quatre documents sous des URL HTTPS stables et reporter les versions exactes dans
  les variables `TERMS_OF_SERVICE_*`, `PRIVACY_POLICY_*`, `SENSITIVE_DATA_CONSENT_*` et
  `LOCATION_CONSENT_*`. L'API exige ces valeurs en production.
- [ ] Vérifier que le client présente séparément les documents et consentements correspondants et
  gère refus, retrait et nouvelle version ; confirmer la correspondance des versions dans chaque langue.
- [ ] Tester export, demande de droits, suppression du compte, effacement des photos et traitement
  Stripe ; prévoir les contacts, responsables et procédures de suivi/recours annoncés.

**Clôture :** les documents publiés correspondent à la livraison, leur validation est traçable et
les parcours de consentement et de droits sont utilisables par les clients et les opérateurs.

Références : [Configuration juridique](docs/configuration.md#documents-juridiques),
[Sources du site](../histae-site/docs/legal/README.md), [Droits](docs/http-contract.md#droits-effacement-et-surete).

## PROD-14 — Clients, sécurité, capacité et livraison finale

- [ ] Fixer le périmètre du lancement, les plateformes clientes et les fonctions conditionnelles
  activées. Le dépôt mobile n'est pas inclus dans cette feuille de route : obtenir ses preuves de validation.
- [ ] Rejouer les contrôles Rust, contrats, inventaire et intégrations nécessaires sur la livraison ;
  valider typage, lint, builds et tests des projets dashboard/site concernés.
- [ ] Vérifier sur les vrais clients : OTP, sessions/refresh/logout, onboarding, consentements,
  profil/préférences/localisation, photo, feed et pagination, matchs, messages/SSE, blocage,
  signalement, abonnement et droits.
- [ ] Vérifier les règles de continuation : 24 heures de conversation puis 24 heures de décision,
  choix privés, coût accepté par chaque participant Free, réservations, débit après deux accords,
  libération et changements de semaine/plan. Le catalogue distingue 3 crédits gratuits, un stock
  publicitaire de 3 et un plafond de 6 continuations confirmées Free.
- [ ] Faire réaliser la revue indépendante de sécurité/modération prévue pour la livraison et traiter
  ses blocages ; vérifier les restrictions d'accès et la minimisation des données du dashboard.
- [ ] Définir les seuils attendus puis mesurer sur une pile de validation du serveur cible, en accès
  privé et via un tunnel dédié : lectures, écritures, uploads, workers, p95/p99, erreurs, quotas,
  mémoire, I/O, pools et retards. Les campagnes locales ne prouvent pas la capacité du serveur final.
- [ ] Mesurer aussi les pics et l'endurance du stockage retenu. Le `mem_limit: 1g` SeaweedFS actuel
  est un plafond de configuration à valider, pas une recommandation officielle de mémoire nécessaire.
- [ ] Répéter déploiement/migration/recréation, arrêt et reprise ; vérifier `/health/ready` depuis le
  proxy et les parcours métier. Un healthcheck Docker live ne valide pas les fournisseurs.
- [ ] Conserver le compte rendu final avec images, configuration expurgée, résultats et limites de
  preuve ; identifier le responsable de l'ouverture et le plan de reprise applicable.

**Clôture :** les scénarios réels du périmètre annoncé passent et le serveur respecte les objectifs
retenus ; les résultats et la procédure de livraison sont reproductibles.

Références : [Tests API](tests/README.md), [Contrat](docs/http-contract.md),
[Tests dashboard](../histae-dashboard/test.md), [Site](../histae-site/README.md),
[Charge](docs/load-testing-validation.md), [Mise en production](docs/production-readiness.md).

## PROD-15 — Firebase/FCM, si les notifications push sont activées

- [ ] Configurer le projet Firebase et le compte de service avec `PUSH_PROVIDER=fcm`,
  `FIREBASE_PROJECT_ID`, `FIREBASE_CLIENT_EMAIL` et `FIREBASE_PRIVATE_KEY`.
- [ ] Raccorder les applications mobiles, autorisations de notification et, pour iOS, l'accès APNs.
- [ ] Enregistrer, renouveler et révoquer les tokens d'appareil via l'API ; vérifier logout et effacement.
- [ ] Vérifier la notification durable, sa livraison par l'outbox, les tokens invalides et la réception
  sur les vrais appareils, notamment lorsque l'application est en arrière-plan.

**Clôture conditionnelle :** les push sont validés de bout en bout, ou `PUSH_PROVIDER=disabled` est
explicitement retenu avec un périmètre client et des promesses cohérents. Un succès FCM côté serveur
ne prouve pas à lui seul la réception ou l'affichage sur le téléphone.

Références : [Configuration](docs/configuration.md#groupes-de-variables),
[Appareils et temps réel](docs/http-contract.md#appareils-et-temps-reel).

## PROD-16 — AdMob, si les crédits publicitaires sont activés

- [ ] Configurer les unités rewarded Android/iOS réelles et leur récompense de 1 crédit, avec le
  libellé cohérent avec `ADMOB_REWARD_ITEM` ; les unités de test sont refusées en production.
- [ ] Intégrer et valider le SDK, les informations/choix UMP et le parcours mobile décrit dans le guide.
- [ ] Configurer GET `/api/auth/admob/webhook` pour la vérification serveur et préserver la chaîne de
  requête signée à travers le tunnel ; transmettre uniquement l'UUID opaque de l'intention en `custom_data`.
- [ ] Vérifier callback réel signé, récompense unique, rejeu, expiration, annulation, plafond du stock
  et comptes indisponibles. Le callback du SDK mobile ne doit pas attribuer le crédit.
- [ ] Mettre à jour et faire valider les textes concernés avant activation, puis aligner le catalogue
  et les interfaces avec `rewarded_advertising_enabled`.

**Clôture conditionnelle :** le parcours réel est validé, ou `ADVERTISING_PROVIDER=disabled` est
retenu avec une interface sans publicité active. L'allocation gratuite reste utilisable sans publicité.

Références : [Continuations et récompenses](docs/continuation-and-rewards.md),
[Validation](docs/continuation-validation.md).

## Ordre de réalisation et décision d'ouverture

1. Fixer le périmètre et les décisions fournisseurs/textes ; préparer PROD-01 et PROD-13.
2. Préparer réseaux, certificats, PostgreSQL, Redis et S3 ; terminer PROD-02 à PROD-06.
3. Raccorder Sweego et Stripe, puis finaliser les accès administratifs ; terminer PROD-07 à PROD-09.
4. Installer traitements réguliers, supervision et reprise ; terminer PROD-10 à PROD-12.
5. Terminer les fonctions conditionnelles activées, puis rejouer PROD-14 sur la livraison finale.

- [ ] PROD-01 à PROD-14 sont clôturés avec des preuves applicables à la cible finale.
- [ ] PROD-15 et PROD-16 sont clôturés ou leur désactivation au lancement est consignée.
- [ ] Aucun blocage connu de configuration, sécurité, confidentialité, fournisseur ou reprise n'est ouvert.
- [ ] Les opérateurs disposent des accès, alertes, contacts et procédures nécessaires à l'exploitation.
- [ ] La décision d'ouverture est datée et rattachée à une livraison précise.
