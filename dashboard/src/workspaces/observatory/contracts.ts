import { z } from 'zod';
import {
  DoctorFindingsPayloadSchema,
  DoctorStorageFindingKindSchema,
} from '../../contracts/wire.ts';

export const StorageFindingSourceStateSchema = z.enum([
  'real',
  'unset',
  'partial',
  'unsupported',
]);
export type StorageFindingSourceState = z.infer<typeof StorageFindingSourceStateSchema>;

export const StorageFindingSourceStatusSchema = z.object({
  kind: DoctorStorageFindingKindSchema,
  state: StorageFindingSourceStateSchema,
  observed_entries: z.number().int().nonnegative(),
  reason: z.string().min(1),
});
export type StorageFindingSourceStatus = z.infer<typeof StorageFindingSourceStatusSchema>;

/**
 * Observatory's additive storage-finding contract. The canonical Doctor
 * payload remains the shared generated shape; this route-specific extension
 * names each Plan 38 producer's source state so omitted evidence cannot render
 * as a clean result.
 */
export const ObservatoryStorageFindingsPayloadSchema = DoctorFindingsPayloadSchema.extend({
  kind_statuses: z.array(StorageFindingSourceStatusSchema).length(5),
});
export type ObservatoryStorageFindingsPayload = z.infer<
  typeof ObservatoryStorageFindingsPayloadSchema
>;
