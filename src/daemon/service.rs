use std::fmt::Write;
#[cfg(target_os = "macos")]
use std::os::unix::fs::PermissionsExt;
#[cfg(unix)]
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::errors::{Result, TraceDecayError};

use super::SOCKET_ENV;

const LAUNCHD_LABEL: &str = "com.tracedecay.daemon";
const LAUNCHD_PLIST_NAME: &str = "com.tracedecay.daemon.plist";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonServiceSpec {
    pub tracedecay_bin: PathBuf,
    pub socket_path: PathBuf,
    pub data_dir_override: Option<PathBuf>,
}

impl DaemonServiceSpec {
    pub fn render_systemd_user_unit(&self) -> String {
        let service_path = daemon_service_path_env(&self.tracedecay_bin);
        format!(
            "[Unit]\n\
             Description=TraceDecay daemon\n\
             After=network.target\n\
             \n\
             [Service]\n\
             Type=simple\n\
             Environment=\"PATH={}\"\n\
             ExecStart={} daemon run --socket {}\n\
             Restart=on-failure\n\
             RestartSec=2\n\
             \n\
             [Install]\n\
             WantedBy=default.target\n",
            systemd_escape_env_value(&service_path),
            self.tracedecay_bin.display(),
            self.socket_path.display()
        )
    }

    pub fn render_launchd_plist(&self) -> Result<String> {
        if !self.tracedecay_bin.is_absolute() {
            return Err(TraceDecayError::Config {
                message: format!(
                    "launchd daemon service requires an absolute tracedecay binary path, got '{}'",
                    self.tracedecay_bin.display()
                ),
            });
        }

        let home = home_for_service_env()?;
        let data_dir = match &self.data_dir_override {
            Some(dir) => dir.clone(),
            None => tracedecay_data_dir()?,
        };
        let mut env_entries = vec![
            (
                "PATH".to_string(),
                daemon_service_path_env(&self.tracedecay_bin),
            ),
            ("HOME".to_string(), home.display().to_string()),
        ];
        if let Some(data_dir_override) = &self.data_dir_override {
            env_entries.push((
                crate::config::USER_DATA_DIR_ENV.to_string(),
                data_dir_override.display().to_string(),
            ));
        }

        let mut environment = String::new();
        for (key, value) in env_entries {
            let _ = write!(
                environment,
                "    <key>{}</key>\n    <string>{}</string>\n",
                plist_xml_escape(&key),
                plist_xml_escape(&value)
            );
        }

        Ok(format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\"\n\
             \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
             <plist version=\"1.0\">\n\
             <dict>\n\
               <key>Label</key>\n\
               <string>{label}</string>\n\
             \n\
               <key>ProgramArguments</key>\n\
               <array>\n\
                 <string>{bin}</string>\n\
                 <string>daemon</string>\n\
                 <string>run</string>\n\
                 <string>--socket</string>\n\
                 <string>{socket}</string>\n\
               </array>\n\
             \n\
               <key>EnvironmentVariables</key>\n\
               <dict>\n\
             {environment}\
               </dict>\n\
             \n\
               <key>RunAtLoad</key>\n\
               <true/>\n\
             \n\
               <key>KeepAlive</key>\n\
               <dict>\n\
                 <key>SuccessfulExit</key>\n\
                 <false/>\n\
               </dict>\n\
             \n\
               <key>ThrottleInterval</key>\n\
               <integer>2</integer>\n\
             \n\
               <key>StandardOutPath</key>\n\
               <string>{stdout}</string>\n\
             \n\
               <key>StandardErrorPath</key>\n\
               <string>{stderr}</string>\n\
             </dict>\n\
             </plist>\n",
            label = plist_xml_escape(LAUNCHD_LABEL),
            bin = plist_xml_escape(&self.tracedecay_bin.display().to_string()),
            socket = plist_xml_escape(&self.socket_path.display().to_string()),
            stdout = plist_xml_escape(&data_dir.join("daemon.out.log").display().to_string()),
            stderr = plist_xml_escape(&data_dir.join("daemon.err.log").display().to_string()),
        ))
    }

    fn render_unit(&self) -> Result<String> {
        match ServiceRunner::current()? {
            ServiceRunner::Systemd => Ok(self.render_systemd_user_unit()),
            ServiceRunner::Launchd => self.render_launchd_plist(),
        }
    }
}

fn daemon_service_path_env(tracedecay_bin: &Path) -> String {
    let mut dirs = Vec::new();

    if let Some(parent) = tracedecay_bin
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        push_unique_path(&mut dirs, parent.to_path_buf());
    }

    if let Some(home) = std::env::var_os("HOME").filter(|home| !home.is_empty()) {
        let home = PathBuf::from(home);
        push_unique_path(&mut dirs, home.join(".cargo/bin"));
        push_unique_path(&mut dirs, home.join(".local/bin"));
    }

    if let Some(path) = std::env::var_os("PATH").filter(|path| !path.is_empty()) {
        for dir in std::env::split_paths(&path) {
            push_unique_path(&mut dirs, dir);
        }
    }

    for dir in [
        "/usr/local/sbin",
        "/usr/local/bin",
        "/usr/sbin",
        "/usr/bin",
        "/sbin",
        "/bin",
        "/opt/homebrew/bin",
    ] {
        push_unique_path(&mut dirs, PathBuf::from(dir));
    }

    std::env::join_paths(&dirs).map_or_else(
        |_| {
            dirs.iter()
                .map(|path| path.to_string_lossy())
                .collect::<Vec<_>>()
                .join(":")
        },
        |path| path.to_string_lossy().into_owned(),
    )
}

