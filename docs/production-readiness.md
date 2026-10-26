# Préparer une mise en production

[Index](README.md) · [Déploiement](container-deployment.md) · [Validation](../tests/README.md)

Ce guide décrit les validations à effectuer sur la livraison et sa topologie réelles. Rejouer ces contrôles
pour chaque nouveau commit, changement de secrets ou de serveur.

## Configuration et livraison

| Point | Validation attendue | Référence |
| --- | --- | --- |
| Image | Commit identifié, build avec lockfiles, WebAuthn inclus par défaut, étiquettes cohérentes entre services | [Développement](development.md), [Docker](container-deployment.md#production) |
| Environnement | `.env` choisi pour interpolation et injection ; `ENV=production`, secrets distincts | [Configuration](configuration.md) |
| PostgreSQL | TLS VerifyFull, rôles admin/migration/application séparés, migrations réussies, index valides et capacité adaptée | [Rôles](postgres-roles.md), [Base de données](database.md), [SQL](sql-performance.md) |
| Redis | TLS, mot de passe, quotas distribués et comportement en cas de panne vérifiés | [Configuration](configuration.md) |
| Photos | Bucket privé persistant, endpoint HTTPS signé accessible aux clients, upload sans analyse automatique et revue manuelle admin utilisables | [Docker](container-deployment.md#certificats-et-stockage), [Contrat](http-contract.md#photo-unique) |
| Proxy | Réseaux corrects, liste précise des proxys approuvés et IP visiteur vérifiée de bout en bout | [Docker](container-deployment.md#developpement-derriere-cloudflare-et-le-reverse-proxy) |
| Administration | Origine/RP ID réels, premier superadmin, passkey de secours et cookie HTTPS conformes | [Administration](administration.md) |
| Maintenance | Worker permanent, ordonnanceur actif, progression observée et absence de retard inexpliqué | [Exploitation](operations.md) |
| Supervision | Collecte privée authentifiée, destinataire d’alertes et personne responsable définis | [Supervision](observability.md) |
| Reprise | Sauvegardes hors machine et restauration isolée testées, y compris stockage objet et configuration | [Base de données](database.md#sauvegarde-et-restauration) |

L’API Docker utilise un healthcheck live. Vérifier aussi readiness depuis le proxy, les parcours métiers et
les files ; un statut Docker healthy ne prouve pas la livraison d’un SMS, d’un push ou d’un paiement.

## Fournisseurs et clients réels

Les validations locales avec doubles de test ne remplacent pas les essais suivants :

- Sweego : acceptation et livraison d’un OTP, erreurs/issue incertaine, webhook signé sur les octets bruts,
  suivi sans réactiver un code consommé ou expiré.
- Stripe sur le site : UUID/téléphone/OTP, session cookie, refus de rejeu et cohérence produit/prix et redirections, signatures de webhook, rejeux, états d’abonnement,
  réconciliation et effacement avec un compte fournisseur réel. Tester d’abord en développement/sandbox ;
  `ENV=production` exige ensuite une clé live.
- Apple/Google si activés : achats et restauration sur appareils, liaison au compte, prix du SDK,
  notifications authentifiées, renouvellement/grâce/hold/remboursement, acquittement et réconciliation.
  Vérifier la séparation test/live et le service privé ; voir [Achats multicanaux](store-billing.md).
- FCM si activé : credentials, enregistrement/révocation d’appareil, notification durable et résultat push.
- WebAuthn : enrôlement, connexion, passkey supplémentaire, révocation et expiration sur les navigateurs et
  authentificateurs effectivement utilisés, depuis le domaine HTTPS retenu.
- Mobile/dashboard : onboarding, consentements, refus d’accès, photo, feed, pagination, messagerie/SSE,
  gestion des admins, modération, signalements et droits RGPD selon le [contrat HTTP](http-contract.md).

Les webhooks publics doivent rejoindre la route correcte en conservant leur corps brut et signature.
Un `202` accepte une intention durable ; vérifier ensuite sa progression effective.

## Charge et capacité

Mesurer sur la topologie prévue avec des données et une concurrence représentatives : débit, p95/p99,
`5xx`, `429`, CPU, mémoire, I/O, attente des pools, verrous, retards outbox et maintenance.
Fixer les critères acceptables avant le test et conserver la configuration de mesure.
Les optimisations SQL et le nombre d’utilisateurs fictifs ne suffisent pas à annoncer une capacité RPS.
Voir [Performance SQL](sql-performance.md) et [Supervision](observability.md).

Le packaging actuel est mono-machine. PostgreSQL et SeaweedFS persistants ne constituent pas de haute
disponibilité ; tester la reprise et éviter builds ou grosses sauvegardes pendant les pics.

## Documents et revue juridique

Publier les quatre documents requis, leurs versions et URLs HTTPS ; renseigner la référence réelle de revue
juridique/DPO. Valider les durées de conservation, sous-traitants, consentements et parcours de droits pour
l’usage prévu. Les brouillons du site ont été actualisés pour les achats multicanaux ; leurs versions
publiées et `LEGAL_REVIEW_REFERENCE` ne sont pas changées automatiquement. La validation technique de ces variables ne valide pas leur contenu juridique.
Voir [Configuration juridique](configuration.md#documents-juridiques).

Une revue indépendante de sécurité/modération et les décisions opérationnelles d’astreinte restent des
validations externes au code. Conserver une preuve datée expurgée de données personnelles et secrets.

## Validation de la livraison

1. Exécuter les contrôles statiques et suites nécessaires selon [Tests](../tests/README.md).
2. Sauvegarder, appliquer la migration, déployer et vérifier readiness.
3. Vérifier les parcours réels concernés, la maintenance et la collecte privée.
4. Conserver commit/image, environnement, commandes, résultats et limites de preuve.
5. Préparer une reprise compatible avec le schéma et les effets déjà écrits avant de remplacer la livraison.
