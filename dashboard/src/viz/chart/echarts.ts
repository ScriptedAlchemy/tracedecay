/**
 * The dashboard's ECharts build: only the pieces the product draws.
 *
 * `import('echarts')` pulls the whole distribution — every series type, both
 * renderers, the map/geo stack — which measured 1,080 KiB raw and 288 KiB
 * Brotli in one async chunk, against plan 11's 200 KiB per-chunk ceiling. The
 * product draws bar and line series on a cartesian grid with a tooltip, so that
 * is what gets registered.
 *
 * REGISTERING A NEW SERIES TYPE IS A TWO-LINE CHANGE HERE, AND IT IS REQUIRED.
 * ECharts does not throw on an unregistered series; it draws nothing. On a
 * chart of measured quantities that is a blank canvas standing in for real
 * data, so `Chart` refuses to render a series type absent from
 * `REGISTERED_SERIES` rather than presenting emptiness as a reading.
 */
import { init, use } from 'echarts/core';
import { BarChart, LineChart } from 'echarts/charts';
import { GridComponent, TooltipComponent } from 'echarts/components';
import { CanvasRenderer } from 'echarts/renderers';

use([BarChart, LineChart, GridComponent, TooltipComponent, CanvasRenderer]);

/** Series types this build can actually draw. Keep in step with `use` above. */
export const REGISTERED_SERIES = ['bar', 'line'] as const;

export type RegisteredSeries = (typeof REGISTERED_SERIES)[number];

export function isRegisteredSeries(type: string): type is RegisteredSeries {
  return (REGISTERED_SERIES as readonly string[]).includes(type);
}

export { init };
