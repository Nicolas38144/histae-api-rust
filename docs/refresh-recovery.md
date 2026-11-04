# Reprise d'un renouvellement de session mobile

[Documentation](README.md) · [Contrat HTTP](http-contract.md)

## Contrat et sécurité

`POST /api/auth/refresh` accepte le refresh opaque habituel et, pour les clients compatibles,
`recovery_id` avec la même clé UUIDv4 dans `Idempotency-Key`. Le client persiste le couple
refresh/clé avant tout envoi et le conserve après un résultat incertain. Le champ de corps est
volontaire : une ancienne API le rejette avant de consommer le jeton, au lieu d'ignorer la clé.
Le mode de reprise exige les deux identifiants, identiques : un seul des deux est refusé avant
consommation du token. Les clients historiques sans ces deux champs gardent la rotation stricte.

La transaction PostgreSQL verrouille le compte, le parent et la famille, consomme le parent,
crée un seul descendant et stocke son refresh chiffré. `RefreshRecovery` utilise les fonctions
AES-256-GCM existantes, avec un nonce aléatoire et une clé SHA-256 dérivée du secret refresh
original et de l'identifiant de tentative, avec séparation de domaine. Seuls le hash de la clé
de tentative et le ciphertext sont enregistrés ; ni le refresh ni la clé de déchiffrement ne
sont stockés en clair. Aucun secret serveur ou champ `.env` supplémentaire n'est nécessaire.

La même tentative récupère ce descendant pendant **sept jours**, tant que le compte, la famille
et le descendant sont actifs. Le résultat déchiffré est contrôlé contre le JTI et le hash du
descendant. La reprise réémet un access token ; elle ne prolonge ni `last_refreshed_at`, ni
`expires_at`, ni la fenêtre de reprise, et ne crée aucun autre descendant. Les requêtes
concurrentes portant la même clé retrouvent donc un seul refresh.

Une autre clé sur un ancien refresh authentique non expiré reste un rejeu et révoque la famille.
Une tentative connue devenue trop ancienne ou dont le descendant a déjà tourné renvoie `401`
sans révoquer un descendant plus récent. La révocation ou l'indisponibilité du compte prime
toujours sur la reprise. Logout, logout-all, retrait de session et bannissement purgent les résultats
chiffrés de leur périmètre ; un débannissement ne réactive pas les familles révoquées. La limite
existante de 30 refresh/15 minutes/IP s'applique aussi aux reprises. Ne jamais journaliser le corps,
le refresh, la clé de tentative ou le résultat chiffré.

La maintenance efface les ciphertexts après sept jours, par lots comptabilisés et bornés. Les lots
ignorent les tokens verrouillés (`FOR UPDATE SKIP LOCKED`) pour ne pas supprimer un parent dont
la rotation est en cours ni attendre son commit. Un parent expiré peut être conservé jusqu'à
cette échéance pour une reprise validée ; cela ne
prolonge pas sa famille ni son descendant. La révocation efface immédiatement ses ciphertexts.
Le hash d'une ancienne tentative reste jusqu'à la purge du parent pour reconnaître une reprise
légitime devenue trop ancienne. Les effacements de compte suppriment ces données en cascade.

## Ordre de livraison

1. Sauvegarder la base et appliquer le migrateur actuel avec le rôle de migration habituel.
   Il ajoute `refresh_tokens.rotation_request_hash` et `rotation_result`, sans modifier les
   sessions existantes ni leur échéance. Le schéma initial contient aussi ces colonnes.
2. Déployer l'API et les workers utilisant ce schéma, dont la maintenance.
3. Distribuer l'application compatible.

Les anciens clients fonctionnent encore sur la nouvelle API ; ils ne disposent pas de la reprise.
Les anciens binaires de maintenance doivent être remplacés : ils ne connaissent pas la rétention
supplémentaire des parents expirés. Les jetons déjà effacés par une ancienne app sont irrécupérables.
Cette livraison ne change pas `JWT_REFRESH_TTL` ni les échéances des jetons précédemment émis.

## Organisation et validation

`identity/mobile/pg/rotation.rs` porte les transactions de rotation et de reprise. Le service
interprète un enum distinguant les résultats ordinaires d'une reprise contenant un secret protégé ;
il ne peut pas substituer un candidat inutilisé au token effectivement récupéré. Le module
`refresh_recovery.rs` encapsule le chiffrement et expose ses métadonnées en lecture seule.

Validation locale du **4 novembre 2026** : **375 tests** sans les features d'intégration et
**20 tests PostgreSQL** ciblés réussis, ainsi que formatage, Clippy avec toutes les features et
upgrade répété du schéma. La base PostgreSQL, ses fixtures et ses identifiants temporaires ont été
supprimés après la campagne. Aucun fournisseur réel, serveur distant ou client mobile n'a été testé.

Depuis la racine de l'API, avec les prérequis habituels :

```bash
cargo fmt --all -- --check
cargo test --locked --all-targets
cargo clippy --locked --all-targets --all-features -- -D warnings
```

Les scénarios couvrent la première rotation concurrente, les reprises, les ciphertexts altérés,
un descendant ne correspondant pas au résultat déchiffré, le nettoyage avec une rotation verrouillée,
le bannissement et les autres révocations. Le lanceur vérifie aussi l'upgrade depuis une base sans
les colonnes de reprise, en conservant les sessions, les tokens historiques et leurs échéances.
Utiliser une base **locale jetable**, jamais la production :

```bash
cargo build --locked --bin db-migrate
python3 scripts/verify-continuation-postgres.py \
  --native-migrator target/debug/db-migrate \
  --test mobile_identity --test maintenance_outbox_postgres \
  --test administration_reports_postgres --test play_review_postgres --test account_erasure_postgres
```

Le lanceur exige son image PostgreSQL/PostGIS locale et Docker ; ajouter `--sudo-docker` si
nécessaire. Il prépare ses propres identifiants fictifs et sa base isolée. Avec une base de
développement déjà préparée et migrée, l'équivalent direct est :

```bash
cargo test --locked --features postgres-integration --test mobile_identity --test maintenance_outbox_postgres
```

Sur une installation de test, vérifier aussi une coupure après commit, la reprise avec la même
clé, un arrêt du processus mobile avant stockage, un refus `429`, le retrait de la session et
le redémarrage sur une ancienne API. Un refus définitif doit revenir à la connexion ; une panne
temporaire doit proposer Réessayer sans supprimer la tentative.
