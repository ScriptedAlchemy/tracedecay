/**
 * The temporal execution field's vocabulary.
 *
 * Three modules share these types: `journey.ts` joins the real Loom
 * authorities into a `JourneyProjection`, `layout.ts` turns that projection
 * plus a viewport into a `TemporalSceneModel`, and the scene renderer draws
 * whatever the layout produced and decides nothing. The split is the honesty
 * boundary named in the V2 implementation reference: time is X, hierarchy is
 * Y, and coordinates never depend on which renderer is drawing them.
 *
 * Nothing in this file has a plausible default. If an authority did not carry
 * a quantity, it is `null` here and the surface says so.
 */

/* -------------------------------------------------------------------------
 * Evidence
 * ---------------------------------------------------------------------- */

/** The canonical evidence-grade ladder from `DESIGN-SYSTEM.md`. Ordered by
 * support, never a confidence percentage. */
export type EvidenceGrade =
  | 'exact'
  | 'explicit'
  | 'inferred'
  | 'ambiguous'
  | 'stale'
  | 'unavailable';

/** Where a record was persisted or observed. Orthogonal to the grade. */
export type SourceClass =
  | 'session'
  | 'transcript'
  | 'parentage'
  | 'commit'
  | 'git_span'
  | 'file_rollup'
  | 'proximity';

/* -------------------------------------------------------------------------
 * Journey projection — the joined input
 * ---------------------------------------------------------------------- */

/** How a lane's right edge was measured. `null` means the extent is unknown. */
export type LaneEndSource = 'session_end' | 'last_message' | null;

/** One session as a lane. The provider-qualified `id` is the selection key. */
export interface JourneyLane {
  /** `JSON.stringify([provider, session_id])` — the identity the rest of the
   * Loom already selects by. */
  readonly id: string;
  readonly sessionId: string;
  readonly provider: string;
  readonly label: string;
  /** Recorded agent label from the parentage authority, or null. */
  readonly agent: string | null;
  /** Epoch seconds. Measured. */
  readonly start: number;
  /** Epoch seconds, or null when the store served nothing usable. */
  readonly end: number | null;
  readonly endSource: LaneEndSource;
  /** Lane id of the recorded parent when it is in the loaded page and the
   * parentage link is `linked`; otherwise null. */
  readonly parentId: string | null;
  /** Nesting depth under its root within the loaded page. Roots are 0. */
  readonly depth: number;
  readonly isSubagent: boolean;
  /** Measured message count from the session store. Zero is a reading. */
  readonly messages: number;
  /** Whether an edited-files rollup exists for the session (even empty). */
  readonly editedFilesRecorded: boolean;
  /** Distinct edited paths in the loaded relation rows. */
  readonly editedFileCount: number;
  readonly models: readonly string[];
}

export type JourneyEventKind =
  | 'session_start'
  | 'session_end'
  | 'message_user'
  | 'message_assistant'
  | 'message_other'
  | 'tool_call'
  | 'spawn'
  | 'commit';

export const JOURNEY_EVENT_KINDS: readonly JourneyEventKind[] = [
  'session_start',
  'session_end',
  'message_user',
  'message_assistant',
  'message_other',
  'tool_call',
  'spawn',
  'commit',
];

/** One drawable record on a lane. */
export interface JourneyEvent {
  /** Stable across reloads: derived from the source record's own identity. */
  readonly id: string;
  readonly laneId: string;
  readonly kind: JourneyEventKind;
  /** Recorded epoch seconds, or null for an undated record. */
  readonly time: number | null;
  /** Position in the lane's own recorded order. This is the X basis for an
   * undated record and a tiebreaker for dated ones. */
  readonly sequence: number;
  readonly grade: EvidenceGrade;
  readonly source: SourceClass;
  /** Short printable name: tool name, role, short SHA, child label. */
  readonly label: string;
  /** One line of exact evidence or basis; never invented. */
  readonly detail: string | null;
  /** The source record's own identifier: message id, commit SHA, child lane id. */
  readonly ref: string;
}

export type JourneyRelationKind = 'spawn' | 'handoff' | 'rejoin' | 'result';

/** A named directed relation between two lanes. */
export interface JourneyRelation {
  readonly id: string;
  readonly kind: JourneyRelationKind;
  readonly fromLaneId: string;
  readonly toLaneId: string;
  /** Epoch seconds of the recorded event that evidences the relation. */
  readonly time: number | null;
  readonly grade: EvidenceGrade;
  /** The stated basis, e.g. `parent_session_id · parent_tool_use_id toolu_01`. */
  readonly basis: string;
}

export type JourneyGapKind =
  | 'parent_outside_page'
  | 'parent_cycle'
  | 'parentage_unavailable'
  | 'extent_unknown'
  | 'undated_events'
  | 'handoff_unavailable';

/** Something Loom cannot prove, kept spatially visible and selectable. */
export interface JourneyGap {
  readonly id: string;
  /** Lane the gap attaches to, or null for a page-wide gap. */
  readonly laneId: string | null;
  readonly kind: JourneyGapKind;
  readonly grade: 'ambiguous' | 'unavailable';
  readonly detail: string;
}

