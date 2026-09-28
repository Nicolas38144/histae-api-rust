'use strict';

const path = require('node:path');
const { createRequire } = require('node:module');

const INVALID_PHOTO = 20;
const PHOTO_TOO_LARGE = 21;
const CODEC_FAILURE = 22;
const MAX_INPUT_BYTES = 500_000;

async function main() {
  const filename = process.argv[2];
  const mimetype = process.argv[3];
  if (!filename || !mimetype) {
    process.exitCode = CODEC_FAILURE;
    return;
  }
  const requireCodec = createRequire(path.join(process.cwd(), 'package.json'));
  const { processPhoto, InvalidPhotoError, PhotoTooLargeError } = requireCodec('./processor.cjs');
  const chunks = [];
  let size = 0;
  for await (const chunk of process.stdin) {
    size += chunk.length;
    if (size > MAX_INPUT_BYTES) {
      process.exitCode = PHOTO_TOO_LARGE;
      return;
    }
    chunks.push(chunk);
  }
  try {
    const result = await processPhoto({ filename, mimetype, body: Buffer.concat(chunks, size) });
    await new Promise((resolve, reject) => {
      process.stdout.end(result.body, error => error ? reject(error) : resolve());
    });
  } catch (error) {
    if (error instanceof PhotoTooLargeError) process.exitCode = PHOTO_TOO_LARGE;
    else if (error instanceof InvalidPhotoError) process.exitCode = INVALID_PHOTO;
    else process.exitCode = CODEC_FAILURE;
  }
}

main().catch(() => { process.exitCode = CODEC_FAILURE; });
