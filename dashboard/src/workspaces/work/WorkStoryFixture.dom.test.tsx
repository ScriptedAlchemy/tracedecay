/**
 * The Work surface, against the fixtures the visual audit serves it.
 *
 * The component fixture remains useful while the Work authority is
 * unavailable, but the visual-audit registry must not advertise an unmounted
 * route. The rendering test separately preserves the fixture contract.
 */
import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { WorkPage } from './WorkPage.tsx';
import { resolveFixture } from '../../../stories/fixtures/data.ts';
import { STORY_SURFACES } from '../../../stories/registry.ts';
import { useScope } from '../../data/scope/store.ts';

// No EventsProvider is mounted here, and the subject is the snapshot read
// rather than the live stream, so both hooks answer as a connected feed that
// has carried nothing.
vi.mock('../../data/sse/useEvents.tsx', () => ({
  useLiveActivity: () => ({ pulses: [], revision: 0 }),
  useEventStreamState: () => ({ state: 'live', lastEventAt: null }),
}));

beforeEach(() => {
  useScope.getState().selectAllProjects();
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const url = new URL(String(input), 'http://localhost');
      return new Response(JSON.stringify(resolveFixture(url.pathname, url.search)), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
});

afterEach(() => {
  vi.unstubAllGlobals();
});

function renderWork() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  return render(
    <MemoryRouter>
      <QueryClientProvider client={client}>
        <WorkPage />
      </QueryClientProvider>
    </MemoryRouter>,
  );
}

describe('the Work surface the visual audit screenshots', () => {
  it('does not advertise the unavailable route as a wired surface', () => {
    const work = STORY_SURFACES.find((surface) => surface.id === 'work');
    expect(work).toBeUndefined();
  });

  it('draws the board from the fixture, not a refusal plate', async () => {
    renderWork();

    // A task title only exists on this page if the snapshot decoded: the
    // application envelope opened, and the payload satisfied
    // `WorkProjectionSnapshotV1Schema`.
    await waitFor(() =>
      expect(
        screen.getByText('Gate fixture payloads against their generated contract'),
      ).toBeTruthy(),
    );

    const page = screen.getByTestId('work-page');
    expect(page.getAttribute('data-work-authority')).toBe('read');
    // The refusal the catch-all `{}` used to produce.
    expect(screen.queryByText(/unsupported_schema/i)).toBeNull();
  });
});