export type JourneyIntervalKind = 'git_span' | 'proximity';

/** A timed segment on a lane: a branch/worktree activity span, or a proximity
 * encounter interval on one participant. */
export interface JourneyInterval {
  readonly id: string;
  readonly laneId: string;
  readonly kind: JourneyIntervalKind;
  /** Epoch seconds. */
  readonly start: number;
  readonly end: number;
  readonly label: string;
  readonly grade: EvidenceGrade;
  /** Proximity tone, when `kind === 'proximity'`. */
  readonly tone: 'candidate' | 'overlap' | 'conflict' | null;
  /** The encounter id for proximity intervals, so selection can pivot. */
  readonly ref: string | null;
}

export interface JourneyExtent {
  readonly start: number;
  readonly end: number;
}

export interface JourneyStats {
  readonly lanes: number;
  readonly roots: number;
  readonly subagents: number;
  readonly messages: number;
  readonly openEnded: number;
  readonly hollow: number;
  /** Session rows dropped for lack of a usable start. */
  readonly undated: number;
  readonly providers: readonly { id: string; lanes: number; messages: number }[];
}

export interface JourneyProjection {
  readonly lanes: readonly JourneyLane[];
  readonly events: readonly JourneyEvent[];
  readonly relations: readonly JourneyRelation[];
  readonly gaps: readonly JourneyGap[];
  readonly intervals: readonly JourneyInterval[];
  /** Full time extent across every lane and dated event, or null. */
  readonly extent: JourneyExtent | null;
  readonly stats: JourneyStats;
}

/* -------------------------------------------------------------------------
 * Layout inputs
 * ---------------------------------------------------------------------- */

export interface SceneWindow {
  /** Epoch seconds. */
  readonly start: number;
  readonly end: number;
}

export interface SceneViewport {
  /** CSS pixels of the whole field, ruler gutter included. */
  readonly width: number;
  /** Pixels reserved left of the time axis for lane labels. */
  readonly left: number;
  /** Pixels reserved right of the time axis. */
  readonly right: number;
  readonly window: SceneWindow;
}

/** Semantic zoom is an information contract, not camera scale. */
export type SemanticZoom = 'workstream' | 'agent' | 'event';

/** The reveal boundary playback applies. Records after it are unrevealed. */
export interface RevealBoundary {
  /** Recorded epoch seconds of the active event, or null when it is undated. */
  readonly time: number | null;
  /** Lane the active event belongs to. */
  readonly laneId: string;
  /** The active event's own sequence in that lane. */
  readonly sequence: number;
}

export interface BranchState {
  /** Lanes the reader explicitly collapsed. */
  readonly collapsed: ReadonlySet<string>;
  /** Lanes the reader explicitly expanded on a dense page. */
  readonly expanded: ReadonlySet<string>;
}

export interface LayoutOptions {
  readonly viewport: SceneViewport;
  readonly zoom: SemanticZoom;
  readonly branches: BranchState;
  /** Selected lane, or null. Drives focus-plus-context. */
  readonly selectedLaneId: string | null;
  /** Selected event id, or null. */
  readonly selectedEventId: string | null;
  readonly reveal: RevealBoundary | null;
  /** Event kinds the reader hid. Visibility only, never source truth. */
  readonly hiddenKinds: ReadonlySet<JourneyEventKind>;
  /** Above this many lanes, roots with descendants start collapsed unless
   * explicitly expanded. */
  readonly denseLaneThreshold: number;
}

/* -------------------------------------------------------------------------
 * TemporalSceneModel — the layout output
 * ---------------------------------------------------------------------- */

export type XBasis = 'time' | 'sequence';

export type FocusTreatment = 'selected' | 'path' | 'context' | 'neutral';

export interface SceneLane {
  readonly id: string;
  /** `'bundle'` when the lane stands for a collapsed subtree. */
  readonly kind: 'session' | 'bundle';
  readonly label: string;
  readonly provider: string;
  readonly depth: number;
  /** Center line of the lane. */
  readonly y: number;
  /** Vertical room allocated to the lane. */
  readonly height: number;
  /** Pixel extent of the session on the time axis, clamped to the window. */
  readonly x0: number;
  readonly x1: number;
  /** Whether `x1` is a measured end or an open tail. */
  readonly endSource: LaneEndSource;
  readonly focus: FocusTreatment;
  /** True when this lane's events are drawn in-lane (event zoom). */
  readonly expanded: boolean;
  /** True when the lane's own recorded extent lies entirely outside the
   * window; still emitted so the label column stays stable. */
  readonly offscreen: boolean;
  /** Count of descendants hidden under this lane when collapsed. */
  readonly collapsedDescendants: number;
  /** Ordinal row from the top, for the exact table and minimap. */
  readonly row: number;
}

