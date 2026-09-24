import type { MemoryReadStatusV1 } from '../../../contracts/generated.ts';
import type { FactScene } from './factScene.ts';

/** What every constellation variant receives: the same scene and the same
 * three verbs as the default drawing. */
export interface RendererProps {
  scene: FactScene;
  inspectedFactId: string | null;
  selectedFactId: string | null;
  onInspect: (factId: string) => void;
  onSelect: (factId: string) => void;
  graphRead: MemoryReadStatusV1 | undefined;
}
