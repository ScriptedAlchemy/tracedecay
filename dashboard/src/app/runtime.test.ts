import { describe, expect, it } from 'vitest';
import {
  applyEmbeddedPublicPath,
  dashboardRouterBasename,
  HERMES_EMBED_BASENAME,
} from './embedBasename.ts';

describe('dashboardRouterBasename', () => {
  it('uses the Hermes embed mount and its workspace paths', () => {
    expect(dashboardRouterBasename(HERMES_EMBED_BASENAME)).toBe(HERMES_EMBED_BASENAME);
    expect(dashboardRouterBasename(`${HERMES_EMBED_BASENAME}/delivery`)).toBe(
      HERMES_EMBED_BASENAME,
    );
  });

  it('does not treat sibling or ordinary dashboard paths as the embed', () => {
    expect(dashboardRouterBasename(`${HERMES_EMBED_BASENAME}-evil`)).toBeUndefined();
    expect(dashboardRouterBasename('/delivery')).toBeUndefined();
    expect(dashboardRouterBasename('/')).toBeUndefined();
  });
});

describe('applyEmbeddedPublicPath', () => {
  it('sets the webpack public path only for an embed basename', () => {
    const runtime = globalThis as typeof globalThis & { __webpack_public_path__?: string };
    delete runtime.__webpack_public_path__;
    applyEmbeddedPublicPath(undefined);
    expect(runtime.__webpack_public_path__).toBeUndefined();
    applyEmbeddedPublicPath(HERMES_EMBED_BASENAME);
    expect(runtime.__webpack_public_path__).toBe(`${HERMES_EMBED_BASENAME}/`);
  });
});
