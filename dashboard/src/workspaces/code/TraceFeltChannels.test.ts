/**
 * An inert channel must not read as a measured one.
 *
 * The felt half of the sensory contract prints five rows on a route that drives
 * two of them, so the only thing standing between that and a surface claiming
 * five measurements is how the other three print. These assertions pin the
 * words and the ink for each state: an unavailable channel says so, in the
 * unknown tone, and never borrows the vocabulary of a measured one.
 */
import { describe, expect, it } from 'vitest';

import type { SensoryChannelState } from '../../viz/trace/types.ts';
import { channelState } from './TraceFeltChannels.tsx';

const INERT: readonly SensoryChannelState[] = ['not-on-this-wire', 'coarser-scope'];

describe('channelState', () => {
  it('prints a driven channel as measured, in the ordinary reading tone', () => {
    expect(channelState('measured')).toEqual({
      label: 'measured',
      tone: 'text-text-secondary',
    });
  });

  it('says which kind of absence, because they are different claims', () => {
    // "No field on this payload carried it" and "it exists, but only at a
    // coarser scope than this field draws" call for different next actions, so
    // they are not allowed to collapse into one word.
    expect(channelState('not-on-this-wire').label).toBe('not on this wire');
    expect(channelState('coarser-scope').label).toBe('coarser scope');
    expect(channelState('not-on-this-wire').label).not.toBe(
      channelState('coarser-scope').label,
    );
  });

  it('never lets an inert channel read as measured', () => {
    for (const state of INERT) {
      const printed = channelState(state);
      expect(printed.label).not.toBe('measured');
      expect(printed.label.length).toBeGreaterThan(0);
      // The unknown ink is the same one `Reading` prints an absent measurement
      // in, so an inert channel looks like the absence it is on both plates.
      expect(printed.tone).toBe('text-state-unknown');
    }
  });
});
