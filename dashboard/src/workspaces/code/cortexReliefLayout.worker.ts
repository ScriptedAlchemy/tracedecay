import { reliefLayout } from './cortexReliefField.ts';
import type { ReliefLayoutJob, ReliefLayoutReply } from './cortexReliefOffThread.ts';

// One layout per worker; terminating the worker cancels the synchronous loops.
self.onmessage = (event: MessageEvent<ReliefLayoutJob>) => {
  try {
    const layout = reliefLayout(event.data.scene, event.data.aspect);
    self.postMessage({ layout } satisfies ReliefLayoutReply);
  } catch (error) {
    self.postMessage({
      error: error instanceof Error ? error.message : 'the layout did not complete',
    } satisfies ReliefLayoutReply);
  }
};
