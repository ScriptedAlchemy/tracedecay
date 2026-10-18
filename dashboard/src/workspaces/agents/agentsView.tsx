import { useCallback } from 'react';
import { useSearchParams } from 'react-router';
import {
  assertNever,
  type AnalyticsSubagentNodeV1,
  type AnalyticsSubagentTreePayloadV1,
} from '../../contracts/generated.ts';
import {
  joinIndexRow,
  readSessionSelection,
  writeSessionSelection,
  type SessionSelection,
} from '../sessions/model.ts';

export type AgentsSelection =
  | { readonly kind: 'none' }
  | { readonly kind: 'node'; readonly node: AnalyticsSubagentNodeV1 }
  | {
      readonly kind: 'unresolved';
      readonly selection: SessionSelection;
      readonly reason: 'pending' | 'unavailable' | 'absent' | 'ambiguous';
    };

export function useAgentsSelection(
  tree: AnalyticsSubagentTreePayloadV1 | 'pending' | 'unavailable',
): [AgentsSelection, (node: AnalyticsSubagentNodeV1 | null) => void] {
  const [params, setParams] = useSearchParams();
  const select = useCallback(
    (node: AnalyticsSubagentNodeV1 | null) =>
      setParams(
        writeSessionSelection(
          params,
          node === null ? null : { provider: node.provider, sessionId: node.session_id },
        ),
      ),
    [params, setParams],
  );
  return [resolveAgentsSelection(readSessionSelection(params), tree), select];
}

function resolveAgentsSelection(
  linked: SessionSelection | null,
  tree: AnalyticsSubagentTreePayloadV1 | 'pending' | 'unavailable',
): AgentsSelection {
  if (linked === null) return { kind: 'none' };
  if (typeof tree === 'string') return { kind: 'unresolved', selection: linked, reason: tree };
  const join = joinIndexRow(tree.nodes, linked);
  switch (join.kind) {
    case 'exact':
      return { kind: 'node', node: join.row };
    case 'ambiguous':
    case 'absent':
      return { kind: 'unresolved', selection: linked, reason: join.kind };
    default:
      return assertNever(join);
  }
}
