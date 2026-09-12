//! Self-update for the tracedecay binary.
//!
//! Direct installs use GitHub release assets: the platform archive is
//! verified against the release's `SHA256SUMS`, every required release member
//! (the executable plus the runtime companions the executable resolves beside
//! itself) is staged in an attempt-owned scratch directory, and only then is
//! the bundle published around the running executable. Installations owned by
//! a package manager (Homebrew, Scoop) are upgraded by that manager and never
//! written to directly; see [`UpgradeSource`].
//! Beta and stable are separate channels — a beta build only sees beta
//! releases and vice versa.

use std::fmt;
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitStatus, Output};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::cloud::{self, InstallMethod};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_runtime_core::git::{GitCommandBounds, GitCommandError, bounded_command_output};
use tracedecay_session_memory::user_config::UserConfig;

const GITHUB_REPO: &str = "ScriptedAlchemy/tracedecay";

/// Kind of a required release-archive member.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ReleaseMemberKind {
    /// The `tracedecay` entry point; published last, mode `0755`.
    Executable,
    /// A runtime file the executable resolves beside itself (`$ORIGIN`);
    /// published before the entry point, mode `0644`.
    Companion,
}

/// One member every release archive for this platform must carry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ReleaseMember {
    name: &'static str,
    kind: ReleaseMemberKind,
}

impl ReleaseMember {
    #[cfg(unix)]
    const fn mode(self) -> u32 {
        match self.kind {
            ReleaseMemberKind::Executable => 0o755,
            ReleaseMemberKind::Companion => 0o644,
        }
    }
}

const EXECUTABLE_MEMBER: ReleaseMember = ReleaseMember {
    name: if cfg!(windows) {
        "tracedecay.exe"
    } else {
        "tracedecay"
    },
    kind: ReleaseMemberKind::Executable,
};

/// Runtime companions the Linux release archives carry beside the executable:
/// the ONNX Runtime the binary is linked against with an `$ORIGIN` rpath and
/// that library's redistribution notices. These are the `entry_name`s the
/// Linux targets in `.github/release-targets.json` declare; the unit test
/// `required_members_match_the_release_target_manifest` pins the two together
/// so the installer and the packaging step cannot disagree about what a
/// complete release is. Other platforms ship the executable alone.
#[cfg(target_os = "linux")]
const RUNTIME_COMPANIONS: &[&str] = &[
    "libonnxruntime.so.1",
    "onnxruntime-LICENSE",
    "onnxruntime-ThirdPartyNotices.txt",
];
#[cfg(not(target_os = "linux"))]
const RUNTIME_COMPANIONS: &[&str] = &[];

/// Every member a release archive for this platform must contain.
fn required_members() -> Vec<ReleaseMember> {
    std::iter::once(EXECUTABLE_MEMBER)
        .chain(RUNTIME_COMPANIONS.iter().map(|name| ReleaseMember {
            name,
            kind: ReleaseMemberKind::Companion,
        }))
        .collect()
}

/// A verified release whose required members sit in an attempt-owned scratch
/// directory. The directory (and everything staged in it) is removed when the
/// value drops, on every success and failure path.
#[derive(Debug)]
struct StagedRelease {
    scratch: TempDir,
    members: Vec<ReleaseMember>,
}

impl StagedRelease {
    fn path_of(&self, member: ReleaseMember) -> PathBuf {
        self.scratch.path().join(member.name)
    }

    fn executable(&self) -> PathBuf {
        self.path_of(EXECUTABLE_MEMBER)
    }

    fn companions(&self) -> impl Iterator<Item = ReleaseMember> + '_ {
        self.members
            .iter()
            .copied()
            .filter(|member| member.kind == ReleaseMemberKind::Companion)
    }
}

// Asset-naming and platform helpers live in `crate::cloud` so the version-
// detection path can use the same naming convention to filter out releases
// whose CI hasn't finished uploading the current platform's binary yet.
use crate::cloud::asset_name;
#[cfg(test)]
use crate::cloud::current_platform;

/// The GitHub release tag for a given version.
fn release_tag(version: &str) -> String {
    format!("v{version}")
}

fn io_err(msg: &str) -> impl Fn(std::io::Error) -> TraceDecayError + '_ {
    move |e| TraceDecayError::Config {
        message: format!("{msg}: {e}"),
    }
}

/// The platform archive and checksum manifest of one GitHub release, with the
/// byte sizes the release metadata advertises for each. Those sizes are the
/// download ceilings: a body that runs past its advertised size, or ends
/// short of it, is not the published asset.
#[derive(Debug)]
struct ReleaseDownload {
    asset_name: String,
    asset_url: String,
    asset_size: u64,
    checksums_url: String,
    checksums_size: u64,
}

/// Resolves both the platform archive and its checksum manifest from one
/// GitHub release. An archive without `SHA256SUMS` is not installable.
#[hotpath::measure(label = "cli.upgrade.fetch_release")]
fn fetch_release_download(tag: &str, asset_name: &str) -> Result<ReleaseDownload> {
    #[derive(serde::Deserialize)]
    struct Asset {
        name: String,
        browser_download_url: String,
        size: u64,
    }
    #[derive(serde::Deserialize)]
    struct Release {
        assets: Vec<Asset>,
    }

    let url = format!("https://api.github.com/repos/{GITHUB_REPO}/releases/tags/{tag}");
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .build()
        .into();

    let release: Release = agent
        .get(&url)
        .header("User-Agent", "tracedecay")
        .call()
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to reach GitHub: {e}"),
        })?
        .body_mut()
        .read_json()
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to parse release info: {e}"),
        })?;

    let archive = release
        .assets
        .iter()
        .find(|asset| asset.name == asset_name)
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "release {tag} exists but asset '{asset_name}' is not yet available.\n  \
                 CI build may still be in progress — try again in a few minutes.\n  \
                 https://github.com/{GITHUB_REPO}/releases/tag/{tag}",
            ),
        })?;
    let checksums = release
        .assets
        .iter()
        .find(|asset| asset.name == "SHA256SUMS")
        .ok_or_else(|| TraceDecayError::Config {
            message: format!(
                "release {tag} has asset '{}' but no SHA256SUMS; refusing an unverified upgrade",
                archive.name
            ),
        })?;
    Ok(ReleaseDownload {
        asset_name: archive.name.clone(),
        asset_url: archive.browser_download_url.clone(),
        asset_size: archive.size,
        checksums_url: checksums.browser_download_url.clone(),
        checksums_size: checksums.size,
    })
}

/// Streams `url` into `sink`, requiring exactly `advertised_len` bytes: the
/// body is cut off the moment it runs past the size the release metadata
/// advertises, and a body that ends short of it is a truncated download.
/// Only a fixed transfer buffer is ever held in memory.
fn download_exact(
    agent: &ureq::Agent,
    url: &str,
    advertised_len: u64,
    description: &str,
    sink: &mut impl Write,
) -> Result<()> {
    let mut response = agent
        .get(url)
        .header("User-Agent", "tracedecay")
        .call()
        .map_err(|e| TraceDecayError::Config {
            message: format!("{description} download failed: {e}"),
        })?;
    let mut body = response.body_mut().as_reader();
    let mut buffer = [0u8; 64 * 1024];
    let mut received: u64 = 0;
    loop {
        let count = body
            .read(&mut buffer)
            .map_err(|error| TraceDecayError::Config {
                message: format!("{description} download read failed: {error}"),
            })?;
        if count == 0 {
            break;
        }
        received += count as u64;
        if received > advertised_len {
            return Err(TraceDecayError::Config {
                message: format!(
                    "{description} exceeds the {advertised_len} bytes the release advertises; \
                     refusing an unverified download"
                ),
            });
        }
        sink.write_all(&buffer[..count])
            .map_err(io_err("cannot write downloaded bytes"))?;
    }
    if received != advertised_len {
        return Err(TraceDecayError::Config {
            message: format!(
                "{description} ended after {received} of the {advertised_len} bytes the release \
                 advertises"
            ),
        });
    }
    sink.flush()
        .map_err(io_err("cannot flush downloaded bytes"))
}

