import type { ReactNode } from 'react';
import type { AnalyticsRecentHookV1 } from '../../contracts/generated.ts';
import { EvidenceGrade } from '../../ui/EvidenceGrade.tsx';
import { StateChip } from '../../ui/StateChip.tsx';
import { cn } from '../../ui/cn';
import { formatMicrosUtc, formatStamp } from '../../ui/format.ts';
import {
  incomingDelegation,
  outgoingDelegations,
  sessionHooks,
  type InspectorSubject,
  type SubjectMode,
} from './agentInspector.ts';
import type { DelegationTopologyModel } from './delegationTopology.ts';
import type { AttemptFailureReading } from './failure.ts';
import type { AgentHandoffReading } from './handoff.ts';
import { handoffTargetLabel, type HandoffTokenReading } from './handoffTokens.ts';
import { subagentElapsedSeconds } from './subagentTree.ts';

/**
 * The Agents inspector: one session (or one bundle) read across every
 * authority the page holds, each section labelled with the authority it comes
 * from and the grade of what it can say.
 *
 * The sections are independent on purpose. The session store, the grant
 * store, the work-product graph and the analytics fold are four reads with
 * four states, and a reader looking at a session needs to see which of them
 * answered. Where an authority cannot be joined to a session at all — Work
 * handoffs are recorded against actor principals, attempts against tasks and
 * runs — the section says so as a typed gap rather than showing an empty list
 * that reads as "nothing happened".
 */

/** The analytics diagnostics read, reduced to what the inspector joins on. */
export type DiagnosticsForInspector =
  | { readonly state: 'pending' }
  | { readonly state: 'refused'; readonly detail: string }
  | { readonly state: 'unavailable' }
  | {
      readonly state: 'read';
      readonly hooks: readonly AnalyticsRecentHookV1[];
      readonly truncated: boolean;
    };

