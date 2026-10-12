import http from 'k6/http';
import { check, fail } from 'k6';
import { SharedArray } from 'k6/data';
import exec from 'k6/execution';
import { Counter, Rate } from 'k6/metrics';

// Read-only checks against explicitly prepared synthetic development fixtures.
const baseUrl = (__ENV.BASE_URL || '').replace(/\/$/, '');
if (!/^http:\/\/(127\.0\.0\.1|localhost)(:[0-9]+)?$/.test(baseUrl)) {
  throw new Error('Feed variants require a loopback development API');
}
function integer(name, fallback, max) {
  const value = __ENV[name] || String(fallback);
  if (!/^[1-9][0-9]*$/.test(value) || Number(value) > max) {
    throw new Error(`Invalid ${name}`);
  }
  return Number(value);
}
const rate = integer('VARIANT_RPS', 50, 100);
const seconds = integer('VARIANT_SECONDS', 60, 120);
const vus = integer('VARIANT_VUS', 100, 200);
const p95 = integer('P95_MS', 750, 60000);
const p99 = integer('P99_MS', 1000, 60000);
const fixturePath = __ENV.ACCOUNTS_FILE;
if (!fixturePath) {
  throw new Error('ACCOUNTS_FILE is required');
}
const allowedIds = new SharedArray('prepared variant allowlist', () => JSON.parse(open(fixturePath)).allowed_ids);
const allowed = new Set();
for (let i = 0; i < allowedIds.length; i += 1) allowed.add(allowedIds[i]);
const accounts = new SharedArray('prepared development feed variant', () => {
  const fixture = JSON.parse(open(fixturePath));
  if (fixture.environment !== 'development' || fixture.schema_version !== 1 ||
      fixture.api_origin !== baseUrl || !/^[a-z0-9-]{1,60}$/.test(fixture.variant) ||
      !Array.isArray(fixture.accounts) || fixture.accounts.length < 100 ||
      !Number.isFinite(fixture.expires_at) || typeof fixture.expect_empty !== 'boolean' ||
      !Array.isArray(fixture.allowed_ids)) {
    throw new Error('Invalid prepared feed variant');
  }
  return fixture.accounts.map((a) => {
    if (!a.user_id || !a.access_token || a.expires_at !== fixture.expires_at ||
        !Array.isArray(a.previous_ids) || typeof a.cursor !== 'string') {
      throw new Error('Invalid variant account');
    }
    return { ...a, variant: fixture.variant, expect_empty: fixture.expect_empty,
      max_distance_km: fixture.max_distance_km };
  });
});
const validPayload = new Rate('histae_variant_payload_valid');
const emptyPages = new Counter('histae_variant_empty_pages');
const errors = new Counter('histae_error_codes');

export const options = {
  scenarios: { reads: {
    executor: 'constant-arrival-rate', rate, timeUnit: '1s', duration: `${seconds}s`,
    preAllocatedVUs: vus, gracefulStop: '10s',
  } },
  systemTags: ['status', 'method', 'name', 'scenario', 'expected_response'],
  summaryTrendStats: ['avg', 'min', 'med', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    http_req_failed: ['rate==0'],
    checks: ['rate==1'],
    histae_variant_payload_valid: ['rate==1'],
    'http_req_duration{expected_response:true}': [`p(95)<${p95}`, `p(99)<${p99}`],
    dropped_iterations: ['count==0'],
  },
};

function feed(account) {
  const suffix = account.cursor ? `&cursor=${encodeURIComponent(account.cursor)}` : '';
  const response = http.get(`${baseUrl}/api/feed?limit=20${suffix}`, {
    headers: { Authorization: `Bearer ${account.access_token}`, Accept: 'application/json' },
    tags: { name: 'GET /api/feed' }, timeout: '10s', redirects: 0,
  });
  let body;
  try { body = response.json(); } catch (_) { body = null; }
  const profiles = body && body.profiles;
  const valid = response.status === 200 && Array.isArray(profiles) && profiles.length <= 20 &&
    (account.expect_empty ? profiles.length === 0 && body.next_cursor === null : profiles.length > 0) &&
    new Set(profiles.map((p) => p.user_id)).size === profiles.length &&
    profiles.every((p) => p.user_id !== account.user_id && !account.previous_ids.includes(p.user_id) &&
      (allowed.size === 0 || allowed.has(p.user_id)) &&
      typeof p.distance_km === 'number' && p.distance_km >= 0 &&
      p.distance_km <= account.max_distance_km + 0.05);
  validPayload.add(valid);
  check(response, { 'variant status, eligibility and page contract': () => valid });
  if (Array.isArray(profiles) && profiles.length === 0) emptyPages.add(1);
  if (!valid) {
    const code = body && body.error && body.error.code;
    errors.add(1, { status: String(response.status),
      code: typeof code === 'string' && /^[a-z0-9_]{1,80}$/.test(code) ? code : 'unexpected_feed_payload' });
  }
  return valid;
}

export function setup() {
  if (accounts[0].expires_at < Date.now() / 1000 + seconds + 120) {
    fail('Variant sessions expire too soon');
  }
  for (let i = 0; i < 4; i += 1) {
    if (!feed(accounts[i])) fail('Prepared feed variant preflight failed');
  }
}

export default function () {
  feed(accounts[exec.scenario.iterationInTest % accounts.length]);
}
