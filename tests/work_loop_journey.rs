//! The production Work-loop journey, end to end, through the real daemon.
//!
//! Plans 09 and 24 both name one direct acceptance journey — seven steps that
//! have to hold together on a live surface rather than one at a time in unit
//! isolation. Every leg of it already had a component test; nothing had ever
//! run the whole loop through the daemon a client actually calls, so the
//! couplings between the legs (a proposal digest surviving into acceptance, an
//! admission gate that a start honours, a pinned configuration snapshot that a
//! real process resolves, a terminal receipt that does not become task
//! acceptance) were untested.
//!
//! Surface. Everything below is a POST to the daemon's published HTTP
//! application endpoint, authenticated with the token and origin from the
//! daemon's own authority record, exactly as
//! `work_route_exposure_conformance.rs` does. Nothing here reaches into an
//! in-process router, a service struct, or a store: if a route is unmounted,
//! an authority is missing, or a projection is not published, this test fails
//! where a client would fail.
//!
//! Provider. Step five needs a *real* spawned process. The repository does not
//! accept a synthetic stand-in for an external provider's behaviour as
//! acceptance evidence, and this test does not create one: it never parses,
//! asserts on, or imitates provider semantics. What it does is exercise the
//! spawn path — the thing the daemon owns — against a local executable pinned
//! through the production configuration control plane. That is the same
//! mechanism the runtime uses for every provider (`PinnedWorkExecutable-
//! BindingResolver`, `src/config/work_executable_binding.rs`): an executable id
//! bound to an absolute path and a sha256 of the file's bytes, verified at
//! spawn time. The binding is written here through `configuration_set` on the
//! live daemon, so the resolution under test is the production one and the
//! only fixture-shaped thing in it is which bytes the digest names.
//!
//! What the script emits is deliberately incidental. `work_attempt_exec.rs`
//! selects argv from the `(backend, protocol)` pair and captures both streams
//! as bounded opaque bytes summarised by length and digest; it parses no
//! framing. The fixture emits Claude `stream-json` shaped lines so the capture
//! is exercised over realistic bytes, and asserts only on what the daemon
//! actually owns: the forwarded argv, the instructions on stdin, the sealed
//! terminal state, and the requested-versus-actual route.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tracedecay::config::USER_DATA_DIR_ENV;
use tracedecay::storage::PrivateStoreIo;
use tracedecay_domain::{
    CommitId, ConfigurationRevisionId, ConfigurationSnapshotId, ManifestDigest, ProposalId,
    ProviderId, RefId, TaskId, UtcMicros, WorkApprovalPolicy, WorkEffectStateV1, WorkEgressPolicy,
    WorkExecutableReference, WorkExecutionLimits, WorkExecutionSnapshot,
    WorkExecutionSnapshotInput, WorkFallbackTopology, WorkFilesystemPolicy, WorkGraphVersionV1,
    WorkProviderBackendV1, WorkProviderProtocol, WorkProviderRouteId, WorkProviderRouteV1,
    WorkRelationReplanProposalV1, WorkSandboxPolicy, WorkflowOperationRef,
};

/// Pins the global database away from the operator's profile. The production
/// constant is crate-private, so the name is repeated here for the same reason
/// the shared test harness repeats it.
const GLOBAL_DB_ENV: &str = "TRACEDECAY_GLOBAL_DB";

/// The one Work task this journey drives from creation to acceptance.
const TASK_ID: &str = "task.work-loop-journey";
const RUN_ID: &str = "run.work-loop-journey";
/// The attempt that runs a provider to a clean terminal receipt.
const SETTLED_ATTEMPT_ID: &str = "attempt.work-loop-journey.settled";
/// The attempt cancelled while its provider is still running, i.e. before the
/// effect commit point.
const CANCELLED_ATTEMPT_ID: &str = "attempt.work-loop-journey.cancelled";

/// Executable ids pinned through the configuration control plane. They sort
/// ascending, which the setting's own validator requires.
const FAST_EXECUTABLE_ID: &str = "executable.work-loop-journey.fast";
const SLOW_EXECUTABLE_ID: &str = "executable.work-loop-journey.slow";

/// A symbol name the fixture defines exactly once, used as the source anchor
/// step two retrieves and expands.
const ANCHOR_SYMBOL: &str = "work_loop_journey_anchor";

/// Instructions written to the provider's stdin; asserted byte-exact on the
/// other side.
const INSTRUCTIONS: &str = "Execute the admitted Work-loop journey step.";

/// How long a poll may wait for an eventually consistent publication (project
/// runtime mount, code index generation, background attempt settlement).
const POLL_BUDGET: Duration = Duration::from_secs(180);

/// Serializes the process-wide environment this binary pins. One test lives
/// here today; the lock keeps that from being a latent assumption.
fn lock_env() -> MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Restores a process environment variable when the guard drops.
struct EnvVarGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvVarGuard {
    fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
        let previous = std::env::var_os(key);
        // The fixture holds the binary-wide environment lock for its whole
        // life, so no other thread reads the environment while it is pinned.
        unsafe { std::env::set_var(key, value) };
        Self { key, previous }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        unsafe {
            match self.previous.take() {
                Some(previous) => std::env::set_var(self.key, previous),
                None => std::env::remove_var(self.key),
            }
        }
    }
}

/// A live daemon over a registered project under a throwaway profile, plus the
/// credentials it published for its own HTTP application endpoint.
struct ProductionDaemon {
    daemon: Child,
    project: PathBuf,
    project_id: String,
    repository_id: String,
    base_url: String,
    origin: String,
    authorization: String,
    agent: ureq::Agent,
    _home: TempDir,
    _guards: Vec<EnvVarGuard>,
    _env_lock: MutexGuard<'static, ()>,
}

