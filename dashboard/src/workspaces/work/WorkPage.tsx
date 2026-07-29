import { StateChip } from '../../ui/StateChip.tsx';
import { Corners, Legend, Panel, WorkspaceHeader } from '../../ui/instrument.tsx';

const CONTRACT_GATE_EXPLANATION =
  'No generated Work read model is available in this build. Kanban, DAG, timeline, causal, workload, runtime, and control state are withheld rather than inferred.';

export function WorkPage() {
  return (
    <div className="flex min-h-0 min-w-0 flex-1 flex-col overflow-hidden bg-surface-0">
      <WorkspaceHeader
        path="work"
        title="Work"
        note="Canonical task graph · contract gated"
      />

      <main className="min-w-0 flex-1 overflow-x-hidden overflow-y-auto p-3 sm:p-4">
        <div className="mx-auto flex min-h-full w-full max-w-5xl items-center justify-center">
          <Panel
            legend="Work contract gate"
            className="w-full"
            bodyClassName="p-0"
            footer={
              <p className="text-3xs tracking-[0.04em] text-text-muted">
                No Work projection or command is exposed without generated contract authority.
              </p>
            }
          >
            <div className="grid min-w-0 lg:grid-cols-[minmax(0,1.2fr)_minmax(16rem,0.8fr)]">
              <div className="min-w-0 p-4 sm:p-6 lg:p-8">
                <StateChip
                  kind="unsupported_schema"
                  detail="generated Work read model absent"
                />
                <p className="mt-5 text-3xs font-semibold uppercase tracking-[0.18em] text-text-muted">
                  Contract-bound workspace
                </p>
                <p className="mt-2 max-w-2xl text-sm leading-6 text-text-primary sm:text-base">
                  {CONTRACT_GATE_EXPLANATION}
                </p>
                <p className="mt-4 max-w-2xl text-xs leading-5 text-text-secondary">
                  The workspace shell remains visible so unavailable authority is explicit; its
                  data plane stays closed until the generated contract can represent canonical
                  Work state.
                </p>
              </div>

              <aside className="min-w-0 border-t border-edge-subtle bg-surface-0 p-4 sm:p-6 lg:border-l lg:border-t-0">
                <div className="relative min-w-0 border border-edge-subtle bg-surface-1 p-4">
                  <Corners />
                  <Legend
                    trailing={
                      <span className="text-3xs font-semibold uppercase tracking-[0.14em] text-state-unsupported-schema">
                        Withheld
                      </span>
                    }
                  >
                    Authority boundary
                  </Legend>

                  <dl className="mt-5 space-y-3 text-xs">
                    <div className="grid min-w-0 grid-cols-[minmax(0,1fr)_auto] gap-3 border-b border-edge-subtle pb-3">
                      <dt className="text-text-muted">Read model</dt>
                      <dd className="text-right font-medium text-text-primary">Not generated</dd>
                    </div>
                    <div className="grid min-w-0 grid-cols-[minmax(0,1fr)_auto] gap-3 border-b border-edge-subtle pb-3">
                      <dt className="text-text-muted">Projection state</dt>
                      <dd className="text-right font-medium text-text-primary">Not rendered</dd>
                    </div>
                    <div className="grid min-w-0 grid-cols-[minmax(0,1fr)_auto] gap-3">
                      <dt className="text-text-muted">Work commands</dt>
                      <dd className="text-right font-medium text-text-primary">Not exposed</dd>
                    </div>
                  </dl>
                </div>
              </aside>
            </div>
          </Panel>
        </div>
      </main>
    </div>
  );
}
