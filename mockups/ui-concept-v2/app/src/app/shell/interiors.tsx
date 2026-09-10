import type { ComponentType } from "react";
import { DeliveryPage } from "../../delivery/DeliveryPage";
import { LoomPage } from "../../loom";
import { parseLoomState, type LoomStateId } from "../../loom/types";
import { parseDeliveryState, type SurfaceSlug } from "./surfaceChrome";
import { SessionsPage } from "../../sessions";
import { AgentsPage } from "../../agents";
import { CodePage } from "../../code";
import { KnowledgePage } from "../../knowledge";
import { AutomationsPage } from "../../automations";
import { ObservatoryPage } from "../../observatory";
import { CostsPage } from "../../costs";
import { SettingsPage } from "../../settings";
import { WorkPage } from "../../work";
import { WorkflowsPage } from "../../workflows";
import type { DeliveryStateId } from "../../delivery/data";

export type InteriorProps = {
  initialSessionId?: string;
  state?: string;
  onState?: (id: string) => void;
};
export type InteriorComponent = ComponentType<InteriorProps>;

function DeliveryInterior(props: InteriorProps) {
  const state = parseDeliveryState(props.state ?? null) as DeliveryStateId;
  return <DeliveryPage state={state} onState={(id) => props.onState?.(id)} />;
}

function LoomInterior(props: InteriorProps) {
  const state = parseLoomState(props.state ?? null);
  return <LoomPage state={state} onState={(id: LoomStateId) => props.onState?.(id)} />;
}

function SessionsInterior(props: InteriorProps) {
  return <SessionsPage initialSessionId={props.initialSessionId} />;
}

function AgentsInterior(props: InteriorProps) {
  return <AgentsPage initialSessionId={props.initialSessionId} />;
}

function CodeInterior(props: InteriorProps) {
  return <CodePage onState={props.onState} />;
}

function KnowledgeInterior(props: InteriorProps) {
  return <KnowledgePage onState={props.onState} />;
}

function AutomationsInterior(props: InteriorProps) {
  return <AutomationsPage state={props.state} onState={props.onState} />;
}

function ObservatoryInterior(_props: InteriorProps) {
  return <ObservatoryPage />;
}

function CostsInterior(_props: InteriorProps) {
  return <CostsPage />;
}

function SettingsInterior(_props: InteriorProps) {
  return <SettingsPage />;
}

function WorkInterior(props: InteriorProps) {
  return <WorkPage initialSessionId={props.initialSessionId} />;
}

function WorkflowsInterior(props: InteriorProps) {
  return <WorkflowsPage state={props.state} onState={props.onState} />;
}

/**
 * Canonical interiors under src/{slug}/. Explorer is the remaining
 * SurfacePage fallback mounted by SurfaceAperture.
 */
export const INTERIORS: Partial<Record<SurfaceSlug, InteriorComponent>> = {
  delivery: DeliveryInterior,
  loom: LoomInterior,
  sessions: SessionsInterior,
  agents: AgentsInterior,
  code: CodeInterior,
  knowledge: KnowledgeInterior,
  automations: AutomationsInterior,
  observatory: ObservatoryInterior,
  costs: CostsInterior,
  settings: SettingsInterior,
  work: WorkInterior,
  workflows: WorkflowsInterior,
};
