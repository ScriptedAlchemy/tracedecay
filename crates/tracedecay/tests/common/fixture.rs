//! The shared fixture authority: one isolated home, one profile, and
//! project identities resolved and registered through the production paths.
//!
//! Test setup used to reimplement identity resolution, synthesizing a profile
//! root, re-deriving a store layout, writing an enrollment marker by hand,
//! running a fresh `git init` per fixture, and each reimplementation got some
//! part of it subtly wrong in a different way. The pieces here compose instead,
//! and every one of them delegates to the authority production uses:
//!
//! ```ignore
//! let profile = TestProfile::isolated();                   // isolated home, one profile
//! let repo = GitFixture::primary(profile.path("project")); // template-seeded checkout
//! let project = profile.enroll(repo.root()).await;         // registered + enrolled
//! let data_root = project.data_root();                     // taken from the opened graph
//! ```
//!
//! What each piece owns:
//!
//! * [`TestProfile`] wraps [`super::IsolatedHome`], so the home, profile
//!   data directory, and global DB it hands out always point inside a
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
//! * [`GitFixture`] builds repositories through [`tracedecay_runtime_core::git::try_git_program`]
//!   from a template built once per target directory.
//!
//! Negative identity states stay expressible on purpose through
//! [`TestProfile::unenrolled`] for a checkout this profile never enrolled.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use tracedecay_domain::ProjectId;
use tracedecay_project::project::{TraceDecay, TraceDecayOpenOptions};
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_runtime_core::path_safety::canonical_existing_identity;
use tracedecay_runtime_core::storage::{self, StoreLayout};

use super::IsolatedHome;

// ---------------------------------------------------------------------------
// Profile: one isolated home, one profile, N projects
// ---------------------------------------------------------------------------

struct TestProfileInner {
    env: IsolatedHome,
}

/// One isolated home plus the single profile every project in a fixture is
/// enrolled in.
///
/// Cloning is cheap and shares the isolated home, so a [`RegisteredProject`]
/// keeps the throwaway directory alive for as long as any handle to it exists.
#[derive(Clone)]
pub struct TestProfile {
    inner: Rc<TestProfileInner>,
}

impl TestProfile {
    pub fn isolated() -> Self {
        Self {
            inner: Rc::new(TestProfileInner {
                env: IsolatedHome::new().0,
            }),
        }
    }

    /// This fixture's profile root (`<home>/.tracedecay`).
    pub fn root(&self) -> &Path {
        self.inner.env.profile_root()
    }

    /// This fixture's profile, for APIs that take it explicitly.
    pub fn profile(&self) -> &tracedecay_runtime_core::config::ProfileRoot {
        self.inner.env.profile()
    }

    pub fn home(&self) -> &Path {
        self.inner.env.home()
    }