/// Writer that hashes every byte it forwards, so the archive digest is taken
/// over exactly the bytes that reached the staging file.
struct DigestingWriter<W> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> Write for DigestingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.inner.write(buf)?;
        self.hasher.update(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

/// Downloads the checksum manifest, bounded by its advertised size.
fn download_manifest(agent: &ureq::Agent, download: &ReleaseDownload) -> Result<Vec<u8>> {
    let mut manifest = Vec::new();
    download_exact(
        agent,
        &download.checksums_url,
        download.checksums_size,
        "checksum manifest",
        &mut manifest,
    )?;
    Ok(manifest)
}

/// Streams the release archive into `archive`, bounded by its advertised
/// size, and returns the lowercase hex SHA-256 of the bytes written.
fn stream_archive(
    agent: &ureq::Agent,
    download: &ReleaseDownload,
    archive: &mut File,
) -> Result<String> {
    let mut sink = DigestingWriter {
        inner: archive,
        hasher: Sha256::new(),
    };
    download_exact(
        agent,
        &download.asset_url,
        download.asset_size,
        "release archive",
        &mut sink,
    )?;
    Ok(hex::encode(sink.hasher.finalize()))
}

fn expected_sha256(manifest: &[u8], asset_name: &str) -> Result<String> {
    let text = std::str::from_utf8(manifest).map_err(|e| TraceDecayError::Config {
        message: format!("SHA256SUMS is not valid UTF-8: {e}"),
    })?;
    let mut matches = text.lines().filter_map(|line| {
        let mut fields = line.split_whitespace();
        let digest = fields.next()?;
        let name = fields.next()?.trim_start_matches('*');
        (fields.next().is_none() && name == asset_name).then_some(digest)
    });
    let digest = matches.next().ok_or_else(|| TraceDecayError::Config {
        message: format!("SHA256SUMS has no entry for {asset_name}"),
    })?;
    if matches.next().is_some() {
        return Err(TraceDecayError::Config {
            message: format!("SHA256SUMS has duplicate entries for {asset_name}"),
        });
    }
    if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(TraceDecayError::Config {
            message: format!("SHA256SUMS has an invalid digest for {asset_name}"),
        });
    }
    Ok(digest.to_ascii_lowercase())
}

fn verify_sha256(actual: &str, expected: &str, asset_name: &str) -> Result<()> {
    if actual == expected {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: format!("checksum mismatch for {asset_name}: expected {expected}, got {actual}"),
    })
}

/// Downloads and verifies the archive, then stages every required release
/// member in an attempt-owned scratch directory. Nothing is published here.
#[hotpath::measure(label = "cli.upgrade.download_and_stage")]
fn download_and_stage(
    download: &ReleaseDownload,
    members: &[ReleaseMember],
) -> Result<StagedRelease> {
    let scratch = tempfile::Builder::new()
        .prefix("tracedecay-upgrade-")
        .tempdir()
        .map_err(io_err("cannot create upgrade staging directory"))?;
    stage_release_in(scratch, download, members)
}

/// Streams the archive into an exclusively created file inside `scratch`,
/// hashing as it lands, verifies the whole-archive digest against the
/// release's checksum manifest, and only then rewinds that same file for
/// extraction. `scratch` is owned by this attempt: it is removed with
/// everything in it whenever this returns an error.
fn stage_release_in(
    scratch: TempDir,
    download: &ReleaseDownload,
    members: &[ReleaseMember],
) -> Result<StagedRelease> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_mins(5)))
        .build()
        .into();

    eprint!("  Downloading...");

    let manifest = download_manifest(&agent, download)?;
    let expected = expected_sha256(&manifest, &download.asset_name)?;
    let mut archive = File::options()
        .read(true)
        .write(true)
        .create_new(true)
        .open(scratch.path().join("archive"))
        .map_err(io_err("cannot create staged archive"))?;
    let actual = stream_archive(&agent, download, &mut archive)?;

    eprintln!(" ({:.1} MiB)", download.asset_size as f64 / 1_048_576.0);
    verify_sha256(&actual, &expected, &download.asset_name)?;
    eprintln!("  Checksum verified");
    eprint!("  Extracting...");

    archive
        .seek(SeekFrom::Start(0))
        .map_err(io_err("cannot rewind staged archive"))?;

    #[cfg(not(windows))]
    extract_targz(io::BufReader::new(archive), scratch.path(), members)?;

    #[cfg(windows)]
    extract_zip(io::BufReader::new(archive), scratch.path(), members)?;

    eprintln!(" Done");
    Ok(StagedRelease {
        scratch,
        members: members.to_vec(),
    })
}

/// The required member an archive entry path names, if any. Release archives
/// are flat: only a single-component path (an optional leading `./` aside)
/// can name a member, so nested entries are never unpacked.
fn intended_member(path: &Path, members: &[ReleaseMember]) -> Option<ReleaseMember> {
    let mut components = path
        .components()
        .filter(|component| !matches!(component, Component::CurDir));
    let Some(Component::Normal(name)) = components.next() else {
        return None;
    };
    if components.next().is_some() {
        return None;
    }
    let name = name.to_str()?;
    members.iter().copied().find(|member| member.name == name)
}

/// Writes one archive member into `staging` under its own name, refusing a
/// duplicate entry: an archive that names the same member twice is
/// ambiguous, not a payload to pick from.
fn stage_member(
    staging: &Path,
    member: ReleaseMember,
    contents: &mut impl Read,
    staged: &mut Vec<&'static str>,
) -> Result<()> {
    if staged.contains(&member.name) {
        return Err(TraceDecayError::Config {
            message: format!(
                "release archive contains duplicate entries for '{}'",
                member.name
            ),
        });
    }
    let path = staging.join(member.name);
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(&path)
        .map_err(io_err("cannot create staged release member"))?;
    io::copy(contents, &mut file).map_err(io_err("extract failed"))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(std::fs::Permissions::from_mode(member.mode()))
            .map_err(io_err("cannot set staged member permissions"))?;
    }
    staged.push(member.name);
    Ok(())
}

fn not_a_regular_file(member: ReleaseMember) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "release archive entry '{}' is not a regular file",
            member.name
        ),
    }
}

/// Fails unless every required member was staged, naming the missing ones.
fn require_complete_release(members: &[ReleaseMember], staged: &[&str]) -> Result<()> {
    let missing: Vec<&str> = members
        .iter()
        .map(|member| member.name)
        .filter(|name| !staged.contains(name))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: format!(
            "release archive is missing required member(s): {}",
            missing.join(", ")
        ),
    })
}

/// Stages every required member of a `.tar.gz` release archive (Unix).
#[cfg(not(windows))]
fn extract_targz(archive: impl Read, staging: &Path, members: &[ReleaseMember]) -> Result<()> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let mut archive = Archive::new(GzDecoder::new(archive));
    let mut staged = Vec::new();

    for entry in archive.entries().map_err(io_err("archive open failed"))? {
        let mut entry = entry.map_err(io_err("archive read failed"))?;
        let path = entry
            .path()
            .map_err(io_err("archive path error"))?
            .into_owned();
        let Some(member) = intended_member(&path, members) else {
            continue;
        };
        if !entry.header().entry_type().is_file() {
            return Err(not_a_regular_file(member));
        }
        stage_member(staging, member, &mut entry, &mut staged)?;
    }

    require_complete_release(members, &staged)
}

/// Stages every required member of a `.zip` release archive (Windows).
#[cfg(windows)]
fn extract_zip(archive: impl Read + Seek, staging: &Path, members: &[ReleaseMember]) -> Result<()> {
    let mut archive = zip::ZipArchive::new(archive).map_err(|e| TraceDecayError::Config {
        message: format!("zip open failed: {e}"),
    })?;
    let mut staged = Vec::new();

    for i in 0..archive.len() {
        let mut file = archive.by_index(i).map_err(|e| TraceDecayError::Config {
            message: format!("zip entry error: {e}"),
        })?;
        let Some(member) = intended_member(Path::new(file.name()), members) else {
            continue;
        };
        if !file.is_file() {
            return Err(not_a_regular_file(member));
        }
        stage_member(staging, member, &mut file, &mut staged)?;
    }

    require_complete_release(members, &staged)
}

/// Publishes a staged release around the running executable and returns the
/// path the new binary was installed at, when known.
///
/// On Unix the running executable is resolved to its real file first: a
/// symlinked entry point must be replaced at its target (`self_replace`
/// resolves relative link targets from the CWD and fails with `ENOENT`), and
/// `$ORIGIN` is that target's directory. The path is captured before the
/// swap because on Linux `/proc/self/exe` reads `… (deleted)` afterwards.
#[hotpath::measure(label = "cli.upgrade.publish_release")]
fn publish_release(staged: &StagedRelease) -> Result<Option<PathBuf>> {
    #[cfg(unix)]
    {
        let executable = std::env::current_exe()
            .and_then(|exe| exe.canonicalize())
            .map_err(io_err("cannot resolve the running executable"))?;
        publish_release_at(staged, &executable)?;
        Ok(Some(executable))
    }
    #[cfg(not(unix))]
    {
        let exe = std::env::current_exe().ok();
        self_replace::self_replace(staged.executable()).map_err(|e| TraceDecayError::Config {
            message: format!(
                "binary replacement failed: {e}\n  \
                 The old version is still in place.\n  \
                 To upgrade manually: https://github.com/{GITHUB_REPO}/releases/latest"
            ),
        })?;
        Ok(exe)
    }
}

/// Publishes a staged release with `executable` as its entry point.
/// Companions are renamed into the executable's directory first so the new
/// entry point never appears without the runtime it resolves beside itself;
/// the executable is replaced last.
#[cfg(unix)]
fn publish_release_at(staged: &StagedRelease, executable: &Path) -> Result<()> {
    let install_dir = executable.parent().ok_or_else(|| TraceDecayError::Config {
        message: "cannot determine the running executable's directory".into(),
    })?;
    for member in staged.companions() {
        publish_member(
            &staged.path_of(member),
            &install_dir.join(member.name),
            member.mode(),
        )?;
    }
    publish_member(&staged.executable(), executable, EXECUTABLE_MEMBER.mode())
}

