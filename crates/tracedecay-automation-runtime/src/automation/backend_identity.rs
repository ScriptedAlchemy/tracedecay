//! The durable identity a run's backend and configuration executed under.
//!
//! A deterministic backend failure — a typed permanent protocol or
//! configuration fault, as opposed to transient I/O — reproduces exactly as
//! long as nothing about the backend or the configuration changes. Retrying
//! it is not recovery: it burns a scheduler tick, spawns the provider again,
//! and settles on the same terminal state. Such a failure must settle **once**
//! and stay suppressed until the identity it failed under changes.
//!
//! That requires a real identity to key on, not a timer and not an attempt
//! counter. This module computes one: a content digest over
//!
//! * the **effective automation configuration revision** — the full
//!   [`AutomationConfig`] the run executed under. A content digest is the
//!   configuration's revision: it changes exactly when the effective settings
//!   change, and (unlike a monotonic revision counter) never advances for a
//!   rewrite that leaves the settings identical. Backend kind, host mode,
//!   model, timeout, and every per-task setting are inside it.
//! * the **backend executable identity** — the opened `codex` binary the port
//!   will actually spawn. The component carries stable opened-file identity
//!   (Unix `dev`/`ino`, Windows volume serial / file index / link count) plus
//!   revision evidence (length, mtime, and a `sha256:` content digest). A
//!   missing or unreadable executable is a typed `spec` + `unreadable`
//!   component, not a hasher error. Pointing the backend at a different or
//!   upgraded executable is a backend change even when no setting moved.
//! * the **protocol revision** — [`AGENT_BACKEND_PROTOCOL_REVISION`], our own
//!   side of the transport contract. The app-server handshake, framing, and
//!   process lifetime are ours, so shipping a transport fix is a backend
//!   change. Without this component a suppression recorded by a broken build
//!   would outlive the build that fixed it.
//!
//! Re-admission is automatic and needs no operator action beyond the change
//! itself: the scheduler compares the identity stamped on the settled failure
//! against the identity now configured, and any difference re-admits the task.

use std::collections::HashMap;
use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use serde_json::{Value, json};
use tracedecay_domain::canonical_sha256;

use super::backend::AgentTaskFailureClass;
use super::config::{AutomationBackend, AutomationConfig};
use super::config_error;
use crate::errors::Result;
use crate::ports::codex_app_server::SummaryConfig as CodexAppServerSummaryConfig;

/// Skip reason published when a settled deterministic backend failure
/// suppresses a task under an unchanged backend/configuration identity.
pub const BACKEND_IDENTITY_SUPPRESSED: &str = "backend_identity_suppressed";

/// Revision of **our** side of the agent-backend protocol contract.
///
/// This is the component that lets a deterministic failure recorded by a
/// broken build be re-admitted by the build that fixes it. The crate version
/// cannot serve that role: `tracedecay-agent-hosts` is pinned at `0.1.0` and
/// does not move when the workspace is released, so a suppression written
/// today would otherwise outlive its own fix forever.
///
/// **Bump this whenever the transport contract changes** — the handshake,
/// the request framing, the process lifetime, or which provider methods a
/// turn depends on. Every task settled against the previous revision is
/// re-admitted on the next tick, which is exactly the intended effect of
/// shipping a transport fix.
///
/// `v2` holds the client's stdin open for the whole turn. `v1` closed it
/// immediately after `turn/start`, which `codex app-server` reads as a client
/// disconnect: it shut the session down within 70ms, cancelled the in-flight
/// turn, and exited 0, so every run failed as
/// `codex app-server closed stdout before completing`.
const AGENT_BACKEND_PROTOCOL_REVISION: &str = "codex-app-server.v2.stdin-held-through-turn";

#[derive(Clone, Eq, Hash, PartialEq)]
struct ExecutableDigestCacheKey {
    path: PathBuf,
    device: u64,
    file_index: u64,
    len: u64,
    mtime_secs: i64,
    mtime_nanos: i64,
}

