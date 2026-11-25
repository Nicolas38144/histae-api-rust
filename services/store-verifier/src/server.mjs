import { createServer } from 'node:http';
import { createServer as createTlsServer } from 'node:https';
import { readFileSync } from 'node:fs';
import { timingSafeEqual } from 'node:crypto';
import { pathToFileURL } from 'node:url';
import { configuration } from './config.mjs';
import { providers, StoreError } from './providers.mjs';

const MAX_REQUEST_BYTES = 96 * 1024;

function operations(gateway) {
  const routes = new Map([
    ['/verify', { active: 0, max: 4, run: ({ provider, reference }) => gateway.verify(provider, reference) }],
    ['/acknowledge', { active: 0, max: 1, async run({ provider, reference }) {
      if (provider !== 'google_play') throw new StoreError('route_not_found', 404);
      await gateway.acknowledge(reference);
      return { acknowledged: true };
    } }],
  ]);
  for (const provider of ['app_store', 'google_play']) {
    routes.set(`/notification/${provider}`, {
      active: 0, max: 2,
      run: ({ body, authorization }) => gateway.notification(provider, body, authorization),
    });
    routes.set(`/verify-notification/${provider}`, {
      active: 0, max: 1,
      run: ({ reference }) => gateway.verify(provider, reference),
    });
  }
  return routes;
}

async function readJson(request) {
  let size = 0;
  const chunks = [];
  for await (const chunk of request) {
    size += chunk.length;
    if (size > MAX_REQUEST_BYTES) throw new StoreError('request_too_large', 413);
    chunks.push(chunk);
  }
  return JSON.parse(Buffer.concat(chunks).toString('utf8'));
}

export function server(config, gateway, tls) {
  if (config.production && !tls) throw new Error('Store verifier TLS is required');
  const routes = operations(gateway);
  const expected = config.token ? Buffer.from(`Bearer ${config.token}`) : null;
  const handle = async (request, response) => {
    response.setHeader('Content-Type', 'application/json');
    response.setHeader('Cache-Control', 'no-store');
    const send = (status, body) => { response.writeHead(status); response.end(JSON.stringify(body)); };
    if (request.url === '/health/live' && request.method === 'GET') return send(200, { status: 'ok' });
    const supplied = Buffer.from(request.headers.authorization || '');
    if (!expected || supplied.length !== expected.length || !timingSafeEqual(supplied, expected)) {
      return send(401, { error: 'unauthorized' });
    }
    const operation = routes.get(request.url);
    if (!operation || request.method !== 'POST') return send(404, { error: 'route_not_found' });
    if (operation.active >= operation.max) return send(503, { error: 'store_unavailable' });
    operation.active += 1;
    try {
      const body = await readJson(request);
      return send(200, await operation.run(body));
    } catch (error) {
      // Provider exceptions may contain bearer tokens, URLs or credentials.
      // Never serialize or log them, including request paths and bodies.
      return send(error instanceof StoreError ? error.status : 503, { error: error instanceof StoreError ? error.code : 'store_verification_unavailable' });
    } finally { operation.active -= 1; }
  };
  return tls ? createTlsServer({ ...tls, minVersion: 'TLSv1.2' }, handle) : createServer(handle);
}

if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const config = configuration();
    const certificate = process.env.STORE_VERIFIER_TLS_CERT_FILE;
    const key = process.env.STORE_VERIFIER_TLS_KEY_FILE;
    if (Boolean(certificate) !== Boolean(key)) throw new Error('Incomplete store verifier TLS configuration');
    const tls = certificate ? { cert: readFileSync(certificate), key: readFileSync(key) } : undefined;
    const app = server(config, providers(config), tls);
    app.requestTimeout = 20000;
    app.headersTimeout = 10000;
    app.listen(Number(process.env.PORT || 8080), '0.0.0.0');
    for (const signal of ['SIGTERM', 'SIGINT']) process.on(signal, () => app.close(() => process.exit(0)));
  } catch { process.stderr.write('store_verifier_configuration_invalid\n'); process.exitCode = 1; }
}
