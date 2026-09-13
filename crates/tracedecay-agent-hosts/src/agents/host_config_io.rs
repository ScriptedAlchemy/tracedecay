//! Host configuration file IO shared by every agent integration: lenient and
//! strict JSON/JSONC/TOML loaders, backup-then-atomic-replace writers with
//! durable write intents, host file metadata capture, and the binary and
//! host-directory probes installers embed into generated config.

use std::borrow::Cow;
use std::cell::RefCell;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tracedecay_domain::canonical_text::sha256_hex;
use tracedecay_domain::errors::{Result, TraceDecayError};

use super::text_file_transaction::{self, TextFileMutation, update_config_file_transactionally};

#[cfg(test)]
mod tests;

/// Load a JSON file, returning an empty object on missing/invalid.
/// Use this for **read-only** paths (healthcheck, `has_tracedecay`, etc.).
/// For install/edit paths, use [`load_json_file_strict`] instead.
pub fn load_json_file(path: &Path) -> serde_json::Value {
    if path.exists() {
        let contents = std::fs::read_to_string(path).unwrap_or_default();
        serde_json::from_str(&contents).unwrap_or_else(|_| serde_json::json!({}))
    } else {
        serde_json::json!({})
    }
}

/// Dialect of a JSON-shaped host config, binding the strict content parser
/// used on write paths. Read-only probes keep the lenient [`load_json_file`]
/// / [`load_jsonc_file`] loaders.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JsonConfigDialect {
    Json,
    Jsonc,
}

impl JsonConfigDialect {
    /// Strict parse of already-observed config contents for a write path.
    /// Missing or blank content is a fresh `{}`; anything unparseable is a
    /// typed error so a transform never runs against fabricated state.
    pub(super) fn parse_for_edit(self, path: &Path, contents: &str) -> Result<serde_json::Value> {
        if contents.trim().is_empty() {
            return Ok(serde_json::json!({}));
        }
        let (dialect_label, parseable) = match self {
            Self::Json => ("JSON", Cow::Borrowed(contents)),
            Self::Jsonc => ("JSONC", Cow::Owned(strip_jsonc_comments(contents))),
        };
        serde_json::from_str(&parseable).map_err(|e| TraceDecayError::Config {
            message: format!(
                "cannot parse {} as {dialect_label}: {e}\n  \
                 Hint: fix the JSON syntax manually and re-run the command,\n  \
                 or delete the file to start fresh",
                path.display()
            ),
        })
    }
}

/// Load a JSON file for **editing**. Unlike [`load_json_file`], this returns
/// an error if the file exists but cannot be parsed, preventing silent data
/// loss when the modified value is written back.
///
/// # Error conditions
/// - File exists but is not readable (permissions, I/O error).
/// - File exists and has content but contains invalid JSON.
///
/// Returns `Ok(json!({}))` only when the file does not exist or is empty,
/// which is safe for creating a new config from scratch.
#[hotpath::measure(label = "agent_hosts.agents.config.load_json")]
pub fn load_json_file_strict(path: &Path) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let contents = std::fs::read_to_string(path).map_err(|e| TraceDecayError::Config {
        message: format!("cannot read {}: {e}", path.display()),
    })?;
    JsonConfigDialect::Json.parse_for_edit(path, &contents)
}

pub fn config_backup_path(path: &Path) -> PathBuf {
    PathBuf::from(format!("{}.bak", path.display()))
}

/// Create a backup copy of a config file before modifying it.
///
/// The backup itself is written atomically: content is first written to a
/// staging file (`.bak.new`), then renamed to `.bak`. This ensures the
/// `.bak` file is never half-written even if the process is killed.
///
/// Returns `Ok(Some(backup_path))` when a backup was created, or `Ok(None)`
/// when the file did not exist (nothing to back up).
///
/// # Error conditions
/// - File exists but cannot be read (permissions, I/O error).
/// - Staging file cannot be written (disk full, permissions).
/// - Staging file cannot be renamed to `.bak` (cross-device, permissions).
#[hotpath::measure(label = "agent_hosts.agents.host_config.backup")]
pub fn backup_config_file(path: &Path) -> Result<Option<PathBuf>> {
    if !path.exists() {
        return Ok(None);
    }
    let backup_path = config_backup_path(path);
    let staging_path = PathBuf::from(format!("{}.bak.new", path.display()));

    let content = std::fs::read(path).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to read {} for backup: {e}\n  \
             Hint: check file permissions",
            path.display()
        ),
    })?;
    std::fs::write(&staging_path, &content).map_err(|e| {
        std::fs::remove_file(&staging_path).ok();
        TraceDecayError::Config {
            message: format!(
                "failed to write backup staging file {}: {e}\n  \
                 Hint: check available disk space and permissions",
                staging_path.display()
            ),
        }
    })?;
    // The backup holds the same secrets as the original (host configs can
    // carry credential env values), so it must not be published with the
    // umask-default mode: copy the original's permission identity onto the
    // staging file before it becomes `.bak`.
    let original_metadata =
        capture_host_file_metadata(path).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to capture metadata for {} before backup: {error}",
                path.display()
            ),
        })?;
    restore_host_file_metadata(&staging_path, &original_metadata).map_err(|error| {
        std::fs::remove_file(&staging_path).ok();
        TraceDecayError::Config {
            message: format!(
                "failed to apply original permissions to backup staging file {}: {error}",
                staging_path.display()
            ),
        }
    })?;
    let backup_metadata =
        capture_host_file_metadata(&staging_path).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to inspect backup staging file {}: {error}",
                staging_path.display()
            ),
        })?;
    persist_host_config_write_intent(&backup_path, &content, Some(&backup_metadata))?;

    // Atomic rename staging → .bak
    std::fs::rename(&staging_path, &backup_path).map_err(|e| {
        std::fs::remove_file(&staging_path).ok();
        TraceDecayError::Config {
            message: format!(
                "failed to create backup {}: {e}\n  \
                 Hint: check file permissions",
                backup_path.display()
            ),
        }
    })?;

    Ok(Some(backup_path))
}

