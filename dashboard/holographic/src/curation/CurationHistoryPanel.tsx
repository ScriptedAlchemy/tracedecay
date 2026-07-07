import { CurationTabPanel } from "./CurationTabPanel";
import { MemoryOperationsSection } from "./MemoryOperationsSection";
import { RunHistorySection } from "./RunHistorySection";
import { SnapshotsSection } from "./SnapshotsSection";
import type { MemoryCuratorStatusResponse, MemoryOplogEvent } from "../types";

interface CurationHistoryPanelProps {
  status: MemoryCuratorStatusResponse | null;
  statusLoading: boolean;
  statusError: string;
  oplog: MemoryOplogEvent[];
  oplogError: string;
  loadStatus: () => void;
  loadOplog: () => void;
}

export function CurationHistoryPanel({
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
    </CurationTabPanel>
  );
}
