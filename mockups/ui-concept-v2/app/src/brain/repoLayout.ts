import { type ProjectBody } from "../data/fixtures";

export type RepoOrb = {
  id: string;
  name: string;
  color: string;
  x: number;
  y: number;
  radius: number;
  lastSeen: string;
};

export type RepoField = {
  hub: { x: number; y: number };
  orbs: RepoOrb[];
};

/** The plate composition reads at 175%; positions/radii are designed there. */
export const REPO_DESIGN_ZOOM = 1.75;

const FALLBACK_COLORS = ["#f0b429", "#4fc3f7", "#2dd4bf", "#c084fc"];

/** Real tracedecay checkouts. Amber canonical, cyan siblings — never invented. */
export const CHECKOUT_COLORS: Record<string, string> = {
  redesign: "#f0b429",
  "ui-concept-first-party": "#5ee7ff",
  "ui-concept-v2-final-followup": "#67e8f9",
};

export function layoutRepoField(
  w: number,
  h: number,
  project: ProjectBody,
  zoom: number,
  pan = { x: 0, y: 0 },
): RepoField {
  const hub = { x: w * 0.485, y: h * 0.47 };
  const z = zoom / REPO_DESIGN_ZOOM;
  const n = Math.max(1, project.checkouts.length);
  const orbs = project.checkouts.map((c, i) => {
    const position = [[0.27, 0.29], [0.72, 0.4], [0.49, 0.76]][i];
    let px: number;
    let py: number;
    if (n <= 3 && position) {
      px = position[0] * w;
      py = position[1] * h;
    } else {
      const th = (i / n) * Math.PI * 2 - Math.PI / 2;
      px = hub.x + Math.cos(th) * w * 0.24;
      py = hub.y + Math.sin(th) * h * 0.26;
    }
    return {
      id: c.alias,
      name: c.alias,
      color: CHECKOUT_COLORS[c.alias] ?? FALLBACK_COLORS[i % FALLBACK_COLORS.length],
      x: hub.x + (px - hub.x) * z + pan.x,
      y: hub.y + (py - hub.y) * z + pan.y,
      radius: Math.min(125, w * 0.115, h * 0.16) * z,
      lastSeen: c.lastSeen === "—" ? "unavailable" : c.lastSeen,
    };
  });
  return { hub: { x: hub.x + pan.x, y: hub.y + pan.y }, orbs };
}