/// Restore a config file from its backup. Prints instructions for manual
/// recovery if the restore itself fails.
pub fn restore_config_backup(original: &Path, backup: &Path) {
    match std::fs::copy(backup, original) {
        Ok(_) => {
            eprintln!(
                "\x1b[33m⚠\x1b[0m  Restored {} from backup",
                original.display()
            );
        }
        Err(e) => {
            eprintln!(
                "\x1b[31m✗\x1b[0m Failed to auto-restore {} from backup: {e}",
                original.display()
            );
            eprintln!(
                "  Manual recovery: cp '{}' '{}'",
                backup.display(),
                original.display()
            );
        }
    }
}

/// Write a JSON value to a file via atomic rename.
///
/// The caller is responsible for creating the backup via
/// [`backup_config_file`] before loading the config. Pass the backup path
/// here so that it can be mentioned in error messages and used for restore
/// if the rename somehow leaves the target in a bad state.
///
/// # Strategy
///
/// Serialize and re-validate, then use the shared durable atomic writer to
/// replace the target, preserve its permissions, and sync its parent entry.
///
/// # Error conditions
/// - Serialization failure (should not happen with well-formed Values).
/// - Re-parse validation failure (internal bug).
/// - Cannot create parent directory.
/// - Atomic staging or publication failure (permissions, disk full).
///
/// In every error case the original file remains intact.
pub fn safe_write_json_file(
    path: &Path,
    value: &serde_json::Value,
    backup: Option<&Path>,
) -> Result<()> {
    let content = render_json_config(path, value)?;
    safe_write_bytes_file(path, content.as_bytes(), backup)
}

/// Serialize a JSON config value for publication: pretty-printed, re-parse
/// validated, trailing newline.
pub(super) fn render_json_config(path: &Path, value: &serde_json::Value) -> Result<String> {
    let pretty = serde_json::to_string_pretty(value).map_err(|e| TraceDecayError::Config {
        message: format!("failed to serialize JSON for {}: {e}", path.display()),
    })?;

    // Re-parse to verify the serialized output is valid JSON.
    if serde_json::from_str::<serde_json::Value>(&pretty).is_err() {
        return Err(TraceDecayError::Config {
            message: format!(
                "internal error: serialized JSON for {} failed re-parse validation.\n  \
                 This is a bug in tracedecay — please report it.",
                path.display()
            ),
        });
    }

    Ok(format!("{pretty}\n"))
}

/// Value-level mutation for [`update_json_config_transactionally`].
pub(crate) enum JsonConfigMutation {
    Unchanged,
    Write(serde_json::Value),
    Remove,
}

/// Read-under-lock → transform → publish-from-snapshot for a JSON-shaped host
/// config. The transform sees the value parsed strictly from the exact bytes
/// the write lock observed, so a concurrent writer can no longer slip between
/// load and publish, and a corrupt config is a typed error instead of a
/// silently-empty object. Rewrites and removals of an existing file leave a
/// `.bak` (issue #63).
pub(crate) fn update_json_config_transactionally<T>(
    path: &Path,
    dialect: JsonConfigDialect,
    update: impl FnOnce(serde_json::Value) -> Result<(T, JsonConfigMutation)>,
) -> Result<T> {
    update_config_file_transactionally(path, |existing| {
        let settings = dialect.parse_for_edit(path, existing)?;
        let (output, mutation) = update(settings)?;
        let mutation = match mutation {
            JsonConfigMutation::Unchanged => TextFileMutation::Unchanged,
            JsonConfigMutation::Write(value) => {
                TextFileMutation::Write(render_json_config(path, &value)?)
            }
            JsonConfigMutation::Remove => TextFileMutation::Remove,
        };
        Ok((output, mutation))
    })
}

/// TOML sibling of [`update_json_config_transactionally`]. The transform
/// returns the serialized replacement text itself because TOML publication
/// may need post-serialization shaping (Codex's explicit `[hooks.state]`
/// parent table).
pub(crate) fn update_toml_config_transactionally<T>(
    path: &Path,
    update: impl FnOnce(toml::Value) -> Result<(T, TextFileMutation)>,
) -> Result<T> {
    update_config_file_transactionally(path, |existing| {
        let value = parse_toml_config(path, existing)?;
        update(value)
    })
}