impl ProductionDaemon {
    fn start() -> Self {
        let env_lock = lock_env();
        let home = tempfile::tempdir().expect("isolated home");
        let root = home.path().to_path_buf();
        let profile = root.join(".tracedecay");
        let project = root.join("project");
        PrivateStoreIo::create_dir_all(&profile).expect("isolated profile root");
        fs::create_dir_all(project.join("src")).expect("isolated project root");
        fs::write(
            project.join("Cargo.toml"),
            "[package]\nname=\"work-loop-journey-fixture\"\nversion=\"0.0.0\"\nedition=\"2024\"\n",
        )
        .expect("fixture manifest");
        // One indexable symbol whose name appears nowhere else, so step two's
        // retrieval resolves an unambiguous anchor and its expansion can be
        // checked against bytes this test wrote.
        fs::write(
            project.join("src/lib.rs"),
            format!("pub fn {ANCHOR_SYMBOL}(seed: u32) -> u32 {{\n    seed.wrapping_add(1)\n}}\n"),
        )
        .expect("fixture source");

        let guards = vec![
            EnvVarGuard::set("HOME", &root),
            EnvVarGuard::set("USERPROFILE", &root),
            EnvVarGuard::set("XDG_CONFIG_HOME", root.join(".config")),
            EnvVarGuard::set(USER_DATA_DIR_ENV, &profile),
            EnvVarGuard::set(GLOBAL_DB_ENV, profile.join("global.db")),
            EnvVarGuard::set("TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN", "1"),
        ];

        run_ok(
            Command::new("git")
                .args(["init", "--quiet"])
                .current_dir(&project),
            "git init",
        );
        // Symbol-graph admission requires an exact source revision; an unborn
        // HEAD can never satisfy it, so the fixture needs one real commit.
        run_ok(
            Command::new("git").args(["add", "."]).current_dir(&project),
            "git add",
        );
        run_ok(
            Command::new("git")
                .args([
                    "-c",
                    "user.email=journey@tracedecay.invalid",
                    "-c",
                    "user.name=Journey",
                    "commit",
                    "--quiet",
                    "-m",
                    "fixture",
                ])
                .current_dir(&project),
            "git commit",
        );
        let mut daemon = isolated(&root, &profile)
            .args(["daemon", "run"])
            .current_dir(&project)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("daemon should start");
        let authority = wait_for_authority(&mut daemon, &daemon_authority_path(&profile));
        run_ok(
            isolated(&root, &profile).arg("init").current_dir(&project),
            "tracedecay init",
        );

        let context = run_ok(
            isolated(&root, &profile)
                .args(["projects", "context"])
                .arg(&project)
                .arg("--json")
                .current_dir(&project),
            "tracedecay projects context",
        );
        let context: Value = serde_json::from_slice(&context).expect("project context JSON");
        let project_id = context["project"]["project_id"]
            .as_str()
            .expect("registered project id")
            .to_owned();
        let git_common_dir = tracedecay::worktree::git_common_dir(&project)
            .expect("registered Git common directory");
        let repository_id = format!(
            "repository.daemon.{}",
            hex::encode(Sha256::digest(git_common_dir.to_string_lossy().as_bytes()))
        );

        let endpoint = authority["http_application_endpoint"]
            .as_str()
            .expect("published HTTP application endpoint")
            .to_owned();
        let token = authority["auth_token"]
            .as_str()
            .expect("published auth token")
            .to_owned();

        Self {
            daemon,
            project,
            project_id,
            repository_id,
            base_url: format!("http://{endpoint}"),
            origin: format!("http://{endpoint}"),
            authorization: format!("Bearer {token}"),
            agent: ureq::Agent::config_builder()
                .http_status_as_error(false)
                .timeout_global(Some(Duration::from_secs(60)))
                .build()
                .into(),
            _home: home,
            _guards: guards,
            _env_lock: env_lock,
        }
    }

    /// External URL for a canonical route path, which already starts with
    /// `/application` and therefore composes directly onto the project prefix.
    fn url(&self, route_path: &str) -> String {
        format!(
            "{}/projects/{}{}",
            self.base_url, self.project_id, route_path
        )
    }

    fn post(&self, route_path: &str, body: &Value) -> (u16, Value) {
        let url = self.url(route_path);
        let mut response = self
            .agent
            .post(&url)
            .header("authorization", &self.authorization)
            .header("origin", &self.origin)
            .content_type("application/json")
            .send(body.to_string())
            .unwrap_or_else(|error| panic!("POST {url} failed: {error}"));
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .read_to_string()
            .unwrap_or_else(|error| panic!("POST {url} body failed: {error}"));
        let parsed: Value = serde_json::from_str(&text)
            .unwrap_or_else(|error| panic!("POST {url} answered non-JSON `{text}`: {error}"));
        (status, parsed)
    }

    /// Follows only the daemon's explicit same-request warming contract. A
    /// terminal problem or malformed retry response is returned immediately,
    /// so callers still prove their exact success or refusal below.
    fn post_settled(&self, label: &str, route_path: &str, body: &Value) -> (u16, Value) {
        poll_until(label, || {
            let (status, answer) = self.post(route_path, body);
            if status == 503
                && answer["kind"] == "problem"
                && answer["value"]["problem"]["retryable"] == true
                && answer["value"]["problem"]["retry_scope"] == "same_request"
            {
                return Err(format!("{status} {answer}"));
            }
            Ok((status, answer))
        })
    }

    /// Posts and requires the canonical success envelope, returning the
    /// operation payload. Every step of the journey that must succeed goes
    /// through here, so a refusal is reported with its whole envelope.
    fn payload(&self, label: &str, route_path: &str, body: &Value) -> Value {
        let (status, answer) = self.post_settled(label, route_path, body);
        assert_eq!(
            answer["kind"], "success",
            "{label} must succeed at {route_path}: {status} {answer}"
        );
        assert_eq!(status, 200, "{label}: {answer}");
        answer["value"]["outcome"]["value"]["payload"].clone()
    }

    /// Posts and requires a typed refusal, returning the problem record.
    fn problem(&self, label: &str, route_path: &str, body: &Value) -> Value {
        let (status, answer) = self.post_settled(label, route_path, body);
        assert_eq!(
            answer["kind"], "problem",
            "{label} must be refused at {route_path}: {status} {answer}"
        );
        assert!(
            (400..500).contains(&status),
            "{label} must be a client refusal, not a server fault: {status} {answer}"
        );
        assert!(
            answer["value"]["problem"]["retry"].is_string(),
            "{label} must state a retry directive: {answer}"
        );
        answer["value"]["problem"].clone()
    }
}

impl Drop for ProductionDaemon {
    fn drop(&mut self) {
        // Only this fixture's own daemon is signalled; nothing else on the
        // host is touched.
        let _ = self.daemon.kill();
        let _ = self.daemon.wait();
    }
}

fn daemon_authority_path(profile_root: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        profile_root
            .join("daemon-authority")
            .join("daemon-authority.json")
    }
    #[cfg(not(windows))]
    {
        profile_root.join("daemon-authority.json")
    }
}

