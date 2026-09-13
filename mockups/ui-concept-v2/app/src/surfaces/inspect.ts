export type InspectRow = { l: string; r?: string };

export type InspectSection = { k: string; rows: InspectRow[] };

export type SurfaceInspect = {
  title: string;
  kind: string;
  id?: string;
  sections: InspectSection[];
  hint: string;
};

export type StatusTone = "live" | "quiet" | "ready" | "scope" | "warn" | "danger" | "violet";

export type StatusCell = { lab: string; val: string; tone: StatusTone };

export type RegisterCopy = {
  project: string;
  kicker: string;
  line: string;
  box: string;
  bright: string;
};
