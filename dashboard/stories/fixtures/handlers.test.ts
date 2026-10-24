import { afterAll, beforeAll, describe, expect, it } from 'vitest';

import { fixtureServer } from './handlers.ts';
import { FEEDBACK_PROXIMITY_ROUTE } from '../../src/viz/proximity/proximityApi.ts';
import { callWork } from '../../src/workspaces/work/workApi.ts';

const server = fixtureServer();
beforeAll(() => server.listen({ onUnhandledFrame: 'error' }));
afterAll(() => server.close());

describe('fixture handlers', () => {
  it.each(['/api/events', '/api/projects/tracedecay/events', '/api/projects/repo%2Fworktree/events'])(
    'serves the empty event stream at %s without inventing activity', async (pathname) => {
      const response = await fetch(`http://localhost${pathname}`);
      expect(response.status).toBe(200);
      expect(response.headers.get('content-type')).toBe('text/event-stream');
      expect(await response.text()).toBe(':ok\n\n');
    },
  );

  it.each(['/api/feedback/proximity', '/api/projects/tracedecay/feedback/proximity'])(
    'supports the mounted Loom POST read at %s', async (pathname) => {
      expect(await callWork(
        FEEDBACK_PROXIMITY_ROUTE,
        { observed_at: 1_790_000_000_000_000 },
        `http://localhost${pathname}`,
      )).toMatchObject({ outcome: 'value' });
    },
  );

  it('refuses unbound event suffixes and proximity commands', async () => {
    await expect(fetch('http://localhost/api/projects/tracedecay/events/missing')).rejects.toThrow();
    await expect(fetch('http://localhost/api/projects/tracedecay/feedback/proximity/missing', {
      method: 'POST',
    })).rejects.toThrow();
  });

  it('answer a modelled route and refuse a path the daemon does not bind', async () => {
    const bound = await fetch('http://localhost/api/plugins/holographic?limit=100');
    expect(bound.status).toBe(200);
    expect(((await bound.json()) as { domain_state: string }).domain_state).toBe('ready');

    await expect(fetch('http://localhost/api/plugins/holographic/?limit=100')).rejects.toThrow();
  });
});
