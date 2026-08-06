//! The shared fixture authority: one isolated environment, one profile, and
//! project identities resolved and registered through the production paths.
//!
//! Test setup used to reimplement identity resolution — synthesizing a profile
//! root, re-deriving a store layout, writing an enrollment marker by hand,
//! running a fresh `git init` per fixture — and each reimplementation got some
//! part of it subtly wrong in a different way. The pieces here compose instead,
//! and every one of them delegates to the authority production uses:
//!
//! ```ignore
//! let profile = TestProfile::acquire().await;              // isolated HOME, one profile
//! let repo = GitFixture::primary(profile.path("project")); // template-seeded checkout
//! let project = profile.enroll_indexed(repo.root()).await; // registered + enrolled
//! let data_root = project.data_root();                     // taken from the opened graph
//! ```
//!
//! What each piece owns:
//!
//! * [`TestProfile`] wraps [`super::IsolatedEnv`], so `HOME`,
//!   `TRACEDECAY_DATA_DIR`, and the global-DB override always point inside a
//!   throwaway directory. It is also the *only* source of
//!   [`TraceDecayOpenOptions`] in a fixture, which is what keeps N projects in
//!   ONE profile: an open with default options synthesizes a per-project
//!   standalone test profile, and a project store is keyed by
//!   (profile, project), so two default opens can never see each other.
//! * [`TestProfile::enroll`] resolves the project id with
//!   [`storage::default_profile_project_id`] and registers it through
//!   [`HostAdmissionTestRuntimeV1::project`], which writes the enrollment
//!   marker with production's atomic writer and mounts this profile's registry
//!   database plus the project-session authority.
//! * [`RegisteredProject`] carries its own open options, so reopening a graph
//!   or a branch cannot drift onto another profile, and it snapshots the store
//!   layout *from the graph that created it* rather than resolving one again.
//! * [`GitFixture`] builds repositories through [`tracedecay::git::git_program`]
//!   from a template built once per target directory.
//!
//! Negative identity states stay expressible on purpose through
//! [`TestProfile::unenrolled`] for a checkout this profile never enrolled.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::{Arc, OnceLock};

use tracedecay::application::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay::storage::{self, StoreLayout};
use tracedecay::tracedecay::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_domain::ProjectId;

use super::IsolatedEnv;

// ---------------------------------------------------------------------------
// Profile: one isolated environment, one profile, N projects
// ---------------------------------------------------------------------------

struct TestProfileInner {
    env: IsolatedEnv,
    root: PathBuf,
    global_db_path: PathBuf,
}

/// One isolated environment plus the single profile every project in a fixture
/// is enrolled in.
///
/// Cloning is cheap and shares the isolated environment, so a
/// [`RegisteredProject`] keeps the throwaway `HOME` and the process-wide env
/// lock alive for as long as any handle to it exists. A fixture therefore
/// cannot drop its environment guard while still using a store.
#[derive(Clone)]
pub struct TestProfile {
    inner: Arc<TestProfileInner>,
}

impl TestProfile {
    /// Acquires the isolated environment and pins this fixture's profile.
    pub async fn acquire() -> Self {
        Self::build(IsolatedEnv::acquire().await.0)
    }

    /// Sync counterpart of [`Self::acquire`] for plain `#[test]` fns.
    ///
    /// Panics if called from within an async context; use [`Self::acquire`]
    /// there.
    pub fn acquire_blocking() -> Self {
        Self::build(IsolatedEnv::acquire_blocking().0)
    }

    fn build(env: IsolatedEnv) -> Self {
        let root = env.home().join(".tracedecay");
        fs::create_dir_all(&root).unwrap_or_else(|err| {
            panic!(
                "failed to create fixture profile '{}': {err}",
                root.display()
            )
        });
        let global_db_path = root.join("global.db");
        Self {
            inner: Arc::new(TestProfileInner {
                env,
                root,
                global_db_path,
            }),
        }
    }

