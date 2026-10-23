import { describe, expect, it } from 'vitest';

import type { GraphEdgeV1, GraphNodeV1 } from '../../contracts/generated.ts';
import { luminousPainter, type LuminousLayout } from './cortexLuminous.ts';
import { plateLayout, platePainter } from './cortexPlate.ts';
import { fitCamera, sceneFromSlice, type PaintFrame } from './cortexScene.ts';

interface TextMark { text: string; x: number; y: number; width: number }

function recordingFrame(width: number, height: number): { frame: PaintFrame; text: TextMark[] } {
  const text: TextMark[] = [];
  const target: Record<string, unknown> = {
    measureText: (value: string) => ({ width: value.length * 6 }),
    fillText: (value: string, x: number, y: number) => text.push({ text: value, x, y, width: value.length * 6 }),
  };
  const ctx = new Proxy(target, {
    get: (object, key: string) => key in object ? object[key] : () => {},
  }) as unknown as CanvasRenderingContext2D;
  return {
    text,
    frame: {
      ctx, width, height, camera: { k: 1, tx: 0, ty: 0 }, fitK: 1,
      palette: { substrate: '#000', dim: '#333', text: '#fff', muted: '#888', edge: '#666', accent: '#0ff', unknown: '#888', danger: '#f00' },
      hovered: null, selected: null, cursor: null, emphasis: null, heat: () => 0,
    },
  };
}

const scene = sceneFromSlice(Array.from({ length: 6 }, (_, index) => ({
  id: `n${index}`, name: `symbol_${index}`, kind: 'function', degree: 20 - index,
} as GraphNodeV1)), []);
const positions = new Map([
  ['n0', { x: 310, y: 60 }],
  ['n1', { x: 165, y: 110 }],
  ['n2', { x: 165, y: 90 }],
  ['n3', { x: 165, y: 130 }],
  ['n4', { x: 215, y: 110 }],
  ['n5', { x: 115, y: 110 }],
]);
const layout: LuminousLayout = { positions, bounds: { x0: 0, y0: 0, x1: 320, y1: 220 } };

describe('Cortex annotations on narrow canvases', () => {
  it('keeps luminous labels inside the viewport and clear of other symbol bodies', () => {
    const { frame, text } = recordingFrame(320, 220);
    luminousPainter.draw(layout, scene, frame);
    expect(text.length).toBeGreaterThan(0);
    expect(text.length).toBeLessThanOrEqual(2);
    for (const label of text) {
      expect(label.x).toBeGreaterThanOrEqual(0);
      expect(label.x + label.width).toBeLessThanOrEqual(frame.width);
      expect(label.y - 7).toBeGreaterThanOrEqual(0);
      expect(label.y + 7).toBeLessThanOrEqual(frame.height);
      for (const node of scene.nodes) {
        if (node.label === label.text) continue;
        const point = positions.get(node.id)!;
        const radius = luminousPainter.hitRadius(layout, scene, node.id, frame);
        const covers = label.x < point.x + radius && label.x + label.width > point.x - radius &&
          label.y - 7 < point.y + radius && label.y + 7 > point.y - radius;
        expect(covers, `${label.text} covers ${node.id}`).toBe(false);
      }
    }
  });

  it('prioritizes an inspected symbol outside the ambient mobile label budget', () => {
    const { frame, text } = recordingFrame(320, 220);
    luminousPainter.draw(layout, scene, { ...frame, hovered: 'n5' });
    expect(text.some((label) => label.text === 'symbol_5')).toBe(true);
    expect([...layout.positions.keys()]).toEqual(scene.nodes.map((node) => node.id));
  });

  it('does not overprint count lines in tightly fitted Plate depth bands', () => {
    const chain = sceneFromSlice(
      ['a', 'b', 'c'].map((id) => ({ id, name: id, kind: 'function', file_path: 'src/a.rs', degree: 1 } as GraphNodeV1)),
      [{ source: 'a', target: 'b', kind: 'calls' }, { source: 'b', target: 'c', kind: 'calls' }] as GraphEdgeV1[],
    );
    const plate = plateLayout(chain, { width: 800, height: 160 }, 'pending');
    const painter = platePainter('pending');
    const { frame, text } = recordingFrame(320, 160);
    const camera = fitCamera(painter.bounds(plate), frame.width, frame.height, 0);
    painter.draw(plate, chain, { ...frame, camera, fitK: camera.k });
    const depths = text.filter((label) => /^D\d+$/.test(label.text));
    expect(depths.map((label) => label.text)).toEqual(['D2', 'D1', 'D0']);
    expect(text.some((label) => /\d+ sym$/.test(label.text))).toBe(false);
    for (let i = 1; i < depths.length; i += 1) expect(depths[i]!.y - depths[i - 1]!.y).toBeGreaterThanOrEqual(14);
    expect(plate.positions.size).toBe(3);
  });
});
