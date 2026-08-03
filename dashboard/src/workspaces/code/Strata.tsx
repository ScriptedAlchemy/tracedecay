/**
 * STRATA — `GET /api/plugins/graph/strata` (plan 11b Surface 2).
 *
 * The graph's dependency layering: how deep the import/dependency chains run,
 * and which directories form the clusters those chains cross. Where the spine
 * beside it ranks symbols by how *connected* they are, this ranks directories
 * by how *entangled* they are, which is the measurement that actually predicts
 * what a change will drag with it.
 *
 * The reading that matters is `max_depth` against `ideal_depth`: the producer
 * computes both, and the gap between them is the amount of layering the graph
 * has beyond what its own structure requires.
 *
 * Truthfulness. This is a BUDGETED scan — `scan` carries the file and edge caps
 * it ran under, how many it actually examined, and whether the answer came from
 * cache. A depth computed over a truncated file set is a floor, not a depth, so
 * a capped scan says so on the same line as the number rather than underneath
 * it. Nothing here reports a total the scan did not actually reach.
 */
import {
  StructureReadV12Schema as StrataReadSchema,
  type StrataMeasurementV1,
} from '../../contracts/generated.ts';
import { absenceReason, useStructure } from '../../data/query/structure.ts';
import { Meter } from '../../ui/instrument.tsx';

const BASE = '/api/plugins/graph';

export function Strata() {
  const strata = useStructure<StrataMeasurementV1>(
    ['graph', 'strata'],
    `${BASE}/strata`,
    StrataReadSchema,
  );

  return (
    <section className="flex flex-col gap-1.5" aria-label="Dependency layering">
      <div className="flex items-center gap-1.5">
        <h3 className="td-legend">layering</h3>
        <span aria-hidden className="td-rule" />
      </div>
      {strata.isPending ? (
        <p className="text-2xs text-state-loading">scanning…</p>
      ) : strata.data === undefined ? (
        <p className="text-2xs text-state-unknown">no response recorded</p>
      ) : strata.data.outcome !== 'measured' ? (
        <p className="text-2xs leading-relaxed text-state-unknown">
          {absenceReason(strata.data)}
        </p>
      ) : (
        <StrataReading measurement={strata.data.measurement} />
      )}
    </section>
  );
}

function StrataReading({ measurement }: { measurement: StrataMeasurementV1 }) {
  const { scan } = measurement;
  const filesCapped = scan.files_examined >= scan.max_files;
  const edgesCapped = scan.dependency_edges_examined >= scan.max_dependency_edges;
  const capped = filesCapped || edgesCapped;

  // Directories that leak the most: boundary edges are the ones a change
  // inside the cluster can propagate through.
  const leakiest = [...measurement.clusters]
    .sort((a, b) => b.boundary_edges - a.boundary_edges)
    .slice(0, 5);
  const boundaryCeiling = leakiest[0]?.boundary_edges ?? 0;

  return (
    <div className="flex flex-col gap-2">
      <div className="td-raised flex items-baseline gap-3 border border-edge-subtle px-2.5 py-2">
        <span className="flex min-w-0 flex-col">
          <span className="td-legend">depth</span>
          <span className="flex items-baseline gap-1">
            <span className="td-value text-base text-text-primary" data-cell="numeric">
              {measurement.max_depth}
            </span>
            <span className="td-unit">
              {capped ? 'or more' : `of ${measurement.ideal_depth} ideal`}
            </span>
          </span>
        </span>
        <span className="flex min-w-0 flex-col">
          <span className="td-legend">clusters</span>
          <span className="flex items-baseline gap-1">
            <span className="td-value text-base text-text-primary" data-cell="numeric">
              {measurement.clusters.length.toLocaleString()}
            </span>
            <span className="td-unit">dirs</span>
          </span>
        </span>
        <span className="flex min-w-0 flex-col">
          <span className="td-legend">files</span>
          <span className="flex items-baseline gap-1">
            <span className="td-value text-base text-text-primary" data-cell="numeric">
              {measurement.files.length.toLocaleString()}
            </span>
            <span className="td-unit">laid out</span>
          </span>
        </span>
      </div>

      {capped ? (
        <p className="text-3xs leading-snug text-state-unknown">
          the scan stopped at its budget —{' '}
          {filesCapped ? `${scan.max_files.toLocaleString()} files` : null}
          {filesCapped && edgesCapped ? ' and ' : null}
          {edgesCapped
            ? `${scan.max_dependency_edges.toLocaleString()} dependency edges`
            : null}{' '}
          in {scan.budget_ms} ms — so the depth above is a floor and these clusters are
          the ones it reached, not all of them
        </p>
      ) : null}

      {leakiest.length > 0 ? (
        <div className="flex flex-col gap-1">
          <span className="td-legend normal-case tracking-normal text-text-muted">
            most entangled directories, by edges crossing the boundary
          </span>
          <ol className="flex flex-col gap-1">
            {leakiest.map((cluster) => (
              <li key={cluster.directory} className="flex min-w-0 flex-col gap-0.5">
                <span className="flex min-w-0 items-baseline gap-2">
                  <span
                    className="td-value min-w-0 flex-1 truncate text-2xs text-text-secondary"
                    title={cluster.directory}
                  >
                    {cluster.directory || '(root)'}
                  </span>
                  <span
                    className="td-value shrink-0 text-2xs text-text-primary"
                    data-cell="numeric"
                  >
                    {cluster.boundary_edges.toLocaleString()}
                  </span>
                </span>
                <Meter
                  fraction={
                    boundaryCeiling > 0 ? cluster.boundary_edges / boundaryCeiling : null
                  }
                  height="row"
                  className="w-full"
                />
                <span className="text-3xs leading-snug text-text-muted">
                  {cluster.file_count} {cluster.file_count === 1 ? 'file' : 'files'} ·{' '}
                  {cluster.internal_edges.toLocaleString()} internal ·{' '}
                  {cluster.incoming_edges.toLocaleString()} in ·{' '}
                  {cluster.outgoing_edges.toLocaleString()} out
                </span>
              </li>
            ))}
          </ol>
        </div>
      ) : null}

      <p className="text-3xs leading-snug text-text-muted">
        {measurement.algorithm} over {measurement.dependency_edge_kinds.join(', ')} edges ·
        ordered by {measurement.cluster_ordering} · cache {scan.cache_state}
      </p>
    </div>
  );
}
