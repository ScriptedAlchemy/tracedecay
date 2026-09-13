/**
 * The instrument plate for the TRACE field: the header readout strip and the
 * legend row, both derived from the one `TraceModel` the field is drawn from.
 *
 * Why this is a module and not JSX
 * --------------------------------
 * Sheet 02 of the approved design carries two plates around its field — a
 * seven-cell readout strip above and a six-panel key below. A legend is the
 * one place in an instrument where a *second* source of truth can grow: the
 * picture is drawn from the payload, the legend is typed by hand, and the day
 * the payload changes only one of them moves. The approved sheet avoids this
 * by printing counts it computed from its own dataset, and this module is how
 * that property survives the port: every number on both plates is counted here
 * from `model`, the same record `render.ts` draws. Nothing on either plate is
 * a literal that a payload change could falsify.
 *
 * The house rule from the design note — "every position, size, elevation and
 * width encodes a stated measurement" — has a corollary this file exists to
 * enforce: a measurement that did not arrive is *printed as absent*, never
 * blanked and never defaulted to zero. `ReadoutValue` has no third state for
 * that reason. A caller cannot render a cell without having decided what it
 * says when the wire was silent.
 *
 * Pure by construction: types only, no DOM, no colour, no clock. `sim.ts`
 * imports nothing at all and keeps that stronger boundary; this module sits
 * beside it on the same side of the honesty line.
 */

import type { TraceModel, TraceNode } from './types.ts';

/**
 * A single reading, with its absence made unrepresentable-as-blank.
 *
 * `absent` carries `why` because "no membranes" and "the wire did not carry
 * contains edges" are different claims, and only the second one is true here.
 */
export type ReadoutValue =
  | { readonly kind: 'measured'; readonly value: string; readonly unit: string | null }
  | { readonly kind: 'absent'; readonly why: string };

/** One cell of the header strip. */
export interface ReadoutCell {
  /** Engraved label, as the approved sheet prints it. */
  readonly label: string;
  readonly value: ReadoutValue;
  /**
   * Disclosure that the reading is a floor rather than a total — a list that
   * came back at the endpoint's limit, or a neighbour that was never expanded.
   * `null` when the number is complete as far as this frame can know.
   */
  readonly qualifier: string | null;
}

/** One panel of the legend row. */
export interface LegendPanel {
  readonly label: string;
  /** The sensory contract in one clause: what this channel means. */
  readonly teach: string;
  /** What that channel is actually carrying on THIS frame. */
  readonly reading: ReadoutValue;
  readonly qualifier: string | null;
  /**
   * Which sample the row should draw beside the panel. A closed set so the
   * component cannot invent a swatch for a channel the field does not draw.
   */
  readonly sample: LegendSample;
}

export type LegendSample = 'channel' | 'sill' | 'rows' | 'hue' | 'membrane' | 'mouth';

/* ---- small shared counting helpers -------------------------------------- */

function measured(value: string, unit: string | null = null): ReadoutValue {
  return { kind: 'measured', value, unit };
}

function absent(why: string): ReadoutValue {
  return { kind: 'absent', why };
}

function plural(n: number, one: string, many: string): string {
  return n === 1 ? one : many;
}

/**
 * Inclusive range of a set of numbers as the legend prints it. Returns null
 * for an empty set so the caller must decide what absence reads as, rather
 * than receiving a plausible `0–0`.
 */
function range(values: readonly number[]): string | null {
  if (values.length === 0) return null;
  let low = values[0] as number;
  let high = low;
  for (const value of values) {
    if (value < low) low = value;
    if (value > high) high = value;
  }
  return low === high ? String(low) : `${low}–${high}`;
}

/**
 * The module a symbol belongs to: the directory of its `file_path`.
 *
 * A symbol whose row carried no path has no module *on this wire*, which is
 * not the same as belonging to none — so it returns null and is counted as
 * unattributed rather than folded into a root bucket.
 */
function moduleOf(node: TraceNode): string | null {
  if (node.filePath === null) return null;
  const cut = node.filePath.lastIndexOf('/');
  return cut <= 0 ? '/' : node.filePath.slice(0, cut);
}

