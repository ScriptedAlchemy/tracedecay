import { Eye } from "lucide-react";
import type { Key } from "react";

import { Badge, Button } from "../sdk";
import { Spinner } from "../Spinner";
import { formatHistoryTime } from "./format";
import {
  automationArtifactPreview,
  automationRunStatusClass,
  automationRunSummary,
} from "./historyFormat";
import type {
  MemoryAutomationRunArtifact,
  MemoryAutomationRunArtifactPayloadResponse,
  MemoryAutomationRunArtifactsResponse,
  MemoryAutomationRunRecord,
} from "../types";

function ArtifactButton({
  runId,
  artifact,
  artifactLoading,
  onLoadArtifact,
}: {
  key?: Key;
  runId: string;
  artifact: MemoryAutomationRunArtifact;
  artifactLoading: string | null;
  onLoadArtifact: (runId: string, kind: string) => void;
}) {
  const loadingKey = `${runId}:${artifact.kind}`;
  const loading = artifactLoading === loadingKey;

  return (
    <Button
      size="xs"
      outlined
      disabled={loading}
      onClick={() => onLoadArtifact(runId, artifact.kind)}
      className="gap-1.5"
    >
      {loading ? <Spinner /> : <Eye className="h-3.5 w-3.5" />}
      {artifact.kind}
    </Button>
  );
}

export function AutomationRunsSection({
  runs,
  error,
  artifacts,
  artifact,
  artifactLoading,
  artifactError,
  onLoadArtifact,
}: {
  runs: MemoryAutomationRunRecord[];
  error: string;
  artifacts: MemoryAutomationRunArtifactsResponse | null;
  artifact: MemoryAutomationRunArtifactPayloadResponse | null;
  artifactLoading: string | null;
  artifactError: string;
  onLoadArtifact: (runId: string, kind: string) => void;
}) {
  const artifactPreview = automationArtifactPreview(artifact);

  return (
    <div className="border border-border bg-background/30 px-3 py-2">
      <div className="mb-1 text-[11px] uppercase tracking-[0.08em] text-text-tertiary">
        Automation runs
      </div>
      {error ? <div className="text-xs text-destructive">{error}</div> : null}
      {runs.length ? (
        <div className="flex flex-col gap-1.5">
          {runs.map((record) => (
            <div
              key={record.run_id}
              className="grid grid-cols-[7.5rem_5.5rem_minmax(0,1fr)] gap-2 font-mono-ui text-xs"
            >
              <span className="text-text-tertiary">
                {formatHistoryTime(record.completed_at || record.started_at)}
              </span>
              <span
                className={`truncate border px-1.5 py-0.5 text-center uppercase tracking-[0.08em] ${automationRunStatusClass(record.status)}`}
              >
                {record.status}
              </span>
              <div className="min-w-0">
                <div className="break-all text-text-tertiary">
                  {automationRunSummary(record)}
                </div>
                {record.artifacts?.length ? (
                  <div className="mt-1 flex flex-wrap gap-1">
                    {record.artifacts.map((runArtifact) => (
                      <ArtifactButton
                        key={`${record.run_id}:${runArtifact.kind}`}
                        runId={record.run_id}
                        artifact={runArtifact}
                        artifactLoading={artifactLoading}
                        onLoadArtifact={onLoadArtifact}
                      />
                    ))}
                  </div>
                ) : null}
              </div>
            </div>
          ))}
        </div>
      ) : (
        <div className="text-xs text-text-tertiary">
          No automation runs recorded yet.
        </div>
      )}
      {artifactError ? (
        <div className="mt-2 text-xs text-destructive">{artifactError}</div>
      ) : null}
      {artifact ? (
        <div className="mt-2 border border-border bg-background/40 px-3 py-2">
          <div className="mb-1 flex flex-wrap items-center gap-2 text-xs">
            <span className="font-mono-ui text-text-secondary">
              {artifact.run_id}
            </span>
            <Badge>{artifact.artifact.kind}</Badge>
            <span className="min-w-0 break-all font-mono-ui text-text-tertiary">
              {artifact.artifact.sha256}
            </span>
          </div>
          {artifacts?.run_id === artifact.run_id && artifacts.artifact_chain ? (
            <div className="mb-2 flex flex-wrap items-center gap-1.5 font-mono-ui text-[11px] text-text-tertiary">
              <Badge>
                {artifacts.artifact_chain.complete ? "chain complete" : "chain pending"}
              </Badge>
              {(artifacts.artifact_chain.present_kinds || []).map((kind) => (
                <span key={`${artifact.run_id}:${kind}`}>{kind}</span>
              ))}
            </div>
          ) : null}
          <pre className="max-h-56 overflow-auto whitespace-pre-wrap break-words font-mono-ui text-xs text-text-secondary">
            {artifactPreview}
          </pre>
        </div>
      ) : null}
    </div>
  );
}
