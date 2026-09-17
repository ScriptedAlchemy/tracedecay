import { screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { INBOX, INBOX_BRANCH_ONLY } from '../../test/deliveryFixtures.ts';
import { PR_8, renderDelivery } from '../../test/renderDelivery.tsx';

const WORK_UMBRELLA = 'shared_work_objective%3Awork.retry-backoff';
const AGENT_UMBRELLA = 'shared_agent%3Aagent.claude-code';

function readout(bar: HTMLElement, label: string): string {
  const legend = within(bar).getByText(label);
  return legend.parentElement?.querySelector('[data-cell="numeric"]')?.textContent ?? '';
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('UmbrellaWorkspace', () => {
  it('lists every umbrella with its basis and grade, inspects the first, and counts the projection', async () => {
    renderDelivery(INBOX, { route: '/delivery?mode=umbrella' });

    const outcomes = await screen.findByRole('region', { name: 'Umbrella outcomes' });
    expect(within(outcomes).getAllByRole('button')).toHaveLength(2);
    expect(within(outcomes).getByText('WORK / EXPLICIT')).toBeTruthy();
    expect(within(outcomes).getByText('AGENT / INFERRED')).toBeTruthy();
    expect(
      within(outcomes).getByRole('button', { name: /Shared Work objective/, pressed: true }),
    ).toBeTruthy();
    expect(within(outcomes).getByRole('button', { name: /Shared agent/, pressed: false })).toBeTruthy();

    const inspector = screen.getByRole('region', { name: 'Umbrella inspector' });
    expect(within(inspector).getByRole('heading', { name: 'Shared Work objective' })).toBeTruthy();
    expect(within(inspector).getByText('work.retry-backoff')).toBeTruthy();
    expect(within(inspector).getByText(/not repository truth/)).toBeTruthy();
    expect(within(inspector).getByText('Admit delivery inbox')).toBeTruthy();
    expect(within(inspector).getByText('Emit retry events')).toBeTruthy();
    expect(within(inspector).getAllByText(/read-only provider/i).length).toBeGreaterThan(0);

    const bar = screen.getByLabelText('Umbrella readings');
    expect(readout(bar, 'umbrellas')).toBe('2');
    expect(readout(bar, 'correlating edges')).toBe('5');
    expect(readout(bar, 'PRs grouped')).toBe('3');
    expect(readout(bar, 'projects spanned')).toBe('2');
    expect(readout(bar, 'unresolved edges')).toBe('0');

    expect(screen.getByRole('group', { name: 'Delivery outcome field' })).toBeTruthy();
    expect(screen.getByText(/Umbrella · Shared Work objective · work.retry-backoff/)).toBeTruthy();
    expect(screen.getByTestId('location').textContent).not.toContain('umbrella=');
  });

  it('selects the umbrella named in the URL and labels an inferred grouping as reversible', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=umbrella&umbrella=${AGENT_UMBRELLA}` });

    const inspector = await screen.findByRole('region', { name: 'Umbrella inspector' });
    expect(within(inspector).getByRole('heading', { name: 'Shared agent' })).toBeTruthy();
    expect(within(inspector).getByText('agent.claude-code')).toBeTruthy();
    expect(within(inspector).getByText(/reversible/)).toBeTruthy();
    expect(within(inspector).getByText('Persist retry backoff')).toBeTruthy();
    expect(within(inspector).getByText('Emit retry events')).toBeTruthy();
    expect(within(inspector).queryByText('Admit delivery inbox')).toBeNull();
    expect(within(inspector).getAllByText('AGENT / INFERRED').length).toBeGreaterThanOrEqual(3);

    const outcomes = screen.getByRole('region', { name: 'Umbrella outcomes' });
    expect(within(outcomes).getByRole('button', { name: /Shared agent/, pressed: true })).toBeTruthy();
  });

  it('moves between umbrellas through the URL and opens a member journey by its own row id', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX, { route: '/delivery?mode=umbrella' });

    const outcomes = await screen.findByRole('region', { name: 'Umbrella outcomes' });
    await user.click(within(outcomes).getByRole('button', { name: /Shared agent/ }));
    expect(screen.getByTestId('location').textContent).toContain(`umbrella=${AGENT_UMBRELLA}`);
    await user.click(within(outcomes).getByRole('button', { name: /Shared Work objective/ }));
    expect(screen.getByTestId('location').textContent).toContain(`umbrella=${WORK_UMBRELLA}`);

    const inspector = screen.getByRole('region', { name: 'Umbrella inspector' });
    const member = within(inspector).getByText('Emit retry events').closest('li');
    expect(member).not.toBeNull();
    await user.click(within(member as HTMLElement).getByRole('button', { name: 'Select' }));
    expect(screen.getByTestId('location').textContent).toContain(`pr=${PR_8}`);
    expect(screen.getByTestId('location').textContent).toContain('mode=umbrella');

    await user.click(within(member as HTMLElement).getByRole('button', { name: 'Open journey' }));
    const location = screen.getByTestId('location').textContent ?? '';
    expect(location).toContain('mode=journey');
    expect(location).toContain(`pr=${PR_8}`);
  });

  it('prints the daemon reason as a typed unavailable plate when no correlating edge was served', async () => {
    const user = userEvent.setup();
    renderDelivery(INBOX_BRANCH_ONLY, { route: '/delivery?mode=umbrella' });

    expect(await screen.findByText('Umbrella correlation unavailable')).toBeTruthy();
    expect(screen.getByText(/served no cross-PR correlation edge/)).toBeTruthy();
    expect(screen.getByText(/Proximity never groups/)).toBeTruthy();
    expect(screen.queryByRole('group', { name: 'Delivery outcome field' })).toBeNull();
    expect(screen.queryByRole('region', { name: 'Umbrella outcomes' })).toBeNull();
    expect(screen.queryByLabelText('Umbrella readings')).toBeNull();

    await user.click(screen.getByRole('button', { name: 'Back to inbox' }));
    expect(screen.getByTestId('location').textContent).not.toContain('mode=');
  });

  it('renders the exact membership table with one row per umbrella member and no field', async () => {
    renderDelivery(INBOX, { route: '/delivery?mode=umbrella&layout=table' });

    const table = await screen.findByRole('table', { name: 'Umbrella membership table' });
    const body = table.querySelector('tbody');
    expect(body).not.toBeNull();
    expect(within(body as HTMLElement).getAllByRole('row')).toHaveLength(4);
    expect(within(table).getAllByText('WORK / EXPLICIT')).toHaveLength(2);
    expect(within(table).getAllByText('AGENT / INFERRED')).toHaveLength(2);
    expect(within(table).getAllByRole('button', { name: 'Journey' })).toHaveLength(4);
    expect(within(table).getAllByRole('columnheader')).toHaveLength(9);
    expect(screen.queryByRole('group', { name: 'Delivery outcome field' })).toBeNull();
    expect(screen.getByRole('group', { name: 'Layout' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'table', pressed: true })).toBeTruthy();
  });

  it('exposes no provider mutation control anywhere in the umbrella workspace', async () => {
    renderDelivery(INBOX, { route: `/delivery?mode=umbrella&umbrella=${WORK_UMBRELLA}` });
    await screen.findByRole('region', { name: 'Umbrella inspector' });
    for (const verb of [/merge/i, /rerun/i, /re-run/i, /resolve/i, /approve/i]) {
      expect(screen.queryByRole('button', { name: verb })).toBeNull();
    }
  });
});
