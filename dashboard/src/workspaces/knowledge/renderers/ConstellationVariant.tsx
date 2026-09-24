import { useMemo } from 'react';

import type { MemoryFactRowV1, MemoryGraphPayloadV1 } from '../../../contracts/generated.ts';
import { CamerasRenderer } from './CamerasRenderer.tsx';
import { composeFactScene } from './factScene.ts';
import { LatticeRenderer } from './LatticeRenderer.tsx';
import { TrustFieldRenderer } from './TrustFieldRenderer.tsx';
import type { RendererProps } from './types.ts';
import type { ConstellationVariant as Variant } from './variant.ts';

/** One candidate renderer over the scene joined from the served graph and rows. */
export function ConstellationVariant({
  variant,
  graph,
  rows,
  ...props
}: Omit<RendererProps, 'scene'> & {
  variant: Variant;
  graph: MemoryGraphPayloadV1;
  rows: readonly MemoryFactRowV1[];
}) {
  const scene = useMemo(() => composeFactScene(graph, rows), [graph, rows]);
  switch (variant) {
    case 'cameras':
      return <CamerasRenderer scene={scene} {...props} />;
    case 'field':
      return <TrustFieldRenderer scene={scene} {...props} />;
    case 'lattice':
      return <LatticeRenderer scene={scene} {...props} />;
    default: {
      const unhandled: never = variant;
      return unhandled;
    }
  }
}
