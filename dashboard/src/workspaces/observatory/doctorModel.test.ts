import { describe, expect, it } from 'vitest';
import { DoctorFindingsPayloadV1Schema } from '../../contracts/generated.ts';
import { doctorEvidencePresentation, doctorFamilyLabel } from './doctorModel.ts';

describe('Doctor frontend diagnostics', () => {
  it('decodes canonical finding evidence and coverage', () => {
    const payload = DoctorFindingsPayloadV1Schema.parse({
      family_filter: null,
      entries: [
        {
          finding: {
            family: 'configuration',
            state: 'degraded',
            evidence: [
              {
                family: 'configuration',
                reference: 'configuration:desired-effective-drift',
              },
            ],
            coverage: {
              completeness: 'complete',
              statement: 'configuration authority was consulted',
            },
          },
          storage_kind: null,
        },
      ],
      report_coverage: {
        families: [
          { family: 'configuration', consultation: { status: 'consulted' } },
          {
            family: 'semantic_index',
            consultation: { status: 'unavailable', reason: 'unsupported' },
          },
        ],
        completeness: 'partial',
        statement: {
          completeness: 'partial',
          statement: 'one family was unavailable',
        },
      },
      known_families: ['configuration', 'semantic_index'],
      note: 'one family was unavailable',
    });

    expect(payload.entries[0]?.finding.evidence[0]?.reference).toBe(
      'configuration:desired-effective-drift',
    );
    expect(payload.report_coverage?.families[1]?.consultation).toEqual({
      status: 'unavailable',
      reason: 'unsupported',
    });
  });

  it('presents evidence and family states through the shared typed vocabulary', () => {
    expect(doctorEvidencePresentation('degraded')).toMatchObject({
      label: 'Degraded',
      domainState: 'error',
    });
    expect(doctorFamilyLabel('semantic_index')).toBe('Semantic index');
  });
});
