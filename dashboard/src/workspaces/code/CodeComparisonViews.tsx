import type { ReactNode } from 'react';
import { CenteredState } from '../../ui/ReadSection.tsx';
import type { CodeView } from './codeView.ts';
import type { TraceFocus } from './TraceView.tsx';

export function CodeViewContent({
  view,
  focus,
  children,
}: {
  view: CodeView;
  focus: TraceFocus | null;
  children: ReactNode;
}) {
  if (view === 'shared-code') return <SharedCodeView focus={focus} />;
  if (view === 'compare') return <CompareView />;
  return children;
}

function SharedCodeView({ focus }: { focus: TraceFocus | null }) {
  if (focus === null) {
    return (
      <CenteredState
        title="Shared Code needs a source occurrence"
        kind="unavailable"
        detail="Select a function or method in Topology."
      />
    );
  }

  return (
    <div className="flex h-full min-h-0 flex-col bg-surface-0">
      <header className="border-b border-edge-subtle p-4">
        <p className="td-legend">source occurrence</p>
        <h1 className="mt-1 truncate text-base font-semibold text-text-primary">
          {focus.qualified_name ?? focus.name ?? focus.id}
        </h1>
        <p className="mt-1 break-all font-mono text-2xs text-text-muted">
          {focus.file_path ?? 'source path unavailable'}
          {focus.start_line == null
            ? ''
            : `:${focus.start_line}${focus.end_line == null ? '' : `-${focus.end_line}`}`}
        </p>
      </header>
      <div className="min-h-0 flex-1">
        <CenteredState
          title="Shared implementations unavailable"
          kind="unavailable"
          detail="The shared-code family read is not mounted on the dashboard."
        />
      </div>
      <p className="border-t border-edge-subtle px-4 py-2 text-3xs leading-relaxed text-text-muted">
        Families stay grouped by verified digest. Coverage reports complete, partial,
        excluded too small, or excluded after incomplete tokenization.
      </p>
    </div>
  );
}

function CompareView() {
  return (
    <div className="flex h-full min-h-0 flex-col bg-surface-0">
      <header className="border-b border-edge-subtle p-4">
        <p className="td-legend">stable union layout</p>
        <h1 className="mt-1 text-base font-semibold text-text-primary">Compare</h1>
        <p className="mt-1 max-w-2xl text-xs leading-relaxed text-text-muted">
          One shared scope keeps existing regions in place while added and deleted regions retain
          explicit space.
        </p>
      </header>
      <div className="min-h-0 flex-1">
        <CenteredState
          title="Revision comparison unavailable"
          kind="unavailable"
          detail="No dashboard read supplies two exact revisions, one shared scope, and a union layout."
        />
      </div>
      <p className="border-t border-edge-subtle px-4 py-2 text-3xs leading-relaxed text-text-muted">
        No regions or lens measurements are drawn until that read is available. Missing
        comparison evidence is not reported as zero change.
      </p>
    </div>
  );
}
