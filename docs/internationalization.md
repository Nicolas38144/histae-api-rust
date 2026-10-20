# Internationalisation de l’API

[Index](README.md) · [Contrat HTTP](http-contract.md) · [Configuration](configuration.md)

## Sélection de langue

Les langues prises en charge sont `fr`, `en`, `es`, `it`. Les valeurs enregistrées sont ces codes exacts.

1. Un en-tête `Accept-Language` explicite prime pour cette requête. Les variantes (`it-IT`), priorités `q` et l’ordre à priorité égale sont pris en charge.
2. Sans en-tête, les routes authentifiées localisées utilisent la préférence du compte.
3. Le catalogue public et les OTP sans en-tête utilisent le français. Un en-tête invalide ou sans langue prise en charge revient aussi au français.
4. Chaque libellé sans traduction se replie individuellement sur sa valeur française.

Les réponses concernées ont `Content-Language` (langue négociée, même en présence de replis) et `Vary: Accept-Language` ; `Cache-Control: no-store` reste appliqué. CORS accepte cet en-tête. Les clés JSON, UUID, codes métier, erreurs techniques, catégories, quotas, montants, ordre et curseurs restent stables. Les bios, réponses et messages des personnes ne sont jamais traduits.

```http
PATCH /api/users/me/language
Authorization: Bearer <access_token>
Content-Type: application/json

{"language":"it"}
```

`GET` sur ce même chemin lit `{ "language": "it" }`. Les deux routes exigent une session active, acceptent un onboarding incomplet, partagent une limite de 60 requêtes/minute/compte et refusent les champs inconnus. Un changement de langue ne valide aucun consentement. L’effacement remet la préférence à `fr`.

## Routes concernées

| Routes | Effet |
| --- | --- |
| `GET /api/plans` | Noms des plans, fonctionnalités, descriptions et offres mensuelles/annuelles. |
| `GET /api/traits`, `GET /api/users/me/traits` | Noms des traits. |
| `GET /api/profile-questions` | Intitulés des questions. |
| `GET`, `PUT /api/users/me/profile-answers` | Questions associées, réponses personnelles intactes. |
| `GET /api/users/me` | Questions dans `profile_answers`. |
| `GET /api/feed`, `GET /api/matches/me` | Traits et questions des profils, sans modification des exclusions, contrôles, tri ou pagination. |
| `GET /api/admin/users/:id` | Traits du compte consulté après autorisation et audit. |
| `GET /api/admin/content-moderation/:id` | Question de catalogue associée au contenu, contenu personnel intact. |
| `GET`, `POST`, `PATCH /api/admin/traits[/:id]` | Cartes de traductions et édition du catalogue (voir ci-dessous). |
| `GET`, `POST`, `PATCH /api/admin/profile-questions[/:id]` | Idem pour les questions. |
| `DELETE /api/admin/traits/:id`, `DELETE /api/admin/profile-questions/:id` | Suppression en cascade des traductions. |
| `GET`, `PUT /api/users/me/consents` | URL et langue réelle du document proposé, langue historique du consentement. |
| `POST /api/auth/otp/send` | Texte SMS dans la langue négociée ; numéros français `+33` uniquement, comme auparavant. |
| `POST`, `GET /api/users/me/devices` | Préférence facultative `language` par appareil. |
| `POST /api/users/me/subscription/checkout`, `POST /api/users/me/subscription/portal` | Paramètre Stripe `locale` et pages de retour Histae localisées. |
| `GET /api/users/me/data-export` | Le fichier exporté conserve ses clés techniques et inclut la langue du compte, les preuves de consentement et les métadonnées d’appareil (sans jeton push). |

Les notifications push sont localisées à la livraison ; le flux SSE reste un protocole technique sans traduction.

## Catalogues et dashboard

Le français reste la source canonique de `name` / `prompt`. Les réponses administratives ajoutent `translations`, une carte des traductions disponibles, français inclus. Le dashboard reste en français et demande explicitement `Accept-Language: fr`. Il édite les trois autres langues et affiche leur couverture.

```json
{
  "name": "Curieux",
  "translations": {
    "en": "Curious",
    "es": "Curioso",
    "it": "Curioso"
  }
}
```

- Une carte absente ou vide préserve les traductions existantes.
- Une clé absente préserve sa valeur ; une chaîne définit la traduction ; `null` la supprime et rétablit le repli français.
- `fr`, si fourni, doit correspondre au champ canonique et ne peut pas être supprimé. Une modification de question peut fournir uniquement des traductions.
- Les mêmes limites et normalisations que les libellés canoniques s’appliquent ; les textes vides ou les caractères de contrôle sont refusés.
- Les contrôles de session admin, d’origine et de rôle restent obligatoires. L’écriture du libellé et des traductions est transactionnelle. La réponse de modification d’une question est lue dans cette transaction, sous le même verrou.
- Les traductions initiales couvrent les traits et questions livrés, les plans et leurs fonctionnalités. Une question personnalisée avant la migration conserve un repli français : aucune traduction du catalogue d’origine n’est attribuée à un autre texte. Le catalogue traduit est initialisé lors de son introduction ; les migrations suivantes préservent aussi les traductions supprimées.

Seuls les catalogues publics sont mis en cache en mémoire, pendant 15 secondes. Les écritures HTTP invalident le cache du processus ; les autres instances convergent après ce délai. Les préférences privées et preuves de consentement ne sont pas mises en cache. La projection de compte déjà lue par le contrôle de session fournit la langue mobile : aucun aller-retour SQL supplémentaire n’est ajouté au feed pour cette préférence.

## Documents et consentements

