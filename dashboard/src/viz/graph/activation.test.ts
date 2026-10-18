import { describe, expect, it, vi } from 'vitest';
import { ActivationField } from './activation.ts';

describe('ActivationField subscription', () => {
  it('notifies on a strike that lands heat, and stays silent on one that carries no ids', () => {
    const field = new ActivationField();
    const listener = vi.fn();
    field.subscribe(listener);
    field.strike([], 1);
    expect(listener).toHaveBeenCalledTimes(0);
    expect(field.warm).toBe(false);

    field.strike(['a'], 0.5);
    expect(listener).toHaveBeenCalledTimes(1);
    expect(field.warm).toBe(true);
    expect(field.heatOf('a')).toBe(0.5);
  });

  it('never fires on its own: decay is not an event', () => {
    // The field has no clock. `tick` is decay bookkeeping driven by whoever is
    // already drawing; if it notified, a renderer would wake itself forever.
    const field = new ActivationField({ halfLifeMs: 100 });
    const listener = vi.fn();
    field.subscribe(listener);
    field.strike(['a'], 1);
    expect(listener).toHaveBeenCalledTimes(1);
    expect(field.tick(1_000)).toBe(true);
    expect(field.tick(2_000)).toBe(false);
    expect(listener).toHaveBeenCalledTimes(1);
    expect(field.warm).toBe(false);
  });

  it('stops notifying once unsubscribed, while strikes still land', () => {
    const field = new ActivationField();
    const listener = vi.fn();
    const unsubscribe = field.subscribe(listener);
    field.strike(['a'], 1);
    expect(listener).toHaveBeenCalledTimes(1);

    unsubscribe();
    field.strike(['b'], 1);
    expect(listener).toHaveBeenCalledTimes(1);
    expect(field.heatOf('b')).toBe(1);
  });
});

