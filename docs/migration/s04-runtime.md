# S04 — configuration, exécution et logs sûrs

> Compte rendu historique de ce lot. Pour les commandes et l’arborescence actuelles, consulter le [README](../../README.md) et l’[architecture](../architecture.md).

## Parité NestJS retenue

`AppConfig` charge dotenv en best effort puis lit l’environnement du processus, comme `ConfigService`. Il conserve les valeurs par défaut et les bornes actuelles pour PostgreSQL, JWT, clés téléphone, WebAuthn admin, Sweego, Redis, FCM, Stripe, stockage S3, modération photo, textes légaux, proxy/CORS, workloads, métriques et les treize quotas. Les validations croisées restent actives : fournisseurs obligatoires et TLS en production, clés cryptographiques distinctes, budgets de durée, origine/RP ID WebAuthn et secret métriques indépendant.

Les valeurs sensibles sont stockées dans `SecretString`. Son implémentation `Debug` affiche uniquement `[REDACTED]`. `ConfigError` conserve le nom statique de l’option et une catégorie sûre ; il ne contient ni la valeur refusée ni une erreur de fournisseur. Le mode `--check-config` produit seulement `configuration_validated operation=… environment=…`.

## Exécution

Chaque binaire possède son runtime Tokio. `bounded_start` empêche une initialisation bloquée de produire un faux état prêt. `TaskSupervisor` fournit un `CancellationToken`, suit toutes les tâches dans un `JoinSet`, attend leur fin dans un budget explicite puis les interrompt si le drain expire. Les erreurs de tâche sont des codes bornés, jamais les messages ou stacks internes.

Les composants métier ne sont pas démarrés avant leur lot : exécuter un binaire sans `--check-config` renvoie un échec sûr. Cela empêche `api_started` ou `outbox_worker_started` d’être émis avant qu’un listener ou un dispatcher réel soit prêt.

## Journalisation

Le formateur reprend les motifs et la liste blanche de `safe-logging.ts`. Il refuse les champs sensibles, les chaînes libres, les nombres non finis et les noms d’événement invalides. Le subscriber n’active que la cible `histae` aux niveaux INFO et ERROR, afin que les dépendances ne puissent pas injecter leurs propres messages dans les logs de production par défaut.

Une divergence existe déjà dans la source NestJS : `scripts/maintenance.ts` utilise notamment `match_batches` et `match_work_remaining`, absents de la liste blanche de `safe-logging.ts`. S26 a branché la maintenance avec des compteurs agrégés explicitement autorisés, sans réintroduire ces champs non conformes.

## Limites du lot

Le listener HTTP, les pools PostgreSQL et Redis sont arrivés dans S07/S05/S07. Les binaires outbox et maintenance sont lançables depuis S26. Les métriques privées, l’image et les commandes d’exploitation finales restent dans S27 ; le binaire API attend encore son assemblage complet.
