import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { http, HttpResponse, type JsonBodyType } from 'msw';
import { afterAll, afterEach, beforeAll, describe, expect, it } from 'vitest';
import { resolveFixture } from '../../../stories/fixtures/data.ts';
import { fixtureServer } from '../../../stories/fixtures/handlers.ts';
import { AgentsPage } from './AgentsPage.tsx';

/**
 * The V2 Agents plate's interaction contract, exercised through the shipped
 * page against the fixture daemon: hover inspects and changes nothing that
 * persists; click selects, and a selection is the one act that names the
 * session the token-frontier route is asked about; the exact tree and the
 * field share one selection; Escape clears inspection and nothing else.
 */
const server = fixtureServer();
beforeAll(() => server.listen({ onUnhandledRequest: 'error' }));
afterEach(() => server.resetHandlers());
afterAll(() => server.close());

function renderAgents() {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false, gcTime: 0 } },
  });
  return render(
    <QueryClientProvider client={client}>
      <AgentsPage />
    </QueryClientProvider>,
  );
}

const inspector = () => document.querySelector('[data-agent-inspector]')!;
const mark = (id: string) =>
  document.querySelector<HTMLButtonElement>(`[data-topology-control="session"][data-topology-id="${id}"]`)!;

async function settled() {
  await screen.findByText('5 sessions · 5 drawn · 3 generations');
  await waitFor(() =>
    expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('default'),
  );
}

