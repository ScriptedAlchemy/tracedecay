import { act, render, waitFor } from '@testing-library/react';
import { MemoryRouter, useSearchParams } from 'react-router';
import { beforeEach, describe, expect, it } from 'vitest';
import { ScopeUrlSync } from './UrlSync.tsx';
import { useScope } from './store.ts';

let currentSearch = '';
function SearchProbe() {
  const [params] = useSearchParams();
  currentSearch = params.toString();
  return null;
}

function mount(initialEntry: string) {
  return render(
    <MemoryRouter initialEntries={[initialEntry]}>
      <ScopeUrlSync />
      <SearchProbe />
    </MemoryRouter>,
  );
}

describe('ScopeUrlSync', () => {
  beforeEach(() => {
    useScope.setState({ scope: { kind: 'all' } });
    currentSearch = '';
  });

  it('applies a scope deep link to the store', async () => {
    mount('/costs?scope=proj_abc&scopeLabel=tracedecay');
    await waitFor(() => {
      const scope = useScope.getState().scope;
      expect(scope).toEqual({ kind: 'project', projectId: 'proj_abc', label: 'tracedecay' });
    });
  });

  it('writes a selection into the URL and clears it on all-projects', async () => {
    mount('/brain');
    act(() => useScope.getState().selectProject('proj_xyz', 'lynx'));
    await waitFor(() => {
      expect(currentSearch).toContain('scope=proj_xyz');
      expect(currentSearch).toContain('scopeLabel=lynx');
    });
    act(() => useScope.getState().selectAllProjects());
    await waitFor(() => expect(currentSearch).not.toContain('scope='));
  });

  it('defaults a scopeless URL to all-projects', async () => {
    useScope.setState({ scope: { kind: 'project', projectId: 'proj_old', label: 'old' } });
    mount('/knowledge');
    await waitFor(() => expect(useScope.getState().scope).toEqual({ kind: 'all' }));
  });
});
