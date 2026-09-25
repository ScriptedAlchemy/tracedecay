import { describe, expect, it } from 'vitest';

import { kindColor, kindColorVars } from './kindColor.ts';

function parseOklch(color: string): { l: number; c: number; h: number } {
  const match = /^oklch\(([\d.]+) ([\d.]+) ([\d.]+)\)$/.exec(color);
  if (!match) throw new Error(`not an oklch colour: ${color}`);
  return { l: Number(match[1]), c: Number(match[2]), h: Number(match[3]) };
}

describe('kindColor', () => {
  it('resolves a named kind case-insensitively, one side per medium', () => {
    expect(kindColor('Function', false)).toBe('oklch(0.8 0.12 202)');
    expect(kindColor('FUNCTION', true)).toBe('oklch(0.49 0.135 205)');
  });

  it('hashes other names onto the same ramp, stably', () => {
    expect(kindColor('repository', false)).toBe('oklch(0.78 0.11 185)');
    expect(kindColor('worktree', false)).toBe('oklch(0.82 0.08 244)');
    expect(kindColor('project', true)).toBe('oklch(0.58 0.145 250)');
    expect(kindColor('codex', false)).toBe('oklch(0.9 0.045 222)');
  });

  it('never leaves the cool identity band for amber, violet, red or ready green', () => {
    const names = ['function', 'macro', 'src/viz', 'claude', 'run-7f3a', 'unknown', 'x', ''];
    for (let index = 0; index < 200; index += 1) names.push(`kind-${index}`);
    for (const name of names) {
      for (const light of [false, true]) {
        const { c, h } = parseOklch(kindColor(name, light));
        expect(h).toBeGreaterThanOrEqual(165);
        expect(h).toBeLessThanOrEqual(250);
        expect(c).toBeLessThan(0.15);
      }
    }
  });

  it('keeps the two ramps in step: the brightest dark slot is the darkest ink', () => {
    const kinds = ['function', 'method', 'struct', 'trait', 'module', 'enum', 'field', 'impl'];
    const byDark = [...kinds].sort(
      (a, b) => parseOklch(kindColor(b, false)).l - parseOklch(kindColor(a, false)).l,
    );
    const byInk = [...kinds].sort(
      (a, b) => parseOklch(kindColor(a, true)).l - parseOklch(kindColor(b, true)).l,
    );
    expect(byDark).toEqual(['trait', 'field', 'impl', 'function', 'struct', 'module', 'method', 'enum']);
    expect(byInk).toEqual(byDark);
  });

  it('hands DOM marks both sides as custom properties', () => {
    expect(kindColorVars('method')).toEqual({
      '--kind-dark': 'oklch(0.68 0.13 248)',
      '--kind-light': 'oklch(0.58 0.145 250)',
    });
  });
});
