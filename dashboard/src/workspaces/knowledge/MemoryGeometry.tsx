/**
 * MEMORY GEOMETRY — the store's semantic space, and what it implies.
 *
 * Two routes that inspect the same query-time encoding method over their own
 * admitted fact sets. `/projection` may filter and cap facts before deriving
 * FHRR phase encodings and decomposing `[cos(p), sin(p)]` coordinates into two
 * principal components. `/similarity` derives encodings for its own bounded
 * fact set and scores them by phase cosine. They are drawn together because
 * both expose relationships in the canonical fact store without persisting a
 * vector authority.
 *
 * The load-bearing honesty here is `method`. The daemon emits `"pca"` only when
 * it actually decomposed at least two equal-width encodings. One eligible fact
 * returns one origin point; no encodable facts or a failed decomposition return
 * no points. A scatter drawn from `none` would misstate either condition as a
 * real projection, so this surface refuses to draw one and says why instead.
 *
 * Likewise the similarity panel prints three denominators rather than one.
 * `count` is query-time encoded facts, `total_pairs` is every finite pair the
 * computation scored, and `pairs.length` is the bounded list returned for this
 * request's floor. Reporting any one as "the store" misstates the other two.
 */
import { useMemo, useState } from "react";
import type { EChartsOption } from "echarts";

import { PayloadBoundary } from "../../ui/ReadSection.tsx";
import { Panel, Readout } from "../../ui/instrument.tsx";
import { StateChip } from "../../ui/StateChip.tsx";
import { Chart } from "../../viz/chart/Chart.tsx";
import { SearchField } from "../../ui/search/SearchField.tsx";
import {
  useMemoryProjection,
  useMemorySimilarity,
  type ProjectionPayload,
  type SimilarityPayload,
} from "../../data/query/memory.ts";
import { projectionReading, similarityReading } from "./memoryModel.ts";

/**
 * The similarity floors this panel offers.
 *
 * Discrete rather than a slider: the daemon can reuse its store-fingerprinted
 * sorted pair prefix, but a continuous control would still issue one request
 * per pixel of drag. The values are representative phase-cosine cutoffs from a
 * strict 0.95 near-match view to a broader 0.6 relationship view.
 */
const FLOORS = [0.95, 0.85, 0.75, 0.6] as const;

export function MemoryGeometry() {
  const [query, setQuery] = useState("");
  const [applied, setApplied] = useState("");
  const [floor, setFloor] = useState<number>(0.85);
  const projection = useMemoryProjection(applied);
  const similarity = useMemorySimilarity(floor);

  return (
    <div className="flex min-h-0 flex-col gap-3 p-3">
      <Panel legend="Phase projection" elevation="well">
        <div className="flex flex-col gap-3">
          <SearchField
            value={query}
            onChange={setQuery}
            onSubmit={() => setApplied(query.trim())}
            onClear={() => {
              setQuery("");
              setApplied("");
            }}
            label="Restrict the projection"
            placeholder="Restrict the projection"
            hint="an empty query projects a bounded page of query-time encoded facts"
            submitted={applied}
          />
          <PayloadBoundary
            title="Phase projection"
            pending={projection.isPending}
            result={projection.data}
          >
            {(data) => <ProjectionBody data={data} />}
          </PayloadBoundary>
        </div>
      </Panel>
      <Panel
        legend="Pairwise similarity"
        actions={<FloorControl floor={floor} onSelect={setFloor} />}
        elevation="well"
      >
        <PayloadBoundary
          title="Pairwise similarity"
          pending={similarity.isPending}
          result={similarity.data}
        >
          {(data) => <SimilarityBody data={data} />}
        </PayloadBoundary>
      </Panel>
    </div>
  );
}