describe('AgentsPage delegation topology', () => {
  it('draws the reading by generation and inspects the newest root by default', async () => {
    renderAgents();
    await settled();

    // Every drawn session is a real button in column order; the fixture's five
    // sessions are all drawn, so nothing is folded and the counts reconcile.
    const controls = [...document.querySelectorAll('[data-topology-control="session"]')].map(
      (node) => node.getAttribute('data-topology-id'),
    );
    expect(controls).toEqual([
      'codex:session.codex.root',
      'claude:session.claude.orphan',
      'cursor:session.cursor.solo',
      'codex:session.codex.child',
      'codex:session.codex.grandchild',
    ]);
    expect(document.querySelectorAll('[data-topology-control="bundle"]')).toHaveLength(0);
    // The cut edge is drawn as a typed stub, not as a root.
    expect(document.querySelector('[data-topology-stub="missing_parent"]')).toBeTruthy();
    expect(mark('claude:session.claude.orphan').getAttribute('data-topology-link')).toBe('missing_parent');

    // The default subject is the newest top, said so, with its token frontier read.
    expect(inspector().getAttribute('data-agent-inspector-id')).toBe('cursor:session.cursor.solo');
    expect(within(inspector() as HTMLElement).getByText(/default · newest top/)).toBeTruthy();
    await waitFor(() =>
      expect(inspector().querySelector('[data-agent-inspector-tokens]')?.getAttribute('data-agent-inspector-tokens')).toBe('2'),
    );
    // Work handoffs and attempts are shown as authority states with a typed
    // "not joined" gap, never as an empty list for this session.
    expect(inspector().querySelector('[data-agent-inspector-join="handoffs"]')).toBeTruthy();
    expect(inspector().querySelector('[data-agent-inspector-join="attempts"]')).toBeTruthy();
  });

  it('keeps five authority states apart and never totals them', async () => {
    renderAgents();
    await settled();
    const cells = [...document.querySelectorAll('[data-agent-authority]')].map((node) => [
      node.getAttribute('data-agent-authority'),
      node.getAttribute('data-agent-authority-state'),
    ]);
    expect(cells).toEqual([
      ['usage', 'partial'],
      ['hierarchy', 'ready'],
      ['tokens', 'ready'],
      ['work', 'partial'],
      ['failure', 'ready'],
    ]);
    const register = screen.getByLabelText('Independent authorities');
    expect(register.textContent).not.toMatch(/%/);
    expect(register.textContent).toMatch(/1 cut/);
    expect(register.textContent).toMatch(/1 attempts unobserved/);
  });

  it('inspects on hover without selecting or reading', async () => {
    renderAgents();
    await settled();
    const child = mark('codex:session.codex.child');

    fireEvent.mouseEnter(child);
    expect(inspector().getAttribute('data-agent-inspector-id')).toBe('codex:session.codex.child');
    expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('inspecting');
    expect(child.getAttribute('aria-pressed')).toBe('false');
    expect(document.querySelector('[data-topology-selected]')).toBeNull();
    // Hover isolates the path to the top: both edges on it light.
    expect(document.querySelectorAll('[data-topology-lit]')).toHaveLength(2);
    // No read was asked for the hovered session; the inspector says so.
    expect(
      inspector().querySelector('[data-agent-inspector-tokens]')?.getAttribute('data-agent-inspector-tokens'),
    ).toBe('not-requested');
    expect(inspector().querySelector('[data-agent-inspector-incoming]')?.getAttribute('data-agent-inspector-incoming')).toBe('linked');
    expect(within(inspector() as HTMLElement).getByText(/delegated by tool call toolu_codex_01/)).toBeTruthy();

    // Leaving the field returns the inspector to its default subject.
    fireEvent.mouseLeave(screen.getByRole('group', { name: 'Delegation topology field' }));
    expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('default');
    expect(inspector().getAttribute('data-agent-inspector-id')).toBe('cursor:session.cursor.solo');
  });

  it('selects on click, reads that session\'s token frontier, and syncs the exact tree', async () => {
    const asked: string[] = [];
    server.use(
      http.post('*/api/application/handoff/list-task', async ({ request }) => {
        const body = (await request.json()) as { session_id: string };
        asked.push(body.session_id);
        return HttpResponse.json(
          resolveFixture('/api/application/handoff/list-task', '') as JsonBodyType,
        );
      }),
    );
    renderAgents();
    await settled();
    await waitFor(() => expect(asked).toEqual(['session.cursor.solo']));

    const child = mark('codex:session.codex.child');
    fireEvent.click(child);
    expect(child.getAttribute('aria-pressed')).toBe('true');
    expect(document.querySelector('[data-topology-selected]')).toBeTruthy();
    expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('selected');
    // The selection is the act that reads: a second frontier request, for the
    // selected session and no other.
    await waitFor(() => expect(asked).toEqual(['session.cursor.solo', 'session.codex.child']));
    await waitFor(() =>
      expect(inspector().querySelector('[data-agent-inspector-tokens]')?.getAttribute('data-agent-inspector-tokens')).toBe('2'),
    );

    // The exact tree beneath the field carries the same selection.
    const treeRow = document.querySelector('[data-subagent-node="session.codex.child"]')!;
    expect(treeRow.getAttribute('data-subagent-selected')).toBe('true');
    expect(document.querySelectorAll('[data-subagent-selected="true"]')).toHaveLength(1);

    // Selecting from the tree moves the field's selection with it.
    fireEvent.click(within(document.querySelector('[data-subagent-node="session.codex.root"]') as HTMLElement).getByRole('button'));
    expect(mark('codex:session.codex.root').getAttribute('aria-pressed')).toBe('true');
    expect(child.getAttribute('aria-pressed')).toBe('false');
    expect(inspector().getAttribute('data-agent-inspector-id')).toBe('codex:session.codex.root');

    // Clearing the selection from the inspector returns to the default subject.
    fireEvent.click(screen.getByRole('button', { name: 'Clear selection' }));
    expect(document.querySelector('[data-topology-selected]')).toBeNull();
    expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('default');
  });

  it('captions a real click as selected even while the pointer still hovers the mark', async () => {
    renderAgents();
    await settled();
    const child = mark('codex:session.codex.child');
    // A pointer click is enter, focus, click — the mark stays inspected.
    fireEvent.mouseEnter(child);
    fireEvent.focus(child);
    fireEvent.click(child);
    expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('selected');
    expect(screen.getByRole('button', { name: 'Clear selection' })).toBeTruthy();
    // Hovering a different mark inspects it; returning to the selected one
    // reads as the selection again.
    fireEvent.mouseEnter(mark('codex:session.codex.root'));
    expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('inspecting');
    fireEvent.mouseEnter(child);
    expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('selected');
  });

  it('ends inspection when focus leaves the field, as the pointer does', async () => {
    renderAgents();
    await settled();
    const root = mark('codex:session.codex.root');
    root.focus();
    fireEvent.focus(root);
    expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('inspecting');
    // Focus moving to another mark keeps inspecting.
    const child = mark('codex:session.codex.child');
    fireEvent.blur(root, { relatedTarget: child });
    fireEvent.focus(child);
    expect(inspector().getAttribute('data-agent-inspector-id')).toBe('codex:session.codex.child');
    // Focus leaving the field ends it.
    const summary = screen.getByText(/exact tree · 5 sessions/i);
    fireEvent.blur(child, { relatedTarget: summary });
    expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('default');
  });

  it('inspects on keyboard focus and clears inspection on Escape without touching selection', async () => {
    renderAgents();
    await settled();
    const child = mark('codex:session.codex.child');
    fireEvent.click(child);
    expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('selected');

    const root = mark('codex:session.codex.root');
    root.focus();
    fireEvent.focus(root);
    expect(inspector().getAttribute('data-agent-inspector-id')).toBe('codex:session.codex.root');
    expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('inspecting');
    // Focus never substitutes for selection.
    expect(root.getAttribute('aria-pressed')).toBe('false');
    expect(child.getAttribute('aria-pressed')).toBe('true');

    fireEvent.keyDown(root, { key: 'Escape' });
    expect(inspector().getAttribute('data-agent-inspector-id')).toBe('codex:session.codex.child');
    expect(inspector().getAttribute('data-agent-inspector-mode')).toBe('selected');
    expect(child.getAttribute('aria-pressed')).toBe('true');
  });

  it('names every mark for a screen reader with its generation and link state', async () => {
    renderAgents();
    await settled();
    expect(mark('claude:session.claude.orphan').getAttribute('aria-label')).toBe(
      'Claude, claude session session.claude.orphan, generation 0, parent not in this reading. Select to read its token frontier.',
    );
    expect(mark('codex:session.codex.grandchild').getAttribute('aria-label')).toMatch(
      /^Codex, codex session session\.codex\.grandchild, generation 2\. Select to read its token frontier\.$/,
    );
    // The generation headers say what generation 0 actually holds.
    const gen0 = document.querySelector('[data-topology-generation="0"]')!;
    expect(gen0.textContent).toMatch(/gen 0/);
    expect(gen0.textContent).toMatch(/tops/);
    expect(gen0.textContent).toMatch(/2 roots · 1 cut/);
  });
});