export function AgentInspector({
  subject,
  model,
  source,
  tokens,
  handoffs,
  failures,
  diagnostics,
  onClearSelection,
  onToggleExpanded,
}: {
  subject: InspectorSubject;
  model: DelegationTopologyModel;
  /** The subagent tree's own `source`. */
  source: string;
  /** The token frontier for the subject's session, or `null` when this
   * subject is only being inspected and no read was requested for it. */
  tokens: HandoffTokenReading | null;
  handoffs: AgentHandoffReading;
  failures: AttemptFailureReading;
  diagnostics: DiagnosticsForInspector;
  onClearSelection: () => void;
  onToggleExpanded: (id: string) => void;
}) {
  if (subject.kind === 'none') {
    return (
      <div className="flex h-full flex-col gap-3 p-3" data-agent-inspector="none">
        <h2 className="td-title">Inspector</h2>
        <p className="text-2xs leading-relaxed text-text-muted">{subject.detail}</p>
        <p className="text-3xs leading-relaxed text-text-muted">
          Hover or focus inspects and changes nothing. Click or Enter selects; a selection is
          the only act that reads a session's token frontier. Escape clears inspection.
        </p>
      </div>
    );
  }

  if (subject.kind === 'bundle') {
    const { mark } = subject;
    const shown = mark.members.slice(0, 12);
    return (
      <div className="flex h-full flex-col gap-3 p-3" data-agent-inspector="bundle" data-agent-inspector-id={mark.id}>
        <header className="flex flex-col gap-0.5">
          <span className="td-legend">
            gen {mark.generation} · bundle · inspecting
          </span>
          <h2 className="td-title text-text-primary">
            {mark.basis === 'remainder' ? mark.label : `${mark.sessions} × ${mark.label}`}
          </h2>
        </header>
        <Section legend="Folded siblings" grade={<EvidenceGrade grade="EXACT" source="RETAINED" />}>
          <p className="text-2xs leading-relaxed text-text-secondary">
            {mark.sessions} sessions grouped by{' '}
            {mark.basis === 'remainder'
              ? 'overflow past the fan-out limit'
              : mark.basis === 'agent'
                ? 'agent label'
                : 'provider, no agent recorded'}
            {mark.descendants > 0 ? `, with ${mark.descendants} sessions beneath them` : ''}. Nothing
            beneath is drawn until the bundle is opened.
          </p>
          <ul className="flex flex-col">
            {shown.map((member) => {
              const elapsed = subagentElapsedSeconds(member);
              return (
                <li
                  key={`${member.provider}:${member.session_id}`}
                  className="flex min-w-0 items-baseline gap-2 border-b border-edge-subtle py-1 text-2xs last:border-b-0"
                >
                  <span className="min-w-0 flex-1 truncate text-text-primary" title={member.session_id}>
                    {member.agent ?? member.title ?? member.session_id}
                  </span>
                  <span className="td-value shrink-0 text-3xs text-text-muted">
                    {member.descendants > 0 ? `${member.descendants} below · ` : ''}
                    {elapsed != null ? `${elapsed.toLocaleString()}s` : 'span unrecorded'}
                  </span>
                </li>
              );
            })}
          </ul>
          {mark.members.length > shown.length ? (
            <p className="text-3xs text-text-muted">
              {mark.members.length - shown.length} more sessions in this bundle are listed in the
              exact tree below the field.
            </p>
          ) : null}
        </Section>
        {/* No open control here: a bundle is only ever inspected, and this
          * panel is gone the moment the pointer leaves the field for it. The
          * bundle's own mark opens it, by click or Enter. */}
        <p className="text-3xs leading-relaxed text-text-muted">
          Click the bundle in the field, or press Enter on it, to draw its members.
        </p>
      </div>
    );
  }

  const { mark, mode } = subject;
  const node = mark.node;
  const incoming = incomingDelegation(mark, model);
  const outgoing = outgoingDelegations(mark, model);
  const elapsed = subagentElapsedSeconds(node);

  return (
    <div
      className="flex h-full flex-col gap-3 p-3"
      data-agent-inspector="session"
      data-agent-inspector-id={mark.id}
      data-agent-inspector-mode={mode}
    >
      <header className="flex items-start gap-2">
        <span className="flex min-w-0 flex-1 flex-col gap-0.5">
          <span className="td-legend">
            gen {mark.generation} · {node.provider} ·{' '}
            {mode === 'inspecting' ? 'inspecting' : mode === 'selected' ? 'selected' : 'default · newest top'}
          </span>
          <h2 className="td-title truncate text-text-primary" title={node.session_id}>
            {mark.label}
          </h2>
        </span>
        {mode === 'selected' ? (
          <button
            type="button"
            onClick={onClearSelection}
            aria-label="Clear selection"
            className="-my-2 flex size-[var(--touch-target-min)] shrink-0 items-center justify-center text-text-muted hover:text-text-primary"
          >
            ×
          </button>
        ) : null}
      </header>

      <Section legend="Source authority" grade={<EvidenceGrade grade="EXACT" source="RETAINED" />}>
        <Facts
          rows={[
            ['agent', node.agent ?? 'unrecorded'],
            ['session', node.session_id],
            ['provider', node.provider],
            ['title', node.title ?? 'untitled'],
            ['subagent', node.is_subagent ? 'yes' : 'no'],
            ['store', source],
          ]}
        />
      </Section>

      <Section legend="Delegated by" grade={<EvidenceGrade grade={incoming.grade} source="RETAINED" />}>
        <p className="text-2xs leading-relaxed text-text-secondary" data-agent-inspector-incoming={incoming.kind}>
          {incoming.detail}
        </p>
        {incoming.kind === 'linked' ? (
          <Facts
            rows={[
              ['parent', incoming.parent ? incoming.parent.label : incoming.parentSessionId],
              ['parent session', incoming.parentSessionId],
              ['tool call', incoming.toolUseId ?? 'unrecorded'],
            ]}
          />
        ) : null}
      </Section>

      <Section legend="Delegates to" grade={<EvidenceGrade grade="EXACT" source="RETAINED" />}>
        {outgoing.length === 0 && mark.foldedDescendants === 0 ? (
          <p className="text-2xs leading-relaxed text-text-muted">
            none — the store records no session beneath this one
          </p>
        ) : (
          <>
            <ul className="flex flex-col" data-agent-inspector-outgoing={outgoing.length}>
              {outgoing.map((edge) => (
                <li
                  key={edge.to.id}
                  className="flex min-w-0 flex-col border-b border-edge-subtle py-1 last:border-b-0"
                >
                  <span className="truncate text-2xs text-text-primary">
                    {edge.to.kind === 'bundle'
                      ? `${edge.to.sessions} × ${edge.to.label} (bundle)`
                      : edge.to.label}
                  </span>
                  <span className="td-value truncate text-3xs text-text-muted">
                    {edge.to.kind === 'session'
                      ? edge.toolUseId === null
                        ? `${edge.to.node.session_id} · tool call unrecorded`
                        : `${edge.to.node.session_id} · via ${edge.toolUseId}`
                      : `${edge.to.descendants} beneath the bundle`}
                  </span>
                </li>
              ))}
            </ul>
            {mark.foldedDescendants > 0 ? (
              <div className="flex items-center gap-2">
                <p className="text-2xs leading-relaxed text-text-muted">
                  {mark.foldedDescendants} {mark.foldedDescendants === 1 ? 'session' : 'sessions'}{' '}
                  beneath this one are folded by the depth limit.
                </p>
                <FoldButton onClick={() => onToggleExpanded(mark.id)} action="open-fold">
                  Open
                </FoldButton>
              </div>
            ) : null}
            {mark.depthOpened ? (
              <div className="flex items-center gap-2">
                <p className="text-2xs leading-relaxed text-text-muted">
                  Drawn past the depth limit because you opened it.
                </p>
                <FoldButton onClick={() => onToggleExpanded(mark.id)} action="fold-depth">
                  Fold
                </FoldButton>
              </div>
            ) : null}
            {mark.openedBundles.map((bundle) => (
              <div key={bundle.id} className="flex items-center gap-2">
                <p className="text-2xs leading-relaxed text-text-muted">
                  {bundle.sessions} × {bundle.label} drawn individually because you opened the bundle.
                </p>
                <FoldButton onClick={() => onToggleExpanded(bundle.id)} action="fold-bundle">
                  Fold
                </FoldButton>
              </div>
            ))}
          </>
        )}
      </Section>

      <Section legend="Span" grade={<EvidenceGrade grade="EXACT" source="RETAINED" />}>
        <Facts
          rows={[
            ['started', node.started_at == null ? 'unrecorded' : formatStamp(node.started_at)],
            ['ended', node.ended_at == null ? 'unrecorded · open or unrecorded end' : formatStamp(node.ended_at)],
            ['elapsed', elapsed == null ? 'unmeasurable' : `${elapsed.toLocaleString()} s`],
            ['descendants', node.descendants.toLocaleString()],
          ]}
        />
      </Section>

      <TokenFrontier tokens={tokens} mode={mode} />

      <Section
        legend="Work handoffs"
        grade={<EvidenceGrade grade="UNAVAILABLE" source="WORK GRAPH" />}
      >
        {handoffs.state === 'pending' ? (
          <StateChip kind="loading" detail="reading the work-product graph" />
        ) : handoffs.state === 'refused' ? (
          <StateChip kind={handoffs.chip} detail={handoffs.detail} />
        ) : (
          <StateChip
            kind={handoffs.handoffs.length === 0 ? 'complete_zero_findings' : 'ready'}
            detail={`${handoffs.handoffs.length} ${handoffs.handoffs.length === 1 ? 'handoff' : 'handoffs'} on graph version ${handoffs.graphVersion}`}
          />
        )}
        <p className="text-3xs leading-relaxed text-text-muted" data-agent-inspector-join="handoffs">
          Handoffs on the graph name actor principals, not sessions, so none can be attributed
          to this session by identity. The frontier ledger below the field lists them all.
        </p>
      </Section>

      <Section legend="Failure context" grade={<EvidenceGrade grade="UNAVAILABLE" source="RUNTIME" />}>
        {failures.state === 'pending' ? (
          <StateChip kind="loading" detail="reading runtime attempts" />
        ) : failures.state === 'refused' ? (
          <StateChip kind={failures.chip} detail={failures.detail} />
        ) : failures.coverage === 'unavailable' ? (
          <StateChip kind="unavailable" detail="the daemon could observe no attempt" />
        ) : (
          <StateChip
            kind={
              failures.coverage === 'partial'
                ? 'partial'
                : failures.failures.length === 0
                  ? 'complete_zero_findings'
                  : 'ready'
            }
            detail={`${failures.failures.length} unclean of ${failures.attempts} attempts`}
          />
        )}
        <p className="text-3xs leading-relaxed text-text-muted" data-agent-inspector-join="attempts">
          Attempts carry task and run identities, not a session id, so no failure can be pinned
          to this session by identity.
        </p>
      </Section>

      <RecentHooks diagnostics={diagnostics} sessionId={node.session_id} />
    </div>
  );
}

