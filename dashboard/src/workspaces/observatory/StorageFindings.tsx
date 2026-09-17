import type {
  DashboardEnvelopeV1,
  DoctorReportEntryV1,
  StorageFindingKindStatusV1,
  StorageFindingsPayloadV1,
} from '../../contracts/generated.ts';
import { EnvelopeTruth } from '../../ui/EnvelopeTruth.tsx';
import { EvidenceTruthStrip } from '../../ui/EvidenceTruthStrip.tsx';
import { ReadModelState } from '../../ui/ReadSection.tsx';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { doctorEvidencePresentation } from './doctorModel.ts';
import { storageFindingLabel, storageSourcePresentation } from './storageModel.ts';
import { ReadModelNotes } from './StorageTelemetry.tsx';

/** `/api/storage/findings` is the storage-family projection of the admitted
 * canonical Doctor report. The browser preserves its typed subclass, evidence,
 * and coverage without recomputing health. */
export function FindingsReadModel({
  envelope,
  refreshing,
  onRefresh,
}: {
  envelope: DashboardEnvelopeV1<StorageFindingsPayloadV1>;
  refreshing: boolean;
  onRefresh: () => void;
}) {
  return (
    <>
      <EnvelopeTruth envelope={envelope} refreshing={refreshing} onRefresh={onRefresh} />
      <StorageSourceStatuses statuses={envelope.payload.kind_statuses} />
      {envelope.payload.entries.length === 0 ? (
        <ReadModelState kind={envelope.domain_state} detail={envelope.payload.note} />
      ) : (
        <OverviewGrid>
          {envelope.payload.entries.map((entry, index) => (
            <StorageFindingCard
              // The index is part of the key, not a fallback: a live report can
              // legally carry two findings of one kind whose first evidence
              // names the same reference (observed: repeated retention_backlog
              // rows for one store), and `kind:reference` alone collided.
              key={`${entry.storage_kind ?? 'unclassified'}:${entry.finding.evidence[0]?.reference ?? 'no-evidence'}:${index}`}
              entry={entry}
            />
          ))}
        </OverviewGrid>
      )}
      <ReadModelNotes notes={[envelope.payload.note]} />
    </>
  );
}

function StorageSourceStatuses({ statuses }: { statuses: StorageFindingKindStatusV1[] }) {
  return (
    <ul
      className="mx-4 mt-3 grid gap-2 sm:grid-cols-2 lg:grid-cols-3 xl:grid-cols-6"
      aria-label="Storage finding source status"
    >
      {statuses.map((status) => {
        const presentation = storageSourcePresentation(status);
        return (
          <li
            key={status.kind}
            className="min-w-0 rounded-[var(--radius-standard)] border border-edge-subtle bg-surface-1 p-3"
            data-storage-source-kind={status.kind}
            data-storage-source-state={status.state}
          >
            <p className="flex items-center gap-1.5 text-2xs font-medium text-text-secondary">
              <span
                aria-hidden
                className={`size-1.5 shrink-0 rounded-full ${presentation.dotClass}`}
              />
              <span>{storageFindingLabel(status.kind)}</span>
              <span className={presentation.tokenClass}>· {presentation.label}</span>
            </p>
            <p className="mt-1 text-2xs text-text-muted">{status.reason}</p>
            {status.observed_entries > 0 ? (
              <p className="mt-1 text-3xs text-text-muted tabular">
                {status.observed_entries} observed{' '}
                {status.observed_entries === 1 ? 'entry' : 'entries'}
              </p>
            ) : null}
          </li>
        );
      })}
    </ul>
  );
}

function StorageFindingCard({ entry }: { entry: DoctorReportEntryV1 }) {
  const { finding, storage_kind: storageKind } = entry;
  const presentation = doctorEvidencePresentation(finding.state);
  return (
    <OverviewCard
      title={storageKind ? storageFindingLabel(storageKind) : 'Unclassified storage finding'}
    >
      <div
        className="flex flex-col gap-2"
        data-storage-finding-kind={storageKind ?? 'unclassified'}
      >
        <span
          className={`inline-flex w-fit items-center gap-1.5 rounded-[var(--radius-chip)] border border-edge-subtle bg-surface-2 px-2 py-0.5 text-2xs font-medium ${presentation.tokenClass}`}
          data-evidence-state={finding.state}
        >
          <span aria-hidden className={`size-1.5 rounded-full ${presentation.dotClass}`} />
          {presentation.label}
        </span>
        <EvidenceTruthStrip
          coverage={{ completeness: finding.coverage.completeness }}
          citations={finding.evidence.length}
        />
        <p className="text-xs text-text-secondary">{finding.coverage.statement}</p>
        <ul className="space-y-1" aria-label="Storage finding evidence">
          {finding.evidence.map((evidence, index) => (
            <li
              // Indexed like the finding cards above: references are
              // server-authored rows, not unique identities.
              key={`${evidence.family}:${evidence.reference}:${index}`}
              className="break-all font-mono text-2xs text-text-muted"
            >
              {evidence.reference}
            </li>
          ))}
        </ul>
      </div>
    </OverviewCard>
  );
}
