'use strict';

const { createRequire } = require('node:module');
const fs = require('node:fs/promises');
const path = require('node:path');

async function main() {
  const nestRoot = process.argv[2];
  const output = process.argv[3];
  if (!nestRoot || !output) process.exit(2);
  const requireFromNest = createRequire(path.join(nestRoot, 'package.json'));
  const sharp = requireFromNest('sharp');
  await fs.mkdir(output, { recursive: true });

  const orientedPixels = Buffer.from([
    255, 0, 0, 0, 255, 0, 0, 0, 255,
    255, 255, 0, 255, 0, 255, 0, 255, 255,
  ]);
  await sharp(orientedPixels, { raw: { width: 3, height: 2, channels: 3 } })
    .jpeg({ quality: 100 })
    .withMetadata({ orientation: 6 })
    .toFile(path.join(output, 'orientation-6.jpg'));

  const frameA = Buffer.alloc(10 * 10 * 4);
  const frameB = Buffer.alloc(10 * 10 * 4);
  for (let offset = 0; offset < frameA.length; offset += 4) {
    frameA[offset] = 255;
    frameA[offset + 3] = 255;
    frameB[offset + 2] = 255;
    frameB[offset + 3] = 255;
  }
  await sharp(Buffer.concat([frameA, frameB]), {
    raw: { width: 10, height: 20, channels: 4, pageHeight: 10 },
  })
    .webp({ loop: 0, delay: [100, 100] })
    .toFile(path.join(output, 'animated.webp'));

  await sharp({
    create: {
      width: 6_667,
      height: 6_000,
      channels: 3,
      background: { r: 12, g: 34, b: 56 },
    },
  })
    .jpeg({ quality: 1 })
    .toFile(path.join(output, 'over-40mp.jpg'));
}

main().catch(() => process.exit(2));
