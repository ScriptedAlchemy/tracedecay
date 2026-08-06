import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { resolveFixture } from '../../../stories/fixtures/data.ts';
import { CodePage } from './CodePage.tsx';

/**
 * Renderer parity/fallback acceptance (Plan 11, "Renderer-neutral interaction
 * and fallback contract").
 *
 * The sibling suites mock GraphCanvas because the canvas is not their
 * subject. Here it IS the subject, and jsdom's missing WebGL context is not
 * an obstacle but the exact browser this contract exists for: one with no
 * usable GPU context. The default renderer (Sigma) must degrade to a stated
 * truthful reading — never a blank rectangle — while the semantic surfaces
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
      <CodePage />
    </QueryClientProvider>,
  );
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('a browser without a WebGL context', () => {
  it('states the undrawable canvas and points at the text alternative', async () => {
    mockFetch();
    renderCode();

    // The truthful reading: the canvas names its own absence and the caller's
    // fallback description, instead of mounting Sigma into a dead context or
    // rendering an empty box that reads as "no graph".
    expect(await screen.findByText(/no WebGL context/i)).toBeTruthy();
    expect(
      screen.getByText(/symbol results and inspector below remain available/i),
    ).toBeTruthy();
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

    // The hub cards are the accessible selection surface. Selecting one must
    // open the inspector with the symbol's own identity — the selection model
    // is stable IDs in payloads, not anything the renderer owns.
    await user.click(
      await screen.findByRole('button', { name: /find_direct_child_by_kind/ }),
    );

    expect(await screen.findByText('Symbol')).toBeTruthy();
    expect(
      await screen.findByRole('button', { name: /tracing this symbol/i }),
    ).toBeTruthy();
  });
});
