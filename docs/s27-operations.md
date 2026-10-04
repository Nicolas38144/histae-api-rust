# S27 — métriques privées, CLI, image et exploitation

## Analyse de l’existant NestJS

`OperationalMetricsService` borne les séries HTTP à 200 couples méthode/route et conserve les mêmes onze buckets de durée pour HTTP et dépendances. `MetricsServerService` ouvre un listener séparé, désactivé par défaut, qui n’accepte que `GET /metrics` sans query string et un bearer token comparé en temps constant. Les erreurs d’export sont ramenées à `503 Metrics unavailable` et toutes les réponses portent `Cache-Control: no-store` et `X-Content-Type-Options: nosniff`.

Le snapshot persistant regroupe le pool PostgreSQL, l’outbox, les deux files opérationnelles, la livraison OTP, les callbacks Sweego et les cinq maintenances. Une panne PostgreSQL ne doit pas supprimer les compteurs en mémoire : seul `histae_metrics_collection_success` passe à zéro. L’image Nest commune exécute API, migration, outbox et maintenance sans privilèges, en lecture seule, avec un `/tmp` borné. Le codec photo Node est une dépendance de production réelle malgré le remplacement du serveur NestJS.

## Mapping NestJS → Rust

| NestJS | Rust |
| --- | --- |
| `OperationalMetricsService` | `operations::metrics::OperationalMetrics` derrière un `Mutex` court et borné |
| interceptor HTTP de fin de réponse | implémentation `HttpObserver`, composable avec `SafeHttpObserver` |
| `OperationalStatusService` | `PgOperationalStatus` et ses lectures SQLx sans donnée personnelle |
| `PrometheusExporterService` | `PrometheusExporter` et rendu texte déterministe |
| `MetricsServerService` | listener Axum séparé `MetricsServer` avec arrêt gracieux |
| `scripts/migrate.ts` | binaire `db-migrate`, catalogue embarqué et verrou consultatif |
| `scripts/init-object-storage.ts` | binaire idempotent `storage-init` |
| image Node commune | image multi-stage Rust + runtime Node minimal requis par Sharp/HEIC |

## Contrat et divergence runtime approuvée par le plan

Les noms métier, labels fermés et buckets sont conservés. La RSS reste publiée sous `histae_process_resident_memory_bytes` sur la cible Linux de l’image. `histae_runtime_info{runtime="rust"} 1` identifie le runtime.

Les séries `histae_process_heap_used_bytes` et `histae_nodejs_event_loop_delay_p95_seconds` ont été retirées. Elles décrivent V8 et la boucle Node ; produire une valeur Rust sous ces noms serait faux. L’alerte Node correspondante a été retirée. Les dashboards métier ne dépendaient pas de ces séries.

## Exploitation

Développement :

```powershell
docker compose -f compose.yaml -f compose.dev.yaml up -d postgres redis object-storage photo-moderation
cargo run --bin db-migrate
```

Image commune :

```powershell
docker build -t histae-api-rust:local .
```

Le build utilise Rust 1.88 et compile la feature WebAuthn. Le dernier étage est basé sur Node 22 uniquement pour exécuter le codec photo épinglé ; les sources NestJS, `.env`, `.secrets`, tests et fixtures n’entrent pas dans le contexte final. Tous les services applicatifs utilisent UID/GID 1000, un rootfs en lecture seule, des capacités supprimées et un tmpfs borné.

`compose.production.yaml` conserve PostgreSQL 7 Gio uniquement pour la cible serveur 16 Gio. Ce budget ne s’applique pas à `compose.dev.yaml`, où PostgreSQL reste plafonné à 1 Gio. Redis TLS, le gateway S3 HTTPS, SeaweedFS server et la modération restent privés sur leurs réseaux respectifs. Les secrets de supervision sont lus depuis `.secrets/`, exclu du build et de Git.

## Arrêt et codes de sortie

Les workers conservent le superviseur et le `CancellationToken` S04/S26. Le listener métriques a son propre token d’annulation et un drain de cinq secondes. `db-migrate` et `storage-init` rendent un code non nul et un code d’erreur normalisé sans imprimer de secret. Le migrateur ne reset jamais, ne répare jamais un checksum et refuse toute version inconnue.

## Activation en S29

S27 a fourni les composants et manifests d’exploitation. S29 assemble maintenant les routeurs, adaptateurs et le
listener métriques dans le binaire HTTP. La procédure de démarrage, de drain et de retour arrière est décrite dans
[s29-cutover.md](s29-cutover.md).
