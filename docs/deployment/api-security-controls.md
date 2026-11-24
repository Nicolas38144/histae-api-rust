# Déployer les protections de l’API

Utiliser les manifests `compose.yaml` et `compose.production.yaml` avec les images reconstruites. Les
étapes ci-dessous s’appliquent avant de redéployer les services concernés.

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
