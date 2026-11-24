# Déployer les protections de l’API

Utiliser les manifests `compose.yaml` et `compose.production.yaml` avec les images reconstruites. Les
étapes ci-dessous s’appliquent aux installations existantes avant la première migration de cette version.

## TLS du vérificateur d’achats

La commande suivante prépare une autorité dédiée et un certificat serveur valide pour `store-verifier`.
Le script refuse un répertoire existant. Pour installer des certificats issus d’une autorité déjà gérée,
placer `ca.crt`, `server.crt` et `server.key` dans `.secrets/store-verifier/`.

```bash
bash scripts/prepare-store-verifier-tls.sh
```

L’UID 1000 du conteneur doit pouvoir lire `server.key`. Sur un hôte où l’exploitant est un autre UID,
attribuer la clé au groupe 1000 et utiliser le mode `0640`. Les clients montent uniquement `ca.crt`.
La clé `ca.key` reste hors des conteneurs. Mettre cette clé à l’abri et programmer le renouvellement du
certificat serveur avant son expiration. Lors d’une rotation de CA, distribuer les deux certificats de CA
aux clients pendant le basculement, puis retirer l’ancien. Le contrôle de santé vérifie aussi le DNS du
certificat via sa valeur SAN `DNS:store-verifier`.

## Rôle PostgreSQL pour l’archive d’audit

Préparer `POSTGRES_AUDIT_ARCHIVE_PASSWORD` avec `python3 scripts/prepare-postgres-roles-env.py .env`
ou avec le gestionnaire de secrets. Utiliser une valeur indépendante des autres rôles PostgreSQL.
Une installation neuve crée le rôle `histae_audit_archive` à l’initialisation. Pour un cluster existant,
recréer le conteneur `postgres` avec son volume de données existant afin que son environnement voie le
nouveau secret. Puis créer le rôle, sous l’administrateur de ce cluster, **avant** de lancer le migrateur.

```bash
dc=(docker compose --env-file .env -f compose.yaml -f compose.production.yaml)
"${dc[@]}" exec -T postgres sh -ec 'exec psql -X -q -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB"' <<'SQL'
\set bootstrap_mode access
\getenv audit_archive_password HISTAE_BOOTSTRAP_AUDIT_ARCHIVE_PASSWORD
\i /etc/histae/db/000_bootstrap_postgres.sql
SQL
```

Arrêter les anciennes instances de maintenance avant la migration. Elle retire l’écriture directe sur les
traces existantes et met les événements conservés en file d’archivage. Le service `audit-archive` démarre
avec le déploiement de production et utilise un volume persistant monté uniquement chez lui. Les événements
sont archivés en JSON par jour UTC. Le service acquitte chaque lot après écriture durable. Un redémarrage
reprend les événements en attente sans écraser les fichiers existants. Le service assure aussi la purge au
bout d’un an. Les identifiants d’audit restent stables pendant cette période, y compris après effacement du
compte ou de la session concernés.

Sauvegarder le volume d’archive séparément de PostgreSQL, avec un accès restreint et une rétention d’un an.
Pour séparer aussi la panne de l’hôte, utiliser un volume externe ou une sauvegarde hors hôte. En cas
d’arrêt du service ou de manque d’espace, surveiller l’âge de la première entrée en file et les logs
`audit_archive_failed`. Les événements non archivés restent en file et la rétention SQL les préserve.
En développement, l’archive tourne à la demande sous le profil `audit` avec son propre mot de passe.

## Exports

Compose utilise le volume disque `data-export-spool` pour `DATA_EXPORT_SPOOL_DIR`. Les fichiers ouverts
sont retirés du répertoire avant écriture sur Linux. Le noyau rend l’espace à la fermeture du fichier,
y compris à l’arrêt forcé du processus.

Réserver au moins `DATA_EXPORT_SPOOL_BYTES` (2 Gio par instance par défaut) sur le disque du volume.
Le paramètre borne la configuration de l’API. Il doit couvrir la concurrence multipliée par la taille
maximale d’un export assisté (1 Gio par défaut). Pour des réplicas partageant le même disque, prévoir le
budget de chaque instance. Un disque plein rend momentanément les exports indisponibles.
`DATA_EXPORT_DOWNLOAD_TIMEOUT` vaut cinq minutes par défaut et libère le créneau même si le client ne lit
plus la réponse. Une échéance dépassée interrompt le téléchargement. Le client peut le relancer.
