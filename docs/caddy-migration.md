# Serveurs Caddy — déploiement et vérification

[Index](README.md) · [Déploiement](container-deployment.md) · [Tests](../tests/README.md)

## Architecture

Les quatre usages précédents de Nginx sont remplacés par **Caddy 2.11.7**, avec l’image officielle Alpine
figée par digest. L’API Rust reste servie par Axum. Les routes et contrats JSON ne changent pas.

| Service | Configuration | Fonction |
| --- | --- | --- |
| Site | `../histae-site/Caddyfile` | Export statique Next.js, langues, redirections juridiques, cache et CSP |
| Dashboard | `../histae-dashboard/Caddyfile` | SPA React, `/api/*`, absence de cache et protections navigateur |
| Reverse proxy | `../histae-reverse-proxy/Caddyfile` | Domaines publics, confiance du connecteur et routage Docker |
| Passerelle S3 production | `docker/storage-gateway/Caddyfile` | TLS interne et transmission des requêtes S3 signées |

Le site, le dashboard et le proxy ont chacun une copie identique de `caddy-common.caddy` parce que
leurs contextes de build Docker sont indépendants. Le test vérifie cette égalité. Ces snippets centralisent
dans chaque image la politique de logs et la limite de corps HTTP. Les anciennes configurations Nginx sont supprimées.

## Tunnel, HTTPS et adresses de confiance

Le connecteur Cloudflare continue de cibler `http://reverse-proxy:80`. Aucun port hôte n’est publié par
la pile edge. Caddy écoute explicitement sur `:80`, désactive HTTPS automatique et son API d’administration.
Cloudflare prend en charge le HTTPS public ; aucune extension DNS Cloudflare ou demande ACME n’est nécessaire.

- Le proxy approuve seulement `172.30.0.5/32` et lit `CF-Connecting-IP`.
- Il remplace `X-Forwarded-For` par cette IP validée et accepte `https` seulement depuis ce connecteur.
- Le dashboard approuve seulement `172.30.0.4/32`, parse la chaîne depuis la droite et ajoute cette IP de proxy.
- L’API conserve `TRUST_PROXY=172.30.0.3/32,172.30.0.4/32`.
- Le dashboard conserve `Origin` et les cookies, et ajoute HSTS seulement pour HTTPS annoncé par le proxy attendu.

Modifier ces adresses ensemble si le réseau change. Ne pas approuver toute la plage privée. Une requête
directe avec des en-têtes IP ou HTTPS falsifiés ne reçoit pas la confiance du connecteur.

## Sécurité et comportements conservés

Les trois serveurs HTTP fonctionnent avec UID/GID **1000**, avec uniquement `NET_BIND_SERVICE`, sans nouveaux privilèges,
avec racine en lecture seule et un `/tmp` éphémère. La passerelle S3 conserve UID/GID **101**, la racine
en lecture seule et uniquement `NET_BIND_SERVICE`. La configuration Caddy n’est jamais sauvegardée.

La limite de corps HTTP reste **1 Mio**, comme le défaut Nginx précédent. Les limites applicatives Rust
s’appliquent également. Les flux SSE ne sont pas compressés ; le premier événement est envoyé immédiatement.
Les requêtes API gardent préfixe, paramètres et corps. Les erreurs 401/404 API ne deviennent pas une page React.
Les fichiers cachés, sources et source maps du dashboard sont refusés, même si un fichier est présent.

Le site conserve domaine canonique, sélection de langue, redirections 308 des documents et leurs paramètres,
cache public d’un jour pour les ressources, page 404 avec statut 404 et protections navigateur.
Le dashboard force `Cache-Control: no-store`, y compris sur les réponses API.

### Logs

Les logs JSON conservent statut, durée, métriques numériques et champs de diagnostic fixes. Les champs métier
sont uniquement la méthode normalisée, une catégorie fixe de route et un UUID v4 de réponse API validé.
L’objet requête, les en-têtes de réponse, les identifiants de comptes, IP, cookies, URL et paramètres sont retirés.
Les erreurs HTTP détaillées ne sont pas dupliquées par le logger runtime. Les logs Docker gardent leur rotation
à cinq fichiers de 10 Mo. Les contrôles `/healthz` sont exclus des logs.

La passerelle S3 n’active pas de log d’accès et exclut les erreurs HTTP contenant potentiellement une URL signée.

### Stockage

Le certificat et sa clé restent sur l’hôte dans `.secrets/storage-gateway/`, lisibles par UID/GID 101 avec
permissions restreintes. Le montage cible maintenant `/etc/caddy/tls`. Il n’y a pas de génération automatique
de certificat ni de changement du domaine `HISTAE_STORAGE_HOST`.

Le tunnel envoie `storage.histae.com` à `http://reverse-proxy:80`, comme les autres domaines.
Le Caddy principal relaie vers `https://storage-gateway:443`, vérifie le certificat avec
`tls_server_name storage.histae.com` et conserve le Host public avec `header_up Host {hostport}`.
Cette dernière directive empêche le remplacement automatique du Host par Caddy 2.11 sur un upstream HTTPS,
qui invaliderait SigV4. Chemin encodé, query et corps ne sont pas réécrits ; le backend SeaweedFS reste privé.
Ce domaine dessert exclusivement la passerelle de production et ne suit pas `HISTAE_API_UPSTREAM`.
La pile dev conserve son stockage local. Sans passerelle de production disponible, la route renvoie une
erreur 502 ; elle ne bascule pas vers le stockage dev. La passerelle et son certificat sont requis.