/** The grant store's per-session frontier, as the page read it — or the
 * reason it did not. */
function TokenFrontier({
  tokens,
  mode,
}: {
  tokens: HandoffTokenReading | null;
  mode: SubjectMode;
}) {
  if (tokens === null) {
    return (
      <Section legend="Token frontier" grade={<EvidenceGrade grade="UNAVAILABLE" source="GRANT STORE" />}>
        <p className="text-2xs leading-relaxed text-text-muted" data-agent-inspector-tokens="not-requested">
          Not requested for this session. {mode === 'inspecting' ? 'Hover inspects only — ' : ''}
          select it (click or Enter) to read which handoff tokens are outstanding, lapsed or
          redeemed.
        </p>
      </Section>
    );
  }
  switch (tokens.state) {
    case 'pending':
      return (
        <Section legend="Token frontier">
          <StateChip kind="loading" detail="reading the token frontier" />
        </Section>
      );
    case 'unasked':
      return (
        <Section legend="Token frontier" grade={<EvidenceGrade grade="UNAVAILABLE" source="GRANT STORE" />}>
          <p className="text-2xs leading-relaxed text-text-muted" data-agent-inspector-tokens="unasked">
            {tokens.detail}
          </p>
        </Section>
      );
    case 'refused':
      return (
        <Section legend="Token frontier" grade={<EvidenceGrade grade="UNAVAILABLE" source="GRANT STORE" />}>
          <div className="flex flex-col gap-1" data-agent-inspector-tokens="refused">
            <StateChip kind={tokens.chip} detail="token frontier" />
            <p className="text-3xs leading-relaxed text-text-muted">{tokens.detail}</p>
          </div>
        </Section>
      );
    case 'read': {
      const total = tokens.outstanding.length + tokens.lapsed.length + tokens.redeemed.length;
      const rows = [...tokens.outstanding, ...tokens.lapsed, ...tokens.redeemed].slice(0, 6);
      return (
        <Section legend="Token frontier" grade={<EvidenceGrade grade="EXACT" source="GRANT STORE" />}>
          <div className="flex flex-col gap-1.5" data-agent-inspector-tokens={total}>
            <Facts
              rows={[
                ['outstanding', tokens.outstanding.length.toLocaleString()],
                ['lapsed', tokens.lapsed.length.toLocaleString()],
                ['redeemed', tokens.redeemed.length.toLocaleString()],
                ['observed', formatMicrosUtc(tokens.observedAtMicros)],
              ]}
            />
            {total === 0 ? (
              <p className="text-3xs leading-relaxed text-text-muted">
                No token addressed to this reader in this session. The route is recipient-scoped,
                so this is not evidence that no token exists.
              </p>
            ) : (
              <ul className="flex flex-col">
                {rows.map((token) => (
                  <li
                    key={token.token_digest}
                    className="flex min-w-0 flex-col border-b border-edge-subtle py-1 last:border-b-0"
                    data-agent-inspector-token-state={token.state}
                  >
                    <span className="flex items-baseline gap-2">
                      <span className="min-w-0 flex-1 truncate text-2xs text-text-primary">
                        {handoffTargetLabel(token)}
                      </span>
                      <span className="td-legend shrink-0">{token.state}</span>
                    </span>
                    <span className="td-value truncate text-3xs text-text-muted">
                      issued {formatMicrosUtc(token.issued_at)} · expires {formatMicrosUtc(token.expires_at)}
                    </span>
                  </li>
                ))}
              </ul>
            )}
            {total > rows.length ? (
              <p className="text-3xs text-text-muted">
                {total - rows.length} more in the handoff-token ledger below the field.
              </p>
            ) : null}
            {tokens.truncated ? (
              <p className="text-3xs text-state-partial">the frontier was truncated by the daemon</p>
            ) : null}
          </div>
        </Section>
      );
    }
    default: {
      const unhandled: never = tokens;
      return unhandled;
    }
  }
}

