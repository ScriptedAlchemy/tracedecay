import { useEffect, useMemo, useRef, useState } from "react";
import { RefreshCw, Sparkles, X } from "lucide-react";
import { Badge, Button, Card, CardContent, CardHeader, CardTitle } from "./sdk";
import { api } from "./api";
import { PanelError, PanelLoading } from "./viz/Status";
import type {
  MemorySimilarityResponse,
  MemorySimilarityPair,
} from "./types";
import { NUM_BADGE } from "./ui";
import { MiniBar } from "./MiniBar";
import { binValues } from "./viz/scale";
import { diffPair } from "./viz/textDiff";
import { BrushableHistogram } from "./viz/Histogram";

/**
 * The pair set is fetched ONCE per similarity floor; every score/class filter
 * is client-side (brushable histogram + chips). The old slider refetched on
 * every onChange tick, queueing dozens of O(n²·d) server computations per
 * drag. The only refetch is a single debounced request when the user brushes
 * BELOW the currently loaded floor (newer servers honor `min_similarity`).
 */
const DEFAULT_FLOOR = 0.5;
const FETCH_LIMIT = 2000;
const PAGE_SIZE = 40;
/** Debounce for the brush-below-floor deep fetch (fires on brush settle). */
const DEEP_FETCH_DEBOUNCE_MS = 350;

type Metric = "hrr" | "token";

const METRIC_LABEL: Record<Metric, string> = {
  hrr: "HRR similarity",
  token: "token overlap",
};

function metricValue(pair: MemorySimilarityPair, metric: Metric): number {
  return metric === "hrr" ? pair.similarity : (pair.token_overlap ?? 0);
}

const CLASSIFICATIONS = [
  { key: "likely_duplicate", label: "Likely duplicate" },
  { key: "merge_candidate", label: "Merge candidate" },
  { key: "related", label: "Related" },
] as const;

function classificationLabel(classification: string) {
  return (
    CLASSIFICATIONS.find((c) => c.key === classification)?.label ?? "Related"
  );
}

function classificationClass(classification: string) {
  if (classification === "likely_duplicate") {
    return "border-destructive/30 bg-destructive/10 text-destructive";
  }
  if (classification === "merge_candidate") {
    return "border-primary/30 bg-primary/10 text-primary";
  }
  return "border-border bg-muted/40 text-text-secondary";
}

/* ----------------------------------------------------------- diff fact card */

function DiffCard({
  id,
  category,
  tokens,
  side,
}: {
  id: number;
  category: string;
  tokens: ReturnType<typeof diffPair>["a"];
  side: "a" | "b";
}) {
  return (
    <div className={`hv-diff-card hv-diff-${side}`}>
      <div className="mb-1 flex min-w-0 items-center gap-2">
        <span className="font-mono-ui text-xs text-text-tertiary">#{id}</span>
        <Badge tone="secondary" className={NUM_BADGE}>
          {category}
        </Badge>
      </div>
      <p className="hv-diff-text">
        {tokens.map((token, i) =>
          token.kind === "shared" ? (
            <mark key={i} className="hv-token-shared">
              {token.text}
            </mark>
          ) : token.kind === "unique" ? (
            <span key={i} className="hv-token-unique">
              {token.text}
            </span>
          ) : (
            token.text
          ),
        )}
      </p>
    </div>
  );
}

function PairRow({
  pair,
  onShowOnMap,
}: {
  pair: MemorySimilarityPair;
  onShowOnMap?: (pair: MemorySimilarityPair) => void;
}) {
  const diff = useMemo(
    () => diffPair(pair.a_content, pair.b_content),
    [pair.a_content, pair.b_content],
  );
  const tokenPct = Math.round(
    Math.max(0, Math.min(1, pair.token_overlap ?? 0)) * 100,
  );

  return (
    <div className="hv-pair border border-border p-3">
      <div className="mb-2 flex min-w-0 flex-wrap items-center gap-2">
        <span
          className={`border px-2 py-0.5 text-[10px] font-medium uppercase tracking-[0.08em] ${classificationClass(pair.classification)}`}
        >
          {classificationLabel(pair.classification)}
        </span>
        <span className="font-mono-ui text-xs text-text-tertiary">
          #{pair.a_id} ↔ #{pair.b_id}
        </span>
        <span className="ml-auto flex items-center gap-3 font-mono-ui text-xs text-text-secondary">
          <span title="HRR phase-cosine similarity">
            HRR {pair.similarity.toFixed(4)}
          </span>
          <span title="Lexical token overlap">tok {tokenPct}%</span>
          {onShowOnMap && (
            <Button
              ghost
              size="xs"
              className="gap-1"
              onClick={() => onShowOnMap(pair)}
              title="Select and zoom to both facts on the Semantic Map"
              aria-label={`Show facts ${pair.a_id} and ${pair.b_id} on the Semantic Map`}
            >
              <Sparkles className="h-3 w-3" />
              map
            </Button>
          )}
        </span>
      </div>
      <div className="mb-2 grid grid-cols-2 gap-2">
        <MiniBar pct={Math.round(Math.max(0, Math.min(1, pair.similarity)) * 100)} />
        <MiniBar pct={tokenPct} color="var(--hm-cat-1)" />
      </div>
      <div className="grid gap-2 sm:grid-cols-2">
        <DiffCard
          id={pair.a_id}
          category={pair.a_category}
          tokens={diff.a}
          side="a"
        />
        <DiffCard
          id={pair.b_id}
          category={pair.b_category}
          tokens={diff.b}
          side="b"
        />
      </div>
      <p className="mt-2 font-mono-ui text-[0.65rem] text-text-tertiary">
        shared {pair.shared_token_count ?? 0}/
        {Math.min(pair.a_token_count ?? 0, pair.b_token_count ?? 0)} tokens ·
        highlighted words appear in both facts
      </p>
    </div>
  );
}

