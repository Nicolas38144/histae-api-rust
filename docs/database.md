# Base de données PostgreSQL

[Index](README.md) · [Déploiement](container-deployment.md) · [Performance SQL](sql-performance.md)

## Fichiers et responsabilité du migrateur

| Fichier | Usage |
| --- | --- |
| [001_schema_postgres.sql](../db/001_schema_postgres.sql) | Schéma consolidé : 52 tables applicatives, PostGIS et sa table de référence spatiale, contraintes, fonctions, triggers et index |
| [002_insert_postgres.sql](../db/002_insert_postgres.sql) | Catalogue initial ; fixtures fictives conditionnées par `histae.seed_fake_users` |
| [drop_postgres.sql](../db/drop_postgres.sql) | Suppression manuelle et destructive du schéma `public` |
| [migrations.rs](../src/infra/migrations.rs) | Installation neuve, vérification et mise à niveau d’une base existante |
| [query_indexes.rs](../src/infra/query_indexes.rs) | Installation concurrente des 19 index de performance, dont le GiST géographique, sur une base existante |
| [000_bootstrap_postgres.sql](../db/000_bootstrap_postgres.sql) | Bootstrap administrateur : rôles, extensions, permissions, conversion dev et rétablissement après reset |

Il n’existe plus de ledger `schema_migrations`. Le migrateur retire cette ancienne table si elle est présente.

Mode pause (19 octobre 2026) : `user_account.discovery_paused boolean NOT NULL DEFAULT false` est créé par le schéma neuf et ajouté de manière idempotente aux bases existantes par `db-migrate`. La vérification de démarrage exige cette colonne booléenne non nullable ; exécuter le migrateur avant la nouvelle API. Les migrations sont vérifiées sur une instance PostgreSQL jetable ; aucune modification de production n’est effectuée par ces tests. Le réglage conserve les matchs, les données et l’abonnement, figure dans l’export propriétaire et est remis à `false` lors de l’anonymisation du compte. Les contrôles de concurrence du swipe et de la création du match utilisent les verrous partagés sur les comptes, compatibles avec la mise à jour du réglage.

### Base vide

Le migrateur applique schéma et catalogue dans une transaction, avec `histae.seed_fake_users=off`.
Il ne crée pas de comptes de démonstration. Sur un volume Docker neuf, l’entrypoint PostgreSQL prépare
les rôles, extensions et permissions via `000_bootstrap_postgres.sql` ; le service `migrate` applique
ensuite le schéma applicatif et le catalogue avec `histae_migrate`.

### Base existante

