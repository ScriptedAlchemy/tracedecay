import type {
  AnalyticsDiagnosticsPayloadV1,
  ObservatoryReadModelV1,
} from '../../contracts/generated.ts';
import type { EnvelopeResult } from '../../data/query/envelope.ts';

/**
 * The two snapshots the V2 accounting panels are allowed to read.
 *
 * `ObservatoryPage` owns these requests once and passes this bound pair to all
 * three panels. Keeping the result, freshness, and refresh action together
 * prevents one panel from calling a later watermark its own while another is
 * still explaining an earlier one.
 */
export interface AccountingRead<T> {
  readonly result: EnvelopeResult<T> | undefined;
  readonly pending: boolean;
  readonly refreshing: boolean;
  readonly refresh: () => void;
}

export interface ObservatoryAccountingReads {
  readonly observatory: AccountingRead<ObservatoryReadModelV1>;
  readonly diagnostics: AccountingRead<AnalyticsDiagnosticsPayloadV1>;
}
