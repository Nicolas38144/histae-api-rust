import { createHash } from 'node:crypto';

export class StoreError extends Error {
  constructor(code = 'store_verification_failed', status = 422) { super(code); this.code = code; this.status = status; }
}
export const invalid = () => { throw new StoreError(); };
export const text = (value, maximum = 4096) => typeof value === 'string' && value.length > 0 && value.length <= maximum ? value : invalid();
export const instant = (value) => {
  if (!['number', 'string'].includes(typeof value)) invalid();
  const date = new Date(value);
  return Number.isFinite(date.getTime()) ? date.toISOString() : invalid();
};
export const eventUuid = (value) => /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value || '') ? value.toLowerCase() : invalid();
export const uuid = (value) => /^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/i.test(value || '') ? value.toLowerCase() : invalid();
export const digest = (value) => createHash('sha256').update(value).digest('hex');

