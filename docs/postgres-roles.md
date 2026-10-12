# Rôles PostgreSQL

[Index](README.md) · [Configuration](configuration.md) · [Déploiement](container-deployment.md)

Les piles Docker dev et production utilisent la même séparation. Les rôles PostgreSQL sont indépendants
des rôles métier `user`, `admin` et `superadmin` de Histae.

| Rôle PostgreSQL | Services | Permissions |
| --- | --- | --- |
| `histae_admin` | PostgreSQL et administration manuelle | Superuser ; initialise les rôles et les extensions |
| `histae_migrate` | `migrate` | Propriétaire du schéma `public` et des objets applicatifs ; aucun privilège superuser, création de base ou de rôle |
| `histae_app` | API, outbox, maintenance et `storage-init` en production | Connexion, usage du schéma, DML applicatif, séquences et fonctions nécessaires ; aucun DDL ou rôle privilégié |

Le migrateur crée les tables et fonctions ; leurs privilèges par défaut donnent les accès nécessaires à
l'application, y compris pour les futurs objets. Les objets des extensions restent possédés par
l'administrateur. `spatial_ref_sys` est accessible en lecture seulement. L'application n'a aucun droit
`CREATE`, `TEMPORARY`, `TRUNCATE`, `TRIGGER` ou `REFERENCES`, ni appartenance aux rôles privilégiés.
Les fonctions applicatives s'exécutent avec les droits de l'appelant, sans `SECURITY DEFINER`.

Cette séparation limite les capacités SQL du processus applicatif. Les autorisations entre utilisateurs
restent vérifiées par l'API ; `histae_app` dispose des données nécessaires à tous ses parcours métier.

## Mots de passe dans le fichier d'environnement

Les deux modes utilisent le même fichier `.env`, avec trois mots de passe indépendants :

```dotenv
POSTGRES_ADMIN_PASSWORD=<64-caracteres-hexadecimaux-aleatoires>
POSTGRES_MIGRATE_PASSWORD=<64-autres-caracteres-hexadecimaux-aleatoires>
POSTGRES_APP_PASSWORD=<64-autres-caracteres-hexadecimaux-aleatoires>
```

Chaque valeur doit être différente, générée avec 32 octets aléatoires. Pour préparer le fichier sans
afficher les secrets ni remplacer ceux déjà configurés :

```bash
python3 scripts/prepare-postgres-roles-env.py .env
```

Le fichier doit déjà exister. Le script conserve les autres réglages, remplace les placeholders de mots
de passe PostgreSQL, refuse les doublons ou valeurs invalides et écrit le fichier en `0600`.
Il configure aussi l'accès natif avec `POSTGRES_USER=histae_app` et
`POSTGRES_PASSWORD=${POSTGRES_APP_PASSWORD}`. Dans Compose, `environment` impose le rôle approprié à
chaque service et vide les trois variables sources dans les conteneurs applicatifs. L'API et les workers
ne reçoivent que le mot de passe applicatif ; le migrateur ne reçoit que son mot de passe.
PostgreSQL reçoit les trois pour initialiser les rôles. Aucun fichier `.env` n'est monté dans ces conteneurs.

Le fichier réel est ignoré par Git et exclu des images. Il reste sensible sur l'hôte ; le protéger et
ne pas afficher la sortie complète de `docker compose config`. L'exécution native depuis le dépôt charge
le `.env` complet : l'isolation des variables décrite ici concerne les conteneurs Compose.

## Installation neuve

L'entrypoint PostgreSQL exécute [init-roles.sh](../docker/postgres/init-roles.sh) uniquement sur un volume
vide. Il applique [000_bootstrap_postgres.sql](../db/000_bootstrap_postgres.sql) comme administrateur.
Ce fichier regroupe la création des rôles, les extensions, les permissions et la conversion dev. Ses
modes `install`, `access` et `convert-dev` utilisent les mêmes règles d'accès. Le mode `access` rétablit
les permissions après un `DROP SCHEMA`, sans recréer les rôles ni changer leurs mots de passe.

Le schéma [001_schema_postgres.sql](../db/001_schema_postgres.sql) est ensuite exécuté par le migrateur
limité. Fusionner ces deux étapes imposerait au migrateur les droits d'administration du cluster ;
les commandes psql du bootstrap ne sont pas compatibles avec l'exécution SQLx du schéma.
Les mots de passe sont lus par psql via son environnement, jamais ses arguments ; les diagnostics SQL
du bootstrap sont masqués pour éviter d'exposer un mot de passe interpolé.
Le service `migrate` installe ensuite le schéma et le catalogue avec `histae_migrate`, avant le démarrage de
l'API et de l'outbox. Aucun compte fictif n'est créé.

### Développement

Créer `.env` selon [Configuration](configuration.md) s'il est absent, puis :

```bash
python3 scripts/prepare-postgres-roles-env.py .env
```

Remplacer les autres placeholders (JWT, clés téléphone et modération), puis :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml config --quiet
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d --build --wait
bash scripts/smoke-api.sh
```

Si le volume existe déjà avec l'ancien compte unique,
suivre la conversion ci-dessous avant de recréer PostgreSQL.

### Production

```bash
python3 scripts/prepare-postgres-roles-env.py .env
```

Utiliser `.env` sur le serveur de production et compléter les autres secrets, les versions/URLs juridiques réellement publiées, l'étiquette de livraison,
les fournisseurs, certificats et réseaux selon [Déploiement](container-deployment.md#production).
Depuis une installation neuve :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.production.yaml config --quiet
sudo docker compose --env-file .env -f compose.yaml -f compose.production.yaml up -d --build --wait
sudo docker compose --env-file .env -f compose.yaml -f compose.production.yaml ps --all
```

