use std::fs::File;
#[cfg(not(windows))]
use std::fs::OpenOptions;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fs2::FileExt;
use serde::{Deserialize, Deserializer, Serialize};
use tracedecay_domain::{BrainId, UserProfileId};
use tracedecay_runtime_core::path_safety::{
    canonicalize_existing_prefix, collapse_relative_components,
};

use crate::errors::{Result, TraceDecayError};

use super::profile_identity::LocalProfileIdentityAuthorityV1;
use super::transport::DaemonEndpoint;

#[cfg(windows)]
mod windows_acl;

const LOCK_FILE: &str = "daemon-authority.lock";
const RECORD_FILE: &str = "daemon-authority.json";
#[cfg(windows)]
const AUTHORITY_DIRECTORY: &str = "daemon-authority";

fn deserialize_endpoint<'de, D>(deserializer: D) -> std::result::Result<DaemonEndpoint, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum EndpointRecord {
        Current(DaemonEndpoint),
        Legacy(PathBuf),
    }

    match EndpointRecord::deserialize(deserializer)? {
        EndpointRecord::Current(endpoint) => Ok(endpoint),
        EndpointRecord::Legacy(path) => {
            #[cfg(unix)]
            {
                Ok(DaemonEndpoint::Unix(path))
            }
            #[cfg(not(unix))]
            {
                Err(serde::de::Error::custom(format!(
                    "legacy Unix daemon endpoint '{}' is unsupported on this platform",
                    path.display()
                )))
            }
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(super) struct DaemonAuthorityRecord {
    pub(super) pid: u32,
    pub(super) process_run_id: String,
    pub(super) started_at_unix_secs: i64,
    pub(super) epoch: u64,
    pub(super) version: String,
    #[serde(alias = "socket_path", deserialize_with = "deserialize_endpoint")]
    pub(super) endpoint: DaemonEndpoint,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) http_application_endpoint: Option<SocketAddr>,
    pub(super) auth_token: String,
    pub(super) profile_root: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) brain_id: Option<BrainId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(super) profile_id: Option<UserProfileId>,
}

#[derive(Debug)]
pub(super) struct DaemonAuthority {
    _lock: File,
    record_path: PathBuf,
    record: DaemonAuthorityRecord,
    profile_identity: LocalProfileIdentityAuthorityV1,
    endpoint_bound: bool,
}

impl DaemonAuthority {
    pub(super) fn acquire(
        profile_root: &Path,
        endpoint: &DaemonEndpoint,
        version: &str,
    ) -> Result<Self> {
        #[cfg(windows)]
        let _ = validate_existing_profile_root(profile_root)?;
        let profile_root = canonical_identity_path(profile_root)?;
        std::fs::create_dir_all(&profile_root)
            .map_err(|error| config_io("create", &profile_root, &error))?;
        let authority_root = authority_state_root(&profile_root);
        #[cfg(windows)]
        windows_acl::create_private_directory(&authority_root)
            .map_err(|error| config_io("create private", &authority_root, &error))?;
        restrict_directory(&authority_root)?;

        let lock_path = authority_root.join(LOCK_FILE);
        let mut lock = open_private_lock(&lock_path)?;
        if let Err(error) = lock.try_lock_exclusive() {
            if !is_lock_contended(&error) {
                return Err(config_io("lock", &lock_path, &error));
            }
            let record = read_record_if_present(&authority_root.join(RECORD_FILE))
                .ok()
                .flatten()
                .map(|record| {
                    format!(
                        " (pid {}, epoch {}, endpoint '{}')",
                        record.pid, record.epoch, record.endpoint
                    )
                })
                .unwrap_or_default();
            return Err(TraceDecayError::Config {
                message: format!(
                    "daemon authority for profile '{}' is already held{record}: {error}",
                    profile_root.display()
                ),
            });
        }

        let record_path = authority_root.join(RECORD_FILE);
        let prior_record = read_record_if_present(&record_path)?;
        let pinned_identity = match prior_record.as_ref() {
            Some(record) => match (&record.brain_id, &record.profile_id) {
                (Some(brain_id), Some(profile_id)) => Some((brain_id, profile_id)),
                (None, None) => None,
                _ => {
                    return Err(TraceDecayError::Config {
                        message: format!(
                            "daemon authority record '{}' has an incomplete pinned profile identity",
                            record_path.display()
                        ),
                    });
                }
            },
            None => None,
        };
        let profile_identity =
            super::profile_identity::load_or_create_pinned(&profile_root, pinned_identity)?;
        let prior_epoch = prior_record.as_ref().map_or(0, |record| record.epoch);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let record = DaemonAuthorityRecord {
            pid: std::process::id(),
            process_run_id: crate::runtime_identity::process_run_id().to_string(),
            started_at_unix_secs: i64::try_from(now.as_secs()).unwrap_or(i64::MAX),
            epoch: prior_epoch.saturating_add(1),
            version: version.to_string(),
            endpoint: canonical_endpoint(endpoint)?,
            http_application_endpoint: None,
            auth_token: new_auth_token()?,
            profile_root,
            brain_id: Some(profile_identity.brain_id().clone()),
            profile_id: Some(profile_identity.profile_id().clone()),
        };
        write_record(&record_path, &record)?;
        lock.set_len(0)
            .map_err(|error| config_io("truncate", &lock_path, &error))?;
        lock.seek(SeekFrom::Start(0))
            .map_err(|error| config_io("seek", &lock_path, &error))?;
        writeln!(
            lock,
            "pid={} run={} epoch={}",
            record.pid, record.process_run_id, record.epoch
        )
        .map_err(|error| config_io("write", &lock_path, &error))?;
        lock.sync_data()
            .map_err(|error| config_io("sync", &lock_path, &error))?;

        Ok(Self {
            _lock: lock,
            record_path,
            record,
            profile_identity,
            endpoint_bound: false,
        })
    }

