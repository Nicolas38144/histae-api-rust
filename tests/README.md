# Validation locale

[Documentation](../docs/README.md) · [Développement](../docs/development.md) · [Livraison](../docs/production-readiness.md)

Exécuter depuis la racine du dépôt. Les suites isolées injectent leur configuration sans utiliser les secrets du `.env` local.
Utiliser des fixtures fictives et des secrets de développement, jamais une base de production.

## Niveaux et limites de preuve

| Niveau | Emplacement | Vérifie |
| --- | --- | --- |
| Unitaire | Modules `#[cfg(test)]` de `src/` | Règles, configuration, transports, mocks et cycles de vie |
| Contrat | `*_contract.rs`, `http_contract.rs` | Statuts, JSON, validation, guards et permissions |
| Inventaire | `route_inventory.rs` | Les 132 couples méthode/chemin correspondent aux déclarations Axum et aux collections Postman FR/EN ; les requêtes admin utilisent leur cookie |
| PostgreSQL | `*_postgres.rs`, `postgres_compatibility`, `postgres_locks` | SQL, contraintes, verrous, snapshots et concurrence réels |
| Redis / S3 | `redis_integration`, `media_integration` | Adaptateurs sur services locaux |
| Codec | `photo_codec.rs`, `fixtures/photos/` | Formats, taille, métadonnées, corruption et arrêt du processus Node |
| Comparaison | `contract_harness.rs`, `contract/corpus/` | Corpus explicite contre deux API isolées |
| Smoke | `scripts/smoke-api.sh` | Binaire HTTP assemblé : santé, auth, validation et route inexistante |

L’inventaire analyse les sources, pas le montage final du routeur, les schémas JSON ni les autorisations.
Une intégration désactivée par feature n’est pas une preuve de validation de sa dépendance.

## Contrôles sans stockages externes

```bash
cargo fmt --all -- --check
cargo check --locked --all-targets
cargo check --locked --all-targets --no-default-features
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --lib --test http_contract --test route_inventory
```

WebAuthn est activé par défaut via `webauthn-probe`, avec ses prérequis natifs.
`--no-default-features` contrôle la compilation des outils/tests sans WebAuthn ; l’API complète l’exige.
Les tests de contrat par domaine peuvent également être ciblés avec `--test <nom_du_fichier_sans_rs>`.

## Suite complète

