import type { MemoryCuratorStatusResponse } from "../types";

export function SnapshotsSection({
  snapshots,
}: {
  snapshots: MemoryCuratorStatusResponse["snapshots"];
}) {
  return (
    <div className="border border-border bg-background/30 px-3 py-2">
      <div className="mb-1 text-[11px] uppercase tracking-[0.08em] text-text-tertiary">
        Recent snapshots
      </div>
      {snapshots.length ? (
        <div className="flex flex-col gap-1">
          {snapshots.map((snapshot) => (
            <div
              key={snapshot.path}
              className="min-w-0 font-mono-ui text-xs text-text-secondary break-all"
            >
              {snapshot.name}
            </div>
          ))}
        </div>
      ) : (
        <div className="text-xs text-text-tertiary">No snapshots found.</div>
      )}
    </div>
  );
}
