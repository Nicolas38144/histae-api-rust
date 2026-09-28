# Analyse photo locale

Service interne optionnel utilisé avec `PHOTO_MODERATION_PROVIDER=local_http`. Il analyse uniquement le WebP
normalisé en mémoire et renvoie le nombre de visages, la netteté et un score NSFW. Il ne conserve aucun octet.

Depuis la racine du dépôt, après configuration de `PHOTO_MODERATION_TOKEN` dans `.env` :

```bash
docker compose --env-file .env -f compose.dev.yaml up -d --build photo-moderation
```

Le port `8090` est publié uniquement sur loopback. L’API peut approuver un résultat clairement sûr, mais toute
anomalie, indisponibilité ou réponse invalide laisse la photo privée en `pending/analysis_unavailable` ou avec les
codes de revue correspondants. Le service ne produit jamais une décision automatique `rejected`.
