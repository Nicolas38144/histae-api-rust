# Architecture et conventions Rust

[Index](README.md) · [Contrat HTTP](http-contract.md) · [Tests](../tests/README.md)

## Organisation par domaine

Le projet reste un seul crate. Les modules métier regroupent leurs règles, leur contrat HTTP et leurs accès aux
données. Les modules simples restent plats ; un sous-répertoire regroupe une fonctionnalité lorsque plusieurs
responsabilités doivent être séparées.

| Fichier | Responsabilité |
| --- | --- |
| `mod.rs` | Déclaration des modules et façade publique, sans assemblage global |
| `domain.rs` | Types, états fermés, représentations métier et règles locales |
| `error.rs`, lorsque partagé | Erreurs métier utilisées par plusieurs ports ou adaptateurs |
| `store.rs` | Contrats de persistance nécessaires aux services |
| `service.rs` | Cas d’usage, invariants, coordination et erreurs métier |
| `pg.rs` ou `pg/` | SQL, transactions, verrous et mapping des lignes |
| `http.rs` | DTO/extracteurs, authentification, routeur Axum et erreurs HTTP |
| Adaptateur nommé (`stripe.rs`, `s3.rs`, etc.) | Protocole et limites du fournisseur |

Il n’est pas nécessaire de créer chacun de ces fichiers pour une fonctionnalité minuscule. Certains types de
projection utilisent Serde directement lorsqu’ils représentent déjà exactement le contrat public. Les erreurs
techniques normalisées existantes sont conservées dans les ports ; aucun accès au pool SQL n’est nécessaire
pour tester un service avec un store contrôlé.

Les services dépendent des contrats, pas des repositories PostgreSQL concrets. Le code HTTP traduit les erreurs
au bord de l’application avec `http::error::ApiError`. Le SQL reste au plus près du domaine qui possède la
transaction. `infra/` fournit les mécanismes communs, pas un repository générique universel.

## Carte des modules

| Module | Responsabilité |
| --- | --- |
| `app`, `config`, `http`, `infra` | Assemblage, configuration, transport partagé, PostgreSQL/Redis/cryptographie |
| `identity` | OTP, sessions mobiles, WebAuthn et sessions admin |
| `profiles`, `catalog`, `discovery` | Profils/préférences, traits/questions/plans, feed et swipes |
| `matches` | Matchs, continuation, messages et échéances |
| `media`, `moderation` | Photos privées, codec, stockage et modération de contenus |
| `billing`, `advertising`, `notifications` | Stripe/réconciliation, récompenses AdMob, notifications durables, push et SSE |
| `privacy`, `reports`, `administration` | Consentements/droits/effacement, signalements et gestion admin |
| `outbox`, `operations`, `shared` | Effets durables, suivi/metrics/logs et fonctions partagées |

`tests/` contient contrats et intégrations ; `db/` le schéma ; `docker/` et `services/` les dépendances
conteneurisées ; `tools/photo-codec/` le processus Node/Sharp autonome. Les procédures de compilation et
les binaires sont décrits dans [Développement](development.md) et [Exploitation](operations.md).

## Assemblage et ressources

`src/bin/` transmet les arguments au code applicatif. `src/app/mod.rs` sélectionne le composant, charge la
configuration et initialise les logs. `src/app/lifecycle.rs` porte les signaux, l’annulation et la supervision.

L’API est répartie en trois fichiers :

- `src/app/api/resources.rs` ouvre et ferme les ressources partagées ;
- `src/app/api/router.rs` construit explicitement les services et monte les routeurs ;
- `src/app/api/mod.rs` gère les listeners HTTP/métriques et l’arrêt.

`src/app/outbox.rs` assemble les cinq handlers d’effets ; `src/app/maintenance.rs` assemble une passe bornée de
maintenance. Ces compositions restent explicites et distinctes, car leurs besoins et cycles de vie diffèrent.
Les binaires de migration et de bootstrap ne passent pas par le démarrage de l’API.

Les ressources partagées immuables utilisent `Arc`. Les verrous métier restent dans PostgreSQL. Un mutex
en mémoire ne remplace jamais un verrou transactionnel distribué. Les futures des ports existants restent
`Send` ; les services partagés restent `Send + Sync`.

## Sous-domaines structurés

