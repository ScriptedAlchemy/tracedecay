/**
 * The same symbols the field draws, as text, in call-site order.
 *
 * This is not a summary of the picture — it is the picture's exact contents.
 * Anything the field encodes as a position, a width or a latency is printed
 * here as a number, which is what makes the canvas legitimately one `role="img"`
 * rather than a grid of controls a screen reader has to walk.
 *
 * It carries that load twice over, which is why it is a module of its own: it is
 * the accessible equivalent of the field AND the whole reading whenever the
 * field is not drawn — too narrow a column, or a browser with no 2D context. A
 * component that has to stay symbol-for-symbol identical to another one is
 * easier to keep that way when it is not buried in the file that draws it.
 */
import { useMemo } from 'react';
import { Crosshair } from 'lucide-react';

import { cn } from '../../ui/cn';
import { elideStart } from '../../ui/format.ts';
import { kindColorVars } from '../../viz/graph/kindColor.ts';
import type { TraceModel, TraceNode } from '../../viz/trace/types.ts';
import { callSiteTotals, orderByHopThenCallSites } from './traceRanking.ts';
import type { TraceFocus } from './TraceView.tsx';

export function TraceList({
  model,
  focusId,
  onFocusChange,
}: {
  model: TraceModel;
  focusId: string;
  onFocusChange?: (node: TraceFocus) => void;
}) {
  const callSites = useMemo(() => callSiteTotals(model.channels), [model.channels]);
  const ordered = useMemo(
    () => orderByHopThenCallSites(model.nodes, callSites),
    [model.nodes, callSites],
  );

  return (
    <div className="flex flex-col">
      <div className="flex items-center gap-2.5 border-b border-edge-subtle px-3 py-2">
        <span className="td-legend">symbols on the field</span>
        <span aria-hidden className="td-rule" />
        <span className="td-legend shrink-0 normal-case tracking-normal">
          {ordered.length} drawn · ordered by hop, then call sites
        </span>
      </div>
      <ol className="grid grid-cols-1 sm:grid-cols-2 xl:grid-cols-3">
        {ordered.map((node) => (
          <li key={node.id} className="min-w-0 border-b border-l border-edge-subtle">
            <TraceRow
              node={node}
              callSites={callSites.get(node.id) ?? 0}
              isFocus={node.id === focusId}
              {...(onFocusChange ? { onFocusChange } : {})}
            />
          </li>
        ))}
      </ol>
    </div>
  );
}

function TraceRow({
  node,
  callSites,
  isFocus,
  onFocusChange,
}: {
  node: TraceNode;
  callSites: number;
  isFocus: boolean;
  onFocusChange?: (node: TraceFocus) => void;
}) {
  const side = node.ring === 0 ? 'focus' : node.ring < 0 ? 'calls it' : 'called by it';
  const hop = node.ring === 0 ? 'focus' : `${Math.abs(node.ring)} hop${Math.abs(node.ring) === 1 ? '' : 's'} ${node.ring < 0 ? 'up' : 'down'}`;
  const body = (
    <>
      <span className="flex min-w-0 items-baseline gap-2 leading-tight">
        <span
          aria-hidden
          className="size-1.5 shrink-0 translate-y-[-1px] rounded-full bg-[var(--kind-dark)] [[data-theme=light]_&]:bg-[var(--kind-light)]"
          style={kindColorVars(node.kind)}
        />
        <span className="td-value min-w-0 flex-1 truncate text-xs text-text-primary">
          {node.name}
        </span>
        <span className="td-value shrink-0 text-2xs text-text-secondary" data-cell="numeric">
          {callSites}
          <span className="td-unit ml-1">call sites</span>
        </span>
      </span>
      <span className="flex min-w-0 items-baseline gap-2 pl-3.5 leading-tight">
        <span className="td-legend shrink-0">{hop}</span>
        <span className="td-legend shrink-0 max-w-20 truncate normal-case tracking-normal text-text-muted">
          {side}
        </span>
        <span className="td-value min-w-0 flex-1 truncate text-right text-3xs text-text-muted">
          {node.degree == null ? 'degree absent' : `deg ${node.degree}`}
          {node.selfCalls ? ` · ${node.selfCalls} self-calls` : ''}
          {node.undrawnEdges ? ` · ${node.undrawnEdges} edges not drawn` : ''}
        </span>
      </span>
      {node.filePath ? (
        <span
          className="td-value truncate pl-3.5 text-left text-3xs text-text-muted"
          title={node.filePath}
        >
          {elideStart(node.filePath, 40)}
          {node.startLine == null ? '' : `:${node.startLine}`}
        </span>
      ) : null}
    </>
  );

  if (!onFocusChange || isFocus) {
    return (
      <div
        className={cn(
          'flex h-full w-full flex-col gap-0.5 px-3 py-1.5 text-left',
          isFocus ? 'bg-surface-2' : 'bg-surface-0',
        )}
      >
        {isFocus ? (
          <span className="flex items-center gap-1 text-3xs uppercase tracking-[0.18em] text-accent">
            <Crosshair aria-hidden size={10} />
            focus
          </span>
        ) : null}
        {body}
      </div>
    );
  }
  return (
    <button
      type="button"
      onClick={() =>
        onFocusChange({
          id: node.id,
          kind: node.kind,
          name: node.name,
          file_path: node.filePath,
          start_line: node.startLine,
          // An absent degree stays absent. Substituting 0 here would make the
          // re-centred field draw an unmeasured symbol as a measured leaf.
          ...(node.degree == null ? {} : { degree: node.degree }),
        })
      }
      title={`Re-centre the trace on ${node.name}`}
      className="flex h-full w-full flex-col gap-0.5 bg-surface-0 px-3 py-1.5 text-left hover:bg-surface-1 focus-visible:bg-surface-1"
    >
      {body}
    </button>
  );
}