    /// This fixture's profile root (`<home>/.tracedecay`).
    pub fn root(&self) -> &Path {
        &self.inner.root
    }

    pub fn home(&self) -> &Path {
        self.inner.env.home()
    }

    /// The throwaway directory holding the isolated `HOME` and every checkout,
    /// for fixtures that need siblings (a bare `origin`, a linked worktree).
    pub fn scratch(&self) -> &Path {
        self.inner.env.scratch()
    }

    /// Creates `<scratch>/<name>` and returns its canonical path.
    ///
    /// Canonical because identity resolution compares real paths, so a fixture
    /// that keeps a non-canonical root can disagree with the store it opened.
    pub fn path(&self, name: impl AsRef<Path>) -> PathBuf {
        let path = self.scratch().join(name);
        fs::create_dir_all(&path).unwrap_or_else(|err| {
            panic!(
                "failed to create fixture directory '{}': {err}",
                path.display()
            )
        });
        path.canonicalize().unwrap_or_else(|err| {
            panic!(
                "failed to canonicalize fixture directory '{}': {err}",
                path.display()
            )
        })
    }

    /// The open options for this profile.
    ///
    /// Every graph in a fixture must be opened with these. Default options let
    /// test builds synthesize a per-project standalone profile, which silently
    /// puts two projects of one fixture in two profiles.
    pub fn open_options(&self) -> TraceDecayOpenOptions {
        TraceDecayOpenOptions {
            profile_root: Some(self.inner.root.clone()),
            global_db_path: Some(self.inner.global_db_path.clone()),
        }
    }

    /// Registers, enrolls, and initializes `project_root` in this profile.
    pub async fn enroll(&self, project_root: &Path) -> RegisteredProject {
        self.enroll_inner(project_root, false).await
    }

    /// [`Self::enroll`] followed by a full index.
    pub async fn enroll_indexed(&self, project_root: &Path) -> RegisteredProject {
        self.enroll_inner(project_root, true).await
    }

    async fn enroll_inner(&self, project_root: &Path, index: bool) -> RegisteredProject {
        let project_root = project_root.canonicalize().unwrap_or_else(|err| {
            panic!(
                "fixture project root '{}' must exist to be enrolled: {err}",
                project_root.display()
            )
        });

        // Production's identity function, so a linked worktree collapses onto
        // its primary checkout exactly the way every reader resolves it.
        let project_id_text = storage::default_profile_project_id(&project_root);
        let project_id = ProjectId::new(project_id_text.clone()).unwrap_or_else(|err| {
            panic!("fixture project identity '{project_id_text}' is invalid: {err}")
        });

        // Writes the enrollment marker through production's atomic writer and
        // mounts this profile's registry database and project-session
        // authority. Both are what a bare direct context lacks.
        let registry = Arc::new(
            HostAdmissionTestRuntimeV1::project(self.root(), &project_root, project_id)
                .await
                .unwrap_or_else(|err| {
                    panic!(
                        "failed to register fixture project '{}' in profile '{}': {err}",
                        project_root.display(),
                        self.root().display()
                    )
                }),
        );

        // Initialize through that same runtime wherever the registered seam
        // exists, so a fixture never opens a second database scope on one
        // profile. The public entry point is the fallback for builds without
        // the seam; it resolves the same identity from the marker just written.
        let open_options = self.open_options();
        #[cfg(feature = "test-transport")]
        let graph = registry
            .initialize_project_graph_for_test(&project_root, open_options.clone())
            .await;
        #[cfg(not(feature = "test-transport"))]
        let graph = TraceDecay::init_with_options(&project_root, open_options.clone()).await;
        let graph = graph.unwrap_or_else(|err| {
            panic!(
                "failed to initialize fixture graph '{}': {err}",
                project_root.display()
            )
        });
        if index {
            graph.index_all().await.unwrap_or_else(|err| {
                panic!(
                    "failed to index fixture graph '{}': {err}",
                    project_root.display()
                )
            });
        }

        // The layout comes from the graph that just created the store. Resolving
        // one independently can name a different shard than the one this project
        // was indexed into, and later seeding then writes into a store no reader
        // opens.
        let layout = graph.store_layout().clone();
        assert_eq!(
            layout.identity.project_id.as_deref(),
            Some(project_id_text.as_str()),
            "fixture graph identity must match the registered project identity"
        );

        // Initializing a graph does not register it, and a selector resolves
        // against the registry of the profile serving the call.
        registry
            .upsert_code_project(
                &project_id_text,
                &project_root,
                tracedecay::worktree::git_common_dir(&project_root).as_deref(),
                None,
                tracedecay::branch::current_branch(&project_root).as_deref(),
            )
            .await;

        RegisteredProject {
            profile: self.clone(),
            root: project_root,
            project_id: project_id_text,
            layout,
            open_options,
            graph: Arc::new(graph),
            registry,
        }
    }

