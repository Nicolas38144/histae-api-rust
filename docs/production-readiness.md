# Corrections de la revue de migration

Ce suivi distingue les corrections du code de l'autorisation de mise en production.
La configuration de développement reste dans le `.env` local du projet ; aucun secret n'est ajouté ici.

## Corrections

| Constat | Correction | Couverture |
| --- | --- | --- |
| IP du socket absente | Fabrique Axum avec ConnectInfo partagée avec les tests TCP ; aucun proxy inféré d'une adresse fictive | Test TCP avec deux IP loopback et X-Forwarded-For falsifié |
| Appareils/SSE accessibles avant onboarding | OnboardedMobile sur les quatre routes | Contrats notifications et SSE, 403 avant action |
| Corps ignorés et erreurs brutes | Validation et limite de 1 Mio avant les handlers, octets conservés pour HMAC ; enveloppe JSON stable | Contrat HTTP : JSON invalide, type inconnu, dépassement, handler sans body |
| Réauthentification admin supplémentaire | AdminIdentity sur revue modération et reprise photo ; origine exacte conservée | Test HTTP/PostgreSQL avec session de 20 minutes, origine erronée, cookie absent et route exigeant une réauthentification |
| Validation JWT | Expiration refusée à égalité ; nbf vérifié sans arrondi avec la même horloge ; issuer/audience obligatoires et typés | JWT signés aux bornes, claims absents/mal formés et nbf fractionnaire |
| Export PostgreSQL | Date de naissance ISO UTC ; montants bigint en chaînes | Export réel avec montant supérieur à Number.MAX_SAFE_INTEGER |
| Coercition numérique et UUID | Helpers de transport explicites ; formes canoniques, nil/max pour UUID « all » ; Number pour query | Tests de nombres hexadécimaux/binaires/exponentiels et UUID |
| P0E01 masqué | Conversion DatabaseError commune, y compris modération et photos | Test 409 account_unavailable |
| Logs HTTP absents | Composition du logger sûr et des compteurs HTTP | Contrats observateur et branchement API |
| Métriques dépendances inactives | Instrumentation des adaptateurs de l'API | Clients simulés et métriques agrégées ; granularité PostgreSQL explicitée dans observability.md |
| Fichiers export bloquant Tokio | Création/ouverture/nettoyage hors threads async ; garde de nettoyage à l'annulation | Contrats export, fermeture du flux et libération du quota |
| Regex de modération divergentes | Classes et frontières ECMAScript explicites, répétitions excluant les séparateurs de ligne | Corpus Unicode de caractérisation |
| Métadonnée Stripe invalide assimilée à absente | Rejet de la réponse invalide avant le parcours legacy | Parser : absent, vide, valide, invalide |
| Panic expires_in OAuth | Construction et addition de durée vérifiées | Push avec i64 extrêmes, sans envoi FCM |
| Diagnostic Stripe effacé pendant RGPD | Propagation des erreurs métier normalisées | Effacement réel sous verrou avec dépendance exigeant la réconciliation |
| Fermeture SSE retardée | Expiration prioritaire même pendant une lecture de session bloquée ; contrôle de session avant heartbeat/événement prêts simultanément ; addition temporelle vérifiée | Tests révocation + heartbeat, expiration pendant un contrôle bloqué et date hors plage |

L'export de date conserve le comportement NestJS exécuté en UTC (cible conteneur).
Il ne reproduit pas les changements dépendant du fuseau du poste Node historique.
La date du profil HTTP ordinaire reste une date calendrier YYYY-MM-DD.

## Validation locale

Validation historique exécutée le 5 octobre 2026, avec PostgreSQL, Redis, S3 et le service de modération
de la pile Docker locale. Les commandes restent à rejouer sur Debian 13 et sur l’image de livraison :

- `cargo fmt --all -- --check` : réussi.
- `cargo clippy --locked --all-targets --all-features -j 2 -- -D warnings` : réussi.
- `cargo test --locked --all-targets --all-features --no-fail-fast -j 2` : **312 tests réussis**,
  dont 182 tests unitaires ; 0 échec, 0 test ignoré, 47 cibles exécutées.
- smoke HTTP de `http://127.0.0.1:18081` (équivalent Debian : `bash scripts/smoke-api.sh http://127.0.0.1:18081`) : santé, readiness, authentification mobile/admin,
  validation OTP et route inexistante vérifiées sur le binaire assemblé.
- Listener Prometheus local : 401 sans bearer, 200 avec un jeton temporaire aléatoire et 404 sur `/api/metrics`.
  Les compteurs HTTP et les compteurs PostgreSQL, Redis et S3 augmentent effectivement après le smoke.
  Les événements de fermeture HTTP, métriques puis pools ont été observés après interruption contrôlée.

La première passe en sandbox bloquait les connexions locales ; les résultats retenus sont ceux de la passe
complète avec cet accès. Elle a révélé la course SSE corrigée ci-dessus. OpenSSL a été compilé depuis les
sources vendored du lockfile puis cette même bibliothèque statique a été réutilisée pour les tests/Clippy
via des variables limitées aux processus de validation (`OPENSSL_NO_VENDOR`, `OPENSSL_DIR`, `OPENSSL_STATIC`).
La configuration du projet et du système n'a pas été changée pour cela ; aucune erreur de lien n'est retenue dans le bilan final.

Ces résultats ne constituent pas une comparaison exhaustive des 100 routes contre deux déploiements réels,
ni une validation du déploiement Linux de production. Les commandes et règles d'isolation sont dans
[tests/README.md](../tests/README.md).

## Conditions de mise en production restant externes au code

La [roadmap de mise en production](../roadmap.md) centralise désormais les travaux ouverts, leur ordre,
leurs responsables à désigner et les preuves nécessaires. Ce document conserve le bilan des corrections déjà validées.

- Déployer avec la configuration de production, TLS, secrets propres à chaque fournisseur, origine/RP ID WebAuthn réels et liste précise des proxies de confiance.
- Valider les sandboxes puis les comptes fournisseurs Sweego, Stripe et FCM ; les mocks ne prouvent pas les livraisons réelles.
- Effectuer les cérémonies WebAuthn et les parcours mobiles/dashboard avec les vrais clients.
- Tester restauration hors machine, supervision/astreinte et charge sur la topologie de production ; la pile PostgreSQL/S3 mono-nœud garde son point de panne unique.
- Finaliser les validations juridiques/DPO, conservation, sous-traitants et revue indépendante de sécurité et de modération.

Le guide [S29](migration/s29-cutover.md) décrit la bascule et les preuves à conserver avant de supprimer NestJS.
Aucune de ces validations externes n'est déclarée acquise par la seule réussite de Cargo.

## Artefacts de compilation

Le nettoyage initial a retiré 64,8 Gio dans target. Les profils locaux utilisent maintenant debug=1,
incremental=false. Les sources, configurations, données Docker et volumes ne sont pas concernés.
Après reconstruction et validation, les tailles logiques mesurées sont d'environ **5,18 Gio pour target**
et **5,21 Gio pour le projet complet**. Les artefacts conservés correspondent à la validation courante ;
un prochain build peut faire varier cette taille. Le jeton de smoke temporaire a été supprimé.