fn push_unique_path(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if path.as_os_str().is_empty() || paths.iter().any(|existing| existing == &path) {
        return;
    }
    paths.push(path);
}

fn systemd_escape_env_value(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('%', "%%")
}

fn plist_xml_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn plist_xml_unescape(value: &str) -> String {
    value
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&amp;", "&")
}

fn home_for_service_env() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .or_else(dirs::home_dir)
        .ok_or_else(|| TraceDecayError::Config {
            message: "could not determine home directory for daemon service".to_string(),
        })
}

fn tracedecay_data_dir() -> Result<PathBuf> {
    crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })
}

pub fn default_socket_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(SOCKET_ENV).filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    Ok(tracedecay_data_dir()?.join("daemon.sock"))
}

pub fn socket_path_or_default(socket: Option<String>) -> Result<PathBuf> {
    socket.map_or_else(default_socket_path, |path| Ok(PathBuf::from(path)))
}

pub fn service_spec(
    tracedecay_bin: impl Into<PathBuf>,
    socket: Option<String>,
) -> Result<DaemonServiceSpec> {
    Ok(DaemonServiceSpec {
        tracedecay_bin: tracedecay_bin.into(),
        socket_path: socket_path_or_default(socket)?,
        data_dir_override: std::env::var_os(crate::config::USER_DATA_DIR_ENV)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from),
    })
}

pub fn install_service(spec: &DaemonServiceSpec, start: bool) -> Result<PathBuf> {
    let runner = ServiceRunner::current()?;
    let service_path = write_service_unit(spec)?;
    runner.install(&service_path, start, &spec.socket_path)?;

    Ok(service_path)
}

pub fn refresh_service(spec: &DaemonServiceSpec) -> Result<PathBuf> {
    let runner = ServiceRunner::current()?;
    let service_path = write_service_unit(spec)?;
    runner.refresh(&service_path, &spec.socket_path)?;
    Ok(service_path)
}

pub fn refresh_installed_service(spec: &DaemonServiceSpec) -> Result<Option<PathBuf>> {
    let service_path = service_unit_path()?;
    if !service_path.exists() {
        return Ok(None);
    }
    let unit = read_service_unit(&service_path)?;
    let mut refreshed_spec = spec.clone();
    if let Some(socket_path) = socket_path_from_unit_text(&unit) {
        refreshed_spec.socket_path = socket_path;
    }
    if matches!(ServiceRunner::current(), Ok(ServiceRunner::Launchd)) {
        // The installed plist is the source of truth for the daemon's data
        // directory; the refreshing shell may not have the override set.
        refreshed_spec.data_dir_override =
            launchd_plist_env_value(&unit, crate::config::USER_DATA_DIR_ENV).map(PathBuf::from);
    }
    refresh_service(&refreshed_spec).map(Some)
}

fn write_service_unit(spec: &DaemonServiceSpec) -> Result<PathBuf> {
    let service_path = service_unit_path()?;
    let parent = service_path
        .parent()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("service path '{}' has no parent", service_path.display()),
        })?;
    std::fs::create_dir_all(parent).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to create service directory '{}': {e}",
            parent.display()
        ),
    })?;
    std::fs::write(&service_path, spec.render_unit()?).map_err(|e| TraceDecayError::Config {
        message: format!("failed to write service '{}': {e}", service_path.display()),
    })?;
    #[cfg(target_os = "macos")]
    std::fs::set_permissions(&service_path, std::fs::Permissions::from_mode(0o644)).map_err(
        |e| TraceDecayError::Config {
            message: format!(
                "failed to set service permissions '{}': {e}",
                service_path.display()
            ),
        },
    )?;

    Ok(service_path)
}

pub fn installed_service_socket_path() -> Result<Option<PathBuf>> {
    let service_path = service_unit_path()?;
    if !service_path.exists() {
        return Ok(None);
    }
    Ok(socket_path_from_unit_text(&read_service_unit(
        &service_path,
    )?))
}

fn read_service_unit(service_path: &Path) -> Result<String> {
    std::fs::read_to_string(service_path).map_err(|e| TraceDecayError::Config {
        message: format!("failed to read service '{}': {e}", service_path.display()),
    })
}

fn socket_path_from_args<'a>(mut args: impl Iterator<Item = &'a str>) -> Option<PathBuf> {
    while let Some(arg) = args.next() {
        if arg == "--socket" {
            return args.next().map(PathBuf::from);
        }
        if let Some(path) = arg.strip_prefix("--socket=") {
            return Some(PathBuf::from(path));
        }
    }
    None
}

fn socket_path_from_service_unit(unit: &str) -> Option<PathBuf> {
    unit.lines()
        .filter_map(|line| line.trim().strip_prefix("ExecStart="))
        .find_map(|exec_start| socket_path_from_args(exec_start.split_whitespace()))
}

