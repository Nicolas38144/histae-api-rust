# Accès de revue Google Play

Google demande des identifiants réutilisables, accessibles sans SMS ou code temporaire et des
instructions en anglais : [exigences officielles](https://support.google.com/googleplay/android-developer/answer/15748846?hl=en).

## Configuration serveur

L’accès est **désactivé par défaut**. La configuration se fait côté serveur.

```dotenv
GOOGLE_PLAY_REVIEW_ENABLED=false
GOOGLE_PLAY_REVIEW_PHONE_NUMBER=
GOOGLE_PLAY_REVIEW_OTP=
GOOGLE_PLAY_REVIEW_EXPIRES_AT=
```

Pour l’activer, définir `true`, un numéro E.164 de la tranche fictive `+3363998XXXX`, un code
secret de six chiffres aléatoires et la date de fin au format RFC 3339 avec fuseau horaire, comme
`2030-04-01T12:00:00Z`. L’activation exige une date de fin valide. Cette tranche sans abonné réel provient du
[plan national de numérotation de l’Arcep](https://www.arcep.fr/uploads/tx_gsavis/18-0881.pdf), section
« Numéros pour œuvres audiovisuelles ». Le numéro sert uniquement d’identifiant applicatif ;
aucun appel ou SMS ne doit être envoyé dessus.

Les valeurs réelles restent dans le `.env` du serveur, ignoré par Git et exclu de l’image Docker.
Au premier déploiement de H10, la base vierge utilise le schéma SQL initial mis à jour. Pour appliquer
une nouvelle configuration de revue, recréer toutes les instances de l’API. La configuration Compose
transmet ces identifiants à l’API et les masque dans les autres services du runtime.
Garder les identifiants de revue dans la configuration serveur et dans Play Console.

## Préparer le compte dans l’app

1. Déployer cette version de l’API sur le serveur utilisé par l’app publiée.
2. Ouvrir **Inscription / Créer un compte**, puis remplir le profil, les préférences et les
   consentements dans les formulaires habituels.
3. À l’étape téléphone, sélectionner la France (`+33`), saisir le numéro de revue, demander le code
   puis saisir `GOOGLE_PLAY_REVIEW_OTP` ; aucun SMS n’est attendu.
4. La première vérification correcte après cette demande d’inscription crée le **compte de revue**.
   Une simple demande de connexion ne crée plus de compte. Le même couple reste réutilisable
   via « Se connecter » une fois le compte inscrit. Ce compte bénéficie automatiquement de
   **Premium offert, sans expiration et sans achat**.
5. Vérifier l’enregistrement des informations saisies, puis compléter la photo et les autres étapes
   d’accès au feed avec des données de test clairement identifiées et des contenus autorisés.
6. Se déconnecter puis se reconnecter avec le même numéro et le même code pour vérifier le parcours.
7. Dans Play Console → **Contenu de l’application → Accès à l’application**, renseigner le numéro,
   le code et les instructions ci-dessous en remplaçant les deux champs entre chevrons.

En production, l’app transmet `challenge_bound: true` à l’envoi et renvoie le `challenge_id` à la vérification.
L’accès Premium offert est calculé à partir du marqueur serveur `is_review_account` par la projection
centrale des droits : aucun achat, facture ou abonnement fournisseur n’est créé. Un compte de revue
déjà inscrit bénéficie aussi de cet accès après la migration ; actualiser la page d’abonnement ou
se reconnecter dans l’app pour rafraîchir son affichage. Le profil et les consentements restent à remplir.

### Instructions en anglais pour Play Console

```text
Open the app and choose Sign in. Select France (+33) and enter <REVIEW_PHONE_NUMBER>.
Request a verification code, then enter <REVIEW_OTP>.
This is a dedicated review account. No SMS or access to a physical phone is required.
The same credentials can be reused for subsequent sign-ins.
Complimentary Premium access is already included. No purchase is required.
The account profile has been prepared for review.
```

Ne conserver la dernière phrase qu’après avoir effectivement préparé le compte.

## Contrat et sécurité

- Seuls `POST /api/auth/otp/send` et `POST /api/auth/otp/verify` traitent cette exception mobile.
- Pour ce numéro activé, `/send` conserve validation du corps, UUID d’idempotence, délai de renvoi
  et quotas IP/numéro. Il enregistre une intention temporaire avec `registration`, sans téléphone
  chiffré ni événement outbox : aucun appel Sweego ou budget SMS consommé. Le rejeu garde le délai
  initial ; changer le parcours avec le même UUID renvoie le conflit d’idempotence habituel.
- `/verify` compare le code en temps constant. Un mauvais code n’effectue aucune création de compte.
  Les quotas restent appliqués, même avec le bon code : par défaut cinq vérifications par heure
  par IP et numéro. Éviter les demandes et tentatives inutiles pendant la préparation.
- Le compte porte `is_review_account=true`. Sa création exige que la dernière intention du numéro
  soit une inscription encore valide (`OTP_TTL`) ; une intention de connexion ou expirée ne permet
  aucune création. Les connexions suivantes retrouvent le même compte ;
  un utilisateur normal existant avec ce numéro ne peut jamais être adopté par cette exception.
- Création, contrôle de l’état et émission de la session sont atomiques. Un tombstone apparu
  pendant l’insertion fait aussi annuler la création. Compte banni, supprimé ou
  téléphone couvert par un tombstone : refus. Une contrainte PostgreSQL interdit les rôles
  `admin` et `superadmin`, et la route de promotion renvoie son refus habituel.
- La date de fin borne la connexion et la durée des sessions de revue. Un rafraîchissement ou une
  reprise de rafraîchissement conserve cette limite. Le JWT d’accès est également plafonné à cette date.
- La découverte, les swipes et les conversations n’autorisent que des interactions entre comptes du
  même groupe. Le compte de revue affiche un feed vide tant qu’aucun autre profil de revue éligible
  n’existe. Prévoir des profils fictifs de revue pour présenter les interactions à Google.
- Seuls les comptes marqués pour la revue, de rôle `user`, non bannis et non supprimés reçoivent
  le Premium offert. `GET /api/users/me/subscription` renvoie `plan: "premium"`, `access_granted: true`,
  `provider: null` et aucune fin de période pour cet accès interne. L’app, les continuations et
  le dashboard utilisent les mêmes droits. Les protections contre un second achat s’appliquent.
  Les abonnements fournisseurs existants restent intacts ; un abonnement payant actif conserve
  sa projection habituelle et l’accès offert prend le relais s’il ne donne plus accès à Premium.
  Aucun revenu fictif n’est ajouté aux statistiques.
- Pour fermer l’accès avant l’échéance, désactiver `GOOGLE_PLAY_REVIEW_ENABLED` et recréer les instances
  de l’API. Au démarrage, l’API révoque les sessions du cycle fermé et efface leurs inscriptions push.
  Un changement de numéro, de code ou d’échéance ouvre un autre cycle et révoque les sessions du précédent.
- Renouveler le code à chaque cycle. Si Google doit continuer à accéder au compte, renouveler la fenêtre
  et les identifiants dans Play Console avant l’échéance. Google demande un accès disponible à tout
  moment. [Exigences Google Play](https://support.google.com/googleplay/android-developer/answer/15748846?hl=en)

## Vérification

```bash
cargo test --locked --lib --test otp_sweego_contract --test mobile_auth_contract --test billing_contract
cargo build --locked --bin db-migrate
python3 scripts/verify-continuation-postgres.py --native-migrator target/debug/db-migrate \
  --test play_review_postgres --test otp_delivery --test administration_reports_postgres
```

Les fixtures utilisent des identifiants fictifs distincts du `.env`. Le lanceur PostgreSQL désactive
explicitement l’accès de revue local pour ses autres tests et supprime sa base temporaire à la fin.
