import { Corners } from "./Corners";
import type { SurfaceChrome } from "./surfaceChrome";

export function SurfaceRegister(props: { chrome: SurfaceChrome; hideExtra?: boolean; scope?: {name:string; id:string}; query?:string; onQuery?:(query:string)=>void }) {
  const { chrome } = props;
  const extra = props.hideExtra ? undefined : chrome.extra;
  return (
    <header className="register">
      <Corners />
      <div className="reg-left">
        <h1 title={props.scope?.id}>
          {chrome.slug === 'workflows' ? '14 WORKFLOWS / DEFINITION LEDGER' : <>Project: <em>{props.scope?.name ?? 'all'}</em></>}
        </h1>
        {chrome.kicker ? <p className={chrome.slug === "sessions" ? "kicker is-sentence" : "kicker"}>{chrome.kicker}</p> : null}
      </div>
      {extra?.type === "query" ? (
        <div className="reg-query">
          <span className="reg-query-lab">QUERY</span>
          <input
            className="reg-query-field"
            type="search"
            value={props.query ?? ''}
            onChange={event=>props.onQuery?.(event.target.value)}
            placeholder="Filter captured sessions…"
            aria-label="Query"
          />
          <button type="button" className="reg-cancel" disabled={!props.query} onClick={()=>props.onQuery?.('')}>
            Clear
          </button>
          <div className="reg-running" aria-label="Local snapshot filter">
            <span className="reg-running-lab">
              LOCAL SNAPSHOT
            </span>
          </div>
        </div>
      ) : extra?.type === "loom-follow" ? (
        <div className="reg-loom">
          <button type="button" className="reg-chip on" tabIndex={-1}>
            FOLLOW LOADED TAIL
          </button>
          <button type="button" className="reg-chip" tabIndex={-1}>
            PAUSE
          </button>
          <button type="button" className="reg-chip" tabIndex={-1}>
            FIT
          </button>
        </div>
      ) : (
        <div className="reg-meta" />
      )}
    </header>
  );
}