export interface SceneNode {
  readonly id: string;
  readonly laneId: string;
  readonly kind: JourneyEventKind;
  readonly x: number;
  readonly y: number;
  readonly xBasis: XBasis;
  readonly grade: EvidenceGrade;
  readonly source: SourceClass;
  readonly label: string;
  readonly detail: string | null;
  readonly ref: string;
  readonly selected: boolean;
  readonly focus: FocusTreatment;
  /** Half of the pixel gap to the nearest neighbour on the same lane, so a
   * dense lane never lets a later hit region cover an earlier centre. */
  readonly halfHit: number;
}

export type ScenePathKind =
  | 'lane'
  | 'spawn'
  | 'handoff'
  | 'rejoin'
  | 'result'
  | 'sequence';

/** A curve. `controls` is `[x0,y0,cx0,cy0,cx1,cy1,x1,y1]` for a cubic, or
 * `[x0,y0,x1,y1]` for a straight segment. */
export interface ScenePath {
  readonly id: string;
  readonly kind: ScenePathKind;
  readonly fromId: string;
  readonly toId: string;
  readonly grade: EvidenceGrade;
  readonly basis: string | null;
  readonly focus: FocusTreatment;
  readonly controls: readonly number[];
  /** Measured message weight in `[0,1]` for lane paths; null otherwise. */
  readonly weight: number | null;
}

export interface SceneCluster {
  readonly id: string;
  readonly laneId: string;
  readonly memberLaneIds: readonly string[];
  readonly x0: number;
  readonly x1: number;
  readonly y: number;
  readonly height: number;
  readonly counts: {
    readonly sessions: number;
    readonly subagents: number;
    readonly messages: number;
    readonly commits: number;
    readonly openEnded: number;
  };
  /** Evidence mix over the members' relations and extents. */
  readonly grades: Readonly<Partial<Record<EvidenceGrade, number>>>;
  readonly focus: FocusTreatment;
}

export interface SceneInterval {
  readonly id: string;
  readonly laneId: string;
  readonly kind: JourneyIntervalKind;
  readonly x0: number;
  readonly x1: number;
  readonly y: number;
  readonly label: string;
  readonly grade: EvidenceGrade;
  readonly tone: 'candidate' | 'overlap' | 'conflict' | null;
  readonly ref: string | null;
}

export interface SceneGap {
  readonly id: string;
  readonly laneId: string | null;
  readonly kind: JourneyGapKind;
  readonly grade: 'ambiguous' | 'unavailable';
  readonly detail: string;
  /** Anchor for the gap mark; null for a page-wide gap. */
  readonly x: number | null;
  readonly y: number | null;
}

export interface SceneRail {
  readonly id: string;
  readonly kind: 'provider';
  readonly label: string;
  readonly y0: number;
  readonly y1: number;
  readonly lanes: number;
}

export interface SceneTick {
  readonly x: number;
  readonly time: number;
  readonly label: string;
}

export interface SceneLabel {
  readonly id: string;
  readonly text: string;
  readonly x: number;
  readonly y: number;
  readonly anchor: 'start' | 'middle' | 'end';
  readonly priority: number;
  /** Labels in one group are collision-resolved against each other. */
  readonly group: string;
}

export interface MinimapBin {
  readonly x0: number;
  readonly x1: number;
  readonly events: number;
  readonly lanes: number;
}

export interface SceneMinimap {
  /** Bins span the full projection extent, not the window. */
  readonly bins: readonly MinimapBin[];
  readonly lanes: readonly { id: string; y: number; x0: number; x1: number; endSource: LaneEndSource }[];
  /** The current window projected into minimap pixels. */
  readonly window: { readonly x0: number; readonly x1: number };
  readonly width: number;
  readonly height: number;
}

export interface SceneCursor {
  readonly x: number;
  readonly laneId: string;
  readonly xBasis: XBasis;
}

export interface SceneCounts {
  readonly lanesTotal: number;
  readonly lanesVisible: number;
  readonly lanesCollapsed: number;
  readonly eventsTotal: number;
  readonly eventsDrawn: number;
  readonly eventsCulled: number;
  readonly eventsWithheld: number;
  readonly eventsFiltered: number;
  /** Events summarized rather than drawn: transcript turns on a lane that is
   * not expanded at this zoom, and spawn marks on a bundle whose children are
   * hidden inside it. */
  readonly eventsFolded: number;
  readonly relationsTotal: number;
  readonly relationsDrawn: number;
  readonly relationsWithheld: number;
}

export interface TemporalSceneModel {
  readonly viewport: SceneViewport;
  readonly zoom: SemanticZoom;
  readonly height: number;
  readonly lanes: readonly SceneLane[];
  readonly nodes: readonly SceneNode[];
  readonly paths: readonly ScenePath[];
  readonly clusters: readonly SceneCluster[];
  readonly intervals: readonly SceneInterval[];
  readonly gaps: readonly SceneGap[];
  readonly rails: readonly SceneRail[];
  readonly ticks: readonly SceneTick[];
  readonly labels: readonly SceneLabel[];
  readonly minimap: SceneMinimap;
  readonly cursor: SceneCursor | null;
  readonly counts: SceneCounts;
  /** True when the dense threshold made roots start collapsed. */
  readonly denseDefault: boolean;
}