Le bootstrap n'est pas une migration automatique d'une ancienne production. Changer les variables d'un
volume existant ne crée pas les rôles et ne change pas leurs mots de passe.

<a id="convertir-la-base-dev-existante-sans-supprimer-ses-donnees"></a>

## Convertir la base dev existante sans supprimer ses données

La conversion est réservée au conteneur PostgreSQL du projet `histae-rust-dev` et à la base `histae-dev`.
Elle crée les nouveaux comptes, transfère les objets applicatifs, applique les droits existants et futurs,
puis désactive la connexion de l'ancien administrateur. Les extensions gardent leur propriétaire initial.
Elle refuse un rôle déjà configuré ou un autre client encore connecté et s'exécute dans une transaction.

1. Faire une sauvegarde conservée et vérifiée selon [Base de données](database.md#sauvegarde-et-restauration).
2. Préparer les trois mots de passe dans le `.env` existant.
3. Arrêter les clients applicatifs, convertir, puis recréer les conteneurs.

```bash
python3 scripts/prepare-postgres-roles-env.py .env
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml stop api outbox-worker migrate
sudo python3 scripts/configure-dev-postgres-roles.py --env-file .env
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d --force-recreate --wait
bash scripts/smoke-api.sh
```

Ajouter `-f compose.proxy.yaml` aux commandes Compose si la pile utilise le proxy. Arrêter également les
binaires natifs et l'ordonnanceur de maintenance pendant la conversion. Le script utilise l'ancien rôle
administrateur déclaré dans le conteneur encore en cours d'exécution : **ne pas recréer PostgreSQL avant
la conversion**. Aucun utilisateur, message, match ou objet S3 n'est supprimé.

## Exécution native et tests

L'API native utilise `histae_app` grâce aux valeurs préparées dans `.env`. Pour migrer avec le bon compte,
utiliser le service dédié, même lorsque l'API est exécutée sur l'hôte :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml run --rm --no-deps migrate
```

Les fixtures des tests d'intégration existants créent des schémas ou bases temporaires : elles nécessitent
des identifiants administrateur de **développement**, distincts de ceux de l'API. Ne pas élargir les droits
de `histae_app` pour les faire passer. Le test dédié ci-dessous vérifie directement le rôle applicatif.

## Vérifier la séparation

Avec les images PostgreSQL et API compilées localement :

```bash
sudo bash scripts/verify-postgres-roles.sh
```

`HISTAE_POSTGRES_IMAGE` et `HISTAE_API_IMAGE` permettent de sélectionner les images de la livraison.
Le script crée un PostgreSQL sans réseau externe et sans volume persistant, utilise le vrai migrateur
deux fois, puis vérifie les droits présents et futurs, les refus DDL/changement de rôle, PostGIS, les
triggers et l'effacement. Il vérifie aussi un reset de schéma suivi du rétablissement des permissions et
d'une nouvelle migration. Il supprime son conteneur et ses fichiers temporaires, même après échec.
Il ne teste pas TLS ni l'ensemble des routes HTTP ou fournisseurs ; ces validations restent requises
sur la livraison réelle. Il n'accède pas à la base dev en service.

### Contrôle sur la pile déployée

Dans Compose, une API doit annoncer `histae_app` et le migrateur `histae_migrate` sans afficher de mot de passe :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml exec -T api printenv POSTGRES_USER
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml exec -T postgres \
  psql -X -U histae_admin -d histae-dev -c \
  "SELECT rolname, rolsuper, rolcreatedb, rolcreaterole FROM pg_roles WHERE rolname LIKE 'histae_%'"
```

En production, sélectionner ses manifests/env et remplacer `histae-dev` par la base configurée.
La rotation des mots de passe nécessite des commandes SQL coordonnées puis la recréation des clients ;
le préparateur de `.env` conserve les mots de passe existants et ne réalise pas une rotation SQL.

## Validation locale du 13 octobre 2026

- PostgreSQL 18.6 / PostGIS 3.6.4 : bootstrap neuf, migrateur réel limité, second passage et reset de schéma réussis.
- Compte applicatif : DML, séquences, consentements, PostGIS, swipes, match, message et effacement vérifiés ;
  les neuf tentatives de DDL/changement de rôle/écriture fichier serveur sont refusées.
- Conversion d'une ancienne base fictive isolée : client connecté refusé, rollback sans création de rôles,
  données conservées après conversion, permissions existantes/futures vérifiées, nouvelle conversion refusée.
- Modèles Compose dev/production : absence des mots de passe privilégiés dans les environnements applicatifs ;
  préparateur idempotent, refus des valeurs invalides/doublons, fichiers `0600` et liens documentaires vérifiés.

Ces vérifications utilisent des instances jetables. Elles ne convertissent pas la base dev en service et
ne remplacent pas les validations TLS, HTTP et fournisseurs de production.

Références : [permissions PostgreSQL](https://www.postgresql.org/docs/18/ddl-priv.html),
[privilèges futurs](https://www.postgresql.org/docs/18/sql-alterdefaultprivileges.html).
