import { appleAdapter } from './providers/apple.mjs';
import { googlePlayAdapter } from './providers/google-play.mjs';
import { StoreError, invalid } from './providers/validation.mjs';

export { appleSnapshot } from './providers/apple.mjs';
export { googleSnapshot } from './providers/google-play.mjs';
export { StoreError, digest } from './providers/validation.mjs';

// Composition only. Each adapter owns its SDK, verification and provider protocol.
export function providers(config) {
  const apple = config.apple.enabled ? appleAdapter(config) : null;
  const google = config.google.enabled ? googlePlayAdapter(config) : null;
  const enabled = (adapter) => {
    if (!adapter) throw new StoreError('store_unavailable', 503);
    return adapter;
  };
  return {
    async verify(provider, reference) {
      if (provider === 'app_store') return enabled(apple).verify(reference);
      if (provider === 'google_play') return enabled(google).verify(reference);
      return invalid();
    },
    async notification(provider, body, authorization) {
      if (provider === 'app_store') return enabled(apple).notification(body);
      if (provider === 'google_play') return enabled(google).notification(body, authorization);
      throw new StoreError('store_unavailable', 503);
    },
    async acknowledge(reference) { return enabled(google).acknowledge(reference); },
  };
}
