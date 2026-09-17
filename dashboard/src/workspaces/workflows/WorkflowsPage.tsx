import { useMemo, useState } from 'react';
import { Corners, Panel, Ticks, WorkspaceHeader } from '../../ui/instrument.tsx';
import {
  DecodedStepsPanel,
  SelectedDefinitionPanel,
  VersionTrackPanel,
} from './WorkflowDefinitionDetail.tsx';
import { LifecyclePanel } from './WorkflowLifecycle.tsx';
import { RegistryInspectPanel, WorkflowRegistryPanel } from './WorkflowRegistry.tsx';
import { RunLookupPanel } from './WorkflowRunLookup.tsx';
import {
  definitionKey,
  groupRegistry,
  recordReceipt,
  type LifecycleReceipt,
  type ReceiptLedger,
} from './workflowLedger.ts';
import {
  useWorkflowDefinitionHistory,
  useWorkflowDefinitions,
  useWorkflowRun,
} from './workflowQueries.ts';

/**
 * Workflows — channel fourteen: the definition lifecycle ledger.
 *
 * Three regions over the canonical `/application/workflow` routes. The
 * registry (list-definitions) names every stable identity and folds its
 * immutable versions; the selected definition decodes one version and reads
 * its version track (definition-history); the run column looks up one exact
 * run (get-run) and issues compare-and-swap lifecycle commands
 * (activate/retire/reject). Every rendered value is a decoded generated
 * contract; every value the plate shows and the contracts do not carry is
 * named as an absence. Hover inspects, click selects, and no control implies
 * a transition the daemon has not answered.
 */

export function WorkflowsPage() {
  const definitions = useWorkflowDefinitions();
  const [selectedKey, setSelectedKey] = useState<string | null>(null);
  const [inspectedId, setInspectedId] = useState<string | null>(null);
  const [runId, setRunId] = useState<string | null>(null);
  const [receipts, setReceipts] = useState<ReceiptLedger>(() => new Map());

  const listed = definitions.data?.outcome === 'value' ? definitions.data.value : null;
  const entries = useMemo(() => (listed === null ? [] : groupRegistry(listed)), [listed]);
  const registryKeys = useMemo(
    () =>
      new Set(
        (listed ?? []).map((definition) =>
          definitionKey(definition.definition_id, definition.definition_version),
        ),
      ),
    [listed],
  );

  const selectedDefinition =
    listed?.find(
      (definition) =>
        definitionKey(definition.definition_id, definition.definition_version) === selectedKey,
    ) ?? null;
  const selectedEntry =
    selectedDefinition === null
      ? null
      : entries.find((entry) => entry.definitionId === selectedDefinition.definition_id) ?? null;
  const inspectedEntry =
    inspectedId === null ? null : entries.find((entry) => entry.definitionId === inspectedId) ?? null;

  const history = useWorkflowDefinitionHistory(selectedDefinition?.definition_id ?? null);
  const run = useWorkflowRun(runId);

  const selectIdentity = (definitionId: string) => {
    const entry = entries.find((candidate) => candidate.definitionId === definitionId);
    if (entry === undefined) return;
    setSelectedKey(definitionKey(definitionId, entry.latest.definition_version));
  };
  const selectVersion = (definitionId: string, version: number) => {
    setSelectedKey(definitionKey(definitionId, version));
  };
  const onReceipt = (receipt: LifecycleReceipt) => {
    setReceipts((ledger) => recordReceipt(ledger, receipt));
  };

  return (
    <div className="min-w-0" data-testid="workflows-page">
      <WorkspaceHeader
        path="workflows"
        title="Workflows"
        note="definition ledger · immutable versions, pinned references, daemon-validated lifecycle, exact run lookup · /application/workflow"
      />

      <div
        role="region"
        aria-label="Workflows content"
        tabIndex={0}
        className="relative min-w-0 p-3"
      >
        <Corners />
        <Ticks />

        {/* Three bays at desktop width; one column when the viewport — or a
          * 200% zoom — cannot pay for three, so registry, detail and run stay
          * independently addressable rather than clipped. */}
        <div className="grid min-w-0 gap-3 xl:grid-cols-[minmax(18rem,22rem)_minmax(0,1fr)_minmax(20rem,26rem)]">
          <div className="flex min-w-0 flex-col gap-3">
            <WorkflowRegistryPanel
              result={definitions.data}
              pending={definitions.isPending}
              selectedId={selectedDefinition?.definition_id ?? null}
              inspectedId={inspectedId}
              receipts={receipts}
              onSelect={selectIdentity}
              onInspect={setInspectedId}
            />
            <RegistryInspectPanel
              entry={inspectedEntry ?? selectedEntry}
              mode={inspectedEntry !== null ? 'inspecting' : selectedEntry !== null ? 'selected' : 'idle'}
              receipts={receipts}
            />
          </div>

          <div className="flex min-w-0 flex-col gap-3">
            {selectedDefinition === null || selectedEntry === null ? (
              <Panel legend="Selected definition" bodyClassName="p-3">
                <p className="text-3xs text-text-muted">
                  {listed === null
                    ? 'The selected definition, its version track and its decoded steps appear here once the registry has answered.'
                    : listed.length === 0
                      ? 'The registry is empty in this scope, so there is nothing to select.'
                      : 'Select a registry row to decode one immutable version here.'}
                </p>
              </Panel>
            ) : (
              <>
                <SelectedDefinitionPanel
                  entry={selectedEntry}
                  definition={selectedDefinition}
                  receipts={receipts}
                />
                <VersionTrackPanel
                  definitionId={selectedDefinition.definition_id}
                  result={history.data}
                  pending={history.isPending}
                  selectedVersion={selectedDefinition.definition_version}
                  receipts={receipts}
                  onSelectVersion={(version) =>
                    selectVersion(selectedDefinition.definition_id, version)
                  }
                />
                <DecodedStepsPanel key={selectedKey} definition={selectedDefinition} />
              </>
            )}
          </div>

          <div className="flex min-w-0 flex-col gap-3">
            <RunLookupPanel
              runId={runId}
              result={run.data}
              pending={run.isPending}
              registryKeys={registryKeys}
              onLookup={setRunId}
              onSelectDefinition={selectVersion}
            />
            {selectedDefinition === null ? (
              <Panel legend="Lifecycle controls · daemon-validated CAS" bodyClassName="p-3">
                <p className="text-3xs text-text-muted">
                  Activate, retire and reject act on one selected immutable version. Nothing is
                  offered until a definition is selected.
                </p>
              </Panel>
            ) : (
              // Keyed so the staged revision and the last result belong to
              // exactly one version; the receipt ledger above outlives it.
              <LifecyclePanel
                key={selectedKey}
                definition={selectedDefinition}
                receipts={receipts}
                onReceipt={onReceipt}
              />
            )}
          </div>
        </div>
      </div>
    </div>
  );
}
