import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { MemoryRouter } from 'react-router';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { resolveFixture } from '../../../stories/fixtures/data.ts';
import { CodePage } from './CodePage.tsx';

/**
 * Renderer parity/fallback: renderer-neutral interaction and fallback
 * contract.
 *
 * Here the canvas IS the subject, and jsdom's missing 2D context is not an
 * obstacle but the exact browser this contract exists for: one that hands
 * back no drawing context. The relief field must degrade to a stated
 * truthful reading, never a blank rectangle, while the semantic surfaces
 * beside it (hub list, search results, inspector) keep carrying the same
 * stable-ID selection model on their own. The canvas is supplementary; the
 * accessible equivalent is authoritative.
 */

function mockFetch() {
  vi.stubGlobal(
    'fetch',
    vi.fn(async (input: RequestInfo | URL) => {
      const { pathname, search } = new URL(String(input), 'http://localhost');
      return new Response(JSON.stringify(resolveFixture(pathname, search)), {
        status: 200,
        headers: { 'content-type': 'application/json' },
      });
    }),
  );
}

function renderCode() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/code']}>
        <CodePage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('a browser without a 2D canvas context', () => {
  it('states the undrawable canvas and points at the text alternative', async () => {
    mockFetch();
    renderCode();

    // The truthful reading: the field names its own absence and where every
    // symbol still is, instead of rendering an empty box that reads as "no
    // graph". The fixture answers the 250-symbol request whole.
    const state = await screen.findByText(
      'this browser gave no 2D canvas, so the 250-symbol field is not drawn; the symbol list and inspector beside it carry every symbol',
    );
    expect(state.closest('[data-state]')?.getAttribute('data-state')).toBe('unavailable');
  });

  it('keeps the accessible hub list rendering the same graph', async () => {
    mockFetch();
    renderCode();

    // Parity: the list beside the canvas is fed by the same overview payload,
    // so the symbols the canvas would have drawn are still on screen as text.
    expect(await screen.findByText('find_direct_child_by_kind')).toBeTruthy();
    expect((await screen.findAllByText(/12,873/)).length).toBeGreaterThan(0);
  });

  it('still resolves selection through the hub list without the canvas', async () => {
    mockFetch();
    const user = userEvent.setup();
    renderCode();

    // The hub cards are the accessible selection surface. Pinning one must
    // open the inspector on the symbol's own identity, the selection model
    // is stable IDs in payloads, not anything the renderer owns.
    await user.click(
      await screen.findByRole('button', { name: /find_direct_child_by_kind/ }),
    );

    const inspector = await screen.findByRole('complementary', { name: 'Inspector' });
    expect(await within(inspector).findByText('pinned · url identity')).toBeTruthy();
    expect(within(inspector).getByRole('heading', { name: 'find_direct_child_by_kind' })).toBeTruthy();
    expect(
      within(inspector).getByRole('button', { name: /trace call topography/i }),
    ).toBeTruthy();
  });
});
