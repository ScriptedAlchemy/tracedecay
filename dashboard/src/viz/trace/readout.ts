/**
 * The header readout strip for the TRACE surface, counted from the one
 * `TraceModel` the anatomy plate is drawn from, so no cell is a literal a
 * payload change could falsify.
 *
 * The house rule from the design note, "every position, size, elevation and
 * width encodes a stated measurement", has a corollary this file exists to
 * enforce: a measurement that did not arrive is *printed as absent*, never
 * blanked and never defaulted to zero. `ReadoutValue` has no third state for
 * that reason. A caller cannot render a cell without having decided what it
 * says when the wire was silent.
 *
 * Pure by construction: types only, no DOM, no colour, no clock.
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
   * Disclosure that the reading is a floor rather than a total, a list that
   * came back at the endpoint's limit, or a neighbour that was never expanded.
   * `null` when the number is complete as far as this frame can know.
   */
  readonly qualifier: string | null;
}

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
 * The module a symbol belongs to: the directory of its `file_path`.
 *
 * A symbol whose row carried no path has no module *on this wire*, which is
 * not the same as belonging to none, so it returns null and is counted as
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
  return `a list hit the ${cappedAt ?? 'row'} limit, a floor, not a total`;
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
          ? `${coverage.unexpandedNeighbors} ${plural(coverage.unexpandedNeighbors, 'neighbour', 'neighbours')} unexpanded, past them is unknown, not zero`
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
