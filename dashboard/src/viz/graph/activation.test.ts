import { describe, expect, it, vi } from 'vitest';
import { ActivationField } from './activation.ts';

describe('ActivationField subscription', () => {
  it('notifies once per strike so an outside striker can wake a renderer', () => {
    const field = new ActivationField();
    const listener = vi.fn();
    field.subscribe(listener);
    field.strike(['a', 'b'], 0.5);
    expect(listener).toHaveBeenCalledTimes(1);
    field.strike(['a'], 0.5);
    expect(listener).toHaveBeenCalledTimes(2);
  });

  it('stays silent when a strike carries no ids — nothing real happened', () => {
    const field = new ActivationField();
    const listener = vi.fn();
    field.subscribe(listener);
    field.strike([], 1);
    expect(listener).not.toHaveBeenCalled();
  });

  it('never fires on its own: decay is not an event', () => {
    // The field has no clock. `tick` is decay bookkeeping driven by whoever is
    // already drawing; if it notified, a renderer would wake itself forever.
    const field = new ActivationField({ halfLifeMs: 100 });
    field.strike(['a'], 1);
    const listener = vi.fn();
    field.subscribe(listener);
    field.tick(0);
    field.tick(1_000);
    field.tick(2_000);
    expect(listener).not.toHaveBeenCalled();
    expect(field.warm).toBe(false);
  });

  it('stops notifying once unsubscribed', () => {
    const field = new ActivationField();
    const listener = vi.fn();
    field.subscribe(listener)();
    field.strike(['a'], 1);
    expect(listener).not.toHaveBeenCalled();
  });
});
