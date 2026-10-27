# Administration et passkeys

[Index](README.md) · [Contrat HTTP](http-contract.md#administration) · [Postman](../postman/README.md)

Le dashboard admin s’authentifie exclusivement avec WebAuthn. Une session mobile, même pour un compte
superadmin, ne donne pas accès aux routes d’administration. Les sessions admin utilisent un cookie opaque,
pas un JWT ni un OTP SMS.

## Origine et cookie

| Installation | Origine WebAuthn | RP ID |
| --- | --- | --- |
| Locale | `http://localhost:5173` | `localhost` |
| Dashboard HTTPS | `https://dashboard.histae.com` si c’est le domaine retenu | `dashboard.histae.com` |

Configurer l’origine exacte dans `ADMIN_WEBAUTHN_ORIGIN`, sans chemin, et le RP ID correspondant.
Le dashboard transmet `/api` via son proxy de même origine. `localhost` et `127.0.0.1` sont deux origines
différentes ; une IP LAN en HTTP ne convient pas au parcours distant. Utiliser HTTPS pour ce parcours.
Une passkey enrôlée pour `localhost` ne devient pas automatiquement une passkey du domaine production.

Sur HTTPS, le cookie s’appelle `__Host-histae_admin_session`, avec `Secure`, `HttpOnly`, `SameSite=Strict`,
`Path=/` et sans `Domain`. En HTTP local, son nom est `histae_admin_session`.
Chaque mutation admin exige `Origin: <ADMIN_WEBAUTHN_ORIGIN>`. Le contrat détaille les actions qui exigent
une authentification récente, dix minutes par défaut.

## Initialiser le premier superadmin

Créer d’abord un compte par le parcours mobile OTP. Le rôle `superadmin` n’est attribuable par aucune route.
Un index unique PostgreSQL impose **un seul superadmin**, y compris si plusieurs opérateurs agissent simultanément.

Pour automatiser la promotion de l’unique compte et l’émission du jeton, depuis `histae-scripts` :

```bash
bash bootstrap-superadmin.sh --dev
# Ou, pour un compte dans la pile production :
bash bootstrap-superadmin.sh --production
```

Le [script](../../histae-scripts/bootstrap-superadmin.sh) exige un seul compte non supprimé,
non banni et aucun autre superadmin. Il vérifie d’abord la configuration de l’API démarrée,
attribue le rôle dans une transaction verrouillée, puis affiche le jeton et l’origine du dashboard.
L’enregistrement de la passkey nécessite ensuite le navigateur et l’authentificateur.
Une relance pour ce même compte déjà superadmin émet un nouveau jeton ; les anciens jetons non consommés
sont invalidés. Les commandes manuelles ci-dessous restent disponibles.

Choisir la pile et lire les comptes. Exemple dev derrière le proxy :

```bash
COMPOSE=(sudo docker compose --env-file .env -f compose.yaml -f compose.dev.yaml -f compose.proxy.yaml)
"${COMPOSE[@]}" exec -T postgres sh -c \
  'exec psql -X -U "$POSTGRES_USER" -d "$POSTGRES_DB" -c "SELECT user_id, role, is_banned, deleted_at FROM user_account"'
```

Pour une base contenant **un seul compte actif non banni**, la commande ci-dessous effectue le bootstrap
initial hors API. Elle refuse les situations ambiguës et la présence d’un superadmin :

```bash
"${COMPOSE[@]}" exec -T postgres sh -c \
  'exec psql -X -v ON_ERROR_STOP=1 -U "$POSTGRES_USER" -d "$POSTGRES_DB"' <<'SQL'
BEGIN;
LOCK TABLE user_account IN SHARE ROW EXCLUSIVE MODE;
DO $$
BEGIN
  IF EXISTS (SELECT 1 FROM user_account WHERE role = 'superadmin')
     OR (SELECT count(*) FROM user_account WHERE deleted_at IS NULL) <> 1
     OR EXISTS (SELECT 1 FROM user_account WHERE deleted_at IS NULL AND is_banned) THEN
    RAISE EXCEPTION 'Initial superadmin setup requires exactly one active, unbanned account and no superadmin';
  END IF;
  UPDATE user_account SET role = 'superadmin' WHERE deleted_at IS NULL;
END $$;
COMMIT;
SQL
```

Pour une base déjà peuplée, identifier explicitement le compte propriétaire avant toute opération SQL.
La gestion ordinaire des admins passe ensuite par le dashboard/API et son audit.

<a id="enregistrer-la-premiere-passkey"></a>

## Enregistrer la première passkey

Le compte doit déjà être `admin` ou `superadmin`, actif et non banni. Générer son jeton temporaire :

```bash
"${COMPOSE[@]}" exec api /app/bin/admin-bootstrap <UUID_ADMIN>
```

Le jeton est affiché une seule fois, au format `identifiant:secret`, et expire après quinze minutes par défaut.
Le coller dans « Enregistrer la première passkey » du dashboard, choisir un nom, puis terminer la cérémonie
dans le navigateur. Le jeton ne remplace pas une authentification WebAuthn et ne doit pas être partagé.
Un succès crée la passkey et ouvre la session ; un jeton expiré impose une nouvelle émission.

Le navigateur doit fournir une passkey découvrable avec vérification utilisateur. Selon l’authentificateur
disponible, il peut proposer une clé matérielle, une passkey locale ou un appareil externe. Un QR code est
une option du navigateur pour un appareil externe compatible ; l’API ne génère pas de QR code de connexion.
Si le dialogue attend une clé que vous n’avez pas, annuler et choisir un autre authentificateur proposé.
Si aucun n’est disponible, il faut en configurer un avant de terminer l’enrôlement.

## Sessions et récupération

Les durées par défaut sont trente minutes d’inactivité et huit heures absolues. Une passkey supplémentaire
se crée depuis une session récemment authentifiée. Conserver un second moyen d’accès réduit le risque de
blocage lors de la perte du premier.

La passkey courante et la dernière passkey active ne peuvent pas être révoquées. Révoquer une autre passkey
ferme les sessions qui en dépendent. Le logout ferme la session courante ; la révocation des autres sessions
la conserve. Si toutes les passkeys sont perdues, un opérateur disposant de l’accès serveur peut émettre
un nouveau jeton de bootstrap pour ce compte administratif.

## Promouvoir ou retirer un administrateur

Seul le superadmin peut appeler `PATCH /api/admin/users/:id/role` :

```json
{ "role": "admin" }
```

Pour retirer les droits : `{ "role": "user" }`. Cette action conserve le compte utilisateur et révoque ses
sessions, passkeys et jetons admin. La promotion nécessite ensuite un enrôlement par `admin-bootstrap`.
L’API ne permet ni de promouvoir un second superadmin, ni d’agir sur soi-même ou sur le superadmin.
Le bannissement d’un compte et le retrait de son rôle sont deux actions distinctes.

Les consultations sensibles restent auditées. Un admin fournit un motif lorsque le contrat l’exige ; le
superadmin peut l’omettre, auquel cas l’API inscrit un motif automatique. Un motif fourni reste validé.

## Diagnostic d’accès

| Symptôme | Vérification |
| --- | --- |
| `401 admin_session_invalid` | Cookie admin présent pour le bon hôte, session non expirée, compte/passkey encore actifs |
| `403 invalid_admin_request_origin` | Origine exacte du dashboard et en-tête `Origin` sur la mutation |
| `admin_reauthentication_required` | Refaire la connexion WebAuthn avant l’action exigeant une session récente |
| Challenge / bootstrap invalide | Expiration, consommation antérieure, jeton complet et origine/RP ID |
| Dialogue attendant une clé | Authentificateur disponible et vérification utilisateur, pas l’état de la session mobile |

Dans Postman, utiliser le cookie complet `nom=valeur` et `adminOrigin`. Postman ne produit pas une preuve
WebAuthn à la place du navigateur ; voir [les instructions d’import](../postman/README.md).

## Crédits de continuation

La fiche utilisateur conserve la consultation auditée et affiche `continuation_quota` : crédits gratuits, stock publicitaire, réservations et plafond hebdomadaire. L’onglet Plans distingue les deux crédits gratuits du maximum de cinq continuations Free, avec un stock publicitaire expirant chaque lundi à 00:00 UTC et affiche si les récompenses AdMob sont activées. Premium ne consomme aucun crédit et retire les publicités. Le statut `awaiting_continuation` est présenté comme une discussion en pause. Voir [Règles](continuation-and-rewards.md).

## Traductions du catalogue

Les écrans Traits et Questions de profil permettent de gérer l’anglais, l’espagnol et l’italien.
Le champ principal reste français ; les indicateurs EN/ES/IT signalent les traductions disponibles.
Vider une traduction la supprime et rétablit le repli français. Les droits et protections d’origine restent
identiques à ceux des modifications du catalogue. Voir [Internationalisation](internationalization.md).