Le migrateur vérifie les objets attendus, ajoute les évolutions prises en charge et retire le ledger obsolète.
Il met aussi à jour l'ancien tarif annuel Premium de 30 EUR à 48 EUR dans le catalogue, sans modifier
les autres tarifs personnalisés ni les abonnements Stripe existants ; voir [Configuration](configuration.md#tarifs-du-catalogue-et-stripe).

Le catalogue n'a plus de colonne `subscription_plan.trial_days` ni de contrainte associée dans le
schéma initial ; le seed et les requêtes sont alignés. Le migrateur retire aussi cette colonne si
elle existe encore. Les nouvelles souscriptions n'ont aucune période d'essai. Le champ `trial_days` est aussi retiré du
catalogue HTTP et des modèles clients. Les dates d'essais historiques des abonnements ne sont pas modifiées.

Les notifications de fin d’essai sont retirées : le migrateur supprime leurs tâches outbox et
leurs notifications, puis la colonne dédiée `notification.billing_trial_ends_at`. Les contraintes
sont alignées sur le schéma neuf ; cette mise à niveau est idempotente et conserve les autres
notifications et les abonnements.
Les index de performance sont ensuite construits avec `CONCURRENTLY`, hors de la transaction de schéma.
Un verrou advisory sérialise les migrateurs. Un index valide est conservé ; un index invalide laissé par une
interruption est supprimé puis reconstruit au prochain lancement.

### PostGIS et mise à jour d’une installation

Les piles dev et production construisent [l’image PostgreSQL](../docker/postgres/Dockerfile) à partir
de PostgreSQL **18.6 / Debian bookworm**, avec PostGIS **3.6.4**. Le volume, `PGDATA`, la version majeure
et la base Debian sont conservés. `HISTAE_POSTGRES_IMAGE` permet de fournir cette image déjà construite.

Sur une base existante, le migrateur active `postgis` dans `public`, puis construit l’index GiST avec
`CONCURRENTLY`. Il n’ajoute aucune colonne aux présences : l’index porte sur l’expression géographique
des coordonnées existantes. Les exports et DTO conservent leurs champs. L’activation de l’extension
nécessite un rôle PostgreSQL disposant des droits adaptés ; sur une base gérée extérieurement, faire
installer PostGIS par son administrateur avant de lancer le migrateur.

Après une sauvegarde, mettre à jour une pile locale avec :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d --build --wait
```

Ajouter l’override proxy s’il est utilisé. En production, employer les fichiers d’environnement et
Compose de production. La mise à jour de PostgreSQL implique un bref redémarrage ; le migrateur doit
terminer avant le démarrage de la nouvelle API. Une sauvegarde contenant PostGIS doit être restaurée
sur un serveur disposant de cette extension. Le test local de restauration compte désormais 53 tables publiques.

Le migrateur ne transforme pas automatiquement une base arbitraire. Modifier le fichier de schéma ne suffit
pas à modifier une base existante : toute évolution doit aussi avoir son chemin de mise à niveau et ses tests.
Il peut afficher `processed_count=0` sur une base existante tout en ayant installé des index ; cette valeur
compte l’installation initiale, pas tous les objets modifiés.

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
charger l’image de livraison avant la migration. Voir la [procédure de mise à jour](container-deployment.md#mise-a-jour).

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
pour les trois mots de passe dans `.env`, l'installation neuve et la conversion d'une base dev existante.
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

`Development backup/restore verification passed (53 public tables)` confirme cette restauration technique
sur le schéma actuel : 52 tables applicatives et la table spatiale de PostGIS.
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

Pour les comptes fictifs, préférer [la purge dev](development.md#supprimer-les-comptes-de-test), qui conserve
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

Le [guide des crédits](continuation-and-rewards.md) définit les invariants et l’upgrade. `continuation_wallet` conserve le stock publicitaire ; `continuation_reservation` lie chaque accord à sa source et sa semaine ; `advertising_reward_intent` conserve une intention opaque et une empreinte de transaction unique. `continuation_usage` distingue le total confirmé et les crédits gratuits consommés. Le schéma et les upgrades réutilisent les sections marquées du SQL consolidé, y compris les guards et la fonction d’anonymisation. Les nouvelles tables reçoivent les privilèges applicatifs par les default privileges du rôle migrateur. Aucune table de suivi des migrations n’est réintroduite.

Les proofs/intents et compteurs hebdomadaires sont purgés au-delà de 90 jours lors de la maintenance bornée ; les réservations périmées sont libérées et purgées. Un stock publicitaire acquis reste conservé pendant la vie du compte. Export et effacement couvrent ces données, avec libération des réservations du partenaire sur les matchs clôturés.

## Langues et traductions

Le schéma comporte `preferred_language` sur le compte, `language` sur les appareils et tentatives OTP,
et `document_language` sur les décisions de consentement. Quatre tables de traduction sont liées par
clés étrangères en cascade aux traits, questions, plans et fonctionnalités. Le migrateur les ajoute aux
installations existantes et insère les traductions initiales sans remplacer les textes déjà présents.
L’effacement réinitialise la langue du compte ; l’export inclut les préférences et la langue documentaire.
Voir [Internationalisation](internationalization.md).
