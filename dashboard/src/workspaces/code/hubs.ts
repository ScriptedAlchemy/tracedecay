/**
 * Two captions the Code workspace was missing, as pure functions.
 *
 * The graph canvas drew eighty nodes out of 118,672 and said nothing about how
 * those eighty were chosen — which makes the picture unreadable, because
 * "eighty highest-degree symbols" and "eighty neighbours of one symbol" are
 * completely different claims about the same drawing. `describeSubgraph`
 * states the endpoint's actual rule for whichever mode it answered in.
 *
 * And the hub field ranked twelve symbols by connectivity whose names were
 * `path`, `json`, `u64`, `Value`, `trim`, `as_str`, `kind` and `i64` — two of
 * them literally the same word. The endpoint does not serve `qualified_name`
 * for these rows, so the only thing that can tell them apart is the file they
 * live in, and that was set three type steps down at the far right of the card
 * where it read as decoration.
 */

export interface HubRow {
  id?: string | undefined;
  name?: string | null | undefined;
  qualified_name?: string | null | undefined;
  kind?: string | undefined;
  file_path?: string | null | undefined;
  degree?: number | null | undefined;
}

export interface AnnotatedHub<T extends HubRow> {
  hub: T;
  /** The name shown as the card's headline. */
  display: string;
  /** Directory the symbol lives in, with a trailing slash, or '' at the root. */
  module: string;
  /** File name alone. */
  file: string;
  /** Another hub in the same set carries this exact display name. */
  ambiguous: boolean;
}

/** The headline a symbol is shown under, and the one place that decides it.
 *
 * `name` first: `qualified_name` is not served by the hub endpoint at all, so
 * the chain is honest about what the payload actually carries. The em dash is
 * the end of the chain rather than a separate absent case, so a row with no
 * name and a row that is not there read the same way — which they should,
 * because neither can be named. `AnnotatedHub.display` is this same string
 * precomputed; call sites holding an annotated row should read it from there
 * rather than recompute it. */
export function displayName(hub: HubRow | null | undefined): string {
  return hub?.name ?? hub?.qualified_name ?? hub?.id ?? '—';
}

/**
 * Split each hub's file path into module and file, and flag the names that
 * repeat inside the set.
 *
 * The ambiguity flag is deliberately scoped to THIS set and not to the graph:
 * the payload carries twelve rows and no occurrence counts, so "two of these
 * twelve are called `path`" is the strongest honest claim available. Anything
 * about how common `path` is across all 118,672 nodes would be invented.
 */
export function annotateHubs<T extends HubRow>(hubs: readonly T[]): AnnotatedHub<T>[] {
  const counts = new Map<string, number>();
  for (const hub of hubs) {
    const name = displayName(hub);
    counts.set(name, (counts.get(name) ?? 0) + 1);
  }
  return hubs.map((hub) => {
    const path = hub.file_path ?? '';
    const cut = path.lastIndexOf('/');
    const display = displayName(hub);
    return {
      hub,
      display,
      module: cut >= 0 ? path.slice(0, cut + 1) : '',
      file: cut >= 0 ? path.slice(cut + 1) : path,
      ambiguous: (counts.get(display) ?? 0) > 1,
    };
  });
}

/** How many distinct names in the set are carried by more than one hub, and by
 * how many rows in total. Null when every name is unique. */
export function ambiguityNote<T extends HubRow>(
  annotated: readonly AnnotatedHub<T>[],
): string | null {
  const repeated = new Map<string, number>();
  for (const row of annotated) {
    if (!row.ambiguous) continue;
    repeated.set(row.display, (repeated.get(row.display) ?? 0) + 1);
  }
  if (repeated.size === 0) return null;
  const names = [...repeated.entries()]
    .sort((a, b) => b[1] - a[1] || a[0].localeCompare(b[0]))
    .map(([name, count]) => `${count}×${name}`)
    .join(', ');
  const rows = [...repeated.values()].reduce((sum, count) => sum + count, 0);
  return `${rows} of these ${annotated.length} share a name (${names}) — the file under each one is what tells them apart.`;
}

export interface SubgraphMeta {
  mode?: string | undefined;
  seed_id?: string | null | undefined;
  nodes: readonly unknown[];
  edges: readonly unknown[];
  capped?: { nodes?: boolean; edges?: boolean } | undefined;
  limits?: { nodes?: number; edges?: number } | undefined;
}

export interface SubgraphCaption {
  /** "80 of 118,672 symbols · 120 edges". */
  scale: string;
  /** The endpoint's actual selection rule, in a sentence. */
  rule: string;
  /** True when the endpoint reported it had more to give. */
  capped: boolean;
}

/**
 * State exactly how this slice of the graph was chosen.
 *
 * Both branches are read off `graph_service.rs::subgraph_payload`:
 *
 *   default   No seed. The candidate pool is a prefix of the cached top-degree
 *             summary (twice the node budget), and selection walks it greedily,
 *             preferring a candidate that touches something already chosen and
 *             falling back to the next highest-degree node when none does. So
 *             the slice is the busiest region of the graph, grown to stay
 *             connected — NOT simply the top 80 by degree, which is a
 *             different and disconnected set.
 *
 *   seeded    A node id (or the first hit for a query). The slice is that node
 *             plus its direct in-neighbours, then its direct out-neighbours,
 *             ordered by that rank and truncated at the node budget. Depth one
 *             only.
 */
export function describeSubgraph(
  payload: SubgraphMeta | undefined,
  totalNodes: number | null | undefined,
  seedLabel?: string | null,
): SubgraphCaption | null {
  if (!payload) return null;
  const nodes = payload.nodes.length;
  const edges = payload.edges.length;
  if (nodes === 0) {
    return {
      scale: 'no nodes',
      rule:
        payload.mode === 'seeded'
          ? 'The query matched no symbol, so no neighbourhood was composed.'
          : 'The graph returned no slice.',
      capped: false,
    };
  }
  const total =
    totalNodes != null && Number.isFinite(totalNodes) ? totalNodes.toLocaleString() : null;
  const scale = `${nodes.toLocaleString()}${total ? ` of ${total}` : ''} symbols · ${edges.toLocaleString()} edges`;
  const capped = Boolean(payload.capped?.nodes || payload.capped?.edges);
  const rule =
    payload.mode === 'seeded'
      ? `${seedLabel ? `${seedLabel} and its` : 'The selected symbol and its'} direct neighbours — everything one edge away, in-edges first, cut at ${payload.limits?.nodes ?? nodes}.`
      : `Unseeded: the graph's busiest region, grown by adjacency from the highest-degree symbols so the slice stays connected — not the top ${nodes} by degree, which would not be.`;
  return { scale, rule, capped };
}
