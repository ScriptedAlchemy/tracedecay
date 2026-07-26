import { afterEach, describe, expect, it } from 'vitest';
import {
  getMotionPreference,
  resolveReducedMotion,
  setMotionPreference,
} from './reducedMotion.ts';

/**
 * The bridge between the persisted preference and the stylesheet.
 *
 * CSS can read `prefers-reduced-motion` but not our own control, so the
 * preference has to be published to the document for `theme/tokens.css` to act
 * on it. Without that the two disagree in both directions: pinning Reduced on a
 * machine reporting no preference left every entrance and transition running,
 * and pinning Full on a machine set to reduce could not bring them back. The
 * three-state attribute — rather than a boolean — is what makes the second case
 * expressible at all.
 */
describe('motion preference publication', () => {
  afterEach(() => {
    setMotionPreference('system');
    localStorage.removeItem('td.motion-preference');
  });

  it('publishes a pinned "reduced" preference to the document', () => {
    setMotionPreference('reduced');
    expect(document.documentElement.dataset['motion']).toBe('reduced');
    expect(getMotionPreference()).toBe('reduced');
  });

  it('publishes a pinned "full" preference, so CSS can outvote an OS that reduces', () => {
    setMotionPreference('full');
    // 'full' must be distinguishable from absent: tokens.css keys its media-query
    // path on `:not([data-motion='full'])`, which only works if the attribute is
    // actually present and carries the three-state value.
    expect(document.documentElement.dataset['motion']).toBe('full');
  });

  it('removes the attribute for "system", handing the decision back to the OS', () => {
    setMotionPreference('reduced');
    setMotionPreference('system');
    expect(document.documentElement.dataset['motion']).toBeUndefined();
    expect(localStorage.getItem('td.motion-preference')).toBeNull();
  });

  it('resolves the three states against the OS setting', () => {
    expect(resolveReducedMotion('reduced', false)).toBe(true);
    expect(resolveReducedMotion('reduced', true)).toBe(true);
    expect(resolveReducedMotion('full', true)).toBe(false);
    expect(resolveReducedMotion('full', false)).toBe(false);
    expect(resolveReducedMotion('system', true)).toBe(true);
    expect(resolveReducedMotion('system', false)).toBe(false);
  });
});
