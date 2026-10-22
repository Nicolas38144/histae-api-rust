import { GoogleAuth, OAuth2Client } from 'google-auth-library';
import { StoreError, invalid, text, instant, digest } from './validation.mjs';

export function googleSnapshot(purchase, token, config, production) {
  if (production === Boolean(purchase.testPurchase)) throw new StoreError('store_environment_mismatch');
  const items = purchase.lineItems;
  if (!Array.isArray(items) || items.length !== 1) invalid();
  const item = items[0];
  if (item.productId !== config.product) invalid();
  const base = item.offerDetails?.basePlanId;
  const period = base === config.monthly ? 'monthly' : base === config.annual ? 'annual' : invalid();
  const state = ({
    SUBSCRIPTION_STATE_ACTIVE: 'active', SUBSCRIPTION_STATE_IN_GRACE_PERIOD: 'grace_period',
    SUBSCRIPTION_STATE_ON_HOLD: 'on_hold', SUBSCRIPTION_STATE_PAUSED: 'paused',
    SUBSCRIPTION_STATE_CANCELED: 'canceled', SUBSCRIPTION_STATE_EXPIRED: 'expired',
    SUBSCRIPTION_STATE_PENDING: 'pending', SUBSCRIPTION_STATE_PENDING_PURCHASE_CANCELED: 'expired',
  })[purchase.subscriptionState];
  if (!state || !item.autoRenewingPlan || !/^[0-9a-f]{64}$/.test(purchase.externalAccountIdentifiers?.obfuscatedExternalAccountId || '')) invalid();
  // Pending Play purchases may not yet have a start/expiry date. Do not
  // invent a paid period or acknowledge them; the client/provider retries.
  if ((!purchase.startTime || !item.expiryTime) && ['pending', 'expired'].includes(state)) throw new StoreError('store_purchase_pending', 503);
  return {
    provider: 'google_play', original_id: digest(token),
    transaction_id: text(item.latestSuccessfulOrderId || purchase.latestOrderId || digest(token), 256),
    product_id: item.productId, base_plan_id: base,
    account_binding: purchase.externalAccountIdentifiers.obfuscatedExternalAccountId,
    environment: purchase.testPurchase ? 'sandbox' : 'production', billing_period: period,
    status: state, starts_at: instant(purchase.startTime), expires_at: instant(item.expiryTime),
    auto_renew: item.autoRenewingPlan.autoRenewEnabled === true,
    acknowledgement_required: purchase.acknowledgementState === 'ACKNOWLEDGEMENT_STATE_PENDING' && ['active', 'grace_period', 'canceled'].includes(state),
    linked_reference_hash: purchase.linkedPurchaseToken ? digest(purchase.linkedPurchaseToken) : null,
  };
}

export function googlePlayAdapter(config) {
  const auth = new GoogleAuth({ credentials: config.google.credentials, scopes: ['https://www.googleapis.com/auth/androidpublisher'] });
  const googleOidc = new OAuth2Client();
  let googleClient;
  const getClient = () => googleClient ??= auth.getClient();
  const verify = async (reference) => {
    text(reference);
    const client = await getClient();
    const { data } = await client.request({
      url: `https://androidpublisher.googleapis.com/androidpublisher/v3/applications/${encodeURIComponent(config.google.packageName)}/purchases/subscriptionsv2/tokens/${encodeURIComponent(reference)}`,
      timeout: 12000, retry: false, responseType: 'json', maxContentLength: 262144,
    });
    return googleSnapshot(data, reference, config.google, config.production);
  };
  return {
    verify,
    async notification(body, authorization) {
      if (typeof authorization !== 'string' || !authorization.startsWith('Bearer ')) invalid();
      const ticket = await googleOidc.verifyIdToken({ idToken: authorization.slice(7), audience: config.google.audience });
      const identity = ticket.getPayload();
      if (identity?.email !== config.google.serviceAccount || identity.email_verified !== true
        || !['https://accounts.google.com', 'accounts.google.com'].includes(identity.iss)) invalid();
      const message = body?.message;
      const eventId = text(message?.messageId, 128);
      if (!/^[A-Za-z0-9+/]*={0,2}$/.test(message?.data || '')) invalid();
      const data = JSON.parse(Buffer.from(message.data, 'base64').toString('utf8'));
      if (data.packageName !== config.google.packageName) invalid();
      if (data.testNotification) return { event_id: eventId, reference: null };
      const token = data.subscriptionNotification?.purchaseToken || data.voidedPurchaseNotification?.purchaseToken;
      return { event_id: eventId, reference: text(token) };
    },
    async acknowledge(reference) {
      const snapshot = await verify(reference);
      if (!snapshot.acknowledgement_required) return;
      const client = await getClient();
      await client.request({
        method: 'POST',
        url: `https://androidpublisher.googleapis.com/androidpublisher/v3/applications/${encodeURIComponent(config.google.packageName)}/purchases/subscriptions/${encodeURIComponent(config.google.product)}/tokens/${encodeURIComponent(reference)}:acknowledge`,
        data: {}, timeout: 12000, retry: false,
      });
    },
  };
}
