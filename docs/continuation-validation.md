# Validation des continuations — 13 octobre 2026

[Documentation](README.md) · [Règles et activation AdMob](continuation-and-rewards.md) · [Tests](../tests/README.md)

## Périmètre livré

API Rust, dashboard administrateur et site vitrine dans ses quatre langues. Le dépôt mobile n’est pas modifié. Les récompenses publicitaires restent désactivées dans le `.env` local ; le serveur vérifie les preuves et conserve les soldes, sans intégrer de SDK publicitaire dans le dashboard ou le site.

Les règles livrées comprennent les fenêtres fixes de 24 h + 24 h, la pause de l’envoi pendant la décision, les choix privés, les trois crédits gratuits hebdomadaires, le stock publicitaire de trois, le plafond Free de six continuations, et le débit de chaque participant Free seulement après deux accords. Premium ne consomme pas de crédit.

## Résultats après refactoring

| Vérification | Résultat |
| --- | --- |
| `cargo fmt --all -- --check`, `git diff --check` | Réussis |
| Clippy, tous targets et toutes features, warnings refusés | Réussi |
| Compilation de tous les targets sans features par défaut | Réussie |
| Tests unitaires Rust | **193 / 193** |
| Tous les targets Rust, features par défaut | **298 / 298 au total** : 193 tests unitaires et 105 tests de contrats/adaptateurs, dont 15 contrats de matchs |
| Sept suites PostgreSQL sur instance temporaire | **24 / 24**, dont 16 cas de matchs/crédits |
| Migration depuis l’ancien quota | Usage conservé, `free_used_count` rempli, anciens accords en attente remis à zéro |
| Réexécution du migrateur | Nouveaux accords/réservations et usage conservés |
| Séparation des rôles PostgreSQL | Installation neuve, upgrade et reset vérifiés avec les binaires livrés ; privilèges applicatifs limités |
| Collections Postman française et anglaise | JSON valides, **108 requêtes uniques** chacune ; inventaire identique aux routes Axum |
| Dashboard : types, lint et build | Réussis |
| Dashboard : tests de composants/unitaires | **51 / 51** |
| Dashboard : navigateur Chromium | **2 / 2**, sessions et passkey virtuelle ; les cas `@real` ne font pas partie de ce lancement |
| Site : types, lint, build statique | Réussis ; 49 pages produites, validation SEO/navigation des 44 pages localisées |
| Image API locale | Image de la livraison fonctionnelle précédente ; non reconstruite pendant le refactoring |
| Smoke HTTP et santé locale | Réussis sur cette image avant le refactoring ; ce contrôle ne prouve pas un nouveau déploiement des sources |

Le build du dashboard conserve l’avertissement Vite sur un chunk principal dépassant 500 Ko. Ce contrôle réussit ; aucune optimisation du bundle ne fait partie de cette intervention.

### Refactoring des trois projets

- API : domaine/erreurs publicitaires séparés du SQL ; le transport dépend de `RewardStore` et l’assemblage fournit le repository PostgreSQL. Les enums conservent les valeurs JSON existantes.
- API : gardes de compte/consentement partagées dans `identity/pg_guards.rs`, ordre des verrous conservé, crédits inclus distingués du plafond de continuations. Le cache de clés garde ses délais et limites ; le téléchargement et la vérification sont séparés.
- Dashboard : cartes de plans et de crédits avec props typées, écrans Traits/Signalements lisibles, erreurs HTTP centralisées. Les protections de session et les appels API sont conservés.
- Site : modèles de cartes localisés séparés du rendu, composants serveur conservés, identifiants stables pour les clés React, règles ESLint Next.js/TypeScript et contrôles TypeScript renforcés.
- Tests : `ContinuationCredits.test.tsx` n’était pas découvert par le filtre Vitest. Le fichier est renommé en `continuation-credits.spec.tsx` ; ses deux tests sont désormais exécutés. Un test couvre les codes d’erreur pouvant correspondre à des propriétés du prototype d’un objet.

Les sources refactorisées passent les contrôles Rust, les suites PostgreSQL isolées et les builds frontends ci-dessus. Le schéma et le contrat HTTP ne sont pas modifiés par ce refactoring. La vérification de séparation des rôles et les contrôles de la pile réelle restent ceux de la livraison fonctionnelle précédente.

### Cas métier et sécurité couverts

