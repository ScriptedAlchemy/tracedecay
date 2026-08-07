/**
 * CONNECTION — `GET /api/plugins/graph/path?from=&to=&max_depth=`.
 *
 * "Are these two symbols connected at all, and through what?" — over the WHOLE
 * indexed graph, in either direction, along every edge kind the index holds.
 *
 * This is deliberately not the question `CallChain` asks, and the two must not
 * be read as versions of each other. `CallChain` reads
 * `/api/plugins/graph/call-chain`, whose producer is `find_path_directed`: a
 * BFS that follows outgoing `calls` edges only, from a focus, among the symbols
 * already drawn on the trace field. This route's producer is `find_path`, a
 * bidirectional walk over all edge kinds between any two node IDs in the index.
 * So a symbol pair with no call chain can still be connected here — through an
 * import, a containment, a type reference — and that difference is the whole
 * reason both exist. Nothing below describes a result from this route as a call
 * path.
 *
 * Because the endpoints are not restricted to a drawn neighbourhood, they
 * cannot come from a dropdown of what is on screen; each is chosen by searching
 * the index through `/api/plugins/graph/search`, the same route the workspace's
 * own symbol search uses. Searches run on submit rather than per keystroke —
 * the graph store is heavy and a read per keypress would be a read nobody waits
 * for.
 *
 * Honesty. Three separate absences reach this surface and each keeps its own
 * words:
 *
 *   - `found: false` is a MEASUREMENT — the producer searched to `max_depth`
 *     and no route existed within it. It is not an error, and it is not proof
 *     that no route exists, because the search is depth-bounded. So a negative
 *     prints the depth it searched to, and never says "not connected".
 *   - A blocked read (stale generation, unavailable store, denied scope) is the
 *     envelope's own domain state and renders as one; it is not a negative
 *     result and must never be drawn as one.
 *   - An ID in `path` with no hydrated row in `nodes` is a hop that happened,
 *     reported by an index that could not name it. It renders as its raw ID
 *     rather than being dropped, because dropping it would silently shorten a
 *     route the producer measured.
 */
import { useState, type FormEvent } from 'react';
import { ArrowRight, ArrowLeft } from 'lucide-react';

import {
  GraphPathPayloadV1Schema,
  GraphSearchPayloadV1Schema,
  type GraphEdgeV1,
  type GraphNodeV1,
  type GraphPathPayloadV1,
} from '../../contracts/generated.ts';
import { ReadSection, envelopeReadState } from '../../ui/ReadSection.tsx';
import { useEnvelope } from '../../data/query/useEnvelope.ts';
import { elideStart } from '../../ui/format.ts';
import { kindColorVars } from '../../viz/graph/kindColor.ts';

const BASE = '/api/plugins/graph';

/** How many search hits an endpoint picker offers. A picker is a way to choose
 * one symbol, not a second results surface; the workspace's own search is
 * where a broad look belongs. */
const CANDIDATES = 6;

/** One end of the query: what was typed, what was searched for, and what was
 * chosen. The three are separate because they can legitimately disagree — a
 * chosen symbol stays chosen while the reader types a new search. */
interface Endpoint {
  readonly term: string;
  readonly submitted: string;
  readonly node: GraphNodeV1 | null;
}

const EMPTY: Endpoint = { term: '', submitted: '', node: null };

export function SymbolPath() {
  const [from, setFrom] = useState<Endpoint>(EMPTY);
  const [to, setTo] = useState<Endpoint>(EMPTY);
  const fromId = from.node?.id ?? '';
  const toId = to.node?.id ?? '';
  const ready = fromId !== '' && toId !== '' && fromId !== toId;

  // The payload schema is passed un-nullable on purpose. `fetchEnvelope`
  // decodes every envelope with `payloadSchema.nullable()` itself and turns a
  // null payload into the daemon's own domain state, so a body the graph could
  // not produce a projection for arrives as a blocked read rather than as a
  // ready envelope carrying nothing. Adding `.nullable()` here would only make
  // that unreachable case look reachable.
  const path = useEnvelope(
    ['graph', 'path', fromId, toId],
    `${BASE}/path?from=${encodeURIComponent(fromId)}&to=${encodeURIComponent(toId)}`,
    GraphPathPayloadV1Schema,
    { enabled: ready },
  );

  return (
    <section className="flex flex-col gap-1.5" aria-label="Connection between two symbols">
      <div className="flex items-center gap-1.5">
        <h3 className="td-legend">connection</h3>
        <span aria-hidden className="td-rule" />
      </div>
      <p className="text-3xs leading-snug text-text-muted">
        any route between two symbols anywhere in the index, in either direction and along
        every edge kind — not only calls
      </p>
      <EndpointPicker label="from" endpoint={from} onChange={setFrom} />
      <EndpointPicker label="to" endpoint={to} onChange={setTo} />
      {!ready ? (
        <p className="text-2xs leading-relaxed text-text-muted">
          {fromId !== '' && fromId === toId
            ? 'both ends are the same symbol; a route needs two'
            : 'choose a symbol at each end'}
        </p>
      ) : (
        <ReadSection
          title="Connection"
          chrome="panel"
          className="border-0"
          state={envelopeReadState(path.isPending, path.data, {
            loading: 'searching the graph for a route between these symbols',
            transport: 'the route search could not be read',
          })}
        >
          {(envelope) => <PathReading payload={envelope.payload} />}
        </ReadSection>
      )}
    </section>
  );
}

