import { CurationTabPanel } from "./CurationTabPanel";
import { CurrentPreviewSection } from "./CurrentPreviewSection";
import { MemoryOperationsSection } from "./MemoryOperationsSection";
import { RunHistorySection } from "./RunHistorySection";
import { SnapshotsSection } from "./SnapshotsSection";
import type {
  MemoryCurateResponse,
  MemoryCuratorStatusResponse,
  MemoryOplogEvent,
} from "../types";

interface CurationHistoryPanelProps {
  report: MemoryCurateResponse | null;
  previewSavedAt: string | null;
  previewStale: boolean;
  previewStaleReason: string;
  actionsLength: number;
  actionCounts: Array<[string, number]>;
  diagnosticCounts: Array<[string, number]>;
  isPlan: boolean;
  status: MemoryCuratorStatusResponse | null;
  statusLoading: boolean;
  statusError: string;
  oplog: MemoryOplogEvent[];
  oplogError: string;
  loadStatus: () => void;
  loadOplog: () => void;
}

export function CurationHistoryPanel({
  report,
  previewSavedAt,
  previewStale,
  previewStaleReason,
  actionsLength,
  actionCounts,
  diagnosticCounts,
  isPlan,
  status,
  statusLoading,
  statusError,
  oplog,
  oplogError,
  loadStatus,
  loadOplog,
}: CurationHistoryPanelProps) {
  return (
    <CurationTabPanel
      tab="history"
      title="Curator History"
      subtitle="Run history, recent snapshots, and the memory operation log."
      refreshing={statusLoading}
      onRefresh={() => {
        loadStatus();
        loadOplog();
      }}
      error={statusError}
    >
      {status ? (
        <>
          <RunHistorySection status={status} />
          <SnapshotsSection snapshots={status.snapshots} />
        </>
      ) : null}
      <MemoryOperationsSection events={oplog} error={oplogError} />
      <CurrentPreviewSection
        report={report}
        previewSavedAt={previewSavedAt}
        previewStale={previewStale}
        previewStaleReason={previewStaleReason}
        actionsLength={actionsLength}
        actionCounts={actionCounts}
        diagnosticCounts={diagnosticCounts}
        isPlan={isPlan}
      />
    </CurationTabPanel>
  );
}
