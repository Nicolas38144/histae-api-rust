import { test } from 'node:test';
import { generateKeyPairSync } from 'node:crypto';
import assert from 'node:assert/strict';
import { appleSnapshot, googleSnapshot, digest, providers, StoreError } from './providers.mjs';
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

test('provider degradation preserves independent webhook and authenticated event capacity', { timeout: 12000 }, async (t) => {
  const release = Promise.withResolvers();
  const webhooksAdmitted = Promise.withResolvers();
  const purchasesAdmitted = Promise.withResolvers();
  const notificationAdmitted = Promise.withResolvers();
  let googleWebhooks = 0;
  let purchases = 0;
  let googleRecovered = false;
  const app = server({ token: 'test-token' }, {
    async notification(provider) {
      if (provider === 'google_play' && !googleRecovered) {
        if (++googleWebhooks === 2) webhooksAdmitted.resolve();
        await release.promise;
        throw new StoreError('store_unavailable', 503);
      }
      return { event_id: provider };
    },
    async verify(provider, reference) {
      if (reference === 'purchase') {
        if (++purchases === 4) purchasesAdmitted.resolve();
        await release.promise;
      } else if (provider === 'google_play' && !googleRecovered) {
        notificationAdmitted.resolve();
        await release.promise;
        throw new StoreError('store_unavailable', 503);
      }
      return { provider, status: 'active' };
    },
    async acknowledge() {},
  });
  await new Promise((resolve) => app.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise((resolve) => app.close(resolve)));
  const url = `http://127.0.0.1:${app.address().port}`;
  const request = async (path, reference = 'notification') => {
    const start = performance.now();
    const response = await fetch(url + path, {
      method: 'POST',
      headers: { authorization: 'Bearer test-token' },
      body: JSON.stringify({ provider: 'google_play', reference }),
      signal: AbortSignal.timeout(5000),
    });
    const body = await response.json();
    return { status: response.status, body, elapsed: performance.now() - start };
  };
  const webhooks = Promise.all(Array.from({ length: 2 }, () => request('/notification/google_play')));
  const verifications = Promise.all(Array.from({ length: 4 }, () => request('/verify', 'purchase')));
  let notification;
  let results;
  try {
    await Promise.race([Promise.all([webhooksAdmitted.promise, purchasesAdmitted.promise]), webhooks, verifications]);
    assert.equal((await request('/notification/google_play')).status, 503);
    assert.equal((await request('/verify', 'purchase')).status, 503);

    notification = request('/verify-notification/google_play');
    await Promise.race([notificationAdmitted.promise, notification]);
    assert.equal((await request('/verify-notification/google_play')).status, 503);

    const [appleWebhook, appleNotification, ack] = await Promise.all([
      request('/notification/app_store'),
      request('/verify-notification/app_store'),
      request('/acknowledge'),
    ]);
    assert.equal(appleWebhook.status, 200);
    assert.equal(appleWebhook.body.event_id, 'app_store');
    assert.equal(appleNotification.status, 200);
    assert.equal(appleNotification.body.provider, 'app_store');
    assert.equal(ack.status, 200);
    t.diagnostic(`Apple during simulated Google stalls: webhook ${appleWebhook.elapsed.toFixed(1)} ms, authenticated event ${appleNotification.elapsed.toFixed(1)} ms`);
  } finally {
    release.resolve();
    results = await Promise.all([webhooks, verifications, notification]);
  }
  assert.deepEqual(results[0].map(({ status }) => status), [503, 503]);
  assert.deepEqual(results[1].map(({ status }) => status), [200, 200, 200, 200]);
  assert.equal(results[2].status, 503);
  googleRecovered = true;
  const [webhookRetry, notificationRetry] = await Promise.all([
    request('/notification/google_play'), request('/verify-notification/google_play'),
  ]);
  assert.equal(webhookRetry.status, 200);
  assert.equal(notificationRetry.status, 200);
  t.diagnostic(`Google after simulated recovery: webhook retry ${webhookRetry.elapsed.toFixed(1)} ms, authenticated event retry ${notificationRetry.elapsed.toFixed(1)} ms`);
});

test('enabled Apple verifier rejects forged JWS using the official trust anchors without granting access', async () => {
  const key = generateKeyPairSync('ec', { namedCurve: 'prime256v1' }).privateKey.export({ type: 'pkcs8', format: 'pem' });
  const gateway = providers({ production: false, google: { enabled: false }, apple: { ...apple, enabled: true, appId: 1, privateKey: key, keyId: 'SYNTHETIC1', issuerId: binding } });
  await assert.rejects(gateway.notification('app_store', { signedPayload: 'eyJhbGciOiJub25lIn0.e30.' }), { code: 'invalid_store_purchase', status: 422 });
});
