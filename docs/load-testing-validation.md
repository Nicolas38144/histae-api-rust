# Validation approfondie de la charge

[Guide initial](load-testing.md) · [Résultats](load-testing-results.md) · [Contrat HTTP](http-contract.md)

Ce protocole complète le mélange de lectures authentifiées avec des répétitions, des refus
d’autorisation et des données géographiques différentes. Il n’annonce pas de capacité production.
Il utilise une population fictive existante ; aucune route OTP ni livraison fournisseur n’est nécessaire.

## Objectifs et ordre

| Étape | Profil | Condition de poursuite |
| --- | --- | --- |
| Confirmation | Trois maintiens de cinq minutes à 300 RPS, chacun avec ses rampes | Chaque répétition valide les seuils globaux et par route |
| Capacité | 350, puis 400, puis éventuellement 450/500 RPS ; cinq minutes chacun | Le palier précédent valide ses seuils et aucune panne n’est observée |
| Sécurité | Lectures indépendantes à 100 RPS et petites vagues de requêtes concurrentes ciblées | Chaque réponse positive ou refus correspond au contrat attendu |
| Feed | Cinq variantes, 50 RPS pendant une minute chacune | Résultats corrects, aucune perte de requête ni erreur |
| Serveur cible | Même population et ressources, générateur distinct ; privé puis Cloudflare | Une pile distante et un tunnel dédiés existent |

Seuils de latence proposés : **p95 < 750 ms et p99 < 1 000 ms**, globalement et par route.
Pour ces validations locales, exiger également zéro erreur inattendue, zéro réponse métier invalide
et zéro itération perdue. Les refus intentionnels du contrôle de sécurité sont évalués séparément.

Un pic isolé et un percentile décrivent des choses différentes. Conserver aussi le maximum et les
points bruts ; distinguer les rampes des maintiens. Le débit de réponses achevées dans une fenêtre
peut différer légèrement de la cible à ses frontières, même lorsque toutes les requêtes prévues
ont été lancées.

Arrêter toute la montée en cas d’arrêt, redémarrage, OOM ou état `unhealthy` d’un conteneur surveillé.
Les quotas habituels, authentifications et consentements de l’API principale restent actifs.
Le test d’endurance de trente minutes et le scénario avec écritures massives sont des validations
distinctes ; ils ne sont pas exécutés par ce protocole court.

## API temporaire locale

Depuis la racine, avec une pile dev saine et les données fictives déjà créées :

```bash
install -d -m 700 .load-test
```

[compose.load-test.yaml](../compose.load-test.yaml) ajoute `load-api` et `load-redis` au projet dev.
L’API est publiée uniquement sur `127.0.0.1:18081`, ses métriques authentifiées sur
`127.0.0.1:19091`. Elle hérite du durcissement, des limites CPU/RAM et de `nofile` de l’API principale,
avec un pool de 30. Le Redis, la clé JWT et le jeton de métriques sont distincts.
La base et le stockage dev sont partagés : les outils limitent leurs modifications aux UUID fictifs.

Préparer le fichier privé depuis l’environnement du conteneur dev existant. Ce code refuse une
autre cible et un fichier existant ; les versions légales sont lues uniquement sur les comptes fictifs
actifs, sans modifier leurs consentements. Il refuse des versions ambiguës.

