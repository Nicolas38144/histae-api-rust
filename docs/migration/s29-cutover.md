# S29 — bascule de développement et retour arrière

> Compte rendu historique de ce lot. Pour les commandes et l’arborescence actuelles, consulter le [README](../../README.md) et l’[architecture](../architecture.md).

## Analyse de l’existant NestJS

NestJS compose tous les contrôleurs dans un processus HTTP et démarre séparément l’outbox et les maintenances. Les
clients utilisent le préfixe `/api`, sauf `/health/live` et `/health/ready`. Le dashboard de développement conserve
une origine WebAuthn exacte `http://localhost:5173`, un RP ID `localhost` et un proxy Vite vers
`http://localhost:8080`. PostgreSQL reste la source de vérité commune ; Redis et S3 portent respectivement le quota
et le relais SSE, puis les photos privées.

La base contient déjà les migrations `001_baseline_20260905`, `017_postgres_discovery` et
`018_postgres_admin_webauthn_state`, connues des deux implémentations. Un retour applicatif vers NestJS ne doit donc
ni restaurer une ancienne base, ni effacer les écritures Rust. Les intentions outbox, checkpoints Stripe, DSR et
photos doivent être reprises dans leur état courant.

## Composition Rust finale

Le binaire `api`, compilé avec `webauthn-probe`, construit désormais explicitement tous les services et routeurs S07
à S28. Il vérifie PostgreSQL, le pool de verrous d’activité, Redis et la configuration S3 avant d’ouvrir le port. Il
démarre le listener Prometheus privé lorsqu’il est activé, puis draine HTTP pendant au plus trente secondes au
signal d’arrêt. L’API ne lance aucun worker en arrière-plan : `outbox` et `maintenance` restent des binaires séparés.
L’image installe explicitement les autorités de certification utilisées par le vérificateur Rustls pour Sweego,
Stripe, FCM et tout stockage S3 HTTPS.

Le routeur assemblé expose exactement les 100 couples méthode/chemin de [http-contract.md](../http-contract.md). Les
adaptateurs partagés sont uniques dans le processus : même pool PostgreSQL, même pool de verrous d’activité, même
client Redis, même stockage S3, même relais SSE et mêmes compteurs opérationnels.

## Préparation vérifiable

Depuis la racine du dépôt sur Debian 13 :

```bash
docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d postgres redis object-storage photo-moderation
cargo run --locked --features webauthn-probe --bin db-migrate
```

Vérifier une vraie sauvegarde et sa restauration dans une base temporaire générée, sans modifier `histae-dev` :

```bash
bash scripts/verify-dev-backup-restore.sh
```

Le script refuse toute base autre que `histae-dev` et tout conteneur qui ne correspond pas au PostgreSQL du projet
Rust de développement. Il crée un nom aléatoire, restaure l’archive, compare l’historique des migrations et le
nombre de tables, puis supprime uniquement la base et l’archive temporaires qu’il a créées.

## Bascule

La bascule conserve une seule API et un seul worker outbox actifs :

1. arrêter l’entrée de nouvelles opérations depuis les clients de développement ;
2. arrêter proprement le worker NestJS et attendre la fin de ses claims ;
3. arrêter le serveur NestJS afin de libérer le port 8080 ;
4. vérifier qu’aucun processus NestJS ne conserve un verrou de session PostgreSQL ;
5. exécuter le migrateur Rust ;
6. démarrer l’API et l’outbox Rust ;
7. réactiver les clients seulement après les smoke tests.

Avec Compose :

```bash
docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d --build api outbox-worker
bash scripts/smoke-api.sh
```

Le service `maintenance` est derrière le profil `jobs` et ne démarre pas avec l’API. Une passe reste explicite :

```bash
docker compose --env-file .env -f compose.yaml -f compose.dev.yaml --profile jobs run --rm --no-deps maintenance
```

Le dashboard garde sa configuration actuelle. Son smoke réel peut être lancé depuis `histae-dashboard` :

```bash
export HISTAE_REAL_API_URL='http://127.0.0.1:8080'
pnpm run test:e2e:real
```

La validation manuelle restante couvre une cérémonie WebAuthn complète depuis `http://localhost:5173`, puis les
parcours mobiles OTP, refresh, photo, découverte, match et message avec des comptes synthétiques.

## Retour arrière vers NestJS

Le retour arrière ne restaure pas la sauvegarde : elle sert au sinistre, pas au changement de binaire. Procéder dans
l’ordre inverse afin de laisser NestJS lire les écritures déjà committées par Rust :

1. bloquer les nouvelles opérations de développement ;
2. arrêter l’outbox Rust, puis l’API Rust avec leur drain de trente secondes ;
3. vérifier l’absence de processus Rust actif et de claim ou verrou de session abandonné ;
4. démarrer NestJS et son worker outbox sur la même base, le même Redis et le même stockage S3 ;
5. exécuter santé, connexion existante, lecture d’une écriture créée sous Rust et traitement d’un événement outbox ;
6. réactiver les clients après réussite.

Ne jamais exécuter simultanément les workers NestJS et Rust. Un rollback applicatif n’annule pas un SMS, un POST
Stripe, un push FCM ou une écriture S3 dont l’issue est incertaine ; le backend repris doit continuer depuis la clé
d’idempotence et le checkpoint PostgreSQL existants.

## Critères avant suppression du dépôt NestJS

Le dépôt NestJS, ses scripts et ses anciens manifests ne peuvent être supprimés qu’après une fenêtre de validation
explicite ayant archivé :

- deux bascules Rust → NestJS → Rust sur les mêmes données synthétiques ;
- une restauration de sauvegarde vérifiée ;
- les smoke tests mobile et dashboard, y compris WebAuthn ;
- les sandboxes Sweego, Stripe et FCM ;
- les scénarios de panne entre claim, effet externe et checkpoint ;
- la charge, les budgets mémoire/pools et les métriques privées.

Les secrets réels, payloads fournisseur, téléphones, tokens et justifications sensibles ne font jamais partie de ces
preuves.

## Validation exécutée pour S29

La composition finale a été vérifiée sur la pile de développement réelle :

- formatage, compilation de toutes les cibles/features et Clippy avec les warnings refusés ;
- suite Rust complète, dont 157 tests unitaires et toutes les suites de contrat/intégration PostgreSQL, Redis, S3 et
  codec photo ;
- migration PostgreSQL puis sauvegarde/restauration dans une base temporaire, avec 46 tables publiques comparées ;
- image Docker construite, API déclarée saine et smoke HTTP local réussi ;
- arrêt `SIGTERM` avec fermeture ordonnée des listeners, ressources et pools en moins de trente secondes ;
- smoke Playwright du dashboard contre l’API Rust réelle.

Pendant la validation historique, une restriction de sandbox a empêché l’accès aux hardlinks du store pnpm et
produit un faux `photo codec is unavailable` ; le même corpus a réussi après levée de cette restriction. Cette
preuve doit être rejouée avec le codec installé sur la nouvelle machine de développement.
