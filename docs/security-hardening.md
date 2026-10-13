# Durcissement de sécurité — 13 octobre 2026

[Index](README.md) · [Déploiement](container-deployment.md) · [Tests](../tests/README.md)

## Corrections livrées

### Dépendances du site

Next.js et `eslint-config-next` passent à **16.3.8**, Sharp à **0.35.5** et `source-map-js` à **1.2.2**.
Le workspace pnpm applique un plancher corrigé à `source-map-js`, `js-yaml` 4.x et aux branches utilisées de
`brace-expansion`, sans remplacer des branches majeures compatibles. Le lockfile est livré avec ces changements.
L’image statique garde Nginx ; pnpm est installé avec la version fixée dans `package.json`, sans dépendre de
la présence de Corepack dans Node 24. Les fichiers `.env.*` sont exclus du contexte de build.

L’audit de production est passé de **12 alertes, dont 3 critiques, à zéro**. L’audit comprenant les outils de
build conserve une alerte : `braces` 3.0.3, dépendance de lint sans version corrigée publiée.
Elle n’est pas incluse dans le runtime Nginx, qui ne reçoit que l’export statique. Les configurations de lint
et les sources utilisées au build doivent rester contrôlées. Ne pas masquer cette alerte dans pnpm.
Voir [l’avis upstream](https://github.com/advisories/GHSA-vfj7-8cjw-p6xm).

### Confidentialité des journaux Nginx

Le site, le dashboard et le reverse proxy possèdent chacun `nginx-logging.conf`, copié dans
`/etc/nginx/conf.d/00-logging.conf`. Chaque bloc serveur choisit explicitement le format `histae_safe` pour
remplacer le format hérité de l’image officielle.

Les logs d’accès contiennent seulement :

- l’heure et la méthode HTTP normalisée ;
- une catégorie de route fixe, sans identifiant de compte ;
- le statut, la durée et un UUID de réponse API validé.

Ils excluent les URL complètes, paramètres de recherche, motifs administratifs, preuves AdMob, IP,
cookies, Authorization, Referer et User-Agent. Le logger d’erreurs conserve les diagnostics critiques ; les
messages d’erreurs HTTP ordinaires, qui peuvent contenir une URL, sont filtrés. La pile reverse proxy limite
les logs Docker de ses quatre services à **cinq fichiers de 10 Mo par service**.

### Tunnel et confiance proxy

Le serveur est accessible publiquement via le tunnel Cloudflare. `compose.production.yaml` réinitialise
explicitement les ports hôte de l’API et impose `172.30.0.3/32,172.30.0.4/32` comme proxys approuvés, en accord
avec les IP fixes de la pile voisine. Une valeur dev `TRUST_PROXY=false` dans le `.env` commun ne fait donc pas
partager aux visiteurs publics un seul compteur d’IP. Nginx continue de n’accepter l’IP Cloudflare que depuis
le connecteur `172.30.0.5`. Adapter ensemble les IP et ces CIDR si la topologie change.

Les manifests dev/LAN gardent leur publication configurable pour Postman ; aucune modification du `.env`
local ou de la pile en cours n’a été nécessaire. Le schéma et les routes HTTP restent identiques.

### Disponibilité de la vérification AdMob

- Les signatures DER P-256 malformées sont rejetées avant tout téléchargement.
- Le mutex protège uniquement les lectures et mises à jour du cache ; aucun appel HTTP ne le garde.
- Un téléchargement unique est partagé entre les demandes ; au plus 32 demandes peuvent attendre une clé,
  avec 5 secondes maximum d’attente du téléchargement en cours.
- Ces attentes sont séparées des 32 places de vérification cryptographique. Une clé encore valide peut être
  utilisée pendant une actualisation ; la saturation des demandes de clés ne bloque pas ce chemin.
- Le TTL de 24 h, le timeout réseau de 5 s, la limite de 64 Kio, le cooldown de 30 s, HTTPS, l’URL Google fixe,
  l’interdiction des redirections et le rejet des clés expirées sont conservés.

La surcharge et les indisponibilités restent des `503 admob_verification_unavailable` sans octroi de crédit.
La signature porte toujours sur les octets reçus et l’octroi reste lié à l’intention, au compte et à une transaction
unique. Google annonce jusqu’à cinq retries à une seconde d’intervalle : la grâce applicative de 24 h n’est pas
une garantie de relivraison par le fournisseur. [Protocole SSV](https://developers.google.com/admob/android/ssv#faq).

## Validation

| Contrôle | Résultat |
| --- | --- |
| Rust, tous les targets avec les features par défaut | **303 tests réussis**, dont 198 unitaires |
| Tests publicitaires ciblés | **8 réussis**, inclus dans le total Rust |
| Clippy, tous targets/toutes features, warnings refusés ; rustfmt | Réussis |
| Site : typage, lint, build et installation frozen lockfile | Réussis, 49 pages statiques, 44 pages localisées validées |
| Audit pnpm de production | Aucune alerte |
| Audit pnpm complet | Une alerte upstream `braces` dans l’outillage de développement |
| Nginx, trois configurations | Syntaxe valide ; marqueurs privés absents des logs et en-têtes de réponse arbitraires filtrés |
| Rendu Compose | LAN conservé, aucun port API de production publié, confiance proxy bornée, rotation des logs définie |

Les tests Nginx utilisent des conteneurs temporaires sans réseau et un backend factice. Les tests de signature
utilisent des clés en mémoire. Aucun OTP, achat, publicité réelle, changement de données utilisateur ou redémarrage
du serveur de production n’a été effectué. Ces contrôles ne constituent pas un test d’intrusion du serveur distant.

## Appliquer et rejouer

Copier aussi les nouveaux fichiers **`nginx-logging.conf`** dans les trois dossiers frontend/proxy. Reconstruire
les images et recréer les services avec les manifests déjà utilisés sur le serveur ; suivre les commandes du
[guide de déploiement](container-deployment.md). La reconstruction et la recréation sont nécessaires pour les
changements de Dockerfile, de logs et d’environnement. Ne pas changer de mode dev/production pendant une simple
mise à jour : les volumes de données sont différents.

Depuis le dépôt API, les trois projets voisins présents :

```bash
sudo -v
python3 scripts/verify-edge-security.py --sudo-docker
cargo test --locked --lib advertising::
```

Depuis le site : `pnpm audit --prod`, `pnpm run typecheck`, `pnpm run lint`, `pnpm run build`.
`pnpm audit` montre aussi l’alerte upstream de l’outillage mentionnée ci-dessus.
