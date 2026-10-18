import { formatBytes, storageFindingLabel, StoreBadge } from './storageModel.ts';

interface Store {
  kind: string;
  total_bytes: number;
}

export function TelemetryBody({ stores }: { stores: Store[] }) {
  return (
    <ul>
      {stores.slice(0, 7).map((store) => {
        return <li key={store.kind}>{formatBytes(store.total_bytes)}</li>;
      })}
    </ul>
  );
}

export const FindingRows = ({ stores }: { stores: Store[] }) =>
  stores.map((entry, index) => ({
    index,
    labels: [entry].flatMap((nested) => [storageFindingLabel(nested.kind)]),
  }));

export function FindingButton({ store }: { store: Store }) {
  return (
    <button onClick={() => storageFindingLabel(store.kind)}>
      <StoreBadge label={store.kind} />
    </button>
  );
}

export const summaryLabels = [1024, 2048].map(function label(bytes) {
  return formatBytes(bytes);
});

export class StorageTable {
  sizeLabel = (store: Store) => formatBytes(store.total_bytes);
}
