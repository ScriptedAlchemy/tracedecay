import { describe, expect, it } from 'vitest';

import {
  codeViewBlocker,
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
      view: 'topology',
      focusId: 'symbol-42',
    });
  });

  it('defaults unknown views to Topology without losing a valid symbol focus', () => {
    expect(
      readCodeLocation(new URLSearchParams('view=core&symbol=symbol-42')),
    ).toEqual({
      view: 'topology',
      focusId: 'symbol-42',
    });
  });

  it('maps published Trace and Core links into Trace with the same symbol', () => {
    for (const legacyView of ['trace', 'core'] as const) {
      expect(
        readCodeLocation(
          new URLSearchParams(
            `structureLens=${legacyView}&structureFocus=symbol-42`,
          ),
        ),
      ).toEqual({ view: 'trace', focusId: 'symbol-42' });
    }
  });

  it('writes the default view without old or redundant query parameters', () => {
    expect(
      writeCodeLocation(
        new URLSearchParams(
          'view=trace&symbol=symbol-42&structureLens=core&structureFocus=old',
        ),
        {
          view: 'topology',
          focusId: null,
        },
      ).toString(),
    ).toBe('');
  });
});

describe('Code view availability', () => {
  it('keeps Trace unavailable until a symbol is selected', () => {
    expect(codeViewBlocker('trace', 'absent')).toEqual({
      kind: 'unavailable',
      title: 'Trace needs a selected symbol',
      detail: 'Select a symbol in Topology, then return to Trace.',
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

  it('states the unmounted projection each pending view needs', () => {
    expect(codeViewBlocker('atlas', 'absent')?.detail).toMatch(
      /structural treemap/i,
    );
    expect(codeViewBlocker('shared-code', 'absent')?.detail).toMatch(
      /clone family/i,
    );
    expect(codeViewBlocker('compare', 'absent')?.detail).toMatch(
      /revision-pair/i,
    );
  });
});