    pub(super) fn record(&self) -> &DaemonAuthorityRecord {
        &self.record
    }

    pub(super) fn endpoint(&self) -> &DaemonEndpoint {
        &self.record.endpoint
    }

    pub(super) fn auth_token(&self) -> &str {
        &self.record.auth_token
    }

    pub(super) fn profile_identity(&self) -> &LocalProfileIdentityAuthorityV1 {
        &self.profile_identity
    }

    pub(super) fn publish_endpoint(&mut self, endpoint: &DaemonEndpoint) -> Result<()> {
        self.record.endpoint = canonical_endpoint(endpoint)?;
        write_record(&self.record_path, &self.record)?;
        self.endpoint_bound = true;
        Ok(())
    }

    pub(super) fn publish_http_application_endpoint(&mut self, endpoint: SocketAddr) -> Result<()> {
        if !endpoint.ip().is_loopback() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "daemon HTTP application endpoint must be loopback (got '{endpoint}')"
                ),
            });
        }
        self.record.http_application_endpoint = Some(endpoint);
        write_record(&self.record_path, &self.record)
    }

    pub(super) fn ensure_current(&self) -> Result<()> {
        let current = read_record_if_present(&self.record_path)?;
        if current.as_ref().is_some_and(|record| {
            record.epoch == self.record.epoch
                && record.process_run_id == self.record.process_run_id
                && record.profile_root == self.record.profile_root
                && record.endpoint == self.record.endpoint
                && record.http_application_endpoint == self.record.http_application_endpoint
                && record.auth_token == self.record.auth_token
        }) {
            return Ok(());
        }
        Err(TraceDecayError::Config {
            message: format!(
                "daemon authority epoch {} for profile '{}' is no longer current",
                self.record.epoch,
                self.record.profile_root.display()
            ),
        })
    }

    #[cfg(all(test, unix))]
    pub(super) fn mark_endpoint_bound(&mut self) {
        self.endpoint_bound = true;
    }

    // Preserve the fallible cross-platform cleanup contract; Unix removal can fail.
    #[cfg_attr(not(unix), allow(clippy::unnecessary_wraps))]
    pub(super) fn cleanup_owned_endpoint(&mut self) -> Result<()> {
        if !self.endpoint_bound || self.ensure_current().is_err() {
            return Ok(());
        }
        match &self.record.endpoint {
            #[cfg(unix)]
            DaemonEndpoint::Unix(path) => remove_if_present(path)?,
            DaemonEndpoint::Loopback(_) => {}
        }
        self.endpoint_bound = false;
        Ok(())
    }
}

