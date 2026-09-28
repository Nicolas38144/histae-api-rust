'use strict';

const { extname } = require('node:path');
const { Worker } = require('node:worker_threads');
const sharp = require('sharp');

const MAX_BYTES = 500_000;
const MAX_PIXELS = 40_000_000;
const MAX_EDGE = 2_048;
const FORMAT = new Map([
  ['.jpg', 'jpeg'], ['.jpeg', 'jpeg'], ['.png', 'png'],
  ['.heic', 'heif'], ['.heif', 'heif'], ['.webp', 'webp'],
]);
const MIME = {
  jpeg: new Set(['image/jpeg', 'image/jpg', 'application/octet-stream']),
  png: new Set(['image/png', 'application/octet-stream']),
  heif: new Set(['image/heic', 'image/heif', 'application/octet-stream']),
  webp: new Set(['image/webp', 'application/octet-stream']),
};

class InvalidPhotoError extends Error {}
class PhotoTooLargeError extends Error {}

async function processPhoto(upload) {
  if (!upload.body.length) throw new InvalidPhotoError();
  if (upload.body.length > MAX_BYTES) throw new PhotoTooLargeError();
  const expected = FORMAT.get(extname(upload.filename).toLowerCase());
  if (!expected || !MIME[expected].has(upload.mimetype.trim().toLowerCase())) {
    throw new InvalidPhotoError();
  }
  try {
    return expected === 'heif'
      ? await convertHeif(upload.body)
      : await convertSharp(upload.body, expected);
  } catch (error) {
    if (error instanceof PhotoTooLargeError || error instanceof InvalidPhotoError) throw error;
    throw new InvalidPhotoError();
  }
}

async function convertSharp(input, expected) {
  const image = sharp(input, {
    failOn: 'error', limitInputPixels: MAX_PIXELS, animated: false, sequentialRead: true,
  });
  const metadata = await image.metadata();
  if (metadata.format !== expected || !metadata.width || !metadata.height
    || metadata.width * metadata.height > MAX_PIXELS || (metadata.pages ?? 1) !== 1) {
    throw new InvalidPhotoError();
  }
  return encodeWebp(image.rotate());
}

async function convertHeif(input) {
  const metadata = await sharp(input, {
    failOn: 'error', limitInputPixels: false, animated: false,
  }).metadata();
  if (metadata.format !== 'heif' || !metadata.width || !metadata.height
    || (metadata.pages ?? 1) !== 1) throw new InvalidPhotoError();
  if (metadata.width * metadata.height > MAX_PIXELS) throw new PhotoTooLargeError();
  const decoded = await decodeHeif(input);
  if (!decoded.width || !decoded.height || decoded.width * decoded.height > MAX_PIXELS
    || decoded.data.byteLength !== decoded.width * decoded.height * 4) {
    throw new InvalidPhotoError();
  }
  return encodeWebp(sharp(Buffer.from(
    decoded.data.buffer, decoded.data.byteOffset, decoded.data.byteLength,
  ), {
    raw: { width: decoded.width, height: decoded.height, channels: 4 },
    limitInputPixels: MAX_PIXELS,
  }));
}

function decodeHeif(input) {
  return new Promise((resolve, reject) => {
    const worker = new Worker(`
      const { parentPort, workerData } = require('node:worker_threads');
      const decode = require('heic-decode');
      Promise.resolve(decode({ buffer: Buffer.from(workerData) }))
        .then(({ width, height, data }) => {
          const pixels = Uint8Array.from(data);
          parentPort.postMessage({ width, height, data: pixels }, [pixels.buffer]);
        })
        .catch(() => parentPort.postMessage({ error: true }));
    `, { eval: true, workerData: input });
    let settled = false;
    const finish = action => {
      if (settled) return;
      settled = true;
      clearTimeout(timer);
      action();
      void worker.terminate();
    };
    const timer = setTimeout(() => finish(() => reject(new Error('timeout'))), 30_000);
    worker.once('message', message => {
      if (!message || !Number.isInteger(message.width) || !Number.isInteger(message.height)
        || !(message.data instanceof Uint8Array)) {
        finish(() => reject(new Error('decode')));
      } else finish(() => resolve(message));
    });
    worker.once('error', error => finish(() => reject(error)));
    worker.once('exit', code => { if (code !== 0) finish(() => reject(new Error('exit'))); });
  });
}

async function encodeWebp(image) {
  for (const { edge, quality } of [
    { edge: MAX_EDGE, quality: 82 }, { edge: 1800, quality: 74 },
    { edge: 1600, quality: 68 }, { edge: 1400, quality: 62 },
    { edge: 1200, quality: 56 }, { edge: 1024, quality: 50 },
  ]) {
    const result = await image.clone()
      .resize({ width: edge, height: edge, fit: 'inside', withoutEnlargement: true })
      .webp({ quality, effort: 4, smartSubsample: true })
      .toBuffer({ resolveWithObject: true });
    if (result.info.format !== 'webp') throw new InvalidPhotoError();
    if (result.data.length <= MAX_BYTES) {
      return { body: result.data, width: result.info.width, height: result.info.height };
    }
  }
  throw new PhotoTooLargeError();
}

module.exports = { processPhoto, InvalidPhotoError, PhotoTooLargeError };
