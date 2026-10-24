# Protection des SMS Sweego

[Index](README.md) · [Contrat](http-contract.md#protection-des-envois-sms) · [Supervision](observability.md#sms-safety)

## Limites et comptabilité

La configuration par défaut autorise **5 tentatives par numéro sur une heure et 10 sur 24 heures**, et **100 tentatives globales par heure et 500 sur 24 heures**.
Les deux fenêtres sont glissantes ; elles ne se réinitialisent pas à minuit. Le numéro normalisé
E.164 est remplacé par son HMAC avec la clé téléphone existante. Le contrôle s’applique à chaque
requête Sweego, avant le réseau, après la validation et les limites IP/numéro déjà présentes.

```dotenv
OTP_RESEND_COOLDOWN=60s
SMS_MAX_GLOBAL_HOUR=100
SMS_MAX_GLOBAL_DAY=500
SMS_MAX_PER_PHONE_HOUR=5
SMS_MAX_PER_PHONE_DAY=10
SMS_ALERT_SPIKE_5M=20
SMS_SENDING_SUSPENDED=false
```

Les plafonds doivent être positifs et le plafond quotidien doit être au moins égal au plafond horaire.
Les plafonds globaux couvrent tous les numéros et tous les clients du service SMS (mobile et web). Ils ne lisent pas le solde Sweego. La réservation compte une tentative d’envoi ; les tarifs/crédits réellement débités restent
ceux du fournisseur. Les textes OTP actuels restent courts et chaque requête a un seul destinataire.

Une réservation PostgreSQL verrouille le contrôle global puis le téléphone : des instances concurrentes ne peuvent dépasser le dernier créneau global disponible. Elle est
validée avant le POST fournisseur ; elle reste consommée après un timeout, une réponse invalide,
un refus fournisseur ou un arrêt du processus. Aucun remboursement automatique ne peut permettre
un nouvel envoi dont le précédent aurait été facturé. Un même UUID de livraison déjà réservé ne
peut pas déclencher un second appel. Les rejeux OTP existants ne réservent rien de plus.

L’expiration d’un OTP, la déconnexion et la suppression du compte ne réinitialisent pas les compteurs.
Seuls l’UUID technique, le HMAC et la date de réservation sont conservés ; aucune donnée de message,
numéro en clair ou clé fournisseur n’entre dans ces tables, les réponses admin ou les labels métriques.
La maintenance RGPD supprime ces lignes après **25 heures**, avec son retard de planification éventuel ;
un nettoyage borné accompagne aussi les réservations. La maintenance doit rester planifiée.

Au premier déploiement, le migrateur importe les tentatives encore présentes dans les OTP locaux.
Il ne peut pas reconstituer les OTP déjà supprimés : une fenêtre initiale complète de 24 heures
n’est garantie qu’après une journée de fonctionnement sous cette protection.

## Blocage et arrêt d’urgence

- Les quotas HTTP IP/téléphone et le délai de renvoi produisent un `429` avant la mise en file. `otp_resend_too_soon` inclut le temps restant dans `Retry-After`.
- Pour le mobile, le budget fournisseur, la suspension et les échecs Sweego sont traités dans le worker sans modifier le `202` public. Aucun SMS n’est envoyé si le garde refuse. Les autres clients conservent leurs erreurs propres.
- Un OTP accepté antérieurement reste valable selon ses règles normales.
- Une tentative déjà autorisée avant la suspension peut encore aboutir.

Le superadmin peut suspendre ou reprendre depuis l’onglet **SMS Sweego** (`/sms-safety`) du dashboard.
La mutation exige une session WebAuthn récente, l’origine administrateur et la version lue. Elle est
auditée dans son journal d’accès avec `admin_suspend_sms` ou `admin_resume_sms` ; aucun motif manuel
n’est exigé. Les autres administrateurs ont uniquement la lecture. Les compteurs restent inchangés
lors d’une reprise. Une réponse perdue est résolue par une lecture, sans mutation automatique.

L’état manuel est partagé et survit aux redémarrages. En secours, définir `SMS_SENDING_SUSPENDED=true`
dans le `.env` et redéployer l’API ainsi que tous les workers outbox. La configuration force l’arrêt, même si le dashboard demande
une reprise. Remettre la variable à `false`, redéployer puis vérifier la suspension manuelle avant
la reprise. Un redémarrage ne nécessite aucun effacement ni remise à zéro de données.

## Alertes et dépendances

Le dashboard relit le statut toutes les 30 secondes. Un pic est signalé lorsque les réservations
globales atteignent `SMS_ALERT_SPIKE_5M` sur cinq minutes (20 par défaut), ou lorsque leur cadence est
au moins quatre fois celle de l’heure précédente : au moins cinq tentatives récentes et douze dans
la fenêtre de référence de 65 à 5 minutes avant la collecte. Le seuil absolu couvre un démarrage sans
historique. Les refus locaux ne sont pas comptés comme des appels Sweego en échec.

Prometheus déclenche `HistaeSweegoTrafficSpike` et `HistaeSweegoSendingSuspended`. La détection dépend
de la collecte et de l’évaluation ; le garde bloque directement l’envoi fournisseur, sans attendre
les métriques. Les alertes restent visibles dans Prometheus/Alertmanager ; le destinataire existant
ne transmet pas d’e-mail. Aucun e-mail ou SMS d’alerte n’est envoyé par cette fonctionnalité.
Le pic avertit l’opérateur ; il ne déclenche pas une suspension globale automatique.

Les jauges `histae_sweego_send_attempts{window="5m|1h|24h"}` sont partagées entre les instances qui
lisent la même base : utiliser `max` pour une vue commune, jamais la somme de ces copies.
La protection reste active lorsque Prometheus, Alertmanager ou Grafana sont arrêtés. Leurs services
sont facultatifs pour les plafonds et la suspension ; l’API, le worker outbox et PostgreSQL sont requis pour le parcours mobile.

## Livraison et vérification

Appliquer le migrateur, puis déployer l’API et le worker outbox compatibles avec la même configuration SMS et les mêmes clés téléphone. Le mobile utilise désormais une file `identity.otp.send` : sans worker, un `202` n’entraîne aucun envoi. Aucun déploiement n’est effectué automatiquement lors des modifications de code.

Le délai de renvoi est partagé entre tous les parcours et instances pour un téléphone, même quand aucun SMS n’est prévu. Le rejeu garde le même UUID, téléphone, langue et type de parcours ; il ne prolonge pas le délai. La vérification d’un code reçu reste possible pendant cette attente. Le téléphone en attente est chiffré avec un contexte lié à l’UUID, jamais placé dans le payload outbox, et effacé après traitement. Les demandes expirées ou remplacées ne sont pas envoyées. La maintenance RGPD nettoie les métadonnées à expiration.

Les tests utilisent des fournisseurs simulés et une instance PostgreSQL jetable : limites par numéro, concurrence sur le budget global entre numéros et instances, fenêtres glissantes, idempotence et cooldown durables, téléphone chiffré, suspension et purge. Ils ne consomment aucun crédit Sweego. Ils sont fournis sans exécution dans cet environnement :

```bash
cargo fmt --all
cargo test --locked --lib
cargo test --locked --test otp_sweego_contract --test sweego_client
cargo test --locked --features postgres-integration --test sms_safety_postgres -- --test-threads=1
```

La dernière commande exige la configuration locale de développement décrite dans [les tests](../tests/README.md) (`ENV=development`, PostgreSQL local et base `histae-dev`). Elle crée puis supprime sa propre base jetable. Ne pas utiliser une configuration de production.
