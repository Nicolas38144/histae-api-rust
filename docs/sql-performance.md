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
| Feed | Borner les candidats géographiques avant les consentements et préparer les exclusions ; conserver la vérification de concurrence du swipe |
| Réponses de profil | Insertion et préparation de la modération en lot |
| OTP opérationnels | Instant SQL stable pour les agrégats d’expiration, compatible avec l’index |
| Claim outbox | Prédicat explicite d’état actif et index ordonné, avec `SKIP LOCKED`, ownership et retries conservés |

Le SQL appartient aux modules `pg.rs` ou `pg/` du domaine concerné ; [Architecture](architecture.md)
décrit les responsabilités. Une réduction du nombre de requêtes n’autorise pas à retirer l’audit,
une borne de pagination, un consentement, un verrou ou une règle d’idempotence.

## Index et mise à niveau

Les 18 index ciblés sont définis dans [query_indexes.rs](../src/infra/query_indexes.rs), avec leurs
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