    /// A checkout this profile deliberately never enrolled.
    ///
    /// For tests whose scenario *is* the missing enrollment: no marker is
    /// written and nothing is registered, so the code under test still has to
    /// reach its own "not enrolled" conclusion.
    pub fn unenrolled(&self, name: impl AsRef<Path>) -> UnenrolledProject {
        let root = self.path(name);
        assert!(
            storage::read_enrollment_marker(&root)
                .unwrap_or_else(|err| panic!("failed to read fixture enrollment marker: {err}"))
                .is_none(),
            "an unenrolled fixture root must not carry an enrollment marker: {}",
            root.display()
        );
        UnenrolledProject {
            _profile: self.clone(),
            root,
        }
    }
}

// ---------------------------------------------------------------------------
// Registered project
// ---------------------------------------------------------------------------

/// A project registered and enrolled in exactly one [`TestProfile`], with its
/// graph open and its store layout taken from that graph.
pub struct RegisteredProject {
    profile: TestProfile,
    root: PathBuf,
    project_id: String,
    layout: StoreLayout,
    open_options: TraceDecayOpenOptions,
    graph: Arc<TraceDecay>,
    registry: Arc<HostAdmissionTestRuntimeV1>,
}

impl RegisteredProject {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn profile(&self) -> &TestProfile {
        &self.profile
    }

    /// The retained graph, shareable with runtimes that take ownership.
    pub fn graph(&self) -> &Arc<TraceDecay> {
        &self.graph
    }

    /// The layout of the store this project's graph actually wrote.
    pub fn store_layout(&self) -> &StoreLayout {
        &self.layout
    }

    pub fn data_root(&self) -> &Path {
        &self.layout.data_root
    }

    pub fn graph_db_path(&self) -> &Path {
        &self.layout.graph_db_path
    }

    /// The registered runtime backing this project: this profile's registry
    /// database plus its project-session authority.
    pub fn registry(&self) -> &Arc<HostAdmissionTestRuntimeV1> {
        &self.registry
    }

    /// The open options this project's graph was created with.
    ///
    /// Every reopen, branch open, and `*_with_options` call against this
    /// checkout must use these so the fixture cannot drift onto a second
    /// synthesized standalone profile.
    pub fn open_options(&self) -> TraceDecayOpenOptions {
        self.open_options.clone()
    }

