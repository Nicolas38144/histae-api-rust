# Blocage des numéros SMS publics

[Index](README.md) · [Protection SMS](sms-safety.md) · [Rôles PostgreSQL](postgres-roles.md)

## Fonctionnement

L’API applique la politique locale avant toute réservation du budget fournisseur et tout appel
Sweego, pour les OTP mobiles, les OTP du site et les tâches outbox. Aucun appel à une liste externe
n’est effectué pendant une requête utilisateur. Les limites SMS et IP existantes restent applicables.
Un refus ne consomme pas de tentative fournisseur et ne révèle pas publiquement la présence du numéro
dans la liste. Le `202` mobile conserve son contrat habituel.

Le job `phone-blocklist-sync`, dans la même image que l’API, consulte deux sources HTTPS fixes :

| Source | Format et observation retenue |
| --- | --- |
| [Castle](https://github.com/castle/disposable-phone-numbers) | Liste des 1 000 numéros publics les plus actifs ; présence au téléchargement |
| [iP1](https://github.com/iP1SMS/disposable-phone-numbers) | Numéros et dates ; date d’observation fournie par la source |

Ces projets sont publiés sous licence MIT. Ils ne constituent pas un annuaire exhaustif et ne prouvent
pas l’identité ou la propriété d’un numéro. Seuls les numéros français acceptés par le contrat OTP de
Histae sont importés. Les formats, tailles, volumes minimaux et dates sont contrôlés ; les redirections
HTTP sont refusées. Une source invalide n’écrase jamais son dernier import réussi.

Une observation bloque le numéro pendant **90 jours** après sa dernière observation. Télécharger à
nouveau une ancienne date ne prolonge pas sa validité. Une disparition de la liste ne provoque pas de
levée immédiate : elle ne garantit pas que la boîte SMS est devenue privée.

Pour permettre les nouveaux envois, au moins une source doit avoir été importée avec succès depuis
moins de **7 jours**. Avant le premier import ou après sept jours sans succès, les nouveaux envois sont
refusés, sauf autorisation manuelle encore valide. Les erreurs SQL refusent également l’envoi. Le job
réussit si au moins une source a pu être importée ; une défaillance partielle reste visible et alertée.

La vérification d’un OTP déjà émis contrôle aussi le blocage courant. Elle ne dépend pas de la fraîcheur
des téléchargements. Les comptes et sessions existants ne sont pas suspendus automatiquement.
Une requête fournisseur déjà autorisée avant une décision de blocage peut encore aboutir.

## Numéro réattribué et décisions administratives

Dans **SMS Sweego → Numéros publics**, rechercher le numéro complet : la recherche utilise un corps
POST, jamais une URL. Les listes et résultats n’exposent que l’indice masqué, l’UUID technique, les
observations et la décision. Le numéro n’est pas conservé dans l’historique de recherche du dashboard.

| Rôle | Actions | Justification |
| --- | --- | --- |
| Admin | Autorisation temporaire d’envoi | Obligatoire, 3–500 caractères, sans caractères de contrôle |
| Superadmin | Autorisation, blocage ou retour au calcul automatique | Aucune saisie exigée ; motif technique enregistré automatiquement |

L’autorisation et le blocage manuels durent **1 à 90 jours**, 30 jours proposés par le dashboard.
Ils prennent la priorité sur les sources. L’import ne les remplace jamais. À expiration ou après retour
au mode automatique, les observations encore valides s’appliquent de nouveau. Une autorisation
n’efface ni les observations, ni les quotas, ni la suspension globale des SMS.

Avant d’autoriser, vérifier le contexte par un canal adapté ; recevoir un OTP sur une boîte publique
ne prouve pas la possession exclusive du numéro. Ne pas utiliser une autorisation pour compenser un
incident global de synchronisation. Une fois la décision prise, demander un nouvel OTP avec une
nouvelle clé d’idempotence : rejouer une ancienne demande échouée ne relance pas sa livraison.

Les mutations exigent une session WebAuthn récente, l’origine administrateur et la version lue. Une
version périmée donne `409 resource_changed` ; relire avant de décider. Le rôle réel est revérifié dans
la transaction. Le journal contient `admin_allow_phone`, `admin_block_phone` ou
`admin_reset_phone_policy` et l’UUID de politique. Une réponse perdue entraîne une relecture, sans
rejouer automatiquement l’écriture.

## Données et isolation

Les numéros sont normalisés avec la même fonction que l’OTP, puis transformés en HMAC avec
`PHONE_HASH_KEY`. Aucun numéro complet n’est enregistré dans ces tables ; l’indice conserve seulement
`+33` et les quatre derniers chiffres. Ces empreintes et indices restent des données personnelles
pseudonymisées. La maintenance RGPD supprime les observations après 180 jours et les politiques
inactives, sans observation ni décision active, après 180 jours sans modification, par lots bornés.

Le rôle `histae_phone_sync` ne peut pas lire les comptes, modifier les décisions manuelles, supprimer
les politiques ou créer des objets SQL. Le conteneur reçoit uniquement sa connexion PostgreSQL et la
clé HMAC : aucun identifiant Sweego, Stripe, JWT ou clé de chiffrement des téléphones. PostgreSQL utilise
`verify-full` avec la CA en production. Le job ne publie aucun port.

Une empreinte de contrôle lie les imports et l’API à la même clé HMAC. Une clé différente produit
`phone_blocklist_key_mismatch`, sans import. **Ne pas régénérer `PHONE_HASH_KEY` au déploiement** : elle
identifie aussi les comptes existants. Sa rotation demande une migration coordonnée ; supprimer la
table de contrôle ne constitue pas une procédure de rotation.

## Installation et mise à jour

Depuis `histae-api-rust`, préparer le nouveau mot de passe sans afficher les secrets :

```bash
python3 scripts/prepare-postgres-roles-env.py .env
```

Le préparateur conserve les trois mots de passe existants et ajoute
`POSTGRES_PHONE_SYNC_PASSWORD`, 64 caractères hexadécimaux aléatoires, distinct des autres. Les
anciennes bases qui n’ont pas encore adopté les rôles séparés suivent d’abord le guide PostgreSQL.

Avec les dépôts API, dashboard et `histae-scripts` à jour, depuis leur dossier parent :

```bash
sudo bash histae-scripts/reset-api.sh --production
sudo bash histae-scripts/reset-dashboard.sh --production
sudo bash histae-scripts/systemd/install-timers.sh
sudo systemctl enable --now histae-phone-blocklist.timer
```

Le reset API applique les migrations, configure le rôle dédié sur le cluster existant et lance un
premier import. L’installation systemd conserve `/etc/histae/maintenance.env` s’il existe : vérifier
`HISTAE_ROOT` et `HISTAE_DEPLOYMENT_MODE=production` avant d’activer le timer. Celui-ci lance un import
quotidien avec un décalage aléatoire jusqu’à 30 minutes et rattrape une échéance manquée au démarrage.
En développement, utiliser `--dev` et régler le mode de l’environnement systemd en conséquence.

Pour un démarrage Compose direct, lancer le job après `migrate` ; les installations existantes doivent
également configurer le rôle via le reset. Le profil `jobs` n’est pas activé par défaut :

```bash
sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml \
  --profile jobs run --rm --no-deps -T phone-blocklist-sync
```

## Exploitation

```bash
# Depuis le parent des dépôts : import manuel, sans SMS.
sudo bash histae-scripts/run-phone-blocklist-sync.sh --production
sudo systemctl list-timers histae-phone-blocklist.timer
sudo journalctl -u histae-phone-blocklist.service --since today
```

Si PostgreSQL signale `Role "histae_phone_sync" does not exist`, ou un mot de passe de ce rôle
invalide, préparer le rôle et relancer l’import depuis le parent des dépôts :

```bash
sudo bash histae-scripts/run-phone-blocklist-sync.sh --production --prepare-role
```

Cette préparation lit `POSTGRES_PHONE_SYNC_PASSWORD` dans la configuration Compose sélectionnée,
plutôt que dans l’environnement éventuellement ancien du conteneur PostgreSQL. Elle réutilise le
bootstrap administrateur, conserve les autres mots de passe et ne reconstruit ou redémarre aucun
service. Le secret transite uniquement par l’entrée standard ; il n’est pas affiché, écrit dans un
fichier ou passé en argument. Les fichiers API et scripts doivent être à jour. Le lancement quotidien
ordinaire ne prépare pas les rôles : cette option est réservée à l’intervention de l’opérateur.

Le job partage le verrou de maintenance/déploiement. En cas d’échec, vérifier la résolution DNS et
l’accès HTTPS sortant depuis Docker, la disponibilité des sources, les droits du rôle dédié et la
cohérence de `PHONE_HASH_KEY`. Garder le dernier état valide ; ne pas vider les tables pour rétablir
les envois. Relancer l’import après correction et vérifier les deux sources dans le dashboard.

Prometheus expose `histae_phone_blocklist_unavailable` et `histae_phone_blocklist_source_errors`, sans
label de numéro. `HistaePhoneBlocklistUnavailable` alerte après 10 minutes sans source fraîche et
`HistaePhoneBlocklistSourceFailed` après 15 minutes de défaillance d’une source. Ces alertes sont
neutralisées lorsque Sweego est désactivé. Le premier import réussi est indispensable avant ouverture.

## Vérification locale isolée

```bash
cargo build --locked --bin db-migrate --bin phone-blocklist-sync
python3 scripts/verify-continuation-postgres.py \
  --native-migrator target/debug/db-migrate \
  --test phone_blocklist_postgres --test sms_safety_postgres --test otp_delivery
```

Ajouter `--native-phone-sync target/debug/phone-blocklist-sync` pour consulter réellement les deux
sources publiques dans la base jetable, sans appel Sweego. Le test vérifie les migrations répétées,
les droits, la confidentialité des réponses, l’expiration, les écritures concurrentes, les refus avant
budget et le contrôle d’un OTP antérieur. Les tables et identifiants temporaires sont ensuite supprimés.
