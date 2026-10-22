import { test } from 'node:test';
import { generateKeyPairSync } from 'node:crypto';
import assert from 'node:assert/strict';
import { appleSnapshot, googleSnapshot, digest, providers } from './providers.mjs';
import { server } from './server.mjs';
const binding = '10000000-0000-4000-8000-000000000001';
const apple = { bundleId: 'com.histae.app', environment: 'Sandbox', monthly: 'monthly', annual: 'annual' };
const transaction = { bundleId: apple.bundleId, environment: 'Sandbox', type: 'Auto-Renewable Subscription', productId: 'monthly', originalTransactionId: '100', transactionId: '101', appAccountToken: binding, purchaseDate: 1760000000000, expiresDate: 1762600000000 };
const google = { product: 'premium', monthly: 'monthly', annual: 'annual' };
const purchase = { testPurchase: {}, subscriptionState: 'SUBSCRIPTION_STATE_ACTIVE', startTime: '2026-10-01T00:00:00Z', acknowledgementState: 'ACKNOWLEDGEMENT_STATE_PENDING', externalAccountIdentifiers: { obfuscatedExternalAccountId: digest(binding) }, lineItems: [{ productId: 'premium', expiryTime: '2026-11-01T00:00:00Z', offerDetails: { basePlanId: 'monthly' }, autoRenewingPlan: { autoRenewEnabled: true } }] };
test('Apple binds product, bundle, environment and owner; grace uses its own expiry', () => {
  const active = appleSnapshot(transaction, { autoRenewStatus: 1 }, 1, apple);
  assert.equal(active.account_binding, binding); assert.equal(active.status, 'active');
  const grace = appleSnapshot(transaction, { gracePeriodExpiresDate: 1762700000000 }, 4, apple);
  assert.equal(grace.status, 'grace_period'); assert.equal(grace.expires_at, new Date(1762700000000).toISOString());
  for (const change of [{ bundleId: 'other' }, { environment: 'Production' }, { productId: 'other' }, { inAppOwnershipType: 'FAMILY_SHARED' }, { appAccountToken: null }]) assert.throws(() => appleSnapshot({ ...transaction, ...change }, null, 1, apple));
  assert.equal(appleSnapshot({ ...transaction, revocationDate: 1 }, null, 1, apple).status, 'revoked');
});
test('Google checks environment, base plan, account and acknowledgment without granting pending access', () => {
  const active = googleSnapshot(purchase, 'secret-token', google, false);
  assert.equal(active.original_id, digest('secret-token')); assert.equal(active.acknowledgement_required, true);
  for (const production of [true]) assert.throws(() => googleSnapshot(purchase, 'token', google, production));
  assert.throws(() => googleSnapshot({ ...purchase, testPurchase: undefined }, 'token', google, false));
  assert.throws(() => googleSnapshot({ ...purchase, externalAccountIdentifiers: {} }, 'token', google, false));
  for (const state of ['ON_HOLD', 'PAUSED', 'EXPIRED', 'PENDING']) {
    const result = googleSnapshot({ ...purchase, subscriptionState: `SUBSCRIPTION_STATE_${state}` }, 'token', google, false);
    assert.equal(result.acknowledgement_required, false);
  }
  assert.throws(() => googleSnapshot({ ...purchase, subscriptionState: 'SUBSCRIPTION_STATE_PENDING', startTime: undefined }, 'token', google, false), { code: 'store_purchase_pending', status: 503 });
});
test('disabled providers reject untrusted notifications and receipts', async () => {
  const gateway = providers({ production: false, apple: { enabled: false }, google: { enabled: false } });
  await assert.rejects(gateway.verify('app_store', '123'));
  await assert.rejects(gateway.notification('app_store', { signedPayload: 'forged' }));
  await assert.rejects(gateway.notification('google_play', {}, 'Bearer forged'));
});
test('private bridge authenticates, caps bodies, redacts errors and dispatches only known operations', async (t) => {
  const app = server({ token: 'test-token' }, { verify: async (provider, reference) => { if (reference === 'bad') throw new Error('secret-token'); return { provider }; } });
  await new Promise((resolve) => app.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise((resolve) => app.close(resolve)));
  const url = `http://127.0.0.1:${app.address().port}`;
  const request = (body, auth = 'Bearer test-token', path = '/verify') => fetch(url + path, { method: 'POST', headers: { authorization: auth }, body: JSON.stringify(body) });
  assert.equal((await request({}, 'Bearer wrong')).status, 401);
  assert.equal((await request({ provider: 'app_store', reference: '100' })).status, 200);
  const error = await request({ reference: 'bad' }); assert.equal(error.status, 503); assert.deepEqual(await error.json(), { error: 'store_verification_unavailable' });
  assert.equal((await request({ reference: 'x'.repeat(100000) })).status, 413);
  assert.equal((await request({}, undefined, '/unknown')).status, 404);
});

test('enabled Apple verifier rejects forged JWS using the official trust anchors without granting access', async () => {
  const key = generateKeyPairSync('ec', { namedCurve: 'prime256v1' }).privateKey.export({ type: 'pkcs8', format: 'pem' });
  const gateway = providers({ production: false, google: { enabled: false }, apple: { ...apple, enabled: true, appId: 1, privateKey: key, keyId: 'SYNTHETIC1', issuerId: binding } });
  await assert.rejects(gateway.notification('app_store', { signedPayload: 'eyJhbGciOiJub25lIn0.e30.' }), { code: 'invalid_store_purchase', status: 422 });
});
