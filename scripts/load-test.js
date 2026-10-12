import http from 'k6/http';
import { check, fail } from 'k6';
import { SharedArray } from 'k6/data';
import exec from 'k6/execution';
import { Rate } from 'k6/metrics';

// One iteration sends exactly one business request. No automatic retries.
const baseUrl = (__ENV.BASE_URL || '').replace(/\/$/, '');
if (!/^https?:\/\/[^/?#]+$/.test(baseUrl)) {
  throw new Error('BASE_URL must be an HTTP(S) origin without a path or query');
}
const profile = __ENV.PROFILE || 'smoke';
const workload = __ENV.WORKLOAD || 'mixed';
if (!['smoke', 'capacity', 'soak'].includes(profile) || !['mixed', 'feed'].includes(workload)) {
  throw new Error('Use PROFILE=smoke|capacity|soak and WORKLOAD=mixed|feed');
}
function positiveInteger(name, fallback, maximum) {
  const raw = __ENV[name] || String(fallback);
  const number = Number(raw);
  if (!/^[1-9][0-9]*$/.test(raw) || !Number.isSafeInteger(number) || number > maximum) {
    throw new Error(`${name} must be an integer between 1 and ${maximum}`);
  }
  return number;
}
const targetRate = positiveInteger('TARGET_RPS', 100, 1000);
const preallocated = positiveInteger('PREALLOCATED_VUS', 100, 2000);
const soakMinutes = positiveInteger('SOAK_MINUTES', 30, 45);
const p95 = positiveInteger('P95_MS', 750, 60000);
const fixturePath = __ENV.ACCOUNTS_FILE || '../.load-test/accounts.json';
const accounts = new SharedArray('synthetic mobile sessions', () => {
  const fixture = JSON.parse(open(fixturePath));
  if (fixture.schema_version !== 1 || fixture.environment !== 'development' ||
      !Array.isArray(fixture.accounts) || fixture.accounts.length < 1000 ||
      !Number.isFinite(fixture.expires_at)) {
    throw new Error('Invalid development fixture; run prepare-dev-load-test.py');
  }
  if (fixture.accounts.some((account) => !account.access_token || !account.user_id || !account.match_id ||
      account.expires_at !== fixture.expires_at)) {
    throw new Error('Every fixture account needs a token, owner and confirmed conversation');
  }
  return fixture.accounts;
});
// Read one shared account instead of parsing the full token file again per VU.
const expiresAt = accounts[0].expires_at;

const rateLimited = new Rate('histae_http_429');
const serverErrors = new Rate('histae_http_5xx');
const challenged = new Rate('histae_cloudflare_challenge');
const badPayload = new Rate('histae_invalid_payload');
const stages = profile === 'capacity' ? [
  { target: 10, duration: '30s' }, { target: 10, duration: '2m' },
  { target: 25, duration: '30s' }, { target: 25, duration: '2m' },
  { target: 50, duration: '30s' }, { target: 50, duration: '2m' },
  { target: targetRate, duration: '30s' }, { target: targetRate, duration: '5m' },
  { target: 0, duration: '30s' },
] : [
  { target: targetRate, duration: '1m' },
  { target: targetRate, duration: `${soakMinutes}m` },
  { target: 0, duration: '30s' },
];
if (profile === 'capacity' && targetRate < 50) {
  throw new Error('Capacity TARGET_RPS must be at least 50; use smoke for the first check');
}
const runSeconds = profile === 'smoke' ? 20 : profile === 'capacity' ? 810 : soakMinutes * 60 + 90;

export const options = {
  scenarios: {
    reads: profile === 'smoke' ? {
      executor: 'constant-arrival-rate', rate: 1, timeUnit: '1s', duration: '20s',
      preAllocatedVUs: 2, gracefulStop: '10s',
    } : {
      executor: 'ramping-arrival-rate', startRate: 1, timeUnit: '1s', stages,
      preAllocatedVUs: preallocated, gracefulStop: '10s',
    },
  },
  systemTags: ['status', 'method', 'name', 'scenario', 'expected_response'],
  summaryTrendStats: ['avg', 'min', 'med', 'p(95)', 'p(99)', 'max'],
  thresholds: {
    http_req_failed: [{ threshold: 'rate<0.01', abortOnFail: true, delayAbortEval: '30s' }],
    checks: [{ threshold: 'rate>0.99', abortOnFail: true, delayAbortEval: '30s' }],
    'http_req_duration{expected_response:true}': [
      { threshold: `p(95)<${p95}`, abortOnFail: true, delayAbortEval: '60s' },
    ],
    dropped_iterations: [{ threshold: 'count==0', abortOnFail: true, delayAbortEval: '30s' }],
    histae_http_429: ['rate==0'],
    histae_cloudflare_challenge: ['rate==0'],
  },
};

function request(account, path, name, field) {
  const response = http.get(`${baseUrl}${path}`, {
    headers: { Authorization: `Bearer ${account.access_token}`, Accept: 'application/json' },
    timeout: '10s', redirects: 0, tags: { name },
  });
  const challenge = response.headers['Cf-Mitigated'] === 'challenge';
  let payload;
  try { payload = response.json(); } catch (_) { payload = null; }
  const valid = response.status === 200 && !challenge && payload !== null &&
    (field === 'user_id' ? payload.user_id === account.user_id && payload.onboarding_complete === true
      : Array.isArray(payload[field]) && payload[field].length > 0);
  rateLimited.add(response.status === 429);
  serverErrors.add(response.status >= 500);
  challenged.add(challenge);
  badPayload.add(!valid);
  check(response, { 'expected status and nonempty API payload': () => valid }, { route: name });
  const code = payload && payload.error && payload.error.code;
  return { valid, status: response.status, challenge,
    code: typeof code === 'string' && /^[a-z0-9_]{1,80}$/.test(code) ? code : 'unknown' };
}

export function setup() {
  if (expiresAt - Date.now() / 1000 < runSeconds + 120) {
    fail('Sessions expire too soon for this profile. Cleanup, prepare and copy a fresh fixture.');
  }
  const account = accounts[0];
  for (const [path, name, field] of [
    ['/api/auth/me', 'GET /api/auth/me', 'user_id'],
    ['/api/feed?limit=20', 'GET /api/feed', 'profiles'],
    ['/api/matches/me?limit=20', 'GET /api/matches/me', 'matches'],
    [`/api/matches/${account.match_id}/messages?limit=20`, 'GET /api/matches/:id/messages', 'messages'],
  ]) {
    const result = request(account, path, name, field);
    if (!result.valid) {
      fail(`Preflight failed for ${name}: HTTP ${result.status}, code=${result.code}, ` +
        `Cloudflare challenge=${result.challenge}. Check sessions, presence, consent, quotas and Cloudflare events.`);
    }
  }
}

export default function () {
  const iteration = exec.scenario.iterationInTest;
  // Spread traffic across all accounts, independently of VU scheduling.
  const account = accounts[(workload === 'feed' ? iteration : Math.floor(iteration / 5)) % accounts.length];
  const choice = workload === 'feed' ? 0 : iteration % 5;
  if (choice < 3) {
    request(account, '/api/feed?limit=20', 'GET /api/feed', 'profiles');
  } else if (choice === 3) {
    request(account, '/api/matches/me?limit=20', 'GET /api/matches/me', 'matches');
  } else {
    request(account, `/api/matches/${account.match_id}/messages?limit=20`,
      'GET /api/matches/:id/messages', 'messages');
  }
}
