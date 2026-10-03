import { readFile } from "node:fs/promises";
import os from "node:os";
import path from "node:path";
import { z } from "zod";
import type { Failure } from "../shared/view.js";

// Mirrors `ProfileRoot::from_env` in crates/tracedecay-runtime-core: the data
// directory is `TRACEDECAY_DATA_DIR`, otherwise `.tracedecay` under the home.
// `HOME_ENV` there is `USERPROFILE` on Windows and `HOME` elsewhere, so this
// must read the same platform variable the daemon does rather than preferring
// `HOME` (which Git-Bash-style shells also export on Windows).
export function resolveProfileRoot(env: NodeJS.ProcessEnv = process.env): string {
  const explicit = env.TRACEDECAY_DATA_DIR;
  if (explicit !== undefined && explicit.length > 0) return path.resolve(explicit);
  const homeEnv = process.platform === "win32" ? env.USERPROFILE : env.HOME;
  const home = homeEnv !== undefined && homeEnv.length > 0 ? homeEnv : os.homedir();
  return path.join(home, ".tracedecay");
}

const SocketAddr = z.string().min(1);

// Subset of `DaemonAuthorityRecord` (crates/tracedecay-daemon-identity) that
// the bridge consumes. The token never leaves this process.
export const DaemonAuthorityRecordSchema = z.object({
  pid: z.number().int().nonnegative(),
  process_run_id: z.string(),
  started_at_unix_secs: z.number().int(),
  epoch: z.number().int().nonnegative(),
  version: z.string(),
  endpoint: z.unknown(),
  http_application_endpoint: SocketAddr.nullable().optional(),
  auth_token: z.string().min(1),
  profile_root: z.string().min(1),
});

export type DaemonAuthorityRecord = z.infer<typeof DaemonAuthorityRecordSchema>;

export type AuthorityLookup =
  | { readonly kind: "available"; readonly record: DaemonAuthorityRecord }
  | { readonly kind: "absent"; readonly failure: Failure }
  | { readonly kind: "invalid"; readonly failure: Failure };

export async function readDaemonAuthority(profileRoot: string): Promise<AuthorityLookup> {
  const recordPath = path.join(profileRoot, "daemon-authority.json");
  let raw: string;
  try {
    raw = await readFile(recordPath, "utf8");
  } catch (error) {
    const code = typeof error === "object" && error !== null && "code" in error ? String(error.code) : "io_error";
    return {
      kind: "absent",
      failure: {
        kind: "disconnected",
        code: code === "ENOENT" ? "daemon_authority_absent" : code,
        message:
          code === "ENOENT"
            ? `No TraceDecay daemon authority record at ${recordPath}; start the daemon (tracedecay daemon run) first.`
            : `Cannot read ${recordPath}: ${String(error)}`,
      },
    };
  }
  let json: unknown;
  try {
    json = JSON.parse(raw);
  } catch (error) {
    return {
      kind: "invalid",
      failure: { kind: "protocol", code: "daemon_authority_malformed", message: `${recordPath} is not JSON: ${String(error)}` },
    };
  }
  const parsed = DaemonAuthorityRecordSchema.safeParse(json);
  if (!parsed.success) {
    return {
      kind: "invalid",
      failure: {
        kind: "protocol",
        code: "daemon_authority_malformed",
        message: `${recordPath} does not match the daemon authority record: ${parsed.error.message}`,
      },
    };
  }
  return { kind: "available", record: parsed.data };
}

/**
 * The authority record outlives a daemon that exited, so a published record
 * alone does not prove the daemon is serving. Any HTTP response from the
 * published application endpoint (including 401 without a token) proves the
 * process behind the record is alive.
 */
export async function probeDaemon(record: DaemonAuthorityRecord, fetchImpl: typeof fetch = fetch): Promise<Failure | null> {
  const baseUrl = httpBaseUrl(record);
  if (baseUrl === null) {
    // The daemon withdraws its HTTP endpoint from the record when it shuts
    // down, so an endpoint-less record means either "starting" or "exited".
    return processAlive(record.pid)
      ? { kind: "unavailable", code: "http_application_endpoint_absent", message: `Daemon pid ${record.pid} has not published its HTTP application endpoint yet.` }
      : { kind: "disconnected", code: "daemon_exited", message: `Daemon pid ${record.pid} recorded in ${record.profile_root} is no longer running; start it with \`tracedecay daemon run\`.` };
  }
  try {
    await fetchImpl(`${baseUrl}/`, { method: "GET", signal: AbortSignal.timeout(2_000), redirect: "manual" });
    return null;
  } catch (error) {
    return {
      kind: "disconnected",
      code: "daemon_unreachable",
      message: `Daemon pid ${record.pid} published ${baseUrl} but is not answering: ${error instanceof Error ? (error.cause instanceof Error ? error.cause.message : error.message) : String(error)}`,
    };
  }
}

function processAlive(pid: number): boolean {
  if (pid <= 0) return false;
  try {
    process.kill(pid, 0);
    return true;
  } catch (error) {
    return typeof error === "object" && error !== null && "code" in error && error.code === "EPERM";
  }
}

export function httpBaseUrl(record: DaemonAuthorityRecord): string | null {
  const endpoint = record.http_application_endpoint;
  if (endpoint === null || endpoint === undefined) return null;
  return `http://${endpoint}`;
}