/// Outcome of an upgrade attempt that completed without error.
///
/// Mirrors the pre-install classification `UpgradeStatus`:
/// `UpgradeStatus::AlreadyCurrent` maps to `UpgradeOutcome::AlreadyCurrent`,
/// `UpgradeStatus::UpgradeAvailable` ends in `UpgradeOutcome::Installed`.
#[derive(Debug)]
pub enum UpgradeOutcome {
    /// A new binary was installed (or a delegated package manager reported a
    /// successful upgrade). Post-install refresh work is warranted.
    Installed {
        /// Where the freshly installed binary lives, when known. Callers
        /// re-execing `post-update` must prefer this over re-resolving the
        /// binary: `which_tracedecay()`'s current-exe-first order can point
        /// at the OLD binary (e.g. a stale Homebrew keg) after an upgrade.
        binary: Option<PathBuf>,
        /// Version of the freshly installed binary: the release-manifest
        /// version for GitHub-release installs, the linked binary's
        /// self-reported version for package-manager installs. Daemon restore
        /// validates this version — the binary it actually restarts — instead
        /// of the one that was running before the upgrade. `None` only when
        /// the manager's install could not be interrogated; restore
        /// verification then validates the pre-upgrade version and, if a new
        /// daemon really was installed, fails with a typed identity mismatch
        /// rather than silently passing.
        version: Option<String>,
    },
    /// Already on the latest version — the binary was not replaced.
    AlreadyCurrent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpgradeStatus<'a> {
    AlreadyCurrent,
    UpgradeAvailable(&'a str),
}

fn classify_upgrade<'a>(current: &str, latest: &'a str) -> UpgradeStatus<'a> {
    // Plain semver is safe across the version-epoch reset: `cloud::fetch_latest_*`
    // never yields pre-reset releases (see `release_has_current_platform_asset`).
    if cloud::is_newer_version(current, latest) {
        UpgradeStatus::UpgradeAvailable(latest)
    } else {
        UpgradeStatus::AlreadyCurrent
    }
}

/// A native package manager that owns a TraceDecay installation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PackageManager {
    Homebrew,
    Scoop,
}

/// Who installs a release for the running binary.
///
/// Installation ownership is exclusive: files under a package manager's tree
/// are only ever changed by that manager, so a managed install delegates its
/// upgrade to the manager and refuses operations the manager has no command
/// for. Everything else is a direct install that TraceDecay publishes itself
/// from the verified GitHub release bundle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UpgradeSource {
    PackageManager(PackageManager),
    GitHubRelease,
}

fn upgrade_source_for(method: &InstallMethod) -> UpgradeSource {
    match method {
        InstallMethod::Brew => UpgradeSource::PackageManager(PackageManager::Homebrew),
        InstallMethod::Scoop => UpgradeSource::PackageManager(PackageManager::Scoop),
        InstallMethod::Cargo | InstallMethod::Unknown => UpgradeSource::GitHubRelease,
    }
}

/// One package-manager invocation, spelled the way an operator would run it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagerCommand {
    program: String,
    args: Vec<String>,
}

impl ManagerCommand {
    fn new(program: &str, args: &[&str]) -> Self {
        Self {
            program: program.to_owned(),
            args: args.iter().map(|arg| (*arg).to_owned()).collect(),
        }
    }

    /// Runs the manager with inherited stdio: delegated installation is the
    /// operator's interactive command, and may legitimately take a while.
    fn status(&self) -> io::Result<ExitStatus> {
        Command::new(&self.program).args(&self.args).status()
    }

    /// Captures a short metadata answer from the manager under the shared
    /// bounded process boundary (deadline, output bounds, settlement).
    fn output(&self) -> std::result::Result<Output, GitCommandError> {
        let mut command = Command::new(&self.program);
        command.args(&self.args);
        bounded_command_output(command, None, &GitCommandBounds::default())
    }
}

impl fmt::Display for ManagerCommand {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.program)?;
        for arg in &self.args {
            write!(formatter, " {arg}")?;
        }
        Ok(())
    }
}

/// Scoop publishes the two channels as separate apps, mirroring the package
/// ids the Scoop service hooks accept.
fn scoop_package(is_beta: bool) -> &'static str {
    if is_beta {
        "tracedecay-beta"
    } else {
        "tracedecay"
    }
}

impl PackageManager {
    fn label(self) -> &'static str {
        match self {
            Self::Homebrew => "Homebrew",
            Self::Scoop => "Scoop",
        }
    }

    /// The manager's own shim is a `.cmd` on Windows; Rust spawns those
    /// through `cmd.exe` only when the extension is spelled out.
    fn scoop_program() -> &'static str {
        if cfg!(windows) { "scoop.cmd" } else { "scoop" }
    }

    /// Refreshes the manager's package metadata before an upgrade.
    fn refresh_command(self) -> ManagerCommand {
        match self {
            Self::Homebrew => ManagerCommand::new("brew", &["update", "--quiet"]),
            Self::Scoop => ManagerCommand::new(Self::scoop_program(), &["update"]),
        }
    }

    /// The manager's own upgrade of the installed TraceDecay package.
    fn upgrade_command(self, is_beta: bool) -> ManagerCommand {
        match self {
            Self::Homebrew => ManagerCommand::new("brew", &["upgrade", "tracedecay"]),
            Self::Scoop => {
                ManagerCommand::new(Self::scoop_program(), &["update", scoop_package(is_beta)])
            }
        }
    }

    /// Asks the manager where it installed the package.
    fn prefix_command(self, is_beta: bool) -> ManagerCommand {
        match self {
            Self::Homebrew => ManagerCommand::new("brew", &["--prefix", "tracedecay"]),
            Self::Scoop => {
                ManagerCommand::new(Self::scoop_program(), &["prefix", scoop_package(is_beta)])
            }
        }
    }

    /// The binary the manager currently links: Homebrew's opt prefix
    /// survives keg-version bumps and cleanup (unlike the keg-versioned
    /// Cellar path the running process resolves to); Scoop's `current`
    /// junction plays the same role.
    fn installed_binary(self, is_beta: bool) -> std::result::Result<PathBuf, String> {
        let command = self.prefix_command(is_beta);
        let output = command
            .output()
            .map_err(|error| format!("`{command}` {}", describe_process_failure(&error)))?;
        if !output.status.success() {
            return Err(format!("`{command}` exited with {}", output.status));
        }
        let prefix = String::from_utf8(output.stdout)
            .map_err(|_| format!("`{command}` printed a non-UTF-8 path"))?;
        let binary = match self {
            Self::Homebrew => Path::new(prefix.trim()).join("bin").join("tracedecay"),
            Self::Scoop => Path::new(prefix.trim()).join("tracedecay.exe"),
        };
        if binary.is_file() {
            Ok(binary)
        } else {
            Err(format!(
                "`{command}` named {}, which is not a file",
                binary.display()
            ))
        }
    }

    /// Channel switching has no manager command: Homebrew ships one formula,
    /// and Scoop's channels are separate apps whose transition (uninstall,
    /// then install) changes which package owns the daemon service. Both are
    /// the operator's call, so the request is refused before anything moves.
    fn channel_switch_refusal(self, is_beta: bool, target_channel: &str) -> TraceDecayError {
        let label = self.label();
        let guidance = match self {
            Self::Homebrew => format!(
                "keep upgrading with `{}`, or install the {target_channel} channel outside the \
                 Homebrew prefix from https://github.com/{GITHUB_REPO}/releases",
                self.upgrade_command(is_beta)
            ),
            Self::Scoop => format!(
                "switch with `scoop uninstall {}` followed by `scoop install {}`",
                scoop_package(is_beta),
                scoop_package(!is_beta),
            ),
        };
        TraceDecayError::Config {
            message: format!(
                "this tracedecay was installed by {label}, which owns its files; TraceDecay will \
                 not rewrite a {label}-managed installation to switch channels.\n  {guidance}"
            ),
        }
    }
}

fn latest_upgrade_version(is_beta: bool) -> Result<String> {
    cloud::fetch_latest_version().ok_or_else(|| github_latest_unavailable_error(is_beta))
}

fn github_latest_unavailable_error(is_beta: bool) -> TraceDecayError {
    let channel = if is_beta { "beta" } else { "stable" };
    TraceDecayError::Config {
        message: format!(
            "failed to check for updates — no installable GitHub release asset is available for \
             the current platform on the {channel} channel.\n  \
             GitHub may be reachable, but release CI may still be uploading binaries."
        ),
    }
}

fn install_upgrade_version(latest: &str, is_beta: bool) -> Result<Option<PathBuf>> {
    let download = preflight_asset_check(latest, is_beta)?;
    perform_upgrade(&download)
}

