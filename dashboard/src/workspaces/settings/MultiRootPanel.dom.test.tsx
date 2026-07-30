/**
 * The multi-root capability, from a bundle nobody was reading.
 *
 * `mod.rs::capabilities` builds `multi_root` as the generated
 * `MultiRootCapabilityV1` on every request. No dashboard code parsed it, and
 * the fixture bundle did not even carry the member — so the capability was
 * both unread and unrepresented, and the audit could not have caught either
 * from a screenshot.
 *
 * Four readings are pinned here. The pair that matters is `absent` against
 * `unavailable`: a daemon that never mentioned the capability and a daemon
 * that mentioned it in order to decline are different facts, and collapsing
 * them would attribute a reason to a daemon that gave none.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { MultiRootPanel } from './MultiRootPanel.tsx';
import { multiRootReading, CapabilitiesReadSchema } from '../../data/query/capabilities.ts';
import { resolveFixture } from '../../../stories/fixtures/data.ts';

function serve(body: unknown, status = 200) {
  return vi.fn(
    async () =>
      ({
        ok: status >= 200 && status < 300,
        status,
        json: async () => body,
      }) as Response,
  );
}

function renderPanel() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <QueryClientProvider client={client}>
      <MultiRootPanel />
    </QueryClientProvider>,
  );
}

/** The bundle's other members, which the panel ignores but the route sends. */
const REST = { name: 'tracedecay-dashboard', mode: 'standalone', features: {} };

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('the multi-root capability the daemon reports', () => {
  it('shows a mounted scope set with its roots, revision and digest', async () => {
    vi.stubGlobal(
      'fetch',
      serve({
        ...REST,
        multi_root: {
          status: 'mounted',
          scope_set_id: 'scope-set.alpha',
          revision: 12,
          scope_set_digest: 'sha256:aaaabbbbccccdddd',
          root_count: 4,
        },
      }),
    );
    renderPanel();

    const panel = await screen.findByLabelText('Multi-root scope set');
    await waitFor(() =>
      expect(panel.querySelector('[data-multi-root="mounted"]')).not.toBeNull(),
    );
    expect(panel.textContent).toContain('4');
    expect(panel.textContent).toContain('12');
    expect(panel.textContent).toContain('scope-set.alpha');
  });

  /**
   * The claim the capability does not license. `MultiRootQueryReadModelV1` is
   * generated and unserved, so a mounted scope set must not be presented as a
   * dashboard that can read across it.
   */
  it('says a mounted scope set is still not a federated read', async () => {
    vi.stubGlobal(
      'fetch',
      serve({
        ...REST,
        multi_root: {
          status: 'mounted',
          scope_set_id: 'scope-set.alpha',
          revision: 1,
          scope_set_digest: 'sha256:d1',
          root_count: 2,
        },
      }),
    );
    renderPanel();

    const panel = await screen.findByLabelText('Multi-root scope set');
    await waitFor(() => expect(panel.textContent).toMatch(/no route runs a query across it/i));
    expect(panel.textContent).toMatch(/still one root/i);
  });

  it('carries the daemon’s own reason when no scope set is mounted', async () => {
    vi.stubGlobal(
      'fetch',
      serve({
        ...REST,
        multi_root: { status: 'unavailable', reason: 'authorized scope set is not mounted' },
      }),
    );
    renderPanel();

    const panel = await screen.findByLabelText('Multi-root scope set');
    await waitFor(() =>
      expect(panel.querySelector('[data-multi-root="unavailable"]')).not.toBeNull(),
    );
    expect(panel.textContent).toContain('authorized scope set is not mounted');
  });

  it('keeps a daemon that never mentioned the capability out of that reason', async () => {
    vi.stubGlobal('fetch', serve({ ...REST }));
    renderPanel();

    const panel = await screen.findByLabelText('Multi-root scope set');
    await waitFor(() => expect(panel.querySelector('[data-multi-root="absent"]')).not.toBeNull());
    // Neither the decline nor its reason may be invented for a silent bundle.
    expect(panel.textContent).not.toMatch(/not mounted/i);
    expect(panel.querySelector('[data-multi-root="unavailable"]')).toBeNull();
  });

  it('reports a capability read that failed as unknown, not as no scope set', async () => {
    vi.stubGlobal('fetch', serve({}, 500));
    renderPanel();

    const panel = await screen.findByLabelText('Multi-root scope set');
    await waitFor(() => expect(panel.textContent).toMatch(/did not answer/i));
    expect(panel.querySelector('[data-multi-root]')).toBeNull();
  });
});

describe('the reading, as a function', () => {
  it('separates a silent bundle from a declined one', () => {
    expect(multiRootReading(undefined)).toEqual({ state: 'absent' });
    expect(multiRootReading({ status: 'unavailable', reason: 'no scope set' })).toEqual({
      state: 'unavailable',
      reason: 'no scope set',
    });
  });

  it('never reports a federated query as mounted', () => {
    const reading = multiRootReading({
      status: 'mounted',
      scope_set_id: 's',
      revision: 1,
      scope_set_digest: 'd',
      root_count: 9,
    });
    expect(reading.state).toBe('mounted');
    if (reading.state !== 'mounted') return;
    expect(reading.federatedQueryMounted).toBe(false);
    expect(reading.rootCount).toBe(9);
  });
});

describe('the capabilities fixture', () => {
  /**
   * The fixture carried no `multi_root` at all, so every screenshot of this
   * dashboard showed a bundle the daemon does not send. The bundle's own
   * shape is held by `endpoint-fixtures.test.ts`; what this adds is that the
   * member is present and readable as a mounted capability.
   */
  it('carries the member the daemon sends', () => {
    const parsed = CapabilitiesReadSchema.safeParse(resolveFixture('/api/capabilities'));
    expect(parsed.success).toBe(true);
    if (!parsed.success) return;
    expect(multiRootReading(parsed.data.multi_root).state).toBe('mounted');
  });
});
