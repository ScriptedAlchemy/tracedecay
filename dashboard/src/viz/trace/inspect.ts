/**
 * Geometry-free rules the Trace plate and its interaction share: channel
 * identity, the route a hover or keyboard focus lights, and the one line the
 * inspect readout prints. Nothing here reads a payload.
 */
import { coverageCaption } from './model.ts';
import type { TraceChannel, TraceModel } from './types.ts';

export function channelKey(channel: TraceChannel): string {
  return `${channel.a}\0${channel.b}`;
}

/** The drawn channel between two symbols, in either direction. */
export function channelBetween(
  model: TraceModel,
  a: string,
  b: string,
): TraceChannel | undefined {
  return model.channels.find(
    (channel) => (channel.a === a && channel.b === b) || (channel.a === b && channel.b === a),
  );
}

export interface InspectedPath {
  readonly nodes: ReadonlySet<string>;
  readonly channels: ReadonlySet<string>;
}

/**
 * What a hover or keyboard focus on `id` lights: every drawn route from it to
 * the focus that steps strictly inward one hop at a time, plus the channels
 * incident on it. Only drawn channels are walked, so a lit path is a path the
 * reader can see.
 */
export function inspectPath(model: TraceModel, id: string | null): InspectedPath {
  const nodes = new Set<string>();
  const channels = new Set<string>();
  if (id === null) return { nodes, channels };
  const ringOf = new Map(model.nodes.map((node) => [node.id, Math.abs(node.ring)]));
  if (!ringOf.has(id)) return { nodes, channels };
  nodes.add(id);
  for (const channel of model.channels) {
    if (channel.a === id || channel.b === id) {
      channels.add(channelKey(channel));
      nodes.add(channel.a === id ? channel.b : channel.a);
    }
  }
  let frontier = [id];
  while (frontier.length > 0) {
    const next: string[] = [];
    for (const current of frontier) {
      const hop = ringOf.get(current)!;
      for (const channel of model.channels) {
        const other = channel.a === current ? channel.b : channel.b === current ? channel.a : null;
        if (other === null || ringOf.get(other) !== hop - 1) continue;
        channels.add(channelKey(channel));
        if (!next.includes(other)) next.push(other);
        nodes.add(other);
      }
    }
    frontier = next;
  }
  return { nodes, channels };
}

/** One line the inspect readout prints for a symbol; `absent` is printed. */
export function inspectLine(model: TraceModel, id: string): string {
  const node = model.nodes.find((candidate) => candidate.id === id);
  if (!node) return '';
  const hop =
    node.ring === 0
      ? 'focus'
      : `${Math.abs(node.ring)} ${Math.abs(node.ring) === 1 ? 'hop' : 'hops'} ${node.ring < 0 ? 'up (caller side)' : 'down (callee side)'}`;
  const drawn = model.channels
    .filter((channel) => channel.a === id || channel.b === id)
    .reduce((sum, channel) => sum + channel.calls, 0);
  const place =
    node.filePath === null
      ? 'file absent'
      : `${node.filePath}${node.startLine === null ? '' : `:${node.startLine}`}`;
  return [
    node.name,
    node.kind,
    hop,
    `${drawn} drawn call ${drawn === 1 ? 'site' : 'sites'}`,
    node.degree === null ? 'degree absent' : `degree ${node.degree}`,
    node.undrawnEdges === null ? 'undrawn edges absent' : `${node.undrawnEdges} edges not drawn`,
    place,
  ].join(' · ');
}

/** The plate's accessible description: what is drawn, and the coverage caption. */
export function plateDescription(model: TraceModel): string {
  const focus = model.nodes.find((node) => node.id === model.focusId);
  const up = model.nodes.filter((node) => node.ring < 0).length;
  const down = model.nodes.filter((node) => node.ring > 0).length;
  const sites = model.channels.reduce((sum, channel) => sum + channel.calls, 0);
  return (
    `Call neighbourhood of ${focus?.name ?? model.focusId} as an anatomy plate, callers left and callees right on one call-site scale. ` +
    `${up} calling and ${down} called symbols, joined by ${model.channels.length} channels carrying ${sites} call sites. ` +
    `${coverageCaption(model)}. Each symbol is a focusable control; the ranked list below carries the same symbols as text.`
  );
}

/** Truncate from the end with an ellipsis, for a fixed label budget. */
export function clip(text: string, max: number): string {
  return text.length <= max ? text : `${text.slice(0, Math.max(1, max - 1))}…`;
}

/** Truncate from the start, for paths whose tail is the identifying part. */
export function clipStart(text: string, max: number): string {
  return text.length <= max ? text : `…${text.slice(text.length - Math.max(1, max - 1))}`;
}
