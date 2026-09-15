import { describe, expect, it } from 'vitest';

import { codeReadState, describeCodeReadReason } from './codeRead.ts';

describe('code-read reason wording', () => {
  it('words every typed reason the two routes emit and passes unknown ones through', () => {
    for (const reason of [
      'selected_source_not_found',
      'invalid_request',
      'selected_revision_changed',
      'code_read_authority_unavailable',
      'code_generation_unavailable',
      'code_read_capacity_unavailable',
      'code_index_reset_required',
      'request_cancelled',
      'request_timed_out',
      'code_read_failed',
    ]) {
      const sentence = describeCodeReadReason(reason);
      expect(sentence).not.toBe(reason);
      expect(sentence).toMatch(/\.$/);
    }
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
    expect(state.detail).toMatch(/no longer points at its expected revision/);

    const bare = codeReadState(false, { outcome: 'transport', state: 'offline' }, { loading: 'l', transport: 't' });
    if (bare.kind !== 'blocked') throw new Error('unreachable');
    expect(bare.detail).toBe('t');
  });
});
