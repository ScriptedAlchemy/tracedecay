import { describe, expect, it } from 'vitest';

import { codeReadState, describeCodeReadReason } from './codeRead.ts';

describe('code-read reason wording', () => {
  it('words every typed reason the two routes emit and passes unknown ones through', () => {
    expect(describeCodeReadReason('selected_source_not_found')).toBe(
      'The selected occurrence is not in the retained clone index: it is stale, unknown, or outside the retained scope.',
    );
    expect(describeCodeReadReason('invalid_request')).toBe(
      'The request was malformed; the daemon refused it before reading anything.',
    );
    expect(describeCodeReadReason('selected_revision_changed')).toBe(
      'A selected reference no longer points at its expected revision. The comparison was not made against a different commit.',
    );
    expect(describeCodeReadReason('code_read_authority_unavailable')).toBe(
      'The code-read authority is not mounted on this daemon.',
    );
    expect(describeCodeReadReason('code_generation_unavailable')).toBe(
      'No sealed code-index generation is available for this scope yet.',
    );
    expect(describeCodeReadReason('code_read_capacity_unavailable')).toBe(
      'A retained generation exceeds the bounded-read limits, so the daemon refused rather than read it partially.',
    );
    expect(describeCodeReadReason('code_index_reset_required')).toBe(
      'The clone index reports corruption and requires an explicit reset; nothing was read.',
    );
    expect(describeCodeReadReason('request_cancelled')).toBe(
      'The read was cancelled before it finished.',
    );
    expect(describeCodeReadReason('request_timed_out')).toBe(
      'The read did not finish within its deadline.',
    );
    expect(describeCodeReadReason('code_read_failed')).toBe('The read failed inside the daemon.');
    expect(describeCodeReadReason('something_new')).toBe('something_new');
    expect(describeCodeReadReason(undefined)).toBeUndefined();
  });

  it('keeps the daemon state and swaps only the detail on a transport outcome', () => {
    const state = codeReadState(
      false,
      { outcome: 'transport', state: 'stale', detail: 'selected_revision_changed' },
      { loading: 'l', transport: 't' },
    );
    expect(state.kind).toBe('blocked');
    if (state.kind !== 'blocked') throw new Error('unreachable');
    expect(state.state).toBe('stale');
    expect(state.detail).toBe(
      'A selected reference no longer points at its expected revision. The comparison was not made against a different commit.',
    );

    const bare = codeReadState(false, { outcome: 'transport', state: 'offline' }, { loading: 'l', transport: 't' });
    if (bare.kind !== 'blocked') throw new Error('unreachable');
    expect(bare.detail).toBe('t');
  });
});