/** Recent hooks joined to the session by exact id — a bounded tape, not a
 * history, and captioned as the tape it is. */
function RecentHooks({
  diagnostics,
  sessionId,
}: {
  diagnostics: DiagnosticsForInspector;
  sessionId: string;
}) {
  switch (diagnostics.state) {
    case 'pending':
      return (
        <Section legend="Recent hooks">
          <StateChip kind="loading" detail="reading analytics diagnostics" />
        </Section>
      );
    case 'refused':
      return (
        <Section legend="Recent hooks" grade={<EvidenceGrade grade="UNAVAILABLE" source="OBSERVED" />}>
          <p className="text-3xs leading-relaxed text-text-muted" data-agent-inspector-hooks="refused">
            {diagnostics.detail}
          </p>
        </Section>
      );
    case 'unavailable':
      return (
        <Section legend="Recent hooks" grade={<EvidenceGrade grade="UNAVAILABLE" source="OBSERVED" />}>
          <p className="text-3xs leading-relaxed text-text-muted" data-agent-inspector-hooks="unavailable">
            analytics diagnostics declared themselves unavailable
          </p>
        </Section>
      );
    case 'read': {
      const joined = sessionHooks(diagnostics.hooks, sessionId, diagnostics.truncated);
      return (
        <Section legend="Recent hooks" grade={<EvidenceGrade grade="EXACT" source="OBSERVED" />}>
          <div className="flex flex-col gap-1" data-agent-inspector-hooks={joined.matched.length}>
            {joined.matched.length === 0 ? (
              <p className="text-3xs leading-relaxed text-text-muted">
                None of the {joined.served} served recent hooks names this session. The tape is
                the latest {joined.served} only{joined.truncated ? ', from a truncated window' : ''};
                it is not this session's history.
              </p>
            ) : (
              <>
                <ul className="flex flex-col">
                  {joined.matched.slice(0, 8).map((hook, index) => (
                    <li
                      key={`${hook.ts_unix_ms ?? 'na'}-${index}`}
                      className="flex min-w-0 items-baseline gap-2 border-b border-edge-subtle py-1 text-2xs last:border-b-0"
                    >
                      <span className="td-value shrink-0 text-3xs text-text-muted">
                        {hook.ts_unix_ms == null ? '—' : formatStamp(hook.ts_unix_ms / 1000)}
                      </span>
                      <span className="min-w-0 flex-1 truncate text-text-primary" title={hook.tool_name}>
                        {hook.tool_name || hook.hook_name}
                      </span>
                      <span className="td-legend shrink-0">{hook.hook_name}</span>
                    </li>
                  ))}
                </ul>
                <p className="text-3xs leading-relaxed text-text-muted">
                  {joined.matched.length} of the {joined.served} served recent hooks, joined on
                  session id. A tape of the latest few, not a count of the session's calls.
                </p>
              </>
            )}
          </div>
        </Section>
      );
    }
    default: {
      const unhandled: never = diagnostics;
      return unhandled;
    }
  }
}

