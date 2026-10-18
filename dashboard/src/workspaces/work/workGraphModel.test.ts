import { describe, expect, it } from 'vitest';
import { WorkGraphReadV1Schema, type WorkGraphReadV1 } from '../../contracts/generated.ts';
import {
  workGraphRead,
  workGraphTimeline,
  type WorkGraphVersionSpec,
} from '../../test/workGraphFixture.ts';
import type { WorkResult } from './workApi.ts';
import {
  graphChannelGap,
  graphEntryOf,
  workGraphReading,
} from './workGraphModel.ts';

/**
 * The work-product graph read, falsified against wire-shaped fixtures.
 *
 * Every read here goes through `WorkGraphReadV1Schema` first, the same schema
 * `callWork` parses the wire with, so the object these derivations are proved
 * against is the contract and not a hand-shaped approximation of it. The
 * assertions that matter most are the separations: an empty window from a
 * refusal, a withheld figure from a zero,
 * and a timeline's own order from the authority's.
 */

function read(spec: WorkGraphVersionSpec): WorkResult<WorkGraphReadV1> {
  return { outcome: 'value', value: WorkGraphReadV1Schema.parse(workGraphRead(spec)) };
}

function timeline(
  versions: readonly WorkGraphVersionSpec[],
  coverage?: Parameters<typeof workGraphTimeline>[1],
): WorkResult<WorkGraphReadV1> {
  return {
    outcome: 'value',
    value: WorkGraphReadV1Schema.parse(workGraphTimeline(versions, coverage)),
  };
}

const REFUSED = {
  outcome: 'refused',
  state: 'unavailable',
  detail: 'the authority could not be read',
} as const;

describe('the graph read as a reading', () => {
  it('reduces a current read to its one version', () => {
    const reading = workGraphReading(read({ tasks: [{ taskId: 'a' }] }));

    expect(reading.state).toBe('read');
    if (reading.state !== 'read') return;
    expect(reading.page.mode).toBe('current');
    expect(reading.page.entries).toBe(1);
    expect(reading.page.coverage).toBeNull();
    expect(reading.page.entry?.projections.dag.task_ids).toEqual(['a']);
  });

  /** The authority's ordering is not restated as an assumption: the newest
   * entry is chosen by comparing the versions, so a timeline delivered oldest
   * first still reduces to the version that is actually newest. */
  it('reduces a timeline to its newest version by valid_at, not by position', () => {
    const reading = workGraphReading(
      timeline([
        { tasks: [{ taskId: 'old' }], version: 3, validAt: 1_000 },
        { tasks: [{ taskId: 'new' }], version: 4, validAt: 2_000 },
        { tasks: [{ taskId: 'mid' }], version: 5, validAt: 1_500 },
      ]),
    );

    expect(reading.state).toBe('read');
    if (reading.state !== 'read') return;
    expect(reading.page.entries).toBe(3);
    expect(reading.page.coverage).toEqual({ coverage: 'complete', returned: 3 });
    expect(reading.page.entry?.projections.dag.task_ids).toEqual(['new']);
  });

  it('keeps the pending, refused and answered states as three different answers', () => {
    expect(workGraphReading(undefined)).toEqual({ state: 'pending' });
    expect(workGraphReading(REFUSED)).toEqual({
      state: 'refused',
      chip: 'unavailable',
      detail: 'the authority could not be read',
    });
    expect(graphEntryOf(workGraphReading(undefined))).toBeNull();
  });
});

describe('the empty window', () => {
  /** The honest success with nothing in it: complete coverage over zero
   * returned entries is the authority answering, and it must never wear a
   * failure state. */
  it('reads an empty complete timeline as a success without a version', () => {
    const reading = workGraphReading(timeline([]));

    expect(reading.state).toBe('read');
    if (reading.state !== 'read') return;
    expect(reading.page.entry).toBeNull();
    expect(reading.page.entries).toBe(0);
    expect(reading.page.coverage).toEqual({ coverage: 'complete', returned: 0 });

    const gap = graphChannelGap(reading, 'declared effort mass');
    expect(gap.available).toBe(false);
    if (gap.available) return;
    expect(gap.state).toBe('complete_zero_findings');
    expect(gap.detail).toContain('empty window');
  });

  it('names the read state, not a schema absence, when the read has not answered', () => {
    const pending = graphChannelGap({ state: 'pending' }, 'recent churn');
    expect(pending.available).toBe(false);
    if (pending.available) return;
    expect(pending.state).toBe('loading');

    const refused = graphChannelGap(workGraphReading(REFUSED), 'recent churn');
    expect(refused.available).toBe(false);
    if (refused.available) return;
    expect(refused.state).toBe('unavailable');
    expect(refused.detail).toContain('the authority could not be read');
  });
});
