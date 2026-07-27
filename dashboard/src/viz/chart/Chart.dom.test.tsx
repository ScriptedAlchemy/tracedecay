import { render, waitFor } from '@testing-library/react';
import type { EChartsOption } from 'echarts';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { Chart } from './Chart.tsx';

/**
 * Two defects lived in this component, and both were invisible from the outside:
 * the chart named its own font family, and it decided about motion by asking the
 * OS instead of the app.
 *
 * The font one mattered more than it looks. `textStyle.fontFamily` was the only
 * hard-coded family in the tree, so every ECharts axis label, tooltip and legend
 * in the product kept typing in the old body face after the design system moved
 * to a new one — a token layer that a component is free to bypass is not a token
 * layer. The assertion below is deliberately about the SOURCE of the family
 * rather than its value: naming the new face here would just re-create the bug
 * one face later.
 *
 * The motion one is a correctness claim, not a comfort setting. ECharts' entry
 * animation grows a bar from zero to its real value, which on a chart of
 * measured quantities draws a sequence of numbers the daemon never reported. A
 * reader who asks for stillness is asking to be shown the reading rather than a
 * performance of it, and the request has to be honoured whichever way it was
 * made.
 */

/** Every option handed to the chart instance, in order. */
const applied: EChartsOption[] = [];

// The narrow registered build, not the full distribution: `Chart` imports
// `./echarts.ts` so the bundle carries only the series the product draws.
// `isRegisteredSeries` is the real one — mocking it would make the
// unsupported-series case unable to fail.
vi.mock('./echarts.ts', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./echarts.ts')>()),
  init: () => ({
    setOption: (option: EChartsOption) => {
      applied.push(option);
    },
    resize: () => {},
    dispose: () => {},
  }),
}));

/** The live theme, as the component would sample it off its own container. */
const TOKENS: Record<string, string> = {
  '--font-sans': "'IBM Plex Sans Variable', system-ui, sans-serif",
  '--raw-text-secondary': '#aab0bd',
  '--raw-text-muted': '#8a90a0',
  '--raw-edge-subtle': '#333a46',
  '--raw-accent': '#7aa2f7',
  '--raw-surface-2': '#22252d',
};

const OPTION: EChartsOption = {
  xAxis: { type: 'category', data: ['a', 'b'] },
  yAxis: { type: 'value' },
  series: [{ type: 'bar', data: [1, 2] }],
};

/** Resolve the OS media query to a fixed answer for the duration of a case. */
function stubSystemPrefersReduced(matches: boolean): void {
  vi.stubGlobal(
    'matchMedia',
    vi.fn().mockReturnValue({
      matches,
      addEventListener: () => {},
      removeEventListener: () => {},
    }),
  );
}

/** The chart's last applied option, once the lazy `import('echarts')` lands. */
async function lastOption(): Promise<EChartsOption> {
  await waitFor(() => expect(applied.length).toBeGreaterThan(0));
  return applied[applied.length - 1]!;
}

describe('Chart theming and motion', () => {
  beforeEach(() => {
    applied.length = 0;
    localStorage.removeItem('td.motion-preference');
    stubSystemPrefersReduced(false);
    vi.stubGlobal(
      'ResizeObserver',
      class {
        observe(): void {}
        disconnect(): void {}
      },
    );
    // jsdom does not inherit custom properties into computed style, so the token
    // layer is supplied here rather than through a stylesheet. Only custom
    // properties are intercepted; everything else answers normally, because
    // Testing Library consults computed style for its own visibility checks.
    const real = window.getComputedStyle.bind(window);
    vi.stubGlobal('getComputedStyle', (element: Element, pseudo?: string | null) => {
      const computed = real(element, pseudo ?? undefined);
      return {
        getPropertyValue: (name: string) => TOKENS[name] ?? computed.getPropertyValue(name),
      };
    });
  });

  afterEach(() => {
    vi.unstubAllGlobals();
    localStorage.removeItem('td.motion-preference');
  });

  it('types from the --font-sans token rather than naming a family', async () => {
    render(<Chart option={OPTION} ariaLabel="tokens per day" />);
    const option = await lastOption();

    expect(option.textStyle).toMatchObject({ fontFamily: TOKENS['--font-sans'] });
    // The specific regression: this component used to spell out the old face,
    // which silently outranked the design system on every chart in the app.
    // Inter is no longer vendored at all, so naming it here would now point at
    // a face the bundle cannot supply.
    expect(JSON.stringify(option)).not.toContain('Inter');
  });

  it('animates by default when nothing has asked for stillness', async () => {
    render(<Chart option={OPTION} ariaLabel="tokens per day" />);
    expect((await lastOption()).animation).toBe(true);
  });

  it('honours a pinned "reduced" preference on an OS reporting none', async () => {
    // The exact bypass: the app's own control said reduce, the OS said nothing,
    // and the chart animated anyway because it only ever asked the OS.
    localStorage.setItem('td.motion-preference', 'reduced');
    render(<Chart option={OPTION} ariaLabel="tokens per day" />);

    // `false`, never a shortened duration — the intermediate values are numbers
    // that were never measured, so they must not be drawn at any speed.
    expect((await lastOption()).animation).toBe(false);
  });

  it('honours a pinned "full" preference on an OS asking to reduce', async () => {
    stubSystemPrefersReduced(true);
    localStorage.setItem('td.motion-preference', 'full');
    render(<Chart option={OPTION} ariaLabel="tokens per day" />);

    // A control that can only agree with the OS is not a control.
    expect((await lastOption()).animation).toBe(true);
  });

  it('follows the OS when the preference is left on system', async () => {
    stubSystemPrefersReduced(true);
    render(<Chart option={OPTION} ariaLabel="tokens per day" />);
    expect((await lastOption()).animation).toBe(false);
  });
});

/**
 * The dashboard registers only the series it draws, which is what keeps the
 * ECharts chunk inside plan 11's per-chunk budget. ECharts answers an
 * unregistered series with an empty canvas rather than an error, so the cost of
 * that narrowing is a chart-shaped blank standing in for a real measurement.
 */
describe('Chart registered-series guard', () => {
  beforeEach(() => {
    applied.length = 0;
    stubSystemPrefersReduced(false);
    vi.stubGlobal(
      'ResizeObserver',
      class {
        observe(): void {}
        disconnect(): void {}
      },
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('draws a registered series', async () => {
    render(<Chart option={OPTION} ariaLabel="tokens per day" />);
    expect((await lastOption()).series).toBeTruthy();
  });

  it('names an unregistered series instead of mounting a blank canvas', async () => {
    const { queryByRole, getByText } = render(
      <Chart
        option={{ series: [{ type: 'treemap', data: [] }] } as EChartsOption}
        ariaLabel="fact families"
      />,
    );

    expect(getByText(/cannot draw a treemap series/i)).toBeTruthy();
    expect(getByText(/fact families is not shown/i)).toBeTruthy();
    // The canvas host carries `role="img"`; its absence is the whole point.
    expect(queryByRole('img')).toBeNull();
    await waitFor(() => expect(applied.length).toBe(0));
  });

  it('reports every unregistered series once, and still refuses a mixed option', () => {
    const { getByText } = render(
      <Chart
        option={
          {
            series: [
              { type: 'bar', data: [1] },
              { type: 'pie', data: [] },
              { type: 'pie', data: [] },
            ],
          } as EChartsOption
        }
        ariaLabel="spend mix"
      />,
    );

    // One registered series does not license drawing the option: the pie would
    // simply be missing from a chart that otherwise looks complete.
    expect(getByText(/cannot draw a pie series/i)).toBeTruthy();
  });
});
