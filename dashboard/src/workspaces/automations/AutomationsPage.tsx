import { z } from 'zod';
import { OverviewCard, OverviewGrid } from '../../ui/archetypes/OverviewGrid';
import { KeyValueTree } from '../../ui/archetypes/ExplorerSplit.tsx';
import { LegacyBoundary } from '../../ui/LegacyStates.tsx';
import { AnyObject } from '../../data/query/legacy.ts';
import { useLegacy } from '../../data/query/useLegacy.ts';

const SkillsPayload = z
  .object({ skills: z.array(AnyObject).optional(), items: z.array(AnyObject).optional() })
  .passthrough();

/** Automations: scheduler status, managed skills, fact proposals — real
 * /api/automation surfaces; bounded controls land with the actions phase. */
export function AutomationsPage() {
  const scheduler = useLegacy(
    ['automation', 'scheduler'],
    '/api/automation/scheduler/status',
    AnyObject,
  );
  const skills = useLegacy(['automation', 'skills'], '/api/automation/skills', SkillsPayload);
  const proposals = useLegacy(
    ['automation', 'fact-proposals'],
    '/api/automation/fact-proposals',
    AnyObject,
  );

  return (
    <div className="flex h-full flex-col overflow-auto">
      <div className="flex items-center gap-3 border-b border-edge-subtle px-4 py-2">
        <h1 className="text-sm font-semibold tracking-tight">Automations</h1>
      </div>
      <OverviewGrid>
        <OverviewCard title="Scheduler">
          <LegacyBoundary title="Scheduler" pending={scheduler.isPending} result={scheduler.data}>
            {(data) => <KeyValueTree value={data} />}
          </LegacyBoundary>
        </OverviewCard>
        <OverviewCard title="Managed skills">
          <LegacyBoundary title="Skills" pending={skills.isPending} result={skills.data}>
            {(data) => <KeyValueTree value={data.skills ?? data.items ?? data} />}
          </LegacyBoundary>
        </OverviewCard>
        <OverviewCard title="Fact proposals">
          <LegacyBoundary title="Proposals" pending={proposals.isPending} result={proposals.data}>
            {(data) => <KeyValueTree value={data} />}
          </LegacyBoundary>
        </OverviewCard>
      </OverviewGrid>
    </div>
  );
}
