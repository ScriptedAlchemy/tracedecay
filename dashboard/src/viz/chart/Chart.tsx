import { useEffect, useRef } from 'react';
import type { EChartsOption, ECharts } from 'echarts';

/** ECharts host (plan 11: the single quantitative charting library, loaded
 * lazily per route). Token-driven: colors resolve from the live theme and
 * re-resolve on theme flips; reduced motion disables animation. The
 * surrounding view must keep an accessible textual equivalent. */
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

  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    let disposed = false;

    const themed = (): EChartsOption => {
      const style = getComputedStyle(container);
      const token = (name: string, fallback: string) =>
        style.getPropertyValue(name).trim() || fallback;
      const text = token('--raw-text-secondary', '#aab0bd');
      const muted = token('--raw-text-muted', '#8a90a0');
      const edge = token('--raw-edge-subtle', '#333a46');
      const accent = token('--raw-accent', '#7aa2f7');
      const reducedMotion = window.matchMedia('(prefers-reduced-motion: reduce)').matches;
      return {
        color: [accent],
        animation: !reducedMotion,
        textStyle: { color: text, fontFamily: 'Inter Variable, system-ui, sans-serif' },
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
    };

    let chart: ECharts | null = null;
    void import('echarts').then((echarts) => {
      if (disposed || !containerRef.current) return;
      chart = echarts.init(containerRef.current);
      chartRef.current = chart;
      chart.setOption(themed());
    });

    const themeObserver = new MutationObserver(() => {
      chartRef.current?.setOption(themed(), { notMerge: true });
    });
    themeObserver.observe(document.documentElement, {
      attributes: true,
      attributeFilter: ['data-theme'],
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
  }, [option]);

  return <div ref={containerRef} style={{ height }} role="img" aria-label={ariaLabel} />;
}