/* ----------------------------------------------------------------- panel */

export default function SimilarityPanel({
  onShowOnMap,
}: {
  /** Cross-view navigation: select + zoom to a pair on the Semantic Map. */
  onShowOnMap?: (pair: MemorySimilarityPair) => void;
}) {
  const [data, setData] = useState<MemorySimilarityResponse | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState("");
  const [metric, setMetric] = useState<Metric>("hrr");
  const [brush, setBrush] = useState<[number, number] | null>(null);
  const [classFilter, setClassFilter] = useState<string | null>(null);
  const [visibleCount, setVisibleCount] = useState(PAGE_SIZE);
  const [reloadKey, setReloadKey] = useState(0);
  const [floor, setFloor] = useState(DEFAULT_FLOOR);
  // The sentinel is held as state (callback ref) so the observer effect
  // re-runs when the node remounts — e.g. after scrolling to the end (sentinel
  // unmounts) and then re-filtering to a list of the same length.
  const [sentinel, setSentinel] = useState<HTMLDivElement | null>(null);
  const deepFetchTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError("");
    api
      .getMemorySimilarity({ minSimilarity: floor, limit: FETCH_LIMIT })
      .then((resp) => {
        if (!cancelled) setData(resp);
      })
      .catch((err) => {
        if (!cancelled) setError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [reloadKey, floor]);

  const pairs = useMemo(() => data?.pairs ?? [], [data]);

  // Brushing below the loaded floor triggers ONE debounced deeper fetch once
  // the brush settles; everything else stays client-side.
  useEffect(() => {
    if (deepFetchTimer.current) clearTimeout(deepFetchTimer.current);
    if (metric !== "hrr" || !brush) return;
    const loadedFloor = data?.min_similarity ?? floor;
    const wanted = Math.max(-1, brush[0]);
    if (wanted < loadedFloor - 0.005) {
      deepFetchTimer.current = setTimeout(() => {
        setFloor((prev) => Math.min(prev, wanted));
      }, DEEP_FETCH_DEBOUNCE_MS);
    }
    return () => {
      if (deepFetchTimer.current) clearTimeout(deepFetchTimer.current);
    };
  }, [brush, metric, data, floor]);

  const classCounts = useMemo(() => {
    const counts = new Map<string, number>();
    for (const pair of pairs) {
      counts.set(pair.classification, (counts.get(pair.classification) ?? 0) + 1);
    }
    return counts;
  }, [pairs]);

  const metricValues = useMemo(
    () => pairs.map((pair) => metricValue(pair, metric)),
    [pairs, metric],
  );

  /**
   * HRR histogram prefers the server's full-population distribution (ALL
   * computed pairs, not just the returned page). When that distribution is
   * degenerate at the server's fixed bin width (e.g. every pair lands in one
   * 0.1-wide bin), or for older servers / the token metric, fall back to fine
   * client-side bins over the loaded pairs' full-precision scores.
   */
  const distribution = metric === "hrr" ? (data?.score_distribution ?? null) : null;
  const bins = useMemo(() => {
    if (distribution?.bins?.length) {
      const all = distribution.bins.map((b) => ({
        x0: b.start,
        x1: b.end,
        count: b.count,
      }));
      let first = all.findIndex((b) => b.count > 0);
      let last = all.length - 1;
      while (last >= 0 && all[last].count === 0) last -= 1;
      if (first !== -1) {
        const trimmed = all.slice(Math.max(0, first - 1), last + 2);
        if (trimmed.filter((b) => b.count > 0).length > 2) return trimmed;
      }
    }
    return metricValues.length > 0 ? binValues(metricValues, 28) : [];
  }, [distribution, metricValues]);

  const filtered = useMemo(() => {
    let out = pairs;
    if (classFilter) out = out.filter((p) => p.classification === classFilter);
    if (brush) {
      out = out.filter((p) => {
        const v = metricValue(p, metric);
        return v >= brush[0] && v <= brush[1];
      });
    }
    return out;
  }, [pairs, classFilter, brush, metric]);

  // Incremental rendering: cards are variable-height, so instead of strict
  // windowing we render a slice and grow it when the sentinel scrolls into
  // view; `content-visibility` on each card skips offscreen layout work.
  useEffect(() => {
    setVisibleCount(PAGE_SIZE);
  }, [filtered]);

  useEffect(() => {
    if (!sentinel) return;
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        setVisibleCount((prev) =>
          prev < filtered.length ? Math.min(filtered.length, prev + PAGE_SIZE) : prev,
        );
      }
    });
    observer.observe(sentinel);
    return () => observer.disconnect();
  }, [sentinel, filtered.length]);

  const fmt = (v: number) => (metric === "hrr" ? v.toFixed(4) : `${Math.round(v * 100)}%`);

  return (
    <Card>
      <CardHeader>
        <CardTitle>Similar Pairs</CardTitle>
      </CardHeader>
      <CardContent>
        <p className="mb-3 text-xs text-text-tertiary">
          HRR similarity compares holographic vector phase patterns; lexical
          overlap separates related facts from merge candidates and likely
          duplicates. Drag across the histogram to filter by score range.
        </p>

        {loading ? (
          <PanelLoading label="Comparing fact vectors…" />
        ) : error ? (
          <PanelError error={error} onRetry={() => setReloadKey((k) => k + 1)} />
        ) : (
          <>
            <div className="mb-3 flex min-w-0 flex-wrap items-center gap-2">
              <div className="flex shrink-0 border border-border">
                {(Object.keys(METRIC_LABEL) as Metric[]).map((m) => (
                  <Button
                    key={m}
                    ghost={metric !== m}
                    size="xs"
                    onClick={() => {
                      setMetric(m);
                      setBrush(null);
                    }}
                  >
                    {METRIC_LABEL[m]}
                  </Button>
                ))}
              </div>
              {CLASSIFICATIONS.map(({ key, label }) => {
                const count = classCounts.get(key) ?? 0;
                const active = classFilter === key;
                return (
                  <button
                    key={key}
                    type="button"
                    className={`hv-chip${active ? " hv-chip-active" : ""}`}
                    aria-pressed={active}
                    onClick={() => setClassFilter(active ? null : key)}
                  >
                    {label} <span className="hv-legend-count">{count}</span>
                  </button>
                );
              })}
              <span className="ml-auto flex items-center gap-2 font-mono-ui text-xs text-text-tertiary">
                {filtered.length}/{pairs.length} loaded
                {data?.total_pairs != null && data.total_pairs > pairs.length
                  ? ` of ${data.total_pairs.toLocaleString()} computed`
                  : ""}{" "}
                · {data?.count ?? 0} facts
                <Button
                  ghost
                  size="xs"
                  onClick={() => setReloadKey((k) => k + 1)}
                  aria-label="Recompute pairs"
                  title="Recompute pairs"
                >
                  <RefreshCw />
                </Button>
              </span>
            </div>

            {pairs.length > 0 && (
              <div className="mb-3 border border-border bg-background/30 p-2">
                <div className="mb-1 flex flex-wrap items-center justify-between gap-2 font-mono-ui text-[0.65rem] text-text-tertiary">
                  <span>
                    {METRIC_LABEL[metric]} distribution
                    {distribution
                      ? ` · all ${distribution.total_pairs.toLocaleString()} pairs · avg ${distribution.average_score.toFixed(4)}`
                      : ` · ${pairs.length} loaded pairs`}
                  </span>
                  {brush ? (
                    <button
                      type="button"
                      className="hv-chip"
                      onClick={() => setBrush(null)}
                    >
                      {fmt(brush[0])} – {fmt(brush[1])} · clear{" "}
                      <X className="h-2.5 w-2.5" />
                    </button>
                  ) : (
                    <span>drag to filter{metric === "hrr" ? " · brush low to load deeper" : ""}</span>
                  )}
                </div>
                <BrushableHistogram
                  bins={bins}
                  height={92}
                  brush={brush}
                  onBrush={setBrush}
                  format={fmt}
                  tipLabel="pairs"
                />
              </div>
            )}

            {filtered.length === 0 ? (
              <div className="border border-border bg-background/30 p-4 text-xs text-text-tertiary">
                <p className="text-text-secondary">
                  No fact pairs match the current filters.
                </p>
                <p className="mt-1">
                  Clear the histogram brush or classification filter to surface
                  more pairs.
                </p>
              </div>
            ) : (
              <div className="flex max-h-[36rem] flex-col gap-2 overflow-y-auto pr-1">
                {filtered.slice(0, visibleCount).map((pair) => (
                  <PairRow
                    key={`${pair.a_id}-${pair.b_id}`}
                    pair={pair}
                    onShowOnMap={onShowOnMap}
                  />
                ))}
                {visibleCount < filtered.length && (
                  <div
                    ref={setSentinel}
                    className="py-2 text-center font-mono-ui text-[0.65rem] text-text-tertiary"
                  >
                    {filtered.length - visibleCount} more…
                  </div>
                )}
              </div>
            )}
          </>
        )}
      </CardContent>
    </Card>
  );
}