    /// The throwaway directory holding the isolated home and every checkout,
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
        canonical_existing_identity(&path).unwrap_or_else(|err| {
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
        self.inner.env.open_options()
    }

    /// Registers, enrolls, and initializes `project_root` in this profile.
    pub async fn enroll(&self, project_root: &Path) -> RegisteredProject {
        Box::pin(self.enroll_inner(project_root)).await
    }

    async fn enroll_inner(&self, project_root: &Path) -> RegisteredProject {
        let project_root = canonical_existing_identity(project_root).unwrap_or_else(|err| {
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
            Box::pin(HostAdmissionTestRuntimeV1::project(
                self.root(),
                &project_root,
                project_id,
            ))
            .await
            .unwrap_or_else(|err| {
                panic!(
                    "failed to register fixture project '{}' in profile '{}': {err}",
                    project_root.display(),
                    self.root().display()
                )
            }),
        );

        // Initialize through that same runtime, so a fixture never opens a
        // second database scope on one profile. The public entry point is not
        // an alternative here: outside `test-transport` builds
        // `TraceDecay::init_with_options` takes a *maintenance* database scope
        // on this profile root, the registered runtime above already holds a
        // *daemon* scope on it, and every later exact-scoped call (the
        // `upsert_code_project` below first of all) then fails with "daemon and
        // maintenance database scopes overlap".
        let open_options = self.open_options();
        let graph = Box::pin(
            registry.initialize_project_graph_for_test(&project_root, open_options.clone()),
        )
        .await;
        let graph = graph.unwrap_or_else(|err| {
            panic!(
                "failed to initialize fixture graph '{}': {err}",
                project_root.display()
            )
        });
        // The layout comes from the graph that just created the store. Resolving
        // one independently can name a different shard than the one this project
        // was initialized in, and later seeding then writes into a store no reader
        // opens.
        let layout = graph.store_layout().clone();
        assert_eq!(
            layout.identity.project_id.as_deref(),
            Some(project_id_text.as_str()),
            "fixture graph identity must match the registered project identity"
        );

        // Initializing a graph does not register it, and a selector resolves
        // against the registry of the profile serving the call. The result was
        // previously dropped on the floor, so a fixture whose root the registry
        // refused produced tests that failed far away from the cause.
        Box::pin(registry.upsert_code_project(
            &project_id_text,
            &project_root,
            tracedecay_runtime_core::worktree::git_common_dir(&project_root).as_deref(),
            None,
            tracedecay_runtime_core::branch::current_branch(&project_root).as_deref(),
        ))
        .await
        .unwrap_or_else(|error| {
            panic!(
                "register fixture project '{}' at {}: {error}",
                project_id_text,
                project_root.display()
            )
        });

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
            !storage::has_repository_identity_marker(&root),
            "an unenrolled fixture root must not carry an identity marker: {}",
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
        Box::pin(TraceDecay::open_with_options(
            &self.root,
            self.open_options.clone(),
        ))
        .await
        .unwrap_or_else(|err| {
            panic!(
                "failed to reopen fixture graph '{}': {err}",
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
        Box::pin(TraceDecay::open_with_options(
            &self.root,
            self.open_options.clone(),
        ))
        .await
        .unwrap_or_else(|err| {
            panic!(
                "failed to reopen fixture graph '{}': {err}",
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
    // Keeps the isolated home alive for the negative case too.
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
    ) -> tracedecay_project::test_support::host_admission::ProjectScopedTestRuntimeV1 {
        tracedecay_project::test_support::host_admission::ProjectScopedTestRuntimeV1::new(
            Arc::clone(&self.registry),
        )
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

    /// [`Self::mcp_server`] plus servers for projects other than this one, which
    /// cross-project tools reach only through the retained resolver.
    pub async fn mcp_server_retaining(
        &self,
        retained_servers: Vec<Arc<tracedecay::mcp::McpServer>>,
    ) -> Arc<tracedecay::mcp::McpServer> {
        tracedecay::mcp::McpServer::new_with_retained_test_servers_for_test(
            self.reopen().await,
            None,
            self.project_scoped_runtime(),
            retained_servers,
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

/// Runs `git <args>` in `dir` through the cached [`tracedecay_runtime_core::git::try_git_program`].
///
/// Resolving `git` once per process (rather than letting the OS re-walk `PATH`
/// per spawn) is worth 100-300 ms per call on Windows and makes the lookup
/// deterministic under the parallel load nextest creates.
pub fn git_output(dir: &Path, args: &[&str]) -> Output {
    Command::new(
        tracedecay_runtime_core::git::try_git_program()
            .expect("absolute git executable should resolve"),
    )
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
    /// commit, including a `.gitignore` for `.tracedecay/`, so a fixture that
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
        let origin = canonical_existing_identity(&origin).unwrap_or_else(|err| {
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

    fn assert_collapses_onto_primary(&self, path: &Path) -> PathBuf {
        let path = canonical_existing_identity(path).unwrap_or_else(|err| {
            panic!(
                "failed to canonicalize fixture worktree '{}': {err}",
                path.display()
            )
        });
        let primary = canonical_existing_identity(&self.root).unwrap_or_else(|_| self.root.clone());
        assert_eq!(
            tracedecay_runtime_core::worktree::repository_identity_root(&path),
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
    lock_file.lock().ok()?;

    // Another process may have finished the build while we waited.
    if shared.join("READY").is_file() {
        let _ = lock_file.unlock();
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
    let _ = lock_file.unlock();
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
    let output = Command::new(
        tracedecay_runtime_core::git::try_git_program()
            .expect("absolute git executable should resolve"),
    )
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

/// The one report the TypeScript fixture's compiler prints, byte-for-byte the
/// `tsc --noEmit --pretty false` shape for a real `TS4023` on `src/index.ts`
/// line 3 column 14, where `value` is declared.
pub const TYPESCRIPT_FIXTURE_TSC_REPORT: &str = "src/index.ts(3,14): error TS4023: Exported variable 'value' has or is using name 'Hidden' from external module \"./src/dep\" but cannot be named.\n";

/// Where the fixture compiler records each invocation: one line per run with
/// the working directory and the arguments it received.
pub const TYPESCRIPT_FIXTURE_TSC_INVOCATIONS: &str = "node_modules/tsc-invocations.log";

/// Whether the TypeScript fixture project carries its own compiler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TypeScriptFixtureCompiler {
    /// `node_modules/.bin/tsc` exists and reports [`TYPESCRIPT_FIXTURE_TSC_REPORT`].
    Present,
    /// No `node_modules` at all: a checkout before `npm install`.
    Missing,
}

/// A small TypeScript project whose sources genuinely produce `TS4023` under
/// `declaration: true`: `index.ts` exports a value typed by an interface
/// `dep.ts` does not export. With [`TypeScriptFixtureCompiler::Present`] the
/// project's own `node_modules/.bin/tsc` reports that finding, so the daemon's
/// producer must run exactly that binary from the project root.
#[cfg(unix)]
pub fn write_typescript_diagnostics_fixture(project: &Path, compiler: TypeScriptFixtureCompiler) {
    use std::os::unix::fs::PermissionsExt;

    fs::create_dir_all(project.join("src")).unwrap();
    fs::write(
        project.join("package.json"),
        "{\n  \"name\": \"diagnostics-fixture\",\n  \"private\": true\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("tsconfig.json"),
        "{\n  \"compilerOptions\": {\n    \"strict\": true,\n    \"declaration\": true,\n    \"module\": \"es2022\",\n    \"target\": \"es2022\"\n  },\n  \"include\": [\"src\"]\n}\n",
    )
    .unwrap();
    fs::write(project.join(".gitignore"), "node_modules/\n").unwrap();
    fs::write(
        project.join("src/dep.ts"),
        "interface Hidden {\n  a: number;\n}\n\nexport function make(): Hidden {\n  return { a: 1 };\n}\n",
    )
    .unwrap();
    fs::write(
        project.join("src/index.ts"),
        "import { make } from \"./dep\";\n\nexport const value = make();\n",
    )
    .unwrap();
    if compiler == TypeScriptFixtureCompiler::Missing {
        return;
    }
    let bin = project.join("node_modules/.bin");
    fs::create_dir_all(&bin).unwrap();
    let tsc = bin.join("tsc");
    fs::write(
        &tsc,
        format!(
            "#!/bin/sh\nprintf '%s %s\\n' \"$(pwd)\" \"$*\" >> \"{}\"\ncat <<'TSC_REPORT'\n{}TSC_REPORT\nexit 2\n",
            project.join(TYPESCRIPT_FIXTURE_TSC_INVOCATIONS).display(),
            TYPESCRIPT_FIXTURE_TSC_REPORT
        ),
    )
    .unwrap();
    fs::set_permissions(&tsc, fs::Permissions::from_mode(0o755)).unwrap();
}

/// The package file the monorepo fixture's `TS4023` is reported on.
pub const TYPESCRIPT_MONOREPO_APP_FILE: &str = "packages/app/src/index.ts";

/// A TypeScript file in the monorepo fixture that no tsconfig owns.
pub const TYPESCRIPT_MONOREPO_UNOWNED_FILE: &str = "scripts/release.ts";

/// The issue #2025 layout: a pnpm workspace whose packages each carry a
/// `tsconfig.json` extending a root `tsconfig.base.json`, with no root
/// `tsconfig.json`. `packages/app` has the same genuine `TS4023` sources as
/// [`write_typescript_diagnostics_fixture`]; `packages/lib` is clean.
///
/// With [`TypeScriptFixtureCompiler::Present`] the workspace root's
/// `node_modules/.bin/tsc` (pnpm hoists the binary of a root dev dependency)
/// logs every invocation to [`TYPESCRIPT_FIXTURE_TSC_INVOCATIONS`] and reports
/// the finding only when pointed at `packages/app/tsconfig.json`, so a record
/// on [`TYPESCRIPT_MONOREPO_APP_FILE`] proves the producer checked that
/// package's own tsconfig.
#[cfg(unix)]
pub fn write_typescript_monorepo_diagnostics_fixture(
    project: &Path,
    compiler: TypeScriptFixtureCompiler,
) {
    use std::os::unix::fs::PermissionsExt;

    let files = [
        (
            "package.json",
            "{\n  \"name\": \"monorepo-fixture\",\n  \"private\": true,\n  \"devDependencies\": { \"typescript\": \"5.6.3\" }\n}\n",
        ),
        ("pnpm-workspace.yaml", "packages:\n  - 'packages/*'\n"),
        ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
        (".gitignore", "node_modules/\n"),
        (
            "tsconfig.base.json",
            "{\n  // shared by every package\n  \"compilerOptions\": {\n    \"strict\": true,\n    \"declaration\": true,\n    \"module\": \"es2022\",\n    \"target\": \"es2022\",\n  },\n}\n",
        ),
        (
            "packages/app/package.json",
            "{ \"name\": \"@fixture/app\", \"private\": true }\n",
        ),
        (
            "packages/app/tsconfig.json",
            "{ \"extends\": \"../../tsconfig.base.json\", \"include\": [\"src\"] }\n",
        ),
        (
            "packages/app/src/dep.ts",
            "interface Hidden {\n  a: number;\n}\n\nexport function make(): Hidden {\n  return { a: 1 };\n}\n",
        ),
        (
            TYPESCRIPT_MONOREPO_APP_FILE,
            "import { make } from \"./dep\";\n\nexport const value = make();\n",
        ),
        (
            "packages/lib/package.json",
            "{ \"name\": \"@fixture/lib\", \"private\": true }\n",
        ),
        (
            "packages/lib/tsconfig.json",
            "{ \"extends\": \"../../tsconfig.base.json\", \"include\": [\"src\"] }\n",
        ),
        (
            "packages/lib/src/lib.ts",
            "export function add(a: number, b: number): number {\n  return a + b;\n}\n",
        ),
        (
            TYPESCRIPT_MONOREPO_UNOWNED_FILE,
            "export const release = \"v1\";\n",
        ),
    ];
    for (path, contents) in files {
        let path = project.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, contents).unwrap();
    }
    if compiler == TypeScriptFixtureCompiler::Missing {
        return;
    }
    let bin = project.join("node_modules/.bin");
    fs::create_dir_all(&bin).unwrap();
    let tsc = bin.join("tsc");
    fs::write(
        &tsc,
        format!(
            "#!/bin/sh\nprintf '%s %s\\n' \"$(pwd)\" \"$*\" >> \"{}\"\ncase \"$2\" in\n  */packages/app/tsconfig.json)\n    echo \"{TYPESCRIPT_MONOREPO_APP_FILE}(3,14): error TS4023: Exported variable 'value' has or is using name 'Hidden' from external module \\\"./dep\\\" but cannot be named.\"\n    exit 2 ;;\nesac\nexit 0\n",
            project.join(TYPESCRIPT_FIXTURE_TSC_INVOCATIONS).display(),
        ),
    )
    .unwrap();
    fs::set_permissions(&tsc, fs::Permissions::from_mode(0o755)).unwrap();
}
