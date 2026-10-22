import { createServer } from 'node:http';
import { timingSafeEqual } from 'node:crypto';
import { configuration } from './config.mjs';
import { providers, StoreError } from './providers.mjs';

export function server(config, gateway) {
  let active = 0;
  return createServer(async (request, response) => {
    response.setHeader('Content-Type', 'application/json');
    response.setHeader('Cache-Control', 'no-store');
    const send = (status, body) => { response.writeHead(status); response.end(JSON.stringify(body)); };
    if (request.url === '/health/live' && request.method === 'GET') return send(200, { status: 'ok' });
    const expected = Buffer.from(`Bearer ${config.token}`);
    const supplied = Buffer.from(request.headers.authorization || '');
    if (!config.token || supplied.length !== expected.length || !timingSafeEqual(supplied, expected)) return send(401, { error: 'unauthorized' });
    if (request.method !== 'POST' || active >= 8) return send(503, { error: 'store_unavailable' });
    active += 1;
    try {
      let size = 0;
      const chunks = [];
      for await (const chunk of request) {
        size += chunk.length;
        if (size > 98304) throw new StoreError('request_too_large', 413);
        chunks.push(chunk);
      }
      const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
      if (request.url === '/verify') return send(200, await gateway.verify(body.provider, body.reference));
      if (request.url === '/notification') return send(200, await gateway.notification(body.provider, body.body, body.authorization));
      if (request.url === '/acknowledge' && body.provider === 'google_play') {
        await gateway.acknowledge(body.reference);
        return send(200, { acknowledged: true });
      }
      return send(404, { error: 'route_not_found' });
    } catch (error) {
      // Provider exceptions may contain bearer tokens, URLs or credentials.
      // Never serialize or log them, including request paths and bodies.
      return send(error instanceof StoreError ? error.status : 503, { error: error instanceof StoreError ? error.code : 'store_verification_unavailable' });
    } finally { active -= 1; }
  });
}

if (process.argv[1] && import.meta.url === new URL(`file://${process.argv[1]}`).href) {
  try {
    const config = configuration();
    const app = server(config, providers(config));
    app.requestTimeout = 20000;
    app.headersTimeout = 10000;
    app.listen(Number(process.env.PORT || 8080), '0.0.0.0');
    for (const signal of ['SIGTERM', 'SIGINT']) process.on(signal, () => app.close(() => process.exit(0)));
  } catch { process.stderr.write('store_verifier_configuration_invalid\n'); process.exitCode = 1; }
}