fn socket_path_from_launchd_plist(plist: &str) -> Option<PathBuf> {
    let program_arguments_start = plist.find("<key>ProgramArguments</key>")?;
    let arguments_text = &plist[program_arguments_start..];
    let array_start = arguments_text.find("<array>")? + "<array>".len();
    let after_array_start = &arguments_text[array_start..];
    let array_end = after_array_start.find("</array>")?;
    let array_text = &after_array_start[..array_end];
    let strings = plist_string_values(array_text);

    socket_path_from_args(strings.iter().map(String::as_str))
}

fn launchd_plist_env_value(plist: &str, name: &str) -> Option<String> {
    let env_start = plist.find("<key>EnvironmentVariables</key>")?;
    let after_env = &plist[env_start..];
    let dict_start = after_env.find("<dict>")? + "<dict>".len();
    let after_dict_start = &after_env[dict_start..];
    let dict_end = after_dict_start.find("</dict>")?;
    let dict_text = &after_dict_start[..dict_end];

    let key_tag = format!("<key>{}</key>", plist_xml_escape(name));
    let key_end = dict_text.find(&key_tag)? + key_tag.len();
    plist_string_values(&dict_text[key_end..])
        .into_iter()
        .next()
}

fn plist_string_values(text: &str) -> Vec<String> {
    let mut values = Vec::new();
    let mut remaining = text;
    while let Some(start) = remaining.find("<string>") {
        let value_start = start + "<string>".len();
        let after_start = &remaining[value_start..];
        let Some(end) = after_start.find("</string>") else {
            break;
        };
        values.push(plist_xml_unescape(&after_start[..end]));
        remaining = &after_start[end + "</string>".len()..];
    }
    values
}

fn socket_path_from_unit_text(unit: &str) -> Option<PathBuf> {
    match ServiceRunner::current().ok()? {
        ServiceRunner::Systemd => socket_path_from_service_unit(unit),
        ServiceRunner::Launchd => socket_path_from_launchd_plist(unit),
    }
}

pub fn uninstall_service(stop: bool) -> Result<PathBuf> {
    let runner = ServiceRunner::current()?;
    let service_path = service_unit_path()?;
    runner.before_uninstall(stop)?;
    match std::fs::remove_file(&service_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(TraceDecayError::Config {
                message: format!("failed to remove service '{}': {e}", service_path.display()),
            });
        }
    }
    runner.after_uninstall(stop);
    Ok(service_path)
}

pub fn service_status(socket_path: &Path) -> String {
    let socket_state = daemon_socket_state(socket_path);
    let service = service_unit_path().map_or_else(
        |e| format!("unavailable: {e}"),
        |path| path.display().to_string(),
    );
    let runner = ServiceRunner::current();
    let detail = runner
        .as_ref()
        .ok()
        .and_then(ServiceRunner::service_detail_hint)
        .map(|hint| format!("service-detail: {hint}\n"))
        .unwrap_or_default();
    let logs = runner.map_or_else(|e| format!("unavailable: {e}"), |runner| runner.log_hint());
    format!(
        "service: {}\nsocket: {} ({})\n{}logs: {}\n",
        service,
        socket_path.display(),
        socket_state,
        detail,
        logs,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DaemonSocketState {
    Missing,
    Connectable,
    #[cfg(unix)]
    Stale,
    #[cfg(unix)]
    PresentNotAccessible,
    #[cfg(unix)]
    PresentUnreachable,
    #[cfg(not(unix))]
    Present,
}

impl std::fmt::Display for DaemonSocketState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = match self {
            Self::Missing => "missing",
            Self::Connectable => "connectable",
            #[cfg(unix)]
            Self::Stale => "stale",
            #[cfg(unix)]
            Self::PresentNotAccessible => "present but not accessible",
            #[cfg(unix)]
            Self::PresentUnreachable => "present but unreachable",
            #[cfg(not(unix))]
            Self::Present => "present",
        };
        f.write_str(text)
    }
}

#[cfg(unix)]
fn daemon_socket_state(socket_path: &Path) -> DaemonSocketState {
    if !socket_path.exists() {
        return DaemonSocketState::Missing;
    }
    match StdUnixStream::connect(socket_path) {
        Ok(_) => DaemonSocketState::Connectable,
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => DaemonSocketState::Stale,
        Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
            DaemonSocketState::PresentNotAccessible
        }
        Err(_) => DaemonSocketState::PresentUnreachable,
    }
}

#[cfg(not(unix))]
fn daemon_socket_state(socket_path: &Path) -> DaemonSocketState {
    if socket_path.exists() {
        DaemonSocketState::Present
    } else {
        DaemonSocketState::Missing
    }
}

/// Both variants exist on every platform so that `match` dispatch stays
/// exhaustive everywhere; `current()` is the only constructor and returns an
/// error on platforms without a supported service manager.
enum ServiceRunner {
    Systemd,
    Launchd,
}

impl ServiceRunner {
    fn current() -> Result<Self> {
        if cfg!(target_os = "linux") {
            Ok(Self::Systemd)
        } else if cfg!(target_os = "macos") {
            Ok(Self::Launchd)
        } else {
            Err(unsupported_service_platform())
        }
    }

    fn install(&self, service_path: &Path, start: bool, socket_path: &Path) -> Result<()> {
        match self {
            Self::Systemd => {
                if start {
                    run_systemctl(&["daemon-reload"])?;
                    run_systemctl(&["enable", "--now", super::SERVICE_NAME])?;
                }
                Ok(())
            }
            Self::Launchd => launchd_install(service_path, start, socket_path),
        }
    }