    /// Reopens the project graph in the same profile.
    pub async fn reopen(&self) -> TraceDecay {
        TraceDecay::open_with_options(&self.root, self.open_options.clone())
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "failed to reopen fixture graph '{}': {err}",
                    self.root.display()
                )
            })
    }

    /// Opens one tracked branch's graph in the same profile.
    pub async fn open_branch(&self, branch_name: &str) -> TraceDecay {
        TraceDecay::open_branch_with_options(&self.root, branch_name, self.open_options.clone())
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "failed to open fixture branch '{branch_name}' of '{}': {err}",
                    self.root.display()
                )
            })
    }

    /// Checkpoints and closes the retained graph while keeping profile
    /// isolation, enrollment, and the store layout for a later reopen.
    ///
    /// Branch-drift and recovery fixtures need an exclusive close before they
    /// reopen and pin a serving branch; consuming `self` is deliberate so the
    /// closed handle cannot keep serving through a graph that is gone.
    pub async fn close(self) -> ClosedRegisteredProject {
        let Self {
            profile,
            root,
            project_id,
            layout,
            open_options,
            graph,
            registry,
        } = self;
        match Arc::try_unwrap(graph) {
            Ok(graph) => {
                graph.checkpoint().await.unwrap_or_else(|err| {
                    panic!(
                        "failed to checkpoint fixture graph '{}': {err}",
                        root.display()
                    )
                });
                graph.close();
            }
            Err(_) => panic!(
                "fixture graph for '{}' still has external Arc clones; drop those before close",
                root.display()
            ),
        }
        ClosedRegisteredProject {
            profile,
            root,
            project_id,
            layout,
            open_options,
            _registry: registry,
        }
    }
}

/// An enrolled project whose retained graph has been closed.
///
/// Keeps the isolated profile and the layout snapshot so a later
/// [`Self::reopen`] cannot resolve a different shard than the one that was
/// indexed.
pub struct ClosedRegisteredProject {
    profile: TestProfile,
    root: PathBuf,
    project_id: String,
    layout: StoreLayout,
    open_options: TraceDecayOpenOptions,
    _registry: Arc<HostAdmissionTestRuntimeV1>,
}

impl ClosedRegisteredProject {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn project_id(&self) -> &str {
        &self.project_id
    }

    pub fn profile(&self) -> &TestProfile {
        &self.profile
    }

    pub fn store_layout(&self) -> &StoreLayout {
        &self.layout
    }

    pub fn data_root(&self) -> &Path {
        &self.layout.data_root
    }

    pub fn graph_db_path(&self) -> &Path {
        &self.layout.graph_db_path
    }

    pub fn open_options(&self) -> TraceDecayOpenOptions {
        self.open_options.clone()
    }

    /// Reopens the project graph in the same profile.
    pub async fn reopen(&self) -> TraceDecay {
        TraceDecay::open_with_options(&self.root, self.open_options.clone())
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "failed to reopen fixture graph '{}': {err}",
                    self.root.display()
                )
            })
    }

    /// Opens one tracked branch's graph in the same profile.
    pub async fn open_branch(&self, branch_name: &str) -> TraceDecay {
        TraceDecay::open_branch_with_options(&self.root, branch_name, self.open_options.clone())
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "failed to open fixture branch '{branch_name}' of '{}': {err}",
                    self.root.display()
                )
            })
    }
}

impl std::ops::Deref for RegisteredProject {
    type Target = TraceDecay;

    fn deref(&self) -> &Self::Target {
        &self.graph
    }
}

/// A checkout inside a fixture profile that was never enrolled or registered.
pub struct UnenrolledProject {
    // Keeps the isolated environment alive for the negative case too.
    _profile: TestProfile,
    root: PathBuf,
}

impl UnenrolledProject {
    pub fn root(&self) -> &Path {
        &self.root
    }
}

// ---------------------------------------------------------------------------
// Server / runtime composition
// ---------------------------------------------------------------------------

#[cfg(feature = "test-transport")]
impl RegisteredProject {
    /// This project's runtime, promoted to the project scope that project-graph
    /// and project-session seams require.
    pub fn project_scoped_runtime(
        &self,
    ) -> tracedecay::application::host_admission::ProjectScopedTestRuntimeV1 {
        tracedecay::application::host_admission::ProjectScopedTestRuntimeV1::new(Arc::clone(
            &self.registry,
        ))
        .unwrap_or_else(|err| panic!("fixture project runtime must be project-scoped: {err}"))
    }

