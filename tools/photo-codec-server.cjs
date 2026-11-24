'use strict';

const { createServer } = require('node:http');
const { execFile } = require('node:child_process');
const { lstatSync, unlinkSync } = require('node:fs');
const path = require('node:path');

const limit = 500_000;
const formats = new Map([
  ['/jpeg', 'photo.jpg'], ['/png', 'photo.png'], ['/heif', 'photo.heic'], ['/webp', 'photo.webp'],
]);
const socket = process.argv[2];
if (!socket || !path.isAbsolute(socket)) process.exit(1);
try {
  if (!lstatSync(socket).isSocket()) process.exit(1);
  unlinkSync(socket);
} catch (error) {
  if (error.code !== 'ENOENT') throw error;
}
process.umask(0o177);

let busy = false;
const server = createServer(async (request, response) => {
  const reply = (status, body) => {
    response.writeHead(status, body ? { 'Content-Type': 'image/webp', 'Content-Length': body.length } : {});
    response.end(body);
  };
  if (request.method === 'GET' && request.url === '/health/live') return reply(204);
  const filename = formats.get(request.url);
  if (request.method !== 'POST' || !filename) return reply(404);
  if (busy) return reply(429);
  const length = Number(request.headers['content-length']);
  if (!Number.isSafeInteger(length) || length <= 0) return reply(422);
  if (length > limit) return reply(413);
  busy = true;
  try {
    request.setTimeout(5_000, () => request.destroy());
    const chunks = [];
    let size = 0;
    for await (const chunk of request) {
      size += chunk.length;
      if (size > limit) return reply(413);
      chunks.push(chunk);
    }
    if (size !== length) return reply(422);
    request.setTimeout(0);
    await new Promise(resolve => {
      const child = execFile(process.execPath, [path.join(__dirname, 'photo-codec-runner.cjs'), filename, 'application/octet-stream'], {
        cwd: path.join(__dirname, 'photo-codec'), env: {}, encoding: 'buffer',
        timeout: 34_000, killSignal: 'SIGKILL', maxBuffer: limit,
      }, (error, output) => {
        response.off('close', cancel);
        if (!response.destroyed) {
          if (!error) reply(200, output);
          else reply(error.code === 20 ? 422 : error.code === 21 ? 413 : error.killed ? 504 : 503);
        }
        resolve();
      });
      const cancel = () => child.kill('SIGKILL');
      response.once('close', cancel);
      child.stdin.on('error', () => {});
      child.stdin.end(Buffer.concat(chunks, size));
    });
  } catch {
    if (!response.destroyed) reply(503);
  } finally {
    busy = false;
  }
});
server.headersTimeout = 5_000;
server.requestTimeout = 10_000;
server.listen(socket);
