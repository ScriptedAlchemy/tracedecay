import { Corners } from "./Corners";
import type { SurfaceInspectorSpec } from "./surfaceChrome";

export function SurfaceInspector(props: { inspector: SurfaceInspectorSpec; onClose?:()=>void }) {
  const { inspector } = props;
  return (
    <aside className="inspector">
      <Corners />
      {props.onClose ? <button className="inspector-close" aria-label="Close inspector" onClick={props.onClose}>×</button> : null}
      <h2>{inspector.title}</h2>
      <div className="kind">{inspector.kind}</div>
      {inspector.id ? <div className="mono-id">{inspector.id}</div> : null}
      {inspector.sections.map((sec) => (
        <div className="kv" key={sec.k}>
          <div className="k">{sec.k}</div>
          {sec.text ? <div className="v inspect-text">{sec.text}</div> : null}
          {sec.code ? <pre className="inspect-code">{sec.code}</pre> : null}
          {sec.rows?.map((row, i) => (
            <div className="v" key={`${sec.k}-${i}-${row.label ?? row.value}`}>
              {row.label ? <span>{row.label}</span> : null}
              <span>{row.value}</span>
            </div>
          ))}
        </div>
      ))}
    </aside>
  );
}
