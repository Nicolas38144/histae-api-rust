# Base de données PostgreSQL

[Index](../README.md) · [Déploiement](../deployment/container-deployment.md) · [Performance SQL](sql-performance.md)

## Fichiers et responsabilité du migrateur

| Fichier | Usage |
| --- | --- |
| [000_bootstrap_postgres.sql](../../db/000_bootstrap_postgres.sql) | Rôles, extensions et permissions administratives |
| [001_schema_postgres.sql](../../db/001_schema_postgres.sql) | Schéma de référence, avec contraintes, index et triggers |
| [002_insert_postgres.sql](../../db/002_insert_postgres.sql) | Catalogue initial et données de démonstration sur demande explicite |
| [003_casework_notifications.sql](../../db/003_casework_notifications.sql) | Mise à niveau des notifications de support et de confidentialité sur une base existante |
| [migrations.rs](../../src/infra/migrations.rs) | Initialisation ou contrôle du schéma PostgreSQL |
| [query_indexes.rs](../../src/infra/query_indexes.rs) | Réparation des index manquants ou invalides |
| [drop_postgres.sql](../../db/drop_postgres.sql) | Suppression manuelle du schéma de test |

### Identifiants des événements

Les messages, notifications et événements outbox utilisent des UUID v7 par défaut. Les références de
livraison push partagent l’identifiant de leur événement outbox. La pagination et la rétention utilisent
les horodatages métier. Les comptes, matchs, sessions et challenges gardent des identifiants v4,
et les identifiants du catalogue sont déterministes.

### Base vide

Sur un volume neuf, l’entrypoint PostgreSQL applique les rôles, extensions et permissions.
Le service `migrate` exécute ensuite le schéma et le catalogue dans une transaction avec le rôle
`histae_migrate` et `histae.seed_fake_users=off`. Le schéma initial définit les identifiants de challenge,
les compteurs et les index qui séparent les parcours OTP.

### Base existante

`db-migrate` vérifie le schéma de référence. Sur une base compatible, il installe si nécessaire la contrainte
et les fonctions des notifications de support et de confidentialité dans une transaction. Il répare ensuite
les index manquants ou invalides avec `CONCURRENTLY`. Un verrou consultatif sérialise ses exécutions.
Un redéploiement conserve les données et le catalogue de la base.

Pour déployer cette version sur une base de test d’un ancien schéma, repartir d’une base vide.
Pour une base dont les données doivent être conservées, préparer une migration dédiée à partir d’une sauvegarde.

Les tables de protection SMS conservent les décisions et la comptabilité d’envoi. Lors d’une restauration,
conserver les tables de blocage téléphonique avec leur `PHONE_HASH_KEY`. Voir les [rôles](postgres-roles.md)
et la [politique de blocage](../features/phone-blocklist.md).

### PostGIS et mise à jour d’une installation

Les piles dev et production construisent [l’image PostgreSQL](../../docker/postgres/Dockerfile) à partir
de PostgreSQL **18.7 / Debian bookworm**, avec PostGIS **3.7.0**. Le volume, `PGDATA`, la version majeure
et la base Debian sont conservés. `HISTAE_POSTGRES_IMAGE` permet de fournir cette image déjà construite.

