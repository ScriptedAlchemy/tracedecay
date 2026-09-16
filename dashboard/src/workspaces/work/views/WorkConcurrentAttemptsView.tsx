import type { FeedbackProximityReadResultV1 } from '../../../contracts/index.ts';
import { StateChip } from '../../../ui/StateChip.tsx';
import { Panel } from '../../../ui/instrument.tsx';
import { ProximityPanel } from '../../../viz/proximity/index.ts';
import type { WorkResult } from '../workApi.ts';
import type { ConcurrentAttemptsReading } from '../workConcurrentAttempts.ts';

export function WorkConcurrentAttemptsView({
  reading,
  proximity,
  selectedEncounterId,
  onSelectEncounter,
}: {
  reading: ConcurrentAttemptsReading;
  proximity: WorkResult<FeedbackProximityReadResultV1> | undefined;
  selectedEncounterId: string | null;
  onSelectEncounter: (encounterId: string | null) => void;
}) {
  return (
    <div className="grid min-w-0 gap-3">
      <Panel legend="Concurrent attempt spans">
        <ReadingState reading={reading} />
        {reading.state === 'ready' || reading.state === 'partial' ? (
          <div className="mt-2 grid gap-2">
            {reading.rows.map((row) => (
              <button
                key={row.encounter.encounter_id}
                type="button"
                onClick={() => onSelectEncounter(row.encounter.encounter_id)}
                className="min-h-11 border border-edge-subtle p-2 text-left text-3xs text-text-muted"
              >
                <span className="block text-xs text-text-primary">
                  {row.left.identity.attempt_id} + {row.right.identity.attempt_id}
                </span>
                <span>
                  {formatMicros(row.left.span.started_at)} to{' '}
                  {row.left.span.ended_at === null
                    ? 'open'
                    : formatMicros(row.left.span.ended_at)}
                  {' · '}
                  {formatMicros(row.right.span.started_at)} to{' '}
                  {row.right.span.ended_at === null
                    ? 'open'
                    : formatMicros(row.right.span.ended_at)}
                </span>
              </button>
            ))}
          </div>
        ) : null}
      </Panel>
      <ProximityPanel
        legend="Concurrent attempt encounters"
        result={proximity}
        selectedId={selectedEncounterId}
        onSelect={onSelectEncounter}
      />
    </div>
  );
}

function ReadingState({ reading }: { reading: ConcurrentAttemptsReading }) {
  switch (reading.state) {
    case 'complete_zero':
      return <StateChip kind="complete_zero_findings" detail="zero joined attempt encounters" />;
    case 'ready':
      return <StateChip kind="ready" detail={`${reading.rows.length} joined encounters`} />;
    case 'partial':
      return (
        <StateChip
          kind="partial"
          detail={`${reading.rows.length} joined encounters · ${reading.missingJoins} missing provider/session joins`}
        />
      );
    case 'unavailable':
      return <StateChip kind="unavailable" detail={reading.detail} />;
    default: {
      const unhandled: never = reading;
      return unhandled;
    }
  }
}

function formatMicros(value: number): string {
  return new Date(value / 1_000).toISOString();
}
