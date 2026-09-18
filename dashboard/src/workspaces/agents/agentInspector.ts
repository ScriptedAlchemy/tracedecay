import type { AnalyticsRecentHookV1 } from '../../contracts/generated.ts';
import type { EvidenceGrade as EvidenceGradeKind } from '../../ui/EvidenceGrade.tsx';
import type {
  DelegationTopologyModel,
  TopologyBundleMark,
  TopologyMark,
  TopologySessionMark,
} from './delegationTopology.ts';

/**
 * What the Agents inspector is looking at, and what each of its sections may
 * say about it. Pure derivations over the laid-out topology and the reads the
 * page already holds; nothing here fetches, and nothing here joins two
 * authorities on anything weaker than a shared identity.
 */

/** How the subject came to be in the inspector. `inspecting` is a hover or
 * focus and persists nothing; `selected` is a reader's click or Enter;
 * `default` is the page's own choice when nothing is selected, the newest
 * root, and is labelled as such rather than passed off as a selection. */
export type SubjectMode = 'inspecting' | 'selected' | 'default';

export type InspectorSubject =
  | { readonly kind: 'session'; readonly mark: TopologySessionMark; readonly mode: SubjectMode }
  | { readonly kind: 'bundle'; readonly mark: TopologyBundleMark; readonly mode: 'inspecting' }
  | { readonly kind: 'none'; readonly detail: string };

export function resolveSubject(
  model: DelegationTopologyModel,
  inspectedId: string | null,
  selectedId: string | null,
  defaultSessionId: string | null,
  /** Whether the selected id names a session the reading still holds. A
   * selection can outlive a refetch; the wording must not call that a fold. */
  selectedInReading: boolean = true,
): InspectorSubject {
  const byId = new Map(model.marks.map((mark) => [mark.id, mark]));
  const inspected = inspectedId === null ? undefined : byId.get(inspectedId);
  // A click lands with the pointer still over the mark, so the selected mark
  // is also the inspected one; the persistent act wins the caption.
  if (inspected && inspected.id !== selectedId) return subjectFor(inspected, 'inspecting');
  const selected = selectedId === null ? undefined : byId.get(selectedId);
  if (selected && selected.kind === 'session') return { kind: 'session', mark: selected, mode: 'selected' };
  if (selectedId !== null && selected === undefined) {
    // Selected, but not drawn. Either the reading still holds it and a bundle
    // or the depth limit folded it, or a refetch dropped it altogether.
    return {
      kind: 'none',
      detail: selectedInReading
        ? 'the selected session is folded out of the field; open its bundle or generation to inspect it'
        : 'the selected session is no longer in the reading; clear the selection or select a drawn session',
    };
  }
  const fallback =
    defaultSessionId === null
      ? undefined
      : model.marks.find(
          (mark): mark is TopologySessionMark =>
            mark.kind === 'session' && mark.node.session_id === defaultSessionId,
        );
  if (fallback) return { kind: 'session', mark: fallback, mode: 'default' };
  return {
    kind: 'none',
    detail:
      model.marks.length === 0
        ? 'the reading draws no session, so there is nothing to inspect'
        : 'hover or focus a mark to inspect it; click or Enter selects it',
  };
}

function subjectFor(mark: TopologyMark, mode: 'inspecting'): InspectorSubject {
  return mark.kind === 'bundle'
    ? { kind: 'bundle', mark, mode }
    : { kind: 'session', mark, mode };
}

/** The delegation that brought a session into being, graded by what the
 * store could establish about it. */
export type IncomingDelegation =
  | {
      readonly kind: 'root';
      readonly grade: EvidenceGradeKind;
      readonly detail: string;
    }
  | {
      readonly kind: 'linked';
      readonly grade: EvidenceGradeKind;
      readonly parent: TopologySessionMark | null;
      readonly parentSessionId: string;
      readonly toolUseId: string | null;
      readonly detail: string;
    }
  | {
      readonly kind: 'missing_parent';
      readonly grade: EvidenceGradeKind;
      readonly parentSessionId: string | null;
      readonly detail: string;
    }
  | {
      readonly kind: 'cycle';
      readonly grade: EvidenceGradeKind;
      readonly parentSessionId: string | null;
      readonly detail: string;
    };

export function incomingDelegation(
  mark: TopologySessionMark,
  model: DelegationTopologyModel,
): IncomingDelegation {
  const node = mark.node;
  switch (node.link) {
    case 'root':
      return {
        kind: 'root',
        grade: 'EXACT',
        detail: 'no parent session is recorded; this session began on its own',
      };
    case 'linked': {
      const parent =
        mark.parentId === null
          ? null
          : (model.marks.find(
              (candidate): candidate is TopologySessionMark =>
                candidate.kind === 'session' && candidate.id === mark.parentId,
            ) ?? null);
      return {
        kind: 'linked',
        grade: 'EXACT',
        parent,
        parentSessionId: node.parent_session_id ?? '',
        toolUseId: node.parent_tool_use_id,
        detail:
          node.parent_tool_use_id === null
            ? 'the store records the parent session but no delegating tool call'
            : `delegated by tool call ${node.parent_tool_use_id}`,
      };
    }
    case 'missing_parent':
      return {
        kind: 'missing_parent',
        grade: 'UNAVAILABLE',
        parentSessionId: node.parent_session_id,
        detail: `parent ${node.parent_session_id ?? '(unnamed)'} is not in this reading, a cut edge, not a root`,
      };
    case 'cycle':
      return {
        kind: 'cycle',
        grade: 'AMBIGUOUS',
        parentSessionId: node.parent_session_id,
        detail: 'its parent chain closes on itself, so no root reaches it and no delegation order can be read',
      };
    default: {
      const unhandled: never = node.link;
      return unhandled;
    }
  }
}

/** One delegation out of the subject, to a drawn child or a drawn bundle. */
export interface OutgoingDelegation {
  readonly to: TopologyMark;
  readonly toolUseId: string | null;
}

export function outgoingDelegations(
  mark: TopologySessionMark,
  model: DelegationTopologyModel,
): readonly OutgoingDelegation[] {
  const byId = new Map(model.marks.map((candidate) => [candidate.id, candidate]));
  return model.edges
    .filter((edge) => edge.from === mark.id)
    .flatMap((edge) => {
      const to = byId.get(edge.to);
      return to ? [{ to, toolUseId: edge.toolUseId }] : [];
    });
}

/**
 * Recent hooks the diagnostics tape attributes to this exact session id.
 *
 * `served` is the tape's whole length, because "none of the twenty served
 * hooks names this session" is a statement about twenty hooks and not about
 * the session's history. The join is on identity and nothing else.
 */
export interface SessionHooks {
  readonly matched: readonly AnalyticsRecentHookV1[];
  readonly served: number;
  readonly truncated: boolean;
}

export function sessionHooks(
  hooks: readonly AnalyticsRecentHookV1[],
  sessionId: string,
  truncated: boolean,
): SessionHooks {
  return {
    matched: hooks.filter((hook) => hook.session_id === sessionId),
    served: hooks.length,
    truncated,
  };
}
