# Histae API Rust

Migration incrémentale de l’API NestJS Histae. Le backend Nest reste la référence exécutable jusqu’à la campagne de parité et la bascule de développement.

## S01 — comparaison de contrat HTTP

Le binaire `contract-compare` exécute séquentiellement le même corpus contre une API de référence et une API candidate. Il vérifie chaque cible contre le contrat explicite, puis compare les réponses. Les corps reçus ne sont jamais inclus dans le rapport d’écart.

Les deux URLs et les deux namespaces d’état doivent être distincts. Un scénario `isolated_mutation` est refusé sans `--allow-isolated-mutations`. Ce drapeau certifie seulement l’intention : la préparation effective de schémas PostgreSQL, namespaces Redis et préfixes S3 distincts reste à la charge du lanceur d’intégration de chaque lot.

```powershell
cargo run --bin contract-compare -- `
  --corpus tests/contract/corpus/smoke.json `
  --reference-url http://127.0.0.1:8080/ `
  --reference-state nest-contract-a `
  --candidate-url http://127.0.0.1:8081/ `
  --candidate-state rust-contract-b
```

Le code de sortie vaut `0` pour une parité complète, `1` pour des différences de contrat et `2` pour une configuration, un corpus ou un transport inexploitable. Le rapport JSON identifie scénario, cible et champ sans recopier de payload potentiellement sensible.

Les scénarios sont exécutés dans l’ordre du corpus. Une mutation peut donc être suivie de lectures qui vérifient ses effets HTTP observables sur chaque état isolé. Chaque cible possède son propre client et son propre jar de cookies afin de couvrir les parcours authentifiés sans fuite de session. Dans un corps `exact_json`, `dynamic_fields` catalogue par pointeur JSON les seules valeurs non déterministes tolérées (`any`, `uuid_v4`, `non_empty_string` ou `integer`) ; toutes les autres valeurs, l’ordre des tableaux, `null` et l’absence restent comparés exactement.

Le corpus initial fixe deux comportements communs observés dans NestJS : `GET /health/live` et l’enveloppe JSON d’une route inconnue. Il grandira avec chaque module migré. Les flux multipart, SSE et fournisseurs signés auront des adaptateurs spécialisés dans leurs lots ; ils ne sont pas normalisés silencieusement par ce harnais JSON.

## S04 — socle d’exécution

Le crate expose désormais quatre binaires Tokio distincts : `api`, `outbox`, `maintenance` et `admin-bootstrap`. Ils partagent une configuration typée et stricte, un superviseur de tâches avec annulation explicite et drain borné, ainsi qu’un formateur de logs à champs autorisés. Les secrets utilisent un type dont `Debug` est expurgé et les erreurs de configuration ne recopient jamais leur valeur.

Durant cette étape, aucun serveur HTTP, worker ou accès PostgreSQL n’est encore disponible. Pour éviter un faux état prêt, les binaires refusent donc leur lancement normal avec un code sûr `component_not_implemented`. Le mode suivant valide seulement la configuration et sort immédiatement :

```powershell
cargo run --bin api -- --check-config
```

Les variables et contraintes conservées depuis NestJS sont décrites dans [docs/s04-runtime.md](docs/s04-runtime.md). Les routes HTTP commencent avec S07, après le socle PostgreSQL S05 et les verrous S06.

## S03 — prototype du codec photo

Le prototype conserve temporairement `PhotoProcessorService` comme référence de conversion dans un processus Node isolé. Le parent Rust reproduit les contrôles extension/MIME/signature et borne l’entrée, la sortie, la concurrence et la durée du processus. Les octets circulent par pipes : aucune photo temporaire n’est créée sur disque.

Cette décision préserve exactement le pipeline Sharp/libvips pour JPEG, PNG, WebP, HEIC et HEIF, notamment l’orientation EXIF, le refus des animations, les limites de pixels et les six tentatives d’encodage WebP. Elle est détaillée dans [docs/s03-photo-codec.md](docs/s03-photo-codec.md).
