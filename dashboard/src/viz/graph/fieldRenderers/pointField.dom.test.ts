import { fireEvent } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ActivationField } from '../activation.ts';
import { createLuminousField, createNeuralField, createPointField } from './pointField.ts';
import { buildNeuralBody } from './neuralBody.ts';
import { fitCamera, sampleFieldPalette, toScreen, type FieldRendererFactory, type FieldScene, type SceneBody } from './scene.ts';

const WIDTH = 800;
const HEIGHT = 600;

function body(id: string, x: number, y: number): SceneBody {
  return {
    id,
    label: id,
    role: 'body',
    kind: 'primary',
    x,
    y,
    radius: 0.1,
    mass: 10,
    units: { stores: 3, artifacts: 7 },
    vitality: 1,
    detail: [],
    group: null,
    cluster: null,
  };
}

const SCENE: FieldScene = {
  bodies: [body('a', 0, 1), body('b', 3, 2)],
  paths: [],
  clusters: [],
  extent: { x: [-0.5, 4.5], y: [0, 3] },
  columns: null,
  neighbors: new Map(),
};

function mount(onHover: (id: string | null) => void, create: FieldRendererFactory = createPointField, onSelect = vi.fn(), scene = SCENE): HTMLCanvasElement {
  const context = new Proxy(
    {},
    {
      get: (_, name) =>
        name === 'createRadialGradient'
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
  const container = document.createElement('div');
  Object.defineProperty(container, 'clientWidth', { value: WIDTH });
  Object.defineProperty(container, 'clientHeight', { value: HEIGHT });
  document.body.appendChild(container);
  create({
    container,
    scene,
    field: new ActivationField(),
    palette: sampleFieldPalette(container),
    isReduced: () => true,
    onHover,
    onSelect,
  });
  return container.querySelector('canvas')!;
}

const at = (x: number, y: number) =>
  toScreen(fitCamera(SCENE.extent, WIDTH, HEIGHT, { top: 24, right: 24, bottom: 24, left: 24 }), x, y);

describe('point field hover', () => {
  afterEach(() => {
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
    document.body.innerHTML = '';
  });

  it('ends inspection when the pointer moves off a body onto empty canvas', () => {
    const onHover = vi.fn();
    const canvas = mount(onHover);
    const [ax, ay] = at(0, 1);
    fireEvent.pointerMove(canvas, { clientX: ax, clientY: ay });
    expect(onHover).toHaveBeenLastCalledWith('a');
    const [ex, ey] = at(1.5, 0.2);
    fireEvent.pointerMove(canvas, { clientX: ex, clientY: ey });
    expect(onHover).toHaveBeenLastCalledWith(null);
    expect(onHover).toHaveBeenCalledTimes(2);
  });

  it('ends inspection when the pointer leaves the field from a body', () => {
    const onHover = vi.fn();
    const canvas = mount(onHover);
    const [bx, by] = at(3, 2);
    fireEvent.pointerMove(canvas, { clientX: bx, clientY: by });
    expect(onHover).toHaveBeenLastCalledWith('b');
    fireEvent.pointerLeave(canvas);
    expect(onHover).toHaveBeenLastCalledWith(null);
  });

  it('clears only the inspection it made', () => {
    const onHover = vi.fn();
    const canvas = mount(onHover);
    const [ax, ay] = at(0, 1);
    const [ex, ey] = at(1.5, 0.2);
    fireEvent.pointerMove(canvas, { clientX: ax, clientY: ay });
    fireEvent.pointerLeave(canvas);
    fireEvent.pointerMove(canvas, { clientX: ex, clientY: ey });
    fireEvent.pointerLeave(canvas);
    expect(onHover.mock.calls).toEqual([['a'], [null]]);
  });

  it.each([['luminous', createLuminousField], ['neural', createNeuralField]] as const)('picks and clears the same exact bodies in the restored %s view', (_, create) => {
    const onHover = vi.fn();
    const onSelect = vi.fn();
    const canvas = mount(onHover, create, onSelect);
    const [ax, ay] = at(0, 1);
    fireEvent.pointerMove(canvas, { clientX: ax, clientY: ay });
    expect(onHover).toHaveBeenLastCalledWith('a');
    fireEvent.click(canvas, { clientX: ax, clientY: ay });
    expect(onSelect).toHaveBeenLastCalledWith('a');
    fireEvent.pointerLeave(canvas);
    expect(onHover).toHaveBeenLastCalledWith(null);
  });

  it('picks the visible Neural root beyond the measured disc and clears outside its envelope', () => {
    const core = { ...body('core', 0, 0), radius: 0.2 };
    const scene: FieldScene = { ...SCENE, bodies: [core], extent: { x: [-1, 1], y: [-1, 1] } };
    const onHover = vi.fn();
    const onSelect = vi.fn();
    const canvas = mount(onHover, createNeuralField, onSelect, scene);
    const camera = fitCamera(scene.extent, WIDTH, HEIGHT, { top: 24, right: 24, bottom: 24, left: 24 });
    const geometry = buildNeuralBody({ id: core.id, radius: core.radius, depth: 0 });
    const [x, rootY] = toScreen(camera, core.x, core.y - geometry.depthReach);
    // The prominent basal knot falls outside the old disc-only hit target.
    expect(geometry.depthReach * camera.scale).toBeGreaterThan(core.radius * camera.scale + 5);
    fireEvent.pointerMove(canvas, { clientX: x, clientY: rootY });
    expect(onHover).toHaveBeenLastCalledWith('core');
    fireEvent.click(canvas, { clientX: x, clientY: rootY });
    expect(onSelect).toHaveBeenCalledExactlyOnceWith('core');

    const [emptyX, emptyY] = toScreen(camera, core.x, core.y - core.radius * 3);
    fireEvent.pointerMove(canvas, { clientX: emptyX, clientY: emptyY });
    expect(onHover).toHaveBeenLastCalledWith(null);
    fireEvent.click(canvas, { clientX: emptyX, clientY: emptyY });
    expect(onSelect).toHaveBeenCalledTimes(1);
  });
});
