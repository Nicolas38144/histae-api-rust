# Documentation Histae API

Cette documentation regroupe les guides nécessaires pour développer, intégrer et exploiter l’API actuelle.

## Parcours de lecture

| Objectif | Ordre conseillé |
| --- | --- |
| Premier démarrage local | [Développement](development.md) → [Configuration](configuration.md) → [Docker](container-deployment.md#developpement-local) |
| Tester le mobile ou le dashboard | [Contrat HTTP](http-contract.md) → [Postman](../postman/README.md) → [Administration](administration.md) |
| Installer sur un serveur | [Configuration](configuration.md) → [Docker](container-deployment.md) → [Mise en production](production-readiness.md) |
| Exploiter une installation | [Exploitation](operations.md) → [Supervision](observability.md) → [Base de données](database.md) |
| Modifier le code | [Architecture](architecture.md) → [Tests](../tests/README.md) → [Performance SQL](sql-performance.md) |
| Mesurer le débit de l’API et du tunnel | [Test de charge](load-testing.md) → [Supervision](observability.md) → [Performance SQL](sql-performance.md) |

## Références par sujet

- [Configuration](configuration.md) : chargement, priorité des variables, contraintes et secrets.
- [Déploiement](container-deployment.md) : topologies, réseaux, TLS, démarrage et mise à jour.
- [Secrets de production](production-secrets.md) : génération S3, CA PostgreSQL/Redis, certificat public S3 et identifiants Stripe.
- [Scripts serveur](../../histae-scripts/README.md) : procédures dev/production, vérifications et timer de maintenance.
- [Base de données](database.md) : schéma, migrateur, sauvegarde, restauration et suppression.
- [Rôles PostgreSQL](postgres-roles.md) : comptes séparés, mots de passe dans `.env`, installation et conversion dev.
- [Administration](administration.md) : premier superadmin, enrôlement, sessions et rôles.
- [Exploitation](operations.md) : processus, passes de maintenance et incidents de démarrage.
- [Achats Apple / Google et paiement web](store-billing.md) : configuration, OTP du site, restaurations, rapports et exploitation.
- [Parcours Stripe](billing.md) : configuration, Checkout, Premium, renouvellements, reprises et revenus.
- [Continuations et récompenses](continuation-and-rewards.md) : fenêtres, crédits, confidentialité des choix et activation AdMob.
- [Audit fonctionnel](feature-audit.md) : corrections, contrôles actuels et validations fournisseur restantes.
- [Confidentialité et capacité photo](privacy-and-photo-security.md) : localisation approximative, migration, limites du convertisseur et expiration des liens photo.
- [Internationalisation](internationalization.md) : langues, traductions des catalogues, documents, notifications et Stripe.
- [Contrat HTTP](http-contract.md) : routes, entrées, réponses, permissions et idempotence.
- [Postman](../postman/README.md) : import des JSON et parcours de test.
- [Supervision](observability.md) : Prometheus, Grafana, alertes et runbooks.
- [Performance SQL](sql-performance.md) : index, plans et protocole de mesure.
- [Test de charge](load-testing.md) : comptes fictifs, sessions sans OTP, k6, Cloudflare et nettoyage.
- [Validation approfondie de la charge](load-testing-validation.md) : répétitions, p99, sécurité concurrente et variantes du feed.
- [Résultats de charge locaux](load-testing-results.md) : mesures du 11–12 octobre 2026 et diagnostic du feed.
- [Architecture](architecture.md) : responsabilités du code et cycles de vie.
- [Tests](../tests/README.md) : commandes, infrastructure et limites des preuves.
- [Modération photo interne](../services/photo-moderation/README.md) : protocole du service Python.
- [Migration Caddy](caddy-migration.md) : serveurs statiques, tunnel, passerelle S3 et validation.
- [Mise en production](production-readiness.md) : validations sur la livraison réelle.
- [Feuille de route production](../roadmap-production.md) : blocages, dépendances et critères de clôture avant ouverture.

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

- [Protection des SMS Sweego](sms-safety.md) : plafonds par numéro, arrêt global et alertes.
