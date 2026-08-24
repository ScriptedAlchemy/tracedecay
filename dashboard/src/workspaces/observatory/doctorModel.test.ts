import { describe, expect, it } from 'vitest';
import { doctorEvidencePresentation, doctorFamilyLabel } from './doctorModel.ts';

describe('Doctor frontend diagnostics', () => {
  it('presents evidence and family states through the shared typed vocabulary', () => {
    expect(doctorEvidencePresentation('degraded')).toMatchObject({
      label: 'Degraded',
      domainState: 'error',
    });
    expect(doctorFamilyLabel('semantic_index')).toBe('Semantic index');
  });
});
