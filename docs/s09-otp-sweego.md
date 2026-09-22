# S09 — OTP et Sweego

## Analyse de l’existant NestJS

`AuthController` expose deux routes publiques. `POST /api/auth/otp/send` normalise d’abord le téléphone pour construire la clé de quota, applique les quotas IP puis téléphone, valide ensuite `Idempotency-Key`, persiste une intention et effectue au plus un POST Sweego. `POST /api/auth/otp/verify` applique les mêmes quotas séparés avant de valider le format du code, consomme l’OTP de façon irréversible, retrouve ou crée le compte et émet une famille de refresh S08.

`OtpRepository` protège toutes les transitions d’un téléphone avec `pg_advisory_xact_lock(hashtextextended(phone_hash, 0))`. Une demande commence en `pending`, devient `accepted` après l’accusé HTTP, `sent` après un callback authentifié, `failed` après un rejet certain ou `unknown` après une issue réseau incertaine. Seuls `accepted` et `sent` sont consommables. Un essai plus récent accepté invalide les anciens, y compris lorsqu’un callback ancien arrive en retard. Un essai plus récent définitivement rejeté ne détruit pas un ancien code encore utilisable.

`SweegoSmsService` envoie un unique POST JSON avec `campaign-id` égal à l’identifiant de livraison. Il ne suit pas les redirections, borne le délai et la réponse à 16 384 octets, et ne relance jamais une requête dont l’issue est inconnue. Les statuts de rejet documentés deviennent `failed`; les autres erreurs HTTP, réseau ou de réponse deviennent `unknown`.

`SweegoWebhookService` vérifie le HMAC-SHA256 sur `webhook-id.webhook-timestamp.corps-brut` avec le secret décodé en base64. La fenêtre est de cinq minutes dans le passé et une minute dans le futur. Seuls `sms_sent` et `sms_undelivered` peuvent modifier un OTP. Le payload, le téléphone fournisseur et les détails libres sont validés puis abandonnés sans persistance.

Les comportements implicites importants sont l’ordre des pipes et des quotas, le rejet des champs DTO inconnus, la lecture de l’expiration après acquisition du verrou, l’absence de retry fournisseur, la consommation OTP avant la création de compte et la distinction entre acceptation Sweego et livraison au terminal.

## Mapping NestJS → Rust

| NestJS | Rust | Responsabilité |
| --- | --- | --- |
| `OtpService` | `identity::mobile::otp::OtpService` | Normalisation, HMAC, génération du code, idempotence et classification des issues. |
| `OtpRepository` | `identity::mobile::otp::OtpRepository` | Transactions SQLx, verrou téléphone, transitions, consommation et agrégats. |
| `SmsDelivery` | trait `identity::mobile::sweego::SmsDelivery` | Frontière asynchrone minimale et testable du fournisseur SMS. |
| `SweegoSmsService` | `identity::mobile::sweego::SweegoSmsService` | POST HTTP borné, sans redirection ni retry, parsing strict de l’accusé. |
| `SweegoWebhookService` | `identity::mobile::sweego::SweegoWebhookService` | Signature brute, DTO fournisseur fermé, métriques et projection d’événement. |
| `AuthRepository` pour OTP | `identity::mobile::account::MobileAccountRepository` | Recherche/création du compte, tombstone et conflit d’unicité. |
| `AuthService.verifyOtp` | `identity::mobile::account::MobileLoginService` | Consommation, compte et émission de tokens via S08. |
| `AuthController` / `SweegoWebhookController` | `otp_routes` / `sweego_routes` | DTO Serde stricts, ordre des quotas et mapping `ApiError`. |

Les traits sont des frontières de test et d’infrastructure. Il n’y a ni conteneur DI, ni décorateur simulé, ni exception utilisée comme flux de contrôle.

## Contrat HTTP conservé