    fn refresh(&self, service_path: &Path, socket_path: &Path) -> Result<()> {
        match self {
            Self::Systemd => {
                run_systemctl(&["daemon-reload"])?;
                run_systemctl(&["enable", super::SERVICE_NAME])?;
                run_systemctl(&["restart", super::SERVICE_NAME])?;
                Ok(())
            }
            Self::Launchd => launchd_refresh(service_path, socket_path),
        }
    }

    fn before_uninstall(&self, stop: bool) -> Result<()> {
        match self {
            Self::Systemd => {
                if stop {
                    let _ = run_systemctl(&["disable", "--now", super::SERVICE_NAME]);
                }
                Ok(())
            }
            Self::Launchd => launchd_before_uninstall(stop),
        }
    }

    fn after_uninstall(&self, stop: bool) {
        match self {
            Self::Systemd => {
                if stop {
                    let _ = run_systemctl(&["daemon-reload"]);
                }
            }
            Self::Launchd => {}
        }
    }

    fn log_hint(&self) -> String {
        match self {
            Self::Systemd => format!("journalctl --user -u {} -f", super::SERVICE_NAME),
            Self::Launchd => crate::config::user_data_dir().map_or_else(
                || "tail -f <tracedecay-data-dir>/daemon.err.log".to_string(),
                |dir| format!("tail -f \"{}\"", dir.join("daemon.err.log").display()),
            ),
        }
    }

    fn service_detail_hint(&self) -> Option<String> {
        match self {
            Self::Systemd => None,
            Self::Launchd => launchd_service_target()
                .ok()
                .map(|target| format!("launchctl print {target}")),
        }
    }
}

fn service_unit_path() -> Result<PathBuf> {
    match ServiceRunner::current()? {
        ServiceRunner::Systemd => systemd_user_service_path(),
        ServiceRunner::Launchd => launchd_user_service_path(),
    }
}

fn systemd_user_service_path() -> Result<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .ok_or_else(|| TraceDecayError::Config {
            message: "could not determine XDG config directory".to_string(),
        })?;
    Ok(config_home.join("systemd/user").join(super::SERVICE_NAME))
}

fn launchd_user_service_path() -> Result<PathBuf> {
    let home = home_for_service_env()?;
    Ok(home.join("Library/LaunchAgents").join(LAUNCHD_PLIST_NAME))
}

fn run_systemctl(args: &[&str]) -> Result<()> {
    let output = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .output()
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to run systemctl --user {}: {e}", args.join(" ")),
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(TraceDecayError::Config {
        message: format!(
            "systemctl --user {} failed with status {}\n{}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ),
    })
}

fn unsupported_service_platform() -> TraceDecayError {
    TraceDecayError::Config {
        message: "daemon service install is currently supported on Linux systemd user services and macOS launchd agents"
            .to_string(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LaunchctlFailureMode {
    /// Propagate any failure.
    Fail,
    /// Tolerate "service is not loaded" failures (e.g. `bootout` before the
    /// agent was ever bootstrapped); propagate everything else.
    TolerateNotLoaded,
    /// Best effort: ignore any failure.
    Ignore,
}

#[derive(Debug, PartialEq, Eq)]
struct LaunchdCommand {
    args: Vec<String>,
    failure_mode: LaunchctlFailureMode,
}

impl LaunchdCommand {
    fn new(args: &[&str], failure_mode: LaunchctlFailureMode) -> Self {
        Self {
            args: args.iter().map(|arg| String::from(*arg)).collect(),
            failure_mode,
        }
    }
}

/// Commands that (re)start the launchd agent. Booting the service out first
/// (tolerating "not loaded") makes the sequence idempotent, and enabling
/// before bootstrap clears any persisted disabled state so the bootstrap
/// cannot be rejected.
fn launchd_start_command_plan(
    domain: &str,
    target: &str,
    service_path: &Path,
) -> Vec<LaunchdCommand> {
    vec![
        LaunchdCommand::new(
            &["bootout", target],
            LaunchctlFailureMode::TolerateNotLoaded,
        ),
        LaunchdCommand::new(&["enable", target], LaunchctlFailureMode::Fail),
        LaunchdCommand::new(
            &["bootstrap", domain, &service_path.display().to_string()],
            LaunchctlFailureMode::Fail,
        ),
        LaunchdCommand::new(&["kickstart", "-k", target], LaunchctlFailureMode::Fail),
    ]
}

fn launchd_uninstall_command_plan(target: &str) -> Vec<LaunchdCommand> {
    vec![
        LaunchdCommand::new(
            &["bootout", target],
            LaunchctlFailureMode::TolerateNotLoaded,
        ),
        // Persist the stopped state so launchd does not revive the agent at
        // the next login; best effort because the plist is removed anyway.
        LaunchdCommand::new(&["disable", target], LaunchctlFailureMode::Ignore),
    ]
}

fn run_launchd_commands(commands: &[LaunchdCommand]) -> Result<()> {
    for command in commands {
        let args: Vec<&str> = command.args.iter().map(String::as_str).collect();
        match command.failure_mode {
            LaunchctlFailureMode::Fail => {
                run_launchctl(&args)?;
            }
            LaunchctlFailureMode::TolerateNotLoaded => run_launchctl_allow_not_loaded(&args)?,
            LaunchctlFailureMode::Ignore => {
                let _ = run_launchctl(&args);
            }
        }
    }
    Ok(())
}

fn launchd_domain() -> Result<String> {
    let output = Command::new("id")
        .arg("-u")
        .output()
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to determine user id for launchd domain: {e}"),
        })?;
    if !output.status.success() {
        return Err(TraceDecayError::Config {
            message: format!(
                "id -u failed with status {}\n{}",
                output.status,
                String::from_utf8_lossy(&output.stderr)
            ),
        });
    }
    let uid = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if uid.is_empty() {
        return Err(TraceDecayError::Config {
            message: "id -u returned an empty user id".to_string(),
        });
    }
    Ok(format!("gui/{uid}"))
}

fn launchd_service_target() -> Result<String> {
    Ok(format!("{}/{}", launchd_domain()?, LAUNCHD_LABEL))
}

fn ensure_launchd_runtime_dirs() -> Result<()> {
    let data_dir = tracedecay_data_dir()?;
    std::fs::create_dir_all(&data_dir).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to create daemon data directory '{}': {e}",
            data_dir.display()
        ),
    })
}

