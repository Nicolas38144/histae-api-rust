# Architecture et conventions Rust

## Organisation par domaine

Le projet reste un seul crate. Les modules métier regroupent leurs règles, leur contrat HTTP et leurs accès aux
données. Les modules simples restent plats ; un sous-répertoire regroupe une fonctionnalité lorsque plusieurs
responsabilités doivent être séparées.

| Fichier | Responsabilité |
| --- | --- |
| `mod.rs` | Déclaration des modules et façade publique, sans assemblage global |
| `domain.rs` | Types, états fermés, représentations métier et règles locales |
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
- `identity/admin/webauthn.rs` encapsule la vérification cryptographique.
- `billing/reconcile/` sépare modèles, ports, service, fournisseur Stripe, PostgreSQL, HTTP et worker.
- `billing/webhook/pg.rs` isole la projection transactionnelle des événements signés.
- `matches/pg/{matches,messages,access}.rs` sépare les repositories et les helpers de verrouillage qui reçoivent
  la connexion de l’appelant.
- `administration/{photos,metrics}/` sépare règles, contrats et SQL de chaque fonctionnalité.
- `privacy/{rights,export,erasure}/` regroupe les workflows indépendants avec leur transport et leur persistance.
- `outbox/admin/` sépare les décisions opérateur de leur audit transactionnel.
- `media/codec.rs` contient l’adaptateur du codec autonome dans `tools/photo-codec/`.

## Compatibilité et sécurité

Le refactoring conserve les routes, DTO, statuts, messages d’erreur, règles d’autorisation, SQL et ordre des
transactions. Il ne modifie ni migration SQL, ni durée de rétention, ni nom de variable d’environnement.
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

Ces changements concernent l’arrêt et le diagnostic interne. Ils ne changent pas le format des événements SSE
ni le contrat des requêtes servies pendant le fonctionnement normal.

## Documentation et tests

Le README décrit l’état courant. `docs/migration/` garde les décisions et preuves historiques S03–S29.
Les suites de tests sont nommées par domaine, sans numéros de migration. Les tests unitaires restent proches
des règles ; les contrats HTTP et les intégrations réelles résident dans `tests/`.

Les règles d’exécution et les limites de preuve sont décrites dans [tests/README.md](../tests/README.md).

## Validation du refactoring — 4 octobre 2026

- `cargo fmt --all -- --check` : réussi.
- `cargo clippy --locked --all-targets --all-features -j 2 -- -D warnings` : réussi.
- `cargo check --locked --all-targets -j 2` : réussi avec les features par défaut.
- `cargo test --locked --all-targets --all-features -j 2` : 286 tests réussis, dont 164 unitaires,
  avec PostgreSQL, Redis, S3 et le codec photo locaux.
- Inventaire des 102 couples méthode/chemin et smoke du binaire assemblé : réussis.
- Arrêt local : fermeture HTTP, métriques et ressources observée.
- Les 383 chaînes SQL recensées dans les sources de production avant le refactoring sont inchangées.

Le test outbox initial partageait la file locale et pouvait réclamer des événements étrangers à sa fixture.
Il s’exécute désormais dans une base temporaire migrée et nettoyée, y compris après une assertion en échec.
L’absence de base temporaire restante a été vérifiée après la suite. Cette correction ne change pas le worker
ni ses requêtes de production.

Les tests ne remplacent pas les validations fournisseurs réels, WebAuthn navigateur, charge et sécurité
indépendante documentées dans le guide de bascule.