- Première acceptation sans publication à l’autre personne ; confirmation publiée à deux une seule fois, y compris après retry.
- Coût explicite borné à 0 ou 1 avant mutation ; refus d’un coût Premium devenu incompatible avec Free.
- Réservations concurrentes sur plusieurs matchs sans dépassement de l’allocation ; débit unique des deux participants.
- Envoi et rejeu d’un message refusés pendant la pause ; historique encore lisible, envoi rouvert après les accords.
- Échéance calculée depuis la première fenêtre, sans réouverture d’un ancien match oublié par la maintenance.
- Stock publicitaire conservé au changement de semaine, plafond hebdomadaire indépendant du stock disponible.
- Changement d’abonnement concurrent sérialisé avec la confirmation ; retrait de consentement empêchant tout débit partiel.
- Signature ECDSA correcte, altération de montant refusée, champs répétés/non autorisés refusés, signature encodée acceptée sans réécriture des octets signés.
- Récompense liée à l’intention, à l’unité et au compte ; transaction dédupliquée, rejeu sur une autre intention refusé, stock plafonné, intention annulée non créditable.
- Export des données du demandeur avec pagination ; aucune réservation/décision individuelle du partenaire exportée.
- Effacement du wallet, des intentions et réservations du demandeur, libération des réservations du partenaire, preuve tardive incapable de recréer le solde effacé.
- Guards d’authentification, onboarding, sessions, SSE, profils, découverte, facturation et droits conservés dans les suites de régression.

## Isolation et état local avant refactoring

Les suites finales PostgreSQL utilisent `scripts/verify-continuation-postgres.py` : conteneur temporaire avec stockage tmpfs, port aléatoire publié uniquement sur loopback, mots de passe temporaires, rôle applicatif `histae_app`. Le migrateur est celui de l’image Docker et utilise `histae_migrate`. Les fixtures, le conteneur et les credentials sont supprimés après les tests.

Le premier lancement d’un test d’effacement sur la pile partagée a rencontré son worker réel. Il a été déplacé vers cette instance isolée. Les deux comptes temporaires restés après cette interruption ont été identifiés en excluant les UUID de la sauvegarde protégée et en vérifiant leur cohorte de test, puis nettoyés.

La pile locale contrôlée après la livraison fonctionnelle conservait **5 001 comptes, 5 000 matchs, 74 996 messages et un seul superadmin**. Le schéma compte **49 tables publiques**. L’API et le worker utilisent l’image `histae-api:local` d’identité `sha256:6bf0be51a3530bc28eedd24e842386ffdbb41a8a3336db08ee41aa40d5448688`, sans OOM ni redémarrage inattendu lors du contrôle final.

Le catalogue HTTP local expose Free avec 3 crédits inclus, un plafond de 6 continuations et un stock publicitaire de 3 ; Premium reste à 48 € par an. `rewarded_advertising_enabled` est `false`. Le webhook désactivé renvoie `503 advertising_disabled` avant toute récupération de clés ou ajout de crédit.

## Rejouer

Depuis la racine de l’API, construire l’image puis lancer le test isolé :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml build api
sudo -v
python3 scripts/verify-continuation-postgres.py --sudo-docker
sudo bash scripts/verify-postgres-roles.sh
bash scripts/smoke-api.sh
```

Cargo reste exécuté par l’utilisateur courant. Le smoke exige une API déjà démarrée et utilise uniquement loopback. Les instructions du [guide de tests](../tests/README.md) décrivent les suites Rust et les variantes avec accès Docker déjà disponible.

Dans chaque dépôt frontend : `pnpm run typecheck`, `pnpm run lint`, `pnpm run build`. Dans le dashboard : `pnpm run test:unit` et `pnpm run test:e2e`.

## Limites de la preuve

Ces validations ne constituent pas un test de publicité réelle ou une approbation AdMob/juridique. Les tests cryptographiques utilisent une clé de test générée en mémoire ; la persistance et ses races sont vérifiées sur PostgreSQL. L’activation sur un compte AdMob réel, le callback signé traversant Cloudflare, les unités/configurations réelles, les choix UMP et le parcours mobile restent à vérifier avant ouverture aux utilisateurs. Les textes juridiques du site restent des brouillons à faire valider et versionner.

Le site et le dashboard sont construits localement ; aucun déploiement distant ni achat publicitaire/Stripe n’a été effectué. Aucun nouvel OTP n’a été envoyé. Cette campagne ne relance pas les benchmarks de débit : leurs résultats historiques restent dans [Résultats de charge](load-testing-results.md).
