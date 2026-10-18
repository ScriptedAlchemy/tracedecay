import { formatBytes, StoreBadge } from './storageModel.ts';

console.info(formatBytes(512));

if (import.meta.env.DEV) {
  mount(<StoreBadge label="dev" />);
}

export default defineReport({ size: formatBytes(1024) });