function FoldButton({
  onClick,
  action,
  children,
}: {
  onClick: () => void;
  action: string;
  children: ReactNode;
}) {
  return (
    <button
      type="button"
      className="td-hit shrink-0 border border-edge-subtle bg-surface-2 px-2 text-2xs text-text-primary hover:border-accent"
      onClick={onClick}
      data-agent-inspector-action={action}
    >
      {children}
    </button>
  );
}

function Section({
  legend,
  grade,
  children,
}: {
  legend: string;
  /** Omitted while a read is still in flight: no claim, no grade. */
  grade?: ReactNode;
  children: ReactNode;
}) {
  return (
    <section aria-label={legend} className="flex flex-col gap-1.5 border-t border-edge-subtle pt-2">
      <div className="flex items-center gap-2">
        <h3 className="td-legend text-text-secondary">{legend}</h3>
        <span aria-hidden className="td-rule" />
        {grade}
      </div>
      {children}
    </section>
  );
}

function Facts({ rows }: { rows: ReadonlyArray<readonly [string, string]> }) {
  return (
    <dl className="grid grid-cols-[minmax(4.5rem,7rem)_1fr] gap-x-2 gap-y-0.5 text-2xs">
      {rows.map(([label, value]) => (
        <div key={label} className="contents">
          <dt className="td-legend pt-px">{label}</dt>
          <dd className={cn('td-value min-w-0 break-all text-text-secondary')} title={value}>
            {value}
          </dd>
        </div>
      ))}
    </dl>
  );
}
