import { Corners } from "./Corners";
import { INTERIORS } from "./interiors";
import { SurfacePage } from "../../surfaces";
import type { SurfaceInspect } from "../../surfaces/inspect";
import type { SurfaceSlug } from "./surfaceChrome";
import { resolveLoomPivot } from "../../loom/pivots";
import { SourceDestination } from "../../loom/SourceDestination";
import { SOURCE_ID } from "../../loom/source";
import { FAMILIES, nodeInFamily } from "../../work/model";

export function SurfaceAperture(props: {
  surface: SurfaceSlug;
  state?: string;
  onState?: (id: string) => void;
  onInspect?: (i: SurfaceInspect) => void;
  query?: string;
}) {
  const Interior = INTERIORS[props.surface];
  const pivot = resolveLoomPivot(props.surface);
  const nativeSession = pivot?.kind === 'ready' && SOURCE_ID === 'mac' && pivot.cutoff === null &&
    (props.surface === 'sessions' || props.surface === 'agents' ||
      (props.surface === 'work' && FAMILIES.some(f => nodeInFamily(f, pivot.session.id)?.session))) ? pivot.session.id : undefined;
  return (
    <section className="aperture surface-aperture" data-surface={props.surface}>
      <Corners />
      {pivot ? <SourceDestination key={`${props.surface}:${nativeSession ?? ''}`} pivot={pivot}>
        {Interior && nativeSession ? <Interior initialSessionId={nativeSession} /> : null}
      </SourceDestination> : Interior ? (
        <Interior state={props.state} onState={props.onState} />
      ) : (
        <SurfacePage surface={props.surface} query={props.query} onInspect={props.onInspect ?? (() => {})} />
      )}
    </section>
  );
}
