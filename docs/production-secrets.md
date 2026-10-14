# Préparer les secrets et certificats de production

[Index](README.md) · [Déploiement](container-deployment.md#certificats-et-stockage) · [Roadmap](../roadmap-production.md)

Ces commandes répondent aux fichiers et valeurs manquants signalés par `prepare-production.sh`.
Elles sont à exécuter dans Bash sous Linux. Les commandes serveur partent de `histae-api-rust`,
sauf la génération de la CA qui se fait sur une machine d'administration distincte.
Les valeurs d'exemple doivent être remplacées. Aucun fichier existant n'est à supprimer.

Le dossier local `.secrets/` contient un modèle S3 avec des valeurs fictives et des indications pour
les certificats. Il est ignoré et exclu du contexte Docker : préparer les fichiers sur le serveur
ou les transférer explicitement. Les clés privées et secrets réels ne doivent jamais être committés.

## 1. Clés S3 et identité SeaweedFS

Générer deux valeurs dédiées et les reporter dans le `.env` API :

```bash
printf 'OBJECT_STORAGE_ACCESS_KEY=histae-production-%s\n' "$(openssl rand -hex 8)"
printf 'OBJECT_STORAGE_SECRET_KEY=%s\n' "$(openssl rand -hex 32)"
```

Ces commandes affichent volontairement les valeurs dans ton terminal pour les copier.
Ne pas remplacer des identifiants S3 déjà utilisés sans coordonner leur rotation.

Si aucun `s3.json` n'existe sur le serveur, le préparateur le créera à partir du `.env`.
Si tu utilises le modèle fourni, renseigner manuellement les **mêmes deux valeurs** dans
`.secrets/seaweedfs/s3.json` :

```json
{
  "identities": [{
    "name": "histae",
    "credentials": [{
      "accessKey": "REPLACE_OBJECT_STORAGE_ACCESS_KEY",
      "secretKey": "REPLACE_OBJECT_STORAGE_SECRET_KEY"
    }],
    "actions": ["Admin", "Read", "Write", "List", "Tagging"]
  }]
}
```

Les valeurs `REPLACE_…` restent fictives ; elles n'authentifient aucune requête réelle.
L'identité doit rester celle du serveur, jamais celle d'un client mobile.

Depuis la racine API, avec le compte propriétaire du `.env` :

```bash
umask 077
mkdir -p .secrets/seaweedfs
chmod 700 .secrets .secrets/seaweedfs
# Une fois le JSON complété :
chmod 444 .secrets/seaweedfs/s3.json
chmod 600 .env ../histae-reverse-proxy/.env
```

Le fichier monté peut être lu par le conteneur, tandis que les dossiers privés restreignent son
accès sur l'hôte. Les secrets de supervision déjà cohérents peuvent être conservés.

## 2. Token des métriques et mot de passe initial Grafana

Ces deux valeurs sont des secrets locaux aléatoires ; aucun compte fournisseur n'est nécessaire.
Depuis la racine de `histae-api-rust`, utiliser le préparateur existant :

```bash
bash scripts/prepare-observability-secrets.sh .env .secrets
```

| Fichier | Origine et usage |
| --- | --- |
| `.secrets/histae_metrics_token` | Même valeur que `METRICS_TOKEN` dans le `.env`. L'API la vérifie et Prometheus l'utilise comme bearer token pour collecter les métriques. |
| `.secrets/histae_grafana_admin_password` | Secret aléatoire indépendant, utilisé pour initialiser le compte Grafana `histae-admin`. Il n'a pas besoin d'une variable supplémentaire dans le `.env`. |

Le script effectue la préparation suivante sans afficher les secrets :

- Si `METRICS_TOKEN` existe et contient une valeur littérale valide d'au moins 32 caractères,
  il la réutilise et synchronise le fichier `histae_metrics_token` avec cette valeur.
- Si `METRICS_TOKEN` est absent ou vide, il génère 32 octets aléatoires encodés en **64 caractères
  hexadécimaux**, renseigne cette variable dans le `.env` et crée le fichier correspondant.
- Si le mot de passe Grafana est absent, il génère une autre valeur de 64 caractères hexadécimaux.
  Si son fichier existe avec au moins 32 caractères, son contenu est conservé.
- Il règle le `.env` en `0600`, le dossier `.secrets` en `0700` et les deux fichiers montés en `0444`.

Cette commande peut donc **compléter le `.env`** et resynchroniser le fichier du token. À l'inverse,
`histae-scripts/prepare-production.sh` ne modifie pas le `.env` et conserve les fichiers existants :
avec un token vide, ce dernier demande de le renseigner. Le reset appelle automatiquement le
préparateur de supervision avant le démarrage.

Pour choisir soi-même le token, `openssl rand -hex 32` permet d'en générer un : le copier dans
`METRICS_TOKEN`, puis lancer le préparateur pour écrire le même contenu dans son fichier.
Le mot de passe Grafana doit rester distinct de ce token et des autres secrets.

Si nécessaire, afficher leurs valeurs uniquement dans son terminal privé, depuis la racine API :

```bash
cat .secrets/histae_metrics_token
cat .secrets/histae_grafana_admin_password
```

Pour des fichiers appartenant à un autre utilisateur, utiliser un compte autorisé à les lire.
Dans la supervision intégrée, l'accès normal à Grafana passe par la passkey du superadmin et le
dashboard ; le formulaire de connexion Grafana est désactivé. Le mot de passe fichier sert à
l'initialisation du compte local. Modifier ce fichier ne réinitialise pas le mot de passe d'un
compte déjà enregistré dans le volume Grafana : il est défini au premier démarrage, selon la
[configuration Grafana](https://grafana.com/docs/grafana/latest/setup-grafana/configure-grafana/#admin_password).

Dans le compte rendu affiché, les deux fichiers sont déjà indiqués comme cohérents : leur
génération n'est donc pas un blocage. Après rotation d'un token utilisé, recréer l'API et Prometheus
pour recharger les valeurs ; le reset `--production --full` effectue cette recréation.

## 3. CA interne et certificats PostgreSQL/Redis

Les noms `postgres` et `redis` sont internes à Docker. Une CA privée commune permet aux clients
de vérifier ces noms. Une CA existante doit être conservée et réutilisée ; les commandes suivantes
concernent une première installation.

Sur une **machine d'administration hors du serveur**, avec OpenSSL 3+ :

```bash
(
  set -euo pipefail
  umask 077
  HISTAE_CA_WORKDIR=$(mktemp -d -t histae-internal-ca-XXXXXXXX)

  openssl req -x509 -newkey rsa:3072 -noenc -sha256 -days 3650 \
    -keyout "$HISTAE_CA_WORKDIR/ca.key" \
    -out "$HISTAE_CA_WORKDIR/ca.crt" \
    -subj '/CN=Histae Internal Root CA' \
    -addext 'basicConstraints=critical,CA:TRUE' \
    -addext 'keyUsage=critical,keyCertSign,cRLSign'

  for HISTAE_TLS_SERVICE in postgres redis; do
    mkdir "$HISTAE_CA_WORKDIR/$HISTAE_TLS_SERVICE"
    cat > "$HISTAE_CA_WORKDIR/$HISTAE_TLS_SERVICE/extensions.cnf" <<EOF
basicConstraints=critical,CA:FALSE
keyUsage=critical,digitalSignature,keyEncipherment
extendedKeyUsage=serverAuth
subjectAltName=DNS:$HISTAE_TLS_SERVICE
EOF
    openssl req -new -newkey rsa:3072 -noenc -sha256 \
      -keyout "$HISTAE_CA_WORKDIR/$HISTAE_TLS_SERVICE/server.key" \
      -out "$HISTAE_CA_WORKDIR/$HISTAE_TLS_SERVICE/server.csr" \
      -subj "/CN=$HISTAE_TLS_SERVICE"
    openssl x509 -req -sha256 -days 365 \
      -in "$HISTAE_CA_WORKDIR/$HISTAE_TLS_SERVICE/server.csr" \
      -CA "$HISTAE_CA_WORKDIR/ca.crt" -CAkey "$HISTAE_CA_WORKDIR/ca.key" \
      -set_serial "0x$(openssl rand -hex 16)" \
      -extfile "$HISTAE_CA_WORKDIR/$HISTAE_TLS_SERVICE/extensions.cnf" \
      -out "$HISTAE_CA_WORKDIR/$HISTAE_TLS_SERVICE/server.crt"
  done
  printf '\nFichiers générés dans : %s\n' "$HISTAE_CA_WORKDIR"
  printf 'Conserver ca.key hors du serveur, avec une sauvegarde privée pour le renouvellement.\n'
)
```

Transférer seulement `ca.crt`, `postgres/server.crt`, `postgres/server.key`, `redis/server.crt`
et `redis/server.key` sur le serveur, dans un dossier source privé. **Ne pas transférer `ca.key`.**
Copier aussi `ca.key` et `ca.crt` dans un emplacement privé durable hors serveur : le dossier de
génération est temporaire. La clé servira à signer les renouvellements ; les durées ci-dessus sont des choix initiaux à intégrer
à l'exploitation. Les certificats sont créés selon les options
[OpenSSL req](https://docs.openssl.org/3.0/man1/openssl-req/) et
[OpenSSL x509](https://docs.openssl.org/3.0/man1/openssl-x509/).

Installer ces cinq fichiers depuis la racine API. Adapter `HISTAE_TLS_SOURCE` et l'image PostgreSQL
à la livraison réellement configurée ; les images doivent être présentes localement :

```bash
(
  set -euo pipefail
  HISTAE_TLS_SOURCE='/chemin/vers/certificats-internes'
  HISTAE_TLS_PG_IMAGE='histae-postgres:18.6-postgis3.6.4'

  # Refuser le remplacement de certificats ou clés déjà installés.
  for HISTAE_TLS_FILE in postgres/ca.crt postgres/server.crt postgres/server.key redis/server.crt redis/server.key; do
    if [[ -e ".secrets/$HISTAE_TLS_FILE" || -L ".secrets/$HISTAE_TLS_FILE" ]]; then
      printf 'Fichier existant, installation initiale arrêtée : %s\n' "$HISTAE_TLS_FILE" >&2
      exit 1
    fi
  done

  HISTAE_TLS_PG_ID=$(sudo docker run --rm --pull=never --network none --read-only --cap-drop ALL \
    --entrypoint sh "$HISTAE_TLS_PG_IMAGE" -c 'printf "%s:%s" "$(id -u postgres)" "$(id -g postgres)"')
  HISTAE_TLS_REDIS_ID=$(sudo docker run --rm --pull=never --network none --read-only --cap-drop ALL \
    --entrypoint sh redis:7.4.2-alpine -c 'printf "%s:%s" "$(id -u redis)" "$(id -g redis)"')
  [[ "$HISTAE_TLS_PG_ID" =~ ^[1-9][0-9]*:[0-9]+$ ]]
  [[ "$HISTAE_TLS_REDIS_ID" =~ ^[1-9][0-9]*:[0-9]+$ ]]

  mkdir -p .secrets
  chmod 700 .secrets
  sudo install -d -m 0755 .secrets/postgres .secrets/redis
  sudo install -m 0644 "$HISTAE_TLS_SOURCE/ca.crt" .secrets/postgres/ca.crt
  sudo install -m 0644 "$HISTAE_TLS_SOURCE/postgres/server.crt" .secrets/postgres/server.crt
  sudo install -m 0640 -o 0 -g "${HISTAE_TLS_PG_ID#*:}" \
    "$HISTAE_TLS_SOURCE/postgres/server.key" .secrets/postgres/server.key
  sudo install -m 0644 "$HISTAE_TLS_SOURCE/redis/server.crt" .secrets/redis/server.crt
  sudo install -m 0640 -o 0 -g "${HISTAE_TLS_REDIS_ID#*:}" \
    "$HISTAE_TLS_SOURCE/redis/server.key" .secrets/redis/server.key
)
```

Les clés appartiennent à root et au groupe réel du service, en `0640`. Les dossiers montés sont
traversables et les certificats publics lisibles. Une installation Docker avec remappage des UID
nécessite d'adapter ces propriétaires.

## 4. Certificat HTTPS du stockage S3

Utiliser un certificat public pour le domaine exact de `HISTAE_STORAGE_HOST`. Les clients Rust
ne reçoivent actuellement que la CA interne PostgreSQL/Redis ; cette CA ne remplace pas le magasin
public utilisé pour S3. Les fichiers attendus sont une **chaîne complète** et sa clé privée.

Pour un domaine géré chez Cloudflare, le challenge DNS permet de délivrer ce certificat sans ouvrir
un port HTTP sur le serveur. Le plugin demande un token DNS `Zone:DNS:Edit` limité à la zone concernée,
distinct des identifiants du tunnel. Voir le
[plugin DNS Cloudflare](https://certbot-dns-cloudflare.readthedocs.io/en/stable/).

### Obtenir `dns_cloudflare_api_token`

Créer ce token dans le compte Cloudflare qui gère les DNS du domaine :

1. Ouvrir [Mon profil → API Tokens](https://dash.cloudflare.com/profile/api-tokens).
2. Cliquer sur **Create Token**, puis utiliser le modèle **Edit zone DNS**.
3. Nommer le token, par exemple `histae-certbot-s3`.
4. Garder la permission **Zone → DNS → Edit**.
5. Dans le formulaire du token, descendre sous **Permissions** jusqu'à **Zone Resources**
   (ressources de zone). Configurer les menus sur **Include → Specific zone → histae.com**
   pour un certificat couvrant `storage.histae.com`. Le dernier menu propose le domaine racine
   géré dans Cloudflare. Adapter ce domaine à la zone réelle de `HISTAE_STORAGE_HOST`.
6. Valider le récapitulatif, créer le token et copier sa valeur, affichée une seule fois.

Cette valeur remplace `REPLACE_WITH_SCOPED_CLOUDFLARE_DNS_TOKEN` dans le fichier INI ci-dessous.
Il s'agit du secret du token, distinct du Zone ID, de l'Account ID et du token de tunnel Cloudflared.
Procédure officielle : [créer un token Cloudflare](https://developers.cloudflare.com/fundamentals/api/get-started/create-token/).

Si le token a déjà été créé, rouvrir sa configuration en modification depuis la liste
[API Tokens](https://dash.cloudflare.com/profile/api-tokens) pour ajuster ses ressources de zone.

Exemple sur Debian, après installation de Certbot et de son plugin :

```bash
sudo apt-get update
sudo apt-get install certbot python3-certbot-dns-cloudflare
sudo install -d -m 0700 /etc/histae
sudo touch /etc/histae/certbot-cloudflare.ini
sudo chmod 600 /etc/histae/certbot-cloudflare.ini
sudoedit /etc/histae/certbot-cloudflare.ini
```

Contenu du fichier privé à compléter avec le token réel :

```ini
dns_cloudflare_api_token = REPLACE_WITH_SCOPED_CLOUDFLARE_DNS_TOKEN
```

Puis, toujours depuis la racine API, remplacer le domaine d'exemple par celui du `.env` :

```bash
HISTAE_S3_TLS_HOST='storage.histae.com'
sudo certbot certonly --dns-cloudflare \
  --dns-cloudflare-credentials /etc/histae/certbot-cloudflare.ini \
  --cert-name "$HISTAE_S3_TLS_HOST" -d "$HISTAE_S3_TLS_HOST"
```

Certbot place les fichiers sous `/etc/letsencrypt/live/<domaine>/`. Pour la première installation :

```bash
(
  set -euo pipefail
  HISTAE_S3_TLS_HOST='storage.histae.com'
  for HISTAE_S3_TLS_FILE in server.crt server.key; do
    if [[ -e ".secrets/storage-gateway/$HISTAE_S3_TLS_FILE" || -L ".secrets/storage-gateway/$HISTAE_S3_TLS_FILE" ]]; then
      printf 'Fichier existant, installation initiale arrêtée : %s\n' "$HISTAE_S3_TLS_FILE" >&2
      exit 1
    fi
  done
  mkdir -p .secrets
  chmod 700 .secrets
  sudo install -d -m 0755 .secrets/storage-gateway
  sudo install -m 0644 "/etc/letsencrypt/live/$HISTAE_S3_TLS_HOST/fullchain.pem" \
    .secrets/storage-gateway/server.crt
  sudo install -m 0640 -o 0 -g 101 "/etc/letsencrypt/live/$HISTAE_S3_TLS_HOST/privkey.pem" \
    .secrets/storage-gateway/server.key
)
```

Ces copies sont des fichiers ordinaires, pas des liens symboliques. Pour le renouvellement, prévoir
un `--deploy-hook` qui réinstalle la paire renouvelée puis recharge/recrée la passerelle : renouveler
seulement `/etc/letsencrypt/live/` ne met pas à jour les copies montées. Le hook de renouvellement
est décrit dans le [guide Certbot](https://eff-certbot.readthedocs.io/en/stable/using.html#renewing-certificates).
Son automatisation et le renouvellement de la CA/du TLS interne restent à mettre en place.

## 5. Valeurs qui viennent de Stripe

Ces cinq valeurs ne se génèrent pas avec OpenSSL :

| Variable | Origine |
| --- | --- |
| `STRIPE_SECRET_KEY` | Clé API secrète live `sk_live_…` du compte Stripe |
| `STRIPE_WEBHOOK_SECRET` | Secret de signature `whsec_…` du webhook `/api/billing/stripe/webhook` |
| `STRIPE_PREMIUM_PRODUCT_ID` | Produit Premium réel `prod_…` |
| `STRIPE_PREMIUM_MONTHLY_PRICE_ID` | Price récurrent mensuel réel `price_…` |
| `STRIPE_PREMIUM_ANNUAL_PRICE_ID` | Price récurrent annuel réel `price_…` distinct |

Les exemples `sk_live_…`, `whsec_…`, `prod_…` et `price_…` ne donnent aucun accès. L'API exige des
identifiants live en production ; garder les essais Stripe dans l'environnement de développement.
Références : [clés API](https://docs.stripe.com/keys),
[secrets des endpoints webhook](https://docs.stripe.com/api/webhook_endpoints).

## 6. Références d'images et reprise du préparateur

Dans le `.env` API, choisir des tags propres à la livraison qui sera construite, par exemple :

```dotenv
HISTAE_API_IMAGE=histae-api:release-20261014
HISTAE_PHOTO_MODERATION_IMAGE=histae-photo-moderation:release-20261014
```

Le reset construit les images avec ces tags. Un tag date la livraison mais reste modifiable ;
conserver également les identifiants exacts des images dans le compte rendu de déploiement.
Pour Cloudflared, récupérer le digest de l'image déjà téléchargée, en adaptant la référence à
celle de la machine :

```bash
sudo docker image inspect cloudflare/cloudflared:latest --format '{{index .RepoDigests 0}}'
```

Reporter la référence obtenue `cloudflare/cloudflared@sha256:…` dans
`HISTAE_CLOUDFLARED_IMAGE` du `.env` proxy ; elle doit correspondre à une image/version validée.

Après avoir complété les valeurs et installé les certificats, depuis `histae-scripts` :

```bash
sudo bash prepare-production.sh --check
```

Le contrôle s'exécute ici avec root pour lire les clés privées installées en `root:groupe-service`.
Les chemins par défaut utilisent les `.env` des deux dépôts ; avec un fichier externe, passer
explicitement `--env-file` et `--proxy-env-file` avec leurs chemins.

Si le contrôle ne peut pas détecter les UID/GID PostgreSQL/Redis, distinguer l'accès au moteur
Docker de la présence des images. `docker compose config` peut réussir sans accès au moteur.
Avec le préparateur mis à jour, le message indique lequel de ces prérequis manque. Au besoin,
depuis la racine API, construire PostgreSQL et télécharger Redis sans démarrer les services :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.production.yaml build postgres
sudo docker pull redis:7.4.2-alpine
```

Puis relancer `sudo bash prepare-production.sh --check` depuis `histae-scripts`.
Les identifiants Stripe peuvent être renseignés après cette préparation ; ils resteront signalés
une seule fois par variable manquante. Le validateur Rust exige toujours Stripe live au démarrage
de l'API avec `ENV=production`.

Les commandes de ce guide n'ont pas été exécutées sur le serveur. Les exemples locaux ne clôturent
aucune validation de production. Aucun test automatisé n'a été ajouté ni lancé pour ce guide.
