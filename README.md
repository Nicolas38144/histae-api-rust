# Histae API Rust

Migration incrémentale de l’API NestJS Histae. Le backend Nest reste la référence exécutable jusqu’à la campagne de parité et la bascule de développement.

## S01 — comparaison de contrat HTTP

Le binaire `contract-compare` exécute séquentiellement le même corpus contre une API de référence et une API candidate. Il vérifie chaque cible contre le contrat explicite, puis compare les réponses. Les corps reçus ne sont jamais inclus dans le rapport d’écart.

Les deux URLs et les deux namespaces d’état doivent être distincts. Un scénario `isolated_mutation` est refusé sans `--allow-isolated-mutations`. Ce drapeau certifie seulement l’intention : la préparation effective de schémas PostgreSQL, namespaces Redis et préfixes S3 distincts reste à la charge du lanceur d’intégration de chaque lot.

```powershell
cargo run --bin contract-compare -- `
  --corpus tests/contract/corpus/smoke.json `
  --reference-url http://127.0.0.1:8080/ `
  --reference-state nest-contract-a `
  --candidate-url http://127.0.0.1:8081/ `
  --candidate-state rust-contract-b
```

Le code de sortie vaut `0` pour une parité complète, `1` pour des différences de contrat et `2` pour une configuration, un corpus ou un transport inexploitable. Le rapport JSON identifie scénario, cible et champ sans recopier de payload potentiellement sensible.

Les scénarios sont exécutés dans l’ordre du corpus. Une mutation peut donc être suivie de lectures qui vérifient ses effets HTTP observables sur chaque état isolé. Chaque cible possède son propre client et son propre jar de cookies afin de couvrir les parcours authentifiés sans fuite de session. Dans un corps `exact_json`, `dynamic_fields` catalogue par pointeur JSON les seules valeurs non déterministes tolérées (`any`, `uuid_v4`, `non_empty_string` ou `integer`) ; toutes les autres valeurs, l’ordre des tableaux, `null` et l’absence restent comparés exactement.

Le corpus initial fixe deux comportements communs observés dans NestJS : `GET /health/live` et l’enveloppe JSON d’une route inconnue. Il grandira avec chaque module migré. Les flux multipart, SSE et fournisseurs signés auront des adaptateurs spécialisés dans leurs lots ; ils ne sont pas normalisés silencieusement par ce harnais JSON.

## S04 — socle d’exécution

Le crate expose désormais quatre binaires Tokio distincts : `api`, `outbox`, `maintenance` et `admin-bootstrap`. Ils partagent une configuration typée et stricte, un superviseur de tâches avec annulation explicite et drain borné, ainsi qu’un formateur de logs à champs autorisés. Les secrets utilisent un type dont `Debug` est expurgé et les erreurs de configuration ne recopient jamais leur valeur.

Durant cette étape, aucun serveur HTTP, worker ou accès PostgreSQL n’est encore disponible. Pour éviter un faux état prêt, les binaires refusent donc leur lancement normal avec un code sûr `component_not_implemented`. Le mode suivant valide seulement la configuration et sort immédiatement :

```powershell
cargo run --bin api -- --check-config
```

Les variables et contraintes conservées depuis NestJS sont décrites dans [docs/s04-runtime.md](docs/s04-runtime.md). Les routes HTTP commencent avec S07, après le socle PostgreSQL S05 et les verrous S06.

## S05 — PostgreSQL et historique des migrations

Le socle PostgreSQL utilise SQLx avec un pool borné, les timeouts existants, TLS avec vérification complète et les codecs explicites nécessaires au schéma Histae. À la connexion, Rust exige l’historique exact `001_baseline_20260905` puis `017_postgres_discovery`, leurs checksums actuels et les objets terminaux indispensables.

Le migrateur TypeScript reste l’unique outil qui crée ou fait évoluer le schéma :

```powershell
pnpm run db:migrate
```

Rust n’applique aucune baseline, ne fabrique aucun historique et ne répare aucun checksum. Le test réel est isolé derrière la feature explicite `postgres-integration` et refuse toute cible autre que `histae-dev` sur loopback :

```powershell
cargo test --locked --features postgres-integration --test postgres_compatibility
```

Les garanties et limites de ce lot sont détaillées dans [docs/s05-postgres.md](docs/s05-postgres.md).

## S06 — verrous PostgreSQL de session

Le pool d’activité séparé conserve les clés advisory existantes, l’ordre canonique des UUID et les règles d’éligibilité des comptes. Une lease vérifiable empêche un effet externe après la perte de la session qui portait le verrou. Une annulation détruit toute connexion dont le déverrouillage n’est pas prouvé.

Le leader de maintenance des matchs conserve de la même façon sa connexion et le verrou `37142581` entre les commits de lots. Cette infrastructure exige un pooling PostgreSQL de session. Les choix, garanties et tests de concurrence sont détaillés dans [docs/s06-postgres-locks.md](docs/s06-postgres-locks.md).

## S07 — Redis et cycle HTTP axum

Le socle HTTP expose maintenant le routeur de santé, l’enveloppe d’erreur stable, les extracteurs JSON/query/path, les en-têtes défensifs, CORS, la résolution d’IP derrière proxies et le quota global. Les méthodes non déclarées conservent le `404` Fastify historique et les prévols CORS restent extérieurs au lifecycle.

Redis fournit les fenêtres fixes Lua et le Pub/Sub avec connexions et commandes bornées. Les identités de quota sont HMACées avant stockage et toute panne du store configuré échoue fermement. Les décisions de parité et les commandes de test figurent dans [docs/s07-http-redis.md](docs/s07-http-redis.md).

## S08 — identité mobile et familles de refresh

Les primitives mobiles couvrent maintenant AES-256-GCM/HMAC pour les téléphones, JWT HS256 avec rotation locale par `kid`, refresh opaques, familles PostgreSQL, détection de rejeu, révocation des appareils et pagination des sessions. Les routes `me`, `refresh`, `logout`, `sessions`, révocation ciblée et `logout-all` sont disponibles sous forme de routeur axum composable.

Les mutations conservent l’ordre de verrouillage du backend NestJS. Un faux secret ne révoque rien ; le rejeu d’un ancêtre authentique révoque sa famille et committe avant que le service ne retourne l’erreur publique. Les choix, le mapping NestJS → Rust et les validations figurent dans [docs/s08-mobile-identity.md](docs/s08-mobile-identity.md).

## S09 — OTP et livraison Sweego

Les routes publiques d’envoi et de vérification OTP, ainsi que le webhook Sweego signé, sont disponibles sous forme de routeurs axum composables. PostgreSQL sérialise chaque téléphone pseudonymisé, conserve l’idempotence des demandes et empêche un callback tardif de réactiver un ancien code. La vérification peut créer le compte puis délègue l’émission de la famille mobile au socle S08.

Le client Sweego effectue un seul POST borné, sans retry automatique. Les issues réseau incertaines restent récupérables par callback signé sur les octets bruts. Les contrats, états et commandes de validation figurent dans [docs/s09-otp-sweego.md](docs/s09-otp-sweego.md).

## S03 — prototype du codec photo

Le prototype conserve temporairement `PhotoProcessorService` comme référence de conversion dans un processus Node isolé. Le parent Rust reproduit les contrôles extension/MIME/signature et borne l’entrée, la sortie, la concurrence et la durée du processus. Les octets circulent par pipes : aucune photo temporaire n’est créée sur disque.

Cette décision préserve exactement le pipeline Sharp/libvips pour JPEG, PNG, WebP, HEIC et HEIF, notamment l’orientation EXIF, le refus des animations, les limites de pixels et les six tentatives d’encodage WebP. Elle est détaillée dans [docs/s03-photo-codec.md](docs/s03-photo-codec.md).
