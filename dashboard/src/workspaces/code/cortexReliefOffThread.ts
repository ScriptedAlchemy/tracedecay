/**
 * The relief layout off the UI thread.
 *
 * `reliefLayout` is a few hundred milliseconds of synchronous loops at the
 * 250-directory slice limit, so the mounted field runs it in a worker and
 * keeps the symbol list and inspector live meanwhile. Aborting terminates the
 * worker, which cancels the loops. A finished layout is cached per scene, so
 * asking again for the same scene does not repeat the work.
 */
import type { CortexPainter, CortexScene } from './cortexScene.ts';
import { reliefLayout, reliefPainter, type ReliefLayout } from './cortexReliefField.ts';

export interface ReliefLayoutJob {
  scene: CortexScene;
  aspect: number;
}

export type ReliefLayoutReply = { layout: ReliefLayout } | { error: string };

/** Layouts that completed, by scene then by aspect to two decimals. */
const finished = new WeakMap<CortexScene, Map<number, ReliefLayout>>();

export function reliefLayoutOffThread(
  scene: CortexScene,
  box: { width: number; height: number },
  signal?: AbortSignal,
): Promise<ReliefLayout> {
  const aspect = box.width / Math.max(1, box.height);
  const key = Math.round(aspect * 100);
  const cached = finished.get(scene)?.get(key);
  if (cached) return Promise.resolve(cached);
  const remember = (layout: ReliefLayout): ReliefLayout => {
    const byAspect = finished.get(scene) ?? new Map<number, ReliefLayout>();
    byAspect.set(key, layout);
    finished.set(scene, byAspect);
    return layout;
  };
  // jsdom and server rendering have no Worker; every browser the dashboard
  // targets does, so only there does the layout run in place.
  if (typeof Worker === 'undefined') return Promise.resolve(remember(reliefLayout(scene, aspect)));
  return new Promise((resolve, reject) => {
    if (signal?.aborted) {
      reject(new Error('the layout was superseded'));
      return;
    }
    const worker = new Worker(new URL('./cortexReliefLayout.worker.ts', import.meta.url), {
      type: 'module',
    });
    const settle = (finish: () => void): void => {
      worker.terminate();
      signal?.removeEventListener('abort', onAbort);
      finish();
    };
    const onAbort = (): void => settle(() => reject(new Error('the layout was superseded')));
    signal?.addEventListener('abort', onAbort, { once: true });
    worker.onmessage = (event: MessageEvent<ReliefLayoutReply>) => {
      const reply = event.data;
      if ('layout' in reply) settle(() => resolve(remember(reply.layout)));
      else settle(() => reject(new Error(reply.error)));
    };
    worker.onerror = () => settle(() => reject(new Error('the layout worker failed')));
    worker.onmessageerror = () =>
      settle(() => reject(new Error('the layout result could not be read')));
    try {
      worker.postMessage({ scene, aspect } satisfies ReliefLayoutJob);
    } catch (error) {
      settle(() =>
        reject(error instanceof Error ? error : new Error('the layout could not start')),
      );
    }
  });
}

/** `reliefPainter` with its layout moved to a worker; drawing is unchanged. */
export const reliefPainterOffThread: CortexPainter<ReliefLayout> = {
  ...reliefPainter,
  layout: reliefLayoutOffThread,
};
