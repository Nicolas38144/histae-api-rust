# Exploitation

[Index](README.md) · [Déploiement](container-deployment.md) · [Supervision](observability.md)

## Processus et commandes

| Binaire / service | Fonction | Cycle de vie |
| --- | --- | --- |
| `api` | HTTP métier, SSE et listener métriques privé | Permanent ; maintenance désactivée dans Compose |
| `outbox` / `outbox-worker` | Effets durables : suppression photo, push, effacement de compte et réconciliation Stripe | Permanent, claims et retries bornés |
| `maintenance` | Matchs, photos, rétention RGPD, programmation de réconciliation Stripe | Une passe puis sortie |
| `db-migrate` / `migrate` | Installation et mise à niveau PostgreSQL | Une exécution puis sortie |
| `storage-init` | Vérification/création du bucket privé de production | Une exécution puis sortie |
| `admin-bootstrap` | Émission d’un jeton d’enrôlement administratif | Une exécution avec UUID puis sortie |
| `contract-compare` | Comparaison explicite de deux API isolées | Outil de validation ; voir [Tests](../tests/README.md#comparaison-de-deux-api) |

`MAINTENANCE_MODE=worker` est exigé par les binaires outbox et maintenance. La valeur historique `api`
reste acceptée par la configuration, mais ne lance pas de scheduler dans l’API Rust actuelle. Lancer les
processus séparés ; utiliser `disabled` pour l’API comme dans Compose.
Les processus partagent PostgreSQL mais gardent leurs propres pools et métriques mémoire.

<a id="maintenance-reguliere"></a>

## Maintenance régulière

Sélectionner la pile ; exemple dev derrière le proxy :

```bash
COMPOSE=(sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml -f compose.proxy.yaml)
"${COMPOSE[@]}" --profile jobs run --rm --no-deps maintenance
```

`--profile jobs` sélectionne le service ponctuel, `run` crée un conteneur pour une passe, `--rm` le retire
après sortie et `--no-deps` suppose les dépendances déjà prêtes. Cette commande ne relance pas la pile.
En production, remplacer la sélection par `.env`, `compose.yaml` et `compose.production.yaml`.

| Traitement | Effet |
| --- | --- |
| Matchs | Appliquer les échéances et transitions devenues dues, par lots bornés |
| Photos | Nettoyer les traitements/objets éligibles et les anciennes demandes idempotentes |
| Rétention RGPD | Purger les données dont les durées de rétention sont dépassées |
| Stripe | Programmer les réconciliations dues dans l’outbox ; le worker appelle ensuite Stripe |

Les échéances et nouvelles données arrivent après chaque passe. Une exécution au démarrage ne traite
donc pas le travail futur. Le budget de lots peut aussi laisser `work_remaining=true` : la passe suivante
poursuit le travail. Un intervalle initial de cinq minutes doit être ajusté selon retard, volume et alertes.
Ne pas augmenter les budgets sans regarder la charge SQL.

Dans le dashboard, « Jamais exécuté » signifie qu’aucun état n’a été enregistré pour ce traitement.
Outbox peut avoir réussi alors que maintenance n’a jamais été lancée. La réconciliation Stripe ne s’exécute
pas lorsque `BILLING_PROVIDER=disabled` ; son absence peut donc être attendue en développement.
Vérifier `status`, `last_error_code`, progression et travail restant après une passe, pas seulement son existence.

### Timer systemd sur Debian

Créer les deux unités suivantes sur le serveur. Remplacer `/opt/histae/histae-api-rust` par le chemin réel.
L’exemple cible la production, avec des dépendances démarrées ; pour dev/proxy, adapter les options Compose.
Le service système s’exécute avec les droits d’accès Docker et peut être lancé manuellement pour validation.

`/etc/systemd/system/histae-maintenance.service` :

```ini
[Unit]
Description=Histae bounded maintenance
Requires=docker.service
After=docker.service

[Service]
Type=oneshot
WorkingDirectory=/opt/histae/histae-api-rust
ExecStart=/usr/bin/docker compose --env-file .env -f compose.yaml -f compose.production.yaml --profile jobs run --rm --no-deps maintenance
```

`/etc/systemd/system/histae-maintenance.timer` :

```ini
[Unit]
Description=Run Histae maintenance every five minutes

[Timer]
OnCalendar=*:0/5
Persistent=true
Unit=histae-maintenance.service

[Install]
WantedBy=timers.target
```

Après création des fichiers :

```bash
sudo systemctl daemon-reload
sudo systemctl start histae-maintenance.service
sudo journalctl -u histae-maintenance.service -n 50 --no-pager
sudo systemctl enable --now histae-maintenance.timer
sudo systemctl list-timers histae-maintenance.timer
```

Une même unité active ne démarre pas une seconde instance à chaque tick. `Persistent=true` permet de
rattraper une activation manquée après arrêt ; ce n’est pas un rejeu de toutes les passes manquées.
Désactiver le timer avant une opération destructive sur la base, puis le reprendre après restauration.

<a id="outbox-et-reprises-operateur"></a>

## Outbox et reprises opérateur

Le worker réclame les événements avec `SKIP LOCKED`, conserve l’ownership et renouvelle ses leases avant
les effets. Une erreur peut déclencher un retry puis une dead letter. La suppression d’un conteneur ne
supprime pas l’intention PostgreSQL ; une reprise ne doit pas contourner cette intention durable.

Corriger la dépendance puis utiliser les routes protégées du dashboard pour reprendre une dead letter.
Ces actions exigent une authentification récente et un audit ; le motif est facultatif pour le superadmin.
L’abandon des événements d’effacement de compte et Stripe est interdit ; une suppression photo encore
nécessaire ne peut pas non plus être abandonnée. Le [contrat](http-contract.md#exploitation-et-reprises)
décrit les réponses et conflits. Un acquittement push ne prouve pas la réception sur le terminal.

## Santé et supervision

- `/health/live` : processus HTTP vivant.
- `/health/ready` : vérifications des dépendances de readiness ; ne prouve pas tous les fournisseurs externes.
- Dashboard : métriques métier, outbox, maintenance et diagnostics agrégés.
- Prometheus : listener privé avec bearer token ; [configuration et alertes](observability.md).

Les logs utilisent des événements et codes sûrs. Conserver heure, requête normalisée, code et contexte
de livraison. Ne pas enregistrer corps, téléphone, JWT, bootstrap, cookie ou réponses fournisseurs brutes.

<a id="diagnostic-de-demarrage"></a>

## Diagnostic de démarrage

Depuis la racine, avec la sélection Compose correspondant au déploiement :

```bash
COMPOSE=(sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml -f compose.proxy.yaml)
"${COMPOSE[@]}" ps --all
"${COMPOSE[@]}" logs --since=10m --tail=100 migrate api outbox-worker
```

| Symptôme | Vérifications et action |
| --- | --- |
| `migrate` échoue / `postgres_pool_timeout` | PostgreSQL TCP prêt, host/port internes, mot de passe réel du rôle, CA si TLS, logs PostgreSQL ; relancer le migrateur après correction |
| `redis_connection_failed` | Adresse interne, réseau, TLS et authentification ; dev impose Redis sans mot de passe, production exige TLS et mot de passe |
| API unhealthy | Logs de démarrage puis résultat du healthcheck ; healthy/live ne suffit pas pour readiness |
| `histae-api-production` introuvable | Upstream du proxy sélectionné pour la bonne pile, alias API et réseau edge partagés |
| Réseau edge absent | Démarrer la pile reverse proxy qui crée son IPAM ; ne pas improviser une plage différente |
| SMS `provider_network_error` / `EAI_AGAIN` | DNS et accès HTTPS depuis le conteneur API ; `HISTAE_API_DNS_SERVER` règle son DNS en dev |
| Nouveau `.env` sans effet | Recréer le service, vérifier les valeurs imposées par `environment:` et le fichier `HISTAE_ENV_FILE` |
| Auth admin refusée malgré rôle superadmin | Session WebAuthn, origine exacte et cookie du bon hôte ; un JWT mobile ne suffit pas |

Pour le détail du healthcheck, récupérer d’abord l’identifiant du conteneur puis l’inspecter :

```bash
api_container=$("${COMPOSE[@]}" ps -q api)
sudo docker inspect "$api_container" --format '{{range .State.Health.Log}}{{println .End .ExitCode .Output}}{{end}}'
```

Tester le chemin proxy seulement après readiness de l’API :

```bash
sudo docker exec reverse-proxy-container sh -c 'wget -qO- "$HISTAE_API_UPSTREAM/health/ready"'
```

Une réponse OTP `202` indique l’acceptation fournisseur, pas la livraison au téléphone. Sur
`503 otp_delivery_unknown`, préserver la clé d’idempotence et suivre le
[parcours OTP](http-contract.md#connexion-mobile-et-catalogue) ; ne pas générer des envois en boucle.