/**
 * The endpoint-limit disclosure, shared by every cell whose count it bounds.
 *
 * Deliberately terse. These strings are printed on the plate under the number
 * they qualify, and a disclosure long enough to be skipped is a disclosure
 * that was not really made.
 */
function cappedQualifier(model: TraceModel): string | null {
  const { capped, cappedAt } = model.coverage;
  if (!capped) return null;
  return `a list hit the ${cappedAt ?? 'row'} limit — a floor, not a total`;
}

/* ---- the header readout strip ------------------------------------------- */

/**
 * The seven cells the approved sheet prints above the field, in its order.
 *
 * Every one is counted from `model` here. `FOCUS` is the only cell whose value
 * is a name rather than a number, and it is still a reading: the name the
 * payload resolved to, with the kind as its unit.
 */
export function readoutCells(model: TraceModel): readonly ReadoutCell[] {
  const focus = model.nodes.find((node) => node.id === model.focusId) ?? null;
  const coverage = model.coverage;
  const capped = cappedQualifier(model);

  const upstream = model.nodes.filter((node) => node.ring < 0);
  const downstream = model.nodes.filter((node) => node.ring > 0);
  const upCalls = sumCalls(model, 'up');
  const downCalls = sumCalls(model, 'down');

  // Modules are counted over drawn symbols; crossings are counted over drawn
  // channels, because "crossed" is an event on an edge, not on a node.
  const modules = new Set<string>();
  let unattributed = 0;
  for (const node of model.nodes) {
    const module = moduleOf(node);
    if (module === null) unattributed += 1;
    else modules.add(module);
  }
  const crossings = countCrossings(model);

  return [
    {
      label: 'Focus',
      value: focus ? measured(focus.name, focus.kind) : absent('focus symbol is not among the drawn rows'),
      qualifier: null,
    },
    {
      label: `Callers ≤ ${coverage.hopsFetched} ${plural(coverage.hopsFetched, 'hop', 'hops')}`,
      value: measured(String(upstream.length), `${upCalls} call ${plural(upCalls, 'site', 'sites')}`),
      qualifier: capped,
    },
    {
      label: `Callees ≤ ${coverage.hopsFetched} ${plural(coverage.hopsFetched, 'hop', 'hops')}`,
      value: measured(String(downstream.length), `${downCalls} call ${plural(downCalls, 'site', 'sites')}`),
      qualifier: capped,
    },
    {
      label: 'Depth limit',
      value: measured(`${coverage.hopsFetched} ↑ / ${coverage.hopsFetched} ↓`, 'hops fetched'),
      qualifier:
        coverage.unexpandedNeighbors > 0
          ? `${coverage.unexpandedNeighbors} ${plural(coverage.unexpandedNeighbors, 'neighbour', 'neighbours')} unexpanded — past them is unknown, not zero`
          : null,
    },
    {
      label: 'Beyond the limit',
      value: measured(String(coverage.namedButNotDrawn), 'named, not drawn'),
      // The number counts symbols the fetched rows *named*. Anything past the
      // fetched hops was never named to this view, so it is absent from the
      // count by construction and the cell has to say so.
      qualifier: `past hop ${coverage.hopsFetched}, nothing was named to this view`,
    },
    {
      label: 'Types entered',
      value: coverage.membranesAvailable
        ? measured(
            String(model.membranes.length),
            plural(model.membranes.length, 'membrane', 'membranes'),
          )
        : absent('the payload carried no contains edges'),
      qualifier: coverage.membranesAvailable
        ? null
        : 'not a claim about whether these symbols have types',
    },
    {
      label: 'Modules crossed',
      value:
        modules.size === 0
          ? absent('no drawn row carried a file path')
          : measured(String(modules.size), `${crossings} ${plural(crossings, 'crossing', 'crossings')}`),
      qualifier:
        unattributed > 0
          ? `${unattributed} ${plural(unattributed, 'symbol carries', 'symbols carry')} no file path`
          : null,
    },
  ];
}

/** Call sites on the channels drawn on one side of the focus. */
function sumCalls(model: TraceModel, dir: 'up' | 'down'): number {
  let total = 0;
  for (const channel of model.channels) {
    if (channel.dir === dir) total += channel.calls;
  }
  return total;
}

