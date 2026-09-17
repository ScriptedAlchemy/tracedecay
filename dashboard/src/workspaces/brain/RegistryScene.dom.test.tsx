import { act, fireEvent, render, screen } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ProjectRepoGroup } from '../../contracts/generated.ts';
import { ActivationField } from '../../viz/graph/activation.ts';
import type { RegistryRuntime, SceneView } from '../../viz/scene/registryRuntime.ts';
import { buildRegistryScene, type SceneBody } from '../../viz/scene/registrySceneModel.ts';
import { setMotionPreference } from '../../viz/trace/reducedMotion.ts';
import { composeRegistryField } from './field.ts';
import { RegistryScene } from './RegistryScene.tsx';

/**
 * The React half of the hybrid, with the Three.js half replaced by a recording
 * double. Every assertion here is about the CONTRACT between the two: what the
 * DOM host hands the runtime for a hover, a click, a keyboard focus and a
 * motion preference — and what it refuses to do (create activity, select a
 * hub, draw without a context).
 */

const runtime = vi.hoisted(() => ({
  instance: null as (RegistryRuntime & { calls: string[] }) | null,
  created: 0,
  webgl: true,
  picked: null as SceneBody | null,
  context: null as { onLost: () => void; onRestored: () => void } | null,
}));

vi.mock('../../viz/graph/renderer.ts', () => ({
  hasWebGl: () => runtime.webgl,
  watchWebGlContext: (_canvases: unknown, handlers: { onLost: () => void; onRestored: () => void }) => {
    runtime.context = handlers;
    return () => {};
  },
}));

vi.mock('../../viz/scene/registryRuntime.ts', () => ({
  createRegistryRuntime: (options: { container: HTMLElement; onView: (view: SceneView) => void }) => {
    const calls: string[] = [];
    // ~105 CSS px per column (room for both tick lines) with the whole mass
    // axis inside the 640×320 box, so every body's name can print.
    const camera = { cx: 2, cy: 1.6, scale: 0.0095 };
    const view: SceneView = { camera, viewport: { width: 640, height: 320 }, spread: 1, fit: camera };
    const canvas = document.createElement('canvas');
    options.container.appendChild(canvas);
    const instance: RegistryRuntime & { calls: string[] } = {
      calls,
      canvas,
      resize: () => {
        calls.push('resize');
        options.onView(view);
      },
      focus: (id) => void calls.push(`focus:${id}`),
      emphasize: (ids) => void calls.push(`emphasize:${ids ? [...ids].sort().join(',') : null}`),
      fit: () => void calls.push('fit'),
      zoomIn: () => void calls.push('zoomIn'),
      zoomOut: () => void calls.push('zoomOut'),
      zoomAt: () => void calls.push('zoomAt'),
      panBy: () => void calls.push('panBy'),
      view: () => view,
      pick: () => runtime.picked,
      wake: () => void calls.push('wake'),
      settle: () => void calls.push('settle'),
      retheme: () => void calls.push('retheme'),
      dispose: () => void calls.push('dispose'),
    };
    runtime.instance = instance;
    runtime.created += 1;
    return instance;
  },
}));

const NOW = 1_700_000_000;
const entry = (id: string, ageDays: number, artifacts: number) => ({
  project_id: id,
  label: id,
  project_root: `/${id}`,
  canonical_root: `/${id}`,
  kind: 'primary',
  store_count: 1,
  artifact_count: artifacts,
  alias_count: 0,
  branches: [],
  default_branch: null,
  last_seen_at: NOW - ageDays * 86_400,
});
const GROUPS: ProjectRepoGroup[] = [
  { label: 'shared', git_common_dir: '/shared/.git', branches: [], project_count: 2, projects: [entry('main', 0.1, 300), entry('wt', 2, 20)] },
  { label: 'lone', git_common_dir: '/lone/.git', branches: [], project_count: 1, projects: [entry('lone', 30, 8)] },
];
const MODEL = buildRegistryScene(composeRegistryField(GROUPS, NOW));

function mount(overrides: Partial<Parameters<typeof RegistryScene>[0]> = {}) {
  const activation = new ActivationField({ halfLifeMs: 4200 });
  const onInspect = vi.fn();
  const onSelect = vi.fn();
  const view = render(
    <RegistryScene
      model={MODEL}
      activation={activation}
      inspectedId={null}
      onInspect={onInspect}
      onSelect={onSelect}
      emphasis={null}
      ariaLabel="Registry field: test"
      fallbackDescription="the registry list remains available"
      caption={<p>caption</p>}
      detail={() => ['stores 1']}
      {...overrides}
    />,
  );
  return { ...view, activation, onInspect, onSelect };
}