    /// An MCP server for this project with its registry database, retained
    /// project-graph resolver, and host-admission spool already mounted.
    ///
    /// A server built from a bare direct context has none of those, so every
    /// hook notification fails closed before reaching the code under test.
    pub async fn mcp_server(&self) -> Arc<tracedecay::mcp::McpServer> {
        self.mcp_server_retaining(Vec::new()).await
    }

    /// [`Self::mcp_server`] plus graphs for projects other than this one, which
    /// cross-project tools reach only through the retained resolver.
    pub async fn mcp_server_retaining(
        &self,
        retained_graphs: Vec<Arc<TraceDecay>>,
    ) -> Arc<tracedecay::mcp::McpServer> {
        tracedecay::mcp::McpServer::new_with_retained_test_graphs_for_test(
            self.reopen().await,
            None,
            self.project_scoped_runtime(),
            retained_graphs,
        )
        .await
        .expect("registered test server")
    }
}

// ---------------------------------------------------------------------------
// Git identity
// ---------------------------------------------------------------------------

/// Config every fixture git invocation carries, so the operator's global git
/// configuration (hooks, gc, identity, commit signing) cannot reach a fixture.
const GIT_FIXTURE_CONFIG: [&str; 10] = [
    "-c",
    "core.hooksPath=.git/no-hooks",
    "-c",
    "gc.auto=0",
    "-c",
    "user.name=TraceDecay Test",
    "-c",
    "user.email=tracedecay-test@example.com",
    "-c",
    "commit.gpgsign=false",
];

/// Bump when the template layout changes, so templates left by an earlier
/// revision in a cached target dir are ignored.
const GIT_TEMPLATE_DIR_NAME: &str = "fixture-git-template-v1";
const PRIMARY_TEMPLATE: &str = "primary";
const ORIGIN_TEMPLATE: &str = "origin.git";

static GIT_TEMPLATE_ROOT: OnceLock<Option<PathBuf>> = OnceLock::new();

/// Runs `git <args>` in `dir` through the cached [`tracedecay::git::git_program`].
///
/// Resolving `git` once per process (rather than letting the OS re-walk `PATH`
/// per spawn) is worth 100-300 ms per call on Windows and makes the lookup
/// deterministic under the parallel load nextest creates.
pub fn git_output(dir: &Path, args: &[&str]) -> Output {
    Command::new(tracedecay::git::git_program())
        .args(GIT_FIXTURE_CONFIG)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap_or_else(|err| panic!("failed to run git {args:?} in '{}': {err}", dir.display()))
}