fn executable_digest_cache() -> &'static Mutex<HashMap<ExecutableDigestCacheKey, String>> {
    static CACHE: OnceLock<Mutex<HashMap<ExecutableDigestCacheKey, String>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Computes the durable backend/configuration identity for `config`.
///
/// The executable component is read from the same environment overrides the
/// port itself honours, so the identity tracks the binary that would actually
/// be spawned rather than a nominal default. Envelope hashing stays
/// [`canonical_sha256`]; hasher failures are `config_error` results.
pub fn backend_identity(config: &AutomationConfig) -> Result<String> {
    // `canonical_sha256` is the crate's one identity primitive: key-ordered,
    // whitespace-free, and already used to derive the configuration identity
    // that curation decisions are bound to.
    let configuration_revision = canonical_sha256(config).map_err(|error| {
        config_error(format!("derive automation configuration identity: {error}"))
    })?;
    canonical_sha256(&json!({
        "kind": "automation.backend_identity.v1",
        "configuration_revision": configuration_revision.as_str(),
        "backend": config.backend.as_str(),
        "executable": backend_executable_identity(config),
        "protocol_revision": AGENT_BACKEND_PROTOCOL_REVISION,
    }))
    .map(|digest| digest.as_str().to_owned())
    .map_err(|error| config_error(format!("derive automation backend identity: {error}")))
}

/// The executable the configured backend would spawn, or `None` for a backend
/// that spawns nothing.
///
/// The component is the opened file, not the resolved path alone: stable
/// device/index identity plus length, mtime, and a receipt-tagged content
/// digest. A missing or unreadable executable stays a typed
/// `spec` + `unreadable` component so identity remains computable.
fn backend_executable_identity(config: &AutomationConfig) -> Option<Value> {
    match config.backend {
        AutomationBackend::Disabled => None,
        AutomationBackend::CodexAppServer => {
            let spec = CodexAppServerSummaryConfig::from_env().codex_bin;
            Some(codex_executable_identity(&spec))
        }
    }
}

fn codex_executable_identity(spec: &str) -> Value {
    match locate_backend_executable(spec) {
        Ok(Some(path)) => opened_executable_identity(spec, &path)
            .unwrap_or_else(|| unreadable_executable_identity(spec, Some(path.as_path()))),
        Ok(None) => unreadable_executable_identity(spec, None),
        Err(error) => json!({
            "spec": spec,
            "state": "host_io_unavailable",
            "error": error.to_string(),
        }),
    }
}

fn locate_backend_executable(spec: &str) -> Result<Option<PathBuf>> {
    let spec_path = Path::new(spec);
    if spec_path.is_absolute() || spec.contains(std::path::MAIN_SEPARATOR) {
        return Ok(spec_path.is_file().then(|| spec_path.to_path_buf()));
    }
    super::executable_lookup::resolve_on_path(spec, std::env::var_os("PATH").as_deref())
}

fn unreadable_executable_identity(spec: &str, path: Option<&Path>) -> Value {
    match path {
        Some(path) => json!({
            "spec": spec,
            "path": path.to_string_lossy(),
            "state": "unreadable",
        }),
        None => json!({
            "spec": spec,
            "state": "unreadable",
        }),
    }
}

fn opened_executable_identity(spec: &str, path: &Path) -> Option<Value> {
    let mut file = File::open(path).ok()?;
    let metadata = file.metadata().ok()?;
    let revision = opened_file_revision(&file, &metadata)?;
    let content = cached_executable_content_digest(&revision, path, &mut file)?;
    let mut identity = revision.fields;
    identity["spec"] = json!(spec);
    identity["path"] = json!(path.to_string_lossy());
    identity["content"] = json!(content);
    Some(identity)
}

struct OpenedFileRevision {
    device: u64,
    file_index: u64,
    len: u64,
    mtime_secs: i64,
    mtime_nanos: i64,
    fields: Value,
}

