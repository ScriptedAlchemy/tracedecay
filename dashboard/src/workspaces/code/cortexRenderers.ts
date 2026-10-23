export const CORTEX_RENDERERS = ['luminous', 'plate', 'relief'] as const;
export type CortexRenderer = (typeof CORTEX_RENDERERS)[number];

interface RendererReading {
  readonly label: string;
  readonly layout: string;
  readonly layoutNote: string;
  readonly description: string;
  readonly readings: readonly (readonly [string, string])[];
}

/** Visual modes share the same served slice, selection and evidence. */
export const CORTEX_RENDERER_DETAILS: Record<CortexRenderer, RendererReading> = {
  luminous: {
    label: 'Luminous',
    layout: 'force-directed',
    layoutNote: 'drawn relations settle the symbol positions',
    description: 'Symbols form a luminous constellation of drawn relations; position is a force layout, not a measured distance.',
    readings: [
      ['size + light', 'served degree'],
      ['hue + shape', 'kind'],
      ['links', 'drawn relations · crossings add light'],
      ['position', 'force layout · not a distance measurement'],
    ],
  },
  plate: {
    label: 'Plate',
    layout: 'dependency layers',
    layoutNote: 'depth down, directories across · basis above the field',
    description: 'Symbols sit in directory columns and dependency-depth bands. The field names whether depth is measured across the index or derived from this slice.',
    readings: [
      ['size', 'served degree'],
      ['hue + shape', 'kind'],
      ['bands', 'dependency depth · basis above field'],
      ['columns', 'directory of the file'],
      ['routes', 'dependency relations · other kinds counted only'],
    ],
  },
  relief: {
    label: 'Relief',
    layout: 'module-packed',
    layoutNote: 'directories packed by shared relations',
    description: "Symbols sit in their directory's hull; relief measures drawn relation endpoints within each directory.",
    readings: [
      ['size', 'degree · area ∝ in + out'],
      ['hue + shape', 'kind'],
      ['relief', 'drawn relation endpoints, per directory'],
      ['hull', 'directory of the file'],
      ['trunk', 'relations across directories · brighter = more'],
    ],
  },
};

export function readCortexRenderer(params: URLSearchParams): CortexRenderer {
  switch (params.get('render')) {
    case 'plate': return 'plate';
    case 'relief': return 'relief';
    default: return 'luminous';
  }
}

export function writeCortexRenderer(params: URLSearchParams, renderer: CortexRenderer): URLSearchParams {
  const next = new URLSearchParams(params);
  next.set('render', renderer);
  return next;
}
