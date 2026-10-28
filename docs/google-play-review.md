# Accès de revue Google Play

Google demande des identifiants réutilisables, accessibles sans SMS ou code temporaire et des
instructions en anglais : [exigences officielles](https://support.google.com/googleplay/android-developer/answer/15748846?hl=en).

## Configuration serveur

L’accès est **désactivé par défaut**. Les trois variables suivantes sont exclusivement serveur :

```dotenv
GOOGLE_PLAY_REVIEW_ENABLED=false
GOOGLE_PLAY_REVIEW_PHONE_NUMBER=
GOOGLE_PLAY_REVIEW_OTP=
```

Pour l’activer, définir `true`, un numéro E.164 de la tranche fictive `+3363998XXXX` et un code
secret de six chiffres aléatoires. Cette tranche sans abonné réel provient du
[plan national de numérotation de l’Arcep](https://www.arcep.fr/uploads/tx_gsavis/18-0881.pdf), section
« Numéros pour œuvres audiovisuelles ». Le numéro sert uniquement d’identifiant applicatif ;
aucun appel ou SMS ne doit être envoyé dessus.

Les valeurs réelles restent dans `.env`, ignoré par Git et exclu de l’image Docker. Les recopier
séparément dans le `.env` du serveur de production puis reconstruire l’API avec le script de
réinitialisation habituel : les migrations sont nécessaires. La configuration Compose transmet
ces identifiants à l’API uniquement et les masque dans les autres services du runtime.
Ne pas les intégrer dans Flutter, Postman partagé, un commit ou une capture publique.

## Préparer le compte dans l’app

1. Déployer cette version de l’API sur le serveur utilisé par l’app publiée.
2. Ouvrir **Se connecter**, sélectionner la France (`+33`) et saisir le numéro de revue.
3. Demander le code puis saisir la valeur `GOOGLE_PLAY_REVIEW_OTP` ; aucun SMS n’est attendu.
4. La première vérification correcte crée automatiquement un **nouveau compte de revue**.
   Il est donc inutile de passer par « Créer un compte » ; le même couple reste réutilisable.
5. Compléter le profil, les consentements et les étapes d’accès au feed avec des données de test
   clairement identifiées et une photo pour laquelle vous disposez des droits nécessaires.
6. Se déconnecter puis se reconnecter avec le même numéro et le même code pour vérifier le parcours.
7. Dans Play Console → **Contenu de l’application → Accès à l’application**, renseigner le numéro,
   le code et les instructions ci-dessous en remplaçant les deux champs entre chevrons.

L’app utilise les routes existantes ; aucune modification de Flutter n’est nécessaire.
Ce mécanisme ne crée ni abonnement Premium, ni faux consentement, ni données de démonstration.
Préparer séparément l’accès aux fonctionnalités payantes si la revue en a besoin.

### Instructions en anglais pour Play Console

```text
Open the app and choose Sign in. Select France (+33) and enter <REVIEW_PHONE_NUMBER>.
Request a verification code, then enter <REVIEW_OTP>.
This is a dedicated review account. No SMS or access to a physical phone is required.
The same credentials can be reused for subsequent sign-ins and do not expire.
The account profile has been prepared for review.
```

Ne conserver la dernière phrase qu’après avoir effectivement préparé le compte.

## Contrat et sécurité

- Seuls `POST /api/auth/otp/send` et `POST /api/auth/otp/verify` traitent cette exception mobile.
- Pour ce numéro activé, `/send` conserve validation du corps, UUID d’idempotence et quotas IP/numéro.
  Il répond `202` avec `resend_after_seconds: 0`, sans demande outbox, appel Sweego ou consommation
  de budget SMS. Aucun effet n’étant produit, le rejeu n’ajoute aucune opération.
- `/verify` compare le code en temps constant. Un mauvais code n’effectue aucune création de compte.
  Les quotas restent appliqués, même avec le bon code : par défaut cinq vérifications par heure
  par IP et numéro. Éviter les demandes et tentatives inutiles pendant la préparation.
- Le compte porte `is_review_account=true`. Les connexions suivantes retrouvent le même compte ;
  un utilisateur normal existant avec ce numéro ne peut jamais être adopté par cette exception.
- Création, contrôle de l’état et émission de la session sont atomiques. Un tombstone apparu
  pendant l’insertion fait aussi annuler la création. Compte banni, supprimé ou
  téléphone couvert par un tombstone : refus. Une contrainte PostgreSQL interdit les rôles
  `admin` et `superadmin`, et la route de promotion renvoie son refus habituel.
- Les sessions, leur expiration et leur révocation restent celles de l’authentification mobile.
  Aucun accès WebAuthn ou paiement web par code fixe n’est ajouté. Les parcours SMS ordinaires
  conservent leur traitement habituel.
- Désactiver `GOOGLE_PLAY_REVIEW_ENABLED` ou changer le code puis recréer l’API arrête les nouvelles
  connexions avec ces identifiants. Révoquer aussi les sessions du compte si l’on veut couper les
  sessions déjà ouvertes. Pour une revue en cours, les identifiants fournis à Google doivent rester valides.

## Vérification

```bash
cargo test --locked --lib --test otp_sweego_contract --test mobile_auth_contract --test billing_contract
cargo build --locked --bin db-migrate
python3 scripts/verify-continuation-postgres.py --native-migrator target/debug/db-migrate \
  --test play_review_postgres --test otp_delivery --test administration_reports_postgres
```

Les fixtures utilisent des identifiants fictifs distincts du `.env`. Le lanceur PostgreSQL désactive
explicitement l’accès de revue local pour ses autres tests et supprime sa base temporaire à la fin.
