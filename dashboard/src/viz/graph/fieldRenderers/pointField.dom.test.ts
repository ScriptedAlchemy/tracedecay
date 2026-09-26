import { fireEvent } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';
import { ActivationField } from '../activation.ts';
import { createPointField } from './pointField.ts';
import { fitCamera, sampleFieldPalette, toScreen, type FieldScene, type SceneBody } from './scene.ts';

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

function mount(onHover: (id: string | null) => void): HTMLCanvasElement {
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
  createPointField({
    container,
    scene: SCENE,
    field: new ActivationField(),
    palette: sampleFieldPalette(container),
    isReduced: () => true,
    onHover,
    onSelect: () => {},
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
});
