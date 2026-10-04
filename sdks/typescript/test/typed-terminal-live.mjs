// Invoked by the physical-daemon transport journey after building this SDK.
import { readFileSync } from "node:fs";
import {
  createClient,
  TraceDecayPartialEffectError,
  TraceDecayProblemError,
  TraceDecayResetRequiredError,
} from "../dist/index.js";

const { connection, operation, request, deadlineMicros, allowSuccess = false } = JSON.parse(readFileSync(0, "utf8"));
const client = createClient(connection);
try {
  const options = deadlineMicros === null ? {} : { deadlineMicros };
  const allowed = ["fact_store_add", "fact_store_search", "storage_status", "session_refresh_begin",
    "session_refresh_status", "session_refresh_cancel", "lcm_load_session"];
  if (!allowed.includes(operation)) {
    throw new Error(`unsupported journey operation: ${operation}`);
  }
  const result = await client.operations[`application_${operation}`](request, options);
  if (!allowSuccess) throw new Error("a refused or deadline-expired operation returned success");
  process.stdout.write(JSON.stringify({ kind: "success", value: result }));
} catch (error) {
  if (!(error instanceof TraceDecayProblemError)) throw error;
  if (error.problem.kind === "partial_effect" && !(error instanceof TraceDecayPartialEffectError)) {
    throw new Error("partial effect lost its typed SDK error");
  }
  if (error.problem.kind === "reset_required" && !(error instanceof TraceDecayResetRequiredError)) {
    throw new Error("reset refusal lost its typed SDK error");
  }
  process.stdout.write(JSON.stringify(error.envelope));
}
