import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter, useLocation } from 'react-router';
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest';
import { fixtureServer } from '../../../stories/fixtures/handlers.ts';
import { AgentsPage } from './AgentsPage.tsx';

/**
 * The Agents delegation topology over the fixture reading. It draws hollow
 * rings; hover inspects without selecting, click selects and lifts the
 * selection's subtree. The timeline view that once sat beside it drew the same
 * tree from the same read and moved to Loom (issue #4220).
 */
const server = fixtureServer();
beforeAll(() => server.listen({ onUnhandledFrame: 'error' }));
afterEach(() => server.resetHandlers());
afterAll(() => server.close());

const address = { search: '' };
function AddressProbe() {
  address.search = useLocation().search;
  return null;
}

const inspector = () => document.querySelector('[data-agent-inspector]')!;
const control = (id: string) =>
  document.querySelector<HTMLElement>(`[data-topology-control="session"][data-topology-id="${id}"]`)!;

/** Settled once the population line reconciles: every session read is drawn. */
async function renderAgents(entry = '/agents') {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={[entry]}>
        <AddressProbe />
        <AgentsPage />
      </MemoryRouter>
    </QueryClientProvider>,
  );
  const line = await screen.findByText(/^\d+ sessions · \d+ drawn · \d+ generations$/);
  const [, read, drawn] = /^(\d+) sessions · (\d+) drawn/.exec(line.textContent!)!;
  expect(drawn).toBe(read);
}

describe('the delegation topology', () => {
  it('topology is the default: rings named by kind, cores lit by sessions beneath', async () => {
    await renderAgents();
    const rings = [...document.querySelectorAll('[data-topology-ring]')].map((node) => [
      node.getAttribute('data-topology-ring'),
      node.getAttribute('data-topology-core'),
    ]);
    expect(rings).toEqual([
      ['origin', '0.36'],
      ['cut', '0.06'],
      ['origin', '0.06'],
      ['delegate', '0.249'],
      ['delegate', '0.06'],
    ]);
    fireEvent.mouseEnter(control('codex:session.codex.child'));
    const card = document.querySelector('[data-topology-hovercard="codex:session.codex.child"]')!;
    expect(card.textContent).toContain('beneath1');
    expect(card.textContent).toContain('span1,400 s');
    expect(card.textContent).toContain('tokens357,250 tokenspartial');
    expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('inspecting');
    expect(control('codex:session.codex.child').getAttribute('aria-pressed')).toBe('false');
  });

  it('a selection lifts its subtree with a halo while the rest dims', async () => {
    await renderAgents();
    fireEvent.click(control('codex:session.codex.child'));
    fireEvent.mouseLeave(screen.getByRole('group', { name: 'Delegation topology field' }));
    await waitFor(() => expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('selected'));
    expect(document.querySelectorAll('[data-topology-selected]')).toHaveLength(1);
    expect(document.querySelectorAll('[data-topology-halo]')).toHaveLength(1);
    expect(document.querySelectorAll('[data-topology-lifted]')).toHaveLength(1);
    expect(control('codex:session.codex.root').className).toContain('opacity-40');
    expect(control('codex:session.codex.grandchild').className).not.toContain('opacity-40');
  });
});
