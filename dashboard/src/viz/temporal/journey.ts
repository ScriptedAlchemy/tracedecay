/**
 * The journey projection: the Loom authorities joined into one renderer-
 * neutral reading of who ran when, under whom, and what each run recorded.
 *
 * Pure, no DOM, no clock. Every lane, event, relation, interval and gap here
 * is backed by a wire record the caller handed in; nothing is defaulted into
 * existence. Sessions without a usable start are dropped and counted, undated
 * transcript turns keep only their recorded order, and a relation exists only
 * when the parentage authority served the edge. The output is fully ordered so
 * that the same records in any wire order project to the same picture.
 */
import type {
  AnalyticsSubagentNodeV1,
  AnalyticsSubagentTreePayloadV1,
  FeedbackProximityEncounterV1,
  FeedbackProximityRelationV1,
  LcmMessageV1,
  LoomTemporalPayloadV1,
} from '../../contracts/generated.ts';
import { proximityThreadId, proximityTone } from '../proximity/proximity.ts';
import type {
  EvidenceGrade,
  JourneyEvent,
  JourneyEventKind,
  JourneyExtent,
  JourneyGap,
  JourneyInterval,
  JourneyLane,
  JourneyProjection,
  JourneyRelation,
  JourneyStats,
  LaneEndSource,
} from './types.ts';

export type HierarchyState = 'loading' | 'loaded' | 'unavailable';

export interface JourneySources {
  temporal: LoomTemporalPayloadV1;
  /** The subagent delegation tree, or null when not served. */
  hierarchy: AnalyticsSubagentTreePayloadV1 | null;
  hierarchyState: HierarchyState;
  /** The selected session's loaded transcript page, or null. */
  selected: { laneId: string; messages: readonly LcmMessageV1[] } | null;
  encounters: readonly FeedbackProximityEncounterV1[];
}

/** The provider-qualified identity the rest of the Loom selects by. */
export function laneIdOf(provider: string, sessionId: string): string {
  return JSON.stringify([provider, sessionId]);
}

/** The store ordinal when both sides carry one, otherwise the wire position.
 * Stable, and never a timestamp. The chain summary, the playback frames and
 * the field's transcript events all order through this one function. */
export function orderMessages<T extends { ordinal?: number | null }>(
  messages: readonly T[],
): T[] {
  return messages
    .map((message, index) => ({ message, index }))
    .sort((a, b) => {
      const left = a.message.ordinal;
      const right = b.message.ordinal;
      if (typeof left === 'number' && typeof right === 'number' && left !== right) {
        return left - right;
      }
      return a.index - b.index;
    })
    .map(({ message }) => message);
}

/** Minimum extent, as `extentOf` in the weave: a lone instant gets an hour. */
const MIN_EXTENT_SECONDS = 3600;
const DETAIL_MAX_CHARS = 140;

interface LaneDraft {
  id: string;
  sessionId: string;
  provider: string;
  label: string;
  agent: string | null;
  start: number;
  end: number | null;
  endSource: LaneEndSource;
  parentId: string | null;
  depth: number;
  isSubagent: boolean;
  messages: number;
  editedFilesRecorded: boolean;
  editedFileCount: number;
  models: string[];
}

function compareStrings(a: string, b: string): number {
  return a < b ? -1 : a > b ? 1 : 0;
}

function keyOf(provider: string, sessionId: string): string {
  return laneIdOf(provider || 'unknown', sessionId);
}

function isFinitePositive(value: unknown): value is number {
  return typeof value === 'number' && Number.isFinite(value) && value > 0;
}

function modelsOf(models: readonly { model: string | null }[]): string[] {
  const out: string[] = [];
  for (const row of models) {
    if (typeof row.model === 'string' && row.model.length > 0 && !out.includes(row.model)) {
      out.push(row.model);
    }
  }
  return out;
}

/** Collapse a turn to one printable line, or null when there is nothing. */
function oneLine(text: string | null | undefined): string | null {
  if (typeof text !== 'string') return null;
  const line = text.replace(/\s+/g, ' ').trim();
  if (line.length === 0) return null;
  return line.length > DETAIL_MAX_CHARS ? `${line.slice(0, DETAIL_MAX_CHARS - 1)}…` : line;
}