fn launchd_install(service_path: &Path, start: bool, socket_path: &Path) -> Result<()> {
    ensure_launchd_runtime_dirs()?;
    let target = launchd_service_target()?;
    if !start {
        // launchd bootstraps every plist in ~/Library/LaunchAgents at login,
        // so persist a disabled state to keep --no-start meaning "do not run".
        run_launchctl(&["disable", &target])?;
        return Ok(());
    }
    launchd_start(&target, service_path, socket_path)
}

fn launchd_refresh(service_path: &Path, socket_path: &Path) -> Result<()> {
    ensure_launchd_runtime_dirs()?;
    let target = launchd_service_target()?;
    launchd_start(&target, service_path, socket_path)
}

fn launchd_start(target: &str, service_path: &Path, socket_path: &Path) -> Result<()> {
    let domain = launchd_domain()?;
    run_launchd_commands(&launchd_start_command_plan(&domain, target, service_path))?;
    verify_launchd_started(target, socket_path)
}

fn launchd_before_uninstall(stop: bool) -> Result<()> {
    if !stop {
        return Ok(());
    }
    let target = launchd_service_target()?;
    run_launchd_commands(&launchd_uninstall_command_plan(&target))
}

fn verify_launchd_started(target: &str, socket_path: &Path) -> Result<()> {
    if daemon_socket_state(socket_path) == DaemonSocketState::Connectable {
        return Ok(());
    }
    run_launchctl(&["print", target]).map(|_| ())
}

fn launchctl_spawn(args: &[&str]) -> Result<std::process::Output> {
    Command::new("launchctl")
        .args(args)
        .output()
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to run launchctl {}: {e}", args.join(" ")),
        })
}