- `identity/mobile/otp/` sépare états de livraison, port, service et transactions OTP.
- `advertising/{domain,error,store,http,pg,ssv}.rs` sépare les types métier, le port de persistance, les requêtes propriétaires, l’octroi transactionnel et la vérification AdMob sur les octets signés ; le mobile ne décide jamais du solde.
- `identity/pg_guards.rs` porte les verrous de compte/consentement des transactions de matchs et de récompenses, ainsi que le verrou ordonné par paire partagé entre création de match et blocage.
- `identity/admin/webauthn.rs` encapsule la vérification cryptographique.
- `billing/reconcile/` sépare modèles, ports, service, fournisseur Stripe, PostgreSQL, HTTP et worker.
- `billing/webhook/pg.rs` isole la projection transactionnelle des événements signés.
- `matches/pg/{matches,messages,access,continuation,credits}.rs` sépare les repositories et les helpers de verrouillage qui reçoivent
  la connexion de l’appelant.
- `administration/{photos,metrics}/` sépare règles, contrats et SQL de chaque fonctionnalité.
- `privacy/{rights,export,erasure}/` regroupe les workflows indépendants avec leur transport et leur persistance.
- `outbox/admin/` sépare les décisions opérateur de leur audit transactionnel.
- `media/codec.rs` contient l’adaptateur du codec autonome dans `tools/photo-codec/`.

Les plateformes et états publicitaires utilisent des enums Serde, tout en conservant les chaînes du contrat HTTP.
`EffectivePlan.weekly_credits` désigne les crédits inclus ; `ContinuationQuota.weekly_limit` reste le plafond public de continuations.
Le nom historique de la colonne SQL `subscription_plan.weekly_continuation_limit` est conservé pour les installations existantes.
Les plafonds applicatifs sont définis dans le domaine et les valeurs SQL variables utilisent des paramètres liés.

## Compatibilité et sécurité

Les changements internes doivent respecter les routes, DTO, autorisations, invariants et règles de rejeu
du [contrat HTTP](http-contract.md). Toute évolution observable doit mettre à jour contrat, collections et tests.
Les effets réseau gardent les intentions durables et checkpoints existants ; aucune abstraction de repository
ne doit les déplacer avant le commit métier.

La configuration distingue les types (`types.rs`), la lecture (`environment.rs`), le chargement ordonné
(`loader.rs`), les validateurs (`validation.rs`) et les erreurs normalisées (`error.rs`). Le `Debug` de la source
d’environnement affiche uniquement un nombre d’entrées ; `SecretString` conserve son affichage expurgé.

Le crate interdit `unsafe` et refuse les `unwrap()`/`expect()` des chemins non test lors de Clippy. Les erreurs
publiques conservent `{ "error": { "code", "message" } }`. Les logs utilisent les événements et champs autorisés
de `operations::logging`, jamais les valeurs de configuration ou les erreurs fournisseurs brutes.

## Arrêt et supervision

Le superviseur annule toutes les tâches puis attend leur nettoyage, même si l’une a échoué. Le premier échec
est retourné après le drain. Après expiration du délai, il annule les tâches restantes et attend leur terminaison.
Une sortie inattendue du worker est remontée au processus au lieu de laisser un service apparemment actif.

L’arrêt de l’API ferme explicitement les flux SSE, y compris si la vérification de session attend la base, avant
de drainer HTTP. Le serveur de métriques annule les rendus en cours ; à l’échéance, sa tâche est arrêtée et
rejointe au lieu d’être détachée. Les pools sont fermés après le traitement des listeners.

Le cycle de vie est couvert par les tests de drain, de sessions SSE bloquées et de scrapes interrompus.

## Documentation et tests

L’[index documentaire](README.md) regroupe les guides courants.
Les suites de tests sont nommées par domaine, sans numéros de migration. Les tests unitaires restent proches
des règles ; les contrats HTTP et les intégrations réelles résident dans `tests/`.

Les règles d’exécution et les limites de preuve sont décrites dans [tests/README.md](../tests/README.md).

## Confirmation et récompenses

Les règles sont centralisées dans le [guide des continuations](continuation-and-rewards.md). Le match est verrouillé avant les comptes/consentements, abonnements et wallets ordonnés. Une décision Free réserve un crédit ; les deux débits et la confirmation forment une seule transaction. Le trigger d’abonnement partage les verrous du wallet pour sérialiser les changements Premium. Une récompense publicitaire vérifiée suit l’ordre compte → wallet → intention. Les projections et exports ne publient pas le choix individuel de l’autre personne.

La création d’un match et le blocage partagent un verrou transactionnel sur la même paire d’UUID triés,
avant les verrous de ligne. Un blocage attend ainsi le commit d’un match en cours de création avant de le
clôturer. Les opérations mobiles vérifient également le blocage à l’accès : un ancien match incohérent
ne permet ni lecture, ni message, ni révélation ou continuation.
