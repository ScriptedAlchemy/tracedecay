import { describe, expect, it } from 'vitest';

import {
  codeViewBlocker,
  codeViewNeedsFocus,
  codeViewsOffered,
  readCodeLocation,
  writeCodeLocation,
} from './codeView.ts';

describe('Code view locations', () => {
  it('restores every shell view while preserving symbol identity', () => {
    for (const view of ['atlas', 'trace', 'shared-code', 'compare'] as const) {
      expect(readCodeLocation(new URLSearchParams(`view=${view}&symbol=symbol-42`))).toEqual({
        view,
        focusId: 'symbol-42',
      });
    }

    expect(readCodeLocation(new URLSearchParams('symbol=symbol-42'))).toEqual({
      view: 'cortex',
      focusId: 'symbol-42',
    });
  });

  it('defaults unknown views to Cortex without losing a valid symbol focus', () => {
    expect(
      readCodeLocation(new URLSearchParams('view=core&symbol=symbol-42')),
    ).toEqual({
      view: 'cortex',
      focusId: 'symbol-42',
    });
  });

  it('writes the default view without redundant query parameters', () => {
    expect(
      writeCodeLocation(
        new URLSearchParams('view=trace&symbol=symbol-42'),
        {
          view: 'cortex',
          focusId: null,
        },
      ).toString(),
    ).toBe('');
  });
});

describe('Code view switcher', () => {
  it('hides a pending view until a deep link asks for it', () => {
    expect(codeViewsOffered('cortex')).toEqual([
      'cortex',
      'trace',
      'shared-code',
      'compare',
    ]);
    expect(codeViewsOffered('atlas')).toEqual([
      'cortex',
      'trace',
      'shared-code',
      'compare',
      'atlas',
    ]);
  });
});

describe('Code view availability', () => {
  it('keeps Trace unavailable until a symbol is selected', () => {
    expect(codeViewBlocker('trace', 'absent')).toEqual({
      kind: 'unavailable',
      title: 'Trace needs a selected symbol',
      detail: 'Select a symbol in Cortex, then return to Trace.',
    });
    expect(codeViewBlocker('trace', 'available')).toBeNull();
  });

  it('distinguishes a resolving deep link from an unknown symbol', () => {
    expect(codeViewBlocker('trace', 'loading')?.kind).toBe('loading');
    expect(codeViewBlocker('trace', 'unavailable')).toEqual({
      kind: 'unavailable',
      title: 'The selected symbol is unavailable',
      detail: 'The current graph generation did not resolve the URL identity.',
    });
  });

  it('states the unmounted projection Atlas still needs', () => {
    expect(codeViewBlocker('atlas', 'absent')?.detail).toMatch(
      /structural treemap/i,
    );
  });

  it('gates Shared Code on the same selected symbol as Trace', () => {
    expect(codeViewBlocker('shared-code', 'absent')).toEqual({
      kind: 'unavailable',
      title: 'Shared Code needs a selected symbol',
      detail: 'Select a symbol in Cortex, then return to Shared Code.',
    });
    expect(codeViewBlocker('shared-code', 'loading')?.kind).toBe('loading');
    expect(codeViewBlocker('shared-code', 'unavailable')?.title).toBe(
      'The selected symbol is unavailable',
    );
    expect(codeViewBlocker('shared-code', 'available')).toBeNull();
    expect(codeViewNeedsFocus('shared-code')).toBe(true);
    expect(codeViewNeedsFocus('trace')).toBe(true);
  });

  it('opens Compare without a symbol: its selection is its own', () => {
    for (const focus of ['absent', 'loading', 'available', 'unavailable'] as const) {
      expect(codeViewBlocker('compare', focus)).toBeNull();
    }
    expect(codeViewNeedsFocus('compare')).toBe(false);
    expect(codeViewNeedsFocus('cortex')).toBe(false);
  });
});
