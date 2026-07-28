import { useEffect, useMemo, useRef, useState } from 'react';
import type { EChartsOption, ECharts } from 'echarts';
import { useReducedMotion } from '../trace/reducedMotion.ts';
import { isRegisteredSeries } from './echarts.ts';

/**
 * Series types in this option that the dashboard's ECharts build cannot draw.
 *
 * ECharts answers an unregistered series with an empty canvas, not an error, so
 * without this the reader would see a chart-shaped blank where a measurement
 * belongs. `series` may be one object or an array; an entry with no `type` is
 * left to ECharts, which infers it from the surrounding option.
 */
function unsupportedSeries(option: EChartsOption): string[] {
  const series = option.series;
  const entries = Array.isArray(series) ? series : series ? [series] : [];
  const unsupported = entries
    .map((entry) => (entry as { type?: unknown }).type)
    .filter((type): type is string => typeof type === 'string')
    .filter((type) => !isRegisteredSeries(type));
  return [...new Set(unsupported)];
}

/** Resolve the live theme into the chart's own styling. Canvas renderers
 * cannot consume CSS variables, so the tokens are sampled off the container
 * every time the option is (re)applied. */
function themedOption(
  container: HTMLElement,
  option: EChartsOption,
  reducedMotion: boolean,
): EChartsOption {
  const style = getComputedStyle(container);
  const token = (name: string, fallback: string) =>
    style.getPropertyValue(name).trim() || fallback;
  const text = token('--raw-text-secondary', '#aab0bd');
  const muted = token('--raw-text-muted', '#8a90a0');
  const edge = token('--raw-edge-subtle', '#333a46');
  const accent = token('--raw-accent', '#7aa2f7');
  // The chart's type has to come from the same token as the rest of the app.
  // This used to name Inter directly, which meant a chart axis silently kept
  // typing in the old face after the design system changed its body face --
  // exactly the drift a token layer exists to prevent.
  const sans = token('--font-sans', 'system-ui, sans-serif');
  return {
    color: [accent],
    // `false`, not a shortened duration: ECharts' entry animation grows bars and
    // sweeps lines from zero toward their real value, which on a chart of
    // measured quantities is a sequence of numbers the daemon never reported.
    // Turning it off draws the reading, once, correctly.
    animation: !reducedMotion,
    textStyle: { color: text, fontFamily: sans },
    axisPointer: { lineStyle: { color: edge } },
    xAxis: undefined,
    yAxis: undefined,
    grid: { left: 8, right: 8, top: 24, bottom: 8, containLabel: true },
    tooltip: {
      backgroundColor: token('--raw-surface-2', '#22252d'),
      borderColor: edge,
      textStyle: { color: text, fontSize: 11 },
    },
    ...option,
    // Merge axis styling into caller axes without clobbering their data.
    ...(option.xAxis
      ? {
          xAxis: {
            axisLine: { lineStyle: { color: edge } },
            axisLabel: { color: muted, fontSize: 10 },
            splitLine: { show: false },
            ...option.xAxis,
          },
        }
      : {}),
    ...(option.yAxis
      ? {
          yAxis: {
            axisLine: { show: false },
            axisLabel: { color: muted, fontSize: 10 },
            splitLine: { lineStyle: { color: edge, opacity: 0.5 } },
            ...option.yAxis,
          },
        }
      : {}),
  };
}

/** ECharts host (plan 11: the single quantitative charting library, loaded
 * lazily per route). Token-driven: colors resolve from the live theme and
 * re-resolve on theme flips; reduced motion disables animation. The
 * surrounding view must keep an accessible textual equivalent.
 *
 * Lifecycle: the instance is created ONCE. Callers build `option` inline, so
 * its identity changes on every render — keying the mount effect to it threw
 * the canvas away, re-ran the dynamic `import('echarts')`, and re-initialised
 * the chart on every parent update. Applying the option is now its own effect,
 * and a theme flip bumps a revision counter rather than being read through a
 * closure that would otherwise be pinned to the mount-time option. */
export function Chart({
  option,
  height = 220,
  ariaLabel,
}: {
  option: EChartsOption;
  height?: number;
  ariaLabel: string;
}) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const chartRef = useRef<ECharts | null>(null);
  const [themeRevision, setThemeRevision] = useState(0);
  const [ready, setReady] = useState(false);
  // The app's own three-state control (system / reduced / full), which persists
  // across visits. This used to read `prefers-reduced-motion` directly, so a
  // reader who pinned "Reduced" on a machine whose OS says otherwise got no
  // effect here at all — and a reader who pinned "Full" was overridden by the
  // OS. The media query is one input to that decision, not the decision.
  const { reduced } = useReducedMotion();
  // Pure over `option`, so its identity is the whole cache key — a caller that
  // stabilizes the option literal stops re-scanning its series on every render.
  const unsupported = useMemo(() => unsupportedSeries(option), [option]);

  useEffect(() => {
    // Null whenever the option carries an unsupported series, because that
    // branch renders a notice instead of the canvas the chart would mount into.
    const container = containerRef.current;
    if (!container) return;
    let disposed = false;
    let chart: ECharts | null = null;

    void import('./echarts.ts').then((echarts) => {
      if (disposed || !containerRef.current) return;
      chart = echarts.init(containerRef.current);
      chartRef.current = chart;
      setReady(true);
    });

    const themeObserver = new MutationObserver(() => setThemeRevision((n) => n + 1));
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme', 'data-contrast'],
    });
    const resize = new ResizeObserver(() => chartRef.current?.resize());
    resize.observe(container);

    return () => {
      disposed = true;
      themeObserver.disconnect();
      resize.disconnect();
      chart?.dispose();
      chartRef.current = null;
    };
  }, []);

  useEffect(() => {
    const container = containerRef.current;
    const chart = chartRef.current;
    if (!container || !chart) return;
    chart.setOption(themedOption(container, option, reduced), { notMerge: true });
  }, [option, themeRevision, ready, reduced]);

  // A series this build cannot draw is a reporting failure, not an empty
  // measurement, so it says so instead of mounting a canvas that would stay
  // blank next to a real reading.
  if (unsupported.length > 0) {
    return (
      <div
        style={{ height }}
        className="flex items-center justify-center border border-edge-subtle bg-surface-1 px-3 py-2 text-2xs leading-relaxed text-text-secondary"
      >
        {`This build cannot draw a ${unsupported.join(' or ')} series, so ${ariaLabel} is not shown. Register the series type in viz/chart/echarts.ts.`}
      </div>
    );
  }

  return <div ref={containerRef} style={{ height }} role="img" aria-label={ariaLabel} />;
}
