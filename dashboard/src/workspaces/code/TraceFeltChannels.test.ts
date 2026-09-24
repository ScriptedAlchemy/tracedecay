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

import { channelState } from './TraceFeltChannels.tsx';

describe('channelState', () => {
  it('prints each inert channel as its own kind of absence, in the unknown ink', () => {
    // "No field on this payload carried it" and "it exists, but only at a
    // coarser scope than this field draws" call for different next actions, so
    // they are not allowed to collapse into one word. The unknown ink is the
    // same one `Reading` prints an absent measurement in.
    expect(channelState('not-on-this-wire')).toEqual({
      label: 'not on this wire',
      tone: 'text-state-unknown',
    });
    expect(channelState('coarser-scope')).toEqual({
      label: 'coarser scope',
      tone: 'text-state-unknown',
    });
  });

  it('keeps the measured vocabulary and ink for a driven channel', () => {
    expect(channelState('measured')).toEqual({
      label: 'measured',
      tone: 'text-text-secondary',
    });
  });
});
