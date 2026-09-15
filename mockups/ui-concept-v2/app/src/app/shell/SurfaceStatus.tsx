import { Corners } from "./Corners";
import { SURFACE_ICONS } from "./SurfaceIcons";
import type { SurfaceChrome } from "./surfaceChrome";

export function SurfaceStatus(props: { chrome: SurfaceChrome }) {
  const { chrome } = props;
  return (
    <footer className="status surface-status" tabIndex={0} aria-label="Snapshot status">
      <Corners />
      {chrome.status.map((cell) => (
        <div className={`cell tone-${cell.tone}`} key={cell.lab}>
          {SURFACE_ICONS[cell.icon]}
          <div className="stack">
            <span className="lab">{cell.lab}</span>
            <span className="val">{cell.val}</span>
            {cell.sub ? <span className="sub">{cell.sub}</span> : null}
            {cell.bar != null ? (
              <span className="cell-bar" aria-hidden="true">
                <span className="cell-bar-fill" style={{ width: `${cell.bar}%` }} />
              </span>
            ) : null}
          </div>
        </div>
      ))}
      <div className="stamp">{chrome.stamp}</div>
    </footer>
  );
}
