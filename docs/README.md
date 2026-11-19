# Documentation Histae API

Les guides sont regroupés par thème. Les commandes s’exécutent depuis la racine du dépôt,
sauf indication contraire.

## Parcours de lecture

| Objectif | Ordre conseillé |
| --- | --- |
| Premier démarrage local | [Développement](development/development.md) → [Configuration](development/configuration.md) → [Docker](deployment/container-deployment.md#developpement-local) |
| Intégrer le mobile ou le dashboard | [Contrat HTTP](api/http-contract.md) → [Postman](../postman/README.md) → [Administration](features/administration.md) |
| Installer sur un serveur | [Configuration](development/configuration.md) → [Déploiement](deployment/container-deployment.md) → [Mise en production](deployment/production-readiness.md) |
| Exploiter l’API | [Exploitation](operations/operations.md) → [Supervision](operations/observability.md) → [Base de données](database/database.md) |
| Modifier le code | [Architecture](development/architecture.md) → [Tests](../tests/README.md) → [Performance SQL](database/sql-performance.md) |
| Mesurer la capacité | [Test de charge](testing/load-testing.md) → [Supervision](operations/observability.md) → [Performance SQL](database/sql-performance.md) |

## Guides par thème

### Développement — `development/`

- [Démarrage et outils](development/development.md) : compilation, données fictives et parcours locaux.
- [Configuration](development/configuration.md) : variables, valeurs par défaut, priorités et secrets.
- [Architecture](development/architecture.md) : domaines, responsabilités et assemblage des processus.

### Contrats et intégration — `api/`

- [Contrat HTTP](api/http-contract.md) : routes, requêtes, réponses, autorisations et idempotence.
- [Internationalisation](api/internationalization.md) : langues, catalogues, consentements et préférence du compte.
- [Collections Postman](../postman/README.md) : import et configuration des parcours de test.

### Fonctionnalités — `features/`

- [Administration](features/administration.md) : WebAuthn, sessions, rôles et corrections de profil.
- [Abonnements](features/billing.md) : droits Premium et revenus administratifs.
- [Achats Apple / Google](features/store-billing.md) : preuves, webhooks, restauration et rapports financiers.
- [Continuations et récompenses](features/continuation-and-rewards.md) : décisions privées, crédits et AdMob.
- [Notifications push](features/push-notifications.md) : préférences, destinataires, livraison et navigation.
- [Accès de revue Google Play](features/google-play-review.md) : compte dédié et publication dans le store.
- [Reprise des sessions mobiles](features/refresh-recovery.md) : rotation récupérable, expiration et révocation.
- [Protection SMS Sweego](features/sms-safety.md) : plafonds, arrêt des envois et alertes.
- [Numéros SMS publics](features/phone-blocklist.md) : synchronisation des listes et exceptions administratives.
- [Confidentialité et photos](features/privacy-and-photo-security.md) : localisation minimisée, codec et accès privé.

### Déploiement — `deployment/`

- [Docker, Caddy et Cloudflare](deployment/container-deployment.md) : topologies, réseaux, TLS et mises à jour.
- [Secrets de production](deployment/production-secrets.md) : certificats, stockage, supervision et fournisseurs.
- [Mise en production](deployment/production-readiness.md) : vérifications sur la livraison réelle.
- [Scripts serveur](../../histae-scripts/README.md) : commandes de déploiement et ordonnanceurs.

### Base de données — `database/`

- [Schéma et migrations](database/database.md) : installation, mise à niveau, sauvegarde et restauration.
- [Utilisation des tables](database/database-usage.md) : rôle des tables et points d’accès SQL.
- [Rôles PostgreSQL](database/postgres-roles.md) : séparation des accès, mots de passe et conversion dev.
- [Performance SQL](database/sql-performance.md) : index, plans d’exécution et protocole de mesure.

### Exploitation — `operations/`

- [Processus et maintenance](operations/operations.md) : workers, traitements bornés et incidents de démarrage.
- [Supervision et runbooks](operations/observability.md) : Prometheus, Grafana, alertes et diagnostic.

### Tests de charge — `testing/`

- [Guide k6 et Cloudflare](testing/load-testing.md) : environnement isolé, sessions fictives et montée en charge.
- [Validation approfondie](testing/load-testing-validation.md) : répétitions, sécurité concurrente et variantes du feed.
- [Tests automatisés](../tests/README.md) : suites Rust, PostgreSQL, Redis, S3 et codec.

## Conventions et entretien

Les exemples utilisent Bash depuis la racine de `histae-api-rust`. Les commandes Docker utilisent `sudo` ;
il peut être omis avec un accès Docker déjà administré. Les valeurs entre `<…>` ou `REPLACE_…` doivent être
remplacées avant exécution. Aucun secret réel ne doit être ajouté aux guides ou aux collections partagées.

Avec chaque modification, mettre à jour le guide qui possède le sujet, puis ses liens si nécessaire :

| Modification | Documentation à vérifier |
| --- | --- |
| Route, DTO, erreur, permission ou retry | Contrat HTTP et les deux collections Postman |
| Variable ou valeur par défaut | Configuration ; déploiement si utilisée par Compose |
| Schéma ou index | Base de données, performance SQL et tests d’intégration |
| Worker, rétention ou traitement borné | Exploitation et supervision |
| Module ou assemblage | Architecture |

Les tableaux du contrat restent compatibles avec `tests/route_inventory.rs`. Les ancres explicites
des runbooks sont utilisées par les règles Prometheus et doivent rester stables. Les guides décrivent
les commandes à rejouer pour valider la livraison actuelle.