```bash
sudo python3 - <<'PY'
import json, os, secrets, subprocess
from pathlib import Path

api, pg = json.loads(subprocess.check_output([
    'docker', 'inspect', 'histae-rust-dev-api-1', 'histae-rust-dev-postgres-1'
], text=True))
env = dict(x.split('=', 1) for x in api['Config']['Env'])
database = dict(x.split('=', 1) for x in pg['Config']['Env'])
assert env['ENV'] == 'development' and database['POSTGRES_DB'] == 'histae-dev'
assert all(c['Config']['Labels']['com.docker.compose.project'] == 'histae-rust-dev'
           for c in [api, pg])
sql = """SELECT consent_type, document_version
FROM user_consent c JOIN user_account a ON a.user_id=c.user_id
WHERE c.granted AND c.withdrawn_at IS NULL AND a.role='user'
  AND a.deleted_at IS NULL AND NOT a.is_banned
  AND a.user_id IN (
    SELECT uuid_generate_v5('2fc9a04d-bca3-4fa7-b6a1-a27736103699'::uuid,
                           'histae-load-user-' || i)
    FROM generate_series(1,10000) i
  )
GROUP BY 1,2"""
rows = subprocess.check_output([
    'docker', 'exec', pg['Id'], 'psql', '-X', '-qAt', '-F', '\t',
    '-v', 'ON_ERROR_STOP=1', '-U', database['POSTGRES_USER'],
    '-d', database['POSTGRES_DB'], '-c', sql
], text=True)
versions = {}
for row in rows.splitlines():
    kind, version = row.split('\t')
    assert kind not in versions, 'Ambiguous synthetic consent versions'
    versions[kind] = version
mapping = {
    'terms_of_service_acceptance': 'TERMS_OF_SERVICE_VERSION',
    'privacy_notice_acknowledgement': 'PRIVACY_POLICY_VERSION',
    'sensitive_data_consent': 'SENSITIVE_DATA_CONSENT_VERSION',
    'location_consent': 'LOCATION_CONSENT_VERSION',
}
assert set(versions) == set(mapping)
for key in list(env):
    if key.startswith(('JWT_', 'SWEEGO_', 'STRIPE_', 'FIREBASE_')):
        del env[key]
env.update(JWT_SECRET=secrets.token_hex(32), JWT_ACTIVE_KID='load-validation',
           JWT_ACCESS_TTL='60m', JWT_REFRESH_TTL='168h',
           SMS_PROVIDER='disabled', BILLING_PROVIDER='disabled', PUSH_PROVIDER='disabled',
           METRICS_ENABLED='true', METRICS_HOST='0.0.0.0',
           METRICS_TOKEN=secrets.token_hex(32), RATE_LIMIT_GLOBAL='120000',
           RATE_LIMIT_GLOBAL_WINDOW='1m', MAINTENANCE_MODE='disabled')
env.update({mapping[k]: v for k, v in versions.items()})
assert all('\n' not in v and '\r' not in v for v in env.values())
path = Path('.load-test/runtime.env')
with os.fdopen(os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as target:
    target.write(''.join(f'{k}={v}\n' for k, v in sorted(env.items())))
print('Private development load environment created.')
PY

sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  -f compose.load-test.yaml config --quiet
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  -f compose.load-test.yaml up -d --no-deps --wait load-redis load-api
sudo python3 scripts/prepare-dev-load-test.py 5000 --api-service load-api \
  --compose-file compose.load-test.yaml --fixture .load-test/accounts.json
sudo chown "$(id -u):$(id -g)" .load-test/accounts.json
```

Le quota global élevé est propre à l’API temporaire. Les quotas individuels et les règles métier
restent appliqués. Utiliser le k6 installé selon le guide initial ; les exemples suivants supposent
son binaire natif dans `PATH`. Les commandes qui pilotent Docker nécessitent son accès autorisé.

## Répétitions et paliers

Exemple d’un maintien, à répéter trois fois avec des fichiers de résultat distincts :

```bash
BASE_URL=http://127.0.0.1:18081 PROFILE=soak SOAK_MINUTES=5 \
TARGET_RPS=300 PREALLOCATED_VUS=1000 P95_MS=750 P99_MS=1000 \
k6 run --no-usage-report --summary-export=.load-test/confirm-300-1.json \
  scripts/load-test.js
```

La valeur par défaut de `ACCOUNTS_FILE` convient au dossier ci-dessus. Les clés ne sont jamais
passées en arguments. Pour la montée, changer uniquement `TARGET_RPS` et le nom de sortie,
sans augmenter le pool, les timeouts ou les ressources entre les paliers.

Le scénario arrête automatiquement un dépassement de seuil par défaut. Pour mesurer un maintien
entier malgré une latence trop élevée, `ABORT_ON_THRESHOLD_FAILURE=false` conserve le verdict
d’échec final ; il exige un watchdog Docker indépendant. La campagne locale publiée utilise
une surveillance toutes les deux secondes et arrête aussi sur suspension/horloge anormale ou
expiration des fixtures. Un échec de palier interdit la poursuite vers le débit supérieur.

## Sécurité pendant une charge indépendante

Réserver les 500 premiers comptes pour les contrôles et retirer ceux-ci du fichier de charge :

```bash
python3 - <<'PY'
import json, os
from pathlib import Path
fixture = json.loads(Path('.load-test/accounts.json').read_text())
fixture['accounts'] = fixture['accounts'][500:]
with os.fdopen(os.open('.load-test/security-background.json',
                      os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), 'w') as target:
    json.dump(fixture, target)
PY
```

Dans un terminal, démarrer le mélange habituel à **100 RPS**, deux minutes de maintien :

```bash
BASE_URL=http://127.0.0.1:18081 PROFILE=soak SOAK_MINUTES=2 \
TARGET_RPS=100 PREALLOCATED_VUS=1000 \
ACCOUNTS_FILE="$PWD/.load-test/security-background.json" \
k6 run --no-usage-report --summary-export=.load-test/security-background-summary.json \
  scripts/load-test.js
```

Pendant le maintien, dans un autre terminal :

```bash
sudo python3 scripts/check-dev-load-security.py \
  --fixture .load-test/accounts.json --compose-file compose.load-test.yaml \
  --output .load-test/security-results.json
```

Les 13 cas émettent chacun 16 requêtes avec huit workers : cookie admin requis malgré un Bearer
mobile, signature falsifiée, conversation étrangère, bannissement, versions/retraits de consentements,
blocage dans les deux sens et révocation de session. Les candidats étaient visibles avant leur
exclusion ; le blocage exige aussi une visibilité initiale réciproque. Les contrôles commencent après
la réponse de mutation ou le commit SQL, sans supposer l’annulation d’une réponse déjà en cours.