/** Drawn channels whose two ends sit in different modules. */
function countCrossings(model: TraceModel): number {
  const byId = new Map(model.nodes.map((node) => [node.id, node] as const));
  let crossings = 0;
  for (const channel of model.channels) {
    const a = byId.get(channel.a);
    const b = byId.get(channel.b);
    if (!a || !b) continue;
    const ma = moduleOf(a);
    const mb = moduleOf(b);
    // An unattributed end cannot be said to cross anything.
    if (ma === null || mb === null) continue;
    if (ma !== mb) crossings += 1;
  }
  return crossings;
}

/* ---- the legend row ------------------------------------------------------ */

/**
 * The six channels this field actually draws, each with what it is carrying
 * right now.
 *
 * The approved sheet's sixth panel is `Underlay` — sheet 01's module relief,
 * dimmed behind the flow. This surface draws no relief, so that panel is not
 * here: a legend panel for a channel the renderer does not paint would be the
 * exact drift this module exists to prevent. Its slot goes to `Sill`, which
 * the field does draw and which the sheet folds into its `Width` caption.
 */
export function legendPanels(model: TraceModel): readonly LegendPanel[] {
  const capped = cappedQualifier(model);

  const callSites = model.channels.map((channel) => channel.calls);
  const degrees = model.nodes
    .map((node) => node.degree)
    .filter((degree): degree is number => degree !== null);
  const unmeasuredDegrees = model.nodes.length - degrees.length;

  const up = model.nodes.filter((node) => node.ring < 0).length;
  const down = model.nodes.filter((node) => node.ring > 0).length;
  const kinds = new Set(model.nodes.map((node) => node.kind));

  const mouths = model.nodes.filter((node) => (node.undrawnEdges ?? 0) > 0);
  const undrawn = mouths.reduce((sum, node) => sum + (node.undrawnEdges ?? 0), 0);

  const callRange = range(callSites);
  const degreeRange = range(degrees);

  return [
    {
      label: 'Channel width',
      teach: 'call sites on that one edge',
      reading:
        callRange === null
          ? absent('no calls edge was drawn on this frame')
          : measured(callRange, `across ${model.channels.length} ${plural(model.channels.length, 'channel', 'channels')}`),
      qualifier: capped,
      sample: 'channel',
    },
    {
      label: 'Sill width',
      teach: "the symbol's degree, straight off the payload",
      reading:
        degreeRange === null
          ? absent('no drawn row carried a degree')
          : measured(degreeRange, `over ${degrees.length} ${plural(degrees.length, 'symbol', 'symbols')}`),
      qualifier:
        unmeasuredDegrees > 0
          ? `${unmeasuredDegrees} without a degree — hollow sill at the floor width`
          : null,
      sample: 'sill',
    },
    {
      label: 'Row',
      // Named in the sheet's own words. Sheet 01 spends height on dependency
      // depth; this one does not, and says so rather than borrowing that axis.
      teach: 'hop distance from the focus — not elevation, not importance',
      reading: measured(`${up} ↑ / ${down} ↓`, `${model.coverage.drawn} drawn`),
      qualifier: null,
      sample: 'rows',
    },
    {
      label: 'Hue',
      teach: 'symbol kind, off the same arc as the connectivity spine',
      reading: measured(String(kinds.size), plural(kinds.size, 'kind', 'kinds')),
      qualifier: null,
      sample: 'hue',
    },
    {
      label: 'Membrane',
      teach: 'one type enclosure, from contains edges',
      reading: model.coverage.membranesAvailable
        ? measured(
            String(model.membranes.length),
            plural(model.membranes.length, 'enclosure', 'enclosures'),
          )
        : absent('the payload carried no contains edges'),
      qualifier: model.coverage.membranesAvailable ? null : 'no enclosure is drawn on this frame',
      sample: 'membrane',
    },
    {
      label: 'Dashed mouth',
      teach: 'edges this frame does not draw',
      reading:
        mouths.length === 0
          ? absent('every drawn symbol had all its edges drawn, or none carried a degree')
          : measured(String(undrawn), `at ${mouths.length} ${plural(mouths.length, 'symbol', 'symbols')}`),
      qualifier: null,
      sample: 'mouth',
    },
  ];
}