/// Write text to a file via atomic sibling rename.
///
/// Mirrors [`safe_write_json_file`] for generated prompt/rule files that are
/// plain text rather than structured JSON. The target is not opened for writing
/// until the final rename, so a failed write leaves the original untouched.
pub fn safe_write_text_file(path: &Path, contents: &str, backup: Option<&Path>) -> Result<()> {
    safe_write_bytes_file(path, contents.as_bytes(), backup)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct HostFileMetadataIdentityV1 {
    readonly: bool,
    unix_mode: Option<u32>,
    posix_acl_supported: bool,
    posix_acl_access: Option<Vec<u8>>,
}

pub fn capture_host_file_metadata(path: &Path) -> std::io::Result<HostFileMetadataIdentityV1> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
        return Err(std::io::Error::other(format!(
            "unsafe host metadata path: {}",
            path.display()
        )));
    }
    let permissions = metadata.permissions();
    #[cfg(unix)]
    let unix_mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(permissions.mode())
    };
    #[cfg(not(unix))]
    let unix_mode = None;
    #[cfg(target_os = "linux")]
    let (posix_acl_supported, posix_acl_access) = match xattr::get(path, "system.posix_acl_access")
    {
        Ok(acl) => (true, acl),
        Err(error) if error.kind() == std::io::ErrorKind::Unsupported => (false, None),
        Err(error) => return Err(error),
    };
    #[cfg(not(target_os = "linux"))]
    let (posix_acl_supported, posix_acl_access) = (false, None);
    Ok(HostFileMetadataIdentityV1 {
        readonly: permissions.readonly(),
        unix_mode,
        posix_acl_supported,
        posix_acl_access,
    })
}

pub fn restore_host_file_metadata(
    path: &Path,
    state: &HostFileMetadataIdentityV1,
) -> std::io::Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !(metadata.is_file() || metadata.is_dir()) {
        return Err(std::io::Error::other(format!(
            "unsafe host metadata path: {}",
            path.display()
        )));
    }
    let mut permissions = metadata.permissions();
    #[cfg(unix)]
    if let Some(mode) = state.unix_mode {
        use std::os::unix::fs::PermissionsExt;
        permissions.set_mode(mode);
    }
    #[cfg(not(unix))]
    permissions.set_readonly(state.readonly);
    let current = std::fs::symlink_metadata(path)?;
    if current.file_type().is_symlink()
        || current.file_type() != metadata.file_type()
        || !(current.is_file() || current.is_dir())
    {
        return Err(std::io::Error::other(format!(
            "host metadata path changed type: {}",
            path.display()
        )));
    }
    std::fs::set_permissions(path, permissions)?;
    #[cfg(target_os = "linux")]
    if state.posix_acl_supported {
        let current = std::fs::symlink_metadata(path)?;
        if current.file_type().is_symlink() || !(current.is_file() || current.is_dir()) {
            return Err(std::io::Error::other(format!(
                "host metadata path changed type: {}",
                path.display()
            )));
        }
        match &state.posix_acl_access {
            Some(acl) => xattr::set(path, "system.posix_acl_access", acl)?,
            None => {
                if xattr::get(path, "system.posix_acl_access")?.is_some() {
                    xattr::remove(path, "system.posix_acl_access")?;
                }
            }
        }
    }
    Ok(())
}

/// Atomically replace a host-owned file while preserving existing permissions.
///
/// The shared durable writer syncs bytes and the parent entry. When replacing
/// an existing host config, restore its exact permission bits and durably flush
/// that metadata before returning. This authority is shared by every host:
/// existing config symlinks are always refused so no integration can redirect
/// a lifecycle write outside its inventoried path.
pub fn safe_write_bytes_file(path: &Path, contents: &[u8], backup: Option<&Path>) -> Result<()> {
    safe_write_bytes_file_with_metadata(path, contents, backup, None)
}

#[hotpath::measure(label = "agent_hosts.agents.host_config.write")]
pub fn safe_write_bytes_file_with_metadata(
    path: &Path,
    contents: &[u8],
    backup: Option<&Path>,
    replacement_metadata: Option<&HostFileMetadataIdentityV1>,
) -> Result<()> {
    text_file_transaction::write_bytes_file_locked(path, contents, backup, replacement_metadata)
}

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum TestHostConfigWriteBoundary {
    Validation,
    Publication,
    Published,
}

#[cfg(test)]
struct TestHostConfigWritePause {
    state: std::sync::Mutex<(bool, bool)>,
    changed: std::sync::Condvar,
}

#[cfg(test)]
type TestHostConfigWritePauseEntry = (
    PathBuf,
    TestHostConfigWriteBoundary,
    std::sync::Weak<TestHostConfigWritePause>,
);

#[cfg(test)]
static TEST_HOST_CONFIG_WRITE_PAUSES: std::sync::LazyLock<
    std::sync::Mutex<Vec<TestHostConfigWritePauseEntry>>,
> = std::sync::LazyLock::new(|| std::sync::Mutex::new(Vec::new()));

#[cfg(test)]
pub(super) struct TestHostConfigWritePauseController(std::sync::Arc<TestHostConfigWritePause>);

#[cfg(test)]
impl TestHostConfigWritePauseController {
    pub(super) fn wait_until_reached(&self) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let mut state = self.0.state.lock().expect("host write pause state");
        while !state.0 {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                drop(state);
                panic!("host write did not reach its publication boundary");
            };
            let (next, timeout) = self
                .0
                .changed
                .wait_timeout(state, remaining)
                .expect("host write pause wait");
            state = next;
            if !state.0 && timeout.timed_out() {
                drop(state);
                panic!("host write pause timed out");
            }
        }
    }

    pub(super) fn resume(&self) {
        let mut state = self.0.state.lock().expect("host write pause state");
        state.1 = true;
        self.0.changed.notify_all();
    }
}

#[cfg(test)]
impl Drop for TestHostConfigWritePauseController {
    fn drop(&mut self) {
        self.resume();
    }
}

#[cfg(test)]
pub(super) fn pause_next_host_config_write_after_validation(
    path: &Path,
) -> TestHostConfigWritePauseController {
    pause_next_host_config_write(path, TestHostConfigWriteBoundary::Validation)
}

