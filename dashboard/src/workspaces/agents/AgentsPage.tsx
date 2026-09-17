import { useCallback, useMemo, useState } from 'react';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { ReadFailure } from '../../ui/LegacyStates.tsx';
import { ReadSection, envelopeReadState } from '../../ui/ReadSection.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { MeterRow, Panel, WorkspaceHeader } from '../../ui/instrument.tsx';
import {
  AnalyticsAgentsPayloadV1Schema,
  AnalyticsDiagnosticsPayloadV1Schema,
  AnalyticsSubagentTreePayloadV1Schema,
  AnalyticsUnderusedPayloadV1Schema,
  AnalyticsUsageSummaryV1Schema,
  type AnalyticsAgentUsageV1,
  type AnalyticsSubagentNodeV1,
  type AnalyticsSubagentTreePayloadV1,
} from '../../contracts/generated.ts';
import { envelopePayload, useEnvelope } from '../../data/query/useEnvelope.ts';
import { logFraction } from '../../viz/scale.ts';
import { AgentAuthorityRegister } from './AgentAuthorityRegister.tsx';
import { AgentFailureContext } from './AgentFailureContext.tsx';
import { AgentHandoffs } from './AgentHandoffs.tsx';
import { AgentHandoffTokens } from './AgentHandoffTokens.tsx';
import { AgentInspector, type DiagnosticsForInspector } from './AgentInspector.tsx';
import { AgentTelemetryRegister } from './AgentTelemetryRegister.tsx';
import { DelegationTopology } from './DelegationTopology.tsx';
import { SubagentTree } from './SubagentTree.tsx';
import { resolveSubject } from './agentInspector.ts';
import { useAgentWorkGraph } from './agentWorkQuery.ts';
import {
  failureAuthority,
  hierarchyAuthority,
  tokenAuthority,
  usageAuthority,
  workAuthority,
} from './authorityRegister.ts';
import { fitDelegationTopology, markId } from './delegationTopology.ts';
import { readAttemptFailures } from './failure.ts';
import { readHandoffFrontier } from './handoff.ts';
import { newestTreeSession, useAgentHandoffTokens } from './handoffTokenQuery.ts';
import { readHandoffTokens } from './handoffTokens.ts';

const BASE = '/api/plugins/analytics';

/**
 * Agents: who delegated to whom, read from the authorities that record it.
 *
 * The composition is the V2 Agents plate. A register of five independent
 * authorities leads — usage, hierarchy, tokens, Work, failure — each with its
 * own state and never a total over them. The hero aperture is the delegation
 * topology: the daemon's subagent tree laid out left to right by generation,
 * hover inspecting and click selecting, with the exact tree beneath it as the
 * synchronized fallback. A workspace-owned inspector reads the inspected or
 * selected session across every authority, and the ledgers that used to be
 * the page — handoff frontier, token frontier, failure context, telemetry —
 * remain beneath as the exact evidence the field summarizes.
 *
 * Selection is the only act that reads. Hovering a session changes nothing but
 * the inspector's subject; selecting one names the session the token-frontier
 * route is asked about. With nothing selected the page asks about the newest
 * root and says so, rather than drawing an empty frontier.
 */
