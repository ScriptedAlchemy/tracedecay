import { describe, expect, it } from 'vitest';

import {
  codeViewBlocker,
  codeViewNeedsFocus,
  readCodeLocation,
  writeCodeLocation,
} from './codeView.ts';

describe('Code view locations', () => {
  it('restores every mounted view while preserving symbol identity', () => {
    for (const view of ['cortex', 'trace', 'shared-code', 'compare'] as const) {
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

  it.each(['core', 'atlas', 'unknown'])(
    'opens the unsupported %s view in Cortex without losing symbol focus',
    (view) => {
      expect(
        readCodeLocation(new URLSearchParams(`view=${view}&symbol=symbol-42`)),
      ).toEqual({
        view: 'cortex',
        focusId: 'symbol-42',
      });
    },
  );

  it('preserves project and revision selectors when rewriting a retired view', () => {
    const current = new URLSearchParams(
      'view=atlas&symbol=symbol-42&project=project-7&base_revision=abc&scope=local',
    );
    const next = writeCodeLocation(current, readCodeLocation(current));

    expect(next.has('view')).toBe(false);
    expect(next.get('symbol')).toBe('symbol-42');
    expect(next.get('project')).toBe('project-7');
    expect(next.get('base_revision')).toBe('abc');
    expect(next.get('scope')).toBe('local');
    expect(current.get('view')).toBe('atlas');
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
