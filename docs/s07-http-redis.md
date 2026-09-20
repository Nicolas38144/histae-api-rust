# S07 — Redis et cycle HTTP axum

## Comportement NestJS conservé

Le transport Rust reproduit le cycle Fastify commun avant l’ajout des routes métier. Chaque requête hors prévol CORS reçoit les en-têtes défensifs, un `X-Request-ID` UUID v4 conservé s’il est valide ou remplacé sinon, puis la limite globale par IP. Les deux chemins exacts `/api/billing/stripe/webhook` et `/api/auth/sweego/webhook` sautent uniquement cette limite globale, y compris avec une query string ; leurs limites dédiées restent la responsabilité de leurs futurs handlers.

Les réponses d’erreur utilisent toujours `{ "error": { "code", "message" } }`. Axum ne laisse donc pas sortir ses rejets `422` ou ses textes internes. Une méthode non déclarée, une route inconnue et un slash terminal supplémentaire produisent le `404 route_not_found` historique. `HEAD` sur une route `GET` conserve le statut et les en-têtes en supprimant le corps. La limite JSON générale reste de 1 Mio.

Les extracteurs `ValidatedJson`, `ValidatedQuery` et `ValidatedPath` appliquent le code et le message propres au DTO. Chaque DTO strict doit déclarer `#[serde(deny_unknown_fields)]`, comme les DTO Nest décorés. Une erreur syntaxique JSON, un média non pris en charge ou un corps trop grand reste toutefois une erreur parseur commune `invalid_request_body`, avant la validation DTO, comme avec Fastify.

## CORS et proxies

Quand une liste d’origines existe, `tower-http` applique les méthodes, en-têtes autorisés, en-têtes exposés et le max-age de 600 secondes. Un petit adaptateur transforme son prévol `200` en `204`, valeur réellement observée avec Fastify. Le prévol reste extérieur au lifecycle : il ne consomme pas de quota et ne reçoit ni en-têtes défensifs ni request ID.

`TRUST_PROXY=false` ignore `X-Forwarded-For`. Le mode global utilise l’adresse la plus à gauche. Une liste CIDR est parcourue depuis le socket vers le client et s’arrête au premier proxy non approuvé. Une chaîne invalide retombe sur l’adresse du socket au lieu de faire confiance à une valeur partielle.

## Redis et quotas

`RedisService` utilise une connexion gérée pour les commandes et une connexion dédiée par abonnement Pub/Sub. Les connexions et commandes sont bornées par les timeouts configurés ; les reconnexions suivent les cinq essais exponentiels existants. TLS vérifie le serveur et peut charger `NODE_EXTRA_CA_CERTS` sans inclure son contenu dans une erreur ou un log.

Le script Lua de fenêtre fixe est identique à NestJS : `INCR`, `PEXPIRE` seulement au premier passage, puis retour du compteur et de `PTTL`. Les clés de quota gardent le format `histae:rate-limit:<nom>:<HMAC-SHA256>` ; l’identité brute n’est jamais envoyée à Redis. Une panne Redis refuse la requête avec `503 rate_limit_unavailable`. Le stockage mémoire, réservé aux configurations non Redis, garde la même fenêtre fixe et balaie les entrées expirées toutes les 256 opérations.

## Santé et observabilité

`GET /health/live` retourne `{"status":"ok"}` mais passe volontairement par le quota global. `GET /health/ready` vérifie séquentiellement PostgreSQL, Redis, puis le stockage objet et retourne `503 request_failed` au premier échec. Le port `DependencyProbe` permettra à S15 de raccorder le `HeadBucket` S3 sans placer une dépendance fournisseur dans le transport.

L’observateur HTTP ne reçoit que la méthode, le modèle de route, le statut, le request ID et une durée arrondie. Une route inconnue devient `<unmatched>` ; aucun chemin concret ni query string n’entre dans les logs. Le point d’observation est actuellement la production de la réponse. S22 et S24 devront l’étendre jusqu’à la fermeture du body pour les flux SSE et export, dont la durée ne peut pas être mesurée correctement à ce stade.

## Tests

La suite `http_contract` couvre les enveloppes, en-têtes, UUID, HSTS, quotas et pannes, exemptions webhook, ordre readiness, CORS autorisé/refusé, `404`/méthode/`HEAD`, slash terminal, validation JSON/query/path, valeurs par défaut, champs inconnus et limite 1 Mio. Un test démarre un vrai listener TCP axum avec `ConnectInfo`.

Le test Redis réel est isolé derrière une feature, utilise exclusivement la base logique 15 sur loopback et crée des clés/canaux UUID qui expirent :

```powershell
cargo test --locked --features redis-integration --test redis_integration
```

La suite autonome du lot s’exécute avec :

```powershell
cargo test --locked
cargo clippy --locked --all-targets --features redis-integration -- -D warnings
```

## Hors périmètre

Le binaire `api` n’est pas encore activé par le bootstrap : S15 doit fournir la vraie sonde S3 avant qu’un processus puisse annoncer `ready`. Les identités mobile/admin, les limites dédiées, le multipart photo, les raw bodies signés et les flux longs seront raccordés par leurs lots respectifs au routeur et aux primitives de ce lot.
