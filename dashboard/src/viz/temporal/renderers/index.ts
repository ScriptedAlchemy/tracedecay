import type { SceneRenderer, SceneRendererId } from './contract.ts';
import { currentRenderer } from './current.tsx';
import { railRenderer } from './rail.tsx';
import { strataRenderer } from './strata.tsx';
import { weaveRenderer } from './weave.tsx';

export type { SceneRenderer, SceneRendererId } from './contract.ts';

export const SCENE_RENDERERS: Readonly<Record<SceneRendererId, SceneRenderer>> = {
  current: currentRenderer,
  rail: railRenderer,
  weave: weaveRenderer,
  strata: strataRenderer,
};

/** A renderer id from the URL; anything unrecognised is the shipped weave. */
export function parseSceneRenderer(raw: string | null): SceneRendererId {
  return raw === 'rail' || raw === 'weave' || raw === 'strata' ? raw : 'current';
}
