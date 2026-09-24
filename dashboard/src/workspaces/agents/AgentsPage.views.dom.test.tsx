import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter, useLocation } from 'react-router';
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest';
import { fixtureServer } from '../../../stories/fixtures/handlers.ts';
import { AgentsPage } from './AgentsPage.tsx';

/**
 * The two Agents views over the fixture reading. Topology is the default and
 * draws hollow rings; Timeline is a real view chosen from the workspace's view
 * bar and kept in `?view`. Both hand the page the same acts: hover inspects
 * without selecting, click selects and lifts the selection's subtree.
 */
const server = fixtureServer();
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }));
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

describe('agents views', () => {
  it('topology is the default: rings named by kind, cores lit by sessions beneath', async () => {
    await renderAgents();
    expect(document.querySelector('[data-agents-view]')!.getAttribute('data-agents-view')).toBe('topology');
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
    expect(card.textContent).toContain('tokensabsent');
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

  it('timeline is a view kept in the address, with open ends, inferred joins and a recency ramp', async () => {
    await renderAgents();
    fireEvent.click(document.querySelector('[data-agents-view-option="timeline"]')!);
    await waitFor(() => expect(address.search).toBe('?view=timeline'));
    expect(screen.getByText('Delegation timeline · read-only')).toBeTruthy();
    expect(
      [...document.querySelectorAll('[data-delegation-timeline] [data-topology-control="session"]')].map((node) =>
        node.getAttribute('data-topology-id'),
      ),
    ).toEqual([
      'codex:session.codex.root',
      'codex:session.codex.child',
      'codex:session.codex.grandchild',
      'claude:session.claude.orphan',
      'cursor:session.cursor.solo',
    ]);
    expect(document.querySelectorAll('[data-timeline-bracket="spawn"]')).toHaveLength(2);
    expect(document.querySelectorAll('[data-timeline-bracket="join"]')).toHaveLength(1);
    expect(document.querySelectorAll('[data-timeline-bar="open"]')).toHaveLength(1);
    expect(document.querySelector('[data-timeline-lane="0"]')!.getAttribute('data-timeline-peak')).toBe('2');
    expect(
      [...document.querySelectorAll('[data-timeline-recency]')].map((node) => node.getAttribute('data-timeline-recency')),
    ).toEqual(['0.4', '0.267', '0.4', '0.225', '0.308']);

    fireEvent.click(control('codex:session.codex.child'));
    await waitFor(() => expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('selected'));
    expect(control('codex:session.codex.child').getAttribute('aria-pressed')).toBe('true');
  });

  it('opens on the view the address names', async () => {
    await renderAgents('/agents?view=timeline');
    expect(document.querySelector('[data-agents-view]')!.getAttribute('data-agents-view')).toBe('timeline');
    expect(document.querySelector('[data-delegation-timeline]')).not.toBeNull();
  });
});
