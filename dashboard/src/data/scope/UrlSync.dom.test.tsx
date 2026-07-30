import { act, render, waitFor } from '@testing-library/react';
import { MemoryRouter, useSearchParams } from 'react-router';
import { beforeEach, describe, expect, it } from 'vitest';
import { ScopeUrlSync } from './UrlSync.tsx';
import { scopeWritable, useScope } from './store.ts';

let currentSearch = '';
let setSearch: (next: string) => void = () => {
  throw new Error('SearchProbe is not mounted');
};
function SearchProbe() {
  const [params, setParams] = useSearchParams();
  currentSearch = params.toString();
  setSearch = (next) => setParams(new URLSearchParams(next));
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

  it('applies a scope deep link to the store as an unresolved identity', async () => {
    mount('/costs?scope=proj_abc&scopeLabel=tracedecay');
    await waitFor(() => {
      const scope = useScope.getState().scope;
      expect(scope).toEqual({
        kind: 'project',
        projectId: 'proj_abc',
        label: 'tracedecay',
        // A URL says which project, never whether it is the active one. The
        // link must not be able to talk the dashboard into offering a write:
        // arriving `active` would enable controls the gateway then refuses.
        activation: 'unresolved',
      });
    });
    expect(scopeWritable(useScope.getState().scope).state).toBe('unknown');
  });

  it('leaves a resolved activation alone when an unrelated search param changes', async () => {
    // The URL->store effect runs on every search-param change, including params
    // that belong to a workspace. Reselecting on those would reset a project
    // the registry had already resolved back to `unresolved`, withdrawing a
    // legitimately enabled write control on an unrelated filter change.
    mount('/costs?scope=proj_abc&scopeLabel=tracedecay');
    await waitFor(() =>
      expect(useScope.getState().scope).toMatchObject({ projectId: 'proj_abc' }),
    );
    act(() =>
      useScope.getState().resolveActivation({ state: 'measured', activeProjectId: 'proj_abc' }),
    );
    expect(scopeWritable(useScope.getState().scope).state).toBe('writable');

    act(() => setSearch('scope=proj_abc&scopeLabel=tracedecay&window=7d'));
    await waitFor(() => expect(currentSearch).toContain('window=7d'));
    expect(useScope.getState().scope).toMatchObject({ activation: 'active' });
    expect(scopeWritable(useScope.getState().scope).state).toBe('writable');
  });

  it('returns to an unresolved identity when the link names a different project', async () => {
    mount('/costs?scope=proj_abc&scopeLabel=tracedecay');
    await waitFor(() =>
      expect(useScope.getState().scope).toMatchObject({ projectId: 'proj_abc' }),
    );
    act(() =>
      useScope.getState().resolveActivation({ state: 'measured', activeProjectId: 'proj_abc' }),
    );

    act(() => setSearch('scope=proj_other&scopeLabel=other'));
    await waitFor(() =>
      expect(useScope.getState().scope).toMatchObject({ projectId: 'proj_other' }),
    );
    // The previous project's resolved activation says nothing about this one.
    expect(useScope.getState().scope).toMatchObject({ activation: 'unresolved' });
    expect(scopeWritable(useScope.getState().scope).state).toBe('unknown');
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
    useScope.setState({
      scope: { kind: 'project', projectId: 'proj_old', label: 'old', activation: 'active' },
    });
    mount('/knowledge');
    await waitFor(() => expect(useScope.getState().scope).toEqual({ kind: 'all' }));
  });
});