#[cfg(test)]
pub(super) fn pause_next_host_config_write_at_publication(
    path: &Path,
) -> TestHostConfigWritePauseController {
    pause_next_host_config_write(path, TestHostConfigWriteBoundary::Publication)
}

#[cfg(test)]
fn pause_next_host_config_write_after_publication(
    path: &Path,
) -> TestHostConfigWritePauseController {
    pause_next_host_config_write(path, TestHostConfigWriteBoundary::Published)
}

#[cfg(test)]
fn pause_next_host_config_write(
    path: &Path,
    boundary: TestHostConfigWriteBoundary,
) -> TestHostConfigWritePauseController {
    let pause = std::sync::Arc::new(TestHostConfigWritePause {
        state: std::sync::Mutex::new((false, false)),
        changed: std::sync::Condvar::new(),
    });
    TEST_HOST_CONFIG_WRITE_PAUSES
        .lock()
        .expect("host write pause registry")
        .push((
            path.to_path_buf(),
            boundary,
            std::sync::Arc::downgrade(&pause),
        ));
    TestHostConfigWritePauseController(pause)
}

#[cfg(test)]
pub(super) fn test_pause_host_config_write(path: &Path, boundary: TestHostConfigWriteBoundary) {
    let pause = {
        let mut pauses = TEST_HOST_CONFIG_WRITE_PAUSES
            .lock()
            .expect("host write pause registry");
        let Some(index) = pauses
            .iter()
            .position(|(candidate, candidate_boundary, pause)| {
                candidate == path
                    && *candidate_boundary == boundary
                    && std::sync::Weak::strong_count(pause) > 0
            })
        else {
            return;
        };
        pauses.remove(index).2.upgrade()
    };
    let Some(pause) = pause else {
        return;
    };
    let mut state = pause.state.lock().expect("host write pause state");
    state.0 = true;
    pause.changed.notify_all();
    while !state.1 {
        state = pause.changed.wait(state).expect("host write pause wait");
    }
}

/// Crash-injection point for host-config durability acceptance tests: abort
/// the process right after a config write publishes so recovery paths are
/// exercised against a real torn install.
#[cfg(feature = "test-transport")]
pub(super) fn test_abort_after_host_config_write(path: &Path) {
    if (std::env::var_os("TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE").is_some()
        && !path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .is_some_and(|name| name.ends_with(".bak") || name.ends_with(".tracedecay-original")))
        || std::env::var_os("TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_WRITE_PATH")
            .is_some_and(|expected| Path::new(&expected) == path)
    {
        std::process::abort();
    }
}

thread_local! {
    static HOST_CONFIG_WRITE_INTENT_ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

pub fn with_host_config_write_intents<T>(root: PathBuf, effect: impl FnOnce() -> T) -> T {
    struct ResetIntentRoot(Option<PathBuf>);

    impl Drop for ResetIntentRoot {
        fn drop(&mut self) {
            HOST_CONFIG_WRITE_INTENT_ROOT.with(|current| {
                current.replace(self.0.take());
            });
        }
    }

    let previous = HOST_CONFIG_WRITE_INTENT_ROOT.with(|current| current.replace(Some(root)));
    let _reset = ResetIntentRoot(previous);
    effect()
}

pub fn host_config_write_intent_path(root: &Path, path: &Path) -> Result<PathBuf> {
    let path_bytes = serde_json::to_vec(path).map_err(|error| TraceDecayError::Config {
        message: format!("could not bind host config write intent: {error}"),
    })?;
    Ok(root.join(format!("{}.intent", sha256_hex(&path_bytes))))
}

/// Persist an already-read native-host observation without reading the file a
/// second time. A host CLI runs outside this process, so the caller snapshots
/// its bytes first, checks peer ownership, and then records exactly those
/// bytes. If a foreign writer races after the snapshot, rollback compares the
/// recorded digest to the live state and refuses instead of restoring over it.
#[hotpath::measure(label = "agent_hosts.agents.host_config.observe")]
pub(crate) fn record_host_config_observation_bytes(
    path: &Path,
    contents: Option<&[u8]>,
) -> Result<()> {
    if HOST_CONFIG_WRITE_INTENT_ROOT.with(|current| current.borrow().is_none()) {
        return Ok(());
    }
    match contents {
        Some(contents) => {
            let metadata =
                capture_host_file_metadata(path).map_err(|error| TraceDecayError::Config {
                    message: format!(
                        "failed to capture metadata for {} after host CLI: {error}",
                        path.display()
                    ),
                })?;
            persist_host_config_write_intent(path, contents, Some(&metadata))
        }
        None => persist_host_config_remove_intent(path),
    }
}

#[hotpath::measure(label = "agent_hosts.agents.host_config.remove")]
pub fn safe_remove_host_file(path: &Path) -> std::io::Result<()> {
    persist_host_config_remove_intent(path).map_err(std::io::Error::other)?;
    std::fs::remove_file(path)?;
    #[cfg(feature = "test-transport")]
    if std::env::var_os("TRACEDECAY_TEST_ABORT_AFTER_HOST_CONFIG_REMOVE_PATH")
        .is_some_and(|expected| Path::new(&expected) == path)
    {
        std::process::abort();
    }
    Ok(())
}

#[derive(Serialize)]
struct HostConfigWriteIntentV2<'a> {
    schema_version: u16,
    digest: [u8; 32],
    metadata: Option<&'a HostFileMetadataIdentityV1>,
}