function transcriptKind(message: LcmMessageV1): JourneyEventKind {
  if (typeof message.tool_name === 'string' && message.tool_name.length > 0) return 'tool_call';
  if (message.role === 'user') return 'message_user';
  if (message.role === 'assistant') return 'message_assistant';
  return 'message_other';
}

function transcriptLabel(message: LcmMessageV1): string {
  if (typeof message.tool_name === 'string' && message.tool_name.length > 0) {
    return message.tool_name;
  }
  if (typeof message.role === 'string' && message.role.trim().length > 0) {
    return message.role.trim();
  }
  return 'role unrecorded';
}

function proximityGrade(relation: FeedbackProximityRelationV1): EvidenceGrade {
  switch (relation.relation_kind) {
    case 'overlapping_edit':
    case 'confirmed_conflict':
      return 'exact';
    case 'code_neighborhood_candidate':
    case 'shared_code_candidate':
      return 'inferred';
    default: {
      const unhandled: never = relation;
      return unhandled;
    }
  }
}

interface ToolCallAnchor {
  readonly eventId: string;
  readonly toolUseId: string;
  readonly label: string;
  readonly time: number | null;
}

interface ParentClaim {
  readonly parentSessionId: string;
  readonly toolUseId: string | null;
  readonly source: 'sessions row' | 'subagent tree';
}

function byStartThenId(a: LaneDraft, b: LaneDraft): number {
  return a.start - b.start || compareStrings(a.id, b.id);
}

function byTimeThenId(a: JourneyEvent, b: JourneyEvent): number {
  return (a.time ?? 0) - (b.time ?? 0) || compareStrings(a.id, b.id);
}

/* -------------------------------------------------------------------------
 * Lanes
 * ---------------------------------------------------------------------- */

function lanesFrom(temporal: LoomTemporalPayloadV1): {
  drafts: Map<string, LaneDraft>;
  undated: number;
} {
  const editedPaths = new Map<string, Set<string>>();
  for (const file of temporal.edited_files) {
    const key = keyOf(file.provider, file.session_id);
    const bucket = editedPaths.get(key);
    if (bucket) bucket.add(file.path);
    else editedPaths.set(key, new Set([file.path]));
  }

  const drafts = new Map<string, LaneDraft>();
  let undated = 0;
  for (const row of temporal.sessions) {
    const start = Number(row.started_at);
    if (!Number.isFinite(start) || start <= 0) {
      undated += 1;
      continue;
    }
    const provider = row.provider || 'unknown';
    const id = laneIdOf(provider, row.session_id);
    if (drafts.has(id)) continue;
    // An end equal to the start is the same instant recorded twice, not a
    // duration; only a strictly later reading measures an extent.
    const hasRecordedEnd = isFinitePositive(row.ended_at) && row.ended_at > start;
    const hasLastMessage = isFinitePositive(row.last_message_at) && row.last_message_at > start;
    const end = hasRecordedEnd ? row.ended_at : hasLastMessage ? row.last_message_at : null;
    const endSource: LaneEndSource = hasRecordedEnd
      ? 'session_end'
      : hasLastMessage
        ? 'last_message'
        : null;
    drafts.set(id, {
      id,
      sessionId: row.session_id,
      provider,
      label: row.title?.trim() || row.session_id,
      agent: null,
      start,
      end,
      endSource,
      parentId: null,
      depth: 0,
      isSubagent: row.is_subagent === true,
      messages: row.messages,
      editedFilesRecorded: row.edited_files_recorded === true,
      editedFileCount: editedPaths.get(id)?.size ?? 0,
      models: modelsOf(row.models),
    });
  }
  return { drafts, undated };
}

/** Depth-first from the provider groups: roots earliest first, each followed
 * by its subtree in preorder. Any lane a cycle kept away from a root is
 * appended afterwards so nothing served is lost. */
