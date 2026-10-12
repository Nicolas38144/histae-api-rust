# Développement

[Index](README.md) · [Configuration](configuration.md) · [Tests](../tests/README.md)

## Prérequis Debian 13

Depuis un compte non-root ayant accès à `sudo` :

```bash
bash scripts/install-debian-13.sh
source "$HOME/.profile"
```

Le script installe les outils de compilation, Docker Engine/Compose, Rust 1.88.0, Node.js 22.22.1,
pnpm 10.28.2, `curl`, `jq` et `openssl`. Il vérifie le manifeste SHA-256 de Node et s’arrête
si des paquets Docker incompatibles sont déjà installés. Il ne crée ni secret ni base de données.
L’accès Docker reste à administrer ; les exemples emploient `sudo docker`.

Le chargement de données exige aussi Python 3. S’il manque :

```bash
sudo apt-get install python3
```

Le fichier [rust-toolchain.toml](../rust-toolchain.toml) fixe aussi `rustfmt`, Clippy, `rust-src` et
`rust-analyzer`. Dans VS Code, utiliser ce toolchain puis redémarrer rust-analyzer après installation des
composants. Aucun `rust-analyzer.cargo.features` spécifique n’est requis : WebAuthn est activé par défaut.

`webauthn-probe` est le nom historique de cette feature. OpenSSL est compilé depuis les sources vendored ;
un compilateur C et Perl restent nécessaires. `--no-default-features` permet de vérifier les outils sans
WebAuthn, mais l’API complète refuse de démarrer ainsi (`webauthn_feature_required`).

## Préparer l’environnement

Créer `.env` avec les valeurs de [configuration locale](configuration.md#configuration-locale-minimale).
Le préparateur `python3 scripts/prepare-postgres-roles-env.py .env` fournit les mots de passe des
trois rôles PostgreSQL dans ce fichier unique.
Pour une base déjà existante, effectuer [la conversion](postgres-roles.md#convertir-la-base-dev-existante-sans-supprimer-ses-donnees)
avant de recréer les conteneurs.
Il est ignoré par Git. L’API Rust sur l’hôte le charge automatiquement ; les variables déjà présentes
dans le terminal ont priorité. Compose applique ses propres overrides aux processus conteneurisés.

Les dépendances utilisent des ports hôte liés à loopback. `POSTGRES_PORT=5433` évite de concurrencer un
PostgreSQL natif ; les conteneurs se connectent néanmoins à `postgres:5432`.

## Choisir le mode d’exécution

### Tout dans Docker

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d --build --wait
bash scripts/smoke-api.sh
```

Le code est compilé dans l’image, sans montage des sources ni rechargement automatique.
Après une modification Rust, suivre la [procédure de mise à jour](container-deployment.md#mise-a-jour).

### Rust sur l’hôte, dépendances dans Docker

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  up -d --build --wait postgres redis object-storage photo-moderation
pnpm --dir tools/photo-codec install --frozen-lockfile
```

Si l’API et le worker Docker tournent déjà, les arrêter avant de lancer leurs équivalents natifs :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml stop api outbox-worker
```

Puis migrer et démarrer l’API native :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml run --rm --no-deps migrate
MAINTENANCE_MODE=disabled cargo run --locked --bin api
```

Dans un second terminal, démarrer le worker ; lancer la maintenance à la demande :

```bash
MAINTENANCE_MODE=worker cargo run --locked --bin outbox
# Dans un autre terminal, une seule passe :
MAINTENANCE_MODE=worker cargo run --locked --bin maintenance
```

Le rôle de chaque processus est décrit dans [Exploitation](operations.md). Pour le dashboard voisin,
installer ses dépendances et démarrer son serveur depuis `../histae-dashboard` :

```bash
cd ../histae-dashboard
pnpm install --frozen-lockfile
pnpm run dev
```

Ouvrir `http://localhost:5173` pour conserver l’origine WebAuthn locale. Le proxy Vite `/api` utilise
`VITE_API_PROXY_TARGET`, par défaut `http://localhost:8080`. Voir [Administration](administration.md).

## Tester les routes

- [Postman](../postman/README.md) : collections JSON française et anglaise, 104 requêtes chacune.
- [Accès LAN](container-deployment.md#acces-lan) : publier uniquement le port API nécessaire.
- [Tests automatisés](../tests/README.md) : choisir les suites selon les dépendances disponibles.
- [Smoke](../scripts/smoke-api.sh) : santé, guards, validation et route inexistante ; URL loopback uniquement.
- [Test de charge](load-testing.md) : pile dédiée et scénarios k6, avec ou sans tunnel Cloudflare.
- [Validation approfondie](load-testing-validation.md) : maintiens répétés, contrôles de sécurité sous charge et fixtures géographiques restaurables.

<a id="charger-des-donnees-fictives"></a>

## Charger des données fictives

La pile doit être démarrée avec `ENV=development`, le projet `histae-rust-dev` et la base `histae-dev`.
Dans `.env`, définir temporairement :

```dotenv
RATE_LIMIT_GLOBAL=20000
RATE_LIMIT_GLOBAL_WINDOW=1m
```

Puis appliquer ces valeurs et lancer le chargement :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml up -d --force-recreate api
sudo bash scripts/seed-dev-load.sh 5000
```

Le nombre doit être un multiple de 100, entre 5 000 et 10 000. Le script :

1. crée les comptes fictifs directement dans PostgreSQL, sans appeler l’OTP ;
2. place leurs profils près du superadmin, ou de Paris si aucune présence n’est disponible ;
3. utilise les routes de swipe, message, continuation et signalement avec des sessions temporaires ;
4. crée deux matchs par profil partagé avec ses voisins, soit autant de matchs que de profils ;
5. envoie 5–10 messages par participant et signale 4 % des matchs ;
6. avance les échéances fictives pour confirmer les continuations et supprime les sessions temporaires.

Une relance reprend les matchs et clés d’idempotence existants. Les comptes réels ne sont pas modifiés.
Après le chargement, remettre les limites HTTP habituelles dans `.env` et recréer l’API.

## Supprimer les comptes de test

```bash
sudo bash scripts/purge-dev-users.sh
```

Cette commande supprime **tous les comptes autres que le superadmin**, y compris les admins, et leurs données
PostgreSQL associées dans `histae-dev`. Elle refuse la suppression si les comptes ciblés ont encore des photos
dans le stockage objet ou une identité Stripe. Sauvegarder avant exécution si ces données doivent être récupérables.
Voir [Base de données](database.md) pour la sauvegarde et le test de restauration.

## Espace de compilation

Les profils de développement/test utilisent `debug = 1` et `incremental = false`. `target/` ne contient que
des artefacts reconstructibles. `cargo clean` libère cet espace, puis impose une recompilation, y compris
d’OpenSSL ; le réserver aux besoins d’espace plutôt qu’à chaque validation.