#[cfg(unix)]
fn opened_file_revision(_file: &File, metadata: &Metadata) -> Option<OpenedFileRevision> {
    use std::os::unix::fs::MetadataExt;
    let device = metadata.dev();
    let file_index = metadata.ino();
    let len = metadata.len();
    let mtime_secs = metadata.mtime();
    let mtime_nanos = metadata.mtime_nsec();
    Some(OpenedFileRevision {
        device,
        file_index,
        len,
        mtime_secs,
        mtime_nanos,
        fields: json!({
            "device": device,
            "file_index": file_index,
            "len": len,
            "mtime_secs": mtime_secs,
            "mtime_nanos": mtime_nanos,
        }),
    })
}

#[cfg(windows)]
fn opened_file_revision(file: &File, metadata: &Metadata) -> Option<OpenedFileRevision> {
    let information = tracedecay_private_fs::windows_file::information(file).ok()?;
    let len = metadata.len();
    let (mtime_secs, mtime_nanos) = windows_opened_mtime(metadata)?;
    Some(OpenedFileRevision {
        device: u64::from(information.volume_serial_number),
        file_index: information.file_index,
        len,
        mtime_secs,
        mtime_nanos,
        fields: json!({
            "volume_serial_number": information.volume_serial_number,
            "file_index": information.file_index,
            "number_of_links": information.number_of_links,
            "len": len,
            "mtime_secs": mtime_secs,
            "mtime_nanos": mtime_nanos,
        }),
    })
}

#[cfg(windows)]
fn windows_opened_mtime(metadata: &Metadata) -> Option<(i64, i64)> {
    let modified = metadata.modified().ok()?;
    let duration = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some((
        i64::try_from(duration.as_secs()).ok()?,
        i64::from(duration.subsec_nanos()),
    ))
}

#[cfg(not(any(unix, windows)))]
fn opened_file_revision(_file: &File, _metadata: &Metadata) -> Option<OpenedFileRevision> {
    None
}

fn cached_executable_content_digest(
    revision: &OpenedFileRevision,
    path: &Path,
    file: &mut File,
) -> Option<String> {
    let key = ExecutableDigestCacheKey {
        path: path.to_path_buf(),
        device: revision.device,
        file_index: revision.file_index,
        len: revision.len,
        mtime_secs: revision.mtime_secs,
        mtime_nanos: revision.mtime_nanos,
    };
    if let Some(digest) = executable_digest_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&key)
        .cloned()
    {
        return Some(digest);
    }
    let digest = digest_opened_file(file).ok()?;
    executable_digest_cache()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(key, digest.clone());
    Some(digest)
}

fn digest_opened_file(file: &mut File) -> std::io::Result<String> {
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    Ok(super::artifacts::sha256_bytes(&bytes))
}

/// Whether a failure class is deterministic under a fixed backend and
/// configuration.
///
/// Only [`AgentTaskFailureClass::Permanent`] stands as identity suppress.
/// `Unavailable`, `Denied`, `Disconnected`, `MalformedOutput`, `Timeout`, and
/// `Retryable` can change without a backend or configuration revision
/// (installation, credentials, provider policy, load), so they keep the
/// ordinary failure cooldown.
#[must_use]
pub fn is_deterministic_failure_class(class: AgentTaskFailureClass) -> bool {
    matches!(class, AgentTaskFailureClass::Permanent)
}

#[cfg(test)]
pub(crate) struct CodexBinEnvGuard {
    previous: Option<std::ffi::OsString>,
    _lock: std::sync::MutexGuard<'static, ()>,
}

#[cfg(test)]
impl CodexBinEnvGuard {
    pub(crate) fn set(path: &std::path::Path) -> Self {
        let lock = crate::config::lock_user_data_dir_test_env();
        let previous = std::env::var_os("TRACEDECAY_CODEX_BIN");
        // SAFETY: the shared user-data-dir lock is held for the guard
        // lifetime, so sibling env tests cannot observe this override.
        unsafe {
            std::env::set_var("TRACEDECAY_CODEX_BIN", path);
        }
        Self {
            previous,
            _lock: lock,
        }
    }
}