| Méthode | Route | Entrée | Succès | Authentification |
| --- | --- | --- | --- | --- |
| POST | `/api/auth/otp/send` | JSON strict `{ phone_number }`, header `Idempotency-Key` UUID v4 | `202 { "message": "Verification code request accepted." }` | Publique ; quotas `otp-send-ip` puis `otp-send-phone`. |
| POST | `/api/auth/otp/verify` | JSON strict `{ phone_number, otp }` | `200 { access_token, refresh_token }` | Publique ; quotas `otp-verify-ip` puis `otp-verify-phone`. |
| POST | `/api/auth/sweego/webhook` | Corps brut et trois headers Sweego uniques | `200 { "received": true }` | HMAC fournisseur ; quota dédié `sms-webhook`, sans quota global. |

Les codes spécifiques sont conservés : `invalid_phone_number`, `invalid_otp_request`, `invalid_or_expired_otp`, `invalid_idempotency_key`, `idempotency_key_conflict`, `otp_delivery_unavailable`, `otp_delivery_unknown`, `account_unavailable`, `account_creation_conflict`, `invalid_sweego_signature`, `invalid_sweego_event`, `sweego_delivery_conflict`, `sweego_webhook_unavailable` et les erreurs de quota existantes.

## Invariants et cas limites

- Le téléphone accepté est strictement français E.164 après suppression de ` `, `.`, `(`, `)` et `-`. Il n’est ni persisté ni journalisé en clair.
- Un téléphone invalide est rejeté avant les quotas dédiés. Avec un téléphone valide, une clé d’idempotence invalide est rejetée après les deux quotas, comme dans NestJS.
- Un OTP contient exactement six chiffres. Sa validation métier intervient après les quotas HTTP.
- Le même `Idempotency-Key` et le même téléphone ne provoquent jamais un second POST. La même clé avec un autre téléphone renvoie `409`.
- Un callback peut confirmer une demande `unknown`, mais ne réactive jamais un code consommé, expiré, remplacé ou en échec définitif.
- `sms_undelivered` est absorbant. `sms_sent` signifie seulement que le callback signé a été reçu ; il ne prouve pas la réception sur le téléphone.
- Une erreur PostgreSQL après acceptation fournisseur devient `otp_delivery_unknown`. Aucun second POST n’est lancé.
- L’OTP est consommé avant la recherche/création du compte. Un tombstone, un conflit concurrent ou une réponse perdue ne rendent pas le code réutilisable.
- Les métriques webhook sont des compteurs bornés par résultat et ne contiennent aucun identifiant.

## Fichiers

- `src/identity/mobile/otp.rs` : domaine OTP, service et repository PostgreSQL.
- `src/identity/mobile/sweego.rs` : client fournisseur, signature, DTO callback et compteurs.
- `src/identity/mobile/account.rs` : recherche/création du compte après OTP.
- `src/identity/mobile/http.rs` : trois routes, DTO et erreurs publiques.
- `tests/otp_sweego_contract.rs` : contrat Axum, ordre des quotas, replay, création de compte et raw body.
- `tests/sweego_client.rs` : faux serveur HTTP, payload, absence de retry et classification des réponses.
- `tests/sweego_signature.rs` : fenêtre de replay et filtrage des événements.
- `tests/otp_delivery.rs` : transitions et courses sur PostgreSQL réel.

## Validation

Sans infrastructure :

```powershell
cargo test --locked --test otp_sweego_contract
cargo test --locked --test sweego_client
cargo test --locked --test sweego_signature
cargo test --locked
cargo clippy --locked --all-targets --features postgres-integration -- -D warnings
```

Avec PostgreSQL local `histae-dev`, préparé par le dépôt NestJS :

```powershell
cargo test --locked --features postgres-integration --test otp_delivery
```

Le test PostgreSQL refuse tout environnement autre que `development`, toute base autre que `histae-dev` et tout hôte non loopback. Il utilise des identifiants aléatoires et un nettoyage ciblé.

## Limites du lot

Le binaire `api` reste volontairement non activé avant le lot de composition/démarrage prévu par le plan. Les routeurs S09 sont compilables et testés isolément, mais ne sont pas encore servis par le processus final. Les variables Sweego sont déjà typées par S04 et présentes dans le `.env` local Rust. La pile Docker Rust reste un livrable des lots d’exploitation ultérieurs. Aucun appel réel à Sweego n’est effectué par les tests.