function orderLanes(drafts: readonly LaneDraft[]): LaneDraft[] {
  const byProvider = new Map<string, LaneDraft[]>();
  for (const draft of drafts) {
    const bucket = byProvider.get(draft.provider);
    if (bucket) bucket.push(draft);
    else byProvider.set(draft.provider, [draft]);
  }
  const providers = [...byProvider.keys()].sort(
    (a, b) =>
      (byProvider.get(b)?.length ?? 0) - (byProvider.get(a)?.length ?? 0) ||
      compareStrings(a, b),
  );
  const children = new Map<string, LaneDraft[]>();
  for (const draft of drafts) {
    if (draft.parentId === null) continue;
    const bucket = children.get(draft.parentId);
    if (bucket) bucket.push(draft);
    else children.set(draft.parentId, [draft]);
  }

  const out: LaneDraft[] = [];
  const seen = new Set<string>();
  const visit = (lane: LaneDraft): void => {
    if (seen.has(lane.id)) return;
    seen.add(lane.id);
    out.push(lane);
    for (const child of (children.get(lane.id) ?? []).sort(byStartThenId)) visit(child);
  };
  for (const provider of providers) {
    const roots = (byProvider.get(provider) ?? [])
      .filter((draft) => draft.parentId === null)
      .sort(byStartThenId);
    for (const root of roots) visit(root);
  }
  for (const draft of [...drafts].sort(byStartThenId)) visit(draft);
  return out;
}

/* -------------------------------------------------------------------------
 * Projection
 * ---------------------------------------------------------------------- */