impl Drop for DaemonAuthority {
    fn drop(&mut self) {
        let _ = self.cleanup_owned_endpoint();
    }
}

pub(super) fn current_record(profile_root: &Path) -> Result<Option<DaemonAuthorityRecord>> {
    #[cfg(windows)]
    if !validate_existing_profile_root(profile_root)? {
        return Ok(None);
    }
    let profile_root = canonical_identity_path(profile_root)?;
    let authority_root = authority_state_root(&profile_root);
    #[cfg(windows)]
    match windows_acl::validate_private_directory(&authority_root) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(config_io("validate private", &authority_root, &error)),
    }
    read_record_if_present(&authority_root.join(RECORD_FILE))
}

#[cfg(windows)]
fn validate_existing_profile_root(path: &Path) -> Result<bool> {
    match windows_acl::validate_directory_path(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(config_io("validate existing profile root", path, &error)),
    }
}

fn authority_state_root(profile_root: &Path) -> PathBuf {
    #[cfg(windows)]
    {
        profile_root.join(AUTHORITY_DIRECTORY)
    }
    #[cfg(not(windows))]
    {
        profile_root.to_path_buf()
    }
}

/// Absolutizes `path`, canonicalizes through its deepest existing ancestor,
/// then collapses `.`/`..` so the daemon's recorded identity paths are
/// comparable byte-for-byte.
pub(super) fn canonical_identity_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| config_io("resolve", path, &error))?
            .join(path)
    };
    let canonical =
        canonicalize_existing_prefix(&absolute).ok_or_else(|| TraceDecayError::Config {
            message: format!("failed to resolve identity path '{}'", path.display()),
        })?;
    Ok(collapse_relative_components(&canonical))
}

fn canonical_endpoint(endpoint: &DaemonEndpoint) -> Result<DaemonEndpoint> {
    match endpoint {
        #[cfg(unix)]
        DaemonEndpoint::Unix(path) => {
            let file_name = path.file_name().ok_or_else(|| TraceDecayError::Config {
                message: format!("daemon socket path '{}' has no file name", path.display()),
            })?;
            let parent = path.parent().unwrap_or_else(|| Path::new("."));
            Ok(DaemonEndpoint::Unix(
                canonical_identity_path(parent)?.join(file_name),
            ))
        }
        DaemonEndpoint::Loopback(address) => DaemonEndpoint::loopback(*address),
    }
}

fn read_record_if_present(path: &Path) -> Result<Option<DaemonAuthorityRecord>> {
    #[cfg(windows)]
    let mut file = match windows_acl::open_private_file(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(config_io("secure before reading", path, &error)),
    };
    #[cfg(not(windows))]
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(config_io("open", path, &error)),
    };
    let mut contents = String::new();
    file.read_to_string(&mut contents)
        .map_err(|error| config_io("read", path, &error))?;
    serde_json::from_str(&contents)
        .map(Some)
        .map_err(|error| TraceDecayError::Config {
            message: format!(
                "invalid daemon authority record '{}': {error}",
                path.display()
            ),
        })
}

fn write_record(path: &Path, record: &DaemonAuthorityRecord) -> Result<()> {
    let temporary = path.with_extension(format!("json.{}.tmp", record.process_run_id));
    let bytes = serde_json::to_vec_pretty(record).map_err(|error| TraceDecayError::Config {
        message: format!("failed to encode daemon authority record: {error}"),
    })?;
    crate::db::DatabaseAuthority::publish_record_atomically(
        &temporary,
        path,
        &bytes,
        "daemon authority record",
    )?;
    restrict_file(path)
}

fn open_private_lock(path: &Path) -> Result<File> {
    #[cfg(windows)]
    {
        windows_acl::open_or_create_private_lock_file(path)
            .map_err(|error| config_io("open private lock", path, &error))
    }

    #[cfg(not(windows))]
    {
        let mut options = OpenOptions::new();
        options.create(true).read(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(path)
            .map_err(|error| config_io("open", path, &error))?;
        restrict_file(path)?;
        Ok(file)
    }
}

#[cfg(unix)]
fn restrict_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .map_err(|error| config_io("restrict", path, &error))
}