fn run_versioned_upgrade(current: &str, is_beta: bool) -> Result<UpgradeOutcome> {
    eprintln!("Checking GitHub releases...");
    let latest = latest_upgrade_version(is_beta)?;
    let latest = match classify_upgrade(current, &latest) {
        UpgradeStatus::AlreadyCurrent => {
            eprintln!("\x1b[32m✔\x1b[0m Already up to date (v{current}).");
            return Ok(UpgradeOutcome::AlreadyCurrent);
        }
        UpgradeStatus::UpgradeAvailable(latest) => latest,
    };

    eprintln!("Upgrading v{current} → v{latest}...");
    let binary = install_upgrade_version(latest, is_beta)?;
    record_previous_version();
    eprintln!("\x1b[32m✔\x1b[0m Successfully upgraded to v{latest}!");
    Ok(UpgradeOutcome::Installed {
        binary,
        version: Some(latest.to_owned()),
    })
}

/// Atomically replaces `target` with the contents of `source`: the bytes are
/// copied into an exclusively created sibling in `target`'s directory, given
/// `mode`, then renamed over `target`. Rename swaps directory entries, so a
/// running executable (`ETXTBSY`) or a currently mapped library is never
/// written in place, and a failure leaves `target` untouched.
#[cfg(unix)]
fn publish_member(source: &Path, target: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let dir = target.parent().ok_or_else(|| TraceDecayError::Config {
        message: "cannot determine target directory".into(),
    })?;
    let mut sibling = tempfile::Builder::new()
        .prefix(".tracedecay-upgrade-")
        .tempfile_in(dir)
        .map_err(io_err("cannot stage release member beside its target"))?;
    let mut source = File::open(source).map_err(io_err("cannot open staged release member"))?;
    io::copy(&mut source, sibling.as_file_mut()).map_err(io_err("cannot copy release member"))?;
    sibling
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(mode))
        .map_err(io_err("cannot set permissions"))?;
    sibling
        .persist(target)
        .map_err(|error| io_err("cannot replace release member")(error.error))?;
    Ok(())
}

/// Downloads, extracts, and installs the binary for `version`/`is_beta`.
/// Verifies the release asset exists on GitHub and returns the download URL.
/// Call this early so we fail fast when CI hasn't finished building the
/// release yet.
fn preflight_asset_check(version: &str, is_beta: bool) -> Result<ReleaseDownload> {
    let tag = release_tag(version);
    let asset = asset_name(version, is_beta);
    eprintln!("  Asset: {asset}");
    fetch_release_download(&tag, &asset)
}

/// Record the *currently running* binary's version in user config just before
/// the binary is replaced. The new binary reads this on startup as
/// `previous_version` and decides whether reinstall is required for the
/// transition (e.g. minor/major bumps re-register agents to pick up new MCP
/// tools or hook changes; patch bumps just update the field).
fn record_previous_version() {
    let current = env!("CARGO_PKG_VERSION");
    let mut cfg = UserConfig::load();
    if cfg.previous_version == current {
        return;
    }
    cfg.previous_version = current.to_string();
    if let Err(err) = cfg.save() {
        eprintln!(
            "  \x1b[33mwarning:\x1b[0m could not record previous version ({err}); \
             run `tracedecay reinstall` manually if new tools aren't registered"
        );
    }
}

/// Downloads, verifies, stages and publishes the complete release bundle of a
/// direct install. Returns the path the new binary was installed at, when
/// known.
fn perform_upgrade(download: &ReleaseDownload) -> Result<Option<PathBuf>> {
    let staged = download_and_stage(download, &required_members())?;

    eprint!("  Installing release...");
    let installed_at = publish_release(&staged)?;
    eprintln!(" Done");

    Ok(installed_at)
}

/// A `--version` line is `tracedecay <release>[+<40-hex sha>[.dirty]]`, well
/// under 128 bytes. 4 KiB leaves room for any such line and refuses an
/// executable that chatters or loops without retaining unbounded output.
const VERSION_PROBE_STDOUT_LIMIT: usize = 4 * 1024;
/// A wedged binary must not hang the upgrade; a healthy one answers in
/// milliseconds.
const VERSION_PROBE_DEADLINE: Duration = Duration::from_secs(5);

/// The version a `tracedecay --version` line reports: exactly one line,
/// `tracedecay ` followed by a `SemVer` version. Build metadata is kept so a
/// checkout build's `+<sha>` identity survives; anything else is not version
/// evidence.
fn parse_version_output(output: &str) -> Option<String> {
    let mut lines = output.lines();
    let line = lines.next()?;
    if lines.next().is_some() {
        return None;
    }
    let version = line.strip_prefix("tracedecay ")?;
    semver::Version::parse(version).ok()?;
    Some(version.to_owned())
}

/// Why a version probe produced no installed version.
#[derive(Debug)]
enum VersionProbeError {
    /// Spawn, deadline, output-bound, or settlement failure at the process
    /// boundary; the child was killed and reaped before this is reported.
    Execution(GitCommandError),
    /// The binary ran to completion but did not succeed.
    ExitStatus(ExitStatus),
    /// Stdout was not a single `tracedecay <semver>` line.
    Unrecognized(String),
}

/// Neutral wording for the shared process boundary's failures, whose own
/// messages are phrased for its primary `git` caller.
fn describe_process_failure(error: &GitCommandError) -> String {
    match error {
        GitCommandError::Unavailable(source) => format!("could not run: {source}"),
        GitCommandError::Cancelled => "was cancelled".to_owned(),
        GitCommandError::DeadlineExceeded => {
            "did not exit and release its output before the deadline".to_owned()
        }
        GitCommandError::OutputLimitExceeded { stream, bound } => {
            format!("wrote more than {bound} bytes to {stream}")
        }
        GitCommandError::ReadOutput { stream, source } => {
            format!("could not be read on {stream}: {source}")
        }
        GitCommandError::Wait(source) => format!("could not be waited for: {source}"),
    }
}

impl fmt::Display for VersionProbeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Execution(error) => {
                write!(formatter, "`--version` {}", describe_process_failure(error))
            }
            Self::ExitStatus(status) => write!(formatter, "`--version` exited with {status}"),
            Self::Unrecognized(text) => {
                write!(formatter, "unrecognized `--version` output {text:?}")
            }
        }
    }
}

/// Asks the binary at `path` for its version under one absolute deadline,
/// draining bounded stdout concurrently with the child's progress so a
/// chatty binary cannot deadlock on a full pipe and a wedged one cannot hang
/// the upgrade. Every exit path leaves the child killed and reaped.
fn installed_binary_version(path: &Path) -> std::result::Result<String, VersionProbeError> {
    installed_binary_version_within(path, VERSION_PROBE_DEADLINE)
}

fn installed_binary_version_within(
    path: &Path,
    deadline: Duration,
) -> std::result::Result<String, VersionProbeError> {
    let mut command = Command::new(path);
    command.arg("--version");
    let bounds = GitCommandBounds {
        deadline: Instant::now() + deadline,
        max_stdout_bytes: VERSION_PROBE_STDOUT_LIMIT,
        ..GitCommandBounds::default()
    };
    let output =
        bounded_command_output(command, None, &bounds).map_err(VersionProbeError::Execution)?;
    if !output.status.success() {
        return Err(VersionProbeError::ExitStatus(output.status));
    }
    let text = String::from_utf8(output.stdout).map_err(|error| {
        VersionProbeError::Unrecognized(String::from_utf8_lossy(error.as_bytes()).into_owned())
    })?;
    parse_version_output(&text).ok_or(VersionProbeError::Unrecognized(text))
}

/// Whether a delegated manager upgrade was a no-op: the binary the manager
/// links reports exactly the build version this process is running, which
/// is the same file unless the manager installed something. `None`
/// (undetectable) is treated as a real install so the refresh chain never
/// silently skips.
fn delegated_upgrade_was_noop(
    running_build_version: &str,
    installed_version: Option<&str>,
) -> bool {
    installed_version == Some(running_build_version)
}

