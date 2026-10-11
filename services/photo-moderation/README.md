# Modération photo interne

[Documentation API](../../docs/README.md) · [Configuration](../../docs/configuration.md) · [Contrat photo](../../docs/http-contract.md#photo-unique)

Service Python utilisé avec `PHOTO_MODERATION_PROVIDER=local_http`. Il reçoit le WebP normalisé, analyse
visages/netteté/score NSFW en mémoire et ne conserve pas l’image. L’API Rust décide de l’état de modération.
Il utilise OpenCV et OpenNSFW ONNX sur CPU, avec une seule analyse simultanée et un warmup au démarrage.

## Démarrage local

Depuis la racine de `histae-api-rust`, après configuration de `PHOTO_MODERATION_TOKEN` dans `.env` :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d --build --wait photo-moderation
curl --max-time 5 http://127.0.0.1:8090/health
```

Le manifest transmet le secret à Python sous le nom `MODERATION_TOKEN`, d’au moins 32 octets.
L’API utilise `http://photo-moderation:8080` dans Docker, ou `http://127.0.0.1:8090` sur l’hôte.
Le port hôte dev est lié à loopback ; aucun endpoint de modération n’est public en production.

## Protocole

| Route interne | Auth / entrée | Réponse |
| --- | --- | --- |
| `GET /health` | Sans token | `{ "status": "ok" }` |
| `POST /v1/analyze` | `Authorization: Bearer <token>`, `Content-Type: image/webp`, octets bruts | `face_count`, `sharpness_score`, `nsfw_score` |

Le corps est limité à 500 000 octets et l’image décodée à 16 777 216 pixels. Les erreurs FastAPI de ce
service sont internes (`detail`) et ne sont pas l’enveloppe publique de l’API Rust. Il n’expose pas OpenAPI/Swagger.
La requête ne doit pas contenir une URL à télécharger ni un multipart.

## Décision et indisponibilité

L’API applique notamment `PHOTO_MODERATION_MIN_SHARPNESS` et `PHOTO_MODERATION_NSFW_REVIEW_THRESHOLD`.
Un résultat sûr peut être approuvé automatiquement. Toute anomalie, panne, timeout ou réponse invalide laisse
la photo privée en attente de revue, notamment avec `analysis_unavailable`. L’analyse automatique ne produit
pas seule un rejet. Les contenus non approuvés restent masqués aux autres utilisateurs selon le contrat.

Pour les certificats, ressources et démarrage production, voir [Déploiement](../../docs/container-deployment.md).