#[cfg(windows)]
fn restrict_directory(path: &Path) -> Result<()> {
    windows_acl::validate_private_directory(path)
        .map_err(|error| config_io("validate private", path, &error))
}

#[cfg(all(not(unix), not(windows)))]
#[allow(clippy::unnecessary_wraps)] // Preserve parity with Unix permission enforcement.
fn restrict_directory(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn restrict_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|error| config_io("restrict", path, &error))
}

#[cfg(windows)]
fn restrict_file(path: &Path) -> Result<()> {
    windows_acl::validate_private_file(path)
        .map_err(|error| config_io("validate private", path, &error))
}

#[cfg(all(not(unix), not(windows)))]
#[allow(clippy::unnecessary_wraps)] // Preserve parity with Unix permission enforcement.
fn restrict_file(_path: &Path) -> Result<()> {
    Ok(())
}

fn is_lock_contended(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::WouldBlock {
        return true;
    }
    #[cfg(windows)]
    {
        error.raw_os_error() == Some(33)
    }
    #[cfg(not(windows))]
    false
}

fn new_auth_token() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|error| TraceDecayError::Config {
        message: format!("failed to generate daemon authentication token: {error}"),
    })?;
    Ok(hex::encode(bytes))
}

#[cfg(unix)]
fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(config_io("remove", path, &error)),
    }
}

