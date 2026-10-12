# Performance SQL

[Index](README.md) · [Base de données](database.md) · [Supervision](observability.md#postgres-pool)

Ce guide décrit les optimisations présentes et une méthode de mesure reproductible. Il ne fournit pas
de capacité RPS garantie : la topologie, les données, le trafic, les verrous et les fournisseurs doivent être mesurés.

## Organisation des requêtes actuelles

| Parcours | Approche |
| --- | --- |
| Listes admin | Filtrer et borner la page avant les jointures/comptages coûteux ; clauses facultatives construites par QueryBuilder |
| Fiche utilisateur | Regrouper les projections dans une lecture, puis enregistrer l’audit requis |
| Métriques métier | Regrouper les agrégats dans un snapshot plutôt que multiplier les allers-retours |
| Files outbox | Agrégations filtrées communes pour les compteurs d’états |
| Feed | Préfiltrer par GiST/PostGIS avec un rayon progressif, calculer les distances exactes, vérifier l’éligibilité dans l’ordre distance/UUID et charger des lots adaptatifs ; conserver la vérification des swipes concurrents |
| Réponses de profil | Insertion et préparation de la modération en lot |
| OTP opérationnels | Instant SQL stable pour les agrégats d’expiration, compatible avec l’index |
| Claim outbox | Prédicat explicite d’état actif et index ordonné, avec `SKIP LOCKED`, ownership et retries conservés |

Le SQL appartient aux modules `pg.rs` ou `pg/` du domaine concerné ; [Architecture](architecture.md)
décrit les responsabilités. Une réduction du nombre de requêtes n’autorise pas à retirer l’audit,
une borne de pagination, un consentement, un verrou ou une règle d’idempotence.

### Feed dans une zone dense

L’index GiST sur les coordonnées existantes préfiltre la fenêtre avec `ST_DWithin` sur la sphère.
Le rayon commence à 100 mètres, ou après la distance du curseur, puis double jusqu’à obtenir le lot
ou atteindre la distance maximale du lecteur. Une marge conservatrice du préfiltre évite les omissions
liées aux différences de calcul ; la formule Haversine existante et le rayon exact décident de l’éligibilité.
La requête matérialise les candidats de cette fenêtre et leur distance exacte, puis les trie
selon `(distance_km, user_id)`. Le curseur et la distance maximale du lecteur sont appliqués à cet ordre.
Une jointure `LATERAL` vérifie pour chaque candidat le compte actif, les préférences réciproques,
les consentements de la bonne version et les exclusions. Son `LIMIT 1` garde cette vérification locale ;
le `LIMIT` de page compte uniquement les candidats éligibles. Le parcours continue après les rejets,
y compris lorsque les profils proches sont tous exclus.

Les profils détaillés, traits et réponses modérées sont chargés après la sélection de page.
Pour une page de 20, le service commence avec 21 candidats et augmente les lots si la revérification
des swipes concurrents en rejette. Le budget maximal de candidats parcourus est conservé et le curseur
avance au-delà des rejets. Les objets retenus sont déplacés sans clonage supplémentaire.
Les prérequis du feed sont renvoyés par la sélection PostgreSQL, y compris lorsque sa page est vide,
ce qui supprime la lecture de readiness séparée sur les requêtes valides. L’authentification continue
à contrôler compte, famille de session, révocation et onboarding dans sa requête existante.
En cas d’échec de la sélection fusionnée, une vérification des prérequis conserve les catégories
d’erreur antérieures ; elle ajoute une lecture uniquement sur ce chemin d’échec.
La validation d’un swipe garde sa requête ciblée et ses règles de replay des décisions.
La formule de distance et le curseur chiffré restent compatibles avec les pages existantes.

Sur PostgreSQL 18, le plan mesuré propage l’ordre du CTE géographique au parcours de page et arrête
les jointures détaillées après une page complète. Dans une zone peu éligible, le rayon peut atteindre
la fenêtre entière ; vérifier ce cas et le plan sur les données réellement déployées.
Voir les [mesures locales](load-testing-results.md) et les régressions de `discovery_postgres`.

## Index et mise à niveau

Les 19 index ciblés sont définis dans [query_indexes.rs](../src/infra/query_indexes.rs), avec leurs
équivalents dans le [schéma neuf](../db/001_schema_postgres.sql). Ils couvrent notamment claim outbox,
tri/filtre des comptes et références utilisées pour les jointures, audits ou suppressions.

Sur une base existante, lancer le migrateur de l’image à jour. Il construit les index manquants avec
`CONCURRENTLY`, répare ceux qui sont invalides et conserve ceux qui sont valides. Le `lock_timeout` de
cette connexion est de cinq secondes ; son `statement_timeout` est désactivé pour permettre une construction
longue. Prévoir le coût CPU, I/O et disque d’un build, même concurrent. Voir [Déploiement](container-deployment.md#mise-a-jour).

### Contrôles SQL en lecture

Exécuter ces requêtes via `psql` sur la base choisie. Les plans et statistiques partagés doivent être
expurgés d’identifiants personnels et de valeurs sensibles.

Index invalides :

```sql
SELECT ns.nspname AS schema_name, c.relname AS index_name, i.indisready, i.indisvalid
FROM pg_index i
JOIN pg_class c ON c.oid = i.indexrelid
JOIN pg_namespace ns ON ns.oid = c.relnamespace
WHERE ns.nspname = 'public' AND NOT i.indisvalid;
```

Constructions en cours :

```sql
SELECT pid, command, phase, lockers_total, lockers_done, blocks_total, blocks_done
FROM pg_stat_progress_create_index;
```

Attentes et transactions longues, sans contenu des requêtes :

```sql
SELECT pid, application_name, state, wait_event_type, wait_event,
       clock_timestamp() - xact_start AS transaction_age,
       clock_timestamp() - query_start AS query_age,
       pg_blocking_pids(pid) AS blocking_pids
FROM pg_stat_activity
WHERE datname = current_database() AND pid <> pg_backend_pid()
ORDER BY xact_start NULLS LAST;
```

## Mesurer une modification

1. Créer un jeu de données fictif représentatif dans une base de développement isolée.
2. Fixer commit/image, version PostgreSQL, réglages, matériel et cardinalités ; conserver les mêmes données
   et paramètres avant/après. Rafraîchir les statistiques avec `ANALYZE` après un chargement massif.
3. Examiner `EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON)` pour la requête réelle et plusieurs filtres/pages.
   Le coût estimé seul ne mesure pas la durée ; conserver temps, lignes, boucles et buffers.
4. Répéter en distinguant échauffement et état du cache. Mesurer aussi le parcours HTTP concurrent,
   son p95/p99, les erreurs, la CPU, les I/O et l’attente du pool.
5. Vérifier les résultats fonctionnels et la concurrence avec les intégrations avant de conclure au gain.

`EXPLAIN ANALYZE` **exécute** la requête. Pour une écriture, préférer une base jetable : un rollback
ne neutralise pas nécessairement tous les effets, notamment les séquences ou des effets externes.
Ne pas utiliser un générateur OTP/SMS pour un benchmark SQL.

Le [chargement dev](development.md#charger-des-donnees-fictives) exerce de vrais parcours métier avec des
comptes fictifs ; il ne constitue pas à lui seul un test de charge simultanée ni une prévision de RPS production.
Un sequential scan peut être adapté à un petit catalogue ; ne pas forcer systématiquement un index.

## Régressions et limites

Les suites PostgreSQL vérifient filtres, compteurs, pages, curseurs, données optionnelles, audit et claims.
Les tests du migrateur couvrent base neuve/peuplée, idempotence et réparation d’index invalide.
`query_performance_postgres` est une suite fonctionnelle de régression des requêtes ; elle n’impose pas de
seuil chronométrique dépendant du matériel. Voir [Tests](../tests/README.md).

Pour un incident réel, suivre [Latence HTTP](observability.md#http-latency) et
[Pool PostgreSQL](observability.md#postgres-pool). Augmenter le pool ou les timeouts sans identifier
attentes, plans et ressources peut aggraver la saturation.