/// [`git_output`], asserting a zero exit status.
pub fn git_run(dir: &Path, args: &[&str]) {
    let output = git_output(dir, args);
    assert!(
        output.status.success(),
        "git {args:?} in '{}' failed\nstdout:\n{}\nstderr:\n{}",
        dir.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

/// [`git_run`] returning the trimmed stdout.
pub fn git_capture(dir: &Path, args: &[&str]) -> String {
    let output = git_output(dir, args);
    assert!(
        output.status.success(),
        "git {args:?} in '{}' failed\nstderr:\n{}",
        dir.display(),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .unwrap_or_else(|err| panic!("git {args:?} produced non-UTF-8 output: {err}"))
        .trim()
        .to_owned()
}

/// A git checkout built for a fixture.
pub struct GitFixture {
    root: PathBuf,
}

impl GitFixture {
    /// A primary checkout on `main` with one commit, seeded from a template
    /// built once per target directory.
    ///
    /// The template carries the `git init`, the branch rename, and the initial
    /// commit — including a `.gitignore` for `.tracedecay/`, so a fixture that
    /// stages its working tree can never commit enrollment state. Falls back to
    /// building in place when the template is unavailable, so a template
    /// failure can never change what a test exercises.
    pub fn primary(root: impl Into<PathBuf>) -> Self {
        let root = root.into();
        fs::create_dir_all(&root).unwrap_or_else(|err| {
            panic!(
                "failed to create fixture repository '{}': {err}",
                root.display()
            )
        });
        if let Some(template) = git_template_root()
            && copy_tree(&template.join(PRIMARY_TEMPLATE), &root).is_ok()
            && root.join(".git").is_dir()
        {
            return Self { root };
        }
        let fixture = Self { root };
        fixture.initialize_in_place();
        fixture
    }

    fn initialize_in_place(&self) {
        git_run(&self.root, &["init", "-b", "main"]);
        write_gitignore(&self.root);
        self.commit_all("initial commit");
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn run(&self, args: &[&str]) {
        git_run(&self.root, args);
    }

    pub fn output(&self, args: &[&str]) -> Output {
        git_output(&self.root, args)
    }

    pub fn capture(&self, args: &[&str]) -> String {
        git_capture(&self.root, args)
    }

    /// Stages and commits the working tree.
    ///
    /// Enrollment state is excluded by the `.gitignore` every fixture repository
    /// is seeded with, not by a pathspec here: `git add` refuses a pathspec that
    /// names an ignored path, so the two mechanisms cannot both be used. A
    /// fixture that commits its own enrollment marker changes what later
    /// checkouts of that commit resolve to, which is why
    /// `committing_a_fixture_tree_never_stages_enrollment_state` pins it.
    pub fn commit_all(&self, message: &str) {
        self.run(&["add", "--all"]);
        self.run(&["commit", "-m", message]);
    }

    pub fn head_sha(&self) -> String {
        self.capture(&["rev-parse", "HEAD"])
    }

    /// Adds a sibling bare `origin` and pushes `main` to it, returning the
    /// canonical origin path.
    pub fn with_bare_origin(&self) -> PathBuf {
        let origin = self
            .root
            .parent()
            .unwrap_or(&self.root)
            .join(ORIGIN_TEMPLATE);
        let seeded = git_template_root()
            .is_some_and(|template| copy_tree(&template.join(ORIGIN_TEMPLATE), &origin).is_ok());
        if !seeded {
            self.run(&["init", "--bare", &origin.to_string_lossy()]);
        }
        let origin = origin.canonicalize().unwrap_or_else(|err| {
            panic!(
                "failed to canonicalize fixture origin '{}': {err}",
                origin.display()
            )
        });
        self.run(&["remote", "add", "origin", &origin.to_string_lossy()]);
        self.run(&["push", "origin", "main"]);
        origin
    }

    /// Adds a linked worktree checked out on a new `branch`.
    ///
    /// Asserts the identity production expects: every linked worktree of a
    /// repository shares one git common directory, so it collapses onto this
    /// primary checkout and is the same project.
    pub fn linked_worktree(&self, path: &Path, branch: &str) -> PathBuf {
        self.run(&[
            "worktree",
            "add",
            "-b",
            branch,
            &path.to_string_lossy(),
            "main",
        ]);
        self.assert_collapses_onto_primary(path)
    }

    /// Adds a linked worktree with a detached `HEAD`.
    pub fn detached_worktree(&self, path: &Path) -> PathBuf {
        self.run(&[
            "worktree",
            "add",
            "--detach",
            &path.to_string_lossy(),
            "main",
        ]);
        self.assert_collapses_onto_primary(path)
    }

    fn assert_collapses_onto_primary(&self, path: &Path) -> PathBuf {
        let path = path.canonicalize().unwrap_or_else(|err| {
            panic!(
                "failed to canonicalize fixture worktree '{}': {err}",
                path.display()
            )
        });
        let primary = self
            .root
            .canonicalize()
            .unwrap_or_else(|_| self.root.clone());
        assert_eq!(
            tracedecay::worktree::repository_identity_root(&path),
            Some(primary.clone()),
            "a linked worktree must collapse onto the primary checkout '{}'",
            primary.display()
        );
        path
    }
}

fn write_gitignore(root: &Path) {
    fs::write(root.join(".gitignore"), ".tracedecay/\n").unwrap_or_else(|err| {
        panic!(
            "failed to write fixture .gitignore in '{}': {err}",
            root.display()
        )
    });
}

/// Returns the shared git template directory, building it if this is the first
/// process to need it.
///
/// nextest runs one process per test, so a per-process cache cannot amortize
/// the four `git` subprocesses every repository fixture would otherwise spawn.
/// An exclusive file lock serializes the build machine-wide: exactly one
/// process builds, concurrent processes block briefly and then find `READY`.
fn git_template_root() -> Option<&'static Path> {
    GIT_TEMPLATE_ROOT
        .get_or_init(ensure_git_template)
        .as_deref()
}

fn ensure_git_template() -> Option<PathBuf> {
    let tmp_root = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let shared = tmp_root.join(GIT_TEMPLATE_DIR_NAME);
    if shared.join("READY").is_file() {
        return Some(shared);
    }

    fs::create_dir_all(tmp_root).ok()?;
    let lock_path = tmp_root.join(format!("{GIT_TEMPLATE_DIR_NAME}.lock"));
    let lock_file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&lock_path)
        .ok()?;
    fs2::FileExt::lock_exclusive(&lock_file).ok()?;

    // Another process may have finished the build while we waited.
    if shared.join("READY").is_file() {
        let _ = fs2::FileExt::unlock(&lock_file);
        return Some(shared);
    }

    let build = shared.with_file_name(format!(
        "{GIT_TEMPLATE_DIR_NAME}-build-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&build);
    let result = match build_git_template(&build) {
        Ok(()) => match fs::rename(&build, &shared) {
            Ok(()) => Some(shared),
            Err(_) if shared.join("READY").is_file() => {
                let _ = fs::remove_dir_all(&build);
                Some(shared)
            }
            // The private build tree is still a valid template for this process.
            Err(_) => Some(build),
        },
        Err(err) => {
            eprintln!("[common::fixture] git template build failed, falling back: {err}");
            let _ = fs::remove_dir_all(&build);
            None
        }
    };
    let _ = fs2::FileExt::unlock(&lock_file);
    result
}

fn build_git_template(dest: &Path) -> io::Result<()> {
    let primary = dest.join(PRIMARY_TEMPLATE);
    fs::create_dir_all(&primary)?;
    run_template_git(&primary, &["init", "-b", "main"])?;
    fs::write(primary.join(".gitignore"), ".tracedecay/\n")?;
    run_template_git(&primary, &["add", "--all"])?;
    run_template_git(&primary, &["commit", "-m", "initial commit"])?;

    let origin = dest.join(ORIGIN_TEMPLATE);
    fs::create_dir_all(&origin)?;
    run_template_git(&origin, &["init", "--bare"])?;

    fs::write(dest.join("READY"), b"ok")?;
    Ok(())
}

/// Template-build git runs report failures as `io::Error` rather than panicking,
/// so an unusable git only disables the template instead of failing every test.
fn run_template_git(dir: &Path, args: &[&str]) -> io::Result<()> {
    let output = Command::new(tracedecay::git::git_program())
        .args(GIT_FIXTURE_CONFIG)
        .args(args)
        .current_dir(dir)
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(io::Error::other(format!(
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

/// Recursively copies `src` into `dest`, failing if `dest` already holds a
/// repository so a seeded checkout never lands on top of another one.
fn copy_tree(src: &Path, dest: &Path) -> io::Result<()> {
    if dest.join(".git").exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} already holds a git repository", dest.display()),
        ));
    }
    copy_tree_contents(src, dest)
}

fn copy_tree_contents(src: &Path, dest: &Path) -> io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree_contents(&from, &to)?;
        } else {
            fs::copy(&from, &to)?;
        }
    }
    Ok(())
}
