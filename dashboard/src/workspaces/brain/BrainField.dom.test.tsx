import { fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ProjectRegistryEntry, ProjectRepoGroup } from '../../contracts/generated.ts';
import { BrainField, FieldLegend } from './BrainField.tsx';
import { composeRegistryField } from './field.ts';
import { buildRegistryScene, fieldVariantFromLocation } from './fieldVariant.ts';

const NOW = Date.now() / 1000;

function project(id: string, ageDays: number, artifacts: number): ProjectRegistryEntry {
  return {
    project_id: id,
    label: id,
    project_root: `/repos/${id}`,
    canonical_root: `/repos/${id}`,
    kind: 'primary',
    default_branch: 'main',
    branches: ['main'],
    store_count: 1,
    artifact_count: artifacts,
    alias_count: 1,
    last_seen_at: NOW - ageDays * 86_400,
  };
}

const GROUPS: ProjectRepoGroup[] = [
  { label: 'core', git_common_dir: '/repos/core/.git', project_count: 2, branches: ['main'], projects: [project('core', 0.1, 9), project('core-wt', 3, 4)] },
  { label: 'notes', git_common_dir: '/repos/notes/.git', project_count: 1, branches: ['main'], projects: [project('notes', 40, 1)] },
];
const SCENE = buildRegistryScene(composeRegistryField(GROUPS, NOW), GROUPS, NOW);

/** A 2D context that accepts every call, so the canvas variants construct
 * under jsdom; what they draw is covered by the capture review, not here. */
function stubCanvas(): void {
  const context = new Proxy(
    {},
    {
      get: (_, name) =>
        name === 'measureText'
          ? () => ({ width: 10 })
          : name === 'createRadialGradient'
            ? () => ({ addColorStop: () => {} })
            : name === 'getImageData'
              ? () => ({ data: [80, 90, 100, 255] })
              : () => {},
      set: () => true,
    },
  );
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockImplementation(
    (kind: string) => (kind === '2d' ? context : null) as never,
  );
  vi.stubGlobal('requestAnimationFrame', () => 1);
  vi.stubGlobal('cancelAnimationFrame', () => {});
}

function renderField(variant: 'points' | 'atlas' | 'sigma', onInspect = vi.fn(), onSelect = vi.fn(), inspectedId: string | null = null) {
  return render(
    <BrainField
      variant={variant}
      scene={SCENE}
      inspectedId={inspectedId}
      onInspect={onInspect}
      onSelect={onSelect}
      focus={null}
      activity
      ariaLabel="Registry field: 3 projects"
      legend={<FieldLegend variant={variant} scene={SCENE} />}
    />,
  );
}

describe('BrainField', () => {
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
  });

  it('states a renderer that cannot start instead of drawing an empty field', () => {
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
    renderField('points');
    expect(screen.getByRole('status').textContent).toContain(
      'the points field could not draw (This browser has no 2D canvas context.)',
    );
    expect(screen.queryByRole('group', { name: 'Registry field: 3 projects' })).toBeNull();
  });

  it('refuses the Sigma variant without WebGL rather than falling back silently', () => {
    stubCanvas();
    renderField('sigma');
    expect(screen.getByRole('status').textContent).toContain('This browser has no WebGL context.');
  });

  it('walks projects with arrow keys, speaks the reading, and selects with Enter', () => {
    stubCanvas();
    const onInspect = vi.fn();
    const onSelect = vi.fn();
    const view = renderField('points', onInspect, onSelect);
    const field = screen.getByRole('group', { name: 'Registry field: 3 projects' });
    fireEvent.keyDown(field, { key: 'ArrowRight' });
    expect(onInspect).toHaveBeenLastCalledWith('core');
    view.rerender(
      <BrainField variant="points" scene={SCENE} inspectedId="core" onInspect={onInspect} onSelect={onSelect} focus={null} activity ariaLabel="Registry field: 3 projects" legend={null} />,
    );
    expect(screen.getByText(/^core: stores 1, artifacts 9, mass 10/)).toBeTruthy();
    fireEvent.keyDown(field, { key: 'ArrowRight' });
    expect(onInspect).toHaveBeenLastCalledWith('core-wt');
    fireEvent.keyDown(field, { key: 'Enter' });
    expect(onSelect).toHaveBeenCalledWith('core');
    fireEvent.keyDown(field, { key: 'Escape' });
    expect(onInspect).toHaveBeenLastCalledWith(null);
  });

  it('keeps the list-only Brain unless the URL names a field variant', () => {
    window.history.replaceState({}, '', '/brain');
    expect(fieldVariantFromLocation()).toBeNull();
    window.history.replaceState({}, '', '/brain?scope=core&field=atlas');
    expect(fieldVariantFromLocation()).toBe('atlas');
    window.history.replaceState({}, '', '/brain?field=webgpu');
    expect(fieldVariantFromLocation()).toBeNull();
    window.history.replaceState({}, '', '/');
  });

  it('names amber as admitted activity and cyan as inspection in every variant', () => {
    render(<FieldLegend variant="atlas" scene={SCENE} />);
    expect(screen.getByText('admitted activity on the exact touched project and one drawn hop, 4.2 s half-life')).toBeTruthy();
    expect(screen.getByText('inspection and focus, never activity')).toBeTruthy();
    expect(screen.getByText('one project, ordered by canonical id inside its recency column')).toBeTruthy();
  });
});
