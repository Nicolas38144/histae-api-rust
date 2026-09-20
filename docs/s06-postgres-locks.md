# S06 — verrous de session PostgreSQL

## Comportement NestJS conservé

`AccountActivityService` protège les opérations qui traversent PostgreSQL et un service externe sans garder une transaction ouverte. Les identifiants UUID sont dédupliqués et triés avant toute acquisition. Chaque compte reçoit la même clé que NestJS, calculée par PostgreSQL avec `hashtextextended(uuid_canonique, 13092026)`.

Les opérations normales prennent un verrou partagé puis exigent un compte non supprimé et non banni. Les maintenances `run_existing` tolèrent un compte banni, mais jamais un compte supprimé. Une contention partagée produit `409 account_unavailable`. L’effacement tente un verrou exclusif et reçoit `NotAcquired` sans erreur si une opération est encore active.

Le pool `histae-account-activity` est distinct du pool métier et limité à quatre connexions. Une sonde vérifie la session pendant le travail afin que `ActivityLease::assert_held` refuse tout effet externe après une perte de connexion avec `503 account_activity_unavailable`.

## Cycle de vie des connexions

Une connexion est considérée impropre au pool dès son acquisition. Elle ne redevient réutilisable qu’après le succès de `pg_advisory_unlock_all()` ou, pour un leader, de `pg_advisory_unlock(key)` avec un résultat vrai. Une erreur, une annulation Tokio, un abandon de lease ou une perte réseau ferme la connexion ; PostgreSQL libère alors les verrous avec la session.

Cette règle couvre aussi l’annulation pendant l’acquisition de plusieurs UUID : une future abandonnée ne peut jamais remettre au pool une session partiellement verrouillée.

La sonde d’activité s’exécute toutes les 100 ms sur la connexion dédiée. Le callback ne reçoit pas cette connexion et ne peut donc pas lancer une requête concurrente dessus. Il doit appeler `assert_held()` juste avant chaque effet externe irréversible, comme le callback NestJS.

## Leader de maintenance

`SessionLeaderLease` conserve la connexion principale et le verrou `37142581` pendant tous les lots de maintenance des matchs. Le code métier peut commencer et committer plusieurs transactions avec `connection_mut()` sans relâcher le verrou de session. Un autre worker reçoit `None` tant que le leader le détient.

Ces connexions exigent un pooling PostgreSQL de session. Un proxy en mode transaction casserait le contrat et n’est pas compatible avec ce module.

## Tests

Les tests unitaires vérifient l’ordre canonique des UUID, les codes publics et la clé du leader. Le test PostgreSQL réel couvre :

- la clé d’activité et la graine exactes, y compris un UUID fourni en majuscules côté SQL ;
- les verrous partagés concurrents et l’exclusion de l’effacement ;
- les comptes absents, bannis et supprimés ;
- la déduplication d’un même UUID ;
- la libération après annulation Tokio ;
- l’invalidation du lease après `pg_terminate_backend`, avant tout effet externe ;
- la conservation du leader entre deux commits puis sa libération explicite ;
- la fermeture de la connexion et la reprise après abandon d’un leader sans libération explicite ;
- le retour au pool uniquement des connexions prouvées propres.

Le test réutilise les mêmes garde-fous que S05 : il refuse une base autre que `histae-dev` et un hôte non loopback. Il crée deux comptes synthétiques déterministes puis les supprime ; il ne modifie ni migration ni schéma.

```powershell
cargo test --locked --features postgres-integration --test postgres_locks
```

## Hors périmètre

Les advisory locks transactionnels propres à l’OTP et à la rétention restent dans leurs lots métier. Le mapping des erreurs vers l’enveloppe HTTP commune sera raccordé avec le transport Axum à partir de S07.