function EndpointPicker({
  label,
  endpoint,
  onChange,
}: {
  label: string;
  endpoint: Endpoint;
  onChange: (next: Endpoint) => void;
}) {
  const hits = useEnvelope(
    ['graph', 'search', 'path-endpoint', endpoint.submitted],
    `${BASE}/search?q=${encodeURIComponent(endpoint.submitted)}&limit=${CANDIDATES}`,
    GraphSearchPayloadV1Schema,
    { enabled: endpoint.submitted !== '' },
  );
  // A blocked search yields no candidates rather than an empty result set: the
  // "nothing matches" sentence below is gated on the read having completed, so
  // an unreachable daemon never reads as a symbol that does not exist.
  const results = hits.data?.outcome === 'envelope' ? hits.data.envelope.payload.results : [];
  const blocked = hits.data !== undefined && hits.data.outcome === 'transport';

  function submit(event: FormEvent) {
    event.preventDefault();
    onChange({ ...endpoint, submitted: endpoint.term.trim() });
  }

  return (
    <div className="flex flex-col gap-1">
      <form onSubmit={submit} className="flex items-center gap-1.5">
        <label className="flex min-w-0 flex-1 items-center gap-1.5">
          <span className="td-legend w-8 shrink-0 normal-case tracking-normal text-text-muted">
            {label}
          </span>
          <input
            type="search"
            value={endpoint.term}
            onChange={(event) => onChange({ ...endpoint, term: event.target.value })}
            aria-label={`Search for the ${label} symbol`}
            placeholder="search symbols, then press Enter"
            className="h-6 min-w-0 flex-1 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 px-1.5 text-2xs text-text-primary focus:border-accent/60 focus:outline-none"
          />
        </label>
      </form>
      {endpoint.node !== null ? (
        <p className="flex items-baseline gap-1.5 pl-[2.375rem] text-2xs">
          <span
            aria-hidden
            className="size-1.5 shrink-0 translate-y-[-1px] rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
            style={kindColorVars(endpoint.node.kind)}
          />
          <span className="td-value min-w-0 flex-1 truncate text-text-primary">
            {symbolName(endpoint.node)}
          </span>
          <button
            type="button"
            onClick={() => onChange({ ...endpoint, node: null })}
            className="td-legend shrink-0 text-text-muted hover:text-text-primary focus:outline-none focus:ring-1 focus:ring-accent/60"
          >
            clear
          </button>
        </p>
      ) : null}
      {endpoint.submitted !== '' && endpoint.node === null ? (
        hits.isPending ? (
          <p className="pl-[2.375rem] text-2xs text-state-loading">searching…</p>
        ) : blocked ? (
          <p className="pl-[2.375rem] text-2xs leading-relaxed text-state-unknown">
            the symbol search could not be read, so nothing is being offered here
          </p>
        ) : results.length === 0 ? (
          <p className="pl-[2.375rem] text-2xs leading-relaxed text-text-muted">
            no indexed symbol matches “{endpoint.submitted}”
          </p>
        ) : (
          <ul
            className="flex flex-col pl-[2.375rem]"
            aria-label={`Matches for the ${label} symbol`}
          >
            {results.map((node) => (
              <li key={node.id}>
                <button
                  type="button"
                  onClick={() => onChange({ ...endpoint, node })}
                  className="flex w-full min-w-0 items-baseline gap-1.5 py-0.5 text-left hover:bg-surface-2 focus:bg-surface-2 focus:outline-none"
                >
                  <span
                    aria-hidden
                    className="size-1.5 shrink-0 translate-y-[-1px] rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
                    style={kindColorVars(node.kind)}
                  />
                  <span className="td-value min-w-0 flex-1 truncate text-2xs text-text-primary">
                    {symbolName(node)}
                  </span>
                  <span
                    className="td-value w-28 shrink-0 truncate text-right text-3xs text-text-muted"
                    title={node.file_path ?? undefined}
                  >
                    {node.file_path === null ? '' : elideStart(node.file_path, 18)}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        )
      ) : null}
    </div>
  );
}

function PathReading({ payload }: { payload: GraphPathPayloadV1 }) {
  if (!payload.found) {
    return (
      <p className="py-1 text-2xs leading-relaxed text-state-unknown">
        no route within {payload.max_depth} hops, searched in either direction along every
        edge kind. A longer route is not excluded — the search is depth-bounded, so this is
        a measurement at that depth and not a statement that the two are unconnected.
      </p>
    );
  }
  const hops = Math.max(payload.path.length - 1, 0);
  const byId = new Map(payload.nodes.map((node) => [node.id, node]));
  return (
    <div className="flex flex-col gap-1 py-1">
      <p className="text-3xs leading-snug text-text-secondary">
        {hops} {hops === 1 ? 'hop' : 'hops'} · shortest route found within {payload.max_depth}
        , undirected, any edge kind
      </p>
      <ol className="flex flex-col">
        {payload.path.map((id, index) => {
          const node = byId.get(id);
          const incoming = index === 0 ? null : hop(payload.edges, payload.path[index - 1], id);
          return (
            <li key={`${id}-${index}`} className="flex min-w-0 items-baseline gap-1.5 py-0.5">
              {incoming === null ? (
                <span aria-hidden className="w-3 shrink-0" />
              ) : (
                <HopArrow edge={incoming} />
              )}
              <span
                aria-hidden
                className="size-1.5 shrink-0 translate-y-[-1px] rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
                style={kindColorVars(node?.kind ?? 'unknown')}
              />
              <span className="td-value min-w-0 flex-1 truncate text-2xs text-text-primary">
                {node === undefined ? id : symbolName(node)}
              </span>
              {incoming !== null ? (
                <span className="td-legend shrink-0">{incoming.edge.kind}</span>
              ) : null}
              <span
                className="td-value w-32 shrink-0 truncate text-right text-3xs text-text-muted"
                title={node?.file_path ?? undefined}
              >
                {/* An unhydrated hop has no file to name, and says nothing
                  * rather than borrowing a neighbour's. */}
                {node?.file_path == null ? '' : elideStart(node.file_path, 20)}
              </span>
            </li>
          );
        })}
      </ol>
    </div>
  );
}

/** The edge that carried one hop, with the direction it actually runs in.
 *
 * The producer walks bidirectionally, so consecutive path entries can be joined
 * by an edge pointing either way, and which way it points changes what the hop
 * means: `a → b` on a `calls` edge is "a calls b", and the reverse edge on the
 * same pair is "b calls a". Rendering both as one undirected line would state
 * a relationship neither the index nor the producer claims. */
function hop(
  edges: readonly GraphEdgeV1[],
  previous: string | undefined,
  current: string,
): { edge: GraphEdgeV1; forward: boolean } | null {
  if (previous === undefined) return null;
  const forward = edges.find((edge) => edge.source === previous && edge.target === current);
  if (forward) return { edge: forward, forward: true };
  const backward = edges.find((edge) => edge.source === current && edge.target === previous);
  return backward ? { edge: backward, forward: false } : null;
}

function HopArrow({ edge }: { edge: { edge: GraphEdgeV1; forward: boolean } }) {
  const Icon = edge.forward ? ArrowRight : ArrowLeft;
  return (
    <Icon
      aria-label={
        edge.forward
          ? `${edge.edge.kind} edge running forward along the route`
          : `${edge.edge.kind} edge running backward along the route`
      }
      size={10}
      className="w-3 shrink-0 translate-y-[1px] text-text-muted"
    />
  );
}

/** A symbol's name as the index holds it. `name` is nullable on the wire, and a
 * node without one is shown by its ID rather than by an invented label. */
function symbolName(node: GraphNodeV1): string {
  return node.name ?? node.qualified_name ?? node.id;
}
