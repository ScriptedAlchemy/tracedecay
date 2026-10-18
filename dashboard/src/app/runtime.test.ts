import { describe, expect, it } from 'vitest';
import { dashboardRouterBasename } from './embedBasename.ts';

describe('dashboardRouterBasename', () => {
  it('uses the embed path only as an exact prefix', () => {
    expect(dashboardRouterBasename('/api/plugins/tracedecay/embed')).toBe(
      '/api/plugins/tracedecay/embed',
    );
    expect(dashboardRouterBasename('/api/plugins/tracedecay/embed/agents')).toBe(
      '/api/plugins/tracedecay/embed',
    );
    expect(dashboardRouterBasename('/api/plugins/tracedecay/embed-evil')).toBeUndefined();
    expect(dashboardRouterBasename('/delivery')).toBeUndefined();
    expect(dashboardRouterBasename('/')).toBeUndefined();
  });
});
