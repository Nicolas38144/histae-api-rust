# Validation locale

[Documentation](../docs/README.md) · [Développement](../docs/development.md) · [Livraison](../docs/production-readiness.md)

Exécuter depuis la racine du dépôt. Les suites isolées injectent leur configuration sans utiliser les secrets du `.env` local.
Utiliser des fixtures fictives et des secrets de développement, jamais une base de production.

## Niveaux et limites de preuve

| Niveau | Emplacement | Vérifie |
| --- | --- | --- |
| Unitaire | Modules `#[cfg(test)]` de `src/` | Règles, configuration, transports, mocks et cycles de vie |
| Contrat | `*_contract.rs`, `http_contract.rs` | Statuts, JSON, validation, guards et permissions |
| Inventaire | `route_inventory.rs` | Les 137 couples méthode/chemin correspondent aux déclarations Axum et aux collections Postman FR/EN ; les requêtes admin utilisent leur cookie |
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

Le protocole de [reprise des refresh mobiles](../docs/refresh-recovery.md) ajoute des tests
crypto isolés, le contrat `mobile_auth_contract` et les scénarios PostgreSQL de `mobile_identity`
(réponse perdue, concurrence, rotation suivante, expiration, purge, bannissement et révocation).
Le module `tests/mobile_identity/recovery.rs` vérifie aussi les résultats chiffrés altérés et le
nettoyage concurrent d'une transaction de rotation encore ouverte. Le lanceur PostgreSQL vérifie l'upgrade depuis
l'ancien schéma sans modifier les sessions historiques ni leurs échéances.

Campagne du **18 novembre 2026**, après retrait des paiements web : **360 tests Rust** avec
`cargo test --locked --all-targets` et **102 tests PostgreSQL** : 100 dans la campagne isolée principale, puis deux contrôles SQL ciblés.
Formatage et Clippy toutes cibles/fonctionnalités avec avertissements refusés réussis.
La migration a aussi été testée deux fois depuis le schéma complet du commit précédent.
Les cinq tests du vérificateur privé, les 26 règles Prometheus et leurs tests réussissent.
Le reverse proxy passe sa validation Caddy et six contrôles de routage avec backend simulé.
Aucun fournisseur réel, appareil ou déploiement de production n’a été testé ; Redis/S3 réels
n’ont pas été rejoués. Les bases jetables et leurs identifiants sont supprimés après vérification.

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

### Notifications de cycle de vie et de sécurité — 5 novembre 2026

**380 tests Rust** avec `cargo test --locked --all-targets` et **39 tests PostgreSQL réussis** dans
huit suites : notifications, matchs, inactivité, droits RGPD, sessions mobiles, maintenance/outbox,
effacement et langues. Formatage et Clippy avec toutes les cibles/features et warnings refusés
passent. La campagne utilise une base jetable avec les rôles PostgreSQL limités ; données, conteneur
et identifiants temporaires sont supprimés.

