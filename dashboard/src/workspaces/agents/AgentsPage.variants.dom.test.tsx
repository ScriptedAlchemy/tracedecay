import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest';
import { fixtureServer } from '../../../stories/fixtures/handlers.ts';
import { AgentsPage } from './AgentsPage.tsx';

/**
 * Every delegation renderer draws the same fixture reading and hands the page
 * the same acts: hover inspects without selecting, click selects, and the
 * reconciled population line is identical whichever renderer is chosen.
 */
const server = fixtureServer();
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }));
afterEach(() => server.resetHandlers());
afterAll(() => server.close());

const inspector = () => document.querySelector('[data-agent-inspector]')!;
const control = (id: string) =>
  document.querySelector<HTMLElement>(`[data-topology-control="session"][data-topology-id="${id}"]`)!;

async function renderAgents(variant: string) {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false, gcTime: 0 } } });
  render(
    <QueryClientProvider client={client}>
      <AgentsPage />
    </QueryClientProvider>,
  );
  await screen.findByText('5 sessions · 5 drawn · 3 generations');
  fireEvent.click(document.querySelector(`[data-topology-variant="${variant}"]`)!);
  expect(document.querySelector(`[data-topology-variant="${variant}"]`)!.getAttribute('aria-checked')).toBe('true');
}

describe('delegation renderers', () => {
  it('rings: hollow rings name the reading kind and hover prints exact counts', async () => {
    await renderAgents('rings');
    expect([...document.querySelectorAll('[data-topology-ring]')].map((node) => node.getAttribute('data-topology-ring'))).toEqual([
      'origin',
      'cut',
      'origin',
      'delegate',
      'delegate',
    ]);
    fireEvent.mouseEnter(control('codex:session.codex.child'));
    const card = document.querySelector('[data-topology-hovercard="codex:session.codex.child"]')!;
    expect(card.textContent).toContain('beneath1');
    expect(card.textContent).toContain('span1,400 s');
    expect(card.textContent).toContain('tokensabsent');
    expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('inspecting');
    expect(control('codex:session.codex.child').getAttribute('aria-pressed')).toBe('false');
  });

  it('timeline: rows in pre-order, open ends and inferred joins stated', async () => {
    await renderAgents('timeline');
    expect(document.querySelector('[data-delegation-timeline]')!.getAttribute('data-delegation-timeline')).toBe('5');
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
    expect(screen.getByText('5 sessions · 5 drawn · 3 generations')).toBeTruthy();

    fireEvent.click(control('codex:session.codex.child'));
    await waitFor(() => expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('selected'));
    expect(control('codex:session.codex.child').getAttribute('aria-pressed')).toBe('true');
  });

  it('radial: the origin is a label, not a session, and deep labels wait for hover', async () => {
    await renderAgents('radial');
    expect(document.querySelector('[data-radial-centre]')!.getAttribute('data-radial-centre')).toBe('origin');
    const labelled = () =>
      [...document.querySelectorAll('[data-radial-label]')].map((node) => node.getAttribute('data-radial-label')).sort();
    // jsdom measures a zero-width aperture, so the field sits at its 420px
    // floor; generations 0 and 1 still label without colliding there.
    expect(labelled()).toEqual([
      'claude:session.claude.orphan',
      'codex:session.codex.child',
      'codex:session.codex.root',
      'cursor:session.cursor.solo',
    ]);
    expect(document.querySelectorAll('[data-radial-fan]')).toHaveLength(2);
    fireEvent.mouseEnter(control('codex:session.codex.grandchild'));
    expect(labelled()).toContain('codex:session.codex.grandchild');
    expect(inspector().getAttribute('data-agent-inspector-id')).toBe('codex:session.codex.grandchild');
  });
});