#[hotpath::measure(label = "agent_hosts.agents.host_config.persist")]
pub(super) fn persist_host_config_write_intent(
    path: &Path,
    contents: &[u8],
    metadata: Option<&HostFileMetadataIdentityV1>,
) -> Result<()> {
    let Some(root) = HOST_CONFIG_WRITE_INTENT_ROOT.with(|current| current.borrow().clone()) else {
        return Ok(());
    };
    std::fs::create_dir_all(&root).map_err(|error| TraceDecayError::Config {
        message: format!(
            "could not create host config write intent directory {}: {error}",
            root.display()
        ),
    })?;
    let intent_path = host_config_write_intent_path(&root, path)?;
    let intent = serde_json::to_vec(&HostConfigWriteIntentV2 {
        schema_version: 2,
        digest: Sha256::digest(contents).into(),
        metadata,
    })
    .map_err(|error| TraceDecayError::Config {
        message: format!("could not serialize host config write intent: {error}"),
    })?;
    tracedecay_private_fs::framed_log::atomic_write(
        &intent_path,
        "host-config-intent",
        &intent,
        tracedecay_private_fs::framed_log::DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!(
            "could not persist host config write intent {}: {error}",
            intent_path.display()
        ),
    })
}

#[hotpath::measure(label = "agent_hosts.agents.host_config.persist_remove")]
pub(super) fn persist_host_config_remove_intent(path: &Path) -> Result<()> {
    let Some(root) = HOST_CONFIG_WRITE_INTENT_ROOT.with(|current| current.borrow().clone()) else {
        return Ok(());
    };
    std::fs::create_dir_all(&root).map_err(|error| TraceDecayError::Config {
        message: format!(
            "could not create host config remove intent directory {}: {error}",
            root.display()
        ),
    })?;
    let intent_path = host_config_write_intent_path(&root, path)?;
    tracedecay_private_fs::framed_log::atomic_write(
        &intent_path,
        "host-config-remove-intent",
        &[0],
        tracedecay_private_fs::framed_log::DirectorySyncPolicy::TolerateUnsupported,
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!(
            "could not persist host config remove intent {}: {error}",
            intent_path.display()
        ),
    })
}

/// Write a JSON value to a file with pretty formatting.
/// Creates a backup, writes atomically, and restores on failure.
#[hotpath::measure(label = "agent_hosts.agents.config.write_json")]
pub fn write_json_file(path: &Path, value: &serde_json::Value) -> Result<()> {
    let backup = backup_config_file(path)?;
    safe_write_json_file(path, value, backup.as_deref())?;
    eprintln!("\x1b[32m✔\x1b[0m Wrote {}", path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Shared MCP server registration
// ---------------------------------------------------------------------------
//
// Every JSON/JSONC-configured host registers tracedecay the same way: one
// entry named `tracedecay` under a root key (`mcpServers` for the Cline
// family and Gemini, `mcp` for Kilo). Only the config path, the root key, the
// entry shape, and the config dialect differ, so install, uninstall, and the
// doctor check all live here rather than once per host.

/// Resolve a host home directory from an optional environment override.
///
/// The override is honored only when it is non-empty and falls under `home`.
/// That keeps isolated-HOME tests from picking up the operator's real
/// `KIMI_CODE_HOME` / `VIBE_HOME`, and refuses a host directory that escapes
/// the admitted profile home. Anything else — unset, empty, or outside
/// `home` — uses `home.join(default_relative)`.
pub(crate) fn host_home_override(home: &Path, env_key: &str, default_relative: &str) -> PathBuf {
    std::env::var_os(env_key)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|override_home| override_home.starts_with(home))
        .unwrap_or_else(|| home.join(default_relative))
}

/// Finds the tracedecay binary path.
///
/// On Windows the returned path uses forward slashes so it can be safely
/// embedded in JSON hook commands without backslash-escaping issues.
pub fn which_tracedecay() -> Option<String> {
    which_tracedecay_path().and_then(|path| path.to_str().map(normalize_path_separators))
}

/// Finds the tracedecay binary without converting its platform-native path.
#[hotpath::measure(label = "agent_hosts.agents.which_tracedecay")]
pub fn which_tracedecay_path() -> Option<PathBuf> {
    let current_exe = std::env::current_exe().ok();
    let path_var = std::env::var_os("PATH");
    let cargo_target_dir = std::env::var_os("CARGO_TARGET_DIR").map(PathBuf::from);
    which_tracedecay_path_from(
        current_exe.as_deref(),
        path_var.as_deref(),
        cargo_target_dir.as_deref(),
    )
}

#[cfg(test)]
fn which_tracedecay_from(
    current_exe: Option<&Path>,
    path_var: Option<&std::ffi::OsStr>,
    cargo_target_dir: Option<&Path>,
) -> Option<String> {
    which_tracedecay_path_from(current_exe, path_var, cargo_target_dir)
        .and_then(|path| path.to_str().map(normalize_path_separators))
}

fn which_tracedecay_path_from(
    current_exe: Option<&Path>,
    path_var: Option<&std::ffi::OsStr>,
    cargo_target_dir: Option<&Path>,
) -> Option<PathBuf> {
    if let Some(exe) = current_exe
        .filter(|exe| is_tracedecay_exe(exe) && !is_cargo_target_binary(exe, cargo_target_dir))
    {
        return absolute_executable_path(exe);
    }

    let path_match = path_var.and_then(|path_var| {
        std::env::split_paths(path_var).find_map(|dir| {
            let candidate = dir.join(tracedecay_bin_name());
            (candidate.exists() && !is_cargo_target_binary(&candidate, cargo_target_dir))
                .then(|| absolute_executable_path(&candidate))
                .flatten()
        })
    });
    path_match.or_else(|| {
        current_exe
            .filter(|exe| is_tracedecay_exe(exe))
            .and_then(absolute_executable_path)
    })
}

fn absolute_executable_path(path: &Path) -> Option<PathBuf> {
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        std::path::absolute(path).ok()
    }
}