Les retraits de consentement, blocs et révocation empruntent les routes existantes. Le bannissement
et l’ancienne version sont injectés comme fixtures SQL : cela ne valide pas le parcours admin de
bannissement. La suppression du sexe/préférences ou de la présence après retrait est incluse dans
la restauration vérifiée. La famille de session temporaire testée reste révoquée jusqu’au nettoyage.

Un snapshot privé permet la réparation si le processus est interrompu :

```bash
sudo python3 scripts/check-dev-load-security.py \
  --fixture .load-test/accounts.json --compose-file compose.load-test.yaml \
  --output .load-test/security-results.json --restore
```

Les erreurs attendues ne font pas partie du taux d’erreur du benchmark positif. Un `401` après
révocation ou `409 discovery_not_ready` après retrait constitue ici le résultat attendu.

## Variantes du feed

Chaque variante réserve 200 lecteurs fictifs et restaure ses modifications avant la suivante :

| Variante | Données préparées | Assertion supplémentaire |
| --- | --- | --- |
| `dense-1km` | Rayon du lecteur de 1 km dans la population dense existante | Page non vide, distance dans le rayon |
| `large-radius-500km` | Rayon du lecteur de 500 km, population toujours concentrée | Page non vide ; ce cas ne simule pas une population répartie sur 500 km |
| `sparse-empty` | 200 lecteurs à des positions espacées de plus de 50 km | Page vide et curseur nul, attendus explicitement |
| `deep-page-21` | Vingt pages de 20 profils préparées par lecteur | Aucun doublon avec les 400 profils précédents |
| `heavy-exclusions` | Environ 90 % des comptes fictifs bannis temporairement, lecteurs conservés | Chaque profil retourné appartient à la liste des comptes non bannis et non supprimés |

Exemple pour le premier cas :

```bash
sudo python3 scripts/prepare-dev-feed-variant.py \
  --fixture .load-test/accounts.json --compose-file compose.load-test.yaml \
  --variant dense-1km --output .load-test/feed-variant.json
sudo chown "$(id -u):$(id -g)" .load-test/feed-variant.json

BASE_URL=http://127.0.0.1:18081 ACCOUNTS_FILE="$PWD/.load-test/feed-variant.json" \
VARIANT_RPS=50 VARIANT_SECONDS=60 VARIANT_VUS=100 \
k6 run --no-usage-report --summary-export=.load-test/dense-1km-summary.json \
  scripts/load-test-feed-variants.js

sudo python3 scripts/prepare-dev-feed-variant.py \
  --fixture .load-test/accounts.json --compose-file compose.load-test.yaml \
  --output .load-test/feed-variant.json --restore
```

Répéter avec les autres noms après restauration. Celle-ci doit être exécutée même si k6 échoue.
Le fichier `.snapshot.json` ne doit pas être supprimé avant une restauration réussie.
Les anciennes valeurs de préférences, coordonnées, fraîcheur et timestamps sont comparées après
restauration. La préparation de la page 21 émet 4 000 lectures supplémentaires, hors mesure de charge.

## Collecte et interprétation

Conserver image, version PostgreSQL, ressources, pool, VU, durées et nombre de comptes. Pour les pics,
enregistrer à la seconde le pool, les états d’attente SQL et les deltas `cpu.stat` des cgroups.
Les compteurs `nr_throttled/nr_periods` décrivent la limitation CPU du conteneur ; la CPU Docker seule
ne donne pas la cause d’une requête lente. Observer aussi la fréquence/température CPU et la pression
de scheduling de l’hôte partagé, sans changer son governor au milieu d’une comparaison.

Recouper l’intervalle d’une réponse lente avec ces relevés et les snapshots `pg_stat_statements`.
Une coïncidence n’attribue pas toute la latence à un mécanisme unique. Le compteur d’exécution SQL
ne fournit pas de percentile et inclut les attentes. Publier les agrégats sans tokens, coordonnées
individuelles ou texte SQL contenant des données personnelles.

La comparaison distante nécessite une pile contenant uniquement les données fictives et un tunnel
distinct. Sans ces prérequis, inscrire **non exécuté** dans les résultats et suivre les étapes serveur
et Cloudflare du [guide initial](load-testing.md), sans charger le tunnel de production.

## Nettoyage

Restaurer d’abord les éventuels snapshots de sécurité/variantes, puis :

```bash
sudo python3 scripts/prepare-dev-load-test.py --cleanup --api-service load-api \
  --compose-file compose.load-test.yaml --fixture .load-test/accounts.json
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  -f compose.load-test.yaml rm --stop --force -v load-api load-redis
rm -- .load-test/security-background.json
sudo rm -- .load-test/runtime.env
```

Retirer aussi les copies éventuelles de jetons et le rôle SQL temporaire si un rôle dédié a été créé.
Conserver les métriques, agrégats et preuves de restauration ; contrôler les cardinalités, la santé
de la pile habituelle et le smoke test. Ne supprimer ni volume PostgreSQL ni données métier pour
nettoyer un benchmark.