export function AgentsPage() {
  // The analytics family is envelope-only; every payload decodes with its
  // generated contract schema.
  const usage = useEnvelope(
    ['analytics', 'usage'],
    `${BASE}/usage`,
    AnalyticsUsageSummaryV1Schema,
    // The hook-analytics fold is ~14s against a real store. Once is enough per
    // visit; a refetch interval here would keep a reader's browser and the
    // daemon both busy for no new reading.
    { staleTime: 5 * 60_000 },
  );
  const underused = useEnvelope(
    ['analytics', 'underused'],
    `${BASE}/underused`,
    AnalyticsUnderusedPayloadV1Schema,
  );
  // `/diagnostics` is the only endpoint on this plugin that carries a clock —
  // `events_per_hour` over the counted window, and the most recent events with
  // their own timestamps. It is also the slowest (it folds the full
  // hook-analytics JSONL), so it lives behind its own boundary and the fast
  // plates render without waiting for it.
  const diagnostics = useEnvelope(
    ['analytics', 'diagnostics'],
    `${BASE}/diagnostics`,
    AnalyticsDiagnosticsPayloadV1Schema,
    { staleTime: 5 * 60_000 },
  );
  // Cheap session-store query, separate from the hook-analytics fold: which
  // managed subagents were delegated to, counted in sessions.
  const agents = useEnvelope(
    ['analytics', 'agents'],
    `${BASE}/agents`,
    AnalyticsAgentsPayloadV1Schema,
  );
  // The delegation EDGES, which the rollup above cannot carry: it counts
  // sessions per agent, and a count of two islands never recovers the arrow
  // between them. Served on its own route for the same reason.
  const subagentTree = useEnvelope(
    ['analytics', 'subagent-tree'],
    `${BASE}/subagent-tree`,
    AnalyticsSubagentTreePayloadV1Schema,
  );

  const [inspectedId, setInspectedId] = useState<string | null>(null);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [expanded, setExpanded] = useState<ReadonlySet<string>>(() => new Set());
  const toggleExpanded = useCallback((id: string) => {
    setExpanded((current) => {
      const next = new Set(current);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  }, []);
  const select = useCallback((id: string) => {
    setSelectedId((current) => (current === id ? null : id));
  }, []);

  const treePayload = envelopePayload(subagentTree.data) ?? null;
  const treeAvailable: AnalyticsSubagentTreePayloadV1 | null =
    treePayload !== null && treePayload.available ? treePayload : null;
  const fit = useMemo(
    () => (treeAvailable === null ? null : fitDelegationTopology(treeAvailable, expanded)),
    [treeAvailable, expanded],
  );
  // The selected session by identity, looked up in the reading rather than in
  // the drawing: a bundle closing over it or a depth fold must not unselect it.
  const selectedNode = useMemo<AnalyticsSubagentNodeV1 | null>(() => {
    if (selectedId === null || treeAvailable === null) return null;
    return treeAvailable.nodes.find((node) => markId(node) === selectedId) ?? null;
  }, [selectedId, treeAvailable]);

  // The token frontier is read for a session this page can actually name: the
  // selected one, or — with nothing selected — the newest top of the tree, and
  // the inspector says which. Without a session there is no question to ask,
  // and the surface says so rather than drawing an empty frontier.
  const newestSession = newestTreeSession(treePayload);
  const frontierSession = selectedNode?.session_id ?? newestSession;
  const handoffTokens = useAgentHandoffTokens(frontierSession);
  const tokenReading = readHandoffTokens(
    frontierSession,
    handoffTokens.isPending ? undefined : handoffTokens.data,
  );
  // The work-product graph, read once and read twice: the handoff frontier and
  // the attempt failures both come off this single response, so the two
  // describe one graph version rather than two versions captioned as one.
  const workGraph = useAgentWorkGraph();
  const handoffFrontier = readHandoffFrontier(workGraph.isPending ? undefined : workGraph.data);
  const attemptFailures = readAttemptFailures(workGraph.isPending ? undefined : workGraph.data);

  const authorities = [
    usageAuthority(usage.isPending, usage.data),
    hierarchyAuthority(subagentTree.isPending, subagentTree.data),
    tokenAuthority(tokenReading),
    workAuthority(handoffFrontier, attemptFailures),
    failureAuthority(diagnostics.isPending, diagnostics.data),
  ];
  const hierarchy = authorities[1]!;

  // Three different reasons for having nothing to inspect, kept apart: a read
  // still in flight, a store that answered and declared itself unavailable,
  // and a transport that never delivered a reading.
  const subject =
    fit === null
      ? ({
          kind: 'none',
          detail: subagentTree.isPending
            ? 'the delegation tree is still being read'
            : treePayload !== null
              ? `the session store declared itself unavailable${treePayload.error ? `: ${treePayload.error}` : ''}`
              : 'the delegation tree could not be read',
        } as const)
      : resolveSubject(fit.model, inspectedId, selectedId, newestSession, selectedNode !== null);
  // The frontier reading belongs to exactly one session. Any other subject is
  // being inspected without a read having been asked for it, and says so.
  const tokensForSubject =
    subject.kind === 'session' && subject.mark.node.session_id === frontierSession
      ? tokenReading
      : null;
  const diagnosticsPayload = envelopePayload(diagnostics.data);
  const diagnosticsForInspector: DiagnosticsForInspector = diagnostics.isPending
    ? { state: 'pending' }
    : diagnosticsPayload === undefined
      ? {
          state: 'refused',
          detail:
            diagnostics.data?.outcome === 'transport'
              ? (diagnostics.data.detail ?? 'analytics diagnostics could not be read')
              : 'analytics diagnostics could not be read',
        }
      : diagnosticsPayload.available === false
        ? { state: 'unavailable' }
        : {
            state: 'read',
            hooks: diagnosticsPayload.recent_hooks,
            truncated: diagnosticsPayload.hook_window.truncated,
          };

  return (
    <div
      className="flex h-full flex-col overflow-auto"
      tabIndex={0}
      role="region"
      aria-label="Agents content"
      onKeyDown={(event) => {
        if (event.key === 'Escape') setInspectedId(null);
      }}
    >
      <WorkspaceHeader
        path="agents"
        title="Agents"
        note="delegation topology from the session store · handoffs, tokens and failures from their own authorities"
      />

      <AgentAuthorityRegister authorities={authorities} />

      {/* The hero is the topology: who delegated to whom, by generation. The
        * inspector beside it is workspace-owned and shows only what a hover,
        * focus or selection in the field asked for. */}
      <section aria-label="Delegation topology" className="flex shrink-0 flex-col lg:flex-row">
        <div className="flex min-w-0 flex-1 flex-col gap-2 p-2">
          <Panel
            legend="Delegation topology · read-only"
            // Withdrawn below `sm`: the register above already prints this
            // reading, and in a fixed-height header the detail wraps over the
            // legend at 320px.
            actions={
              <StateChip kind={hierarchy.kind} detail={hierarchy.detail} className="max-sm:hidden" />
            }
            elevation="well"
            bodyClassName="p-2"
          >
            <ReadSection
              title="Delegation edges"
              chrome="centered"
              state={envelopeReadState(subagentTree.isPending, subagentTree.data, {
                loading: 'reading subagent tree',
                transport: 'subagent tree could not be read',
              })}
            >
              {(envelope) => {
                const payload = envelope.payload;
                if (payload.available === false) {
                  return (
                    <ReadFailure
                      label="Subagent tree unavailable"
                      detail={payload.error ?? envelope.coverage.omission_reasons[0]}
                    />
                  );
                }
                return (
                  <div className="flex min-w-0 flex-col gap-3">
                    {fit !== null && fit.model.marks.length > 0 ? (
                      <DelegationTopology
                        fit={fit}
                        interaction={{
                          inspectedId,
                          selectedId,
                          onInspect: setInspectedId,
                          onSelect: select,
                          expanded,
                          onToggleExpanded: toggleExpanded,
                        }}
                      />
                    ) : null}
                    <details
                      className="border-t border-edge-subtle pt-2"
                      open={payload.nodes.length === 0}
                      data-agent-exact-tree={payload.nodes.length}
                    >
                      <summary className="td-legend min-h-[var(--touch-target-min)] cursor-pointer content-center text-text-secondary hover:text-text-primary">
                        exact tree · {payload.sessions_read.toLocaleString()} sessions · keyboard
                        fallback
                      </summary>
                      <div className="pt-2">
                        <SubagentTree
                          payload={payload}
                          selectedId={selectedId}
                          onSelect={(node) => select(markId(node))}
                        />
                      </div>
                    </details>
                  </div>
                );
              }}
            </ReadSection>
          </Panel>
        </div>
        <aside
          aria-label="Inspector"
          className="flex w-full shrink-0 flex-col border-t border-edge-subtle bg-surface-1 lg:w-[22rem] lg:border-l lg:border-t-0 xl:w-[24rem]"
        >
          <div className="flex h-8 shrink-0 items-center gap-2.5 border-b border-edge-subtle px-2.5">
            <span className="td-title">Inspector</span>
            <span aria-hidden className="td-rule" />
            <span className="td-legend">
              {subject.kind === 'session'
                ? subject.mode === 'inspecting'
                  ? 'hover · focus'
                  : subject.mode === 'selected'
                    ? 'selection'
                    : 'default'
                : subject.kind === 'bundle'
                  ? 'bundle'
                  : 'idle'}
            </span>
          </div>
          <AgentInspector
            subject={subject}
            model={fit?.model ?? EMPTY_MODEL}
            source={treeAvailable?.source ?? 'session store'}
            tokens={tokensForSubject}
            handoffs={handoffFrontier}
            failures={attemptFailures}
            diagnostics={diagnosticsForInspector}
            onClearSelection={() => setSelectedId(null)}
            onToggleExpanded={toggleExpanded}
          />
        </aside>
      </section>

      {/* The exact ledgers the field summarizes, each behind its own read. */}
      <section aria-label="Delegation ledgers" className="shrink-0 border-t border-edge-subtle">
        <OverviewGrid>
          <OverviewCard title="Agent groups">
            <ReadSection
              title="Subagents"
              chrome="centered"
              state={envelopeReadState(agents.isPending, agents.data, {
                loading: 'reading subagent sessions',
                transport: 'subagent sessions could not be read',
              })}
            >
              {(envelope) => {
                const payload = envelope.payload;
                return payload == null || payload.available === false ? (
                  <ReadFailure
                    label="Subagent sessions unavailable"
                    detail={envelope.coverage.omission_reasons[0]}
                  />
                ) : (
                  <SubagentSessions rows={payload.by_agent} source={payload.source} />
                );
              }}
            </ReadSection>
          </OverviewCard>

          <OverviewCard title="Handoff frontier">
            <AgentHandoffs reading={handoffFrontier} />
          </OverviewCard>

          <OverviewCard title="Handoff tokens">
            <AgentHandoffTokens reading={tokenReading} />
          </OverviewCard>

          <OverviewCard title="Failure context">
            <ReadSection
              title="Failures"
              chrome="centered"
              state={envelopeReadState(diagnostics.isPending, diagnostics.data, {
                loading: 'reading analytics diagnostics',
                transport: 'analytics diagnostics could not be read',
              })}
            >
              {(envelope) => {
                const payload = envelope.payload;
                return payload.available === false ? (
                  <ReadFailure label="Analytics diagnostics unavailable" />
                ) : (
                  <AgentFailureContext
                    outcomes={payload.by_outcome}
                    recentEvents={payload.recent_events}
                    attempts={attemptFailures}
                  />
                );
              }}
            </ReadSection>
          </OverviewCard>
        </OverviewGrid>
      </section>

      <AgentTelemetryRegister
        usagePending={usage.isPending}
        usageResult={usage.data}
        diagnostics={diagnostics}
        underused={underused}
      />
    </div>
  );
}

/** The model the inspector is handed before the tree has been read. */
const EMPTY_MODEL = fitDelegationTopology({
  available: true,
  source: '',
  error: null,
  nodes: [],
  sessions_read: 0,
  root_count: 0,
  edge_count: 0,
  max_depth: 0,
  missing_parent_count: 0,
  cycle_count: 0,
  truncated: false,
}).model;

/** Sessions per managed subagent, straight from the session store. A count of
 * delegations, not of work done inside them — that context lives in Loom's
 * per-thread drill-down. */
function SubagentSessions({
  rows,
  source,
}: {
  rows: readonly AnalyticsAgentUsageV1[];
  source: string;
}) {
  if (rows.length === 0) {
    return (
      <p className="text-2xs text-text-muted">
        no subagent sessions are recorded in the session store
      </p>
    );
  }
  const ranked = [...rows].sort((a, b) => b.sessions - a.sessions);
  const ceiling = ranked[0]?.sessions ?? 0;
  return (
    <figure className="flex flex-col gap-1.5">
      <figcaption className="td-legend">
        sessions per managed subagent · source: {source} · log scale
      </figcaption>
      {ranked.map((row) => (
        <MeterRow
          key={row.agent}
          label={row.agent}
          title={row.agent}
          fraction={logFraction(row.sessions, ceiling)}
          value={row.sessions.toLocaleString()}
        />
      ))}
    </figure>
  );
}