La livraison du [12 novembre 2026](https://www.postgresql.org/docs/release/18.7/) corrige des
vulnérabilités qui nécessitent aussi des extensions C reconstruites. L’image épingle le paquet
PostGIS `3.7.0+dfsg-1.pgdg12+2` et vérifie la référence au nouveau symbole PostgreSQL
`DatumGetHeapTupleHeader` pendant la construction. Ne pas réutiliser la bibliothèque de l’ancienne image.
Le bootstrap administrateur met aussi à jour les définitions SQL de `postgis`, `pg_trgm`, `pgcrypto`,
`uuid-ossp` et `pg_stat_statements`. Le migrateur applicatif reste limité à ses objets.

Sur un volume existant, après sauvegarde et avant la migration applicative :

1. Mettre `HISTAE_POSTGRES_IMAGE=histae-postgres:18.7-postgis3.7.0` dans le `.env` du serveur.
2. Construire la nouvelle image et redémarrer PostgreSQL avec les fichiers Compose de la pile choisie.
3. Exécuter la mise à jour des extensions avec le rôle administrateur. Le reset standard la fait via
   le bootstrap d’accès ; pour une mise à jour Compose manuelle, utiliser les commandes ci-dessous.

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.production.yaml \
  exec -T postgres sh -ec 'psql -X -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB" \
    -c "ALTER EXTENSION pgcrypto UPDATE; ALTER EXTENSION \"uuid-ossp\" UPDATE; ALTER EXTENSION pg_trgm UPDATE; ALTER EXTENSION postgis UPDATE; ALTER EXTENSION pg_stat_statements UPDATE;"'

sudo docker compose --env-file .env -f compose.yaml -f compose.production.yaml \
  exec -T postgres sh -ec 'psql -X -U "$POSTGRES_USER" -d "$POSTGRES_DB" \
    -c "SELECT version(), postgis_full_version();"'
```

Employer `compose.dev.yaml` en développement. PostgreSQL 18.7 ouvre les volumes 18.x sans
dump/restauration. Les notes de version signalent aussi des corruptions possibles d’index GIN :
si la base a été concernée, reconstruire `idx_user_profile_firstname_trgm` avec
`REINDEX INDEX CONCURRENTLY public.idx_user_profile_firstname_trgm` après mise à jour, hors transaction.
Les résultats de charge historiques restent associés aux versions utilisées lors de leurs mesures.

Le bootstrap administrateur installe PostGIS. Le schéma utilise un index GiST sur les centres des zones.
Sur une base hébergée ailleurs, faire installer PostGIS avant l’initialisation.

Après une sauvegarde, mettre à jour une pile locale avec :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d --build --wait
```

Ajouter l’override proxy s’il est utilisé. En production, employer les fichiers d’environnement et
Compose de production. La mise à jour de PostgreSQL implique un bref redémarrage ; le migrateur doit
terminer avant le démarrage de la nouvelle API. Une sauvegarde contenant PostGIS doit être restaurée
sur un serveur disposant de cette extension. Le schéma actuel comporte 69 tables publiques ; le test de restauration doit retrouver ce nombre.

`processed_count=1` indique une initialisation. `processed_count=0` indique le contrôle d’une base déjà initialisée.

## Exécuter le migrateur

Sur l’hôte, avec la configuration locale et les dépendances prêtes :

```bash
cargo run --locked --bin db-migrate
```

Dans Docker dev déjà démarré :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml run --rm --no-deps migrate
```

Ajouter `-f compose.proxy.yaml` si utilisé. En production, sélectionner `.env` et
`compose.production.yaml`. Le conteneur utilise le SQL **embarqué dans son image** : reconstruire ou
charger l’image de livraison avant la migration. Voir la [procédure de mise à jour](../deployment/container-deployment.md#mise-a-jour).

## Connexions et droits

Le pool SQLx est borné par `POSTGRES_POOL_MAX`, avec des timeouts de connexion, requête et transaction inactive.
Dans Compose, le service API reçoit `HISTAE_API_POSTGRES_POOL_MAX` (30 par défaut) comme valeur de
`POSTGRES_POOL_MAX`. Cette variable permet de régler le pool API indépendamment des autres processus.
L’API utilise aussi un pool de sessions de verrous d’activité pouvant aller jusqu’à quatre connexions.
Additionner les besoins de tous les processus avant d’ajuster `max_connections` ou de répliquer les workers.
Les verrous advisory exigent des sessions PostgreSQL stables : un pooler en mode transaction ne peut pas
remplacer directement cette connexion de session.

La configuration production utilise `verify-full` et une CA PEM. Les piles dev et production séparent
`histae_admin`, `histae_migrate` et `histae_app`. Le bootstrap installe les extensions comme administrateur ;
le migrateur crée les objets applicatifs et leurs privilèges futurs. Voir [Rôles PostgreSQL](postgres-roles.md)
pour les mots de passe de rôle dans `.env`, l'installation neuve et la conversion d'une base dev existante.
Changer les variables d'initialisation sur un volume existant ne change pas ses rôles.

## Statistiques SQL

Les piles Docker dev et production préchargent `pg_stat_statements` avec `compute_query_id=auto`.
Le script d’initialisation installe l’extension dans les nouvelles bases. Sur un volume existant,
recréer PostgreSQL puis appliquer explicitement le script : l’entrypoint ne rejoue pas l’initialisation.
La [procédure et les requêtes de profilage](sql-performance.md#pg-stat-statements)
décrivent cette mise à jour. Les statistiques sont propres au serveur et à la base ; elles ne remplacent
pas les percentiles HTTP ni les observations de CPU, verrous et I/O sous charge.

<a id="sauvegarde-et-restauration"></a>

## Sauvegarde et restauration

Un volume protège contre la recréation du conteneur, pas contre la perte du disque ou sa suppression.
Ne pas copier arbitrairement les fichiers d’un PostgreSQL actif. Utiliser `pg_dump` ou une sauvegarde physique
PostgreSQL adaptée à la politique de reprise.

### Vérification locale déjà fournie

```bash
bash scripts/verify-dev-backup-restore.sh
```

Le script accepte uniquement le conteneur `histae-rust-dev-postgres-1` et la base `histae-dev` par défaut.
Il effectue un dump au format custom, restaure dans une base temporaire, compare les colonnes du schéma
public et le nombre de tables, puis supprime base temporaire et archive. Le rôle doit pouvoir créer une base.
Il choisit Docker direct ou `sudo` selon l’accès disponible.

Le message `Development backup/restore verification passed (...)` confirme cette restauration technique.
Le schéma actuel comporte 68 tables applicatives et la table spatiale de PostGIS, soit 69 tables publiques attendues.
Le script ne conserve pas de sauvegarde, ne compare pas toutes les données et ne teste pas la reprise du S3,
des certificats ou des fournisseurs.

### Sauvegarde conservée

Sélectionner la pile ; exemple production depuis la racine du dépôt :

```bash
COMPOSE=(sudo docker compose --env-file .env -f compose.yaml -f compose.production.yaml)
umask 077
mkdir -p "$HOME/histae-backups"
backup_file="$HOME/histae-backups/postgres-$(date -u +%Y%m%dT%H%M%SZ).dump"
"${COMPOSE[@]}" exec -T postgres sh -c \
  'exec pg_dump -U "$POSTGRES_USER" -d "$POSTGRES_DB" -Fc' > "$backup_file"
```

Vérifier le code de sortie avant de considérer le fichier comme une sauvegarde. Conserver une copie chiffrée
hors machine et restaurer périodiquement dans une base isolée avec `pg_restore --exit-on-error`.
Un dump d’une base n’inclut pas les rôles globaux, les secrets Docker, les certificats ni les objets S3 :
leur sauvegarde doit être coordonnée. Si la perte admissible exige une reprise à un instant précis,
prévoir l’archivage WAL/PITR. Tester une montée de version majeure avant de changer l’image PostgreSQL.

## Supprimer les données

Pour les comptes fictifs, préférer [la purge dev](../development/development.md#supprimer-les-comptes-de-test), qui conserve
le superadmin et possède des gardes d’environnement.

`db/drop_postgres.sql` exécute `DROP SCHEMA public CASCADE`, recrée `public` et ses permissions de base.
Il supprime les objets de ce schéma et leurs dépendances ; il ne supprime ni la base entière, ni les rôles,
ni les objets S3. Il ne possède pas les gardes de la purge dev : l’opérateur doit sélectionner la cible et
posséder une sauvegarde vérifiée.

Exemple **destructif**, exclusivement sur la base dev choisie, après arrêt des processus applicatifs :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml stop api outbox-worker
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml exec -T postgres sh -c \
  'exec psql -X -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB"' < db/drop_postgres.sql
```

Après suppression, réinstaller avec le migrateur et redémarrer les services selon le guide Docker.
Avec les rôles séparés, rétablir d'abord les extensions, le propriétaire du schéma et ses permissions
comme administrateur (les rôles eux-mêmes sont conservés par le `DROP SCHEMA`) :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml exec -T postgres \
  psql -X -v ON_ERROR_STOP=1 -v bootstrap_mode=access -U histae_admin -d histae-dev \
  < db/000_bootstrap_postgres.sql
```

Puis lancer le service `migrate` avant l'API et les workers. Adapter les manifests et la base pour une
production sélectionnée. Ce bootstrap d'accès ne recrée aucun rôle et ne change aucun mot de passe.
Ne pas relancer un ordonnanceur de maintenance avant la fin de la réinstallation.

## Wallet et continuation

Le [guide des crédits](../features/continuation-and-rewards.md) décrit les réservations et les débits. `continuation_wallet` garde le solde et sa semaine UTC. `continuation_usage` distingue les continuations confirmées des crédits gratuits consommés.

Les proofs/intents et compteurs hebdomadaires sont purgés au-delà de 90 jours lors de la maintenance bornée ; les réservations périmées sont libérées et purgées. Un stock publicitaire acquis reste conservé pendant la vie du compte. Export et effacement couvrent ces données, avec libération des réservations du partenaire sur les matchs clôturés.

## Langues et traductions

Le schéma comporte `preferred_language` sur le compte, `language` sur les appareils et tentatives OTP,
et `document_language` sur les décisions de consentement. Quatre tables de traduction sont liées par
clés étrangères en cascade aux traits, questions, plans et fonctionnalités. Le catalogue installe les traductions initiales.
L’effacement réinitialise la langue du compte ; l’export inclut les préférences et la langue documentaire.
Voir [Internationalisation](../api/internationalization.md).

## Projections de paiement et sessions du site

`user_subscription` conserve uniquement les attributions internes non commerciales. `billing_store_account` / `_subscription`
portent les liaisons et projections natives ; `_notification` est l’inbox dédupliquée, `_finance` les
lignes financières importées. `effective_subscription_at` et `effective_user_subscription` réunissent les
sources sans permettre à une projection de supprimer les droits valides d’une autre.
`web_account_challenge` et `web_account_session` authentifient les comptes existants pour les démarches du site.
Voir [rétention et effacement](../features/store-billing.md#donnees-personnelles).

### Horloge des conversations confirmées

Pour `match_init.status='confirmed'`, `expires_at` est l’échéance des 30 jours d’inactivité.
Le trigger `trg_match_inactivity` la fixe à la confirmation et la renouvelle lors d’un nouveau
`last_message_at`. Une lecture et un rejeu idempotent ne modifient pas cette horloge. L’index partiel
`idx_match_init_confirmed_expiry` sert aux avertissements à J23 et aux clôtures à J30.
La lecture calcule l’archivage à J10.

### Identité de revue des stores

`user_account.is_review_account` vaut `false` par défaut. Le parcours mobile de revue crée son compte avec le marqueur `true`. La contrainte `user_account_review_role_check` impose le rôle
`user` pour cette identité ; la route de promotion refuse aussi l’opération. Création et ouverture de
session sont transactionnelles, avec refus des bannissements, suppressions et tombstones. Les données
de profil et les consentements sont ensuite gérés par les routes normales, sans acceptation automatique.
La fonction `effective_subscription_at` ajoute un droit Premium offert sans expiration pour les comptes
de revue actifs de rôle `user`. La CTE `complimentary_accounts` isole l’éligibilité du compte ; les candidats
internes, stores et de revue partagent ensuite une sélection unique avec un ordre de priorité explicite.
Ce droit couvre aussi les comptes de revue déjà inscrits, sans insérer de
projection de store ni de facture et sans écraser l’historique fournisseur. Il disparaît du calcul
si le marqueur est retiré, le compte banni ou supprimé. Voir les champs publics dans le contrat HTTP.
Voir [l’accès Google Play](../features/google-play-review.md).

### Premium offert par le superadmin

`complimentary_premium_grant` conserve une seule offre par utilisateur, avec date et acteur de
l’attribution. L’offre n’a pas de date de fin ; son retrait supprime cette ligne et conserve
l’historique dans `data_access_log`. Aucun abonnement fournisseur ou identifiant de paiement
n’est modifié. La projection `effective_subscription_at` utilise ce droit pour les comptes
non bannis et non supprimés ; un abonnement fournisseur actif conserve sa priorité.

La mutation vérifie le superadmin et l’état de la cible dans une transaction et prend les
mêmes verrous ordonnés de portefeuille que la confirmation d’un match. Les triggers couvrent
aussi les écritures SQL de l’offre. La suppression physique et l’anonymisation du bénéficiaire
retirent son offre. Les actions d’audit sont `admin_grant_premium` et `admin_revoke_premium`.