describe('RegistryScene host contract', () => {
  beforeEach(() => {
    runtime.instance = null;
    runtime.created = 0;
    runtime.webgl = true;
    runtime.picked = null;
    runtime.context = null;
    Object.defineProperties(HTMLElement.prototype, {
      clientWidth: { configurable: true, get: () => 640 },
      clientHeight: { configurable: true, get: () => 320 },
    });
    Object.defineProperty(window, 'matchMedia', {
      configurable: true,
      value: vi.fn().mockReturnValue({ matches: false, addEventListener: () => {}, removeEventListener: () => {} }),
    });
    HTMLElement.prototype.setPointerCapture = () => {};
    HTMLElement.prototype.releasePointerCapture = () => {};
  });

  afterEach(() => {
    localStorage.removeItem('td.motion-preference');
  });

  it('states a missing WebGL context as a typed absence and names the fallback', () => {
    runtime.webgl = false;
    mount();
    const absence = screen.getByRole('status');
    expect(absence.getAttribute('data-state')).toBe('unavailable');
    expect(absence.textContent).toMatch(/no WebGL context/);
    expect(absence.textContent).toMatch(/registry list remains available/);
    expect(runtime.instance).toBeNull();
    expect(screen.queryByRole('img')).toBeNull();
  });

  it('builds the runtime once the container has a box and prints the axis and names from the same camera', () => {
    mount();
    expect(runtime.instance).not.toBeNull();
    expect(runtime.instance!.calls).toContain('resize');
    expect(screen.getByRole('img', { name: 'Registry field: test' })).toBeTruthy();
    // Column ticks and the hub identity are DOM text, never canvas-only.
    expect(screen.getByText('< 24H')).toBeTruthy();
    expect(screen.getByText(/today · 1/i)).toBeTruthy();
    expect(screen.getByText('repo:shared')).toBeTruthy();
    expect(screen.getByText('hub · massless')).toBeTruthy();
    // Controls and the minimap are siblings of the image, never children of a
    // `role="img"` (whose children are presentational to assistive tech).
    const image = screen.getByRole('img', { name: 'Registry field: test' });
    const controls = screen.getByRole('group', { name: 'Registry field camera controls' });
    expect(image.contains(controls)).toBe(false);
    expect(screen.getByLabelText('Registry field zoom').textContent).toBe('100%');
  });

  it('states a lost context as a typed absence and rebuilds when the browser restores it', () => {
    mount();
    expect(runtime.created).toBe(1);
    expect(runtime.context).not.toBeNull();
    act(() => runtime.context!.onLost());
    const absence = screen.getByRole('status');
    expect(absence.getAttribute('data-state')).toBe('unavailable');
    expect(absence.textContent).toMatch(/lost its WebGL context/);
    expect(absence.textContent).toMatch(/returns if the browser restores/);
    expect(runtime.instance!.calls).toContain('dispose');
    act(() => runtime.context!.onRestored());
    expect(screen.queryByText(/lost its WebGL context/)).toBeNull();
    expect(screen.getByRole('img', { name: 'Registry field: test' })).toBeTruthy();
    expect(runtime.created).toBe(2);
  });

  it('turns pointer movement into inspection only: no heat, no selection', () => {
    const { activation, onInspect, onSelect } = mount();
    const field = screen.getByRole('img', { name: 'Registry field: test' });
    runtime.picked = MODEL.byId.get('main')!;
    fireEvent.pointerMove(field, { clientX: 100, clientY: 100, buttons: 0 });
    expect(onInspect).toHaveBeenLastCalledWith('main');
    expect(runtime.instance!.calls).toContain('focus:main');
    expect(activation.warm).toBe(false);
    expect(onSelect).not.toHaveBeenCalled();
    // Moving within the same body is not a new inspection.
    fireEvent.pointerMove(field, { clientX: 102, clientY: 101, buttons: 0 });
    expect(onInspect).toHaveBeenCalledTimes(1);
    runtime.picked = null;
    fireEvent.pointerMove(field, { clientX: 300, clientY: 300, buttons: 0 });
    expect(onInspect).toHaveBeenLastCalledWith(null);
    expect(runtime.instance!.calls).toContain('focus:null');
    expect(activation.warm).toBe(false);
  });

  it('selects a project body on a primary click and never a repository hub or another button', () => {
    const { activation, onSelect } = mount();
    const field = screen.getByRole('img', { name: 'Registry field: test' });
    runtime.picked = MODEL.byId.get('repo:/shared/.git')!;
    fireEvent.pointerDown(field, { button: 0, clientX: 10, clientY: 10, pointerId: 1 });
    fireEvent.pointerUp(field, { button: 0, clientX: 10, clientY: 10, pointerId: 1 });
    expect(onSelect).not.toHaveBeenCalled();
    runtime.picked = MODEL.byId.get('wt')!;
    // A middle or secondary button is not a selection gesture.
    fireEvent.pointerDown(field, { button: 1, clientX: 10, clientY: 10, pointerId: 1 });
    fireEvent.pointerUp(field, { button: 1, clientX: 10, clientY: 10, pointerId: 1 });
    fireEvent.pointerDown(field, { button: 2, clientX: 10, clientY: 10, pointerId: 1 });
    fireEvent.pointerUp(field, { button: 2, clientX: 10, clientY: 10, pointerId: 1 });
    expect(onSelect).not.toHaveBeenCalled();
    fireEvent.pointerDown(field, { button: 0, clientX: 10, clientY: 10, pointerId: 1 });
    fireEvent.pointerUp(field, { button: 0, clientX: 10, clientY: 10, pointerId: 1 });
    expect(onSelect).toHaveBeenCalledWith('wt');
    expect(activation.warm).toBe(false);
  });

  it('un-dims the names when the pointer leaves, even while the inspector retains the project', () => {
    const { rerender, activation, onInspect, onSelect } = mount();
    const field = screen.getByRole('img', { name: 'Registry field: test' });
    runtime.picked = MODEL.byId.get('lone')!;
    fireEvent.pointerMove(field, { clientX: 100, clientY: 100, buttons: 0 });
    // The page retains the inspection (it swallows the null), as BrainPage does.
    rerender(
      <RegistryScene
        model={MODEL}
        activation={activation}
        inspectedId="lone"
        onInspect={onInspect}
        onSelect={onSelect}
        emphasis={null}
        ariaLabel="Registry field: test"
        fallbackDescription="the registry list remains available"
        caption={<p>caption</p>}
        detail={() => []}
      />,
    );
    const labelOf = (id: string) => screen.getByText(id).parentElement!;
    expect(labelOf('main').className).toMatch(/opacity-40/);
    fireEvent.pointerLeave(field);
    expect(runtime.instance!.calls).toContain('focus:null');
    expect(labelOf('main').className).not.toMatch(/opacity-40/);
  });

  it('treats a drag as a pan, not a click', () => {
    const { onSelect } = mount();
    const field = screen.getByRole('img', { name: 'Registry field: test' });
    runtime.picked = MODEL.byId.get('wt')!;
    fireEvent.pointerDown(field, { button: 0, clientX: 10, clientY: 10, pointerId: 1 });
    fireEvent.pointerMove(field, { clientX: 40, clientY: 30, buttons: 1 });
    fireEvent.pointerUp(field, { button: 0, clientX: 40, clientY: 30, pointerId: 1 });
    expect(runtime.instance!.calls).toContain('panBy');
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('routes keyboard inspection, repository emphasis and camera controls to the runtime', () => {
    const { rerender, activation, onInspect, onSelect } = mount();
    rerender(
      <RegistryScene
        model={MODEL}
        activation={activation}
        inspectedId="lone"
        onInspect={onInspect}
        onSelect={onSelect}
        emphasis={new Set(['main', 'wt', 'repo:/shared/.git'])}
        ariaLabel="Registry field: test"
        fallbackDescription="the registry list remains available"
        caption={<p>caption</p>}
        detail={() => []}
      />,
    );
    expect(runtime.instance!.calls).toContain('focus:lone');
    expect(runtime.instance!.calls).toContain('emphasize:main,repo:/shared/.git,wt');
    // Emphasis forces every member's name to print and the minimap to appear.
    expect(screen.getByText('main')).toBeTruthy();
    expect(screen.getByText('wt')).toBeTruthy();
    expect(screen.getByRole('img', { name: /minimap: 3 highlighted bodies/ })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Zoom in registry field' }));
    fireEvent.click(screen.getByRole('button', { name: 'Zoom out registry field' }));
    fireEvent.click(screen.getByRole('button', { name: 'Fit' }));
    expect(runtime.instance!.calls.slice(-3)).toEqual(['zoomIn', 'zoomOut', 'fit']);
    expect(activation.warm).toBe(false);
  });

  it('settles the scene the moment motion is turned off', () => {
    const { rerender, activation, onInspect, onSelect } = mount();
    expect(runtime.instance!.calls).not.toContain('settle');
    setMotionPreference('reduced');
    rerender(
      <RegistryScene
        model={MODEL}
        activation={activation}
        inspectedId={null}
        onInspect={onInspect}
        onSelect={onSelect}
        emphasis={null}
        ariaLabel="Registry field: test"
        fallbackDescription="the registry list remains available"
        caption={<p>caption</p>}
        detail={() => []}
      />,
    );
    expect(runtime.instance!.calls).toContain('settle');
  });

  it('disposes the runtime with the component', () => {
    const { unmount } = mount();
    unmount();
    expect(runtime.instance!.calls.at(-1)).toBe('dispose');
  });
});
