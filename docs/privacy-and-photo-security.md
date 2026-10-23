# Confidentialité de la localisation et capacité photo

[Index](README.md) · [Contrat HTTP](http-contract.md) · [Tests](../tests/README.md)

## Localisation

Le serveur valide les bornes géographiques, puis remplace les coordonnées par le centre d’une zone :
bandes de latitude de 0,05 degré, secteurs de longitude d’environ 5 km ou plus adaptés à la latitude,
et une seule zone par calotte polaire. Les valeurs stockées sont stables et normalisées à six décimales.
La précision de représentation numérique ne correspond pas à une précision GPS : aucun déplacement
à l’intérieur d’une zone ne modifie son centre. Un bruit aléatoire différent à chaque réponse permettrait
au contraire de moyenner les observations ; ce mécanisme n’en ajoute pas.

Le feed, ses exclusions spatiales, ses préférences réciproques et son classement utilisent ces centres.
Même en changeant sa position et en comparant distance, visibilité et ordre, un lecteur ne peut pas
retrouver une coordonnée GPS qui n’a jamais été stockée. Cela réduit la précision accessible sans
garantir l’anonymat d’une personne ni empêcher les déductions à partir de son profil ou de ses déplacements.
Les seuils de distance sont approximatifs aux frontières ; le contrat numérique `distance_km` est conservé
avec des paliers de 5 km, minimum 5. Les consentements, blocages, pause et exigences de fraîcheur restent actifs.

Une nouvelle zone peut être enregistrée toutes les cinq minutes. La décision est atomique, partagée entre
instances par PostgreSQL ; les actualisations dans la même zone ne repoussent pas le prochain changement.
Ce contrôle limite les changements rapides, sans constituer une preuve de localisation réelle.

La migration retire une seule fois les anciennes coordonnées précises et invalide leurs présences,
sans retirer les consentements. Après déploiement, l’application doit renvoyer la position ; une seconde
migration ne doit pas effacer la présence déjà minimisée. Les sauvegardes historiques conservent leur
contenu jusqu’à leur expiration selon la politique de rétention existante.

Pour cette première livraison, arrêter les anciennes instances API avant la migration afin qu’elles ne
réécrivent pas des positions précises pendant le remplacement des conteneurs. Depuis le dossier parent
`Histae`, pour la pile de production :

```bash
sudo docker compose --project-directory histae-api-rust --env-file histae-api-rust/.env \
  -f histae-api-rust/compose.yaml -f histae-api-rust/compose.production.yaml \
  -f histae-api-rust/compose.production-observability.yaml stop api
sudo bash histae-scripts/reset-api.sh --production
```

Le retrait du consentement supprime la présence et son compteur de changements. Le délai de cinq minutes
est un frein par présence, pas une protection anti-fraude persistante après retrait/réactivation ; la
minimisation des coordonnées protège aussi ce cas.

## Conversion des photos

Chaque instance admet une conversion active et au plus quatre requêtes en attente. L’attente est limitée
à cinq secondes, puis le traitement à trente-cinq secondes. Saturation, indisponibilité et timeout du
processus donnent un `503 photo_processing_unavailable` avec `Retry-After: 5`. Les permis sont libérés
sur erreur et annulation. Une erreur du convertisseur fait retirer sa réservation sans remplacer la photo courante.
Les limites de taille/pixels et l’isolation du processus Node restent appliquées.

## Liens photo et révocation

Les nouvelles URL S3 expirent après soixante secondes. Chaque URL signe aussi la surcharge
`response-cache-control=private, no-store`, appliquée aux objets anciens et nouveaux. Ce mécanisme de
surcharge est pris en charge dans [SeaweedFS](https://github.com/seaweedfs/seaweedfs/blob/master/weed/server/common.go).
Les nouveaux objets portent également cette politique dans leurs métadonnées.

Après blocage, fin d’accès ou retrait d’une photo, une URL déjà obtenue peut encore fonctionner jusqu’à
son expiration, ou jusqu’à suppression de l’objet si elle intervient avant. Il n’y a pas de réautorisation
en base à chaque GET S3, donc aucune promesse de révocation instantanée. Les liens émis par une version
antérieure peuvent rester valables cinq minutes pendant le déploiement. Une image déjà téléchargée ou
enregistrée ne peut pas être reprise. Cloudflare doit conserver le bypass de cache du stockage privé.
Les clients doivent relire la ressource autorisée si une URL a expiré.