fn tracedecay_bin_name() -> String {
    format!("tracedecay{}", std::env::consts::EXE_SUFFIX)
}

fn is_tracedecay_exe(path: &Path) -> bool {
    path.file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            if cfg!(windows) {
                name.eq_ignore_ascii_case("tracedecay")
            } else {
                name == "tracedecay"
            }
        })
}

fn is_cargo_target_binary(path: &Path, cargo_target_dir: Option<&Path>) -> bool {
    if cargo_target_dir.is_some_and(|target_dir| path_starts_with_platform(path, target_dir)) {
        return true;
    }

    let mut saw_target = false;
    for component in path.components() {
        let value = component.as_os_str();
        if saw_target
            && tracedecay_runtime_core::config::CARGO_PROFILE_DIRS
                .iter()
                .any(|profile| path_component_eq(value, profile))
        {
            return true;
        }
        if path_component_eq(value, "target") {
            saw_target = true;
        }
    }
    false
}

fn path_starts_with_platform(path: &Path, prefix: &Path) -> bool {
    if !cfg!(windows) {
        return path.starts_with(prefix);
    }
    let mut path_components = path.components();
    prefix.components().all(|expected| {
        path_components
            .next()
            .is_some_and(|actual| path_component_eq(actual.as_os_str(), expected.as_os_str()))
    })
}

fn path_component_eq(actual: &std::ffi::OsStr, expected: impl AsRef<std::ffi::OsStr>) -> bool {
    let expected = expected.as_ref();
    if !cfg!(windows) {
        return actual == expected;
    }
    match (actual.to_str(), expected.to_str()) {
        (Some(actual), Some(expected)) => actual.eq_ignore_ascii_case(expected),
        _ => actual == expected,
    }
}

/// Replace backslashes with forward slashes so paths work in JSON/shell
/// contexts on Windows. No-op on Unix where paths already use `/`.
fn normalize_path_separators(path: &str) -> String {
    path.replace('\\', "/")
}

/// Remove explicitly retired sibling plugin trees.
///
/// Both the retired suffix and ownership manifest are allow-listed: a name
/// prefix alone is never ownership evidence. A sibling is removed only when it
/// is a real directory, its suffix is known to have been created by
/// `TraceDecay`, and one host-specific manifest parses with `name = "tracedecay"`.
#[hotpath::measure(label = "agent_hosts.agents.plugin.sweep_siblings")]
pub(crate) fn sweep_superseded_plugin_siblings(
    current_dir: &Path,
    ownership_manifests: &[&str],
) -> Result<()> {
    const RETIRED_SUFFIXES: &[&str] = &["pre-v2-adopt"];

    let Some(parent) = current_dir.parent() else {
        return Ok(());
    };
    let Some(current_name) = current_dir.file_name().and_then(|name| name.to_str()) else {
        return Ok(());
    };
    let prefix = format!("{current_name}.");
    let entries = match std::fs::read_dir(parent) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(TraceDecayError::Config {
                message: format!(
                    "failed to inspect plugin siblings in {}: {error}",
                    parent.display()
                ),
            });
        }
    };

    for entry in entries {
        let entry = entry.map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to inspect a plugin sibling in {}: {error}",
                parent.display()
            ),
        })?;
        let file_type = entry.file_type().map_err(|error| TraceDecayError::Config {
            message: format!("failed to inspect {}: {error}", entry.path().display()),
        })?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        let retired = name
            .strip_prefix(&prefix)
            .is_some_and(|suffix| RETIRED_SUFFIXES.contains(&suffix));
        if !file_type.is_dir() || !retired {
            continue;
        }
        let sibling = entry.path();
        let owned = ownership_manifests.iter().any(|relative| {
            load_json_file(&sibling.join(relative))
                .get("name")
                .and_then(serde_json::Value::as_str)
                == Some("tracedecay")
        });
        if !owned {
            continue;
        }
        std::fs::remove_dir_all(&sibling).map_err(|error| TraceDecayError::Config {
            message: format!(
                "failed to remove superseded tracedecay plugin {}: {error}",
                sibling.display()
            ),
        })?;
    }
    Ok(())
}

/// Recursively collect every regular file under `root` (following the same
/// hand-rolled walk both the Cursor and Codex installers rely on).
#[hotpath::measure(label = "agent_hosts.agents.fs.collect_regular_files")]
pub(crate) fn collect_regular_files(root: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    collect_regular_files_inner(root, &mut out)?;
    Ok(out)
}

fn collect_regular_files_inner(root: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            collect_regular_files_inner(&entry.path(), out)?;
        } else if file_type.is_file() {
            out.push(entry.path());
        }
    }
    Ok(())
}

pub(crate) fn hook_command(tracedecay_bin: &str, subcommand: &str) -> String {
    hook_command_for_platform(tracedecay_bin, subcommand, cfg!(windows))
}

fn hook_command_for_platform(tracedecay_bin: &str, subcommand: &str, windows: bool) -> String {
    let quoted = if windows {
        quote_windows_command_arg(&normalize_path_separators(tracedecay_bin))
    } else {
        quote_posix_command_arg(tracedecay_bin)
    };
    format!("{quoted} {subcommand}")
}

fn quote_windows_command_arg(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\\\""))
}