function FloorControl({
  floor,
  onSelect,
}: {
  floor: number;
  onSelect: (value: number) => void;
}) {
  return (
    <div
      role="group"
      aria-label="Minimum similarity"
      className="flex shrink-0 flex-wrap items-center gap-1"
    >
      {FLOORS.map((value) => (
        <button
          key={value}
          type="button"
          aria-pressed={value === floor}
          onClick={() => onSelect(value)}
          className={
            value === floor
              ? "td-hit border border-edge-strong bg-surface-3 px-2 text-3xs text-text-primary"
              : "td-hit border border-edge-subtle px-2 text-3xs text-text-secondary hover:bg-surface-2"
          }
        >
          ≥ {value.toFixed(2)}
        </button>
      ))}
    </div>
  );
}

function ProjectionBody({ data }: { data: ProjectionPayload }) {
  const reading = projectionReading(data);
  const coverageComplete = data.coverage.completeness === "complete";
  const option = useMemo<EChartsOption>(
    () => ({
      xAxis: {
        type: "value",
        axisLabel: { show: false },
        axisTick: { show: false },
      },
      yAxis: {
        type: "value",
        axisLabel: { show: false },
        axisTick: { show: false },
      },
      grid: { left: 6, right: 6, top: 8, bottom: 6, containLabel: true },
      series: [
        {
          type: "scatter",
          symbolSize: 5,
          data: reading.points.map((point) => [point.x, point.y]),
        },
      ],
    }),
    [reading.points],
  );
  if (data.error !== "") {
    return (
      <p role="status" className="text-2xs leading-relaxed text-state-error">
        the projection could not be computed: {data.error}
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      <p className="text-2xs leading-relaxed text-text-secondary">
        {reading.note}
      </p>
      {coverageComplete ? null : (
        <p role="status" className="text-3xs leading-relaxed text-state-partial">
          Projection coverage is {data.coverage.completeness}; examined{" "}
          {data.coverage.examined.toLocaleString()} under a limit of{" "}
          {data.coverage.limit.toLocaleString()}
          {data.coverage.omission_reasons.length > 0
            ? `; omissions: ${data.coverage.omission_reasons.join(", ")}.`
            : "."}
        </p>
      )}
      {reading.projected ? (
        <>
          <Chart
            ariaLabel={`Principal-component scatter of ${reading.points.length.toLocaleString()} facts by query-time-derived FHRR phase encoding. The axes are unitless directions of greatest variance; the accessible reading of this projection is the category census and extents printed beneath it.`}
            height={260}
            option={option}
          />
          <dl className="grid grid-cols-2 gap-x-3 gap-y-0.5 border-t border-edge-subtle pt-2 text-2xs sm:grid-cols-4">
            <Figure
              label="projected"
              value={reading.points.length.toLocaleString()}
            />
            <Figure label="vector width" value={reading.dim.toLocaleString()} />
            <Figure
              label="pc1 extent"
              value={
                reading.extent
                  ? `${reading.extent.x[0].toFixed(2)} … ${reading.extent.x[1].toFixed(2)}`
                  : "—"
              }
            />
            <Figure
              label="pc2 extent"
              value={
                reading.extent
                  ? `${reading.extent.y[0].toFixed(2)} … ${reading.extent.y[1].toFixed(2)}`
                  : "—"
              }
            />
          </dl>
          {/* The scatter is a canvas, so the census below is the accessible
           * reading of the same data rather than a decoration of it. */}
          <ul
            aria-label="Projected facts by category"
            className="flex flex-wrap gap-x-3 gap-y-0.5 text-3xs text-text-muted"
          >
            {reading.categories.map((row) => (
              <li key={row.category}>
                {row.category} · {row.count.toLocaleString()}
              </li>
            ))}
          </ul>
        </>
      ) : (
        <StateChip
          kind={
            reading.points.length === 0 && coverageComplete
              ? "complete_zero_findings"
              : reading.points.length === 0
                ? "unknown"
                : "partial"
          }
          detail={`method reported as "${data.method}" for a request bounded to ${data.limit.toLocaleString()} facts`}
        />
      )}
    </div>
  );
}

function SimilarityBody({ data }: { data: SimilarityPayload }) {
  const reading = similarityReading(data);
  if (data.error !== "") {
    return (
      <p role="status" className="text-2xs leading-relaxed text-state-error">
        similarity could not be computed: {data.error}
      </p>
    );
  }
  return (
    <div className="flex flex-col gap-2">
      <p className="text-2xs leading-relaxed text-text-secondary">
        {reading.denominators}
      </p>
      <p className="text-3xs leading-relaxed text-text-muted">
        Global distribution over all{" "}
        {data.score_distribution.total_pairs.toLocaleString()} scored pairs;
        these statistics are not limited to the threshold-matching list below.
      </p>
      <div className="flex flex-wrap items-end gap-4 border-y border-edge-subtle py-2">
        <Readout
          label="global mean"
          size="sm"
          value={
            reading.average == null ? "unmeasured" : reading.average.toFixed(4)
          }
        />
        <Readout
          label="global min"
          size="sm"
          value={reading.min == null ? "unmeasured" : reading.min.toFixed(4)}
        />
        <Readout
          label="global max"
          size="sm"
          value={reading.max == null ? "unmeasured" : reading.max.toFixed(4)}
        />
      </div>
      {reading.capped === null ? (
        <p role="status" className="text-3xs leading-relaxed text-state-partial">
          Threshold-list coverage is unknown: {reading.returned.toLocaleString()}{" "}
          {reading.returned === 1 ? "pair" : "pairs"} returned at or above{" "}
          {data.min_similarity.toFixed(2)}, filling this request's limit of{" "}
          {data.limit.toLocaleString()}. The response cannot distinguish an
          exact fit from a truncated list.
        </p>
      ) : (
        <p className="text-3xs leading-relaxed text-text-muted">
          Threshold-list coverage is bounded: {reading.returned.toLocaleString()}{" "}
          {reading.returned === 1 ? "pair" : "pairs"} returned at or above{" "}
          {data.min_similarity.toFixed(2)}; the response ended before this
          request's limit of {data.limit.toLocaleString()}.
        </p>
      )}
      {reading.returned === 0 ? (
        <p className="text-2xs leading-relaxed text-text-muted">
          no pair was returned at or above {data.min_similarity.toFixed(2)}
        </p>
      ) : (
        // Named and tab-reachable: the pair list scrolls and holds no
        // focusable content of its own (WCAG 2.1.1), and the name has to sit
        // on the node that scrolls rather than on a wrapper.
        <ol
          role="region"
          aria-label="Similar fact pairs"
          tabIndex={0}
          className="flex max-h-96 flex-col gap-2 overflow-auto"
        >
          {data.pairs.map((pair) => (
            <li
              key={JSON.stringify([pair.a_id, pair.b_id])}
              className="flex flex-col gap-1 border-l-2 border-edge-subtle pl-2"
            >
              <p className="flex flex-wrap items-baseline gap-x-2 text-3xs text-text-muted">
                <span className="td-value" data-cell="numeric">
                  {pair.a_id} ↔ {pair.b_id}
                </span>
                <span className="td-value" data-cell="numeric">
                  {pair.similarity.toFixed(4)}
                </span>
                <span className="text-text-secondary">
                  {pair.classification}
                </span>
                <span>
                  {pair.a_category}
                  {pair.a_category === pair.b_category
                    ? ""
                    : ` · ${pair.b_category}`}
                </span>
              </p>
              <p className="text-2xs leading-relaxed text-text-secondary">
                {pair.a_content}
              </p>
              <p className="text-2xs leading-relaxed text-text-secondary">
                {pair.b_content}
              </p>
            </li>
          ))}
        </ol>
      )}
      <p className="text-3xs leading-relaxed text-text-muted">
        a scored pair is a measurement, not a proposal — the Curation view
        reports the daemon's automatic post-validation outcomes and explicit
        policy-owned run control
      </p>
    </div>
  );
}

function Figure({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-baseline justify-between gap-2">
      <dt className="td-legend">{label}</dt>
      <dd className="td-value text-2xs" data-cell="numeric">
        {value}
      </dd>
    </div>
  );
}
