import { createServer } from 'node:http';
import { createServer as createTlsServer } from 'node:https';
import { readFileSync } from 'node:fs';
import { timingSafeEqual } from 'node:crypto';
import { configuration } from './config.mjs';
import { providers, StoreError } from './providers.mjs';

export function server(config, gateway, tls) {
  const capacity = new Map([
    ['/verify', { operation: 'verify', active: 0, max: 4 }],
    ['/acknowledge', { operation: 'acknowledge', active: 0, max: 1 }],
  ]);
  for (const provider of ['app_store', 'google_play']) {
    capacity.set(`/notification/${provider}`, { operation: 'notification', provider, active: 0, max: 2 });
    capacity.set(`/verify-notification/${provider}`, { operation: 'verify', provider, active: 0, max: 1 });
  }
  if (config.production && !tls) throw new Error('Store verifier TLS is required');
  const handle = async (request, response) => {
    response.setHeader('Content-Type', 'application/json');
    response.setHeader('Cache-Control', 'no-store');
    const send = (status, body) => { response.writeHead(status); response.end(JSON.stringify(body)); };
    if (request.url === '/health/live' && request.method === 'GET') return send(200, { status: 'ok' });
    const expected = Buffer.from(`Bearer ${config.token}`);
    const supplied = Buffer.from(request.headers.authorization || '');
    if (!config.token || supplied.length !== expected.length || !timingSafeEqual(supplied, expected)) return send(401, { error: 'unauthorized' });
    const budget = capacity.get(request.url);
    if (!budget || request.method !== 'POST') return send(404, { error: 'route_not_found' });
    if (budget.active >= budget.max) return send(503, { error: 'store_unavailable' });
    budget.active += 1;
    try {
      let size = 0;
      const chunks = [];
      for await (const chunk of request) {
        size += chunk.length;
        if (size > 98304) throw new StoreError('request_too_large', 413);
        chunks.push(chunk);
      }
      const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      if (budget.operation === 'verify') return send(200, await gateway.verify(budget.provider ?? body.provider, body.reference));
      if (budget.operation === 'notification') return send(200, await gateway.notification(budget.provider, body.body, body.authorization));
      if (budget.operation === 'acknowledge' && body.provider === 'google_play') {
        await gateway.acknowledge(body.reference);
        return send(200, { acknowledged: true });
      }
      return send(404, { error: 'route_not_found' });
    } catch (error) {
      // Provider exceptions may contain bearer tokens, URLs or credentials.
      // Never serialize or log them, including request paths and bodies.
      return send(error instanceof StoreError ? error.status : 503, { error: error instanceof StoreError ? error.code : 'store_verification_unavailable' });
    } finally { budget.active -= 1; }
  };
  return tls ? createTlsServer({ ...tls, minVersion: 'TLSv1.2' }, handle) : createServer(handle);
}

if (process.argv[1] && import.meta.url === new URL(`file://${process.argv[1]}`).href) {
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
