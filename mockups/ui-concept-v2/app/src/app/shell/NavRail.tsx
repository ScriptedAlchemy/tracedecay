import { CHANNELS } from "../../data/fixtures";
import { Corners } from "./Corners";
import { useAttentionItems, WorkspaceControls } from '../attention';

export function NavRail(props: { channel: string; onChannel: (name: string) => void }) {
  const items = useAttentionItems();
  return (
    <aside className="rail">
      <Corners />
      <div className="brand">
        <span className="wordmark">TRACEDECAY</span>
        <span className="v2" title="V2">
          <i />
          <i />
          <i />
          <i />
          <em>V2</em>
        </span>
      </div>
      <nav className="channels" aria-label="Workspaces">
        {CHANNELS.map((name, i) => (
          <button
            key={name}
            className={name === props.channel ? "channel active" : "channel"}
            aria-current={name === props.channel ? 'page' : undefined}
            aria-label={`${String(i + 1).padStart(2, '0')} ${name}`}
            title={name}
            onClick={() => props.onChannel(name)}
          >
            <span className="num">{String(i + 1).padStart(2, "0")}</span>
            <span className="channel-label">{name}</span>
            {items.find(item=>item.status==='active'&&item.target.surface===name.toLowerCase())&&<span className="attention-dot workspace-nav-beacon" data-severity={items.find(item=>item.status==='active'&&item.target.surface===name.toLowerCase())?.severity} aria-hidden="true"/>}
          </button>
        ))}
      </nav>
      <WorkspaceControls />
    </aside>
  );
}
