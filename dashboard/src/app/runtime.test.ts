import { describe, expect, it } from 'vitest';
import {
  dashboardRouterBasename,
  HERMES_EMBED_BASENAME,
} from './embedBasename.ts';

describe('dashboardRouterBasename', () => {
  it('does not treat sibling or ordinary dashboard paths as the embed', () => {
    expect(dashboardRouterBasename(`${HERMES_EMBED_BASENAME}-evil`)).toBeUndefined();
    expect(dashboardRouterBasename('/delivery')).toBeUndefined();
    expect(dashboardRouterBasename('/')).toBeUndefined();
  });
});