/// Delegates an upgrade to the package manager that owns the installation
/// and classifies the result by asking the binary the manager links for its
/// version. TraceDecay writes nothing under the manager's tree: a manager
/// failure is reported as-is and `locate_installed_binary` is never reached.
fn run_delegated_upgrade(
    manager: PackageManager,
    refresh: &ManagerCommand,
    upgrade: &ManagerCommand,
    locate_installed_binary: impl FnOnce() -> std::result::Result<PathBuf, String>,
) -> Result<UpgradeOutcome> {
    let label = manager.label();
    eprintln!("Refreshing {label} package metadata: {refresh}");
    if !refresh.status().is_ok_and(|status| status.success()) {
        eprintln!("  warning: `{refresh}` failed — continuing with existing metadata");
    }

    eprintln!("Delegating upgrade to {label}: {upgrade}");
    let status = upgrade.status().map_err(|error| TraceDecayError::Config {
        message: format!("failed to run `{upgrade}`: {error}"),
    })?;
    if !status.success() {
        return Err(TraceDecayError::Config {
            message: format!(
                "`{upgrade}` failed with status: {status}\n  \
                 TraceDecay made no changes to the {label}-owned installation."
            ),
        });
    }

    // The manager exits 0 whether or not it installed anything, so ask the
    // binary it now links for its version to tell the outcomes apart.
    let binary = match locate_installed_binary() {
        Ok(binary) => Some(binary),
        Err(reason) => {
            eprintln!(
                "  \x1b[33mwarning:\x1b[0m could not locate the {label}-installed binary \
                 ({reason}); assuming a new install so the refresh chain runs"
            );
            None
        }
    };
    let installed_version = match binary.as_deref().map(installed_binary_version) {
        Some(Ok(version)) => Some(version),
        Some(Err(reason)) => {
            eprintln!(
                "  \x1b[33mwarning:\x1b[0m could not read the {label}-installed binary's version \
                 ({reason}); assuming a new install so the refresh chain runs"
            );
            None
        }
        None => None,
    };
    if delegated_upgrade_was_noop(
        crate::product_runtime::PRODUCT_BUILD_VERSION,
        installed_version.as_deref(),
    ) {
        eprintln!(
            "\x1b[32m✔\x1b[0m Already up to date (v{}).",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(UpgradeOutcome::AlreadyCurrent);
    }

    // `installed_version` may be None here (assumed install, undetectable
    // binary): daemon restore then validates the pre-upgrade version and
    // reports a typed identity mismatch if the manager really did install a
    // new daemon — truthful failure over a fabricated version.
    Ok(UpgradeOutcome::Installed {
        binary,
        version: installed_version,
    })
}

fn run_package_manager_upgrade(manager: PackageManager, is_beta: bool) -> Result<UpgradeOutcome> {
    let outcome = run_delegated_upgrade(
        manager,
        &manager.refresh_command(),
        &manager.upgrade_command(is_beta),
        || manager.installed_binary(is_beta),
    )?;
    if matches!(outcome, UpgradeOutcome::Installed { .. }) {
        record_previous_version();
    }
    Ok(outcome)
}

/// Check for a newer version and perform the upgrade if one is available.
///
/// Returns whether a new binary was actually installed.
pub fn run_upgrade() -> Result<UpgradeOutcome> {
    let current = env!("CARGO_PKG_VERSION");
    let is_beta = cloud::is_beta();
    let channel = if is_beta { "beta" } else { "stable" };
    let method = cloud::detect_install_method();

    let method_suffix = match &method {
        InstallMethod::Brew => " · Homebrew",
        InstallMethod::Scoop => " · Scoop",
        InstallMethod::Cargo => " · cargo",
        InstallMethod::Unknown => "",
    };
    eprintln!("Current version: v{current} ({channel} channel{method_suffix})");

    match upgrade_source_for(&method) {
        UpgradeSource::PackageManager(manager) => run_package_manager_upgrade(manager, is_beta),
        UpgradeSource::GitHubRelease => run_versioned_upgrade(current, is_beta),
    }
}

/// Print the current channel.
pub fn show_channel() {
    let current = env!("CARGO_PKG_VERSION");
    let channel = if cloud::is_beta() { "beta" } else { "stable" };
    eprintln!("v{current} ({channel})");
}

/// Switch to a different channel by downloading the latest release from it.
/// Package-manager installs are refused before anything is fetched: the
/// manager owns those files, and no manager command switches channels.
#[hotpath::measure(label = "cli.channel.switch")]
pub fn switch_channel(target_channel: &str) -> Result<String> {
    switch_channel_for(&cloud::detect_install_method(), target_channel)
}

fn switch_channel_for(method: &InstallMethod, target_channel: &str) -> Result<String> {
    let current = env!("CARGO_PKG_VERSION");
    let current_is_beta = cloud::is_beta();
    let current_channel = if current_is_beta { "beta" } else { "stable" };

    let target_is_beta = match target_channel {
        "beta" => true,
        "stable" => false,
        other => {
            return Err(TraceDecayError::Config {
                message: format!("unknown channel '{other}'. Valid channels: stable, beta"),
            });
        }
    };

    if target_is_beta == current_is_beta {
        eprintln!("Already on the {current_channel} channel (v{current}).");
        eprintln!("Run `tracedecay upgrade` to check for updates within this channel.");
        return Ok(current.to_string());
    }

    if let UpgradeSource::PackageManager(manager) = upgrade_source_for(method) {
        return Err(manager.channel_switch_refusal(current_is_beta, target_channel));
    }

    eprintln!("Switching from {current_channel} to {target_channel}...");

    let latest = if target_is_beta {
        cloud::fetch_latest_beta_version()
    } else {
        cloud::fetch_latest_stable_version()
    }
    .ok_or_else(|| TraceDecayError::Config {
        message: format!("failed to find latest {target_channel} release — could not reach GitHub"),
    })?;

    eprintln!("  Target: v{latest}");

    let download = preflight_asset_check(&latest, target_is_beta)?;

    // Channel switches do not yet run the post-update refresh chain, so the
    // installed path is unused here.
    let _ = perform_upgrade(&download)?;
    record_previous_version();
    eprintln!("\x1b[32m✔\x1b[0m Switched to {target_channel} channel: v{latest}");
    Ok(latest)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::case_sensitive_file_extension_comparisons,
    clippy::doc_markdown,
    clippy::redundant_closure_for_method_calls
)]
mod tests {
    // All remaining unwrap/expect usage in this module is test-only fixture or
    // assertion setup; production upgrade code above is kept panic-free.
    use super::*;

    #[test]
    fn checksum_manifest_selects_the_exact_release_asset() {
        let digest = "a".repeat(64);
        let manifest = format!(
            "{digest}  tracedecay-v1.2.3-x86_64-linux.tar.gz\n{} *other.tar.gz\n",
            "b".repeat(64)
        );

        assert_eq!(
            expected_sha256(manifest.as_bytes(), "tracedecay-v1.2.3-x86_64-linux.tar.gz").unwrap(),
            digest
        );
    }

    #[test]
    fn checksum_manifest_fails_closed_on_missing_duplicate_or_invalid_entries() {
        let asset = "tracedecay-v1.2.3-x86_64-linux.tar.gz";
        assert!(expected_sha256(b"", asset).is_err());
        let duplicate = format!("{0}  {asset}\n{0}  {asset}\n", "a".repeat(64));
        assert!(expected_sha256(duplicate.as_bytes(), asset).is_err());
        let invalid = format!("not-a-digest  {asset}\n");
        assert!(expected_sha256(invalid.as_bytes(), asset).is_err());
    }

    #[test]
    fn release_archive_checksum_must_match_before_extraction() {
        let digest = hex::encode(Sha256::digest(b"verified archive bytes"));
        let tampered = hex::encode(Sha256::digest(b"tampered"));

        assert!(verify_sha256(&digest, &digest, "archive.tar.gz").is_ok());
        assert!(verify_sha256(&tampered, &digest, "archive.tar.gz").is_err());
    }

    #[test]
    fn only_a_single_tracedecay_semver_line_is_version_evidence() {
        assert_eq!(
            parse_version_output("tracedecay 5.0.1\n").as_deref(),
            Some("5.0.1")
        );
        let build = "0.1.0-beta.37+0123456789abcdef0123456789abcdef01234567.dirty";
        assert_eq!(
            parse_version_output(&format!("tracedecay {build}\n")).as_deref(),
            Some(build),
            "build metadata identifies the exact binary and must survive"
        );
        assert_eq!(parse_version_output(""), None);
        assert_eq!(parse_version_output("tracedecay v5.0.1"), None);
        assert_eq!(parse_version_output("tracedecay"), None);
        assert_eq!(parse_version_output("tracedecay not-a-version"), None);
        assert_eq!(parse_version_output("error: missing runtime 5.0.1"), None);
        assert_eq!(
            parse_version_output("tracedecay 5.0.1\nwarning: a newer version exists\n"),
            None
        );
    }

    #[cfg(unix)]
    mod version_probe {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::path::{Path, PathBuf};
        use std::time::{Duration, Instant};

        use tracedecay_runtime_core::git::GitCommandError;

        use super::super::{
            VersionProbeError, installed_binary_version, installed_binary_version_within,
        };