fn config_io(operation: &str, path: &Path, error: &std::io::Error) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!("failed to {operation} '{}': {error}", path.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn test_endpoint(profile: &Path) -> DaemonEndpoint {
        DaemonEndpoint::Unix(profile.join("daemon.sock"))
    }

    #[cfg(not(unix))]
    fn test_endpoint(_profile: &Path) -> DaemonEndpoint {
        super::super::transport::default_loopback_endpoint()
    }

    #[test]
    fn stale_record_does_not_block_and_epoch_advances() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        let endpoint = test_endpoint(&profile);
        let mut first = DaemonAuthority::acquire(&profile, &endpoint, "test").unwrap();
        let first_epoch = first.record().epoch;
        let first_profile_identity = first.profile_identity().clone();
        assert_eq!(
            first.record().brain_id.as_ref(),
            Some(first_profile_identity.brain_id())
        );
        assert_eq!(
            first.record().profile_id.as_ref(),
            Some(first_profile_identity.profile_id())
        );
        first.endpoint_bound = false;
        drop(first);

        let second = DaemonAuthority::acquire(&profile, &endpoint, "test").unwrap();
        assert_eq!(second.record().epoch, first_epoch + 1);
        assert_eq!(second.profile_identity(), &first_profile_identity);
        assert_eq!(second.auth_token().len(), 64);
        assert!(
            second
                .auth_token()
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit())
        );
    }

    #[test]
    fn pinned_profile_identity_loss_blocks_daemon_reelection() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        let endpoint = test_endpoint(&profile);
        let first = DaemonAuthority::acquire(&profile, &endpoint, "test").unwrap();
        drop(first);
        std::fs::remove_file(profile.join(crate::storage::PROFILE_IDENTITY_FILENAME)).unwrap();

        let error = DaemonAuthority::acquire(&profile, &endpoint, "test").unwrap_err();

        assert!(
            error
                .to_string()
                .contains("missing after its identity was pinned")
        );
    }

    #[test]
    fn contended_lease_does_not_replace_the_live_record() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        let endpoint = test_endpoint(&profile);
        let first = DaemonAuthority::acquire(&profile, &endpoint, "first").unwrap();
        let record_path = first.record_path.clone();
        let live = read_record_if_present(&record_path).unwrap().unwrap();

        let contender = DaemonAuthority::acquire(&profile, &endpoint, "contender");

        assert!(contender.is_err());
        #[cfg(windows)]
        assert!(contender.unwrap_err().to_string().contains("already held"));
        assert_eq!(read_record_if_present(&record_path).unwrap(), Some(live));
        drop(first);
    }

    #[test]
    fn record_replacement_is_complete_and_removes_the_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        let endpoint = test_endpoint(&profile);
        let authority = DaemonAuthority::acquire(&profile, &endpoint, "first").unwrap();
        let mut successor = authority.record().clone();
        successor.epoch += 1;
        successor.version = "successor".to_string();
        let temporary = authority
            .record_path
            .with_extension(format!("json.{}.tmp", successor.process_run_id));

        write_record(&authority.record_path, &successor).unwrap();

        assert_eq!(
            read_record_if_present(&authority.record_path).unwrap(),
            Some(successor)
        );
        assert!(!temporary.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            assert_eq!(
                std::fs::metadata(&profile).unwrap().permissions().mode() & 0o777,
                0o700
            );
            for path in [&authority.record_path, &profile.join(LOCK_FILE)] {
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
        }
    }

    #[cfg(windows)]
    #[test]
    fn acl_failure_prevents_authority_record_publication() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let authority_root = authority_state_root(&profile);
        windows_acl::create_private_directory(&authority_root).unwrap();
        std::fs::create_dir(authority_root.join(LOCK_FILE)).unwrap();
        let endpoint = test_endpoint(&profile);

        let error = DaemonAuthority::acquire(&profile, &endpoint, "test").unwrap_err();

        assert!(error.to_string().contains(LOCK_FILE));
        assert!(!authority_root.join(RECORD_FILE).exists());
    }

    #[cfg(windows)]
    #[test]
    fn authority_state_isolated_without_rewriting_profile_root() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let sentinel = profile.join("unrelated.txt");
        std::fs::write(&sentinel, b"preserve").unwrap();
        assert!(windows_acl::validate_private_directory(&profile).is_err());
        let endpoint = test_endpoint(&profile);

        let authority = DaemonAuthority::acquire(&profile, &endpoint, "test").unwrap();

        assert_eq!(std::fs::read(&sentinel).unwrap(), b"preserve");
        assert!(windows_acl::validate_private_directory(&profile).is_err());
        let authority_root = authority_state_root(&profile.canonicalize().unwrap());
        windows_acl::validate_private_directory(&authority_root).unwrap();
        windows_acl::validate_private_file(&authority.record_path).unwrap();
        assert_eq!(
            authority.record_path.parent(),
            Some(authority_root.as_path())
        );
        assert!(!profile.join(RECORD_FILE).exists());
    }

    #[test]
    fn published_loopback_endpoint_preserves_the_elected_secret() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        let requested = test_endpoint(&profile);
        let mut authority = DaemonAuthority::acquire(&profile, &requested, "test").unwrap();
        let auth_token = authority.auth_token().to_string();
        let concrete = DaemonEndpoint::parse("tcp://127.0.0.1:43123").unwrap();

        authority.publish_endpoint(&concrete).unwrap();

        let published = current_record(&profile).unwrap().unwrap();
        assert_eq!(published.endpoint, concrete);
        assert_eq!(published.auth_token, auth_token);
        assert!(authority.ensure_current().is_ok());
    }

    #[test]
    fn published_http_application_endpoint_is_private_discovery_state() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        let requested = test_endpoint(&profile);
        let mut authority = DaemonAuthority::acquire(&profile, &requested, "test").unwrap();
        let auth_token = authority.auth_token().to_string();
        let endpoint = "127.0.0.1:43124".parse().unwrap();

        authority
            .publish_http_application_endpoint(endpoint)
            .unwrap();

        let published = current_record(&profile).unwrap().unwrap();
        assert_eq!(published.http_application_endpoint, Some(endpoint));
        assert_eq!(published.auth_token, auth_token);
        assert!(authority.ensure_current().is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn legacy_socket_path_record_is_accepted_by_current_reader() {
        #[derive(Serialize)]
        struct LegacySocketRecord {
            pid: u32,
            process_run_id: String,
            started_at_unix_secs: i64,
            epoch: u64,
            version: String,
            socket_path: PathBuf,
            auth_token: String,
            profile_root: PathBuf,
        }

        let temp = tempfile::tempdir().unwrap();
        let profile_root = temp.path().join("profile");
        std::fs::create_dir_all(&profile_root).unwrap();
        let record_path = profile_root.join(RECORD_FILE);
        let socket_path = profile_root.join("daemon.sock");
        let legacy = LegacySocketRecord {
            pid: 42,
            process_run_id: "legacy-run".to_string(),
            started_at_unix_secs: 1,
            epoch: 3,
            version: "legacy".to_string(),
            socket_path: socket_path.clone(),
            auth_token: "a".repeat(64),
            profile_root: profile_root.clone(),
        };
        std::fs::write(&record_path, serde_json::to_vec(&legacy).unwrap()).unwrap();

        let decoded = read_record_if_present(&record_path).unwrap().unwrap();
        assert_eq!(decoded.endpoint, DaemonEndpoint::Unix(socket_path));
        assert_eq!(decoded.auth_token, "a".repeat(64));
    }

    #[test]
    fn current_endpoint_record_fails_closed_for_legacy_reader() {
        #[allow(dead_code)]
        #[derive(Deserialize)]
        struct LegacySocketRecord {
            pid: u32,
            process_run_id: String,
            started_at_unix_secs: i64,
            epoch: u64,
            version: String,
            socket_path: PathBuf,
            profile_root: PathBuf,
        }

        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        let endpoint = test_endpoint(&profile);
        let authority = DaemonAuthority::acquire(&profile, &endpoint, "current").unwrap();
        let encoded = serde_json::to_string(authority.record()).unwrap();

        assert!(serde_json::from_str::<LegacySocketRecord>(&encoded).is_err());
    }

    #[test]
    fn stale_endpoint_or_token_is_rejected_by_the_elected_owner() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        let endpoint = test_endpoint(&profile);
        let authority = DaemonAuthority::acquire(&profile, &endpoint, "test").unwrap();
        let mut stale = authority.record().clone();
        stale.auth_token = "0".repeat(64);
        write_record(&authority.record_path, &stale).unwrap();

        assert!(authority.ensure_current().is_err());
    }

    #[cfg(unix)]
    #[test]
    fn canonical_profile_alias_contends_on_one_kernel_lease() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let alias = temp.path().join("alias");
        std::os::unix::fs::symlink(&profile, &alias).unwrap();
        let first = DaemonAuthority::acquire(
            &profile,
            &DaemonEndpoint::Unix(profile.join("daemon.sock")),
            "test",
        )
        .unwrap();
        let second = DaemonAuthority::acquire(
            &alias,
            &DaemonEndpoint::Unix(alias.join("daemon.sock")),
            "test",
        );
        assert!(second.is_err());
        drop(first);
    }

    #[cfg(unix)]
    #[test]
    fn stale_epoch_cannot_remove_successor_socket() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        let socket = profile.join("daemon.sock");
        let mut authority =
            DaemonAuthority::acquire(&profile, &DaemonEndpoint::Unix(socket.clone()), "test")
                .unwrap();
        std::fs::write(&socket, b"successor").unwrap();
        authority.mark_endpoint_bound();
        let mut successor = authority.record().clone();
        successor.epoch += 1;
        successor.process_run_id.push_str("-successor");
        write_record(&authority.record_path, &successor).unwrap();

        authority.cleanup_owned_endpoint().unwrap();
        assert!(socket.exists());
    }

    #[cfg(unix)]
    #[test]
    fn socket_identity_never_follows_the_socket_leaf_symlink() {
        let temp = tempfile::tempdir().unwrap();
        let profile = temp.path().join("profile");
        std::fs::create_dir_all(&profile).unwrap();
        let target = temp.path().join("unrelated");
        std::fs::write(&target, b"keep").unwrap();
        let socket = profile.join("daemon.sock");
        std::os::unix::fs::symlink(&target, &socket).unwrap();

        let authority =
            DaemonAuthority::acquire(&profile, &DaemonEndpoint::Unix(socket.clone()), "test")
                .unwrap();

        let canonical_socket = profile.canonicalize().unwrap().join("daemon.sock");
        assert_eq!(
            authority.endpoint(),
            &DaemonEndpoint::Unix(canonical_socket)
        );
        assert_eq!(std::fs::read(&target).unwrap(), b"keep");
    }
}
