import type { Surface } from "../data/fixtures";
import type { SurfaceInspect } from "./inspect";
import { ExplorerPage } from "./ExplorerPage";

export function SurfacePage(props: {
  surface: Surface;
  onInspect: (i: SurfaceInspect) => void;
  query?: string;
}) {
  return props.surface === "explorer" ? <ExplorerPage onInspect={props.onInspect} query={props.query} /> : null;
}
