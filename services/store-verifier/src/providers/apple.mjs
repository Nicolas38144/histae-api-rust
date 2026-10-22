import { readFileSync } from 'node:fs';
import { AppStoreServerAPIClient, SignedDataVerifier, Environment, VerificationException, VerificationStatus } from '@apple/app-store-server-library';
import { StoreError, invalid, text, instant, eventUuid, uuid } from './validation.mjs';

export function appleSnapshot(transaction, renewal, status, config) {
  if (transaction.bundleId !== config.bundleId || transaction.environment !== config.environment
    || transaction.type !== 'Auto-Renewable Subscription' || transaction.inAppOwnershipType === 'FAMILY_SHARED') invalid();
  const period = transaction.productId === config.monthly ? 'monthly' : transaction.productId === config.annual ? 'annual' : invalid();
  let state = ({ 1: 'active', 2: 'expired', 3: 'past_due', 4: 'grace_period', 5: 'revoked' })[status];
  if (!state) invalid();
  if (transaction.revocationDate) state = 'revoked';
  const expires = state === 'grace_period' ? renewal?.gracePeriodExpiresDate : transaction.expiresDate;
  return {
    provider: 'app_store',
    original_id: text(transaction.originalTransactionId, 128),
    transaction_id: text(transaction.transactionId, 128),
    product_id: transaction.productId,
    base_plan_id: null,
    account_binding: uuid(transaction.appAccountToken),
    environment: config.environment === 'Production' ? 'production' : 'sandbox',
    billing_period: period,
    status: state,
    starts_at: instant(transaction.purchaseDate),
    expires_at: instant(expires),
    auto_renew: renewal?.autoRenewStatus === 1,
    acknowledgement_required: false,
    linked_reference_hash: null,
  };
}

// Keep provider requests bounded without replacing Apple's signing, JWS or
// certificate verification implementation.
class BoundedAppleClient extends AppStoreServerAPIClient {
  constructor(...args) { super(...args); this.histaeBase = args[4] === Environment.PRODUCTION ? 'https://api.storekit.apple.com' : 'https://api.storekit-sandbox.apple.com'; }
  async makeFetchRequest(path, query, method, body, headers) {
    const response = await fetch(`${this.histaeBase}${path}?${query}`, { method, body, headers, redirect: 'error', signal: AbortSignal.timeout(12000) });
    const chunks = []; let size = 0;
    for await (const chunk of response.body) {
      size += chunk.length;
      if (size > 262144) throw new StoreError('store_verification_unavailable', 503);
      chunks.push(chunk);
    }
    return new Response(Buffer.concat(chunks), { status: response.status, headers: response.headers });
  }
}

export function appleAdapter(config) {
    const environment = config.apple.environment === 'Production' ? Environment.PRODUCTION : Environment.SANDBOX;
    const roots = ['AppleRootCA-G2.cer', 'AppleRootCA-G3.cer'].map((name) => readFileSync(new URL(`../../certificates/${name}`, import.meta.url)));
    const appleVerifier = new SignedDataVerifier(roots, true, environment, config.apple.bundleId, config.apple.appId);
    const appleClient = new BoundedAppleClient(config.apple.privateKey, config.apple.keyId, config.apple.issuerId, config.apple.bundleId, environment);
  const appleData = async (method, payload) => {
    try { return await appleVerifier[method](payload); }
    catch (error) {
      if (error instanceof VerificationException && error.status !== VerificationStatus.RETRYABLE_VERIFICATION_FAILURE) throw new StoreError('invalid_store_purchase', 422);
      throw error;
    }
  };
  const verify = async (reference) => {
    if (!appleClient) throw new StoreError('store_unavailable', 503);
    if (!/^\d{1,128}$/.test(reference || '')) invalid();
    const first = await appleClient.getTransactionInfo(reference);
    const transaction = await appleData('verifyAndDecodeTransaction', first.signedTransactionInfo);
    const all = await appleClient.getAllSubscriptionStatuses(transaction.originalTransactionId);
    const matches = (all.data || []).flatMap((group) => group.lastTransactions || []).filter((row) => row.originalTransactionId === transaction.originalTransactionId);
    if (matches.length !== 1) invalid();
    const row = matches[0];
    const current = await appleData('verifyAndDecodeTransaction', row.signedTransactionInfo);
    const renewal = row.signedRenewalInfo ? await appleData('verifyAndDecodeRenewalInfo', row.signedRenewalInfo) : null;
    if (current.originalTransactionId !== transaction.originalTransactionId) invalid();
    return appleSnapshot(current, renewal, row.status, config.apple);
  };
  return {
    verify,
    async notification(body) {
        const payload = await appleData('verifyAndDecodeNotification', text(body?.signedPayload, 65536));
        if (payload.notificationType === 'TEST') return { event_id: eventUuid(payload.notificationUUID), reference: null };
        if (!payload.data?.signedTransactionInfo) invalid();
        const transaction = await appleData('verifyAndDecodeTransaction', payload.data.signedTransactionInfo);
        return { event_id: eventUuid(payload.notificationUUID), reference: text(transaction.originalTransactionId, 128) };
    },
  };
}
