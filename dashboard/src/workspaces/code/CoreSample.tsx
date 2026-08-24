import type { GraphNodeV1, GraphSubgraphPayloadV1 } from '../../contracts/generated.ts';
import { CenteredState } from '../../ui/ReadSection.tsx';
import { elideStart } from '../../ui/format.ts';
import { buildCoreSample, type CoreFileSample } from './structureLens.ts';

function nodeName(node: GraphNodeV1): string {
  return node.name ?? node.qualified_name ?? node.id;
}

function nodeY(file: CoreFileSample, nodeId: string): number | null {
  const node = file.nodes.find((candidate) => candidate.id === nodeId);
  if (node?.start_line == null) return null;
  const span = Math.max(1, file.maxLine - file.minLine);
  return 8 + ((node.start_line - file.minLine) / span) * 84;
}

function CoreColumn({ file, focusId }: { file: CoreFileSample; focusId: string }) {
  return (
    <article className="min-w-56 border-r border-edge-subtle bg-surface-0">
      <header className="border-b border-edge-subtle px-2.5 py-2">
        <h3 className="td-value truncate text-2xs text-text-primary" title={file.path}>
          {elideStart(file.path, 34)}
        </h3>
        <p className="td-legend normal-case tracking-normal">
          lines {file.minLine}–{file.maxLine} · {file.internalEdges.length} internal calls ·{' '}
          {file.externalEdges.length} crossing
        </p>
      </header>
      <div className="relative h-80 overflow-hidden" aria-hidden>
        <svg className="absolute inset-0 size-full" viewBox="0 0 100 100" preserveAspectRatio="none">
          {file.internalEdges.map((edge, index) => {
            const from = nodeY(file, edge.source);
            const to = nodeY(file, edge.target);
            if (from === null || to === null) return null;
            const bend = 12 + Math.min(18, Math.abs(to - from) / 2);
            return (
              <path
                key={`${edge.source}:${edge.target}:${index}`}
                d={`M 28 ${from} Q ${bend} ${(from + to) / 2} 28 ${to}`}
                fill="none"
                stroke="var(--raw-accent-primary)"
                strokeOpacity="0.55"
                strokeWidth="0.7"
                vectorEffect="non-scaling-stroke"
              />
            );
          })}
          {file.externalEdges.map((edge, index) => {
            const at =
              nodeY(file, edge.source) ??
              nodeY(file, edge.target);
            if (at === null) return null;
            return (
              <path
                key={`${edge.source}:${edge.target}:external:${index}`}
                d={`M 72 ${at} L 100 ${at}`}
                fill="none"
                stroke="var(--raw-text-muted)"
                strokeDasharray="2 2"
                strokeWidth="0.6"
                vectorEffect="non-scaling-stroke"
              />
            );
          })}
        </svg>
        {file.nodes.map((node) => {
          const y = nodeY(file, node.id) ?? 8;
          return (
            <span
              key={node.id}
              className="absolute left-[28%] right-1 flex min-w-0 -translate-y-1/2 items-baseline gap-1 border-l border-edge-strong bg-surface-0/85 pl-1.5"
              style={{ top: `${y}%` }}
            >
              <span className="td-value w-7 shrink-0 text-right text-3xs text-text-muted">
                {node.start_line}
              </span>
              <span
                className={
                  node.id === focusId
                    ? 'td-value min-w-0 truncate text-2xs text-accent-primary'
                    : 'td-value min-w-0 truncate text-2xs text-text-secondary'
                }
              >
                {nodeName(node)}
              </span>
            </span>
          );
        })}
      </div>
    </article>
  );
}

/** File interiors from one exact, already-returned graph slice. The six-column
 * visual is paired with all positioned rows so its cap never becomes silent
 * data loss. */
export function CoreSample({
  payload,
  focusId,
}: {
  payload: GraphSubgraphPayloadV1;
  focusId: string;
}) {
  const sample = buildCoreSample(payload, focusId);
  if (sample === null) {
    return (
      <CenteredState
        title="CORE is unavailable because this graph row has no measured file and line span"
        kind="unavailable"
      />
    );
  }

  return (
    <section className="flex min-h-0 flex-1 flex-col" aria-label="CORE source sample">
      <div className="flex flex-wrap items-baseline gap-2 border-b border-edge-subtle px-3 py-2">
        <h2 className="td-title">CORE sample</h2>
        <span aria-hidden className="td-rule" />
        <span className="td-legend normal-case tracking-normal text-text-muted">
          {sample.files.length} of {sample.totalFileCount} measured files in this returned
          subgraph · {sample.rows.length} positioned symbols
        </span>
        {sample.hiddenFileCount > 0 ? (
          <span className="text-3xs text-state-unknown">
            visual cap omits {sample.hiddenFileCount} files / {sample.hiddenNodeCount} symbols;
            every row remains in the table
          </span>
        ) : null}
        {payload.capped.nodes || payload.capped.edges ? (
          <span className="text-3xs text-state-unknown">
            server projection capped at {payload.limits.nodes} nodes / {payload.limits.edges}{' '}
            edges; identities beyond that projection are not present in this sample or table
          </span>
        ) : null}
      </div>
      <div className="min-h-[24rem] overflow-x-auto border-b border-edge-subtle">
        <div className="flex min-w-max">
          {sample.files.map((file) => (
            <CoreColumn key={file.path} file={file} focusId={focusId} />
          ))}
        </div>
      </div>
      <div className="min-h-0 overflow-auto">
        <table className="w-full text-left text-2xs">
          <caption className="sr-only">
            All source-positioned symbols in the returned subgraph, including files outside
            the six-column visual sample
          </caption>
          <thead className="sticky top-0 bg-surface-1">
            <tr className="border-b border-edge-subtle">
              <th className="td-legend px-3 py-2">file</th>
              <th className="td-legend px-3 py-2">line</th>
              <th className="td-legend px-3 py-2">symbol</th>
              <th className="td-legend px-3 py-2">kind</th>
            </tr>
          </thead>
          <tbody>
            {sample.rows.map((node) => (
              <tr key={node.id} className="border-b border-edge-subtle">
                <td className="max-w-72 truncate px-3 py-2 font-mono text-text-muted">
                  {node.file_path}
                </td>
                <td className="px-3 py-2 tabular-nums text-text-secondary">
                  {node.start_line}–{node.end_line}
                </td>
                <td className="px-3 py-2 text-text-primary">{nodeName(node)}</td>
                <td className="px-3 py-2 text-text-muted">{node.kind}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </section>
  );
}