export function projectJourney(sources: JourneySources): JourneyProjection {
  const { temporal, hierarchy, hierarchyState, selected, encounters } = sources;
  const { drafts, undated } = lanesFrom(temporal);

  const relations: JourneyRelation[] = [];
  const gaps: JourneyGap[] = [];
  const spawnEvents: JourneyEvent[] = [];

  // Tool calls the selected transcript page carries, by the host's own
  // tool-use id. A fork or an edit binds to one only through that identity
  // (or, for an edit, its recorded second); the first recorded call wins.
  const toolCalls = new Map<string, ToolCallAnchor>();
  const toolCallsBySecond = new Map<string, ToolCallAnchor[]>();
  if (selected && drafts.has(selected.laneId)) {
    for (const message of orderMessages(selected.messages)) {
      const toolUseId = message.tool_use_id?.trim();
      if (!toolUseId || transcriptKind(message) !== 'tool_call') continue;
      const key = JSON.stringify([selected.laneId, toolUseId]);
      if (toolCalls.has(key)) continue;
      const time = isFinitePositive(message.timestamp) ? message.timestamp : null;
      const anchor = { eventId: `msg:${selected.laneId}:${message.message_id}`, toolUseId, label: transcriptLabel(message), time };
      toolCalls.set(key, anchor);
      if (time === null) continue;
      const secondKey = JSON.stringify([selected.laneId, time]);
      const bucket = toolCallsBySecond.get(secondKey);
      if (bucket) bucket.push(anchor);
      else toolCallsBySecond.set(secondKey, [anchor]);
    }
  }

  // --- parentage -----------------------------------------------------------
  // Two sources name a parent: the session row's own `parent_session_id`
  // column, and the subagent tree. Where both speak they must agree; a
  // disagreement is drawn as AMBIGUOUS with both candidates, never merged.
  const claims = new Map<string, { row: ParentClaim | null; tree: ParentClaim | null }>();
  const claimFor = (laneId: string) => {
    let entry = claims.get(laneId);
    if (!entry) claims.set(laneId, (entry = { row: null, tree: null }));
    return entry;
  };
  for (const row of temporal.sessions) {
    const parent = row.parent_session_id?.trim();
    const laneId = keyOf(row.provider, row.session_id);
    if (!parent || !drafts.has(laneId)) continue;
    claimFor(laneId).row = { parentSessionId: parent, toolUseId: row.parent_tool_use_id?.trim() || null, source: 'sessions row' };
  }
  const nodeByLane = new Map<string, AnalyticsSubagentNodeV1>();
  for (const node of hierarchy?.nodes ?? []) {
    nodeByLane.set(keyOf(node.provider, node.session_id), node);
  }
  for (const [laneId, node] of nodeByLane) {
    const child = drafts.get(laneId);
    if (!child) continue;
    child.agent = node.agent;
    switch (node.link) {
      case 'root':
        break;
      case 'linked':
        if (node.parent_session_id !== null) {
          claimFor(laneId).tree = { parentSessionId: node.parent_session_id, toolUseId: node.parent_tool_use_id, source: 'subagent tree' };
        }
        break;
      case 'missing_parent':
        gaps.push({
          id: `gap:parent_outside_page:${child.id}`,
          laneId: child.id,
          kind: 'parent_outside_page',
          grade: 'unavailable',
          detail: `recorded parent ${node.parent_session_id ?? 'unrecorded'} was never ingested`,
        });
        break;
      case 'cycle':
        gaps.push({
          id: `gap:parent_cycle:${child.id}`,
          laneId: child.id,
          kind: 'parent_cycle',
          grade: 'ambiguous',
          detail: `recorded parent ${node.parent_session_id ?? 'unrecorded'} closes a delegation cycle`,
        });
        break;
      default: {
        const unhandled: never = node.link;
        return unhandled;
      }
    }
  }
  for (const [laneId, { row, tree }] of claims) {
    const child = drafts.get(laneId);
    const primary = row ?? tree;
    if (!child || !primary) continue;
    const both = row !== null && tree !== null ? { row, tree } : null;
    const parentsDiffer = both !== null && both.row.parentSessionId !== both.tree.parentSessionId;
    const toolsDiffer =
      both !== null &&
      !parentsDiffer &&
      both.row.toolUseId !== null &&
      both.tree.toolUseId !== null &&
      both.row.toolUseId !== both.tree.toolUseId;
    if (both !== null && (parentsDiffer || toolsDiffer)) {
      gaps.push({
        id: `gap:parentage_conflict:${child.id}`,
        laneId: child.id,
        kind: 'parentage_conflict',
        grade: 'ambiguous',
        detail: parentsDiffer
          ? `sessions row names parent ${both.row.parentSessionId}; subagent tree names ${both.tree.parentSessionId}`
          : `sessions row names tool use ${both.row.toolUseId}; subagent tree names ${both.tree.toolUseId}`,
      });
    }
    const candidates = both !== null && parentsDiffer ? [both.row, both.tree] : [primary];
    for (const claim of candidates) {
      const parent = drafts.get(keyOf(child.provider, claim.parentSessionId));
      if (!parent) {
        gaps.push({
          id: `gap:parent_outside_page:${child.id}:${claim.source}`,
          laneId: child.id,
          kind: 'parent_outside_page',
          grade: 'unavailable',
          detail: `${claim.source} names parent ${claim.parentSessionId}, outside this loaded page`,
        });
        continue;
      }
      if (child.parentId === null) child.parentId = parent.id;
      const precedes = child.start < parent.start;
      const agreed = both !== null && !parentsDiffer;
      // The fork sits on the parent's tool call only when a loaded parent
      // message carries the recorded tool-use id; otherwise at the child's
      // start, saying why.
      const anchor =
        claim.toolUseId === null ? undefined : toolCalls.get(JSON.stringify([parent.id, claim.toolUseId]));
      const placement = anchor
        ? `fork placed on the spawning tool call ${anchor.label}`
        : claim.toolUseId === null
          ? 'fork placed at the child start: no parent tool-use id recorded'
          : selected?.laneId !== parent.id
            ? 'fork placed at the child start: the parent transcript is not loaded'
            : `fork placed at the child start: no loaded parent tool call carries ${claim.toolUseId}`;
      const grade: EvidenceGrade =
        parentsDiffer || toolsDiffer || precedes ? 'ambiguous' : anchor ? 'exact' : 'inferred';
      const basis = [
        agreed ? 'sessions row and subagent tree agree' : claim.source,
        `parent_session_id · parent_tool_use_id ${claim.toolUseId ?? 'unrecorded'}`,
        placement,
        parentsDiffer ? 'the other source names a different parent' : null,
        toolsDiffer ? 'the sources name different tool uses' : null,
        precedes ? 'child start precedes parent start' : null,
      ]
        .filter((part): part is string => part !== null)
        .join(' · ');
      const suffix = parentsDiffer ? `:${claim.source}` : '';
      if (anchor) {
        // The tool-call glyph is the fork's mark; no second spawn mark.
        relations.push({ id: `rel:spawn:${child.id}${suffix}`, kind: 'spawn', fromLaneId: parent.id, toLaneId: child.id, time: anchor.time, grade, basis, fromEventId: anchor.eventId });
        continue;
      }
      relations.push({ id: `rel:spawn:${child.id}${suffix}`, kind: 'spawn', fromLaneId: parent.id, toLaneId: child.id, time: child.start, grade, basis });
      spawnEvents.push({
        id: `spawn:${child.id}${suffix}`,
        laneId: parent.id,
        kind: 'spawn',
        time: child.start,
        sequence: 0,
        grade,
        source: 'parentage',
        label: child.label,
        detail: basis,
        ref: child.id,
      });
    }
  }
  // A join only where the page shows it: the child's measured end inside its
  // parent's measured extent. No result record backs it, so it is inferred.
  for (const child of drafts.values()) {
    const parent = child.parentId === null ? undefined : drafts.get(child.parentId);
    if (!parent || child.end === null || parent.end === null) continue;
    if (child.end < parent.start || child.end > parent.end) continue;
    relations.push({
      id: `rel:rejoin:${child.id}`,
      kind: 'rejoin',
      fromLaneId: child.id,
      toLaneId: parent.id,
      time: child.end,
      grade: 'inferred',
      basis: `child ${child.endSource === 'session_end' ? 'recorded end' : 'last message'} inside the parent's measured extent · no result or handoff record in this read`,
    });
  }

  if (hierarchy === null) {
    gaps.push({
      id: 'gap:parentage_unavailable:state',
      laneId: null,
      kind: 'parentage_unavailable',
      grade: 'unavailable',
      detail: `parentage authority ${hierarchyState} · no delegation tree was served`,
    });
  } else if (!hierarchy.available || hierarchy.error) {
    gaps.push({
      id: 'gap:parentage_unavailable:state',
      laneId: null,
      kind: 'parentage_unavailable',
      grade: 'unavailable',
      detail: `parentage authority ${hierarchyState} · ${hierarchy.error ?? 'tree reports unavailable'}`,
    });
  }
  if (hierarchy?.truncated) {
    gaps.push({
      id: 'gap:parentage_unavailable:truncated',
      laneId: null,
      kind: 'parentage_unavailable',
      grade: 'unavailable',
      detail: `parentage authority truncated · ${hierarchy.missing_parent_count} missing parents · ${hierarchy.cycle_count} cycles`,
    });
  }
  gaps.push({
    id: 'gap:handoff_unavailable',
    laneId: null,
    kind: 'handoff_unavailable',
    grade: 'unavailable',
    detail:
      'no handoff or result authority is bound to session identity in this read; a join is drawn only where a child ends inside its parent\'s measured extent, graded inferred',
  });

  // --- depth (guarded against a cycle the authority did not flag) ----------
  for (const draft of drafts.values()) {
    let depth = 0;
    const seen = new Set<string>([draft.id]);
    let cursor = draft.parentId;
    while (cursor !== null && !seen.has(cursor)) {
      seen.add(cursor);
      depth += 1;
      cursor = drafts.get(cursor)?.parentId ?? null;
    }
    draft.depth = depth;
  }

  const untimedEditPaths = new Map<string, Set<string>>();
  for (const file of temporal.edited_files) {
    if (file.edited_at_micros != null) continue;
    const laneId = keyOf(file.provider, file.session_id);
    const bucket = untimedEditPaths.get(laneId);
    if (bucket) bucket.add(file.path);
    else untimedEditPaths.set(laneId, new Set([file.path]));
  }
  const ordered = orderLanes([...drafts.values()]);
  const laneIndex = new Map(ordered.map((lane, index) => [lane.id, index] as const));

  for (const lane of ordered) {
    // An edit the rollup recorded without an integer time has no honest x;
    // the lane says how many of its edited files it cannot place.
    const untimed = untimedEditPaths.get(lane.id)?.size ?? 0;
    if (untimed > 0) {
      gaps.push({
        id: `gap:edit_time_unrecorded:${lane.id}`,
        laneId: lane.id,
        kind: 'edit_time_unrecorded',
        grade: 'unavailable',
        detail: `${untimed} edited ${untimed === 1 ? 'file' : 'files'} recorded · no edit time in this read`,
      });
    }
    if (lane.end === null) {
      gaps.push({
        id: `gap:extent_unknown:${lane.id}`,
        laneId: lane.id,
        kind: 'extent_unknown',
        grade: 'unavailable',
        detail: 'no recorded end or later message observation',
      });
    }
  }

  // --- events ----------------------------------------------------------------
  const recordedByLane = new Map<string, JourneyEvent[]>();
  const pushRecorded = (event: JourneyEvent): void => {
    const bucket = recordedByLane.get(event.laneId);
    if (bucket) bucket.push(event);
    else recordedByLane.set(event.laneId, [event]);
  };
  for (const lane of ordered) {
    pushRecorded({
      id: `start:${lane.id}`,
      laneId: lane.id,
      kind: 'session_start',
      time: lane.start,
      sequence: 0,
      grade: 'exact',
      source: 'session',
      label: 'start',
      detail: null,
      ref: lane.sessionId,
    });
    if (lane.end !== null) {
      const inferred = lane.endSource === 'last_message';
      pushRecorded({
        id: `end:${lane.id}`,
        laneId: lane.id,
        kind: 'session_end',
        time: lane.end,
        sequence: 0,
        grade: inferred ? 'inferred' : 'exact',
        source: 'session',
        label: 'end',
        detail: inferred ? 'last message observation, not a recorded session end' : null,
        ref: lane.sessionId,
      });
    }
  }
  for (const commit of temporal.commits) {
    const laneId = keyOf(commit.provider, commit.session_id);
    if (!laneIndex.has(laneId)) continue;
    const detail =
      `${commit.relation} · ${commit.evidence}` +
      (commit.span_overlap_kind ? ` · ${commit.span_overlap_kind}` : '');
    pushRecorded({
      id: `commit:${laneId}:${commit.commit_sha}`,
      laneId,
      kind: 'commit',
      time: commit.committed_at,
      sequence: 0,
      grade: commit.evidence === 'transcript' ? 'exact' : 'inferred',
      source: 'commit',
      label: commit.commit_sha.slice(0, 7),
      detail,
      ref: commit.commit_sha,
    });
  }
  for (const file of temporal.edited_files) {
    const laneId = keyOf(file.provider, file.session_id);
    if (!laneIndex.has(laneId) || file.edited_at_micros == null) continue;
    // An edit binds to the one loaded tool call recorded in its second; two
    // calls in that second name no single call, so neither is linked.
    const sameSecond = toolCallsBySecond.get(JSON.stringify([laneId, Math.floor(file.edited_at_micros / 1_000_000)]));
    const call = sameSecond?.length === 1 ? sameSecond[0] : undefined;
    const parts = [
      file.change_type,
      file.hunks === null ? null : `${file.hunks} ${file.hunks === 1 ? 'hunk' : 'hunks'}`,
      file.path,
      call ? `tool call ${call.label} ${call.toolUseId}` : null,
    ];
    pushRecorded({
      id: `edit:${laneId}:${file.path}:${file.edited_at_micros}`,
      laneId,
      kind: 'file_edit',
      time: file.edited_at_micros / 1_000_000,
      sequence: 0,
      grade: 'exact',
      source: 'file_rollup',
      label: file.path.split('/').pop() || file.path,
      detail: parts.filter((part): part is string => part !== null).join(' · '),
      ref: file.path,
      ...(call ? { linkedEventId: call.eventId } : {}),
    });
  }
  for (const event of spawnEvents) pushRecorded(event);

  const transcriptByLane = new Map<string, JourneyEvent[]>();
  if (selected && laneIndex.has(selected.laneId)) {
    const laneId = selected.laneId;
    const orderedMessages = orderMessages(selected.messages);
    const events: JourneyEvent[] = orderedMessages.map((message, sequence) => ({
      id: `msg:${laneId}:${message.message_id}`,
      laneId,
      kind: transcriptKind(message),
      time: isFinitePositive(message.timestamp) ? message.timestamp : null,
      sequence,
      grade: 'exact',
      source: 'transcript',
      label: transcriptLabel(message),
      detail: oneLine(message.snippet) ?? oneLine(message.content),
      ref: message.message_id,
    }));
    transcriptByLane.set(laneId, events);
    const undatedTurns = events.filter((event) => event.time === null).length;
    if (undatedTurns > 0) {
      gaps.push({
        id: `gap:undated_events:${laneId}`,
        laneId,
        kind: 'undated_events',
        grade: 'unavailable',
        detail: `${undatedTurns} of ${events.length} loaded turns carry no timestamp; placed in recorded order`,
      });
    }
  }

  const events: JourneyEvent[] = [];
  for (const lane of ordered) {
    const recorded = (recordedByLane.get(lane.id) ?? []).sort(byTimeThenId);
    recorded.forEach((event, sequence) => events.push({ ...event, sequence }));
    for (const event of transcriptByLane.get(lane.id) ?? []) events.push(event);
  }

  // --- intervals -------------------------------------------------------------
  const intervals: JourneyInterval[] = [];
  const intervalIds = new Set<string>();
  const pushInterval = (interval: JourneyInterval): void => {
    if (intervalIds.has(interval.id)) return;
    intervalIds.add(interval.id);
    intervals.push(interval);
  };
  for (const span of temporal.branch_spans) {
    const laneId = keyOf(span.provider, span.session_id);
    if (!laneIndex.has(laneId)) continue;
    pushInterval({
      id: `span:${laneId}:${span.first_at}:${span.worktree}`,
      laneId,
      kind: 'git_span',
      start: span.first_at,
      end: span.last_at,
      label: `${span.branch ?? 'branch unrecorded'} · ${span.worktree}`,
      grade: 'exact',
      tone: null,
      ref: null,
    });
  }
  for (const encounter of encounters) {
    encounter.participants.forEach((_participant, index) => {
      const laneId = proximityThreadId(encounter, index);
      if (laneId === null || !laneIndex.has(laneId)) return;
      pushInterval({
        id: `prox:${encounter.encounter_id}:${laneId}`,
        laneId,
        kind: 'proximity',
        start: encounter.interval.start / 1_000_000,
        end: encounter.interval.end / 1_000_000,
        label: encounter.relation.relation_kind.replaceAll('_', ' '),
        grade: proximityGrade(encounter.relation),
        tone: proximityTone(encounter.relation),
        ref: encounter.encounter_id,
      });
    });
  }
  intervals.sort(
    (a, b) =>
      (laneIndex.get(a.laneId) ?? 0) - (laneIndex.get(b.laneId) ?? 0) ||
      a.start - b.start ||
      compareStrings(a.id, b.id),
  );

  relations.sort((a, b) => (a.time ?? 0) - (b.time ?? 0) || compareStrings(a.id, b.id));
  gaps.sort((a, b) => {
    const left = a.laneId === null ? -1 : (laneIndex.get(a.laneId) ?? Number.MAX_SAFE_INTEGER);
    const right = b.laneId === null ? -1 : (laneIndex.get(b.laneId) ?? Number.MAX_SAFE_INTEGER);
    return left - right || compareStrings(a.id, b.id);
  });

  // --- extent ------------------------------------------------------------------
  let extent: JourneyExtent | null = null;
  if (ordered.length > 0) {
    let start = Infinity;
    let end = -Infinity;
    for (const lane of ordered) {
      if (lane.start < start) start = lane.start;
      if (lane.start > end) end = lane.start;
      if (lane.end !== null && lane.end > end) end = lane.end;
    }
    for (const event of events) {
      if (event.time !== null && event.time > end) end = event.time;
    }
    for (const interval of intervals) {
      if (interval.end > end) end = interval.end;
    }
    if (end - start < MIN_EXTENT_SECONDS) end = start + MIN_EXTENT_SECONDS;
    extent = { start, end };
  }

  // --- stats -------------------------------------------------------------------
  const providerStats = new Map<string, { id: string; lanes: number; messages: number }>();
  for (const lane of ordered) {
    const entry = providerStats.get(lane.provider) ?? { id: lane.provider, lanes: 0, messages: 0 };
    entry.lanes += 1;
    entry.messages += lane.messages;
    providerStats.set(lane.provider, entry);
  }
  const stats: JourneyStats = {
    lanes: ordered.length,
    roots: ordered.filter((lane) => lane.parentId === null).length,
    subagents: ordered.filter((lane) => lane.isSubagent).length,
    messages: ordered.reduce((sum, lane) => sum + lane.messages, 0),
    openEnded: ordered.filter((lane) => lane.end === null).length,
    hollow: ordered.filter((lane) => lane.messages === 0).length,
    undated,
    providers: [...providerStats.values()].sort(
      (a, b) => b.lanes - a.lanes || compareStrings(a.id, b.id),
    ),
  };

  const lanes: JourneyLane[] = ordered.map((lane) => ({
    id: lane.id,
    sessionId: lane.sessionId,
    provider: lane.provider,
    label: lane.label,
    agent: lane.agent,
    start: lane.start,
    end: lane.end,
    endSource: lane.endSource,
    parentId: lane.parentId,
    depth: lane.depth,
    isSubagent: lane.isSubagent,
    messages: lane.messages,
    editedFilesRecorded: lane.editedFilesRecorded,
    editedFileCount: lane.editedFileCount,
    models: lane.models,
  }));

  return { lanes, events, relations, gaps, intervals, extent, stats };
}