        fn script(dir: &Path, body: &str) -> PathBuf {
            let path = dir.join("tracedecay");
            fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        #[test]
        fn a_healthy_binary_reports_its_version() {
            let dir = tempfile::tempdir().unwrap();
            let binary = script(dir.path(), "printf 'tracedecay 1.2.3+abcdef\\n'");

            assert_eq!(installed_binary_version(&binary).unwrap(), "1.2.3+abcdef");
        }

        #[test]
        fn failed_exits_and_malformed_output_are_not_version_evidence() {
            let dir = tempfile::tempdir().unwrap();

            let failing = script(dir.path(), "printf 'tracedecay 1.2.3\\n'; exit 1");
            assert!(matches!(
                installed_binary_version(&failing),
                Err(VersionProbeError::ExitStatus(_))
            ));

            let chatty = script(dir.path(), "printf 'tracedecay 1.2.3\\nnote: hi\\n'");
            assert!(matches!(
                installed_binary_version(&chatty),
                Err(VersionProbeError::Unrecognized(_))
            ));

            let garbage = script(dir.path(), "printf 'segfault at 0x42 1.2.3\\n'");
            assert!(matches!(
                installed_binary_version(&garbage),
                Err(VersionProbeError::Unrecognized(_))
            ));

            let invalid_utf8 = script(dir.path(), "printf 'tracedecay 1.2.3\\377\\n'");
            assert!(matches!(
                installed_binary_version(&invalid_utf8),
                Err(VersionProbeError::Unrecognized(_))
            ));

            let missing = dir.path().join("absent");
            assert!(matches!(
                installed_binary_version(&missing),
                Err(VersionProbeError::Execution(GitCommandError::Unavailable(
                    _
                )))
            ));
        }

        #[test]
        fn a_child_that_overfills_its_stdout_pipe_is_refused_without_a_deadlock() {
            let dir = tempfile::tempdir().unwrap();
            // Well past both the probe's byte bound and the kernel pipe
            // capacity: a wait-before-read probe would block here until its
            // deadline and then misclassify the binary as wedged.
            let flood = script(dir.path(), "head -c 200000 /dev/zero | tr '\\0' a");
            let started = Instant::now();

            let error = installed_binary_version(&flood).unwrap_err();

            assert!(
                matches!(
                    error,
                    VersionProbeError::Execution(GitCommandError::OutputLimitExceeded {
                        stream: "stdout",
                        ..
                    })
                ),
                "{error}"
            );
            assert!(started.elapsed() < Duration::from_secs(2));
        }

        #[test]
        fn a_wedged_binary_is_killed_and_reaped_at_the_deadline() {
            let dir = tempfile::tempdir().unwrap();
            let pid_file = dir.path().join("pid");
            let wedged = script(
                dir.path(),
                &format!("echo $$ > '{}'; exec sleep 30", pid_file.display()),
            );
            let started = Instant::now();

            let error =
                installed_binary_version_within(&wedged, Duration::from_millis(300)).unwrap_err();

            assert!(
                matches!(
                    error,
                    VersionProbeError::Execution(GitCommandError::DeadlineExceeded)
                ),
                "{error}"
            );
            assert!(started.elapsed() < Duration::from_secs(5));
            #[cfg(target_os = "linux")]
            {
                let pid = fs::read_to_string(&pid_file).unwrap().trim().to_owned();
                assert!(
                    !Path::new("/proc").join(&pid).exists(),
                    "the probe must not leave its child running"
                );
            }
        }

        #[test]
        fn a_descendant_holding_stdout_cannot_hold_the_probe_past_its_deadline() {
            let dir = tempfile::tempdir().unwrap();
            let leaky = script(
                dir.path(),
                "printf 'tracedecay 1.2.3\\n'; sleep 20 & exit 0",
            );
            let started = Instant::now();

            let error =
                installed_binary_version_within(&leaky, Duration::from_millis(300)).unwrap_err();

            assert!(
                matches!(
                    error,
                    VersionProbeError::Execution(GitCommandError::DeadlineExceeded)
                ),
                "{error}"
            );
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "a successful parent exit does not close an inherited pipe; the deadline must"
            );
        }
    }

    // ── Installation ownership ──────────────────────────────────────────

    #[test]
    fn package_managers_own_their_installations_and_direct_installs_use_github() {
        assert_eq!(
            upgrade_source_for(&InstallMethod::Brew),
            UpgradeSource::PackageManager(PackageManager::Homebrew)
        );
        assert_eq!(
            upgrade_source_for(&InstallMethod::Scoop),
            UpgradeSource::PackageManager(PackageManager::Scoop)
        );
        assert_eq!(
            upgrade_source_for(&InstallMethod::Cargo),
            UpgradeSource::GitHubRelease
        );
        assert_eq!(
            upgrade_source_for(&InstallMethod::Unknown),
            UpgradeSource::GitHubRelease
        );
    }

    #[test]
    fn managed_upgrades_are_the_managers_own_commands() {
        assert_eq!(
            PackageManager::Homebrew.refresh_command().to_string(),
            "brew update --quiet"
        );
        assert_eq!(
            PackageManager::Homebrew.upgrade_command(false).to_string(),
            "brew upgrade tracedecay"
        );
        assert_eq!(
            PackageManager::Homebrew.upgrade_command(true).to_string(),
            "brew upgrade tracedecay",
            "Homebrew ships one formula; the channel does not change the command"
        );
        let scoop = PackageManager::scoop_program();
        assert_eq!(
            PackageManager::Scoop.upgrade_command(false).to_string(),
            format!("{scoop} update tracedecay")
        );
        assert_eq!(
            PackageManager::Scoop.upgrade_command(true).to_string(),
            format!("{scoop} update tracedecay-beta")
        );
        assert_eq!(
            PackageManager::Scoop.prefix_command(true).to_string(),
            format!("{scoop} prefix tracedecay-beta")
        );
    }

    #[test]
    fn a_no_op_managed_upgrade_is_the_same_build_and_anything_else_is_an_install() {
        let running = "0.1.0-beta.37+0123456789abcdef0123456789abcdef01234567";

        // A no-op must map to `AlreadyCurrent` so it never triggers the
        // post-upgrade refresh chain (daemon restart included).
        assert!(delegated_upgrade_was_noop(running, Some(running)));
        // A different build of the same release is a different binary.
        assert!(!delegated_upgrade_was_noop(
            running,
            Some("0.1.0-beta.37+fedcba9876543210fedcba9876543210fedcba98")
        ));
        assert!(!delegated_upgrade_was_noop(running, Some("0.1.0-beta.38")));
        // Undetectable → assume install so the refresh never silently skips.
        assert!(!delegated_upgrade_was_noop(running, None));
    }

    #[test]
    fn package_manager_installs_refuse_channel_switches_before_fetching_anything() {
        let other_channel = if cloud::is_beta() { "stable" } else { "beta" };

        let homebrew = switch_channel_for(&InstallMethod::Brew, other_channel).unwrap_err();
        let message = homebrew.to_string();
        assert!(message.contains("installed by Homebrew"), "{message}");
        assert!(message.contains("`brew upgrade tracedecay`"), "{message}");

        let scoop = switch_channel_for(&InstallMethod::Scoop, other_channel).unwrap_err();
        let message = scoop.to_string();
        assert!(message.contains("installed by Scoop"), "{message}");
        let (current, target) = if cloud::is_beta() {
            ("tracedecay-beta", "tracedecay")
        } else {
            ("tracedecay", "tracedecay-beta")
        };
        assert!(
            message.contains(&format!("uninstall {current}"))
                && message.contains(&format!("install {target}")),
            "{message}"
        );
    }

    #[cfg(unix)]
    mod delegation {
        use std::cell::Cell;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        use std::path::PathBuf;

        use super::super::{ManagerCommand, PackageManager, UpgradeOutcome, run_delegated_upgrade};

        fn sh(script: &str) -> ManagerCommand {
            ManagerCommand::new("sh", &["-c", script])
        }

        /// A stand-in for the binary a manager links: prints `tracedecay
        /// <version>` like the real `--version`.
        fn fake_binary(dir: &std::path::Path, version: &str) -> PathBuf {
            let path = dir.join("tracedecay");
            fs::write(
                &path,
                format!("#!/bin/sh\nprintf 'tracedecay %s\\n' '{version}'\n"),
            )
            .unwrap();
            fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        #[test]
        fn a_failed_manager_command_is_reported_and_nothing_else_is_touched() {
            let located = Cell::new(false);

            let error = run_delegated_upgrade(
                PackageManager::Homebrew,
                &sh("exit 0"),
                &sh("exit 3"),
                || {
                    located.set(true);
                    Ok(PathBuf::from("/nonexistent"))
                },
            )
            .unwrap_err();

            let message = error.to_string();
            assert!(message.contains("`sh -c exit 3` failed"), "{message}");
            assert!(
                message.contains("no changes to the Homebrew-owned"),
                "{message}"
            );
            assert!(
                !located.get(),
                "a failed manager leaves the outcome unclassified"
            );
        }

        #[test]
        fn a_manager_that_left_the_running_build_in_place_is_a_noop() {
            let dir = tempfile::tempdir().unwrap();
            let binary = fake_binary(dir.path(), crate::product_runtime::PRODUCT_BUILD_VERSION);

            let outcome = run_delegated_upgrade(
                PackageManager::Homebrew,
                &sh("exit 1"),
                &sh("exit 0"),
                || Ok(binary.clone()),
            )
            .unwrap();

            assert!(
                matches!(outcome, UpgradeOutcome::AlreadyCurrent),
                "{outcome:?}"
            );
        }

        #[test]
        fn a_manager_that_linked_a_different_build_installed_it() {
            let dir = tempfile::tempdir().unwrap();
            let binary = fake_binary(dir.path(), "0.0.1+0123456789abcdef0123456789abcdef01234567");

            let outcome =
                run_delegated_upgrade(PackageManager::Scoop, &sh("exit 0"), &sh("exit 0"), || {
                    Ok(binary.clone())
                })
                .unwrap();

            let UpgradeOutcome::Installed {
                binary: installed,
                version,
            } = outcome
            else {
                panic!("expected an install, got {outcome:?}");
            };
            assert_eq!(installed, Some(binary));
            assert_eq!(
                version.as_deref(),
                Some("0.0.1+0123456789abcdef0123456789abcdef01234567")
            );
        }

        #[test]
        fn an_unlocatable_managed_binary_counts_as_an_install_with_unknown_version() {
            let outcome = run_delegated_upgrade(
                PackageManager::Homebrew,
                &sh("exit 0"),
                &sh("exit 0"),
                || Err("`brew --prefix tracedecay` exited with 1".to_owned()),
            )
            .unwrap();

            assert!(
                matches!(
                    outcome,
                    UpgradeOutcome::Installed {
                        binary: None,
                        version: None,
                    }
                ),
                "{outcome:?}"
            );
        }
    }