## Appliquer sur le serveur

Copier les Dockerfiles, les `Caddyfile`, les trois `caddy-common.caddy`, le Compose du proxy, les scripts du dépôt `histae-scripts`,
ainsi que `compose.production.yaml` et le Caddyfile S3 de l’API. Les fichiers `.env` et certificats existants
continuent de fournir la configuration. La reconstruction et la recréation des conteneurs sont nécessaires.

Depuis `histae-reverse-proxy`, pour les trois serveurs HTTP :

```bash
sudo docker compose --env-file .env -f docker-compose.yml config --quiet
sudo docker compose --env-file .env -f docker-compose.yml build
sudo docker compose --env-file .env -f docker-compose.yml \
  up -d --no-deps --force-recreate --wait site dashboard reverse-proxy
sudo docker exec reverse-proxy-container \
  caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
sudo docker exec reverse-proxy-container \
  sh -c 'wget -T 10 -qO- "$HISTAE_API_UPSTREAM/health/ready"'
```

La dernière commande doit renvoyer `{"status":"ready"}`. La recréation coupe brièvement les connexions publiques.
Le nom du service cible du tunnel reste identique.

**Si la pile API utilise déjà les manifests de production**, depuis `histae-api-rust` :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.production.yaml \
  run --rm --no-deps storage-gateway validate --config /etc/caddy/Caddyfile --adapter caddyfile
sudo docker compose --env-file .env -f compose.yaml -f compose.production.yaml \
  up -d --no-deps --force-recreate --wait storage-gateway
```

La pile API dev n’a pas cette passerelle. Ne pas changer de mode pour cette migration : les volumes dev et
production diffèrent. Depuis `histae-scripts`, `bash reset-docker-compose.sh --dev` redémarre la pile dev
et `--production` sélectionne explicitement la production avec supervision. Les deux modes valident les
Caddyfiles avant recréation et exécutent une passe de maintenance ; le mode production valide aussi le Caddyfile S3.

Après déploiement, vérifier aussi connexion passkey sur le domaine réel, chargement d’une fiche administrateur,
réponse 401 sans session, réception SSE et affichage d’une photo signée. L’enrôlement réel d’une passkey nécessite
le navigateur et l’authentificateur de l’administrateur.

## Vérifier les configurations

Après construction des images, valider les trois configurations sans démarrer de serveur ni se connecter
aux services existants :

```bash
sudo docker run --rm --network none histae-site:production \
  caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
sudo docker run --rm --network none histae-dashboard:production \
  caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
sudo docker run --rm --network none histae-reverse-proxy:production \
  caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile
```

La commande Compose de la section production valide la quatrième configuration avec les certificats S3
montés. Le script reset valide également les trois images avant de recréer les services.

Ces commandes vérifient la configuration. Les contrôles HTTP après déploiement restent nécessaires pour
les routes, cookies, SSE et photos signées ; l’enrôlement WebAuthn nécessite un authentificateur réel.
Les scripts ponctuels de la campagne locale ont été supprimés après validation.

## Résultats locaux — 13 octobre 2026

| Contrôle | Résultat |
| --- | --- |
| Configurations des quatre serveurs Caddy | Valides |
| Trois images Docker finales | Construites ; runtime non-root et racine en lecture seule vérifiés |
| Site réel | Les quatre langues sont servies ; build Next.js et export statique réussis |
| Dashboard réel | Routes SPA et absence de cache vérifiées |
| API locale via dashboard et proxy construits | `authentication_required` et `admin_session_invalid` en 401 ; santé `ready` en 200 |
| Chaîne Cloudflare simulée | IP et protocole validés ; chaînes falsifiées rejetées ; Origin conservé |
| SSE, cookies et limites de corps | Réussis ; événement immédiat, attributs cookie conservés, limite 1 Mio |
| Logs de succès et d’échec | Marqueurs privés absents, champs contrôlés, UUID validés conservés |
| TLS upstream non approuvé | Rejeté |
| S3 privé temporaire | Upload/download signés réussis ; signatures/Host altérés et accès sans signature refusés ; objet intact après refus d’un corps trop grand |
| Manifests, script reset, diffs | Rendu Compose synthétique, syntaxe Bash et vérification des espaces réussis |

Ces vérifications n’ont pas redémarré la pile API locale ni déployé le serveur distant. Les conteneurs de test
et leurs volumes ont été retirés. Le test de passkey avec navigateur réel reste une vérification après déploiement.

### Documentation upstream

- [Proxys et transmission des en-têtes](https://caddyserver.com/docs/caddyfile/directives/reverse_proxy)
- [SPA et routes API](https://caddyserver.com/docs/caddyfile/patterns)
- [Logs et filtrage](https://caddyserver.com/docs/caddyfile/directives/log)
- [Options globales et administration](https://caddyserver.com/docs/caddyfile/options)
