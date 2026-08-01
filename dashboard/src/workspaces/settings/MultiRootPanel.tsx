/**
 * The multi-root scope set, as the daemon reports it.
 *
 * `/api/capabilities` has carried a typed `MultiRootCapabilityV1` since the
 * route existed and nothing read it, so a mounted scope set — its id, its
 * revision, the digest that seals it and how many roots it holds — was
 * answered on every page load and shown nowhere.
 *
 * This panel is deliberately a reading and not a control. There is no route
 * that queries across a scope set: `MultiRootQueryReadModelV1` is generated
 * and unserved. A root pivot here would be a control with nothing behind it,
 * so the panel states the capability and then states that boundary, which is
 * the difference between "a scope set is mounted" and "this dashboard can
 * work across it".
 */
import { multiRootReading, useCapabilities } from '../../data/query/capabilities.ts';
import type { MultiRootReading } from '../../data/query/capabilities.ts';
import { CenteredState } from '../../ui/ReadSection.tsx';
import { Legend, Readout } from '../../ui/instrument.tsx';
import { elideStart } from '../../ui/format.ts';

export function MultiRootPanel() {
  const capabilities = useCapabilities();
  const result = capabilities.data;

  // A capability read that did not land says nothing about scope sets. It must
  // not read as "no scope set is mounted", which is a measurement.
  if (capabilities.isPending) {
    return <PanelFrame><p className="td-value text-3xs text-text-muted">reading capabilities…</p></PanelFrame>;
  }
  if (!result || result.outcome !== 'ok') {
    return (
      <PanelFrame>
        <p className="td-value text-3xs text-text-muted">
          The capability bundle did not answer, so whether a scope set is mounted is unknown.
        </p>
      </PanelFrame>
    );
  }

  return <PanelFrame><MultiRootBody reading={multiRootReading(result.data.multi_root)} /></PanelFrame>;
}

function MultiRootBody({ reading }: { reading: MultiRootReading }) {
  switch (reading.state) {
    // Older daemon: the bundle never mentioned the capability. Distinct from
    // a daemon that mentioned it in order to decline, which has a reason.
    case 'absent':
      return (
        <p className="td-value text-3xs text-text-muted" data-multi-root="absent">
          This daemon build reports no multi-root capability, so it holds no opinion about scope
          sets.
        </p>
      );
    case 'unavailable':
      return (
        <div data-multi-root="unavailable">
          <CenteredState
            title="No authorized scope set is mounted"
            kind="unavailable"
            detail={reading.reason}
          />
        </div>
      );
    case 'mounted':
      return (
        <div className="flex flex-col gap-3" data-multi-root="mounted">
          <div className="flex flex-wrap gap-4">
            <Readout label="roots" size="sm" value={String(reading.rootCount)} />
            <Readout label="revision" size="sm" value={String(reading.revision)} />
          </div>
          <dl className="flex flex-col gap-1">
            <Field term="scope set" detail={reading.scopeSetId} />
            {/* Elided from the start: a digest's tail is what distinguishes
             * two of them, and its head is shared boilerplate. */}
            <Field term="digest" detail={elideStart(reading.digest, 24)} />
          </dl>
          {!reading.federatedQueryMounted ? (
            <p className="td-value text-3xs text-text-muted">
              A scope set is mounted, but no route runs a query across it — this build serves no
              federated read model — so every reading on this dashboard is still one root.
            </p>
          ) : null}
        </div>
      );
    default: {
      const exhaustive: never = reading;
      return exhaustive;
    }
  }
}

function Field({ term, detail }: { term: string; detail: string }) {
  return (
    <div className="flex gap-2">
      <dt className="td-legend shrink-0">{term}</dt>
      <dd className="td-value min-w-0 break-all font-mono text-3xs text-text-secondary">
        {detail}
      </dd>
    </div>
  );
}

function PanelFrame({ children }: { children: React.ReactNode }) {
  return (
    // No `data-section`: that attribute is how `findConfigSection` locates a
    // configuration section for the jump index, and this is not one.
    <section
      aria-label="Multi-root scope set"
      className="flex shrink-0 flex-col gap-2 border-b border-edge-subtle p-3"
    >
      <Legend>Multi-root scope set</Legend>
      {children}
    </section>
  );
}
