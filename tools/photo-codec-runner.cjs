'use strict';

const { createRequire } = require('node:module');
const path = require('node:path');

const INVALID_PHOTO = 20;
const PHOTO_TOO_LARGE = 21;
const CODEC_FAILURE = 22;
const MAX_INPUT_BYTES = 500_000;

async function main() {
  const nestRoot = process.env.HISTAE_NEST_ROOT;
  const filename = process.argv[2];
  const mimetype = process.argv[3];
  if (!nestRoot || !path.isAbsolute(nestRoot) || !filename || !mimetype) {
    process.exitCode = CODEC_FAILURE;
    return;
  }

  const requireFromNest = createRequire(path.join(nestRoot, 'package.json'));
  requireFromNest('ts-node/register/transpile-only');
  const {
    InvalidPhotoError,
    PhotoProcessorService,
    PhotoTooLargeError,
  } = requireFromNest(path.join(nestRoot, 'src', 'photos', 'photo-processor.service.ts'));

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
    const result = await new PhotoProcessorService().toWebp({
      filename,
      mimetype,
      body: Buffer.concat(chunks, size),
    });
    await new Promise((resolve, reject) => {
      process.stdout.end(result.body, (error) => error ? reject(error) : resolve());
    });
  } catch (error) {
    if (error instanceof PhotoTooLargeError) {
      process.exitCode = PHOTO_TOO_LARGE;
    } else if (error instanceof InvalidPhotoError) {
      process.exitCode = INVALID_PHOTO;
    } else {
      process.exitCode = CODEC_FAILURE;
    }
  }
}

main().catch(() => {
  process.exitCode = CODEC_FAILURE;
});
