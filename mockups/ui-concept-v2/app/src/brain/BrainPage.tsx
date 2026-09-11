import { useEffect, useRef, useState } from "react";
import {
  HUB,
  PROJECTS,
  RECENCY_AXIS,
  SIGNAL_FAMILIES,
  SYNAPSE_EVENT,
  type BrainView,
  type ProjectBody,
} from "../data/fixtures";
import { BrainCanvas } from "./BrainCanvas";
import { BrainRendererBoundary } from "./BrainRendererBoundary";
import { BrainEvidence } from "./BrainEvidence";
import { DEFAULT_BODY_APPEARANCE, type BodyAppearance } from "./particles";
import { layoutField, type FieldLayout } from "./layout";
import { layoutRepoField, REPO_DESIGN_ZOOM } from "./repoLayout";
import { FiringTree } from "../concept/FiringTree";
import { NeuronLab } from "../concept/neuronLab/NeuronLab";
import { mountVoxelo, type VoxeloHandle } from "../concept/neuronLab/voxeloScene";
import { projectById } from "../concept/neuronLab/interior";
import type { BrainState, LabGrain } from "../concept/neuronLab/labUtil";
import "./brain.css";
import {useDemo} from "../app/workspace";
import {RepositoryAtlas} from "../structure";

export const VIEWS: { id: BrainView; label: string }[] = [
  { id: "overview", label: "Overview" },
  { id: "hover", label: "Hover" },
  { id: "repo-zoom", label: "Repo" },
  { id: "scoped", label: "Scoped" },
  { id: "synapse", label: "Synapse" },
  { id: "firing-tree", label: "Firing tree" },
  { id: "neuron-lab", label: "Neuron lab" },
];

export const BRAIN_CHIPS: { id: BrainState; label: string }[] = [
  { id: "overview", label: "Overview" },
  { id: "hover", label: "Hover" },
  { id: "repo-zoom", label: "Repo" },
  { id: "scoped", label: "Scoped" },
  { id: "synapse", label: "Synapse" },
];

export type BrainDim = "2d" | "3d";

const FIVE: BrainView[] = ["overview", "hover", "repo-zoom", "scoped", "synapse"];

function isFive(view: BrainView): view is BrainState {
  return FIVE.includes(view);
}

type BrainPageProps = {
  view: BrainView;
  setView: (v: BrainView) => void;
  dim: BrainDim;
  setDim: (d: BrainDim) => void;
  focused: ProjectBody;
  setFocusedId: (id: string) => void;
  scoped: boolean;
  setScoped: (v: boolean) => void;
  labScope: string | null;
  setLabScope: (id: string | null) => void;
  labGrain: LabGrain;
  setLabGrain: (g: LabGrain) => void;
};

export function BrainPage(props: BrainPageProps) {
  const [appearance, setAppearance] = useState<BodyAppearance>(DEFAULT_BODY_APPEARANCE);
  const {mode} = useDemo();
  if(mode === "snapshot") {
    const params = new URLSearchParams(location.search);
    return <RepositoryAtlas context="brain" initialSelection={params.get("node") ?? params.get("path") ?? undefined} />;
  }
  return (
    <BrainRendererBoundary
      key={`${props.dim}:${props.view}`}
      onOverview={() => {
        props.setDim("2d");
        props.setView("overview");
        props.setScoped(false);
      }}
    >
      <BrainPageContent {...props} appearance={appearance} setAppearance={setAppearance} />
    </BrainRendererBoundary>
  );
}