pub(super) fn quote_posix_command_arg(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn relative_project_path(
    project_root: &Path,
    canonical_root: &Path,
    absolute: &Path,
    original: &Path,
) -> Option<PathBuf> {
    if !original.is_absolute() {
        return Some(original.to_path_buf());
    }
    absolute
        .strip_prefix(project_root)
        .or_else(|_| absolute.strip_prefix(canonical_root))
        .ok()
        .map(Path::to_path_buf)
}

#[hotpath::measure(label = "agent_hosts.agents.fs.ensure_project_local")]
pub(crate) fn ensure_project_local_safe_path(project_root: &Path, path: &Path) -> Result<()> {
    let root = project_root
        .canonicalize()
        .map_err(|e| TraceDecayError::Config {
            message: format!(
                "failed to resolve project root {}: {e}",
                project_root.display()
            ),
        })?;
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        root.join(path)
    };
    if absolute
        .components()
        .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to write project-local config outside {}: {}",
                root.display(),
                absolute.display()
            ),
        });
    }

    if let Some(relative) = relative_project_path(project_root, &root, &absolute, path) {
        let scan_root = if project_root.is_absolute() {
            project_root.to_path_buf()
        } else {
            root.clone()
        };
        let mut current = scan_root;
        for component in relative.components() {
            if matches!(
                component,
                std::path::Component::Prefix(_) | std::path::Component::RootDir
            ) {
                continue;
            }
            current.push(component.as_os_str());
            let Ok(meta) = std::fs::symlink_metadata(&current) else {
                continue;
            };
            if meta.file_type().is_symlink() {
                return Err(TraceDecayError::Config {
                    message: format!(
                        "refusing to write project-local config through symlink: {}",
                        current.display()
                    ),
                });
            }
        }
    }

    let canonical_candidate = tracedecay_runtime_core::path_safety::canonicalize_existing_prefix(
        &absolute,
    )
    .ok_or_else(|| TraceDecayError::Config {
        message: format!(
            "failed to resolve project-local config path {}",
            absolute.display()
        ),
    })?;
    if !canonical_candidate.starts_with(&root) {
        return Err(TraceDecayError::Config {
            message: format!(
                "refusing to write project-local config outside {}: {}",
                root.display(),
                absolute.display()
            ),
        });
    }

    Ok(())
}

/// Guard every project-local write target up front: reject any path that
/// escapes `project_root` or reaches through a symlinked parent before the
/// installer creates directories or writes files. Mirrors the per-path
/// [`ensure_project_local_safe_path`] contract for adapters that touch several
/// project-local paths in one `install_local`.
pub(crate) fn ensure_project_local_safe_paths<'a, I>(project_root: &Path, paths: I) -> Result<()>
where
    I: IntoIterator<Item = &'a Path>,
{
    for path in paths {
        ensure_project_local_safe_path(project_root, path)?;
    }
    Ok(())
}

/// Returns the user's home directory, cross-platform.
pub fn home_dir() -> Option<PathBuf> {
    std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok()
        .map(PathBuf::from)
}

/// Strip `//` line comments, `/* */` block comments, and trailing commas
/// before `}` / `]` from a JSONC string, then parse with `serde_json`.
/// Falls back to `serde_json::json!({})` on any parse failure.
pub fn parse_jsonc(input: &str) -> serde_json::Value {
    let stripped = strip_jsonc_comments(input);
    serde_json::from_str(&stripped).unwrap_or_else(|_| serde_json::json!({}))
}

/// Internal helper: removes JSONC comments and trailing commas.
pub(super) fn strip_jsonc_comments(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;
    let mut in_string = false;

    while i < len {
        // Handle string literals (skip comment stripping inside strings).
        if in_string {
            if chars[i] == '\\' && i + 1 < len {
                out.push(chars[i]);
                out.push(chars[i + 1]);
                i += 2;
                continue;
            }
            if chars[i] == '"' {
                in_string = false;
            }
            out.push(chars[i]);
            i += 1;
            continue;
        }

        // Start of string.
        if chars[i] == '"' {
            in_string = true;
            out.push(chars[i]);
            i += 1;
            continue;
        }

        // Line comment `//`.
        if chars[i] == '/' && i + 1 < len && chars[i + 1] == '/' {
            // Skip until newline.
            while i < len && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }

        // Block comment `/* ... */`.
        if chars[i] == '/' && i + 1 < len && chars[i + 1] == '*' {
            i += 2;
            while i + 1 < len && !(chars[i] == '*' && chars[i + 1] == '/') {
                i += 1;
            }
            i += 2; // consume `*/`
            continue;
        }

        out.push(chars[i]);
        i += 1;
    }

    // Remove trailing commas before `}` or `]`.
    // Simple regex-free approach: repeatedly collapse ", <whitespace> }" patterns.
    remove_trailing_commas(&out)
}

