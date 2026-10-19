/**
 * The canvas hands its painter an abort signal and lets only the newest layout
 * request write: a superseded result is dropped, an unmount aborts the request
 * and the selection a reader holds is untouched by a layout completing.
 */
import { act, render, screen, waitFor } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { CortexSceneCanvas } from './CortexSceneCanvas.tsx';
import { sceneFromSlice, type CortexPainter } from './cortexScene.ts';
import type { GraphNodeV1 } from '../../contracts/generated.ts';

interface FakeLayout {
  readonly tag: string;
}

interface Request {
  scene: unknown;
  signal: AbortSignal | undefined;
  resolve: (layout: FakeLayout) => void;
}

function fakePainter() {
  const requests: Request[] = [];
  const painter: CortexPainter<FakeLayout> = {
    name: 'fake',
    fitPad: 0,
    layout: (scene, _box, signal) =>
      new Promise<FakeLayout>((resolve) => requests.push({ scene, signal, resolve })),
    bounds: () => ({ x0: 0, y0: 0, x1: 100, y1: 100 }),
    position: () => ({ x: 50, y: 50 }),
    hitRadius: () => 5,
    draw: vi.fn(),
  };
  return { painter, requests };
}

function sceneOf(...ids: string[]) {
  return sceneFromSlice(
    ids.map((id) => ({ id, kind: 'function', name: id, file_path: 'src/x.rs', degree: 1 }) as GraphNodeV1),
    [],
  );
}

function renderCanvas(
  props: { scene: ReturnType<typeof sceneOf>; painter: CortexPainter<FakeLayout>; selectedId?: string | null },
) {
  const noop = () => {};
  return (
    <CortexSceneCanvas
      scene={props.scene}
      painter={props.painter}
      selectedId={props.selectedId ?? null}
      inspectedId={null}
      onSelect={noop}
      onInspect={noop}
      ariaLabel="test field"
    />
  );
}

afterEach(() => {
  vi.restoreAllMocks();
});

function mockCanvas() {
  const context = new Proxy({}, { get: () => () => {}, set: () => true });
  vi.spyOn(HTMLCanvasElement.prototype, 'getContext').mockReturnValue(
    context as unknown as RenderingContext,
  );
  vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockReturnValue({
    x: 0, y: 0, left: 0, top: 0, width: 800, height: 400, right: 800, bottom: 400, toJSON: () => ({}),
  } as DOMRect);
}

describe('layout requests from the canvas', () => {
  it('drops a superseded layout that finishes after its replacement', async () => {
    mockCanvas();
    const { painter, requests } = fakePainter();
    const first = sceneOf('a');
    const second = sceneOf('a', 'b');
    const view = render(renderCanvas({ scene: first, painter }));
    await waitFor(() => expect(requests).toHaveLength(1));
    view.rerender(renderCanvas({ scene: second, painter }));
    await waitFor(() => expect(requests).toHaveLength(2));
    expect(requests[0]!.signal!.aborted).toBe(true);
    expect(requests[1]!.signal!.aborted).toBe(false);

    requests[1]!.resolve({ tag: 'second' });
    await waitFor(() =>
      expect(screen.getByRole('group', { name: /test field/ }).getAttribute('data-layout')).toBe('ready'),
    );
    await waitFor(() => expect(painter.draw).toHaveBeenCalled());
    await act(async () => {
      requests[0]!.resolve({ tag: 'first' });
    });

    const drawn = vi.mocked(painter.draw).mock.calls.map((call) => call[0].tag);
    expect(new Set(drawn)).toEqual(new Set(['second']));
  });

  it('aborts the pending request on unmount', async () => {
    mockCanvas();
    const { painter, requests } = fakePainter();
    const view = render(renderCanvas({ scene: sceneOf('a'), painter }));
    await waitFor(() => expect(requests).toHaveLength(1));
    expect(requests[0]!.signal!.aborted).toBe(false);
    view.unmount();
    expect(requests[0]!.signal!.aborted).toBe(true);
    await act(async () => {
      requests[0]!.resolve({ tag: 'late' });
    });
    expect(painter.draw).not.toHaveBeenCalled();
  });

  it('keeps the pinned symbol, and the list notice, while the layout is pending and after it lands', async () => {
    mockCanvas();
    const { painter, requests } = fakePainter();
    const scene = sceneOf('a', 'b');
    const view = render(renderCanvas({ scene, painter, selectedId: 'b' }));
    await waitFor(() => expect(requests).toHaveLength(1));
    expect(screen.getByRole('status').textContent).toMatch(/symbol list remains available/);
    expect(painter.draw).not.toHaveBeenCalled();

    requests[0]!.resolve({ tag: 'only' });
    await waitFor(() => expect(painter.draw).toHaveBeenCalled());
    const [layout, drawnScene, frame] = vi.mocked(painter.draw).mock.calls.at(-1)!;
    expect(layout.tag).toBe('only');
    expect(drawnScene).toBe(scene);
    expect(frame.selected).toBe('b');
    expect([...drawnScene.byId.keys()]).toEqual(['a', 'b']);
    expect(requests).toHaveLength(1);

    view.rerender(renderCanvas({ scene, painter, selectedId: 'a' }));
    await waitFor(() => expect(vi.mocked(painter.draw).mock.calls.at(-1)![2].selected).toBe('a'));
    expect(requests).toHaveLength(1);
  });
  it('never mixes a queued old painter with a new scene and layout', async () => {
    mockCanvas();
    const frames = new Map<number, FrameRequestCallback>();
    let nextFrame = 0;
    vi.spyOn(window, 'requestAnimationFrame').mockImplementation((callback) => {
      const id = ++nextFrame;
      frames.set(id, callback);
      return id;
    });
    vi.spyOn(window, 'cancelAnimationFrame').mockImplementation((id) => {
      frames.delete(id);
    });

    const first = fakePainter();
    const second = fakePainter();
    const firstScene = sceneOf('first');
    const secondScene = sceneOf('second');
    const view = render(renderCanvas({ scene: firstScene, painter: first.painter }));
    await waitFor(() => expect(first.requests).toHaveLength(1));
    await act(async () => {
      first.requests[0]!.resolve({ tag: 'first' });
    });
    await waitFor(() => expect(frames.size).toBeGreaterThan(0));
    const deferredOldFrame = [...frames.values()][0]!;

    view.rerender(renderCanvas({ scene: secondScene, painter: second.painter }));
    await waitFor(() => expect(second.requests).toHaveLength(1));
    await act(async () => {
      second.requests[0]!.resolve({ tag: 'second' });
    });
    expect(screen.getByRole('group', { name: /test field/ }).getAttribute('data-layout')).toBe('ready');

    // Simulate a previously queued frame firing after scene B committed.
    // The old frame may dispatch B or no-op, but must never draw A with B's layout.
    act(() => deferredOldFrame(performance.now()));
    expect(first.painter.draw).not.toHaveBeenCalled();
    for (const [, frame] of [...frames]) {
      act(() => frame(performance.now()));
    }
    expect(second.painter.draw).toHaveBeenCalled();
    for (const [layout, scene] of vi.mocked(second.painter.draw).mock.calls) {
      expect(layout.tag).toBe('second');
      expect(scene).toBe(secondScene);
    }
    view.unmount();
  });

});
