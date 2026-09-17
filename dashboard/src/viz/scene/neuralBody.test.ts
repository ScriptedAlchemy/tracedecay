import { describe, expect, it } from 'vitest';
import { buildNeuralBody, dustBudget, hashId, mulberry32 } from './neuralBody.ts';

describe('neural body geometry', () => {
  it('is deterministic for the same identity and radius', () => {
    const a = buildNeuralBody({ id: 'proj_alpha', radius: 0.2, depth: 0 });
    const b = buildNeuralBody({ id: 'proj_alpha', radius: 0.2, depth: 0 });
    expect(a.count).toBe(b.count);
    expect(Array.from(a.positions)).toEqual(Array.from(b.positions));
    expect(Array.from(a.filaments)).toEqual(Array.from(b.filaments));
    expect(Array.from(a.glows)).toEqual(Array.from(b.glows));
  });

  it('draws a different body for a different identity', () => {
    const a = buildNeuralBody({ id: 'proj_alpha', radius: 0.2, depth: 0 });
    const b = buildNeuralBody({ id: 'proj_beta', radius: 0.2, depth: 0 });
    expect(Array.from(a.positions)).not.toEqual(Array.from(b.positions));
  });

  it('fills exactly its dust budget and keeps every particle inside the body envelope', () => {
    const radius = 0.24;
    const body = buildNeuralBody({ id: 'proj_gamma', radius, depth: 0 });
    expect(body.count).toBe(dustBudget(radius));
    expect(body.positions).toHaveLength(body.count * 3);
    expect(body.alphas).toHaveLength(body.count);
    expect(body.sizes).toHaveLength(body.count);
    // Crown above, filaments below: nothing wanders past a couple of radii.
    const envelope = radius * 2.6;
    for (let index = 0; index < body.count; index += 1) {
      const x = body.positions[index * 3]!;
      const y = body.positions[index * 3 + 1]!;
      expect(Math.abs(x)).toBeLessThanOrEqual(envelope);
      expect(y).toBeLessThanOrEqual(radius * 1.75);
      expect(y).toBeGreaterThanOrEqual(-body.depthReach - radius * 0.6);
      expect(body.alphas[index]).toBeGreaterThan(0);
      expect(body.alphas[index]).toBeLessThanOrEqual(1);
      expect(body.sizes[index]).toBeGreaterThan(0);
    }
    expect(body.depthReach).toBeGreaterThan(radius * 0.8);
    expect(body.depthReach).toBeLessThan(radius * 1.4);
  });

  it('carries a filament arbor and emission anchors that scale with the crown', () => {
    const small = buildNeuralBody({ id: 'proj_delta', radius: 0.1, depth: 0 });
    const large = buildNeuralBody({ id: 'proj_delta', radius: 0.3, depth: 0 });
    expect(small.filamentSegments).toBeGreaterThan(0);
    expect(small.filaments).toHaveLength(small.filamentSegments * 4);
    expect(small.glowCount).toBeGreaterThan(3);
    expect(large.count).toBeGreaterThan(small.count);
    // Same seed, same arbor shape: only the scale differs.
    expect(large.filamentSegments).toBe(small.filamentSegments);
  });

  it('thins and softens a body one depth layer back without moving it', () => {
    const front = buildNeuralBody({ id: 'proj_epsilon', radius: 0.2, depth: 0 });
    const back = buildNeuralBody({ id: 'proj_epsilon', radius: 0.2, depth: 2 });
    expect(Array.from(front.positions)).toEqual(Array.from(back.positions));
    const mean = (values: Float32Array) => values.reduce((sum, value) => sum + value, 0) / values.length;
    expect(mean(back.alphas)).toBeLessThan(mean(front.alphas));
    expect(mean(back.sizes)).toBeGreaterThan(mean(front.sizes));
  });

  it('seeds from the identity alone', () => {
    expect(hashId('a')).not.toBe(hashId('b'));
    const random = mulberry32(hashId('stable'));
    const first = [random(), random(), random()];
    const again = mulberry32(hashId('stable'));
    expect([again(), again(), again()]).toEqual(first);
    for (const value of first) {
      expect(value).toBeGreaterThanOrEqual(0);
      expect(value).toBeLessThan(1);
    }
  });
});