function BrainPageContent(props: BrainPageProps & {
  appearance: BodyAppearance;
  setAppearance: (appearance: BodyAppearance) => void;
}) {
  const fieldRef = useRef<FieldLayout | null>(null);
  const apertureRef = useRef<HTMLElement | null>(null);
  const voxeloHost = useRef<HTMLDivElement | null>(null);
  const voxelo = useRef<VoxeloHandle | null>(null);
  const [checkoutId, setCheckoutId] = useState<string | null>(null);
  const [activityRequest, setActivityRequest] = useState(0);
  const [field, setField] = useState<FieldLayout | null>(null);
  const [zoomMul, setZoomMul] = useState(props.view === "repo-zoom" ? REPO_DESIGN_ZOOM : 1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const [hoverId, setHoverId] = useState<string | null>(
    props.view === "hover" || props.view === "synapse" ? props.focused.id : null,
  );

  useEffect(() => {
    // 3D path: the canvas is not mounted, measure the aperture directly
    const el = apertureRef.current;
    if (!el) return;
    const measure = () => {
      if (fieldRef.current) return;
      const next = layoutField(el.clientWidth, el.clientHeight);
      fieldRef.current = next;
      setField(next);
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, []);

  const five = isFive(props.view);
  const use3d = props.dim === "3d" && five;
  const scopeId =
    props.view === "repo-zoom" || props.view === "scoped" ? props.focused.id : null;
  const scopedProject = projectById(scopeId ?? "") ?? props.focused;

  useEffect(() => {
    if (!use3d) {
      voxelo.current?.dispose();
      voxelo.current = null;
      return;
    }
    const el = voxeloHost.current;
    if (!el) return;
    const handle = mountVoxelo(el, {
      brainState: props.view as BrainState,
      scope: scopeId,
      focusId: props.view === "hover" || props.view === "synapse" ? SYNAPSE_EVENT.projectId : null,
      onFocus: (id) => {
        if (id && id !== "repo:git_common_dir") props.setFocusedId(id);
      },
      onEnterScope: (id) => {
        props.setFocusedId(id);
        props.setScoped(true);
        props.setView("scoped");
      },
    });
    voxelo.current = handle;
    return () => {
      handle.dispose();
      voxelo.current = null;
    };
  }, [use3d, props.view, scopeId]);

  useEffect(() => {
    if (props.view === "scoped") apertureRef.current?.focus();
  }, [props.view]);

  const effectiveHover =
    props.view === "synapse" ? SYNAPSE_EVENT.projectId : hoverId;

  const labels = field?.bodies ?? [];
  const hub = field?.hub;
  const concept = props.view === "firing-tree" || props.view === "neuron-lab";
  const showField =
    !concept &&
    !use3d &&
    (props.view === "overview" || props.view === "hover" || props.view === "synapse");
  const showRecencyChrome =
    !concept && (props.view === "overview" || props.view === "hover" || props.view === "synapse");

  const repoField =
    props.view === "repo-zoom" && !use3d && field
      ? layoutRepoField(field.width, field.height, scopedProject, zoomMul, pan)
      : null;

  const checkout = scopedProject.checkouts.find((item) => item.alias === checkoutId);

  function go(next: BrainState) {
    setCheckoutId(null);
    setPan({ x: 0, y: 0 });
    setZoomMul(next === "repo-zoom" ? REPO_DESIGN_ZOOM : 1);
    props.setView(next);
    if (next === "overview") props.setScoped(false);
    if (next === "hover" || next === "synapse") {
      props.setFocusedId(SYNAPSE_EVENT.projectId);
      props.setScoped(false);
    }
    if (next === "repo-zoom" || next === "scoped") {
      props.setScoped(next === "scoped");
    }
  }

  function bumpZoom(dir: -1 | 0 | 1) {
    const home = props.view === "repo-zoom" ? REPO_DESIGN_ZOOM : 1;
    const next = dir === 0 ? home : Math.max(0.4, Math.min(4, zoomMul * (dir > 0 ? 1.15 : 1 / 1.15)));
    setZoomMul(next);
    if (dir === 0) setPan({ x: 0, y: 0 });
    if (use3d) voxelo.current?.zoom(dir === 0 ? "fit" : dir);
  }

  return (
    <section
      className={
        props.view === "firing-tree"
          ? "aperture is-firing-tree"
          : props.view === "neuron-lab"
            ? "aperture is-neuron-lab"
            : "aperture"
      }
      ref={apertureRef}
      tabIndex={-1}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          if (props.view === "repo-zoom" && checkoutId) {
            setCheckoutId(null);
            return;
          }
          setHoverId(null);
          props.setScoped(false);
          props.setView("overview");
        }
        if ((event.target === event.currentTarget || event.target instanceof HTMLCanvasElement) && props.view === "repo-zoom") {
          if (event.key === "+" || event.key === "=") bumpZoom(1);
          if (event.key === "-") bumpZoom(-1);
          const move = { ArrowLeft: [30, 0], ArrowRight: [-30, 0], ArrowUp: [0, 30], ArrowDown: [0, -30] }[event.key];
          if (move) { event.preventDefault(); setPan({ x: pan.x + move[0], y: pan.y + move[1] }); }
        }
      }}
    >
      {props.view === "firing-tree" ? (
        <FiringTree />
      ) : props.view === "neuron-lab" ? (
        <NeuronLab
          onFocus={props.setFocusedId}
          scope={props.labScope}
          grain={props.labGrain}
          onEnterScope={(id) => {
            props.setFocusedId(id);
            props.setLabScope(id);
          }}
          onLeaveScope={() => {
            props.setLabScope(null);
            props.setLabGrain("session");
          }}
          onGrain={props.setLabGrain}
        />
      ) : use3d ? (
        <div className="nl-stage" ref={voxeloHost} />
      ) : (
        <BrainCanvas
          pan={pan}
          onPan={setPan}
          onRepoZoom={(factor, anchor) => {
            const next = Math.max(0.4, Math.min(4, zoomMul * factor));
            const ratio = next / zoomMul;
            const origin = { x: (field?.width ?? 0) * 0.485, y: (field?.height ?? 0) * 0.47 };
            setPan({ x: pan.x * ratio + (anchor.x - origin.x) * (1 - ratio), y: pan.y * ratio + (anchor.y - origin.y) * (1 - ratio) });
            setZoomMul(next);
          }}
          appearance={props.appearance}
          onCheckoutPick={setCheckoutId}
          view={props.view}
          hoverId={effectiveHover}
          fieldRef={fieldRef}
          onField={setField}
          projectId={props.focused.id}
          zoom={zoomMul}
          onHover={(id) => {
            if (props.view === "synapse") return;
            setHoverId(id);
            if (id) props.setFocusedId(id);
          }}
          onPick={(id) => {
            if (props.view === "synapse" && id === SYNAPSE_EVENT.projectId) { setActivityRequest((n) => n + 1); return; }
            props.setFocusedId(id);
            props.setScoped(true);
            props.setView("scoped");
          }}
        />
      )}
      <div className="overlay">
        {five && !use3d && <BrainEvidence view={props.view} project={scopedProject}
          activityRequest={activityRequest}
          onInspect={(id) => { props.setFocusedId(id); setHoverId(id); }}
          onSelect={(id) => { props.setFocusedId(id); props.setScoped(true); props.setView("scoped"); }}
          onCheckout={setCheckoutId} />}
        {showField && (
          <details className="render-tuning">
            <summary>Render tuning</summary>
            {([
              ["glow", "Glow strength", 0, 2, 0.1],
              ["dust", "Particle density", 0.25, 2, 0.25],
              ["branch", "Branch width", 0.5, 2, 0.1],
            ] as const).map(([key, label, min, max, step]) => (
              <label key={key}>
                <span>{label}</span>
                <input
                  type="range"
                  aria-label={label}
                  min={min}
                  max={max}
                  step={step}
                  value={props.appearance[key]}
                  onChange={(event) => props.setAppearance({
                    ...props.appearance,
                    [key]: event.currentTarget.valueAsNumber,
                  })}
                />
                <output>{props.appearance[key]}×</output>
              </label>
            ))}
            <button type="button" onClick={() => props.setAppearance(DEFAULT_BODY_APPEARANCE)}>
              Reset tuning
            </button>
          </details>
        )}
        {five ? (
          <div className="brain-chips" role="tablist" aria-label="Brain view">
            {BRAIN_CHIPS.map((c) => (
              <button
                key={c.id}
                type="button"
                role="tab"
                aria-selected={props.view === c.id}
                className={props.view === c.id ? "nl-chip is-on" : "nl-chip"}
                onClick={() => go(c.id)}
              >
                {c.label}
              </button>
            ))}
            <button
              type="button"
              className={props.dim === "2d" ? "nl-chip is-on" : "nl-chip"}
              onClick={() => props.setDim("2d")}
            >
              2D
            </button>
            <button
              type="button"
              className={props.dim === "3d" ? "nl-chip is-on" : "nl-chip"}
              onClick={() => props.setDim("3d")}
            >
              3D
            </button>
          </div>
        ) : null}
        {showRecencyChrome && (
          <>
            <div className="axis-title">RECENCY (PRINTED)</div>
            <div className="axis-x">
              {RECENCY_AXIS.map((t) => (
                <div className="tick" key={t.id}>
                  <b>{t.label}</b>
                  <span>{t.sub}</span>
                </div>
              ))}
            </div>
            <div className="axis-y">
              <div className="line" />
              <div className="hi">high mass</div>
              <div className="mass">INDEXED MASS<br />exported holdings</div>
              <div className="lo">low mass</div>
            </div>
          </>
        )}
        {showField && (
          <>
            <div className="brain-field-key" aria-label="Registry field encoding">
              <b>REGISTERED INDEXED PROJECTS · {PROJECTS.length} BODIES</b>
              <span>BODY AREA · INDEXED MASS</span>
              <span>HORIZONTAL POSITION · RECENCY</span>
              <small>Idle field · activity is absent until an admitted event names an exact project.</small>
            </div>
            <svg className="registry-label-leaders" aria-hidden="true">
              {labels.filter((b) => Math.hypot(b.labelDx, b.labelDy) > b.capR * 1.5).map((b) => (
                <line key={b.project.id} x1={b.x} y1={b.y}
                  x2={b.x + b.labelDx} y2={b.y + b.labelDy + 8}
                  stroke={b.project.color} />
              ))}
            </svg>
            {labels.map((b) => (
              <button
                type="button"
                key={b.project.id}
                className="label project-label"
                data-inspected={props.focused.id === b.project.id}
                aria-label={`Inspect ${b.project.name}, indexed mass ${b.project.indexedMass}`}
                onKeyDown={(event) => {
                  if (!["ArrowRight", "ArrowDown", "ArrowLeft", "ArrowUp", "Home", "End"].includes(event.key)) return;
                  event.preventDefault();
                  const buttons = [...(event.currentTarget.parentElement?.querySelectorAll<HTMLButtonElement>(".project-label:not(.repo-label)") ?? [])];
                  const index = buttons.indexOf(event.currentTarget);
                  const next = event.key === "Home" ? 0 : event.key === "End" ? buttons.length - 1
                    : (index + (event.key === "ArrowRight" || event.key === "ArrowDown" ? 1 : -1) + buttons.length) % buttons.length;
                  buttons[next]?.focus();
                }}
                onFocus={() => {
                  setHoverId(b.project.id);
                  props.setFocusedId(b.project.id);
                }}
                onMouseEnter={() => {
                  setHoverId(b.project.id);
                  props.setFocusedId(b.project.id);
                }}
                onClick={() => {
                  if (props.view === "synapse" && b.project.id === SYNAPSE_EVENT.projectId) { setActivityRequest((n) => n + 1); return; }
                  props.setFocusedId(b.project.id);
                  props.setScoped(true);
                  props.setView("scoped");
                }}
                style={{
                  left: b.x,
                  top: b.y,
                  color: b.project.color,
                  transform: `translate(${b.labelDx}px, ${b.labelDy}px)`,
                }}
              >
                <span className="name">{b.project.name}</span>
                <span className="stats">
                  stores {b.project.storeCount.toLocaleString()}
                  <br />
                  artifacts {b.project.artifactCount.toLocaleString()}
                  <br />
                  {props.view === "synapse" ? "mass " : ""}
                  {b.project.indexedMass.toLocaleString()}
                </span>
              </button>
            ))}
            {hub && (
              <div className="hub-caption" style={{ left: hub.x, top: hub.y }}>
                {HUB.label}
                <br />
                {HUB.caption}
                {props.view === "synapse" ? (
                  <>
                    <br />
                    <span className="hub-sub">(repository for shared code)</span>
                  </>
                ) : null}
              </div>
            )}
            {props.view === "synapse" && hub ? (
              <div className="hub-caption hub-sibling" style={{ left: hub.x + 118, top: hub.y - 6 }}>
                sibling checkout
                <br />
                {Math.round(SYNAPSE_EVENT.siblingCheckout * 100)}%
              </div>
            ) : null}
            <aside className="signal">
              <h3>{props.view === "synapse" ? "SIGNAL LEGEND" : "SIGNAL INSET"}</h3>
              <div className="sub">
                {props.view === "synapse"
                  ? "Current named families. Synapse means any real admitted activity."
                  : "accepted activity families"}
              </div>
              <ul>
                {(props.view === "synapse"
                  ? [
                      { name: "Hook", count: null as number | null, color: "#5ee7ff" },
                      { name: "SessionIngest", count: null, color: "#67e8f9" },
                      { name: "CodeIndex", count: null, color: "#f0b429" },
                      { name: "ToolCall", count: null, color: "#9be15d" },
                      { name: "Task", count: null, color: "#c084fc" },
                    ]
                  : SIGNAL_FAMILIES
                ).map((f) => (
                  <li key={f.name}>
                    <span>
                      <i className="dot" style={{ background: f.color }} />
                      {f.name}
                    </span>
                    {"count" in f && f.count != null ? <span>{f.count.toLocaleString()}</span> : null}
                  </li>
                ))}
              </ul>
              <div className="frames">
                {props.view === "synapse" ? (
                  "Heartbeat · 10s · not work"
                ) : (
                  <>
                    <img className="spark" src="./spark-hedge.png" width={134} height={16} alt="" aria-hidden="true" />
                    <div className="frame-row">
                      <span>last 60 min<br />accepted frames</span>
                      <b>0</b>
                    </div>
                  </>
                )}
              </div>
            </aside>
          </>
        )}
        {repoField && (
          <>
            {repoField.orbs.map((orb) => (
              <button
                type="button"
                key={orb.id}
                className="label project-label repo-label"
                aria-label={`Inspect checkout ${orb.name}`}
                aria-pressed={checkoutId === orb.id}
                onClick={() => setCheckoutId(orb.id)}
                style={{
                  left: Math.max(16, Math.min((field?.width ?? 0) - 220, orb.x + orb.radius * 0.92)),
                  top: orb.y - 30,
                  color: orb.color,
                  transform: "none",
                }}
              >
                <span className="name">{orb.name}</span>
                <span className="stats">
                  <span className="k">last seen: </span>{orb.lastSeen}
                  <br />
                  <span className="k">checkout · fixed size</span>
                </span>
              </button>
            ))}
          </>
        )}
        {props.view === "repo-zoom" && checkout && (
          <aside className="checkout-detail" aria-label="Checkout details">
            <button type="button" aria-label="Close checkout details" onClick={() => setCheckoutId(null)}>×</button>
            <h2>{checkout.alias}</h2>
            <p>{checkout.path}</p>
            <dl><dt>Source</dt><dd>Exported checkout registry</dd>
              <dt>Last seen</dt><dd>{checkout.lastSeen === "—" ? "unavailable" : checkout.lastSeen}</dd>
              <dt>Checkout holdings</dt><dd>unavailable</dd></dl>
            <button type="button" onClick={() => { props.setScoped(true); props.setView("scoped"); }}>
              Open project: {scopedProject.name}
            </button>
          </aside>
        )}
        {props.view === "repo-zoom" && (
          <button type="button" className="repo-minimap" aria-label="Fit repository neighborhood" onClick={() => bumpZoom(0)}>
            <svg viewBox="0 0 140 90" aria-hidden="true">{layoutRepoField(140, 90, scopedProject, REPO_DESIGN_ZOOM).orbs.map((orb) => <g key={orb.id}><path d={`M${orb.x} ${orb.y}L67.9 42.3`} /><circle cx={orb.x} cy={orb.y} r="8" /></g>)}<circle cx="67.9" cy="42.3" r="3" /></svg><span>Registry / {scopedProject.name}</span>
          </button>
        )}
        {props.view === "repo-zoom" && (
          <div className="zoom-hud">
            <button type="button" aria-label="Zoom out" onClick={() => bumpZoom(-1)}>-</button>
            <span>{Math.round(zoomMul * 100)}%</span>
            <button type="button" aria-label="Zoom in" onClick={() => bumpZoom(1)}>+</button>
            <button type="button" onClick={() => bumpZoom(0)}>Fit</button>
          </div>
        )}
      </div>
    </section>
  );
}
