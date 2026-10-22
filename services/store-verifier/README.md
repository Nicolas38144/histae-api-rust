# Private store verifier

Cryptographic and HTTP adapters for the official Apple App Store Server Library
and Google Play Developer API. Entitlements, ownership, persistence and retries
remain in the Rust API. See [deployment and configuration](../../docs/store-billing.md).

The service has no database access or published host port. The backend-only
operations require `STORE_VERIFIER_TOKEN`; `/health/live` is not an assertion that
provider credentials have been exercised. Requests and responses are bounded.
Provider exceptions, tokens, private keys and request bodies are never logged.

```sh
npm ci --ignore-scripts
npm test
```

Unit/HTTP tests use synthetic data and never contact payment providers. Real
Sandbox and Play test purchases require console configuration and devices.

## Public trust anchors

The bundled public DER roots are downloaded from Apple's official certificate
repository; they contain no private keys:

- [AppleRootCA-G2.cer](https://www.apple.com/certificateauthority/AppleRootCA-G2.cer),
  SHA-256 `c2b9b042dd57830e7d117dac55ac8ae19407d38e41d88f3215bc3a890444a050`.
- [AppleRootCA-G3.cer](https://www.apple.com/certificateauthority/AppleRootCA-G3.cer),
  SHA-256 `63343abfb89a6a03ebb57e9b3f5fa7be7c4f5c756f3017b3a8c488c3653e9179`.

Source: [Apple PKI](https://www.apple.com/certificateauthority/). Review Apple's
published certificate policy and root rotations before replacing these files;
never accept a client-provided trust anchor. Apple SDK online verification stays enabled.