#[cfg(test)]
impl Drop for CodexBinEnvGuard {
    fn drop(&mut self) {
        // SAFETY: see `CodexBinEnvGuard::set`.
        unsafe {
            match self.previous.take() {
                Some(previous) => std::env::set_var("TRACEDECAY_CODEX_BIN", previous),
                None => std::env::remove_var("TRACEDECAY_CODEX_BIN"),
            }
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn config() -> AutomationConfig {
        AutomationConfig {
            enabled: true,
            backend: AutomationBackend::CodexAppServer,
            ..AutomationConfig::default()
        }
    }

    #[test]
    fn identity_is_stable_for_an_unchanged_configuration() {
        // `TRACEDECAY_CODEX_BIN` is process-global. Hold the canonical test
        // environment lock across both reads so an executable-replacement
        // test cannot change the authority between them.
        let _env_lock = crate::config::lock_user_data_dir_test_env();
        let config = config();
        assert_eq!(
            backend_identity(&config).unwrap(),
            backend_identity(&config).unwrap()
        );
    }

    #[test]
    fn identity_changes_when_the_configuration_revision_changes() {
        let _env_lock = crate::config::lock_user_data_dir_test_env();
        let before = backend_identity(&config()).unwrap();
        let after_config = AutomationConfig {
            timeout_secs: config().timeout_secs.saturating_add(1),
            ..config()
        };
        assert_ne!(before, backend_identity(&after_config).unwrap());
    }

    #[test]
    fn identity_changes_when_the_backend_changes() {
        let _env_lock = crate::config::lock_user_data_dir_test_env();
        let before = backend_identity(&config()).unwrap();
        let after_config = AutomationConfig {
            backend: AutomationBackend::Disabled,
            ..config()
        };
        assert_ne!(before, backend_identity(&after_config).unwrap());
    }

    #[test]
    fn identity_changes_when_the_protocol_revision_changes() {
        // The property this guards: a suppression written by a build with a
        // broken transport must not survive the build that fixes it. Only the
        // protocol-revision component can carry that, because the crate
        // version never moves.
        let _env_lock = crate::config::lock_user_data_dir_test_env();
        let identity = backend_identity(&config()).unwrap();
        let with_other_revision = canonical_sha256(&json!({
            "kind": "automation.backend_identity.v1",
            "configuration_revision": canonical_sha256(&config()).unwrap().as_str(),
            "backend": config().backend.as_str(),
            "executable": backend_executable_identity(&config()),
            "protocol_revision": "codex-app-server.v1.stdin-closed-after-turn-start",
        }))
        .unwrap();
        assert_ne!(identity, with_other_revision.as_str());
    }

    #[test]
    fn only_typed_permanent_failures_are_deterministic() {
        assert!(is_deterministic_failure_class(
            AgentTaskFailureClass::Permanent
        ));
        for class in [
            AgentTaskFailureClass::Disconnected,
            AgentTaskFailureClass::Unavailable,
            AgentTaskFailureClass::Denied,
            AgentTaskFailureClass::MalformedOutput,
            AgentTaskFailureClass::Timeout,
            AgentTaskFailureClass::Retryable,
        ] {
            assert!(
                !is_deterministic_failure_class(class),
                "{class:?} must keep ordinary cooldown, not standing identity suppress",
            );
        }
    }

    #[test]
    fn identity_changes_when_the_same_path_executable_is_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("codex-backend");
        std::fs::write(&path, b"backend-revision-one").unwrap();
        let _env = super::CodexBinEnvGuard::set(&path);
        let before = backend_identity(&config()).unwrap();
        std::fs::write(&path, b"backend-revision-two-replaced").unwrap();
        let after = backend_identity(&config()).unwrap();
        assert_ne!(
            before, after,
            "replacing bytes at the same executable path must change the identity"
        );
    }
}
