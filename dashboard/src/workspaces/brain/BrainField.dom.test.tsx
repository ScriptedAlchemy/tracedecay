import { fireEvent, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import type { ProjectRegistryEntry, ProjectRepoGroup } from '../../contracts/generated.ts';
import { BrainField, FieldLegend } from './BrainField.tsx';
import { composeRegistryField } from './field.ts';
import { buildGraphScene, buildRegistryScene } from './registryScene.ts';
import { registryAtlas } from '../../viz/graph/fieldRenderers/atlas.ts';

const NOW = Date.now() / 1000;

function project(id: string, ageDays: number, artifacts: number): ProjectRegistryEntry {
  return {
    project_id: id,
    label: id,
    project_root: `/repos/${id}`,
    canonical_root: `/repos/${id}`,
    kind: 'primary',
    default_branch: 'main',
    head_branch: 'main',
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

function renderField(onInspect = vi.fn(), onSelect = vi.fn(), inspectedId: string | null = null) {
  return render(
    <BrainField
      scene={SCENE}
      inspectedId={inspectedId}
      onInspect={onInspect}
      onSelect={onSelect}
      focus={null}
      activity
      ariaLabel="Registry field: 3 projects"
      legend={<FieldLegend scene={SCENE} />}
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
    renderField();
    expect(screen.getByText('The registry field could not draw')).toBeTruthy();
    expect(screen.getByText(/This browser has no 2D canvas context\. The project registry beside it lists the same projects\./)).toBeTruthy();
    expect(screen.queryByRole('group', { name: 'Registry field: 3 projects' })).toBeNull();
  });

  it('recovers by switching views after a canvas construction failure', () => {
    vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(null);
    const view = renderField();
    expect(screen.getByText('The registry field could not draw')).toBeTruthy();
    stubCanvas();
    view.rerender(<BrainField variant="atlas" scene={SCENE} inspectedId="core" onInspect={vi.fn()} onSelect={vi.fn()} focus={null} activity ariaLabel="Registry field: 3 projects" legend={null} />);
    expect(screen.queryByText('The registry field could not draw')).toBeNull();
    expect(screen.getByRole('group', { name: 'Registry field: 3 projects' }).querySelectorAll('canvas')).toHaveLength(1);
  });

  it('keeps Atlas projects selectable while their repository is in focus', () => {
    stubCanvas();
    vi.spyOn(Element.prototype, 'clientWidth', 'get').mockReturnValue(800);
    vi.spyOn(Element.prototype, 'clientHeight', 'get').mockReturnValue(600);
    const onInspect = vi.fn();
    const onSelect = vi.fn();
    const view = render(<BrainField variant="atlas" scene={SCENE} inspectedId={null} onInspect={onInspect} onSelect={onSelect} focus={new Set(['core', 'core-wt'])} activity ariaLabel="Registry field: 3 projects" legend={null} />);
    const canvas = view.container.querySelector('canvas')!;
    const plate = registryAtlas(SCENE, 800, 600).plates.find((entry) => entry.id === 'core')!;
    fireEvent.pointerMove(canvas, { clientX: plate.x + plate.w / 2, clientY: plate.y + 15 });
    expect(onInspect).toHaveBeenLastCalledWith('core');
    fireEvent.click(canvas, { clientX: plate.x + plate.w / 2, clientY: plate.y + 15 });
    expect(onSelect).toHaveBeenLastCalledWith('core');
    fireEvent.pointerLeave(canvas);
    expect(onInspect).toHaveBeenLastCalledWith(null);
  });

  it('walks projects with arrow keys, speaks the reading, and selects with Enter', () => {
    stubCanvas();
    const onInspect = vi.fn();
    const onSelect = vi.fn();
    const view = renderField(onInspect, onSelect);
    const field = screen.getByRole('group', { name: 'Registry field: 3 projects' });
    fireEvent.keyDown(field, { key: 'ArrowRight' });
    expect(onInspect).toHaveBeenLastCalledWith('core');
    view.rerender(
      <BrainField scene={SCENE} inspectedId="core" onInspect={onInspect} onSelect={onSelect} focus={null} activity ariaLabel="Registry field: 3 projects" legend={null} />,
    );
    expect(screen.getByText(/^core: stores 1, artifacts 9, mass 10/)).toBeTruthy();
    fireEvent.keyDown(field, { key: 'ArrowRight' });
    expect(onInspect).toHaveBeenLastCalledWith('core-wt');
    fireEvent.keyDown(field, { key: 'Enter' });
    expect(onSelect).toHaveBeenCalledWith('core');
    fireEvent.keyDown(field, { key: 'Escape' });
    expect(onInspect).toHaveBeenLastCalledWith(null);
  });

  it('claims amber activity only on the registry field and a cluster frame only for a crowded cell', () => {
    const amber = () => screen.getByText('amber').nextElementSibling?.textContent;
    const frame = () => screen.queryByText(/crowded recency × mass cell/);
    const view = render(<FieldLegend scene={SCENE} />);
    expect(amber()).toBe('admitted activity on the exact touched project and one drawn hop, 4.2 s half-life');
    expect(frame()).toBeNull();

    const crowd: ProjectRepoGroup[] = [
      {
        label: 'crowd',
        git_common_dir: '/repos/crowd/.git',
        project_count: 12,
        branches: ['main'],
        projects: Array.from({ length: 12 }, (_, index) => project(`c${String(index).padStart(2, '0')}`, 0.2, 1)),
      },
    ];
    view.rerender(<FieldLegend scene={buildRegistryScene(composeRegistryField(crowd, NOW), crowd, NOW)} />);
    expect(frame()?.textContent).toBe('a crowded recency × mass cell with its exact count; zoom or click to resolve');

    view.rerender(<FieldLegend scene={buildGraphScene([{ id: 'a', label: 'a', kind: 'function', degree: 1, x: 0, y: 0 }], [])} />);
    expect(amber()).toBe('none: no symbol-level activity is supplied, so nothing here blooms');
  });
});