fn launchctl_failure(args: &[&str], output: &std::process::Output) -> TraceDecayError {
    TraceDecayError::Config {
        message: format!(
            "launchctl {} failed with status {}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}

fn run_launchctl(args: &[&str]) -> Result<std::process::Output> {
    let output = launchctl_spawn(args)?;
    if output.status.success() {
        return Ok(output);
    }
    Err(launchctl_failure(args, &output))
}

fn run_launchctl_allow_not_loaded(args: &[&str]) -> Result<()> {
    let output = launchctl_spawn(args)?;
    if output.status.success()
        || launchctl_stderr_is_not_loaded(&String::from_utf8_lossy(&output.stderr))
    {
        return Ok(());
    }
    Err(launchctl_failure(args, &output))
}

fn launchctl_stderr_is_not_loaded(stderr: &str) -> bool {
    [
        "No such process",
        "No such file or directory",
        "Could not find service",
        "Could not find specified service",
        "service is not loaded",
    ]
    .iter()
    .any(|needle| stderr.contains(needle))
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::ffi::{OsStr, OsString};
    use std::path::PathBuf;
    use std::sync::Mutex;

    #[cfg(target_os = "linux")]
    use std::os::unix::fs::PermissionsExt;
    #[cfg(unix)]
    use tempfile::TempDir;

    use super::{DaemonServiceSpec, LaunchctlFailureMode, LaunchdCommand};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<OsStr>) -> Self {
            let previous = std::env::var_os(key);
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn unset(key: &'static str) -> Self {
            let previous = std::env::var_os(key);
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            if let Some(previous) = self.previous.take() {
                std::env::set_var(self.key, previous);
            } else {
                std::env::remove_var(self.key);
            }
        }
    }

    struct CurrentDirGuard {
        previous: PathBuf,
    }

    impl CurrentDirGuard {
        fn set(path: impl AsRef<std::path::Path>) -> Self {
            let previous = std::env::current_dir().expect("current dir");
            std::env::set_current_dir(path).expect("set current dir");
            Self { previous }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.previous).expect("restore current dir");
        }
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn service_status_includes_journalctl_debug_command() {
        let status = super::service_status(&PathBuf::from("/tmp/tracedecay.sock"));

        assert!(status.contains("logs: journalctl --user -u tracedecay.service -f"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn service_status_includes_launchd_debug_commands() {
        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let profile = tempfile::TempDir::new().expect("profile temp dir");
        let _data_dir_guard = EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, profile.path());

        let status = super::service_status(&PathBuf::from("/tmp/tracedecay.sock"));

        assert!(status.contains("service-detail: launchctl print gui/"));
        assert!(status.contains("/com.tracedecay.daemon"));
        assert!(status.contains(&format!(
            "logs: tail -f \"{}\"",
            profile.path().join("daemon.err.log").display()
        )));
    }

    #[cfg(unix)]
    #[test]
    fn service_status_reports_missing_socket() {
        let dir = TempDir::new().expect("temp dir");
        let socket = dir.path().join("missing.sock");

        let status = super::service_status(&socket);

        assert!(
            status.contains(&format!("socket: {} (missing)", socket.display())),
            "status should report missing socket, got:\n{status}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn service_status_reports_unconnectable_socket_file() {
        let dir = TempDir::new().expect("temp dir");
        let socket = dir.path().join("unconnectable.sock");
        std::fs::write(&socket, "").expect("unconnectable socket placeholder");

        let status = super::service_status(&socket);

        assert!(
            status.contains(&format!("socket: {} (stale)", socket.display()))
                || status.contains(&format!(
                    "socket: {} (present but unreachable)",
                    socket.display()
                )),
            "status should report an unconnectable socket, got:\n{status}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn service_status_reports_connectable_socket() {
        let dir = TempDir::new().expect("temp dir");
        let socket = dir.path().join("daemon.sock");
        let _listener = std::os::unix::net::UnixListener::bind(&socket).expect("bind socket");

        let status = super::service_status(&socket);

        assert!(
            status.contains(&format!("socket: {} (connectable)", socket.display())),
            "status should report connectable socket, got:\n{status}"
        );
    }

    #[test]
    fn user_service_runs_daemon_with_socket_path() {
        let spec = DaemonServiceSpec {
            tracedecay_bin: PathBuf::from("/usr/local/bin/tracedecay"),
            socket_path: PathBuf::from("/tmp/tracedecay.sock"),
            data_dir_override: None,
        };

        let unit = spec.render_systemd_user_unit();

        assert!(unit.contains(
            "ExecStart=/usr/local/bin/tracedecay daemon run --socket /tmp/tracedecay.sock"
        ));
        assert!(unit.contains("Environment=\"PATH="));
        assert!(unit.contains("Restart=on-failure"));
    }

    // The launchd render tests use Unix-style absolute binary paths, which
    // `Path::is_absolute` rejects on Windows; launchd is Unix-only anyway.
    #[cfg(unix)]
    #[test]
    fn render_launchd_plist_includes_program_arguments_socket_logs_and_label() {
        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let profile = tempfile::TempDir::new().expect("profile temp dir");
        let home = tempfile::TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let spec = DaemonServiceSpec {
            tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
            socket_path: profile.path().join("daemon.sock"),
            data_dir_override: Some(profile.path().to_path_buf()),
        };

        let plist = spec.render_launchd_plist().expect("launchd plist");

        assert!(plist.contains("<key>Label</key>"));
        assert!(plist.contains("<string>com.tracedecay.daemon</string>"));
        assert!(plist.contains("<key>ProgramArguments</key>"));
        assert!(plist.contains("<string>/opt/tracedecay/bin/tracedecay</string>"));
        assert!(plist.contains("<string>daemon</string>"));
        assert!(plist.contains("<string>run</string>"));
        assert!(plist.contains("<string>--socket</string>"));
        assert!(plist.contains(&format!(
            "<string>{}</string>",
            profile.path().join("daemon.sock").display()
        )));
        assert!(plist.contains(&format!(
            "<string>{}</string>",
            profile.path().join("daemon.out.log").display()
        )));
        assert!(plist.contains(&format!(
            "<string>{}</string>",
            profile.path().join("daemon.err.log").display()
        )));
        assert!(plist.contains("<key>TRACEDECAY_DATA_DIR</key>"));
        assert!(plist.contains("<key>RunAtLoad</key>"));
        assert!(plist.contains("<key>KeepAlive</key>"));
    }

    #[cfg(unix)]
    #[test]
    fn render_launchd_plist_escapes_xml_and_parser_unescapes_socket_path() {
        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let profile = tempfile::TempDir::new().expect("profile temp dir");
        let home = tempfile::TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let _data_dir_guard = EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, profile.path());
        let socket_path = PathBuf::from("/tmp/trace<decay>&\"socket'.sock");
        let spec = DaemonServiceSpec {
            tracedecay_bin: PathBuf::from("/opt/trace&decay/bin/tracedecay"),
            socket_path: socket_path.clone(),
            data_dir_override: None,
        };

        let plist = spec.render_launchd_plist().expect("launchd plist");

        assert!(plist.contains("/opt/trace&amp;decay/bin/tracedecay"));
        assert!(plist.contains("/tmp/trace&lt;decay&gt;&amp;&quot;socket&apos;.sock"));
        assert_eq!(
            super::socket_path_from_launchd_plist(&plist),
            Some(socket_path)
        );
    }

    #[test]
    fn socket_path_from_launchd_plist_returns_none_for_malformed_input() {
        assert_eq!(
            super::socket_path_from_launchd_plist("<plist></plist>"),
            None
        );
        assert_eq!(
            super::socket_path_from_launchd_plist(
                "<key>ProgramArguments</key><array><string>tracedecay</string></array>"
            ),
            None
        );
    }

    #[test]
    fn socket_path_from_launchd_plist_accepts_socket_equals_form() {
        let plist = "\
            <key>ProgramArguments</key>\
            <array>\
              <string>/opt/tracedecay/bin/tracedecay</string>\
              <string>daemon</string>\
              <string>run</string>\
              <string>--socket=/tmp/tracedecay.sock</string>\
            </array>";

        assert_eq!(
            super::socket_path_from_launchd_plist(plist),
            Some(PathBuf::from("/tmp/tracedecay.sock"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn launchd_plist_env_value_round_trips_data_dir_override() {
        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let profile = tempfile::TempDir::new().expect("profile temp dir");
        let home = tempfile::TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let spec = DaemonServiceSpec {
            tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
            socket_path: profile.path().join("daemon.sock"),
            data_dir_override: Some(profile.path().to_path_buf()),
        };

        let plist = spec.render_launchd_plist().expect("launchd plist");

        assert_eq!(
            super::launchd_plist_env_value(&plist, crate::config::USER_DATA_DIR_ENV),
            Some(profile.path().display().to_string())
        );
        assert_eq!(super::launchd_plist_env_value(&plist, "MISSING_VAR"), None);
    }

    #[cfg(unix)]
    #[test]
    fn launchd_plist_env_value_ignores_plist_without_override() {
        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let profile = tempfile::TempDir::new().expect("profile temp dir");
        let home = tempfile::TempDir::new().expect("home temp dir");
        let _home_guard = EnvVarGuard::set("HOME", home.path());
        let _data_dir_guard = EnvVarGuard::set(crate::config::USER_DATA_DIR_ENV, profile.path());
        let spec = DaemonServiceSpec {
            tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
            socket_path: profile.path().join("daemon.sock"),
            data_dir_override: None,
        };

        let plist = spec.render_launchd_plist().expect("launchd plist");

        assert_eq!(
            super::launchd_plist_env_value(&plist, crate::config::USER_DATA_DIR_ENV),
            None
        );
    }

    #[test]
    fn launchd_command_plans_map_start_and_uninstall_sequences() {
        let service_path =
            PathBuf::from("/Users/me/Library/LaunchAgents/com.tracedecay.daemon.plist");

        assert_eq!(
            super::launchd_start_command_plan(
                "gui/501",
                "gui/501/com.tracedecay.daemon",
                &service_path
            ),
            vec![
                LaunchdCommand::new(
                    &["bootout", "gui/501/com.tracedecay.daemon"],
                    LaunchctlFailureMode::TolerateNotLoaded
                ),
                LaunchdCommand::new(
                    &["enable", "gui/501/com.tracedecay.daemon"],
                    LaunchctlFailureMode::Fail
                ),
                LaunchdCommand::new(
                    &[
                        "bootstrap",
                        "gui/501",
                        "/Users/me/Library/LaunchAgents/com.tracedecay.daemon.plist"
                    ],
                    LaunchctlFailureMode::Fail
                ),
                LaunchdCommand::new(
                    &["kickstart", "-k", "gui/501/com.tracedecay.daemon"],
                    LaunchctlFailureMode::Fail
                ),
            ]
        );
        assert_eq!(
            super::launchd_uninstall_command_plan("gui/501/com.tracedecay.daemon"),
            vec![
                LaunchdCommand::new(
                    &["bootout", "gui/501/com.tracedecay.daemon"],
                    LaunchctlFailureMode::TolerateNotLoaded
                ),
                LaunchdCommand::new(
                    &["disable", "gui/501/com.tracedecay.daemon"],
                    LaunchctlFailureMode::Ignore
                ),
            ]
        );
    }

    #[test]
    fn launchctl_stderr_not_loaded_matches_known_messages_only() {
        assert!(super::launchctl_stderr_is_not_loaded(
            "Boot-out failed: 3: No such process"
        ));
        assert!(super::launchctl_stderr_is_not_loaded(
            "Could not find service \"com.tracedecay.daemon\" in domain for user gui: 501"
        ));
        assert!(super::launchctl_stderr_is_not_loaded(
            "service is not loaded"
        ));
        assert!(!super::launchctl_stderr_is_not_loaded(
            "Boot-out failed: 5: Input/output error"
        ));
        assert!(!super::launchctl_stderr_is_not_loaded(""));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn refresh_service_rewrites_unit_and_restarts_daemon() {
        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let dir = TempDir::new().expect("temp dir");
        let config_home = dir.path().join("config");
        let fake_bin = dir.path().join("bin");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
        std::fs::create_dir_all(&home).expect("home dir");

        let systemctl = fake_bin.join("systemctl");
        let log = dir.path().join("systemctl.log");
        std::fs::write(
            &systemctl,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TRACEDECAY_SYSTEMCTL_LOG\"\n",
        )
        .expect("fake systemctl");
        std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
            .expect("systemctl permissions");

        let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
        let _home_guard = EnvVarGuard::set("HOME", &home);
        let _path_guard = EnvVarGuard::set("PATH", &fake_bin);
        let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);
        let spec = DaemonServiceSpec {
            tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
            socket_path: PathBuf::from("/run/user/1000/tracedecay.sock"),
            data_dir_override: None,
        };

        let service_path = super::refresh_service(&spec).expect("refresh service");

        assert_eq!(
            service_path,
            config_home
                .join("systemd/user")
                .join(crate::daemon::SERVICE_NAME)
        );
        let unit = std::fs::read_to_string(&service_path).expect("service unit");
        assert!(unit.contains(
            "ExecStart=/opt/tracedecay/bin/tracedecay daemon run --socket /run/user/1000/tracedecay.sock"
        ));
        assert_eq!(
            std::fs::read_to_string(log).expect("systemctl log"),
            "--user daemon-reload\n--user enable tracedecay.service\n--user restart tracedecay.service\n"
        );
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn refresh_installed_service_skips_missing_unit() {
        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let dir = TempDir::new().expect("temp dir");
        let config_home = dir.path().join("config");
        let fake_bin = dir.path().join("bin");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
        std::fs::create_dir_all(&home).expect("home dir");

        let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
        let _home_guard = EnvVarGuard::set("HOME", &home);
        let _path_guard = EnvVarGuard::set("PATH", &fake_bin);
        let spec = DaemonServiceSpec {
            tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
            socket_path: PathBuf::from("/run/user/1000/tracedecay.sock"),
            data_dir_override: None,
        };

        let service_path = config_home
            .join("systemd/user")
            .join(crate::daemon::SERVICE_NAME);
        let outcome = super::refresh_installed_service(&spec).expect("refresh service");

        assert_eq!(outcome, None);
        assert!(!service_path.exists());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn refresh_installed_service_preserves_existing_socket_path() {
        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let dir = TempDir::new().expect("temp dir");
        let config_home = dir.path().join("config");
        let fake_bin = dir.path().join("bin");
        let home = dir.path().join("home");
        std::fs::create_dir_all(&fake_bin).expect("fake bin dir");
        std::fs::create_dir_all(&home).expect("home dir");

        let systemctl = fake_bin.join("systemctl");
        let log = dir.path().join("systemctl.log");
        std::fs::write(
            &systemctl,
            "#!/bin/sh\nprintf '%s\\n' \"$*\" >> \"$TRACEDECAY_SYSTEMCTL_LOG\"\n",
        )
        .expect("fake systemctl");
        std::fs::set_permissions(&systemctl, std::fs::Permissions::from_mode(0o755))
            .expect("systemctl permissions");

        let _config_guard = EnvVarGuard::set("XDG_CONFIG_HOME", &config_home);
        let _home_guard = EnvVarGuard::set("HOME", &home);
        let _path_guard = EnvVarGuard::set("PATH", &fake_bin);
        let _log_guard = EnvVarGuard::set("TRACEDECAY_SYSTEMCTL_LOG", &log);

        let service_path = config_home
            .join("systemd/user")
            .join(crate::daemon::SERVICE_NAME);
        std::fs::create_dir_all(service_path.parent().expect("service parent"))
            .expect("service dir");
        std::fs::write(
            &service_path,
            "[Unit]\n\
             Description=TraceDecay daemon\n\
             \n\
             [Service]\n\
             ExecStart=/old/tracedecay daemon run --socket /custom/tracedecay.sock\n",
        )
        .expect("existing service unit");

        let spec = DaemonServiceSpec {
            tracedecay_bin: PathBuf::from("/opt/tracedecay/bin/tracedecay"),
            socket_path: PathBuf::from("/run/user/1000/tracedecay.sock"),
            data_dir_override: None,
        };

        let outcome = super::refresh_installed_service(&spec).expect("refresh service");

        assert_eq!(outcome, Some(service_path.clone()));
        let unit = std::fs::read_to_string(service_path).expect("service unit");
        assert!(unit.contains(
            "ExecStart=/opt/tracedecay/bin/tracedecay daemon run --socket /custom/tracedecay.sock"
        ));
        assert!(!unit.contains("/run/user/1000/tracedecay.sock"));
        assert_eq!(
            std::fs::read_to_string(log).expect("systemctl log"),
            "--user daemon-reload\n--user enable tracedecay.service\n--user restart tracedecay.service\n"
        );
    }

    #[test]
    fn default_socket_path_is_profile_scoped_not_project_scoped() {
        let _env_lock = ENV_LOCK.lock().expect("env lock");
        let profile = tempfile::TempDir::new().expect("profile temp dir");
        let project_a = tempfile::TempDir::new().expect("project a temp dir");
        let project_b = tempfile::TempDir::new().expect("project b temp dir");
        let override_socket = profile.path().join("override.sock");
        let _socket_guard = EnvVarGuard::unset(crate::daemon::SOCKET_ENV);
        let _data_dir_guard = EnvVarGuard::set(
            crate::config::USER_DATA_DIR_ENV,
            profile.path().join(".tracedecay"),
        );

        {
            let _cwd_guard = CurrentDirGuard::set(project_a.path());
            assert_eq!(
                super::default_socket_path().expect("default socket path"),
                profile.path().join(".tracedecay/daemon.sock")
            );
        }
        {
            let _cwd_guard = CurrentDirGuard::set(project_b.path());
            assert_eq!(
                super::default_socket_path().expect("default socket path"),
                profile.path().join(".tracedecay/daemon.sock")
            );
        }

        let _override_guard = EnvVarGuard::set(crate::daemon::SOCKET_ENV, &override_socket);
        assert_eq!(
            super::default_socket_path().expect("override socket path"),
            override_socket
        );
    }
}