    #[test]
    fn test_asset_name_matches_ci_convention() {
        let stable = asset_name("3.3.3", false);
        let platform = current_platform();
        if cfg!(windows) {
            assert_eq!(stable, format!("tracedecay-v3.3.3-{platform}.zip"));
        } else {
            assert_eq!(stable, format!("tracedecay-v3.3.3-{platform}.tar.gz"));
        }

        let beta = asset_name("4.0.2-beta.1", true);
        if cfg!(windows) {
            assert_eq!(
                beta,
                format!("tracedecay-beta-v4.0.2-beta.1-{platform}.zip")
            );
        } else {
            assert_eq!(
                beta,
                format!("tracedecay-beta-v4.0.2-beta.1-{platform}.tar.gz")
            );
        }
    }

    #[test]
    fn classify_upgrade_marks_newer_version_as_upgrade_available() {
        assert_eq!(
            classify_upgrade("4.0.2", "4.0.3"),
            UpgradeStatus::UpgradeAvailable("4.0.3")
        );
    }

    #[test]
    fn switch_channel_same_channel_is_a_successful_noop() {
        let current = env!("CARGO_PKG_VERSION").to_string();
        let current_channel = if cloud::is_beta() { "beta" } else { "stable" };

        let result = switch_channel(current_channel);

        assert_eq!(result.unwrap(), current);
    }

    /// The installer's idea of a complete release must be the packaging
    /// step's: the companions it requires are exactly the runtime
    /// `entry_name`s `.github/release-targets.json` declares for this
    /// platform (none when the platform has no runtime, or no release target).
    #[test]
    fn required_members_match_the_release_target_manifest() {
        let manifest_path =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../.github/release-targets.json");
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        let target = manifest["include"]
            .as_array()
            .unwrap()
            .iter()
            .find(|target| target["name"] == current_platform());

        let mut expected: Vec<&str> = Vec::new();
        if let Some(runtime) = target.and_then(|target| target.get("runtime")) {
            expected.push(runtime["entry_name"].as_str().unwrap());
            for notice in runtime["notices"].as_array().unwrap() {
                expected.push(notice["entry_name"].as_str().unwrap());
            }
        }

        assert_eq!(RUNTIME_COMPANIONS, expected.as_slice());
        let members = required_members();
        assert_eq!(members[0], EXECUTABLE_MEMBER);
        assert_eq!(members.len(), 1 + RUNTIME_COMPANIONS.len());
    }

    #[cfg(unix)]
    mod release_bundle {
        use std::fs;
        use std::io::{Cursor, Read, Write};
        use std::net::TcpListener;
        use std::os::unix::fs::PermissionsExt;

        use flate2::Compression;
        use flate2::write::GzEncoder;
        use sha2::{Digest, Sha256};
        use tar::{Builder, EntryType, Header};

        use super::super::{
            EXECUTABLE_MEMBER, ReleaseDownload, ReleaseMember, ReleaseMemberKind, StagedRelease,
            extract_targz, intended_member, publish_member, publish_release_at, stage_release_in,
        };

        const RUNTIME: ReleaseMember = ReleaseMember {
            name: "libonnxruntime.so.1",
            kind: ReleaseMemberKind::Companion,
        };
        const LICENSE: ReleaseMember = ReleaseMember {
            name: "onnxruntime-LICENSE",
            kind: ReleaseMemberKind::Companion,
        };

        /// The Linux release contract, independent of the test host so the
        /// companion path is exercised on every Unix.
        fn linux_members() -> Vec<ReleaseMember> {
            vec![EXECUTABLE_MEMBER, RUNTIME, LICENSE]
        }

        struct Entry {
            path: &'static str,
            contents: &'static [u8],
            kind: EntryType,
        }

        fn file(path: &'static str, contents: &'static [u8]) -> Entry {
            Entry {
                path,
                contents,
                kind: EntryType::Regular,
            }
        }

        fn targz(entries: &[Entry]) -> Vec<u8> {
            let mut builder = Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
            for entry in entries {
                let mut header = Header::new_ustar();
                header.set_entry_type(entry.kind);
                header.set_mode(0o600);
                header.set_size(entry.contents.len() as u64);
                if entry.kind == EntryType::Symlink {
                    header.set_link_name("tracedecay").unwrap();
                }
                builder
                    .append_data(&mut header, entry.path, entry.contents)
                    .unwrap();
            }
            builder.into_inner().unwrap().finish().unwrap()
        }

        fn complete_release() -> Vec<u8> {
            targz(&[
                file("tracedecay", b"new-executable"),
                file("libonnxruntime.so.1", b"new-runtime"),
                file("onnxruntime-LICENSE", b"license"),
            ])
        }

        fn stage(archive: &[u8], members: &[ReleaseMember]) -> super::super::Result<StagedRelease> {
            let scratch = tempfile::tempdir().unwrap();
            extract_targz(Cursor::new(archive), scratch.path(), members)?;
            Ok(StagedRelease {
                scratch,
                members: members.to_vec(),
            })
        }

        fn mode_of(path: &std::path::Path) -> u32 {
            fs::metadata(path).unwrap().permissions().mode() & 0o777
        }

        #[test]
        fn extraction_stages_every_required_member_with_its_publication_mode() {
            let staged = stage(&complete_release(), &linux_members()).unwrap();

            assert_eq!(fs::read(staged.executable()).unwrap(), b"new-executable");
            assert_eq!(mode_of(&staged.executable()), 0o755);
            assert_eq!(fs::read(staged.path_of(RUNTIME)).unwrap(), b"new-runtime");
            assert_eq!(mode_of(&staged.path_of(RUNTIME)), 0o644);
            assert_eq!(fs::read(staged.path_of(LICENSE)).unwrap(), b"license");
            assert_eq!(
                staged.companions().collect::<Vec<_>>(),
                vec![RUNTIME, LICENSE]
            );
        }

        #[test]
        fn a_release_missing_its_runtime_companion_is_rejected() {
            let executable_only = targz(&[file("tracedecay", b"new-executable")]);

            let error = stage(&executable_only, &linux_members()).unwrap_err();

            let message = error.to_string();
            assert!(
                message.contains(
                    "missing required member(s): libonnxruntime.so.1, onnxruntime-LICENSE"
                ),
                "{message}"
            );
        }

        #[test]
        fn an_executable_only_release_satisfies_a_platform_without_companions() {
            let executable_only = targz(&[file("tracedecay", b"new-executable")]);

            let staged = stage(&executable_only, &[EXECUTABLE_MEMBER]).unwrap();

            assert_eq!(fs::read(staged.executable()).unwrap(), b"new-executable");
            assert_eq!(staged.companions().count(), 0);
        }

        #[test]
        fn duplicate_executable_entries_are_ambiguous_not_a_choice() {
            let duplicated = targz(&[
                file("tracedecay", b"first"),
                file("libonnxruntime.so.1", b"runtime"),
                file("onnxruntime-LICENSE", b"license"),
                file("tracedecay", b"second"),
            ]);

            let error = stage(&duplicated, &linux_members()).unwrap_err();

            assert!(
                error
                    .to_string()
                    .contains("duplicate entries for 'tracedecay'"),
                "{error}"
            );
        }

        #[test]
        fn a_required_member_that_is_not_a_regular_file_is_rejected() {
            let symlinked_runtime = targz(&[
                file("tracedecay", b"new-executable"),
                Entry {
                    path: "libonnxruntime.so.1",
                    contents: b"",
                    kind: EntryType::Symlink,
                },
                file("onnxruntime-LICENSE", b"license"),
            ]);

            let error = stage(&symlinked_runtime, &linux_members()).unwrap_err();

            assert!(
                error
                    .to_string()
                    .contains("'libonnxruntime.so.1' is not a regular file"),
                "{error}"
            );
        }

        #[test]
        fn only_flat_entries_can_name_a_required_member() {
            use std::path::Path;

            let members = linux_members();
            assert_eq!(
                intended_member(Path::new("tracedecay"), &members),
                Some(EXECUTABLE_MEMBER)
            );
            assert_eq!(
                intended_member(Path::new("./libonnxruntime.so.1"), &members),
                Some(RUNTIME)
            );
            assert_eq!(intended_member(Path::new("bin/tracedecay"), &members), None);
            assert_eq!(intended_member(Path::new("../tracedecay"), &members), None);
            assert_eq!(intended_member(Path::new("/tracedecay"), &members), None);
            assert_eq!(intended_member(Path::new("README"), &members), None);
        }