/// Removes trailing commas that appear immediately before `}` or `]` (with
/// optional whitespace/newlines in between).
fn remove_trailing_commas(input: &str) -> String {
    // We scan for comma, optional whitespace, then `}` or `]`.
    let bytes = input.as_bytes();
    let len = bytes.len();
    let mut out = Vec::with_capacity(len);
    let mut i = 0;

    while i < len {
        if bytes[i] == b',' {
            // Peek ahead past whitespace.
            let mut j = i + 1;
            while j < len
                && (bytes[j] == b' ' || bytes[j] == b'\t' || bytes[j] == b'\n' || bytes[j] == b'\r')
            {
                j += 1;
            }
            if j < len && (bytes[j] == b'}' || bytes[j] == b']') {
                // Skip the comma; whitespace will be included normally.
                i += 1;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }

    String::from_utf8(out).unwrap_or_else(|_| input.to_string())
}

/// Read a file and parse it as JSONC. Falls back to `json!({})` if the file
/// is missing, unreadable, or unparseable.
/// Use this for **read-only** paths. For install/edit paths, use
/// [`load_jsonc_file_strict`] instead.
pub fn load_jsonc_file(path: &Path) -> serde_json::Value {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return serde_json::json!({});
    };
    parse_jsonc(&contents)
}

/// Load a JSONC file for **editing**. Unlike [`load_jsonc_file`], this returns
/// an error if the file exists but cannot be parsed after comment stripping,
/// preventing silent data loss when the modified value is written back.
///
/// # Error conditions
/// - File exists but is not readable (permissions, I/O error).
/// - File exists and has content but contains invalid JSONC.
///
/// Returns `Ok(json!({}))` only when the file does not exist or is empty.
#[hotpath::measure(label = "agent_hosts.agents.config.load_jsonc")]
pub fn load_jsonc_file_strict(path: &Path) -> Result<serde_json::Value> {
    if !path.exists() {
        return Ok(serde_json::json!({}));
    }
    let contents = std::fs::read_to_string(path).map_err(|e| TraceDecayError::Config {
        message: format!("cannot read {}: {e}", path.display()),
    })?;
    JsonConfigDialect::Jsonc.parse_for_edit(path, &contents)
}

/// Returns the VS Code user data directory, platform-specific.
///
/// Canonical copy lives in `tracedecay_sessions::host_ports` (lower in the
/// dependency graph).
pub use tracedecay_sessions::host_ports::vscode_data_dir;

/// Returns the platform-specific VS Code Insiders data directory.
pub fn vscode_insiders_data_dir(home: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        home.join("Library/Application Support/Code - Insiders")
    }
    #[cfg(target_os = "linux")]
    {
        home.join(".config/Code - Insiders")
    }
    #[cfg(target_os = "windows")]
    {
        if let Ok(appdata) = std::env::var("APPDATA") {
            let appdata_path = PathBuf::from(&appdata);
            if appdata_path.starts_with(home) {
                return appdata_path.join("Code - Insiders");
            }
        }
        home.join("AppData/Roaming/Code - Insiders")
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux", target_os = "windows")))]
    {
        home.join(".config/Code - Insiders")
    }
}

/// Returns the GitHub Copilot CLI config directory.
pub fn copilot_cli_dir(home: &Path) -> PathBuf {
    home.join(".copilot")
}

/// Returns the Kiro IDE user data directory (VS Code-style layout).
///
/// Canonical copy lives in `tracedecay_sessions::host_ports` (lower in the
/// dependency graph).
pub use tracedecay_sessions::host_ports::kiro_data_dir;

/// Load a TOML file as a document.
///
/// Returns an empty table when the file does not exist. When the file exists
/// but cannot be parsed as a TOML document, returns a [`TraceDecayError::Config`]
/// so callers do not silently overwrite the user's data (see issue #63).
#[hotpath::measure(label = "agent_hosts.agents.config.load_toml")]
pub fn load_toml_file(path: &Path) -> Result<toml::Value> {
    if !path.exists() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }
    let contents = std::fs::read_to_string(path).map_err(|e| TraceDecayError::Config {
        message: format!("failed to read {}: {e}", path.display()),
    })?;
    parse_toml_config(path, &contents)
}

/// Strict parse of already-observed TOML config contents for a write path;
/// blank content is an empty document.
fn parse_toml_config(path: &Path, contents: &str) -> Result<toml::Value> {
    if contents.trim().is_empty() {
        return Ok(toml::Value::Table(toml::map::Map::new()));
    }
    // NOTE: `str.parse::<toml::Value>()` parses a single TOML value in toml v1,
    // not a document — using it here would treat any well-formed config.toml as
    // unparseable and silently drop its contents. Use `toml::from_str` instead.
    let table: toml::Table = toml::from_str(contents).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to parse {} as TOML: {e}. Refusing to overwrite — fix the file or remove it manually.",
            path.display()
        ),
    })?;
    Ok(toml::Value::Table(table))
}

/// Copy `path` to `<path>.bak` if it exists. Used before overwriting a user
/// config so an unexpected change is recoverable (issue #63).
fn backup_file(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }
    let mut backup = path.as_os_str().to_owned();
    backup.push(".bak");
    let backup = std::path::PathBuf::from(backup);
    std::fs::copy(path, &backup).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to back up {} to {}: {e}",
            path.display(),
            backup.display()
        ),
    })?;
    eprintln!(
        "\x1b[32m✔\x1b[0m Backed up {} to {}",
        path.display(),
        backup.display()
    );
    Ok(())
}

/// Write a TOML value to a file, backing up any existing file first.
#[hotpath::measure(label = "agent_hosts.agents.config.write_toml")]
pub fn write_toml_file(path: &Path, value: &toml::Value) -> Result<()> {
    backup_file(path)?;
    let contents = toml::to_string_pretty(value).unwrap_or_else(|_| String::new());
    std::fs::write(path, contents).map_err(|e| TraceDecayError::Config {
        message: format!("failed to write {}: {e}", path.display()),
    })?;
    eprintln!("\x1b[32m✔\x1b[0m Wrote {}", path.display());
    Ok(())
}

// ---------------------------------------------------------------------------
// Git post-commit hook
// ---------------------------------------------------------------------------
