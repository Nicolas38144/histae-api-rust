# Préparer les secrets et certificats de production

[Index](README.md) · [Déploiement](container-deployment.md#certificats-et-stockage) · [Roadmap](../roadmap-production.md)

Dernière mise à jour : **15 octobre 2026**. Ce guide rassemble la préparation des secrets,
les choix de domaines Cloudflare, les certificats, leur renouvellement et la procédure de lancement.

Ces commandes répondent aux fichiers et valeurs manquants signalés par `prepare-production.sh`.
Elles sont à exécuter dans Bash sous Linux. Les commandes serveur partent de `histae-api-rust`,
sauf la génération de la CA qui se fait sur une machine d'administration distincte.
Les valeurs d'exemple doivent être remplacées. Les fichiers de configuration, secrets et certificats
existants sont conservés.

Avec les chemins utilisés sur ton serveur, la racine API est :

```bash
cd ~/Projects/Histae/histae-api-rust
```

Les cinq dépôts doivent être voisins : `histae-api-rust`, `histae-dashboard`, `histae-site`,
`histae-reverse-proxy` et `histae-scripts`. Le `.env` API existant reste le fichier de configuration ;
il n'est pas nécessaire de créer un `.env.production.example` ni un deuxième `.env` pour cette procédure.
Un chemin comme `/histae-scripts/…` ne convient que si le dépôt existe réellement à la racine du serveur.

Le dossier local `.secrets/` contient un modèle S3 avec des valeurs fictives et des indications pour
les certificats. Il est ignoré et exclu du contexte Docker : préparer les fichiers sur le serveur
ou les transférer explicitement. Les clés privées et secrets réels ne doivent jamais être committés.

### Fichiers à préparer et permissions

```text
histae-api-rust/
  .env
  .secrets/
    histae_metrics_token
    histae_grafana_admin_password
    seaweedfs/s3.json
    postgres/ca.crt
    postgres/server.crt
    postgres/server.key
    redis/server.crt
    redis/server.key
    storage-gateway/server.crt
    storage-gateway/server.key
histae-reverse-proxy/
  .env
```

| Emplacement | Contenu / permissions retenues |
| --- | --- |
| `.env` API et `.env` proxy | Configuration réelle ; `0600`, accessibles au compte qui prépare la pile |
| `.secrets/` | Dossier privé sur l'hôte ; `0700` |
| `.secrets/seaweedfs/` | Dossier privé ; `0700` |
| `seaweedfs/s3.json` et les deux secrets de supervision | Fichiers ordinaires non vides ; `0444` pour leur lecture dans les conteneurs, sous les dossiers privés |
| `postgres/`, `redis/`, `storage-gateway/` | Dossiers montés ; `0755`, sans écriture pour le groupe ou les autres utilisateurs |
| `ca.crt` et les trois `server.crt` | Certificats publics PEM ; `0644` |
| `postgres/server.key` | Clé privée ; `root:groupe-PostgreSQL`, `0640`, ou propriétaire PostgreSQL en `0600` |
| `redis/server.key` | Clé privée ; `root:groupe-Redis`, `0640` dans la procédure ci-dessous |
| `storage-gateway/server.key` | Clé privée ; `root:101`, `0640`, pour la passerelle `101:101` |
| `/etc/histae/certbot-cloudflare.ini` | Token DNS Cloudflare ; `root:root`, `0600`, hors dépôt |
| Clé privée de la CA interne `ca.key` | Hors serveur, avec sauvegarde privée durable ; jamais montée dans Docker |

Les permissions des fichiers montés et celles de leurs dossiers jouent des rôles différents :
un secret en `0444` reste protégé sur l'hôte par son dossier parent en `0700`, tandis que le montage
du fichier permet au service de le lire. Les dossiers de certificats sont montés en entier : les
utilisateurs des conteneurs doivent pouvoir les traverser. Ne pas appliquer `chmod -R 600 .secrets`
ni `chmod -R 777` ; cela casserait respectivement l'accès aux dossiers ou la protection des clés.

Parcours : [S3](#1-clés-s3-et-identité-seaweedfs), [supervision](#2-token-des-métriques-et-mot-de-passe-initial-grafana),
[TLS interne](#3-ca-interne-et-certificats-postgresqlredis), [HTTPS S3 / Cloudflare](#4-certificat-https-du-stockage-s3),
[Stripe](#5-valeurs-qui-viennent-de-stripe), [contrôles](#6-références-dimages-et-reprise-du-préparateur),
[lancement](#7-lancer-la-pile-de-production-et-comprendre-docker-nuke).

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

Le nom du bucket est défini par `OBJECT_STORAGE_BUCKET`. Le service `storage-init` vérifie ou crée
ce bucket au démarrage ; il ne rend pas les photos publiques. L'API fournit les accès nécessaires
aux clients au moyen d'URL signées. Le JSON d'identité et le `.env` doivent rester cohérents : générer
une nouvelle clé dans un seul des deux fichiers ferait échouer l'authentification S3.

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

### 3.1. Générer la CA et les deux certificats initiaux

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

### 3.2. Installer les fichiers et utiliser les identités des images

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

### 3.3. Corriger les permissions signalées dans ton dernier contrôle

Le dernier compte rendu a détecté ces identités dans les images présentes sur ton serveur :

| Service | UID:GID détecté | Propriétaire de clé retenu |
| --- | --- | --- |
| PostgreSQL | `999:999` | `root:999`, mode `0640` |
| Redis | `999:1000` | `root:1000`, mode `0640` |
| Passerelle S3 | `101:101`, imposé par Compose | `root:101`, mode `0640` |

Le même UID `999` ne signifie pas que les groupes sont identiques. Redis doit lire sa clé avec le
groupe `1000`, PostgreSQL avec le groupe `999`. Ce sont les valeurs constatées pour les images
retenues ; les revérifier après un changement d'image, et adapter les commandes si elles changent.

Les messages « certificat et clé correspondent » et « chaîne et nom DNS vérifiés » confirment
la cohérence cryptographique des fichiers. Ils ne prouvent pas que le processus du conteneur peut
lire sa clé, ni que les fichiers sont protégés contre une modification par un autre utilisateur.
Il faut donc corriger les erreurs de permissions même si ces deux contrôles sont déjà `OK`.

Pour les fichiers existants et les identités ci-dessus, depuis la racine API :

```bash
sudo chmod 0755 .secrets/postgres .secrets/redis
sudo chmod 0644 .secrets/postgres/ca.crt .secrets/postgres/server.crt .secrets/redis/server.crt
sudo chown root:999 .secrets/postgres/server.key
sudo chown root:1000 .secrets/redis/server.key
sudo chmod 0640 .secrets/postgres/server.key .secrets/redis/server.key
```

Ces commandes modifient uniquement les droits et propriétaires ; elles ne régénèrent et ne
remplacent aucun certificat ou secret. `0755` retire l'écriture au groupe et aux autres utilisateurs
sur les dossiers ; `0644` fait de même pour les certificats. Avec `0640`, root peut lire/écrire la
clé, le groupe du service peut la lire, et les autres utilisateurs n'y ont pas accès.

Pour afficher les droits sans afficher les contenus privés :

```bash
sudo stat -c '%a %u:%g %n' \
  .secrets/postgres .secrets/redis \
  .secrets/postgres/ca.crt .secrets/postgres/server.crt .secrets/postgres/server.key \
  .secrets/redis/server.crt .secrets/redis/server.key
```

Puis, depuis le dépôt des scripts :

```bash
cd ../histae-scripts
sudo bash prepare-production.sh --check
```

Le compte `walagaine` n'appartient pas forcément aux groupes des services. Le contrôle avec `sudo`
peut lire les clés `root:groupe-service` sans devoir ouvrir leurs permissions aux autres utilisateurs.
Le préparateur ne corrige pas lui-même les permissions d'un fichier déjà présent.

### 3.4. Prévoir le renouvellement du TLS interne

Les commandes de première génération choisissent **365 jours** pour les certificats serveur et
**3 650 jours** pour la CA. Ce sont des durées choisies dans l'exemple ; contrôler les dates des
certificats réellement installés :

```bash
openssl x509 -in .secrets/postgres/ca.crt -noout -subject -enddate
openssl x509 -in .secrets/postgres/server.crt -noout -subject -enddate
openssl x509 -in .secrets/redis/server.crt -noout -subject -enddate
```

Pour renouveler un certificat serveur, réutiliser la CA existante sur la machine d'administration,
conserver le SAN `DNS:postgres` ou `DNS:redis`, puis installer la nouvelle paire avec les mêmes
propriétaires et permissions. Le bloc de première installation refuse volontairement les fichiers
existants : un renouvellement exige une procédure distincte avec sauvegarde de la paire précédente.
Prévoir le rechargement du service concerné et vérifier une nouvelle connexion TLS depuis les images
clientes. Certbot ne renouvelle pas ces certificats privés.

Renouveler ou remplacer la CA est une opération différente : les clients API, workers et Redis
doivent tous faire confiance à l'autorité qui signe les nouveaux certificats. Préparer une transition
coordonnée et recréer les clients après changement de leur CA montée. Régénérer une nouvelle racine
sans mettre à jour cette confiance interromprait les connexions. La sauvegarde de `ca.key` hors
serveur et la surveillance des expirations restent nécessaires.

## 4. Certificat HTTPS du stockage S3

Utiliser un certificat public pour le domaine exact de `HISTAE_STORAGE_HOST`. Les clients Rust
ne reçoivent actuellement que la CA interne PostgreSQL/Redis ; cette CA ne remplace pas le magasin
public utilisé pour S3. Les fichiers attendus sont une **chaîne complète** et sa clé privée.

Pour un domaine géré chez Cloudflare, le challenge DNS permet de délivrer ce certificat sans ouvrir
un port HTTP sur le serveur. Le plugin demande un token DNS `Zone:DNS:Edit` limité à la zone concernée,
distinct des identifiants du tunnel. Voir le
[plugin DNS Cloudflare](https://certbot-dns-cloudflare.readthedocs.io/en/stable/).

### 4.1. Obtenir `dns_cloudflare_api_token` : les deux formulaires Cloudflare

Le token permet à Certbot de créer puis retirer le TXT du challenge DNS. Il doit autoriser
**la modification DNS** sur la zone qui contient le domaine du certificat. Il n'est pas nécessaire
de lui accorder les droits du tunnel, de la facturation ou des autres produits Cloudflare.

#### Formulaire visible sur tes captures : jeton API du compte

Tes captures montrent **Gérer le compte → Jetons d'API du compte → Créer un jeton**. Ce formulaire
utilise des **Politiques d'autorisation**, et non les menus du formulaire de profil décrits plus bas.
Il peut servir à ce besoin ; Cloudflare indique la compatibilité DNS des
[tokens de compte](https://developers.cloudflare.com/fundamentals/api/get-started/account-owned-tokens/).

1. Garder un nom explicite, par exemple `histae-certbot-s3` ; le nom déjà saisi
   `histae-cloudflare-token` n'a aucune incidence sur les droits du token.
2. Dans **Politiques d'autorisation → Modifier la politique**, ouvrir le menu **Tous les domaines**
   visible juste au-dessus de la recherche des groupes d'autorisations. Sélectionner l'option
   permettant de limiter les domaines, puis choisir **`histae.com`**. Le menu était fermé sur les
   captures : le libellé exact de cette option n'a pas été observé.
3. Déplier **DNS & Zones** et choisir le droit qui permet de modifier les enregistrements DNS
   (`DNS Edit` / `DNS Write` selon les libellés). Le compteur `1/12` indique seulement qu'un droit
   est sélectionné ; vérifier lequel. Un droit de lecture seul ne permet pas le challenge.
4. Relire le récapitulatif : **DNS modifiable pour `histae.com`**, sans accès à tous les domaines.
   C'est ce périmètre final qui fait foi, même si les intitulés du formulaire changent.
5. Choisir une expiration compatible avec l'exploitation. Si le token expire, prévoir son
   remplacement dans le fichier Certbot avant le prochain renouvellement. Si un filtre IP est
   ajouté, il doit autoriser l'adresse publique de sortie de la machine qui exécute Certbot.
6. Créer le token et copier son secret, affiché une seule fois.

Le bandeau bleu de tes captures indique que les droits proposés dépendent de tes propres
autorisations dans le compte. Si `histae.com` ou la modification DNS n'est pas disponible,
vérifier le compte sélectionné et tes droits sur cette zone.

#### Autre formulaire : token utilisateur depuis le profil

Le chemin suivant explique les indications précédentes « Zone Resources » :

1. Ouvrir [Mon profil → API Tokens](https://dash.cloudflare.com/profile/api-tokens).
2. Cliquer sur **Create Token**, puis utiliser le modèle **Edit zone DNS**.
3. Nommer le token, par exemple `histae-certbot-s3`.
4. Garder la permission **Zone → DNS → Edit**.
5. Dans le formulaire du token, descendre sous **Permissions** jusqu'à **Zone Resources**
   (ressources de zone). Configurer les menus sur **Include → Specific zone → histae.com**
   pour un certificat couvrant `storage.histae.com`. Le dernier menu propose le domaine racine
   géré dans Cloudflare. Adapter ce domaine à la zone réelle de `HISTAE_STORAGE_HOST`.
6. Valider le récapitulatif, créer le token et copier sa valeur, affichée une seule fois.

Dans les deux cas, le secret obtenu remplace `REPLACE_WITH_SCOPED_CLOUDFLARE_DNS_TOKEN` dans le fichier INI ci-dessous.
Il s'agit du secret du token, distinct du Zone ID, de l'Account ID et du token de tunnel Cloudflared.
Procédure officielle : [créer un token Cloudflare](https://developers.cloudflare.com/fundamentals/api/get-started/create-token/).

Si le token a déjà été créé, modifier ses permissions et ressources depuis la liste où il a été
créé : **Jetons d'API du compte** ou **API Tokens du profil**. Ne pas créer un deuxième token
simplement pour changer de formulaire.

### 4.2. Où renseigner `histae.com` et `storage.histae.com`

Pour le **périmètre du token DNS**, sélectionner la zone **`histae.com`** suffit. La zone contient
les enregistrements du sous-domaine `storage.histae.com` ; il n'y a pas à ajouter ce sous-domaine
comme une seconde zone dans le token. En revanche, le **nom du service S3** est bien le sous-domaine.

| Emplacement | Valeur pour ce déploiement | Rôle |
| --- | --- | --- |
| Ressource autorisée du token Cloudflare | `histae.com` | Zone DNS sur laquelle Certbot peut agir |
| `.env` API, `HISTAE_STORAGE_HOST` | `storage.histae.com` | Nom public S3, sans `https://`, port ou chemin |
| Endpoint S3 des manifests production | `https://storage.histae.com` | Construit par Compose depuis `HISTAE_STORAGE_HOST` |
| Certbot, options `--cert-name` et `-d` ci-dessous | `storage.histae.com` | Nom de certificat et nom couvert |
| DNS Cloudflare et hostname de la route du tunnel | `storage.histae.com` | Accès des clients au stockage |
| Nom TLS d'origine du tunnel, `originServerName` | `storage.histae.com` | Vérification du certificat de la passerelle |

Dans le `.env` API existant, compléter ou corriger l'unique définition :

```dotenv
HISTAE_STORAGE_HOST=storage.histae.com
```

L'API résout ce même nom vers la passerelle sur le réseau backend Docker ; les clients externes
le résolvent via Cloudflare. Cette cohérence évite de changer le domaine d'une URL après sa signature.
La création du token et la délivrance du certificat ne créent pas automatiquement la route du
tunnel ni l'enregistrement DNS public.

### 4.3. Installer Certbot et enregistrer le token DNS

Sur Debian, installer Certbot et son plugin, puis préparer le fichier privé :

```bash
sudo apt-get update
sudo apt-get install certbot python3-certbot-dns-cloudflare
sudo install -d -m 0700 /etc/histae
sudo touch /etc/histae/certbot-cloudflare.ini
sudo chown root:root /etc/histae/certbot-cloudflare.ini
sudo chmod 600 /etc/histae/certbot-cloudflare.ini
sudoedit /etc/histae/certbot-cloudflare.ini
```

Contenu du fichier privé à compléter avec le token réel :

```ini
dns_cloudflare_api_token = REPLACE_WITH_SCOPED_CLOUDFLARE_DNS_TOKEN
```

Le nom du fichier est libre, mais le chemin passé à Certbot doit rester celui du fichier réel.
Le token DNS reste ici ; il ne remplace pas les credentials Cloudflared dans `/etc/cloudflared`
et n'a pas besoin d'une variable supplémentaire dans le `.env` API. Conserver le fichier privé
et le token utilisable pour les renouvellements, pas seulement pour la première émission.

### 4.4. Obtenir le certificat et installer la première paire

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

`server.crt` contient **`fullchain.pem`**, pas seulement le certificat feuille `cert.pem`.
`server.key` contient **`privkey.pem`**. Le groupe `101` correspond à la passerelle définie en
`101:101` dans `compose.production.yaml`. Le bloc refuse de remplacer une paire existante ;
ne pas le relancer tel quel pour un renouvellement.

### 4.5. Raccorder le DNS et le tunnel à la passerelle HTTPS

Créer ou vérifier l'enregistrement Cloudflare de `storage.histae.com` qui pointe vers le tunnel
retenu, ainsi que sa route S3. Pour le tunnel géré par fichier dans cette pile, adapter
`/etc/cloudflared/config.yml` — ou le dossier choisi par `HISTAE_CLOUDFLARED_DIR` dans le `.env` proxy.
Insérer cette entrée **dans la liste `ingress` existante, avant la règle finale**, en conservant
les routes du site, du dashboard et de l'API :

```yaml
ingress:
  # Autres routes existantes à conserver…
  - hostname: storage.histae.com
    service: https://storage-gateway:443
    originRequest:
      originServerName: storage.histae.com
      noTLSVerify: false
  # Garder la règle finale existante, par exemple :
  - service: http_status:404
```

Cloudflared contacte le service Docker `storage-gateway`, mais vérifie un certificat délivré
pour `storage.histae.com`. `originServerName` précise ce nom et `noTLSVerify: false` conserve la
vérification. Voir les [paramètres TLS d'origine](https://developers.cloudflare.com/cloudflare-one/networks/connectors/cloudflare-tunnel/configure-tunnels/origin-parameters/).

Conserver le `Host` public, le chemin et la query string des requêtes S3 signées ; vérifier aussi
les éventuelles règles Cloudflare qui les modifieraient. La route ne doit pas publier directement
les interfaces brutes SeaweedFS. Après une modification du fichier du tunnel, redémarrer son
conteneur pour charger la configuration ; le reset centralisé avec `--production --full` le recrée.

### 4.6. Comprendre les copies et le renouvellement

Il y a trois étapes distinctes entre Certbot et le certificat utilisé par S3 :

| Étape | Emplacement / état | Qui l'actualise ? |
| --- | --- | --- |
| Certificat géré par Certbot | `/etc/letsencrypt/live/storage.histae.com/fullchain.pem` et `privkey.pem` | Certbot |
| Copies installées pour Docker | `.secrets/storage-gateway/server.crt` et `server.key` | Installation initiale, puis procédure de déploiement du renouvellement |
| Certificat chargé par le serveur HTTPS | Caddy lit `/etc/caddy/tls/server.crt` et `server.key` depuis le dossier monté | Chargement initial, puis rechargement de Caddy |

Les fichiers de `.secrets/storage-gateway/` sont des **copies ordinaires**, pas des liens vers
`/etc/letsencrypt/live/`. Si Certbot renouvelle le certificat, ses chemins `live/` désignent la
nouvelle version, mais les copies conservent l'ancienne. Docker n'effectue aucune copie automatique.
Même après remplacement des copies, Caddy doit recharger la paire qu'il garde en mémoire.

Un exemple : après renouvellement, Certbot peut avoir un certificat qui expire plus tard,
alors que Caddy continue à servir l'ancien. Le renouvellement du fichier source a réussi, mais le
déploiement du certificat servi reste à faire. Le contrôle des fichiers et le contrôle du service
réellement joignable répondent donc à deux questions différentes.

Ne pas remplacer ces copies par des liens symboliques : le préparateur les refuse, et le conteneur
ne monte pas l'arborescence Certbot vers laquelle ces liens pointeraient.

### 4.7. Prévoir le déploiement après renouvellement

Certbot propose un **`--deploy-hook`**, exécuté après une émission ou un renouvellement réussi.
Il peut aussi découvrir un exécutable dans `/etc/letsencrypt/renewal-hooks/deploy/`.
Les variables `RENEWED_LINEAGE` et `RENEWED_DOMAINS` permettent d'identifier la paire et les domaines
concernés. Voir le [guide de renouvellement Certbot](https://eff-certbot.readthedocs.io/en/stable/using.html#renewing-certificates).

Pour cette pile, le hook à mettre en place doit :

1. Ne traiter que la lignée du certificat S3 attendu, sans toucher aux autres certificats du serveur.
2. Utiliser un chemin absolu vers `histae-api-rust`, car son dossier courant ne sera pas forcément le dépôt.
3. Vérifier les nouveaux fichiers : PEM, paire correspondante, nom DNS, chaîne et expiration.
4. Conserver une sauvegarde privée de la paire précédente et préparer les deux nouveaux fichiers
   avant leur installation, avec `server.crt` en `0644` et `server.key` en `root:101` / `0640`.
5. Installer **les deux fichiers**, puis recharger la passerelle ; ne pas recharger entre les copies.
6. Confirmer le chargement et une nouvelle connexion HTTPS vers l'origine S3. Un signal envoyé
   avec succès ne prouve pas, à lui seul, que le nouveau certificat est servi.
7. Remonter tout échec de copie, de permission ou de chargement dans la supervision ; conserver
   une possibilité de revenir à la paire précédente.

Les copies se réinstallent avec `install` comme lors de l'installation initiale, mais sans le
refus des fichiers existants, **après vérification et sauvegarde**. Le hook est exécuté avec un
compte autorisé à lire les fichiers Certbot, attribuer les propriétaires et agir sur Docker.

La passerelle actuelle démarre par `caddy run --config … --adapter caddyfile`, sans `--resume`,
et son Caddyfile contient `admin off`. La commande `caddy reload` dépend de cette API d'administration.
Pour ce démarrage par fichier, Caddy permet un rechargement par **`SIGUSR1`**, y compris des fichiers
TLS dépendants : [signaux Caddy](https://caddyserver.com/docs/command-line#signals).

Après installation complète de la paire renouvelée, depuis la racine API :

```bash
sudo docker compose --env-file .env \
  -f compose.yaml -f compose.production.yaml \
  kill -s SIGUSR1 storage-gateway
```

Ici `kill -s SIGUSR1` envoie le signal de rechargement à Caddy ; il ne demande pas l'arrêt du service.
Ce choix dépend du mode de démarrage actuel : le revoir si Caddy passe à une configuration par API
ou à `--resume`. En cas de besoin, le redémarrage ciblé suivant relit aussi les certificats, avec
une brève interruption de la passerelle :

```bash
sudo docker compose --env-file .env \
  -f compose.yaml -f compose.production.yaml \
  restart storage-gateway
```

Le renouvellement d'un certificat S3 ne nécessite pas de reconstruire toute la pile. Les secrets
de signature S3 restent les mêmes ; seule la paire TLS est renouvelée.

### 4.8. Planifier le renouvellement et distinguer ce qui reste à installer

Le planificateur lance `certbot renew` périodiquement ; le deploy-hook applique la nouvelle paire
après succès. Ce sont deux responsabilités distinctes. Selon l'installation, un timer peut déjà
être présent : vérifier l'existant avant d'ajouter une deuxième planification.

```bash
sudo systemctl list-timers --all | rg -i certbot
sudo systemctl status certbot.timer
```

Si `rg` n'est pas installé, utiliser `grep -i certbot`. L'absence de `certbot.timer` ne prouve pas
l'absence d'une autre planification : vérifier aussi le mode d'installation et les tâches cron.
Un déclenchement manuel utilise :

```bash
sudo certbot renew
```

Cette commande peut ne rien renouveler si aucun certificat n'en a encore besoin. Elle ne met à jour
les copies Docker que si le hook a été installé et s'est exécuté correctement. Éviter de forcer des
émissions répétées simplement pour avancer cette préparation.

**Ce guide décrit le raccordement ; il n'installe pas le hook ni le timer sur le serveur.**
L'automatisation de la copie/recharge S3 et le renouvellement du TLS interne restent à réaliser
et à confirmer sur la cible. Vérifier le certificat de l'origine : le certificat présenté par
Cloudflare au navigateur ne suffit pas à prouver le renouvellement de celui de la passerelle.

## 5. Valeurs qui viennent de Stripe

Stripe a été volontairement reporté à la suite de la préparation. Les secrets S3, la supervision,
les permissions et les certificats peuvent être préparés dès maintenant. En revanche, l'API actuelle
ne propose pas de mode production sans Stripe : ce report laisse un blocage de démarrage ouvert.

Ces quatre valeurs ne se génèrent pas avec OpenSSL :

| Variable | Origine |
| --- | --- |
| `STRIPE_SECRET_KEY` | Clé API secrète live `sk_live_…` du compte Stripe |
| `STRIPE_WEBHOOK_SECRET` | Secret de signature `whsec_…` du webhook `/api/billing/stripe/webhook` |
| `STRIPE_PREMIUM_MONTHLY_PRICE_ID` | Price récurrent mensuel réel `price_…` |
| `STRIPE_PREMIUM_ANNUAL_PRICE_ID` | Price récurrent annuel réel `price_…` distinct |

Les exemples `sk_live_…`, `whsec_…` et `price_…` ne donnent aucun accès. L'API exige des
identifiants live en production ; garder les essais Stripe dans l'environnement de développement.
Références : [clés API](https://docs.stripe.com/keys),
[secrets des endpoints webhook](https://docs.stripe.com/api/webhook_endpoints).

Configurer les quatre valeurs **dans le même environnement live** : les prix mensuel et annuel
doivent être récurrents, actifs et distincts. Le secret du webhook doit
être celui de l'endpoint réel du serveur, pas un secret d'écoute locale. Conserver aussi les
URL de retour Stripe HTTPS déjà présentes dans le `.env`, dont `STRIPE_CHECKOUT_SUCCESS_URL`
avec son paramètre `{CHECKOUT_SESSION_ID}`.

Les variables `STRIPE_PREMIUM_MONTHLY_PRICE_ID` et `STRIPE_PREMIUM_ANNUAL_PRICE_ID` contiennent des
identifiants de **prix** `price_…`. Elles ne contiennent
pas les identifiants `sub_…` des abonnements clients. Aucun identifiant produit n'est nécessaire
dans le `.env` : les webhooks et la réconciliation reconnaissent Premium par les deux prix autorisés.
Chez Stripe, le produit Histae Premium reste associé à ses tarifs mensuel et annuel.

`BILLING_PROVIDER=stripe` est requis par la configuration Rust en production. Si une ancienne
valeur de développement le désactive explicitement, la corriger. Ajouter des chaînes fictives
qui ont seulement le bon préfixe ne raccorde pas Stripe : la validation locale de syntaxe ne
prouve pas l'existence du produit, l'accord des prix ou la réception effective des webhooks.
Ces parcours restent suivis dans [PROD-08](../roadmap-production.md#prod-08--stripe-et-premium).

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

Si les identités ont déjà été vérifiées dans les images exactes de cette livraison, le contrôle
accepte aussi les valeurs explicites. Avec celles de ton compte rendu :

```bash
sudo bash prepare-production.sh --check --postgres-user 999:999 --redis-user 999:1000
```

Cette option déclare une identité connue ; elle ne prouve pas qu'une nouvelle image utilise
encore les mêmes IDs. Elle n'attribue aucun propriétaire et ne corrige aucun droit.
Le script ne construit ni ne télécharge les images, y compris sans `--check`.

### Lire le bilan et reprendre après une erreur

| Message / situation | Signification et action |
| --- | --- |
| Valeur réelle à renseigner | Compléter la variable existante du `.env` ; ne pas conserver `REPLACE_…` ou une valeur de démonstration |
| `.env` : maximum `0600` | Retirer l'accès au groupe et aux autres utilisateurs avec `chmod 600` sur le fichier concerné |
| Fichier absent, vide ou non régulier | Fournir le fichier exact demandé ; un README ou un dossier portant ce nom ne le remplace pas |
| UID/GID non vérifiés / Docker inaccessible | Rendre le moteur accessible, au besoin avec `sudo`, puis relancer le contrôle |
| UID/GID non vérifiés / image absente | Construire PostgreSQL ou télécharger Redis comme ci-dessus, sans démarrer les services |
| Dossier/certificat modifiable par le groupe ou les autres | Appliquer les droits de la section 3.3 ; ne pas régénérer le certificat pour une erreur de droits |
| Clé privée incompatible avec UID:GID | Corriger le propriétaire/groupe et le mode de la clé pour le service réel |
| Certificat et clé correspondent | La paire est cohérente ; cela ne dispense pas des contrôles de droits, chaîne, DNS et expiration |
| Chaîne et nom DNS vérifiés | Les fichiers satisfont le contrôle TLS sur l'hôte ; vérifier ensuite les connexions depuis les images clientes |
| Manifests acceptés par Compose | La configuration Compose est lisible ; cela ne prouve pas l'accès au moteur ou la disponibilité des fournisseurs |

Le préparateur retourne `1` tant qu'un point reste à corriger, même s'il a pu créer des fichiers
manquants. Un bilan positif retourne `0`. En mode `--check`, il n'écrit aucun fichier ; il peut
lancer les conteneurs éphémères nécessaires pour lire les identités des images locales.
La marge de validité des certificats est de 30 jours par défaut ; un certificat reconnu mais trop
proche de l'expiration reste à renouveler.

### État confirmé par le dernier compte rendu partagé

- Les manifests production sont acceptés.
- Le JSON S3, le token des métriques et le mot de passe initial Grafana sont cohérents.
- Les trois paires certificat/clé et leurs contrôles de chaîne / nom DNS sont indiqués `OK`.
- **Quatre erreurs de permissions** subsistent : deux pour PostgreSQL, deux pour Redis.
- Le bilan partagé avant la simplification Stripe comportait **cinq valeurs Stripe** manquantes,
  soit **neuf points** avec les erreurs de permissions.

Les commandes de correction des permissions ont été fournies, mais aucun nouveau bilan ne
confirme encore leur résolution. La configuration actuelle ne demande plus l'identifiant produit :
elle attend les quatre valeurs Stripe décrites en section 5. Conserver les deux entrées de prix dans
le `.env` du serveur avec leurs valeurs `price_…`, puis relancer le contrôle pour obtenir le bilan
actuel. Les sorties précédentes ne constituent pas un audit indépendant du serveur ni des fournisseurs.

## 7. Lancer la pile de production et comprendre `docker-nuke`

### 7.1. Procédure normale après configuration de Stripe

Depuis le dépôt des scripts, dans les chemins utilisés sur le serveur :

```bash
cd ~/Projects/Histae/histae-scripts &&
  sudo bash prepare-production.sh --check &&
  sudo bash reset-docker-compose.sh --production
```

Les `&&` empêchent de lancer l'étape suivante si le changement de dossier ou le contrôle échoue.
Le contrôle et le reset sont deux étapes distinctes : le reset n'appelle pas le préparateur
production complet à ta place. **Passer explicitement `--production`** : sans option, le reset
sélectionne `dev`, sauf si `HISTAE_DEPLOYMENT_MODE=production` est déjà défini.

Le reset centralisé sélectionne `compose.yaml`, `compose.production.yaml` et
`compose.production-observability.yaml` pour l'API, ainsi que le Compose du proxy. Il :

1. Vérifie les versions Docker / Compose et prépare les secrets de supervision.
2. Contrôle les manifests, les fichiers montés et l'accord des réseaux entre API et proxy.
3. Construit les images du site, dashboard, reverse proxy et des services applicatifs avant
   de modifier les services en cours d'exécution.
4. Valide Caddy, le tunnel Cloudflare et la configuration / paire TLS de la passerelle S3.
5. Crée le backend production s'il manque ; la pile proxy crée les réseaux edge et monitoring.
6. Démarre le proxy, le site, le dashboard et Cloudflared, puis les services API de production
   et la supervision. Les dépendances imposent la disponibilité PostgreSQL/Redis, les migrations,
   l'initialisation du bucket et la modération avant le démarrage de l'API et du worker outbox.
7. Exécute une passe de maintenance, contrôle la readiness depuis Caddy, vérifie Prometheus et
   les règles d'alerte, puis affiche les services et la durée.

Les trois réseaux backend, edge et monitoring doivent correspondre à la configuration retenue,
avec les noms communs cohérents entre dépôts. Les fichiers du tunnel, ses credentials, les secrets
et les certificats doivent déjà exister. Prévoir assez de RAM et d'espace disque pour les images,
les builds et les données ; le dimensionnement actuel est décrit dans le
[guide de déploiement](container-deployment.md#ressources).

Pour forcer la recréation d'une pile existante après un changement de valeurs ou de fichiers montés :

```bash
sudo bash reset-docker-compose.sh --production --full
```

`--full` signifie **recréer les conteneurs** ; les volumes persistants sont conservés.
`--check` sur le reset valide les fichiers et les images déjà disponibles, sans construire ni
redémarrer. Ce contrôle peut donc échouer sur une machine neuve dont les images ne sont pas encore
présentes ; ce n'est pas la commande à imposer avant leur première construction.

### 7.2. Utiliser `docker-nuke.sh` pour repartir des seules données dev

Le nom « reset » du script de démarrage ne signifie pas effacer la base. Il reconstruit et relance
la pile en gardant les données. **Configurer Stripe ne nécessite aucune suppression de volumes.**

Le script `docker-nuke.sh`, lui, demande de saisir `NUKE`, puis supprime sur le moteur Docker ciblé :

- tous les conteneurs, y compris ceux des autres projets ;
- toutes les images, pas seulement les images Histae ;
- tous les volumes, donc les bases et les objets persistés ;
- les réseaux inutilisés et le cache de build.

Pour Histae, cela effacerait notamment les comptes, les rôles et les passkeys enregistrées en
PostgreSQL, les photos SeaweedFS, les données Grafana et l'historique de supervision. Ton utilisateur
déjà créé serait perdu si sa base se trouve dans l'un de ces volumes. Les clés de son authentificateur
ne suffiraient pas à recréer les enregistrements WebAuthn supprimés de la base.

Les fichiers hôtes montés comme `.env`, `.secrets/`, `/etc/cloudflared` et `/etc/letsencrypt`
ne sont pas supprimés par ce script. **Les conserver ne restaure pas les données des volumes.**
Une reconstruction après nuke ne fonctionnerait que si tous les prérequis du lancement sont encore
présents et si les images peuvent être reconstruites / téléchargées. Il faudrait aussi réinitialiser
les données et l'administration voulues. Si les images PostgreSQL/Redis ont été effacées, le
préparateur ne pourra plus lire leurs UID/GID avant leur reconstruction / téléchargement.

Tu as précisé que le futur serveur production ne contient actuellement que les conteneurs et
données de développement, et que leur effacement est accepté. **Dans ce cas, tu peux utiliser le
nuke pour repartir de zéro**, puis lancer toute la pile production avec le reset centralisé.

Après avoir renseigné Stripe et corrigé les permissions, la séquence complète est :

```bash
cd ~/Projects/Histae/histae-scripts &&
  sudo bash prepare-production.sh --check &&
  sudo bash docker-nuke.sh &&
  sudo bash reset-docker-compose.sh --production
```

Saisir `NUKE` lorsque le script le demande. Les `&&` arrêtent l'enchaînement si une étape échoue
ou si le nuke est annulé. Le contrôle production est placé **avant** l'effacement, quand les images
PostgreSQL et Redis sont encore disponibles pour la lecture de leurs UID/GID. Après le nuke,
le reset reconstruit les images applicatives/PostgreSQL, télécharge les autres images nécessaires
et recrée les réseaux et volumes. Cette première reconstruction sera plus longue, car le cache
Docker a lui aussi été effacé. `--full` n'est pas nécessaire lorsque les conteneurs ont été supprimés.

Les mêmes `.env`, secrets et certificats hôtes restent utilisés ; il ne faut pas les régénérer
uniquement parce que les volumes dev ont été effacés. La nouvelle base production sera initialisée
par les migrations, et le stockage par `storage-init`. Ensuite, créer le premier compte en
production et suivre la section 7.3 pour son rôle superadmin et sa passkey.

Une fois des données de production à conserver présentes, utiliser le reset normal pour les
mises à jour. Le nuke reste une opération de remise à zéro explicite ; il n'est pas nécessaire
au renouvellement des certificats ou à un changement de configuration.

### 7.3. Attention au compte existant et à la séparation dev / production

La production utilise un projet Compose et des volumes PostgreSQL / stockage distincts du
développement. Si le seul utilisateur a été créé dans la base dev, il n'est pas automatiquement
présent dans la base production, **même sans nuke**. Le reset applique les migrations et initialise
le bucket ; il ne crée pas un utilisateur ni une passkey administrateur.

Après démarrage, créer ou retrouver le compte admissible dans la base production, puis utiliser
le script existant depuis `histae-scripts` :

```bash
sudo bash bootstrap-superadmin.sh --production
```

Il exige exactement un compte non supprimé admissible, lui attribue le rôle `superadmin` et
affiche un jeton temporaire. L'enregistrement de la passkey se termine dans le navigateur du
dashboard, avec ton authentificateur ; le script ne peut pas créer cette clé à sa place.
Conserver le jeton privé et ajouter ensuite une deuxième passkey pour la récupération.
Voir le [guide du bootstrap](../../histae-scripts/README.md#initialisation-du-superadmin).

### 7.4. Ce que Stripe configuré permet, et ce qui reste à confirmer

**Oui, le reset avec `--production` est prévu pour lancer l'ensemble de la pile**, lorsque les
fichiers, permissions, images, réseaux et autres valeurs exigées sont corrects. Stripe configuré
lève le blocage Stripe ; il ne garantit pas à lui seul le succès du lancement ou l'ouverture publique.

Un contrôle local du `.env` ne prouve pas que les comptes fournisseurs sont actifs. Les binaires
Rust valident aussi leur configuration de production, notamment Sweego, WebAuthn, les secrets
distincts et les documents / consentements versionnés. Une valeur de démonstration ou un raccordement
manquant peut encore arrêter un service ou empêcher un parcours réel après le démarrage.

Avant d'accueillir des utilisateurs, conserver dans la roadmap la confirmation de :

- la connectivité TLS depuis les images clientes vers PostgreSQL, Redis et S3 ;
- les domaines publics et les routes tunnel, notamment les photos par URL signée ;
- l'envoi des OTP Sweego et ses callbacks, ainsi que les paiements et webhooks Stripe ;
- l'administration production et les passkeys, la modération et les traitements outbox ;
- la maintenance planifiée et les alertes effectivement reçues ;
- les sauvegardes hors machine et une restauration PostgreSQL + stockage cohérente ;
- le renouvellement S3 complet, le TLS interne, les textes publiés et les parcours clients réels.

Le reset exécute une passe de maintenance ; il n'installe ni n'active le timer systemd permanent.
Il ne configure pas les fournisseurs, les routes Cloudflare, les sauvegardes ou le hook Certbot.
Les tâches détaillées restent dans la [roadmap production](../roadmap-production.md).

Ce document a été rapproché des scripts et manifests du projet et des sorties que tu as partagées.
Il ne constitue pas une preuve de déploiement ou de renouvellement réussi sur ton serveur.
Les vérifications sur la cible restent à consigner dans la roadmap, sans secrets ni données personnelles.