fn isolated(home: &Path, profile: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tracedecay"));
    command
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env(USER_DATA_DIR_ENV, profile)
        .env(GLOBAL_DB_ENV, profile.join("global.db"))
        .env("TRACEDECAY_TEST_ALLOW_INCOMPLETE_HOLDER_SCAN", "1");
    command
}

fn run_ok(command: &mut Command, label: &str) -> Vec<u8> {
    let output = command
        .output()
        .unwrap_or_else(|error| panic!("{label} could not run: {error}"));
    assert!(
        output.status.success(),
        "{label} failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

fn wait_for_authority(daemon: &mut Child, path: &Path) -> Value {
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if let Some(status) = daemon.try_wait().expect("daemon status") {
            let mut stderr = String::new();
            if let Some(mut piped) = daemon.stderr.take() {
                let _ = piped.read_to_string(&mut stderr);
            }
            panic!("daemon exited before publishing authority: {status}; stderr: {stderr}");
        }
        if let Ok(bytes) = fs::read(path)
            && let Ok(record) = serde_json::from_slice::<Value>(&bytes)
            && record["auth_token"]
                .as_str()
                .is_some_and(|token| token.len() == 64)
            && record["http_application_endpoint"].as_str().is_some()
        {
            return record;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!(
        "timed out waiting for a published HTTP application endpoint at {}",
        path.display()
    );
}

/// Runs a poll to a deadline, requiring the closure to return `Some` before it
/// expires. The last observation is reported so a timeout says what it saw.
fn poll_until<T>(label: &str, mut probe: impl FnMut() -> Result<T, String>) -> T {
    let deadline = Instant::now() + POLL_BUDGET;
    let mut last = match probe() {
        Ok(value) => return value,
        Err(observation) => observation,
    };
    loop {
        assert!(
            Instant::now() < deadline,
            "{label} never settled within {POLL_BUDGET:?}; last observation: {last}"
        );
        std::thread::sleep(Duration::from_millis(250));
        match probe() {
            Ok(value) => return value,
            Err(observation) => last = observation,
        }
    }
}

/// Microsecond clock, used for the `occurred_at` every Work command carries.
fn now_micros() -> i64 {
    i64::try_from(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock after the epoch")
            .as_micros(),
    )
    .expect("microsecond clock fits i64")
}

fn product_selection(fixture: &ProductionDaemon) -> Value {
    json!({
        "selection": "relations",
        "relation_scopes": [{
            "kind": "repository",
            "project_id": fixture.project_id,
            "repository_id": fixture.repository_id,
        }],
    })
}

fn product_graph_request(fixture: &ProductionDaemon) -> Value {
    json!({
        "selection": product_selection(fixture),
        "mode": { "mode": "current" },
        "continuation": null,
        "observed_at": now_micros(),
    })
}

fn product_task_create_draft(fixture: &ProductionDaemon) -> Value {
    let occurred_at = now_micros();
    json!({
        "selection": product_selection(fixture),
        "causation_event_id": null,
        "evidence": [],
        "change": {
            "change": "create_task",
            "initiative": {
                "id": "initiative.work-loop-journey",
                "title": "Work loop journey",
                "created_at": occurred_at,
            },
            "plan": {
                "id": "plan.work-loop-journey",
                "initiative_id": "initiative.work-loop-journey",
                "title": "Work loop journey",
                "created_at": occurred_at,
            },
            "milestone": {
                "id": "milestone.work-loop-journey",
                "plan_id": "plan.work-loop-journey",
                "title": "Work loop journey",
                "created_at": occurred_at,
            },
            "item": {
                "input": {
                    "task_id": TASK_ID,
                    "hierarchy": {
                        "initiative_id": "initiative.work-loop-journey",
                        "plan_id": "plan.work-loop-journey",
                        "milestone_id": "milestone.work-loop-journey",
                    },
                    "title": "Work loop journey",
                    "dependencies": [],
                    "informational_relations": [],
                    "causal_candidates": [],
                    "acceptance_criteria": [],
                    "effort": 1,
                    "scheduled_at": null,
                    "deadline": null,
                    "created_at": occurred_at,
                    "updated_at": occurred_at,
                },
                "accepted_proposal": null,
                "accepted_route": null,
                "execution_admitted_at": null,
                "evidence_links": [],
                "accepted_criteria": {},
                "accepted_attempts": [],
                "handoffs": [],
                "accepted_at": null,
                "archived_at": null,
            },
        },
    })
}

fn prepare_product_mutation(fixture: &ProductionDaemon, label: &str, change: Value) -> Value {
    fixture.payload(
        label,
        "/application/work/prepare-graph-mutation",
        &json!({
            "selection": product_selection(fixture),
            "change": change,
            "causation_event_id": null,
            "evidence": [],
        }),
    )
}

fn commit_product_mutation(fixture: &ProductionDaemon, label: &str, mutation: &Value) -> Value {
    fixture.payload(label, "/application/work/mutate-graph", mutation)
}

fn product_graph(fixture: &ProductionDaemon, label: &str) -> Value {
    fixture.payload(
        label,
        "/application/work/views",
        &product_graph_request(fixture),
    )
}

fn graph_version(graph: &Value) -> u64 {
    graph["snapshot"]["graph"]["version"]
        .as_u64()
        .unwrap_or_else(|| panic!("current product graph must carry a version: {graph}"))
}

fn typed<T>(value: &str) -> T
where
    T: TryFrom<String>,
    T::Error: std::fmt::Debug,
{
    T::try_from(value.to_owned()).expect("canonical typed identifier")
}

/// Writes an executable shell script and returns its path with the sha256 the
/// configuration binding will pin. The script text carries absolute marker
/// paths because the spawn path calls `env_clear()`.
#[cfg(unix)]
fn pinned_executable(directory: &Path, name: &str, body: &str) -> (PathBuf, ManifestDigest) {
    use std::os::unix::fs::PermissionsExt;

    let path = directory.join(name);
    fs::write(&path, body).expect("fixture executable");
    let mut permissions = fs::metadata(&path)
        .expect("executable metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&path, permissions).expect("executable mode");
    let digest = ManifestDigest::new(format!(
        "sha256:{}",
        hex::encode(Sha256::digest(body.as_bytes()))
    ))
    .expect("sha256 digest");
    (
        path.canonicalize().expect("canonical executable path"),
        digest,
    )
}

/// The execution snapshot a start-attempt command pins. Every configuration
/// identity in it is read back out of the live control plane, so the attempt
/// records the snapshot that actually governed it rather than a literal.
struct PinnedConfiguration {
    revision_id: String,
    snapshot_id: String,
    effective_behavior_digest: String,
    resolution_provenance_digest: String,
}

#[allow(clippy::too_many_arguments)]
fn execution_snapshot(
    configuration: &PinnedConfiguration,
    executable_id: &str,
    artifact_digest: &ManifestDigest,
    deadline: UtcMicros,
) -> Value {
    let snapshot = WorkExecutionSnapshot::new(WorkExecutionSnapshotInput {
        configuration_revision_id: typed::<ConfigurationRevisionId>(&configuration.revision_id),
        configuration_snapshot_id: typed::<ConfigurationSnapshotId>(&configuration.snapshot_id),
        effective_behavior_digest: ManifestDigest::new(
            configuration.effective_behavior_digest.clone(),
        )
        .expect("effective behavior digest"),
        resolution_provenance_digest: ManifestDigest::new(
            configuration.resolution_provenance_digest.clone(),
        )
        .expect("resolution provenance digest"),
        route: requested_route(),
        backend: WorkProviderBackendV1::ClaudeCodeCli,
        protocol: WorkProviderProtocol::ClaudeStreamJson,
        model: "model.work-loop-journey".to_owned(),
        executable: WorkExecutableReference::new(executable_id.to_owned(), artifact_digest.clone())
            .expect("pinned executable reference"),
        sandbox: WorkSandboxPolicy::Required,
        approval: WorkApprovalPolicy::Never,
        filesystem: WorkFilesystemPolicy::WorkspaceWrite,
        egress: WorkEgressPolicy::Deny,
        environment_allowlist: BTreeSet::new(),
        credential_references: BTreeSet::new(),
        limits: WorkExecutionLimits::new(128_000, 8_192, 65_536, 65_536, 65_536, 1)
            .expect("execution limits"),
        deadline,
        fallback: WorkFallbackTopology::Disabled,
        topology: tracedecay_domain::safe_work_topology_policy_v1(),
    })
    .expect("valid execution snapshot");
    serde_json::to_value(snapshot).expect("execution snapshot encodes")
}

/// The route the attempt requests. `work_attempt_exec` reports the actual
/// route alongside it on the receipt, which is what step six compares.
fn requested_route() -> WorkProviderRouteV1 {
    WorkProviderRouteV1::new(
        typed::<ProviderId>("provider.work.claude-code-cli"),
        typed::<WorkProviderRouteId>("route.work-loop-journey.v1"),
    )
    .expect("provider route")
}

fn start_attempt_body(
    project: &Path,
    attempt_id: &str,
    execution_snapshot: Value,
    occurred_at: i64,
) -> Value {
    json!({
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "attempt_id": attempt_id,
        "operation": typed::<WorkflowOperationRef>("operation.work-loop-journey"),
        "execution_snapshot": execution_snapshot,
        "worktree_root": project.to_string_lossy(),
        "reference": typed::<RefId>("refs/heads/work-loop-journey"),
        "commit": typed::<CommitId>("0123456789abcdef0123456789abcdef01234567"),
        "instructions": INSTRUCTIONS,
        "effect_state": WorkEffectStateV1::Observational,
        "occurred_at": occurred_at,
    })
}

/// The seven-step production Work loop, on the surface a client calls.
#[cfg(unix)]
#[test]
fn the_work_loop_journey_runs_end_to_end_through_the_daemon() {
    let fixture = ProductionDaemon::start();
    let scripts = tempfile::tempdir().expect("provider script directory");

    // ---------------------------------------------------------------------
    // Warming. Mutation preparation is the first product-authority handoff.
    // While the per-project runtime is binding it may answer only the typed
    // retryable unavailable problem; once mounted it supplies the exact
    // command, graph authority, and revision pins the caller must submit.
    // ---------------------------------------------------------------------
    let create_draft = product_task_create_draft(&fixture);
    let prepared_create = poll_until("the project runtime mount", || {
        let (status, answer) =
            fixture.post("/application/work/prepare-graph-mutation", &create_draft);
        if status == 503 {
            assert_eq!(
                answer["value"]["problem"]["kind"], "unavailable",
                "a warming Work read may only answer the typed unavailable problem: {answer}"
            );
            assert_eq!(
                answer["value"]["problem"]["retryable"], true,
                "a warming Work read must be retryable: {answer}"
            );
            return Err(format!("{status} {answer}"));
        }
        assert_eq!(answer["kind"], "success", "{status} {answer}");
        Ok(answer["value"]["outcome"]["value"]["payload"].clone())
    });

    // =====================================================================
    // 1. Create versioned Work, and prove the identity is content-addressed.
    // =====================================================================
    assert_eq!(
        prepared_create["mutation"], "create_task",
        "{prepared_create}"
    );
    let created = commit_product_mutation(&fixture, "create product task", &prepared_create);
    assert_eq!(created["event"]["payload"]["kind"], "created", "{created}");
    assert_eq!(
        created["event"]["payload"]["graph"]["items"][0]["input"]["task_id"],
        TASK_ID
    );
    assert_eq!(
        created["verified_graph_version"]["graph_version"], 1,
        "a created task starts at the initial version: {created}"
    );

    // Idempotent replay. The same command id is the same command: the durable
    // history must not grow, so the version must not move.
    let replayed = commit_product_mutation(&fixture, "create replay", &prepared_create);
    assert!(replayed["replayed"].as_bool().is_some_and(|value| value));
    assert_eq!(
        replayed["event"], created["event"],
        "replaying a prepared command must return the same durable event"
    );

    // Hold one valid command at version one. A later committed proposal
    // decision advances the graph; submitting this exact prepared command
    // afterwards must prove the CAS rather than being silently re-anchored.
    let stale_accept = prepare_product_mutation(
        &fixture,
        "prepare future stale acceptance",
        json!({
            "change": "accept_task",
            "task_id": TASK_ID,
            "evidence_by_criterion": {},
        }),
    );

    // =====================================================================
    // 2. Retrieve exact authorized evidence, and expand a source anchor.
    // =====================================================================
    // The authorized Work evidence: the write above is readable in this
    // scope's exact product graph, with one verified version and no synthetic
    // compatibility projection.
    let graph = product_graph(&fixture, "current product graph");
    assert_eq!(graph["mode"], "current", "{graph}");
    assert!(
        graph["snapshot"]["graph"]["items"]
            .as_array()
            .is_some_and(|items| items.iter().any(|item| item["input"]["task_id"] == TASK_ID)),
        "the created task must be readable as authorized evidence: {graph}"
    );

    // The code anchor. The index publishes behind the write, so this polls to
    // the first generation that serves the fixture symbol rather than assuming
    // one. The anchor is read back from the daemon, never constructed here.
    let anchor_request = json!({
        "query": ANCHOR_SYMBOL,
        "scope": { "path_prefix": Value::Null },
        "lazy_index_ignored_dependencies": false,
        "meta": { "projection": "summary", "order": "relevance", "cursor": Value::Null },
    });
    let anchor_node_id = poll_until("the fixture source anchor", || {
        let (status, answer) =
            fixture.post("/application/code/code_symbol_search", &anchor_request);
        if answer["kind"] != "success" {
            return Err(format!("{status} {answer}"));
        }
        let payload = &answer["value"]["outcome"]["value"]["payload"];
        payload["items"]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["name"] == ANCHOR_SYMBOL))
            .and_then(|item| item["node_id"].as_str())
            .map(str::to_owned)
            .ok_or_else(|| format!("{payload}"))
    });

    // Expanding the anchor returns the exact bytes this test wrote, which is
    // what makes it an expansion of *that* anchor rather than a plausible one.
    let expanded = fixture.payload(
        "source anchor expansion",
        "/application/primitives/source_body",
        &json!({ "node_id": anchor_node_id }),
    );
    assert_eq!(expanded["file"], "src/lib.rs", "{expanded}");
    assert!(
        expanded["body"]
            .as_str()
            .is_some_and(|body| body.contains("seed.wrapping_add(1)")),
        "the expanded anchor must carry the fixture's own source: {expanded}"
    );

    // =====================================================================
    // 3. The explained proposal, over the pinned redacted configuration.
    // =====================================================================
    // The control plane is the display authority for what governs the task.
    // A sensitive setting is readable as an effective value with its snapshot
    // identity and provenance, and the plaintext of a credential never is —
    // this reads the setting the provider step will later be pinned to.
    let bindings_key = "work.executable_bindings.v1";
    let resolved = fixture.payload(
        "configuration get (work executable bindings)",
        "/application/configuration/configuration_get",
        &json!({ "key": bindings_key }),
    );
    assert_eq!(resolved["key"], bindings_key, "{resolved}");
    assert!(
        resolved["snapshot_id"].is_string()
            && resolved["effective_behavior_digest"].is_string()
            && resolved["resolution_provenance_digest"].is_string(),
        "a resolved setting must carry its snapshot identity and provenance: {resolved}"
    );

    let proposal = fixture.payload(
        "generate proposal",
        "/application/work/generate-proposal",
        &json!({
            "selection": product_selection(&fixture),
            "task_id": TASK_ID,
            "proposal_id": "proposal.work-loop-journey.acceptance",
            "live_git_evidence": Value::Null,
            "occurred_at": now_micros(),
        }),
    );
    assert_eq!(proposal["proposal"]["based_on_version"], 1, "{proposal}");
    assert_eq!(
        proposal["decision"]["disposition"], "allow",
        "a ready task with no unresolved dependencies is allowed: {proposal}"
    );
    assert_eq!(
        proposal["decision"]["recommended_action"], "proceed_to_acceptance",
        "the first recommendation is acceptance, not execution: {proposal}"
    );
    // The decision is explained: it names the evaluator that produced it and
    // the ordered reasons, and it binds the configuration that governed it.
    assert!(
        proposal["decision"]["evaluator_id"].is_string()
            && proposal["decision"]["ordered_reason_codes"]
                .as_array()
                .is_some_and(|reasons| !reasons.is_empty()),
        "a proposal must be explained, not merely emitted: {proposal}"
    );
    assert!(
        proposal["decision"]["configuration_digest"].is_string(),
        "the decision must bind the configuration it was made under: {proposal}"
    );
    // Generation is read-only: nothing about the task moved.
    let after_proposal = product_graph(&fixture, "product graph after proposal");
    assert_eq!(
        graph_version(&after_proposal),
        1,
        "generating a proposal must not write: {after_proposal}"
    );

    // =====================================================================
    // 4. Explicit proposal acceptance.
    // =====================================================================
    let prepared_accept = prepare_product_mutation(
        &fixture,
        "prepare proposal acceptance",
        json!({
            "change": "decide_proposal",
            "proposal": proposal["proposal"].clone(),
            "disposition": "accepted",
        }),
    );
    let accepted = commit_product_mutation(&fixture, "accept proposal", &prepared_accept);
    assert_eq!(accepted["event"]["payload"]["kind"], "changed");
    assert_eq!(
        accepted["event"]["payload"]["change"]["kind"],
        "proposal_accepted"
    );
    assert_eq!(accepted["verified_graph_version"]["graph_version"], 2);
    let accepted_graph = product_graph(&fixture, "product graph after proposal acceptance");
    let accepted_item = task_item(&accepted_graph);
    assert_eq!(
        accepted_item["accepted_proposal"],
        proposal["proposal"]["proposal_id"]
    );
    assert_eq!(
        accepted_item["execution_admitted_at"],
        Value::Null,
        "accepting a proposal must not admit execution: {accepted_graph}"
    );

    let stale = fixture.problem(
        "submit a prepared command after its graph head moved",
        "/application/work/mutate-graph",
        &stale_accept,
    );
    assert_eq!(stale["kind"], "stale", "{stale}");
    assert_eq!(
        stale["code"], "work.graph_version_conflict",
        "the refusal must name the product graph CAS: {stale}"
    );
    assert_eq!(stale["retry"], "after_revalidate", "{stale}");
    assert_eq!(stale["retry_scope"], "fresh_request", "{stale}");

    // Acceptance is not admission: a start refuses until admission happens.
    let unadmitted = fixture.problem(
        "start before admission",
        "/application/work/start-attempt",
        &start_attempt_body(
            &fixture.project,
            SETTLED_ATTEMPT_ID,
            execution_snapshot(
                &PinnedConfiguration {
                    revision_id: "configuration.work-loop-journey.probe".to_owned(),
                    snapshot_id: "configuration-snapshot.work-loop-journey.probe".to_owned(),
                    effective_behavior_digest: format!("sha256:{}", "1".repeat(64)),
                    resolution_provenance_digest: format!("sha256:{}", "2".repeat(64)),
                },
                FAST_EXECUTABLE_ID,
                &ManifestDigest::new(format!("sha256:{}", "3".repeat(64))).expect("probe digest"),
                UtcMicros(now_micros().saturating_add(120 * 1_000_000)),
            ),
            now_micros(),
        ),
    );
    assert_eq!(
        unadmitted["code"], "application.work-attempt.execution-not-admitted",
        "starting an attempt before execution admission must be refused by the \
         admission gate, not by anything downstream of it: {unadmitted}"
    );

    // The proposal recommendation now moves to admission, on its own evidence.
    let admission_proposal = fixture.payload(
        "generate proposal after acceptance",
        "/application/work/generate-proposal",
        &json!({
            "selection": product_selection(&fixture),
            "task_id": TASK_ID,
            "proposal_id": "proposal.work-loop-journey.admission",
            "live_git_evidence": Value::Null,
            "occurred_at": now_micros(),
        }),
    );
    assert_eq!(
        admission_proposal["decision"]["recommended_action"], "admit_execution",
        "{admission_proposal}"
    );

    // =====================================================================
    // 5. Separate execution admission, then one real provider step.
    // =====================================================================
    let prepared_admission = prepare_product_mutation(
        &fixture,
        "prepare execution admission",
        json!({ "change": "admit_execution", "task_id": TASK_ID }),
    );
    let admitted = commit_product_mutation(&fixture, "admit execution", &prepared_admission);
    assert_eq!(admitted["verified_graph_version"]["graph_version"], 3);
    let admitted_graph = product_graph(&fixture, "product graph after execution admission");
    let admitted_item = task_item(&admitted_graph);
    assert!(
        admitted_item["execution_admitted_at"].is_number(),
        "{admitted_graph}"
    );
    assert_eq!(
        admitted_item["accepted_at"],
        Value::Null,
        "admitting execution must not accept the task: {admitted_graph}"
    );

    // Pin two provider executables through the production control plane. The
    // resolver canonicalizes the path and re-digests the bytes at spawn time,
    // so this is the same fail-closed admission a shipped provider goes
    // through — the only fixture-shaped part is which bytes are pinned.
    let argv_marker = scripts.path().join("argv");
    let stdin_marker = scripts.path().join("stdin");
    let (fast_path, fast_digest) = pinned_executable(
        scripts.path(),
        "fast-provider",
        &format!(
            "#!/bin/sh\nprintf '%s' \"$*\" > {argv}\ncat > {stdin}\n\
             printf '%s\\n' '{{\"type\":\"system\",\"subtype\":\"init\"}}'\n\
             printf '%s\\n' '{{\"type\":\"result\",\"subtype\":\"success\",\"is_error\":false}}'\n\
             exit 0\n",
            argv = argv_marker.display(),
            stdin = stdin_marker.display(),
        ),
    );
    let started_marker = scripts.path().join("started");
    let (slow_path, slow_digest) = pinned_executable(
        scripts.path(),
        "slow-provider",
        &format!(
            "#!/bin/sh\nprintf x > {started}\ncat > /dev/null\n\
             i=0\nwhile [ $i -lt 300 ]; do sleep 1; i=$((i+1)); done\nexit 0\n",
            started = started_marker.display(),
        ),
    );

    let observed = fixture.payload(
        "configuration observed state",
        "/application/configuration/configuration_observed_state",
        &json!({}),
    );
    let base_revision = observed
        .as_array()
        .and_then(|components| components.first())
        .and_then(|component| component["desired_revision_id"].as_str())
        .unwrap_or_else(|| panic!("configuration observed state: {observed}"))
        .to_owned();

    let set = fixture.payload(
        "configuration set (work executable bindings)",
        "/application/configuration/configuration_set",
        &json!({
            "layer": { "kind": "project", "project_id": fixture.project_id },
            "key": bindings_key,
            "value": {
                "kind": "work_executable_bindings",
                "value": [
                    {
                        "executable": {
                            "executable_id": FAST_EXECUTABLE_ID,
                            "artifact_digest": fast_digest,
                        },
                        "canonical_path": fast_path,
                        "capabilities": ["claude_code_stream_json"],
                    },
                    {
                        "executable": {
                            "executable_id": SLOW_EXECUTABLE_ID,
                            "artifact_digest": slow_digest,
                        },
                        "canonical_path": slow_path,
                        "capabilities": ["claude_code_stream_json"],
                    },
                ],
            },
            "expected_revision": base_revision,
            "idempotency_key": "configuration.idempotency.work-loop-journey.bindings",
        }),
    );
    assert_ne!(
        set["result_revision_id"], set["base_revision_id"],
        "a committed configuration write must advance the revision: {set}"
    );

    // Read the pinned snapshot back and carry its exact identity into the
    // attempt, so the attempt records the configuration that governed it.
    let pinned_setting = fixture.payload(
        "configuration get after pinning",
        "/application/configuration/configuration_get",
        &json!({ "key": bindings_key }),
    );
    let pinned = PinnedConfiguration {
        revision_id: set["result_revision_id"]
            .as_str()
            .expect("result revision id")
            .to_owned(),
        snapshot_id: pinned_setting["snapshot_id"]
            .as_str()
            .expect("pinned snapshot id")
            .to_owned(),
        effective_behavior_digest: pinned_setting["effective_behavior_digest"]
            .as_str()
            .expect("effective behavior digest")
            .to_owned(),
        resolution_provenance_digest: pinned_setting["resolution_provenance_digest"]
            .as_str()
            .expect("resolution provenance digest")
            .to_owned(),
    };

    // A run's first attempt seals its immutable deadline and topology. Every
    // later attempt in the same run must carry that exact authority rather
    // than silently extending the run by minting a fresh relative deadline.
    let run_deadline = UtcMicros(now_micros().saturating_add(600 * 1_000_000));
    let start = start_attempt_body(
        &fixture.project,
        SETTLED_ATTEMPT_ID,
        execution_snapshot(&pinned, FAST_EXECUTABLE_ID, &fast_digest, run_deadline),
        now_micros(),
    );
    let leased = fixture.payload("start attempt", "/application/work/start-attempt", &start);
    assert_eq!(leased["state"], "leased", "{leased}");
    assert_eq!(
        leased["requested_route"]["provider_id"], "provider.work.claude-code-cli",
        "{leased}"
    );

    // Idempotent replay of the admission: the same identity with the same
    // content returns the durable attempt, never a second lease.
    let replayed_start = fixture.payload(
        "start attempt replay",
        "/application/work/start-attempt",
        &start,
    );
    assert_eq!(
        replayed_start["lease"], leased["lease"],
        "replaying a start must return the same lease: {replayed_start}"
    );

    // =====================================================================
    // 6. Progress, resumption, and the truthful terminal receipt.
    // =====================================================================
    let status_request = json!({
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "attempt_id": SETTLED_ATTEMPT_ID,
    });
    let settled = poll_until("the provider attempt terminal receipt", || {
        let attempt = fixture.payload(
            "attempt status",
            "/application/work/attempt-status",
            &status_request,
        );
        match attempt["state"].as_str() {
            Some("succeeded") => Ok(attempt),
            // Every pre-terminal answer must still be a state the runtime
            // declares, so a stuck attempt fails loudly rather than silently.
            Some("leased" | "running") => Err(format!("{attempt}")),
            _ => panic!("the provider attempt reached an unexpected state: {attempt}"),
        }
    });

    // The daemon owns argv selection and stdin delivery; both reached a real
    // child process.
    assert_eq!(
        fs::read_to_string(&argv_marker).expect("provider argv marker"),
        "--print --output-format stream-json --verbose",
        "the protocol's argv must reach the spawned provider"
    );
    assert_eq!(
        fs::read_to_string(&stdin_marker).expect("provider stdin marker"),
        INSTRUCTIONS,
        "the attempt instructions must reach the provider's stdin"
    );

    // The receipt is truthful about the route: it reports what was requested
    // and what actually ran, as two separate facts.
    assert_eq!(settled["terminal"]["outcome"], "succeeded", "{settled}");
    assert!(
        settled["terminal"]["evidence_digest"].is_string(),
        "a terminal receipt must carry its evidence digest: {settled}"
    );
    assert_eq!(
        settled["requested_route"], leased["requested_route"],
        "the receipt must preserve the requested route verbatim: {settled}"
    );
    assert_eq!(
        settled["actual_route"], settled["requested_route"],
        "an unrouted-away attempt must report the route it actually ran: {settled}"
    );

    // Activation drift is reported, not inferred: the control plane states
    // whether the committed revision is the one the runtime is running.
    let drift = fixture.payload(
        "configuration observed state after pinning",
        "/application/configuration/configuration_observed_state",
        &json!({}),
    );
    let components = drift
        .as_array()
        .unwrap_or_else(|| panic!("configuration observed state: {drift}"));
    assert!(!components.is_empty(), "{drift}");
    for component in components {
        assert!(
            matches!(
                component["drift"].as_str(),
                Some("current" | "never_activated" | "pending_restart" | "activation_failed")
            ),
            "activation drift must be one of the declared states: {drift}"
        );
    }
    assert!(
        components
            .iter()
            .any(|component| component["desired_revision_id"] == set["result_revision_id"]),
        "the desired revision must be the one the write committed: {drift}"
    );

    // Resumption is idempotent over a settled attempt: a terminal receipt is
    // never reopened by recovery.
    let recovery = fixture.payload(
        "resume attempts",
        "/application/work/resume-attempts",
        &json!({ "occurred_at": now_micros() }),
    );
    assert_eq!(
        recovery["recovery_required"].as_array().map(Vec::len),
        Some(0),
        "a settled attempt must not be reopened by resumption: {recovery}"
    );
    let after_resume = fixture.payload(
        "attempt status after resume",
        "/application/work/attempt-status",
        &status_request,
    );
    assert_eq!(
        after_resume["terminal"], settled["terminal"],
        "resumption must not rewrite a sealed receipt: {after_resume}"
    );

    // Cancellation *after* the effect commit point. The attempt has already
    // sealed its receipt; cancelling it is a typed refusal, and the receipt
    // stays exactly as it was.
    let late_cancellation = fixture.problem(
        "cancel a settled attempt",
        "/application/work/cancel-attempt",
        &json!({
            "task_id": TASK_ID,
            "run_id": RUN_ID,
            "attempt_id": SETTLED_ATTEMPT_ID,
            "request_id": "cancellation.work-loop-journey.late",
            "occurred_at": now_micros(),
        }),
    );
    assert_eq!(
        late_cancellation["kind"], "conflict",
        "cancelling past the commit point is a conflict, not a silent no-op: {late_cancellation}"
    );
    assert_eq!(
        late_cancellation["code"], "application.work-attempt.not-cancellable",
        "the refusal must name the terminal state that caused it: {late_cancellation}"
    );
    let unchanged = fixture.payload(
        "attempt status after a refused cancellation",
        "/application/work/attempt-status",
        &status_request,
    );
    assert_eq!(
        unchanged["terminal"], settled["terminal"],
        "a refused cancellation must leave the receipt byte-identical: {unchanged}"
    );

    // Cancellation *before* the effect commit point. A second attempt runs a
    // provider that will not finish on its own; cancelling it must drive the
    // ladder and seal a cancelled receipt, not a fabricated success.
    let slow_start = start_attempt_body(
        &fixture.project,
        CANCELLED_ATTEMPT_ID,
        execution_snapshot(&pinned, SLOW_EXECUTABLE_ID, &slow_digest, run_deadline),
        now_micros(),
    );
    fixture.payload(
        "start the cancellable attempt",
        "/application/work/start-attempt",
        &slow_start,
    );
    let cancel_status = json!({
        "task_id": TASK_ID,
        "run_id": RUN_ID,
        "attempt_id": CANCELLED_ATTEMPT_ID,
    });
    // Only a *running* attempt is cancellable. Process creation precedes the
    // durable Running transition, but the child may not have executed its
    // first instruction when that transition becomes observable, so require
    // both sides of the asynchronous spawn boundary before cancelling it.
    poll_until("the cancellable provider process", || {
        let attempt = fixture.payload(
            "cancellable attempt status",
            "/application/work/attempt-status",
            &cancel_status,
        );
        if attempt["state"] == "running" && started_marker.exists() {
            Ok(())
        } else {
            Err(format!("{attempt}"))
        }
    });
    let requested = fixture.payload(
        "cancel a running attempt",
        "/application/work/cancel-attempt",
        &json!({
            "task_id": TASK_ID,
            "run_id": RUN_ID,
            "attempt_id": CANCELLED_ATTEMPT_ID,
            "request_id": "cancellation.work-loop-journey.early",
            "occurred_at": now_micros(),
        }),
    );
    assert!(
        requested["cancellation"]["state"] != Value::String("none".to_owned()),
        "a cancellation request must be durable before the process reacts: {requested}"
    );
    let cancelled = poll_until("the cancelled terminal receipt", || {
        let attempt = fixture.payload(
            "cancelled attempt status",
            "/application/work/attempt-status",
            &cancel_status,
        );
        if attempt["state"] == "cancelled" {
            Ok(attempt)
        } else {
            Err(format!("{attempt}"))
        }
    });
    assert_eq!(
        cancelled["terminal"]["outcome"], "cancelled",
        "a cancelled attempt seals a cancelled receipt, never a success: {cancelled}"
    );

    // A completed *runtime* is not an accepted *task*. The product graph read
    // does not own a verified executor-topology join to the attempt rows, so it
    // reports that runtime projection as unavailable instead of joining by a
    // matching-looking identity. The exact attempt authority above remains the
    // terminal-evidence source.
    let with_evidence = product_graph(&fixture, "product graph after terminal runtime evidence");
    assert_eq!(
        with_evidence["snapshot"]["runtime"]["coverage"],
        json!({ "coverage": "unavailable" }),
        "an unjoinable runtime projection must be named unavailable: {with_evidence}"
    );
    assert_eq!(
        with_evidence["snapshot"]["runtime"]["attempts"],
        json!([]),
        "the product graph must not fabricate attempt hydration: {with_evidence}"
    );
    assert_eq!(
        task_item(&with_evidence)["accepted_at"],
        Value::Null,
        "terminal runtime evidence must never accept the task: {with_evidence}"
    );

    // =====================================================================
    // 7. Unavailable runtime coverage blocks an automated recommendation.
    // =====================================================================
    let version_before_replan = graph_version(&with_evidence);
    let replan = fixture.payload(
        "generate a replan proposal",
        "/application/work/generate-proposal",
        &json!({
            "selection": product_selection(&fixture),
            "task_id": TASK_ID,
            "proposal_id": "proposal.work-loop-journey.replan",
            "live_git_evidence": Value::Null,
            "occurred_at": now_micros(),
        }),
    );
    assert_eq!(
        replan["decision"]["disposition"], "indeterminate",
        "unavailable runtime coverage cannot support a product decision: {replan}"
    );
    assert_eq!(
        replan["decision"]["recommended_action"],
        Value::Null,
        "an indeterminate decision must recommend no action: {replan}"
    );
    assert!(
        replan["decision"]["ordered_reason_codes"]
            .as_array()
            .is_some_and(|reasons| reasons
                .iter()
                .any(|reason| reason == "runtime_coverage_unavailable")),
        "the decision must name the unavailable runtime authority: {replan}"
    );
    assert_eq!(
        replan["decision"]["deterministic_fallback"], false,
        "an unavailable authority must not trigger a fallback decision: {replan}"
    );

    // The indeterminate proposal changed nothing. Graph and runtime state are
    // exactly where they were until another explicit command moves them.
    let after_replan_proposal = product_graph(&fixture, "graph after the replan proposal");
    assert_eq!(
        graph_version(&after_replan_proposal),
        version_before_replan,
        "an indeterminate proposal must not be applied: {after_replan_proposal}"
    );
    let attempts_after_replan = fixture.payload(
        "attempt status after the replan proposal",
        "/application/work/attempt-status",
        &status_request,
    );
    assert_eq!(
        attempts_after_replan["terminal"], settled["terminal"],
        "an indeterminate proposal must not disturb runtime receipts: {attempts_after_replan}"
    );

    // Applying it is a two-event, version-checked product mutation: first the
    // relation proposal is explicitly accepted, then that accepted proposal is
    // applied. No direct dependency command survives this authority boundary.
    let relation_proposal = WorkRelationReplanProposalV1::new(
        typed::<ProposalId>("proposal.work-loop-journey.relation-replan"),
        typed::<TaskId>(TASK_ID),
        WorkGraphVersionV1::new(version_before_replan).expect("current graph version"),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    )
    .expect("valid relation replan proposal");
    let prepared_replan_decision = prepare_product_mutation(
        &fixture,
        "prepare relation replan decision",
        json!({
            "change": "decide_relation_replan",
            "proposal": serde_json::to_value(&relation_proposal).expect("relation proposal JSON"),
            "disposition": "accepted",
        }),
    );
    let decided_replan = commit_product_mutation(
        &fixture,
        "accept relation replan",
        &prepared_replan_decision,
    );
    assert_eq!(
        decided_replan["verified_graph_version"]["graph_version"],
        version_before_replan + 1,
        "accepting the replan proposal advances one graph version: {decided_replan}"
    );
    let prepared_replan = prepare_product_mutation(
        &fixture,
        "prepare accepted relation replan",
        json!({
            "change": "apply_relation_replan",
            "proposal_id": "proposal.work-loop-journey.relation-replan",
        }),
    );
    let applied = commit_product_mutation(
        &fixture,
        "apply the accepted relation replan",
        &prepared_replan,
    );
    assert_eq!(
        applied["verified_graph_version"]["graph_version"],
        version_before_replan + 2,
        "applying the accepted replan advances the next graph version: {applied}"
    );

    // Acceptance closes the loop, and only acceptance does.
    let prepared_task_acceptance = prepare_product_mutation(
        &fixture,
        "prepare task acceptance",
        json!({
            "change": "accept_task",
            "task_id": TASK_ID,
            "evidence_by_criterion": {},
        }),
    );
    let accepted_task =
        commit_product_mutation(&fixture, "accept the task", &prepared_task_acceptance);
    let accepted_task_graph = product_graph(&fixture, "graph after task acceptance");
    assert!(
        task_item(&accepted_task_graph)["accepted_at"].is_number(),
        "{accepted_task}"
    );
    let closed = fixture.payload(
        "generate a proposal against the accepted task",
        "/application/work/generate-proposal",
        &json!({
            "selection": product_selection(&fixture),
            "task_id": TASK_ID,
            "proposal_id": "proposal.work-loop-journey.closed",
            "live_git_evidence": Value::Null,
            "occurred_at": now_micros(),
        }),
    );
    assert_eq!(
        closed["decision"]["disposition"], "deny",
        "an accepted task refuses further proposals: {closed}"
    );
    assert_eq!(
        closed["decision"]["recommended_action"],
        Value::Null,
        "a denied proposal recommends nothing: {closed}"
    );
}

/// The journey's task, out of the current product graph payload.
fn task_item(graph: &Value) -> Value {
    graph["snapshot"]["graph"]["items"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["input"]["task_id"] == TASK_ID)
        })
        .cloned()
        .unwrap_or_else(|| panic!("the journey task must be in the product graph: {graph}"))
}
