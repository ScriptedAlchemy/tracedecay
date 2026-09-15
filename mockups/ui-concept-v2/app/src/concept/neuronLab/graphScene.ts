import ForceGraph3D, { type ForceGraph3DInstance } from "3d-force-graph";
import { CSS2DObject, CSS2DRenderer } from "three/addons/renderers/CSS2DRenderer.js";
import { PROJECTS } from "../../data/fixtures";
import { attributionPipes, layoutLabBodies, type LabHooks } from "./labUtil";

type LabNode = {
  id: string;
  name: string;
  val: number;
  color: string;
};

type LabLink = {
  source: string;
  target: string;
};

const BG = "#05070e";

function nodeVal(mass: number) {
  return 8 + Math.log10(Math.max(10, mass)) * 4;
}

function nameBadge(node: LabNode) {
  const el = document.createElement("div");
  el.textContent = node.name;
  el.style.cssText = [
    "color:#d7eaf4",
    "font:500 12px/1.2 ui-sans-serif,system-ui,sans-serif",
    "padding:3px 8px",
    "background:rgba(8,12,20,0.78)",
    "border:1px solid rgba(106,168,200,0.32)",
    "white-space:nowrap",
    "pointer-events:none",
    "letter-spacing:0.02em",
  ].join(";");
  const badge = new CSS2DObject(el);
  badge.center.set(0.5, 1.35);
  return badge;
}

export function mountGraph(host: HTMLElement, hooks: LabHooks = {}): { dispose: () => void } {
  const bodies = layoutLabBodies();
  const pipes = attributionPipes(bodies);
  // Empty hops → six disconnected nerves. No Kruskal, no invented tracedecay↔ZeroFS.

  // No seeded x/y/z — planar layoutLabBodies coords collapse into a 2D arc.
  // d3-force-3d phyllotaxis + charge fans the six PROJECTS nodes in 3D.
  const nodes: LabNode[] = PROJECTS.map((p) => ({
    id: p.id,
    name: p.name,
    val: nodeVal(p.indexedMass || p.storeCount),
    color: p.color,
  }));

  const links: LabLink[] = pipes.map((pipe) => ({
    source: pipe.a.project.id,
    target: pipe.b.project.id,
  }));

  const inspect = document.createElement("div");
  inspect.className = "nl-inspect";
  inspect.textContent = "GRAPH · 3d-force-graph · hover inspect";

  const reduced = window.matchMedia?.("(prefers-reduced-motion: reduce)")?.matches ?? false;

  const graph = new ForceGraph3D(host, {
    controlType: "orbit",
    rendererConfig: { antialias: true, alpha: false },
    extraRenderers: [new CSS2DRenderer() as never],
  }) as unknown as ForceGraph3DInstance<LabNode, LabLink>;

  const fitSize = () => {
    graph.width(host.clientWidth || 1);
    graph.height(host.clientHeight || 1);
  };
  fitSize();

  const charge = graph.d3Force("charge");
  if (charge && typeof charge.strength === "function") charge.strength(-620);
  const linkForce = graph.d3Force("link");
  if (linkForce && typeof linkForce.distance === "function") linkForce.distance(220);

  graph
    .backgroundColor(BG)
    .showNavInfo(false)
    .enableNavigationControls(true)
    .enableNodeDrag(true)
    .numDimensions(3)
    .nodeId("id")
    .nodeLabel("name")
    .nodeVal("val")
    .nodeColor("color")
    .nodeOpacity(0.94)
    .nodeResolution(18)
    .nodeRelSize(10)
    .nodeThreeObject((node) => nameBadge(node))
    .nodeThreeObjectExtend(true)
    .linkColor(() => "#6aa8c8")
    .linkOpacity(0.72)
    .linkWidth(1.6)
    .linkCurvature(0.18)
    .linkDirectionalParticles(reduced ? 0 : 14)
    .linkDirectionalParticleSpeed(0.014)
    .linkDirectionalParticleWidth(5)
    .linkDirectionalParticleColor(() => "#cdecff")
    .linkDirectionalParticleResolution(10)
    .onNodeHover((node) => {
      // Hover inspect only — never a synapse fire.
      if (node) {
        inspect.textContent = `${node.name} · inspect`;
        hooks.onFocus?.(node.id);
      } else {
        inspect.textContent = "hover · inspect only";
      }
    })
    .d3VelocityDecay(0.28)
    .warmupTicks(80)
    .cooldownTicks(90)
    .cooldownTime(1400)
    .onEngineStop(() => {
      graph.zoomToFit(500, 28);
    })
    .graphData({ nodes, links });

  // zoomToFit at construct time is a no-op (graph bbox not initialised yet).
  // Engine-stop is the real fit; 1s timeout covers a still-running sim.
  const fitTimer = window.setTimeout(() => {
    graph.zoomToFit(500, 28);
  }, 1000);

  host.appendChild(inspect);

  const ro = new ResizeObserver(fitSize);
  ro.observe(host);

  return {
    dispose() {
      window.clearTimeout(fitTimer);
      ro.disconnect();
      inspect.remove();
      graph._destructor();
      host.replaceChildren();
    },
  };
}
