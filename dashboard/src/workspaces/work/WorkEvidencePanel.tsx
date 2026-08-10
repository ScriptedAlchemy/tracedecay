import { useEffect, useState } from 'react';
import type {
  WorkEvidenceContinuationV1,
  WorkEvidenceRetrievalV1,
  WorkEvidenceSourceV1,
  WorkGraphReadV1,
} from '../../contracts/index.ts';
import { StateChip } from '../../ui/StateChip.tsx';
import { Panel } from '../../ui/instrument.tsx';
import type { WorkResult } from './workApi.ts';
import { useWorkEvidence, workEvidenceAuthorityKey } from './workEvidenceQueries.ts';

function qualifiedSession(source: WorkEvidenceSourceV1): string | null {
  if (source.kind !== 'task_session') return null;
  const identity = source.evidence.source;
  return `${identity.provider ?? 'unknown provider'} / ${identity.session_id}`;
}

function contentText(content: number[] | null): string | null {
  if (content === null) return null;
  return new TextDecoder('utf-8', { fatal: false }).decode(Uint8Array.from(content));
}

function EvidenceResult({ value }: { value: WorkEvidenceRetrievalV1 }) {
  const sessions = value.sources.filter((source) => source.kind === 'task_session');
  const receipts = value.sources.filter((source) => source.kind === 'attempt_receipt');
  return (
    <>
      <div className="flex flex-wrap gap-2">
        <StateChip
          kind={value.coverage.state === 'complete' ? 'ready' : 'partial'}
          detail={`${value.coverage.hydrated} hydrated · ${value.coverage.omitted} omitted · ${value.freshness}`}
        />
        {value.redacted ? <StateChip kind="redacted" detail="some evidence is redacted" /> : null}
      </div>

      <dl className="mt-1 grid gap-0.5 text-3xs text-text-muted">
        <div>
          <dt className="inline text-text-secondary">Graph authority: </dt>
          <dd className="inline font-mono">
            v{value.verified_version.graph_version} / sequence {value.verified_version.event_sequence}
          </dd>
        </div>
        <div>
          <dt className="inline text-text-secondary">Who worked: </dt>
          <dd className="inline">
            {sessions.length === 0
              ? 'no provider-qualified session was returned'
              : sessions.map(qualifiedSession).join(', ')}
          </dd>
        </div>
        <div>
          <dt className="inline text-text-secondary">Attempt receipts: </dt>
          <dd className="inline">{receipts.length}</dd>
        </div>
      </dl>

      {sessions.map((source) => (
        <section
          key={`${source.attempt.run_id}:${source.attempt.attempt_id}`}
          className="mt-2 rounded-sm border border-edge bg-surface-2 p-2"
          aria-label={`Session evidence ${source.attempt.attempt_id}`}
        >
          <h4 className="text-2xs text-text-primary">{qualifiedSession(source)}</h4>
          <p className="font-mono text-3xs text-text-muted">
            {source.attempt.run_id} / {source.attempt.attempt_id} · {source.evidence.coverage} · {source.evidence.freshness}
          </p>
          <ul className="mt-1 grid gap-1">
            {source.evidence.hydrated.map((hydration) => {
              const text = contentText(hydration.content);
              return (
                <li key={`${hydration.rank}:${hydration.anchor_id}`} className="text-3xs text-text-muted">
                  <span className="font-mono text-text-secondary">
                    #{hydration.rank} {hydration.state} {hydration.anchor_id}
                  </span>
                  {text === null ? null : <p className="mt-0.5 whitespace-pre-wrap text-text-primary">{text}</p>}
                </li>
              );
            })}
          </ul>
        </section>
      ))}

      {value.omissions.length === 0 ? null : (
        <ul className="mt-2 grid gap-0.5 text-3xs text-text-muted" aria-label="Evidence omissions">
          {value.omissions.map((omission, index) => (
            <li key={`${omission.relation}:${omission.reason}:${index}`}>
              {omission.relation}: {omission.reason}
            </li>
          ))}
        </ul>
      )}
    </>
  );
}

export function WorkEvidencePanel({
  taskId,
  graph,
}: {
  taskId: string;
  graph: WorkResult<WorkGraphReadV1> | undefined;
}) {
  const [continuation, setContinuation] = useState<WorkEvidenceContinuationV1 | null>(null);
  const authorityKey = workEvidenceAuthorityKey(graph, taskId);
  useEffect(() => setContinuation(null), [authorityKey]);
  const evidence = useWorkEvidence(graph, taskId, continuation);
  const result = evidence.data;
  const value = result?.outcome === 'value' ? result.value : undefined;

  return (
    <Panel legend={`Evidence · ${taskId}`}>
      {evidence.isPending ? <StateChip kind="loading" detail="retrieving exact task evidence" /> : null}
      {result?.outcome === 'refused' ? <StateChip kind={result.state} detail={result.detail} /> : null}
      {value === undefined ? null : <EvidenceResult value={value} />}
      {value?.continuations.map((next, index) => (
        <button
          key={`${next.kind}:${index}`}
          type="button"
          className="mt-2 min-h-[44px] rounded-sm border border-edge px-2 py-1 text-2xs text-text-primary hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
          onClick={() => setContinuation(next)}
        >
          Continue {next.kind === 'task_session' ? 'provider session' : 'evidence anchor'}
        </button>
      ))}
      {continuation === null ? null : (
        <button
          type="button"
          className="ml-2 mt-2 min-h-[44px] rounded-sm border border-edge px-2 py-1 text-2xs text-text-secondary hover:bg-surface-3 focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent"
          onClick={() => setContinuation(null)}
        >
          Return to task evidence root
        </button>
      )}
    </Panel>
  );
}
