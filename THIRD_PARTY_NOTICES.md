# Third-party licensing review

Reviewed on 2026-10-23.

[Histae's proprietary source-access terms](LICENSE.md) apply only to original Histae
materials. Dependencies, SDKs, fonts, container images and other third-party materials
retain their own licenses and rights, including reuse rights those licenses grant.

## Components requiring particular attention

The WebAuthn crates `base64urlsafedata`, `webauthn-attestation-ca`, `webauthn-rs-core`
and `webauthn-rs-proto` 0.5.5 declare MPL-2.0. Review obligations for those files rather
than applying Histae's proprietary terms to them. An expression using `OR` offers
alternative license choices: for example, `r-efi` does not require choosing LGPL when
another applicable alternative is selected. Unicode data and certificate-root packages
also carry their own notices.

The image-processing dependency tree contains `libheif-js` (LGPL-3.0) and
`@img/sharp-libvips-*` packages (LGPL-3.0-or-later). Some Sharp platform packages declare
combined Apache/LGPL terms. Inspect the exact native or WebAssembly artifacts shipped in
each image and retain their license texts. Corresponding source, modification and
relinking obligations must be evaluated for the chosen licenses and distribution.
Their licenses cannot be replaced with Histae's reuse restrictions.

Container images are separate third-party software, not proprietary Histae code.
The configured Redis 7.4.2 line uses RSALv2 or SSPLv1; do not describe it as BSD-licensed or
apply the Redis 8 license choices to it. Grafana 13.2.1 uses AGPLv3. PostGIS 3.6.4 has GPL
terms. Caddy 2.11.7 uses Apache-2.0. Actual deployed images, modifications, distribution and
network-service obligations require review for the selected license and deployment.
Cloudflared, SeaweedFS, PostgreSQL, Prometheus, Alertmanager and base-image packages also
retain their own terms; this package inventory does not cover their complete contents.

References: [Redis licensing](https://redis.io/legal/licenses/),
[Grafana licensing](https://grafana.com/licensing/),
[PostGIS COPYING](https://github.com/postgis/postgis/blob/3.6.4/COPYING),
[Caddy LICENSE](https://github.com/caddyserver/caddy/blob/v2.11.7/LICENSE).

## Recorded package metadata

465 package-version entries are recorded in [THIRD_PARTY_LICENSES.json](THIRD_PARTY_LICENSES.json). These counts include build/test and optional packages and may repeat a package across nested tools. They do not describe the contents of a production artifact.

| Declared license expression | Entries |
| --- | ---: |
| `(Apache-2.0 OR MIT) AND BSD-3-Clause` | 1 |
| `(MIT OR Apache-2.0) AND Unicode-3.0` | 1 |
| `0BSD` | 1 |
| `Apache-2.0` | 23 |
| `Apache-2.0 AND ISC` | 1 |
| `Apache-2.0 AND LGPL-3.0-or-later` | 3 |
| `Apache-2.0 AND LGPL-3.0-or-later AND MIT` | 1 |
| `Apache-2.0 OR BSL-1.0` | 1 |
| `Apache-2.0 OR BSL-1.0 OR MIT` | 2 |
| `Apache-2.0 OR ISC OR MIT` | 3 |
| `Apache-2.0 OR MIT` | 27 |
| `Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT` | 3 |
| `Apache-2.0/MIT` | 1 |
| `BSD-2-Clause` | 1 |
| `BSD-2-Clause OR Apache-2.0 OR MIT` | 2 |
| `BSD-3-Clause` | 3 |
| `CC0-1.0 OR MIT-0 OR Apache-2.0` | 1 |
| `CDLA-Permissive-2.0` | 3 |
| `ISC` | 7 |
| `ISC AND (Apache-2.0 OR ISC)` | 1 |
| `ISC AND (Apache-2.0 OR ISC) AND Apache-2.0 AND MIT AND BSD-3-Clause AND (Apache-2.0 OR ISC OR MIT) AND (Apache-2.0 OR ISC OR MIT-0)` | 1 |
| `LGPL-3.0` | 1 |
| `LGPL-3.0-or-later` | 10 |
| `MIT` | 110 |
| `MIT AND BSD-3-Clause` | 1 |
| `MIT OR Apache-2.0` | 205 |
| `MIT OR Apache-2.0 OR LGPL-2.1-or-later` | 2 |
| `MIT OR Apache-2.0 OR Zlib` | 1 |
| `MIT/Apache-2.0` | 18 |
| `MPL-2.0` | 4 |
| `Unicode-3.0` | 18 |
| `Unlicense OR MIT` | 4 |
| `Unlicense/MIT` | 2 |
| `Zlib` | 1 |
| `Zlib OR Apache-2.0 OR MIT` | 1 |

## Release responsibilities and limits

This review records declared licenses, not a complete attribution or license-text bundle.
Keep upstream copyright statements, LICENSE/NOTICE files and required attribution with
distributed components. Meet applicable source-availability and other copyleft requirements;
do not strip existing generated license information or third-party notices from bundles.
An `AND` expression requires satisfying both licenses; an `OR` expression allows a choice
subject to its terms. Unusual or custom expressions need the corresponding upstream text.

Before a release, review the actual dependency versions and shipped platform artifacts,
collect their full required texts and document any modifications. Base-image packages,
external service contracts and complete container dependency trees have not
received a complete artifact-level review. No compatibility, ownership or legal-compliance
certification is asserted. This report does not revoke any earlier valid license grant.
