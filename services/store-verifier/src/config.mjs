import { readFileSync } from 'node:fs';

export function configuration(env = process.env) {
  const bool = (key) => {
    const value = env[key] || 'false';
    if (!['true', 'false'].includes(value)) throw new Error(`Invalid ${key}`);
    return value === 'true';
  };
  const required = (key, enabled) => {
    const value = env[key] || '';
    if (enabled && !value.trim()) throw new Error(`Missing ${key}`);
    return value;
  };
  const apple = bool('APP_STORE_ENABLED');
  const google = bool('GOOGLE_PLAY_ENABLED');
  const production = env.ENV === 'production';
  const token = required('STORE_VERIFIER_TOKEN', apple || google);
  if ((apple || google) && token.length < 32) throw new Error('Invalid STORE_VERIFIER_TOKEN');
  const appleEnvironment = env.APP_STORE_ENVIRONMENT || (production ? 'Production' : 'Sandbox');
  if (!['Production', 'Sandbox'].includes(appleEnvironment) || (production && appleEnvironment !== 'Production')) throw new Error('Invalid APP_STORE_ENVIRONMENT');
  const appleAppId = Number(required('APP_STORE_APP_ID', apple));
  if (apple && (!Number.isSafeInteger(appleAppId) || appleAppId <= 0)) throw new Error('Invalid APP_STORE_APP_ID');
  let googleCredentials;
  if (google) {
    googleCredentials = env.GOOGLE_PLAY_CREDENTIALS_FILE ? JSON.parse(readFileSync(env.GOOGLE_PLAY_CREDENTIALS_FILE, 'utf8')) : { type: 'service_account', client_email: required('GOOGLE_PLAY_CLIENT_EMAIL', true), private_key: required('GOOGLE_PLAY_PRIVATE_KEY', true).replaceAll('\\n', '\n') };
    if (googleCredentials.type !== 'service_account' || !googleCredentials.client_email || !googleCredentials.private_key) throw new Error('Invalid GOOGLE_PLAY_CREDENTIALS_FILE');
  }
  return {
    token,
    production,
    apple: {
      enabled: apple,
      environment: appleEnvironment,
      appId: appleAppId,
      bundleId: required('APP_STORE_BUNDLE_ID', apple),
      issuerId: required('APP_STORE_ISSUER_ID', apple),
      keyId: required('APP_STORE_KEY_ID', apple),
      privateKey: apple ? (env.APP_STORE_PRIVATE_KEY_FILE ? readFileSync(env.APP_STORE_PRIVATE_KEY_FILE, 'utf8') : required('APP_STORE_PRIVATE_KEY', true).replaceAll('\\n', '\n')) : '',
      monthly: required('APP_STORE_PREMIUM_MONTHLY_PRODUCT_ID', apple),
      annual: required('APP_STORE_PREMIUM_ANNUAL_PRODUCT_ID', apple),
    },
    google: {
      enabled: google,
      credentials: googleCredentials,
      packageName: required('GOOGLE_PLAY_PACKAGE_NAME', google),
      product: required('GOOGLE_PLAY_PREMIUM_PRODUCT_ID', google),
      monthly: required('GOOGLE_PLAY_MONTHLY_BASE_PLAN_ID', google),
      annual: required('GOOGLE_PLAY_ANNUAL_BASE_PLAN_ID', google),
      audience: required('GOOGLE_PLAY_PUBSUB_AUDIENCE', google),
      serviceAccount: required('GOOGLE_PLAY_PUBSUB_SERVICE_ACCOUNT', google),
    },
  };
}
