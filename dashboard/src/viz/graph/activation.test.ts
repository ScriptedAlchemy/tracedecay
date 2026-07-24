import { describe, expect, it, vi } from 'vitest';
import { ActivationField } from './activation.ts';

describe('ActivationField subscription', () => {
  it('notifies subscribers when a strike lands', () => {
    const field = new ActivationField();
    const listener = vi.fn();
    field.subscribe(listener);
    field.strike(['a'], 0.5);
    expect(listener).toHaveBeenCalledTimes(1);
    field.strike(['a', 'b'], 0.2);
    expect(listener).toHaveBeenCalledTimes(2);
  });

  it('never notifies for a strike with no ids — nothing real happened', () => {
    const field = new ActivationField();
    const listener = vi.fn();
    field.subscribe(listener);
    field.strike([], 1);
    expect(listener).not.toHaveBeenCalled();
  });

  it('stops notifying once unsubscribed', () => {
    const field = new ActivationField();
    const listener = vi.fn();
    const unsubscribe = field.subscribe(listener);
    field.strike(['a'], 1);
    unsubscribe();
    field.strike(['a'], 1);
    expect(listener).toHaveBeenCalledTimes(1);
  });

  it('supports multiple independent subscribers', () => {
    const field = new ActivationField();
    const first = vi.fn();
    const second = vi.fn();
    field.subscribe(first);
    const unsubscribeSecond = field.subscribe(second);
    field.strike(['a'], 1);
    unsubscribeSecond();
    field.strike(['a'], 1);
    expect(first).toHaveBeenCalledTimes(2);
    expect(second).toHaveBeenCalledTimes(1);
  });
});