`document_language` indique la langue **réellement disponible** du document à l’URL fournie. `accepted_document_language` indique la langue enregistrée avec le consentement existant ; ce champ est absent avant toute décision. La version documentaire reste commune aux quatre traductions et doit être augmentée lors d’un changement juridique de fond.

Pour les chemins juridiques connus sur `histae.com` / `www.histae.com`, l’API sélectionne le chemin existant `/fr/…`, `/en/…`, `/es/…` ou `/it/…`. Les URL personnalisées gardent leur langue française, sauf configuration explicite :

```dotenv
TERMS_OF_SERVICE_URL_EN=https://example.org/en/terms
PRIVACY_POLICY_URL_EN=https://example.org/en/privacy
SENSITIVE_DATA_CONSENT_URL_EN=https://example.org/en/sensitive-data
LOCATION_CONSENT_URL_EN=https://example.org/en/location
```

Les suffixes `_ES` et `_IT` sont aussi disponibles. Ces douze variables sont facultatives, validées au démarrage (HTTPS requis en production, sans identifiants dans l’URL). Le document effectivement présenté est enregistré à chaque nouvelle décision. Répéter une même décision/version conserve la preuve initiale, y compris sa langue. Les anciennes décisions sont migrées en français. Les traductions juridiques doivent rester cohérentes avec la version validée ; la préférence linguistique ne vaut pas consentement.

## OTP, push et facturation

- La langue OTP fait partie de la tentative persistée. Réutiliser une clé d’idempotence avec une autre langue produit un conflit, sans nouvel appel au fournisseur. Utiliser une nouvelle clé pour une nouvelle tentative volontaire.
- L’enregistrement d’appareil accepte `language: "es"`. Une valeur absente ou `null` suit la préférence du compte au moment de la livraison. Réenregistrer un appareil peut donc définir ou enlever son exception. Les textes push restent génériques et n’incluent ni message privé ni prénom.
- Une nouvelle demande Stripe reçoit la langue négociée. Le payload Checkout complet est persisté avant l’appel : une reprise réutilise **sa langue et ses URL d’origine**, même si le compte ou l’en-tête change. Les anciennes tentatives sans `locale` continuent à omettre ce paramètre auprès de Stripe.
- Seules les URL de retour des chemins de facturation connus sur les domaines Histae sont adaptées ; une URL personnalisée reste inchangée. Le placeholder `{CHECKOUT_SESSION_ID}` est préservé.

## Modération et responsabilités

La politique `text_rules_v2_multilingual` vérifie conjointement les lexiques français, anglais, espagnols et italiens, quelle que soit la langue déclarée. Elle conserve la normalisation Unicode, la détection des contacts et la mise en attente pour examen humain. Elle s’applique aux nouvelles bios/réponses et modifications ; les anciens contenus ne sont pas automatiquement reclassés. Ces règles déterministes ne constituent pas une compréhension exhaustive des langues ou de leurs ambiguïtés. La conversation privée ne reçoit aucun nouveau traitement.

- `localization/language.rs` : codes et négociation, sans I/O.
- `localization/catalog.rs`, `copy.rs`, `legal.rs` : projections typées, textes et résolution des URL.
- `localization/store.rs` / `pg.rs` : contrat de stockage et implémentation SQL.
- `localization/mod.rs` : orchestration et cache du catalogue.
- `localization/http.rs` : extraction HTTP, préférence du compte et en-têtes.
- Les services de consentement, OTP, facturation et notification possèdent leurs invariants métier ; leurs dépôts gardent les transactions et les accès SQL.

## Livraison et validation

Le migrateur sait créer une base neuve et mettre à niveau une base existante sans effacer ses données. Il ajoute quatre colonnes et quatre tables de traduction avec contraintes, clés étrangères en cascade et privilèges du rôle applicatif existant. Migrer avant de démarrer la nouvelle API, puis déployer le dashboard compatible ; aucun changement des images PostgreSQL ou de PostGIS n’est nécessaire.

Commandes de vérification :

```bash
cargo fmt --all --check
cargo test --locked
cargo clippy --locked --all-targets --all-features -- -D warnings
python3 scripts/verify-continuation-postgres.py --api-image histae-api-rust:local
```

Les tests utilisent des fournisseurs simulés et une base jetable ; ils ne déclenchent aucun envoi SMS ni paiement réel. Importer l’une des deux collections Postman puis définir `language` (`fr` par défaut). Désactiver l’en-tête pour vérifier le repli sur la préférence du compte. Tester aussi une traduction supprimée (`null`), un compte sans onboarding, une session révoquée, le changement de langue après consentement et la reprise d’une tentative Checkout.

### Vérifications locales du 20 octobre 2026

| Contrôle | Résultat |
| --- | --- |
| Suite Rust par défaut | 328 tests réussis |
| Régressions PostgreSQL avec rôles limités | 45 tests réussis ; reprise ciblée des 2 scénarios multilingues après intégration de la langue dans le contrôle de session |
| Migrations | Base neuve, ancien schéma, couverture des traductions, question déjà personnalisée, modification et suppression de traduction préservées après réexécution |
| Compilation Rust | Tous les binaires compilés ; contrôle sans features par défaut réussi ; formatage et Clippy sur toutes les cibles/features réussis |
| Dashboard | 77 tests réussis ; typage, lint et build de production réussis |
| Contrat et Postman | 113 routes cohérentes avec les deux collections JSON |

PostgreSQL a tourné dans un conteneur jetable avec le migrateur natif compilé depuis ces sources.
Les fournisseurs SMS, Stripe et FCM ont été simulés. Aucun SMS ni paiement réel n’a été déclenché.
Le site possède déjà les quatre variantes de ses documents et retours Stripe ; les chemins de l’API
utilisent ces routes existantes. Le dépôt mobile n’a pas été modifié.
