import { trackingCoverage } from "../data/tracked-workload";
import { useDemo } from "../app/workspace";
import { DeliverySnapshot } from "./snapshot";
import type { ReactNode } from "react";
import { AgentBranches, DenseFanoutState, HonestPartial, JourneyOverview, TemporalReplay } from "./journey";
import { FollowStory, DecisionToCode, ReviewCoverage } from "./review";
import { GlobalInbox, LocalFirst, ProjectInbox, UmbrellaGraph } from "./inbox";
import { DELIVERY_META, DELIVERY_STATE_IDS, type DeliveryStateId } from "./data";
import "./delivery.css";

export function DeliveryPage(props: {
  state: DeliveryStateId;
  onState: (id: DeliveryStateId) => void;
}) {
  const { mode, navigate } = useDemo();
  const context = new URLSearchParams(location.search);
  const workflow = context.get("workflow");
  const run = context.get("run");
  const task = context.get("task");
  const meta = DELIVERY_META[props.state];
  let interior: ReactNode;
  switch (props.state) {
    case "01":
      interior = <GlobalInbox />;
      break;
    case "02":
      interior = <ProjectInbox />;
      break;
    case "03":
      interior = <UmbrellaGraph />;
      break;
    case "04":
      interior = <JourneyOverview />;
      break;
    case "05":
      interior = <TemporalReplay />;
      break;
    case "06":
      interior = <AgentBranches />;
      break;
    case "07":
      interior = <HonestPartial />;
      break;
    case "08":
      interior = <ReviewCoverage />;
      break;
    case "09":
      interior = <FollowStory />;
      break;
    case "10":
      interior = <DecisionToCode />;
      break;
    case "11":
      interior = <LocalFirst />;
      break;
    case "12":
      interior = <DenseFanoutState />;
      break;
  }
  return (
    <section className="aperture dl-aperture" aria-label={mode === "snapshot" ? "Delivery tracked branch scope" : meta.kicker}>
      <header className="dl-topbar">
        <div className="lead">
          <div className="stack">
            <span className="proj">
              Project: <em>{mode === "snapshot" ? "indexed branch scope" : meta.scope}</em>
            </span>
            <h2>{mode === "snapshot" ? "DELIVERY / TRACKED BRANCHES" : meta.kicker}</h2>
          </div>
          <span className="note">{mode === "snapshot" ? "Indexed branches · linked PR context" : meta.scopeNote}</span>
        </div>
        <div className="trail">
          <span className={meta.provider === "read-only" ? "provchip" : "provchip is-off"}>
            🔒 {mode === "snapshot" ? (trackingCoverage.state === "unavailable" ? "SCOPE UNAVAILABLE" : "RECORDED BRANCH INDEX") : props.state === "03" ? "AUTHORED EXAMPLE" : meta.provider === "read-only" ? "READ-ONLY PROVIDER" : "PROVIDER NOT CONFIGURED"}
          </span>
          {mode === "fixture" && <label className="dl-view-select">View
            <select aria-label="Delivery view" value={props.state} onChange={(event) => props.onState(event.target.value as DeliveryStateId)}>
              {DELIVERY_STATE_IDS.map((id) => <option key={id} value={id}>{DELIVERY_META[id].slug.replaceAll("-", " ")}</option>)}
            </select>
          </label>}
        </div>
      </header>
      {workflow && <div className="dl-fixture-notice" role="note">Incoming workflow context: {workflow} · run {run ?? "not supplied"} · task {task ?? "not supplied"}. Navigation context only; no PR or readiness join is established. <button className="dl-textbutton" onClick={() => navigate("workflows", { workflow, ...(run ? { run } : {}), ...(task ? { task } : {}) })}>Return to workflow</button></div>}
      {mode === "snapshot" ? <DeliverySnapshot /> : <><div className="dl-fixture-notice">Authored design fixture · synthetic identities except the retained PR743 packet · no live provider writes</div>{interior}</>}
    </section>
  );
}