Préparer [la pile dev](../docs/container-deployment.md#developpement-local), avec les adresses **hôte**
dans `.env`, puis installer le codec :

```bash
pnpm --dir tools/photo-codec install --frozen-lockfile
cargo test --locked --all-targets --all-features
```

Exemples ciblés :

```bash
cargo test --locked --features postgres-integration --test matches_postgres --test query_performance_postgres
cargo test --locked --features redis-integration --test redis_integration
cargo test --locked --test photo_codec
```

`postgres-integration` et `redis-integration` activent les suites concernées. Les tests codec doivent
pouvoir lire le store pnpm et lancer Node. Vérifier le résultat de chaque suite, pas seulement la compilation.

## Isolation

### Localisation et photos — 23 octobre 2026

Vérifications ciblées : **251 tests unitaires/contrats/codec/inventaire**, **28 tests PostgreSQL**
et **1 test S3 réel** réussis ; Clippy avec toutes les cibles/features et warnings refusés passe.
Les services PostgreSQL et SeaweedFS 4.45 utilisés pour cette campagne étaient jetables, avec secrets
temporaires et nettoyage complet. Aucun service ni compte de production n’a été utilisé.

Les tests couvrent : cellules stables aux pôles et à la ligne de changement de date, coordonnées
minimisées avant stockage, changements concurrents, actualisation dans la même cellule, refus HTTP
avec `Retry-After`, pagination malgré des distances publiques identiques, consentements/exclusions,
effacement, file photo bornée avec annulation/timeout, conversion réelle, surcharge `no-store` d’un
ancien objet S3 et refus d’une URL dont cette surcharge signée a été altérée.

```bash
cargo test --locked --lib --test discovery_contract --test profiles_contract --test photo_codec --test route_inventory
cargo build --locked --bin db-migrate
python3 scripts/verify-continuation-postgres.py --native-migrator target/debug/db-migrate \
  --test profiles_postgres --test discovery_postgres --test matches_postgres \
  --test privacy_rights_postgres --test account_erasure_postgres
# Avec S3 local et son bucket déjà initialisés, configuration locale uniquement :
cargo test --locked --features postgres-integration --test media_integration \
  s3_put_presign_and_idempotent_delete_work_against_local_storage -- --exact
```

Le lanceur PostgreSQL vérifie aussi le retrait unique des anciennes coordonnées lors de l’upgrade et
la conservation des nouvelles présences lors des migrations suivantes. Ajouter `--sudo-docker` si
nécessaire ; Cargo reste exécuté sans sudo. Les limites de confidentialité et l’ordre de livraison
sont décrits dans [Confidentialité et capacité photo](../docs/privacy-and-photo-security.md).

### Précautions pour les autres suites

Les fixtures PostgreSQL contrôlent la cible de développement et utilisent des schémas/données isolés.
Le claim outbox parcourt toute une file : son test crée `histae_outbox_test_<UUID>`, applique le migrateur
réel et supprime cette base après fermeture, y compris si une assertion échoue. Le rôle local doit pouvoir
créer une base ; la file de la base de développement partagée n’est pas nettoyée par ce test.

`sms_safety_postgres` crée également une base privée avec le migrateur réel : il vérifie la concurrence
sur les plafonds par numéro, la suspension/version/rôle superadmin, la conservation au redéploiement,
le rejeu OTP refusé et la purge bornée. Ce test exige le compte administrateur de développement pour
créer/supprimer sa base ; il ne touche pas les compteurs d’une API partagée et ne contacte pas Sweego.

Redis utilise sa base logique et des clés de test ; S3 des objets dédiés. Ne pas arrêter les dépendances
partagées pour simuler une panne ni remplacer le nettoyage ciblé par une purge Docker globale.
Pour les garde-fous précis, consulter les fixtures et le [guide de base de données](../docs/database.md).

Les comptes applicatifs Docker dev et production sont limités. Les fixtures qui créent des schémas ou
bases nécessitent un compte administrateur de développement ; l'API utilise toujours `histae_app`.
`sudo bash scripts/verify-postgres-roles.sh` valide l'installation neuve avec le migrateur réel et les
permissions applicatives dans un conteneur temporaire. Voir [Rôles PostgreSQL](../docs/postgres-roles.md).

## Régressions couvertes

`cargo test --test catalog_offers` vérifie les trois offres Free/Premium Mensuel/Premium Annuel,
la conservation des deux niveaux de droits, la sérialisation et la propagation des prix/devise/essai
du catalogue. Il ne contacte ni PostgreSQL ni Stripe. Les tests clients vérifient l'affichage séparé
et le choix de la bonne périodicité ; ils ne prouvent pas un paiement réel.

| Sujet | Exemples |
| --- | --- |
| Auth / transport | JWT aux bornes temporelles, claims invalides, IP socket/X-Forwarded-For falsifié, coercition numérique, corps JSON invalides ou trop grands |
| Consentements / administration | Onboarding des appareils/SSE, données sensibles, sessions admin, rôles et consultations auditées |
| Cycle de vie | Drain après échec, worker terminé inopinément, annulation au drop, SSE bloqué, expiration prioritaire du JWT et scrape interrompu |
| Abonnements Stripe / revenus | Requêtes Checkout figées, reprises bornées, concurrence, abonnement distant sans webhook, prix historiques, projections dépassées et montants des factures payées |
| Fournisseurs / exports | Durées OAuth hors plage, diagnostics Stripe préservés pendant effacement, date UTC et bigint exportés sans perte, nettoyage des fichiers temporaires |
| SQL | Filtres et pages admin, compte sans profil, compteurs, curseurs, fiche/audit et équivalence des agrégats de files |
| Découverte et blocage | Rejeu d’un like après création du match, filtres toujours appliqués, match terminé jamais rouvert, création concurrente d’un blocage et d’un match sérialisée |
| Clôture de match | Participants uniquement, rejeux, fermeture concurrente, messages en attente refusés, événement SSE et rétention/quota conservés |
| Continuations / récompenses | Envoi suspendu, choix privés, coût explicite, réservations concurrentes, double débit évité, deux participants, changement de semaine/plan/consentement, signature ECDSA et récompense unique plafonnée |
| Migrateur | Base neuve/peuplée, idempotence, index concurrents et réparation d’un index invalide |
| Confidentialité | `Debug` expurgé de configuration, erreurs/labels bornés et absence de valeurs sensibles |

Les mesures de performance restent séparées des assertions fonctionnelles : voir [Performance SQL](../docs/sql-performance.md).
Les [contrôles sous charge](../docs/load-testing-validation.md) testent aussi les refus de l’API
assemblée et les variantes du feed. Ils utilisent uniquement les comptes fictifs d’une API temporaire dev.

<a id="comparaison-de-deux-api"></a>

## Comparaison de deux API

Le binaire `contract-compare` exige deux URLs et namespaces d’état distincts. Préparer également leurs
bases, clés Redis et préfixes S3 isolés. Exemple sur deux déploiements locaux déjà disponibles :

```bash
cargo run --locked --bin contract-compare -- \
  --corpus tests/contract/corpus/smoke.json \
  --reference-url http://127.0.0.1:8080/ --reference-state reference-test \
  --candidate-url http://127.0.0.1:8081/ --candidate-state candidate-test
```

Sortie : `0` parité, `1` différences, `2` configuration/corpus/transport invalide.
`--allow-isolated-mutations` autorise les scénarios correspondants mais ne crée pas leur isolation.

## Validations externes

Les tests locaux ne prouvent ni les cérémonies WebAuthn du navigateur, ni la livraison réelle des fournisseurs,
ni la capacité en charge, ni une restauration complète hors machine. Exécuter et conserver ces preuves selon
[Mise en production](../docs/production-readiness.md).

## Crédits et récompenses

Le [guide](../docs/continuation-and-rewards.md) décrit les règles et la validation AdMob restante. Les tests `matches_postgres` couvrent les wallets, récompenses et consentements avec le rôle applicatif, sans appel OTP et sans publicité réelle. Les tests ECDSA utilisent une clé de test générée en mémoire ; ils ne désactivent jamais la vérification de production. Les résultats et limites sont regroupés dans [Audit fonctionnel](../docs/feature-audit.md).

Les tests unitaires `advertising::` couvrent aussi la validation d’URL réservée à la console AdMob : un callback signé avec `custom_data=histae-ssv-validation` est distingué d’une récompense UUID, une signature altérée reste refusée et le chemin de validation ne doit jamais appeler le stockage des récompenses. Exécuter `cargo test --locked --lib advertising::` dans un environnement Rust configuré. Ces tests ne prouvent pas à eux seuls la validation réelle de la console ni le crédit après une vidéo sur appareil.

Pour ces suites, utiliser le lanceur isolé afin que le worker de la pile dev ne récupère pas les événements des fixtures :

```bash
cargo build --locked --bin db-migrate
sudo -v
python3 scripts/verify-continuation-postgres.py --sudo-docker \
  --native-migrator target/debug/db-migrate
```

Cargo reste exécuté par l’utilisateur courant ; `sudo` s’applique uniquement aux commandes Docker. Avec un accès Docker déjà disponible, omettre `sudo -v` et `--sudo-docker`. Le lanceur crée un PostgreSQL avec une publication sur une adresse loopback et un port aléatoire, teste le migrateur fraîchement compilé sur base neuve et ancienne, puis exécute les suites avec `histae_app`. Ses fixtures, conteneur, tmpfs et mots de passe temporaires sont supprimés à la fin. Il ne se connecte pas au PostgreSQL de la pile dev.

Pour vérifier le migrateur de l’image Docker à la place du binaire local, reconstruire cette image depuis
le code actuel, omettre `--native-migrator` et préciser son nom avec `--api-image` (défaut : `histae-api:local`).
Les images doivent être disponibles localement : le lanceur les vérifie avant de créer PostgreSQL et ne les
télécharge pas implicitement. En cas d’échec de migration Docker, seuls le code de sortie et les
`error_code` structurés sont affichés ; les mots de passe, SQL et environnements restent masqués.

## Vérifications HTTP derrière Caddy

Le [guide Caddy](../docs/caddy-migration.md) décrit les commandes de validation des configurations et les
contrôles après déploiement : authentification, erreurs HTTP, SSE et photos signées. Les résultats de la
campagne locale de migration sont conservés dans ce guide.

## Signature des récompenses publicitaires

Les tests `advertising::ssv::tests` couvrent la signature, le cache périmé, l’actualisation concurrente et
les limites d’admission, avec des clés générées en mémoire et sans appel à Google.

<a id="mode-pause"></a>

## Mode pause de la découverte — 19 octobre 2026

La compilation et les tests de ce réglage sont vérifiés sur le poste Rust, avec une instance
PostgreSQL/PostGIS jetable pour la migration et les cas de concurrence. Aucun compte réel ni fournisseur
SMS n’est utilisé.

### Cas couverts

- `discovery_contract` : authentification, booléen strict, affectation idempotente, diagnostic
  `resume_discovery`, blocage et reprise du feed/des swipes.
- `discovery_postgres` : persistance entre repositories, exclusion des profils en pause, écriture refusée
  même avec une carte chargée précédemment et conservation des conversations existantes.
- `writes_wait_for_a_concurrent_pause_and_recheck_both_participants` : pause non encore commitée du
  demandeur ou du candidat, attente effective sur PostgreSQL, puis refus du swipe et de la création de
  match après commit. Aucun swipe ni match ne subsiste après ces quatre courses contrôlées.
- `privacy_rights_postgres` et `account_erasure_postgres` : préférence présente dans l’export propriétaire,
  puis remise à `false` pendant l’anonymisation. Le test d’effacement reproduit l’oubli avant correction.
- Lanceur isolé : colonne créée à `false`, ajout sur une base existante et conservation d’une pause
  active lors d’une seconde migration ; les migrations de crédits restent aussi vérifiées.

### Résultats du 19 octobre 2026

| Vérification | Résultat |
| --- | --- |
| Compilation des binaires (`cargo build --locked --bins`) | Réussie |
| Suite Rust avec features par défaut, tous les targets | 320 tests réussis |
| Suites PostgreSQL ciblées, feature `postgres-integration` | 27 tests réussis : découverte, matchs, profils, export et effacement |
| Migration neuve, upgrade et réexécution | Réussis ; préférence existante conservée |
| Clippy, tous les targets et toutes les features, warnings refusés | Réussi |
| rustfmt et `git diff --check` | Réussis |
| Inventaire Axum, contrat et collections Postman FR/EN | 113 routes cohérentes |

Le test de logs AdMob utilise désormais un processus isolé pour éviter les interactions entre son
subscriber temporaire et le cache Tracing partagé par les tests parallèles. Les assertions de rejet
cryptographique et d’absence de données privées restent actives ; le code de production est conservé.

### Rejouer

```bash
cargo build --locked --bins
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets

# Construire préalablement l’image API depuis les sources actuelles, comme décrit plus haut.
sudo -v
python3 scripts/verify-continuation-postgres.py --sudo-docker \
  --test discovery_postgres --test matches_postgres \
  --test privacy_rights_postgres --test account_erasure_postgres --test profiles_postgres
```

Les 27 tests PostgreSQL ciblés réussissent, y compris les cas pause et les migrations.
La campagne locale emploie le migrateur natif fraîchement compilé avec les mêmes rôles séparés ;
le lanceur maintenu ci-dessus emploie le migrateur de l’image indiquée par `--api-image`.
Les résultats Flutter avec réponses simulées ne prouvent pas le fonctionnement de cette API.
Le déploiement exige le passage du migrateur avant le démarrage de la nouvelle API.

## Internationalisation

Les scénarios multilingues sont couverts par `localization_postgres`, les contrats de profil et de session,
les transports simulés Sweego/Stripe/FCM et les règles de modération. Le runner PostgreSQL inclut ces
suites, vérifie la couverture initiale des traductions, simule l’ancien schéma puis prouve que les migrations
répétées conservent les préférences, les modifications et les traductions supprimées. Voir le
[guide d’internationalisation](../docs/internationalization.md).

## Protection SMS (21 octobre 2026)

La fonctionnalité est documentée dans [le guide Sweego](../docs/sms-safety.md). Les scénarios couvrent
les refus avant tout appel fournisseur, l’indisponibilité PostgreSQL, les pics absolus/relatifs, les
mutations réservées au superadmin et les versions périmées. Le contrat et les deux collections Postman
comprennent désormais **115 routes**. Les scénarios Prometheus incluent déclenchement et résolution
des alertes de pic et de suspension. Les fournisseurs sont simulés ; aucune consommation de SMS.

Vérifications locales réalisées :

- 240 tests Rust ciblés (bibliothèque, contrats HTTP/OTP, inventaire des routes et client Sweego).
- 10 tests d’intégration PostgreSQL, dans un serveur jetable, dont la migration initiale et répétée,
  30 réservations concurrentes pour un même numéro et le rejeu OTP après épuisement du quota.
- Clippy sur toutes les cibles et fonctionnalités avec les avertissements traités comme erreurs.
- Validation des règles Prometheus avec `promtool`.
- Dashboard : typecheck, lint, build, 96 tests unitaires/composants et 5 parcours Chromium ciblés.

Ces vérifications ne valident ni un envoi SMS réel ni le déploiement sur le serveur de production.

<a id="achats-multicanaux"></a>

## Achats Apple/Google et Stripe web — 22 octobre 2026

Le [guide de facturation](../docs/store-billing.md) décrit les nouveaux contrats, les variables,
la livraison et les essais fournisseur restant nécessaires. L’API et Postman FR/EN comprennent
**131 routes**. Les anciennes routes mobiles Stripe Checkout/portail sont des refus authentifiés
`410 billing_web_only` ; le site utilise sa propre session OTP.

### Résultats locaux

| Vérification | Résultat |
| --- | --- |
| Rust bibliothèque + contrats billing/webhooks/OTP + inventaire | 237 tests réussis (221 + 2 + 5 + 7 + 2) |
| Binaires Rust, contrôle sans features par défaut, rustfmt et Clippy tous targets/features avec `-D warnings` | Réussis |
| Migration PostgreSQL fraîche, répétée et upgrade quota/pause/langue | Réussies avec le migrateur natif et les rôles limités |
| Régressions PostgreSQL Stripe, matchs/continuations, maintenance, export et effacement | 29 tests réussis |
| Nouvelle suite PostgreSQL des achats natifs, OTP web et import financier | 6 tests réussis |
| Vérificateur Node privé | 5 tests réussis ; image Docker construite |
| Flutter | Analyse sans problème ; 236 tests réussis après refactoring et ajout de l’UUID |
| Site | TypeScript, lint, export statique et 4 tests navigateur/DTO réussis |
| Dashboard | TypeScript, lint et build ; 98 tests initiaux, puis 12 contrôles ciblés et 2 parcours Chromium stores réussis après refactoring |
| Compose dev/production et deux Caddyfiles modifiés | Configurations valides ; clés privées réservées au vérificateur |
| Alertes Prometheus | Règles valides ; déclenchement et résolution des trois alertes stores vérifiés |

Les 35 tests PostgreSQL ont été rejoués ensemble dans un serveur jetable, avec simulation
d’un ancien schéma sans tables de stores et conservation d’une projection Stripe, sans accès à la base partagée. Les fichiers de logs locaux ne constituent pas des artefacts
à versionner : garder les commandes et preuves synthétiques ci-dessous, sans secrets.

### Scénarios prouvés

- Les droits Stripe et natifs restent indépendants ; début de période inclusif et fin exclusive.
  Une observation fournisseur ancienne ne remplace pas la nouvelle ; un token Google remplacé
  reste exclu même s’il est rejoué.
- La liaison opaque ne permet pas de réattribuer un achat à un autre compte. L’effacement coupe
  les références et sessions sans recréer un lien lors d’une restauration.
- L’acquittement Google est durable. Produit/environnement invalides sont refusés avant écriture ;
  un JWS Apple forgé est rejeté par la bibliothèque officielle et ses certificats de confiance.
- UUID/téléphone incorrects ne créent aucun compte. Les cinq tentatives d’un challenge sont
  consommées atomiquement ; son OTP ne peut servir ni au mobile ni à un autre challenge.
  Les sessions expirent et vérifient l’état courant du compte.
- Le site exige l’OTP avant paiement, reprend une réponse Checkout perdue avec la même clé,
  refuse les redirections non autorisées et ne propose pas de second achat Stripe à un abonné natif.
- L’import financier vérifie le rôle superadmin courant, accepte un rejeu identique et annule tout
  le rapport si une ligne contredit une ligne existante ; le dashboard exige une confirmation.

### Refactoring du parcours et identifiant du profil

Le service web est testé avec OTP et persistance réels, fournisseur SMS fictif et Stripe désactivé :
aucun SMS pour un couple UUID/téléphone incorrect, code inutilisable sur le mobile, exactement une
session lors de deux confirmations concurrentes, onboarding requis pour payer et refus après ban.
Les handlers ne possèdent plus ce workflow ; les projections web et administratives sont typées.

Flutter vérifie l’UUID copiable/sélectionnable dans quatre langues et à 200 %, ainsi que le refus
localisé d’une copie impossible. Les tests de vie privée, logout et vidéo ont confirmé le défilement
du profil avec la nouvelle zone de sélection. Les contrôleurs et fixtures d’achat utilisent des
valeurs immuables, sans types du SDK ; seul l’adaptateur natif les manipule.

Les contrôles frontend ciblent aussi les réponses admin malformées (erreur localisée contrôlée),
les totaux par devise et les nets inconnus. Les parcours de confirmation d’import et les quatre
parcours web passent avec les nouveaux hooks/composants. Les tests n’appellent aucun fournisseur réel.

### Rejouer les contrôles ciblés

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --lib --test billing_contract --test billing_webhook_contract \
  --test otp_sweego_contract --test route_inventory
cargo build --locked --bin db-migrate
python3 scripts/verify-continuation-postgres.py --native-migrator target/debug/db-migrate \
  --test store_billing_postgres --test billing_postgres --test matches_postgres \
  --test maintenance_outbox_postgres --test privacy_rights_postgres --test account_erasure_postgres
npm --prefix services/store-verifier test
```

L’option `--native-migrator` teste le binaire fraîchement compilé via le port loopback jetable ;
omettre cette option pour employer le migrateur de l’image API reconstruite. Ajouter
`--sudo-docker` après `sudo -v` si nécessaire. Les tests HTTP factices exigent le droit d’ouvrir
un listener loopback. Aucune commande de cette section ne doit viser une base de production.

Dans les dépôts clients : `flutter analyze` et `flutter test` ; site `pnpm typecheck`, `pnpm lint`,
`pnpm build`, `pnpm test` ; dashboard `pnpm check`, `pnpm test:unit` et
`pnpm exec playwright test tests/e2e/store-billing.spec.ts`.

### Limites

Aucun SMS ni paiement réel, achat Sandbox/Play test, build Android signé ou build iOS n’a été effectué.
Le SDK Flutter temporaire a permis l’analyse et les tests Dart/widgets sur ce poste, pas une validation
native des stores. Les tests navigateur interceptent l’API ; les tests PostgreSQL vérifient séparément
ses transactions réelles avec des fournisseurs simulés. Les credentials/produits, notifications réelles,
réconciliation fournisseur, essais sur appareils et déploiement derrière Cloudflare restent à valider.
Les fournisseurs natifs et `BILLING_ENABLED` mobile restent désactivés avant cette configuration.
Les brouillons juridiques mis à jour exigent une validation avant publication.
