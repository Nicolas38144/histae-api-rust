# S03 — décision sur le codec photo

## Comportement NestJS observé

`PhotoProcessorService` valide d’abord une entrée d’au plus 500 000 octets. Il exige une extension parmi `.jpg`, `.jpeg`, `.png`, `.heic`, `.heif` et `.webp`, un MIME compatible, puis une signature de conteneur compatible avec l’extension. `application/octet-stream` est accepté pour chaque format.

JPEG, PNG et WebP sont lus par Sharp avec un plafond de 40 000 000 pixels, une seule page et l’auto-orientation. HEIC/HEIF lit d’abord les métadonnées, puis décode les pixels RGBA dans un worker `heic-decode` arrêté après 30 secondes. Le tampon RGBA doit mesurer exactement `width × height × 4`.

La sortie est un WebP sans agrandissement. Six couples bord/qualité sont essayés, de 2 048/82 à 1 024/50, jusqu’à obtenir au plus 500 000 octets. La réponse interne contient les octets, `image/webp`, leur taille, les dimensions après orientation et le SHA-256. Une entrée ou un décodage invalide devient `InvalidPhotoError`; une entrée ou sortie trop grande devient `PhotoTooLargeError`.

## Mapping NestJS → Rust

| NestJS | Prototype Rust |
| --- | --- |
| Validation extension, MIME, signature et 500 kB | `validate_upload` avant tout processus |
| `PhotoProcessorService.toWebp` | processus Node `photo-codec-runner.cjs` et processeur autonome |
| Worker HEIF et timeout 30 s | timeout interne conservé, plus arrêt du processus complet à 35 s |
| tampon `Buffer` | pipes stdin/stdout bornés, sans fichier temporaire |
| `ProcessedPhoto` | type Rust explicite et SHA-256 recalculé par le parent |
| concurrence implicite de libuv | sémaphore Rust à une conversion simultanée par instance du prototype |

## Décision

Le codec TypeScript est maintenu comme pont de migration. Les options Rust pures évaluées ne fournissent pas ensemble le décodage HEIC/HEIF, l’auto-orientation et un encodage WebP lossy compatible avec Sharp. Des bindings `libheif` et `libvips` déplaceraient le risque vers l’ABI native et ne garantiraient pas le même rendu, les mêmes métadonnées ou les mêmes erreurs.

Le processus isolé préserve la version réellement éprouvée ici : Sharp 0.35.4, libvips 8.18.6, libheif 1.23.2 et libwebp 1.6.0. Un crash, une fuite native ou une annulation libère le processus entier. Le parent ne conserve au plus que 500 000 octets d’entrée et 500 000 octets de sortie par conversion. Le décodage HEIF peut toutefois allouer environ 160 Mo rien que pour le RGBA au plafond de pixels, avant les buffers de conversion ; le sémaphore est donc une contrainte d’exploitation, pas une optimisation.

S15 a remplacé le chargement du dépôt Nest par `tools/photo-codec/processor.cjs`. Le package et son lockfile épinglent Sharp 0.35.4 et `heic-decode` 2.1.0 ; les pipes et les deux plafonds de 500 000 octets restent contrôlés par Rust. L’image finale devra installer ce package en mode production et conserver Node 22, point traité avec les artefacts de déploiement.

## Parité vérifiée

Les tests Rust exécutent les fixtures Nest JPEG, JPEG alternatif, PNG, WebP, HEIC et HEIF. Ils vérifient aussi :

- auto-orientation EXIF 6 et suppression des chunks de métadonnées source ;
- refus d’un WebP animé à deux pages ;
- refus d’un JPEG valide dépassant 40 millions de pixels ;
- contenu corrompu malgré une signature JPEG ;
- extension, MIME, signature et limite exacte d’entrée ;
- sortie WebP, dimensions maximales, taille maximale et SHA-256 ;
- arrêt d’un processus bloqué et d’un processus produisant plus de 500 000 octets.

## Limites restantes

- Le timeout externe et l’indisponibilité du processus sont traduits par S15 en `400 invalid_photo` avec le message de décodage stable ; seul le code d’événement sûr `photo_codec_unavailable` est journalisé.
- Le `Dockerfile` final doit installer uniquement les dépendances de production du codec autonome ; il appartient à S27.
- Le nombre global de conversions doit être borné entre instances, ou dimensionné avec le rate limit photo et la mémoire du serveur. Le sémaphore local protège une seule instance.
- Tout remplacement futur par un codec natif devra repasser ce corpus. Une différence d’octets est acceptable seulement après vérification des dimensions, orientation, suppression des métadonnées, modération et limite de 500 kB, puis approbation explicite.