Les régressions couvrent les échéances du match, de la session destinataire et de la nouvelle
connexion, la conservation d'un jour des réponses RGPD, les destinataires et le rejeu des
continuations, les générations du partage photo, l'unicité des rappels et l'absence de texte privé
dans les alertes. L'upgrade répété conserve notifications historiques, échéances des matchs et
générations des consentements photo. Les commandes et la recette sur appareils restent dans le
[guide des notifications push](../docs/push-notifications.md#organization-and-verification).

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

Il simule également l’ancienne rétention de 365 jours : likes et profils passés sont ramenés à
30 jours depuis leur date initiale, y compris pour un compte déjà désactivé. Le scénario vérifie
l’expiration des anciens likes, la conservation des choix récents et des dates, le rétablissement
du contrôle de compte actif, le nouveau défaut SQL et l’absence de prolongation lors d’une seconde migration.
Le contrôle restauré est aussi exercé par une écriture refusée sur un compte désactivé.
La suite `discovery_postgres` vérifie qu’un like de 31 jours ne crée plus de match, qu’un profil
passé expiré redevient éligible, qu’un rejeu conserve ses dates et que la purge retire les anciens
choix tout en préservant les choix récents et le match créé après deux nouveaux likes.

### Précautions pour les autres suites

Les fixtures PostgreSQL contrôlent la cible de développement et utilisent des schémas/données isolés.
Le claim outbox parcourt toute une file : son test crée `histae_outbox_test_<UUID>`, applique le migrateur
réel et supprime cette base après fermeture, y compris si une assertion échoue. Le rôle local doit pouvoir
créer une base ; la file de la base de développement partagée n’est pas nettoyée par ce test.

`sms_safety_postgres` crée également une base privée avec le migrateur réel : il vérifie la concurrence
sur les plafonds par numéro, la suspension/version/rôle superadmin, la conservation au redéploiement,
le rejeu OTP refusé et la purge bornée. Ce test exige le compte administrateur de développement pour
créer/supprimer sa base ; il ne touche pas les compteurs d’une API partagée et ne contacte pas Sweego.
Le lanceur isolé accepte `--test sms_safety_postgres` et fournit pour cette seule fixture l’administrateur
de son cluster jetable ; les autres suites sélectionnées continuent d’utiliser `histae_app`.

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
du catalogue. Il ne contacte ni PostgreSQL ni fournisseur de paiement. Les tests clients vérifient l'affichage séparé
et le choix de la bonne périodicité ; ils ne prouvent pas un paiement réel.

| Sujet | Exemples |
| --- | --- |
| Auth / transport | JWT aux bornes temporelles, claims invalides, IP socket/X-Forwarded-For falsifié, coercition numérique, corps JSON invalides ou trop grands |
| Consentements / administration | Onboarding des appareils/SSE, données sensibles, sessions admin, rôles et consultations auditées |
| Cycle de vie | Drain après échec, worker terminé inopinément, annulation au drop, SSE bloqué, expiration prioritaire du JWT et scrape interrompu |
| Achats natifs / revenus | Liaison au compte, rejeux, concurrence, périodes, projections dépassées, acquittement et rapports financiers |
| Fournisseurs / exports | Durées OAuth hors plage, date UTC et bigint exportés sans perte, nettoyage des fichiers temporaires |
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
le code actuel, omettre `--native-migrator` et préciser son nom avec `--api-image` (défaut : `histae-api-rust:local`).
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
les transports simulés Sweego/FCM et les règles de modération. Le runner PostgreSQL inclut ces
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

### Intentions OTP asynchrones — 25 octobre 2026

Le refactoring sépare modèles/erreurs, port de persistance, chiffrement lié à l’UUID, mise en file,
traitement outbox et transactions PostgreSQL dans `identity/mobile/otp/requests/`. L’assemblage API/worker
utilise la façade OTP. La traduction des erreurs reste dans le transport HTTP ; la classification des
plafonds par numéro et globaux est partagée par les chemins de livraison et de rejeu.

**257 tests réussis** : 232 unitaires, 13 contrats OTP/Sweego, 6 clients Sweego simulés, 2 inventaires,
3 intégrations OTP et 1 scénario complet de sécurité SMS sur PostgreSQL jetable. Ce dernier vérifie
notamment 30 réservations concurrentes, les limites globales entre numéros, les rejeux, l’expiration,
le chiffrement du téléphone et la purge. Le migrateur réel a également passé ses contrôles sur base
neuve, schéma ancien et migration répétée. Clippy toutes cibles/features avec `-D warnings` et formatage passent.

Le refactoring est vérifié sans envoi fournisseur ni consommation de crédits. Les commandes de
reproduction avec les rôles de test sont dans [Protection des SMS](../docs/sms-safety.md#livraison-et-vérification).

<a id="achats-multicanaux"></a>

## Achats natifs et retrait du paiement web — 18 novembre 2026

Le [guide des achats](../docs/store-billing.md) et le [guide de migration](../docs/billing.md)
décrivent le contrat natif, les variables et les étapes de livraison. L’inventaire et les deux
collections Postman couvrent 137 routes. Les anciennes routes de paiement web retournent 404.

Contrôles à rejouer :

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo build --locked --bin db-migrate
python3 scripts/verify-continuation-postgres.py --native-migrator target/debug/db-migrate
npm --prefix services/store-verifier test
```

Le runner utilise une base jetable, les rôles limités et des fournisseurs simulés. Il vérifie aussi
la migration d’anciens paiements web de test, la conservation des preuves Apple/Google, des droits
internes et des Premium offerts, et l’idempotence d’une seconde migration. Les suites
`outbox_postgres` et `sms_safety_postgres` sont incluses par défaut et utilisent leur propre base
jetable sous l’administrateur temporaire. `--test` permet de sélectionner une suite. Aucun SMS ni paiement réel n’est déclenché.
Les essais Sandbox/Play test/TestFlight et les validations du tunnel distant restent nécessaires.
Cette intervention ne modifie ni ne valide les dépôts clients.

## Retrait du vérificateur photo (2026-10-26)

Le service d’analyse et ses contrôles visage/netteté/NSFW sont retirés. Une photo est disponible
dès l’upload ; la modération manuelle conserve ses droits, audit et contrôle de version.
Les contrôles techniques du fichier et les règles d’accès privé restent vérifiés.

Vérifications locales : 270 tests Rust unitaires/contrats/codec et 29 tests PostgreSQL réussis.
La base temporaire simule les anciennes colonnes et décisions automatiques : migration vers les
photos disponibles, conservation des décisions humaines et absence de changement à la réexécution.
Le test média PostgreSQL valide activation, idempotence, remplacement et outbox sans appeler S3.
La suite S3 réelle et le parcours sur un téléphone ne sont pas inclus dans cette vérification.

```bash
cargo build --locked --bin db-migrate
python3 scripts/verify-continuation-postgres.py --native-migrator target/debug/db-migrate \
  --test discovery_postgres --test matches_postgres --test moderation_postgres \
  --test account_erasure_postgres --test media_integration
```

### Inactivité des conversations confirmées

`matches_inactivity_postgres` vérifie les seuils 10/23/30 jours, la même règle Free/Premium,
la reprise sans crédit, les lectures et rejeux sans prolongation, les avertissements uniques aux
deux participants, le refus des push obsolètes et la suppression sans rétention supplémentaire.
Une course avec le verrou d’un envoi vérifie qu’une maintenance ne supprime pas un match renouvelé.
Le seuil d’avertissement est testé juste avant J23 puis à l’échéance exacte. Les projections
mobile et administration utilisent une horloge injectée pour vérifier l’archivage sans dépendre
de la date réelle du poste.
Le lanceur isolé vérifie aussi le préavis initial pour les anciennes bases et l’absence de remise
à zéro lors d’un second déploiement.

```bash
cargo build --locked --bin db-migrate
python3 scripts/verify-continuation-postgres.py --native-migrator target/debug/db-migrate \
  --test matches_inactivity_postgres --test matches_postgres --test notifications_postgres
```

### Accès de revue Google Play

Le [guide de publication](../docs/google-play-review.md) décrit les trois variables privées et la
préparation du compte dans l’app. Les tests `otp_sweego_contract` vérifient l’activation explicite,
le code réutilisable, l’absence de SMS/outbox, les quotas, le délai de renvoi et l’idempotence.
Une première création exige une intention d’inscription active ; une connexion ou une inscription
expirée ne crée aucun compte. Les formulaires de profil et de consentement restent à remplir. `play_review_postgres`
vérifie la création concurrente d’un seul compte, les refus pour les comptes ordinaires, bannis,
supprimés ou sous tombstone, la contrainte de rôle, le refus de promotion par un superadmin et la suppression concurrente.
Il vérifie aussi le Premium offert aux nouveaux comptes de revue comme aux comptes déjà inscrits,
les continuations Premium illimitées, le blocage d’un achat inutile, l’absence de facture ou de
projection fournisseur créée, la conservation de l’historique natif et le retrait des droits
offerts si le compte n’est plus marqué pour la revue, est banni ou supprimé.
Ces scénarios de facturation sont regroupés dans `play_review/premium.rs`, avec des helpers pour
l’inscription de la fixture, la forme du droit offert et l’absence d’achat. Les scénarios
d’authentification restent dans `play_review_postgres.rs` ; la commande de lancement est inchangée.
Le lanceur isolé vérifie également la migration d’une base sans marqueur de revue puis sa réexécution.

```bash
cargo test --locked --lib --test otp_sweego_contract --test mobile_auth_contract
cargo build --locked --bin db-migrate
python3 scripts/verify-continuation-postgres.py --native-migrator target/debug/db-migrate \
  --test play_review_postgres --test otp_delivery --test administration_reports_postgres \
  --test store_billing_postgres
```

### Premium offert par Histae

`complimentary_premium_postgres` couvre l’attribution et le retrait idempotents par un superadmin,
l’audit atomique (y compris son échec), les métadonnées du détail administratif, les droits mobiles
et les continuations, la conservation d’un abonnement payant, le refus des comptes bannis/supprimés
ou de revue, la revalidation du rôle en base, le verrou commun aux confirmations et l’anonymisation.
Les tests unitaires de l’administration contrôlent le corps strict et les permissions du service.
Le lanceur isolé simule aussi une ancienne base sans la table ni les actions d’audit et vérifie
que les migrations répétées conservent la date de l’offre.

```bash
cargo build --locked --bin db-migrate
python3 scripts/verify-continuation-postgres.py --native-migrator target/debug/db-migrate \
  --test complimentary_premium_postgres --test play_review_postgres --test billing_postgres \
  --test store_billing_postgres --test matches_postgres --test account_erasure_postgres
```

### Suppression des données depuis le site

`web_privacy_postgres` exerce les routes HTTP avec PostgreSQL isolé et des fournisseurs simulés :
origine exacte, cookies absents/ambigus/expirés, refus des identités mobiles et admin, gardes avant
validation des champs JSON, budget partagé par compte, retraits partiels idempotents et limités au compte,
historique des consentements, nettoyage photo différé, jeton d'effacement lié au compte et invalidation
des sessions. Le parcours OTP partage limites et cookie avec le paiement, tout en restant disponible
avec paiement désactivé et onboarding incomplet. Aucun achat web n’est proposé.
Les collections Postman FR/EN couvrent les 140 routes avec les protections de cookie et d'origine.
Validation locale du 29 octobre 2026 : 373 tests Rust par défaut et 17 tests PostgreSQL réussis,
dont les six nouveaux scénarios web. Formatage et Clippy sans avertissement. Les migrations neuves,
les upgrades et leur répétition sont validés ; le lanceur supprime sa base et ses secrets temporaires.

```bash
cargo build --locked --bin db-migrate
python3 scripts/verify-continuation-postgres.py --native-migrator target/debug/db-migrate \
  --test web_privacy_postgres --test store_billing_postgres \
  --test privacy_rights_postgres --test account_erasure_postgres
```

### Lecture administrative des conversations signalées

`administration_reports_postgres` utilise des messages réels et deux acteurs distincts (admin et
superadmin). La lecture exige un signalement conservé qui référence exactement la conversation et
ses deux participants, dans l'un ou l'autre sens. Les cas de profil, conversation différente et
participants incohérents restent interdits. Les statuts `pending`, `reviewed` et `dismissed` sont couverts.

Les assertions portent sur le contenu renvoyé, l'isolation des conversations, les pages par offset et
curseur, le retrait d'accès après suppression du signalement et l'absence d'audit pour les refus.
Chaque lecture autorisée produit deux audits ; un échec d'audit annule la transaction. La projection
publique mobile reste dépourvue de `can_view_messages` ; la projection administrative reste aplatie.

Validation locale du 31 octobre 2026 : 373 tests Rust par défaut et 29 tests PostgreSQL réussis
(administration/signalements, matchs, modération et Premium offert), formatage et Clippy réussis.
Les migrations neuves, les upgrades et leur répétition passent dans la base jetable, supprimée ensuite.

```bash
cargo fmt --all -- --check
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo test --locked --all-targets
cargo build --locked --bin db-migrate
python3 scripts/verify-continuation-postgres.py --native-migrator target/debug/db-migrate \
  --test administration_reports_postgres --test matches_postgres \
  --test moderation_postgres --test complimentary_premium_postgres
```

### Numéros SMS publics et exceptions (6 novembre 2026)

La fixture `phone_blocklist_postgres` nécessite le lanceur jetable. Elle vérifie le refus avant
réservation de budget, le contrôle des OTP antérieurs, les exceptions prioritaires sur les imports,
l’expiration, les versions concurrentes, le rôle réel de l’admin et l’audit automatique du superadmin.
Deux cas concurrents couvrent la décision en cours et l’ordre des verrous avec la rétention. Le rôle
`histae_phone_sync` importe réellement sans pouvoir lire les comptes ou modifier les décisions.
Le lanceur vérifie aussi l’ajout de ce rôle sur un cluster existant et les upgrades répétés sans
modifier les exceptions, dates des sources ou l’empreinte de contrôle de la clé.

```bash
cargo build --locked --bin db-migrate --bin phone-blocklist-sync
python3 scripts/verify-continuation-postgres.py \
  --native-migrator target/debug/db-migrate \
  --test phone_blocklist_postgres --test sms_safety_postgres --test otp_delivery
```

`--native-phone-sync target/debug/phone-blocklist-sync` ajoute une consultation réelle des deux
listes publiques, toujours dans la base jetable. Les tests standard n’en dépendent pas. Aucun envoi
Sweego n’est effectué. Voir [le guide et les procédures](../docs/phone-blocklist.md).

Validation locale : 384 tests Rust par défaut réussis ; 36 tests PostgreSQL distincts réussis sur les parcours de politique SMS,
budget, OTP, identité mobile, confidentialité web, facturation, outbox, effacement et administration.
Le smoke réseau a importé 7 observations françaises Castle et 228 iP1, sans données en clair
persistées dans les tables de politique ; les volumes dépendent des sources et de la date.
Les deux nouvelles règles Prometheus et les cinq topologies Compose passent leur validation.
Le dashboard passe son build, TypeScript, ESLint, 166 tests unitaires et 12 tests navigateur
(`pnpm exec playwright test --grep-invert @real --workers=1`). Le premier passage avec quatre workers
a dépassé le délai d’une navigation pendant les compilations ; le passage séquentiel complet réussit.

### Consolidation SQL et protection SMS — 6 novembre 2026

Lors de la consolidation du 6 novembre, les sections `phone blocklist` et `notification events`
de `db/001_schema_postgres.sql` étaient partagées par l’installation neuve et les upgrades.
Les fichiers SQL séparés avaient été retirés.
Depuis le nettoyage du schéma initial du 19 novembre, les définitions de colonnes et contraintes sont
dans les `CREATE TABLE` ; les `ALTER TABLE` d’upgrade sont réservés à
`src/infra/migrations/table_upgrades.sql`. Les index de notification restent partagés dans la section
`notification indexes`. `outbox_postgres` compare le catalogue PostgreSQL neuf et celui obtenu après
deux migrations d’une structure ancienne, puis vérifie la conservation des identifiants et des données.
Le test du lecteur de sections exige les deux délimiteurs et exclut les instructions voisines.
Le lanceur PostgreSQL simule aussi des notifications d’essai retirées en présence de notifications
de continuation : il vérifie leur nettoyage avant la contrainte actuelle, la conservation des autres
notifications et leur échéance, puis l’idempotence d’un nouveau passage.

Le refactoring SMS garde la vérification du rôle actif sous verrou, les versions et l’audit dans la
transaction. Les règles de durée et de justification sont testées séparément, y compris la longueur
UTF-16 et le motif automatique du superadmin. La recherche par téléphone utilise une seule requête.

Validation locale : **387 tests Rust**, **25 tests PostgreSQL** (politiques SMS, notifications, OTP,
identité mobile, confidentialité web et outbox) et Clippy tous targets/features réussis.
Les migrations neuves, historiques et répétées passent. Les deux imports réels réussissent avec le
rôle limité ; aucun SMS n’est envoyé et les données temporaires sont supprimées.

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
cargo build --locked --bin db-migrate --bin phone-blocklist-sync
python3 scripts/verify-continuation-postgres.py \
  --native-migrator target/debug/db-migrate \
  --test phone_blocklist_postgres --test notifications_postgres \
  --test maintenance_outbox_postgres --test otp_delivery --test sms_safety_postgres \
  --test mobile_identity --test web_privacy_postgres
```
