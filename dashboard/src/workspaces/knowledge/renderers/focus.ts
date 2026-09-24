import { useMemo } from 'react';

import { hubFact, type FactScene, type SceneFact } from './factScene.ts';

export interface SceneFocus {
  /** Nodes kept at full strength while a fact is inspected; `null` dims nothing. */
  readonly set: ReadonlySet<string> | null;
  readonly inspectedNode: string | null;
  readonly selectedNode: string | null;
  /** The fact the focal readout names, and why. */
  readonly fact: SceneFact | null;
  readonly role: 'selected' | 'inspecting' | 'hub';
}

/**
 * Which fact the reader is looking at, and what it dims, shared by every
 * variant so the three verbs behave alike whichever is drawn. Inspection dims
 * what the fact is not wired to; selection does not dim, it rings.
 */
export function useSceneFocus(
  scene: FactScene,
  hovered: string | null,
  inspectedFactId: string | null,
  selectedFactId: string | null,
): SceneFocus {
  const nodeOf = (factId: string | null) =>
    factId == null ? null : (scene.model.nodeIdByFact.get(factId) ?? null);
  const inspectedNode = hovered ?? nodeOf(inspectedFactId);
  const selectedNode = nodeOf(selectedFactId);
  const focusNode = inspectedNode !== selectedNode ? inspectedNode : null;
  const set = useMemo(() => {
    if (focusNode == null) return null;
    const out = new Set<string>([focusNode]);
    for (const neighbour of scene.model.neighbours.get(focusNode) ?? []) out.add(neighbour);
    if (selectedNode) out.add(selectedNode);
    return out;
  }, [focusNode, scene.model.neighbours, selectedNode]);
  const focused = focusNode ? scene.byNode.get(focusNode) : undefined;
  const selected = selectedNode ? scene.byNode.get(selectedNode) : undefined;
  if (focused) return { set, inspectedNode, selectedNode, fact: focused, role: 'inspecting' };
  if (selected) return { set, inspectedNode, selectedNode, fact: selected, role: 'selected' };
  return { set, inspectedNode, selectedNode, fact: hubFact(scene), role: 'hub' };
}