        #[test]
        fn nested_and_unknown_entries_are_never_unpacked() {
            let scratch = tempfile::tempdir().unwrap();
            let archive = targz(&[
                file("tracedecay", b"new-executable"),
                file("bin/tracedecay", b"nested-decoy"),
                file("libonnxruntime.so.1", b"runtime"),
                file("onnxruntime-LICENSE", b"license"),
                file("README", b"unrequested"),
            ]);

            extract_targz(Cursor::new(&archive[..]), scratch.path(), &linux_members()).unwrap();

            let mut staged: Vec<String> = fs::read_dir(scratch.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            staged.sort();
            assert_eq!(
                staged,
                ["libonnxruntime.so.1", "onnxruntime-LICENSE", "tracedecay"]
            );
            assert_eq!(
                fs::read(scratch.path().join("tracedecay")).unwrap(),
                b"new-executable"
            );
        }

        #[test]
        fn publishing_places_companions_where_the_executable_resolves_them() {
            let staged = stage(&complete_release(), &linux_members()).unwrap();
            let install = tempfile::tempdir().unwrap();
            let executable = install.path().join("tracedecay");
            fs::write(&executable, b"old-executable").unwrap();
            fs::write(install.path().join("libonnxruntime.so.1"), b"old-runtime").unwrap();

            publish_release_at(&staged, &executable).unwrap();

            assert_eq!(fs::read(&executable).unwrap(), b"new-executable");
            assert_eq!(mode_of(&executable), 0o755);
            let runtime = install.path().join("libonnxruntime.so.1");
            assert_eq!(fs::read(&runtime).unwrap(), b"new-runtime");
            assert_eq!(mode_of(&runtime), 0o644);
            assert_eq!(
                fs::read(install.path().join("onnxruntime-LICENSE")).unwrap(),
                b"license"
            );
            let mut published: Vec<String> = fs::read_dir(install.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            published.sort();
            assert_eq!(
                published,
                ["libonnxruntime.so.1", "onnxruntime-LICENSE", "tracedecay"],
                "no staging sibling may outlive publication"
            );
        }

        #[test]
        fn a_companion_that_cannot_be_published_leaves_the_old_executable_in_place() {
            let staged = stage(&complete_release(), &linux_members()).unwrap();
            let install = tempfile::tempdir().unwrap();
            let executable = install.path().join("tracedecay");
            fs::write(&executable, b"old-executable").unwrap();
            fs::create_dir(install.path().join("libonnxruntime.so.1")).unwrap();

            let error = publish_release_at(&staged, &executable).unwrap_err();

            assert!(
                error.to_string().contains("cannot replace release member"),
                "{error}"
            );
            assert_eq!(
                fs::read(&executable).unwrap(),
                b"old-executable",
                "companions publish before the entry point, so a companion failure never \
                 leaves a new executable beside an old runtime"
            );
        }

        #[test]
        fn a_failed_companion_publication_leaves_the_target_untouched() {
            let staged = stage(&complete_release(), &linux_members()).unwrap();
            let install = tempfile::tempdir().unwrap();
            // The target name is occupied by a directory, so the rename over it
            // must fail after the sibling was fully staged.
            let occupied = install.path().join("libonnxruntime.so.1");
            fs::create_dir(&occupied).unwrap();
            fs::write(occupied.join("marker"), b"keep").unwrap();

            let error = publish_member(&staged.path_of(RUNTIME), &occupied, 0o644).unwrap_err();

            assert!(
                error.to_string().contains("cannot replace release member"),
                "{error}"
            );
            assert_eq!(fs::read(occupied.join("marker")).unwrap(), b"keep");
            let leftovers: Vec<_> = fs::read_dir(install.path())
                .unwrap()
                .map(|entry| entry.unwrap().file_name())
                .filter(|name| name != "libonnxruntime.so.1")
                .collect();
            assert!(
                leftovers.is_empty(),
                "staging sibling leaked: {leftovers:?}"
            );
        }

        // ── Streamed download into owned staging ───────────────────────

        const ASSET: &str = "tracedecay-v9.9.9-x86_64-linux.tar.gz";

        /// Serves each `(path, body)` over plain HTTP/1.1 on a loopback port
        /// for as long as the test process lives.
        fn serve(responses: Vec<(&'static str, Vec<u8>)>) -> String {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { break };
                    let mut request = [0u8; 4096];
                    let read = stream.read(&mut request).unwrap_or(0);
                    let head = String::from_utf8_lossy(&request[..read]).into_owned();
                    let path = head.split_whitespace().nth(1).unwrap_or("").to_owned();
                    let body = responses
                        .iter()
                        .find(|(served, _)| *served == path)
                        .map_or(&[][..], |(_, body)| body.as_slice());
                    let _ = write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                        body.len()
                    );
                    let _ = stream.write_all(body);
                }
            });
            base
        }

        fn manifest_for(archive: &[u8]) -> Vec<u8> {
            format!("{}  {ASSET}\n", hex::encode(Sha256::digest(archive))).into_bytes()
        }

        fn download(base: &str, asset_size: u64, checksums_size: u64) -> ReleaseDownload {
            ReleaseDownload {
                asset_name: ASSET.to_owned(),
                asset_url: format!("{base}/archive"),
                asset_size,
                checksums_url: format!("{base}/SHA256SUMS"),
                checksums_size,
            }
        }

        /// A scratch directory inside a test-owned parent, so the test can
        /// prove the attempt removed its own staging afterwards.
        fn scratch_in(parent: &std::path::Path) -> tempfile::TempDir {
            tempfile::Builder::new()
                .prefix("tracedecay-upgrade-")
                .tempdir_in(parent)
                .unwrap()
        }

        fn is_empty_dir(path: &std::path::Path) -> bool {
            fs::read_dir(path).unwrap().next().is_none()
        }

        #[test]
        fn a_verified_archive_is_streamed_into_owned_scratch_and_staged() {
            let archive = complete_release();
            let manifest = manifest_for(&archive);
            let base = serve(vec![
                ("/SHA256SUMS", manifest.clone()),
                ("/archive", archive.clone()),
            ]);
            let parent = tempfile::tempdir().unwrap();
            let download = download(&base, archive.len() as u64, manifest.len() as u64);

            let staged =
                stage_release_in(scratch_in(parent.path()), &download, &linux_members()).unwrap();

            assert!(staged.scratch.path().starts_with(parent.path()));
            assert_eq!(
                fs::read(staged.scratch.path().join("archive")).unwrap(),
                archive,
                "the verified bytes are the ones extracted"
            );
            assert_eq!(fs::read(staged.executable()).unwrap(), b"new-executable");
            assert_eq!(fs::read(staged.path_of(RUNTIME)).unwrap(), b"new-runtime");
            drop(staged);
            assert!(
                is_empty_dir(parent.path()),
                "the attempt must remove its own scratch on handoff"
            );
        }

        #[test]
        fn an_archive_running_past_its_advertised_size_is_refused() {
            let archive = complete_release();
            let manifest = manifest_for(&archive);
            let base = serve(vec![
                ("/SHA256SUMS", manifest.clone()),
                ("/archive", archive.clone()),
            ]);
            let parent = tempfile::tempdir().unwrap();
            let download = download(&base, archive.len() as u64 - 1, manifest.len() as u64);

            let error = stage_release_in(scratch_in(parent.path()), &download, &linux_members())
                .unwrap_err();

            assert!(error.to_string().contains("exceeds the"), "{error}");
            assert!(
                is_empty_dir(parent.path()),
                "failed attempts release their scratch"
            );
        }

        #[test]
        fn a_truncated_archive_is_refused() {
            let archive = complete_release();
            let manifest = manifest_for(&archive);
            let base = serve(vec![
                ("/SHA256SUMS", manifest.clone()),
                ("/archive", archive.clone()),
            ]);
            let parent = tempfile::tempdir().unwrap();
            let download = download(&base, archive.len() as u64 + 1, manifest.len() as u64);

            let error = stage_release_in(scratch_in(parent.path()), &download, &linux_members())
                .unwrap_err();

            assert!(error.to_string().contains("ended after"), "{error}");
            assert!(is_empty_dir(parent.path()));
        }

        #[test]
        fn a_checksum_mismatch_refuses_before_extraction() {
            let archive = complete_release();
            let manifest = manifest_for(b"some other release");
            let base = serve(vec![
                ("/SHA256SUMS", manifest.clone()),
                ("/archive", archive.clone()),
            ]);
            let parent = tempfile::tempdir().unwrap();
            let download = download(&base, archive.len() as u64, manifest.len() as u64);

            let error = stage_release_in(scratch_in(parent.path()), &download, &linux_members())
                .unwrap_err();

            assert!(error.to_string().contains("checksum mismatch"), "{error}");
            assert!(is_empty_dir(parent.path()));
        }

        #[test]
        fn an_oversized_checksum_manifest_is_refused() {
            let archive = complete_release();
            let manifest = manifest_for(&archive);
            let base = serve(vec![
                ("/SHA256SUMS", manifest.clone()),
                ("/archive", archive.clone()),
            ]);
            let parent = tempfile::tempdir().unwrap();
            let download = download(&base, archive.len() as u64, manifest.len() as u64 - 1);

            let error = stage_release_in(scratch_in(parent.path()), &download, &linux_members())
                .unwrap_err();

            assert!(
                error.to_string().contains("checksum manifest exceeds"),
                "{error}"
            );
            assert!(is_empty_dir(parent.path()));
        }
    }
}
