import { QueryClient, QueryClientProvider } from '@tanstack/react-query';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { useState } from 'react';
import { MemoryRouter } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { useInspectorStack } from './inspectorStack.ts';
import { CommandPalette } from './CommandPalette.tsx';
import {
  usePaletteEntries,
  usePaletteRegistry,
  type PaletteEntry,
} from './paletteRegistry.ts';

const taskInspector = {
  scope: { kind: 'project', project_id: 'project-alpha' },
  entity: { kind: 'task', id: 'task-42' },
  evidence: { kind: 'attempt', id: 'attempt-9' },
} as const;

function Contributor({ entries }: { entries: readonly PaletteEntry[] }) {
  usePaletteEntries('work-product', entries);
  return null;
}

function Harness({ entries }: { entries: readonly PaletteEntry[] }) {
  const [open, setOpen] = useState(true);
  return (
    <>
      <Contributor entries={entries} />
      <CommandPalette open={open} onOpenChange={setOpen} />
    </>
  );
}

function mount(entries: readonly PaletteEntry[]) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false }, mutations: { retry: false } },
  });
  return render(
    <QueryClientProvider client={client}>
      <MemoryRouter initialEntries={['/work?scope=project-alpha&scopeLabel=Alpha']}>
        <Harness entries={entries} />
      </MemoryRouter>
    </QueryClientProvider>,
  );
}

afterEach(() => {
  usePaletteRegistry.getState().clear();
  useInspectorStack.getState().replace([]);
  vi.unstubAllGlobals();
});

describe('command palette providers', () => {
  it('opens an exact entity identity supplied by a workspace provider', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));
    mount([
      {
        kind: 'inspect',
        id: 'work:task-42',
        label: 'Repair task 42',
        hint: 'task · partial',
        state: 'partial',
        scopeLabel: 'Alpha',
        inspector: taskInspector,
      },
    ]);

    const input = await screen.findByRole('combobox');
    fireEvent.change(input, { target: { value: 'repair task' } });
    const result = await screen.findByRole('option', { name: /Repair task 42/ });
    expect(result.textContent).toContain('partial');
    expect(result.textContent).toContain('Alpha');

    fireEvent.click(result);
    expect(useInspectorStack.getState().entries).toEqual([taskInspector]);
    expect(useInspectorStack.getState().activeKey).toContain('task-42');
  });

  it('invokes only the opaque legal-action reference the provider supplied', async () => {
    vi.stubGlobal('fetch', vi.fn().mockRejectedValue(new Error('offline')));
    const reference = Object.freeze({
      action_id: 'cancel-attempt-9',
      expected_version: 17,
    });
    let received: Readonly<Record<string, unknown>> | null = null;
    mount([
      {
        kind: 'legal_action',
        id: 'work:cancel-attempt-9',
        label: 'Cancel attempt 9',
        hint: 'legal action',
        state: 'ready',
        scopeLabel: 'Alpha',
        reference,
        invoke: (value) => {
          received = value;
        },
      },
    ]);

    const input = await screen.findByRole('combobox');
    fireEvent.change(input, { target: { value: 'cancel attempt' } });
    fireEvent.click(await screen.findByRole('option', { name: /Cancel attempt 9/ }));

    await waitFor(() => expect(received).toBe(reference));
  });
});
