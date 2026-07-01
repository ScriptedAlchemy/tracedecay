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
#[cfg(target_os = "macos")]
const LAUNCHD_PLIST_NAME: &str = "com.tracedecay.daemon.plist";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonServiceSpec {
    pub tracedecay_bin: PathBuf,
    pub socket_path: PathBuf,
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
        let data_dir = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
            message: "could not determine TraceDecay user data directory".to_string(),
        })?;
        let mut env_entries = vec![
            (
                "PATH".to_string(),
                daemon_service_path_env(&self.tracedecay_bin),
            ),
            ("HOME".to_string(), home.display().to_string()),
        ];
        if let Some(data_dir_override) =
            std::env::var_os(crate::config::USER_DATA_DIR_ENV).filter(|value| !value.is_empty())
        {
            env_entries.push((
                crate::config::USER_DATA_DIR_ENV.to_string(),
                PathBuf::from(data_dir_override).display().to_string(),
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
            #[cfg(target_os = "linux")]
            ServiceRunner::Systemd => Ok(self.render_systemd_user_unit()),
            #[cfg(target_os = "macos")]
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

#[cfg(any(test, target_os = "macos"))]
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

pub fn default_socket_path() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os(SOCKET_ENV).filter(|path| !path.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let data_dir = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    Ok(data_dir.join("daemon.sock"))
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
    let mut refreshed_spec = spec.clone();
    if let Some(socket_path) = service_socket_path_from_unit_file(&service_path)? {
        refreshed_spec.socket_path = socket_path;
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
    service_socket_path_from_unit_file(&service_path)
}

fn service_socket_path_from_unit_file(service_path: &Path) -> Result<Option<PathBuf>> {
    let unit = std::fs::read_to_string(service_path).map_err(|e| TraceDecayError::Config {
        message: format!("failed to read service '{}': {e}", service_path.display()),
    })?;
    Ok(socket_path_from_unit_text(&unit))
}

#[cfg(target_os = "linux")]
fn socket_path_from_service_unit(unit: &str) -> Option<PathBuf> {
    unit.lines()
        .filter_map(|line| line.trim().strip_prefix("ExecStart="))
        .find_map(|exec_start| {
            let mut args = exec_start.split_whitespace();
            while let Some(arg) = args.next() {
                if arg == "--socket" {
                    return args.next().map(PathBuf::from);
                }
                if let Some(path) = arg.strip_prefix("--socket=") {
                    return Some(PathBuf::from(path));
                }
            }
            None
        })
}

#[cfg(any(test, target_os = "macos"))]
pub(super) fn socket_path_from_launchd_plist(plist: &str) -> Option<PathBuf> {
    let program_arguments_start = plist.find("<key>ProgramArguments</key>")?;
    let arguments_text = &plist[program_arguments_start..];
    let array_start = arguments_text.find("<array>")? + "<array>".len();
    let after_array_start = &arguments_text[array_start..];
    let array_end = after_array_start.find("</array>")?;
    let array_text = &after_array_start[..array_end];
    let strings = plist_string_values(array_text);

    let mut args = strings.iter();
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

#[cfg(any(test, target_os = "macos"))]
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
        #[cfg(target_os = "linux")]
        ServiceRunner::Systemd => socket_path_from_service_unit(unit),
        #[cfg(target_os = "macos")]
        ServiceRunner::Launchd => socket_path_from_launchd_plist(unit),
    }
}

pub fn uninstall_service(stop: bool) -> Result<PathBuf> {
    let runner = ServiceRunner::current()?;
    let service_path = service_unit_path()?;
    runner.before_uninstall(&service_path, stop)?;
    match std::fs::remove_file(&service_path) {
        Ok(()) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(TraceDecayError::Config {
                message: format!("failed to remove service '{}': {e}", service_path.display()),
            });
        }
    }
    runner.after_uninstall(stop)?;
    Ok(service_path)
}

pub fn service_status(socket_path: &Path) -> String {
    let socket_state = daemon_socket_state(socket_path);
    let service = service_unit_path().map_or_else(
        |e| format!("unavailable: {e}"),
        |path| path.display().to_string(),
    );
    let detail = ServiceRunner::current()
        .ok()
        .and_then(|runner| runner.service_detail_hint())
        .map(|hint| format!("service-detail: {hint}\n"))
        .unwrap_or_default();
    let logs = ServiceRunner::current()
        .map_or_else(|e| format!("unavailable: {e}"), |runner| runner.log_hint());
    format!(
        "service: {}\nsocket: {} ({})\n{}logs: {}\n",
        service,
        socket_path.display(),
        socket_state,
        detail,
        logs,
    )
}

#[cfg(unix)]
fn daemon_socket_state(socket_path: &Path) -> &'static str {
    if !socket_path.exists() {
        return "missing";
    }
    match StdUnixStream::connect(socket_path) {
        Ok(_) => "connectable",
        Err(e) if e.kind() == std::io::ErrorKind::ConnectionRefused => "stale",
        Err(_) => "present but unreachable",
    }
}

#[cfg(not(unix))]
fn daemon_socket_state(socket_path: &Path) -> &'static str {
    if socket_path.exists() {
        "present but unsupported on this platform"
    } else {
        "missing"
    }
}

enum ServiceRunner {
    #[cfg(target_os = "linux")]
    Systemd,
    #[cfg(target_os = "macos")]
    Launchd,
}

impl ServiceRunner {
    fn current() -> Result<Self> {
        #[cfg(target_os = "linux")]
        {
            return Ok(Self::Systemd);
        }
        #[cfg(target_os = "macos")]
        {
            return Ok(Self::Launchd);
        }
        #[allow(unreachable_code)]
        Err(unsupported_service_platform())
    }

    fn install(&self, service_path: &Path, start: bool, socket_path: &Path) -> Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Systemd => {
                if start {
                    run_systemctl(&["daemon-reload"])?;
                    run_systemctl(&["enable", "--now", super::SERVICE_NAME])?;
                }
                Ok(())
            }
            #[cfg(target_os = "macos")]
            Self::Launchd => launchd_install(service_path, start, socket_path),
        }
    }

    fn refresh(&self, service_path: &Path, socket_path: &Path) -> Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Systemd => {
                run_systemctl(&["daemon-reload"])?;
                run_systemctl(&["enable", super::SERVICE_NAME])?;
                run_systemctl(&["restart", super::SERVICE_NAME])?;
                Ok(())
            }
            #[cfg(target_os = "macos")]
            Self::Launchd => launchd_refresh(service_path, socket_path),
        }
    }

    fn before_uninstall(&self, service_path: &Path, stop: bool) -> Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Systemd => {
                if stop {
                    let _ = run_systemctl(&["disable", "--now", super::SERVICE_NAME]);
                }
                Ok(())
            }
            #[cfg(target_os = "macos")]
            Self::Launchd => launchd_before_uninstall(service_path, stop),
        }
    }

    fn after_uninstall(&self, _stop: bool) -> Result<()> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Systemd => {
                if _stop {
                    let _ = run_systemctl(&["daemon-reload"]);
                }
                Ok(())
            }
            #[cfg(target_os = "macos")]
            Self::Launchd => Ok(()),
        }
    }

    fn log_hint(&self) -> String {
        match self {
            #[cfg(target_os = "linux")]
            Self::Systemd => format!("journalctl --user -u {} -f", super::SERVICE_NAME),
            #[cfg(target_os = "macos")]
            Self::Launchd => crate::config::user_data_dir().map_or_else(
                || "tail -f <tracedecay-data-dir>/daemon.err.log".to_string(),
                |dir| format!("tail -f \"{}\"", dir.join("daemon.err.log").display()),
            ),
        }
    }

    fn service_detail_hint(&self) -> Option<String> {
        match self {
            #[cfg(target_os = "linux")]
            Self::Systemd => None,
            #[cfg(target_os = "macos")]
            Self::Launchd => launchd_service_target()
                .ok()
                .map(|target| format!("launchctl print {target}")),
        }
    }
}

fn service_unit_path() -> Result<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        return systemd_user_service_path();
    }
    #[cfg(target_os = "macos")]
    {
        return launchd_user_service_path();
    }
    #[allow(unreachable_code)]
    Err(unsupported_service_platform())
}

#[cfg(target_os = "linux")]
fn systemd_user_service_path() -> Result<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .ok_or_else(|| TraceDecayError::Config {
            message: "could not determine XDG config directory".to_string(),
        })?;
    Ok(config_home.join("systemd/user").join(super::SERVICE_NAME))
}

#[cfg(target_os = "macos")]
fn launchd_user_service_path() -> Result<PathBuf> {
    let home = home_for_service_env()?;
    Ok(home.join("Library/LaunchAgents").join(LAUNCHD_PLIST_NAME))
}

#[cfg(target_os = "linux")]
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

#[cfg(any(test, target_os = "macos"))]
pub(super) fn launchd_install_command_args(
    domain: &str,
    target: &str,
    service_path: &Path,
) -> Vec<Vec<String>> {
    vec![
        vec![
            "bootstrap".to_string(),
            domain.to_string(),
            service_path.display().to_string(),
        ],
        vec!["enable".to_string(), target.to_string()],
        vec![
            "kickstart".to_string(),
            "-k".to_string(),
            target.to_string(),
        ],
    ]
}

#[cfg(any(test, target_os = "macos"))]
pub(super) fn launchd_refresh_command_args(
    domain: &str,
    target: &str,
    service_path: &Path,
) -> Vec<Vec<String>> {
    let mut commands = vec![vec!["bootout".to_string(), target.to_string()]];
    commands.extend(launchd_install_command_args(domain, target, service_path));
    commands
}

#[cfg(any(test, target_os = "macos"))]
pub(super) fn launchd_uninstall_command_args(target: &str) -> Vec<Vec<String>> {
    vec![
        vec!["bootout".to_string(), target.to_string()],
        vec!["disable".to_string(), target.to_string()],
    ]
}

#[cfg(target_os = "macos")]
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

#[cfg(target_os = "macos")]
fn launchd_service_target() -> Result<String> {
    Ok(format!("{}/{}", launchd_domain()?, LAUNCHD_LABEL))
}

#[cfg(target_os = "macos")]
fn ensure_launchd_runtime_dirs() -> Result<()> {
    let data_dir = crate::config::user_data_dir().ok_or_else(|| TraceDecayError::Config {
        message: "could not determine TraceDecay user data directory".to_string(),
    })?;
    std::fs::create_dir_all(&data_dir).map_err(|e| TraceDecayError::Config {
        message: format!(
            "failed to create daemon data directory '{}': {e}",
            data_dir.display()
        ),
    })
}

#[cfg(target_os = "macos")]
fn launchd_install(service_path: &Path, start: bool, socket_path: &Path) -> Result<()> {
    if !start {
        return Ok(());
    }
    ensure_launchd_runtime_dirs()?;
    let domain = launchd_domain()?;
    let target = launchd_service_target()?;
    for command in launchd_install_command_args(&domain, &target, service_path) {
        run_launchctl_owned(&command)?;
    }
    verify_launchd_started(&target, socket_path)
}

#[cfg(target_os = "macos")]
fn launchd_refresh(service_path: &Path, socket_path: &Path) -> Result<()> {
    ensure_launchd_runtime_dirs()?;
    let domain = launchd_domain()?;
    let target = launchd_service_target()?;
    let commands = launchd_refresh_command_args(&domain, &target, service_path);
    for (index, command) in commands.iter().enumerate() {
        if index == 0 {
            run_launchctl_owned_allow_not_loaded(command)?;
        } else {
            run_launchctl_owned(command)?;
        }
    }
    verify_launchd_started(&target, socket_path)
}

#[cfg(target_os = "macos")]
fn launchd_before_uninstall(_service_path: &Path, stop: bool) -> Result<()> {
    if !stop {
        return Ok(());
    }
    let target = launchd_service_target()?;
    let commands = launchd_uninstall_command_args(&target);
    for (index, command) in commands.iter().enumerate() {
        if index == 0 {
            run_launchctl_owned_allow_not_loaded(command)?;
        } else {
            let _ = run_launchctl_owned(command);
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
fn verify_launchd_started(target: &str, socket_path: &Path) -> Result<()> {
    if daemon_socket_state(socket_path) == "connectable" {
        return Ok(());
    }
    run_launchctl(&["print", target]).map(|_| ())
}

#[cfg(target_os = "macos")]
fn run_launchctl_owned(args: &[String]) -> Result<std::process::Output> {
    let args = args.iter().map(String::as_str).collect::<Vec<_>>();
    run_launchctl(&args)
}

#[cfg(target_os = "macos")]
fn run_launchctl_owned_allow_not_loaded(args: &[String]) -> Result<()> {
    match run_launchctl_owned(args) {
        Ok(_) => Ok(()),
        Err(error) if launchctl_error_is_not_loaded(&error.to_string()) => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(target_os = "macos")]
fn run_launchctl(args: &[&str]) -> Result<std::process::Output> {
    let output =
        Command::new("launchctl")
            .args(args)
            .output()
            .map_err(|e| TraceDecayError::Config {
                message: format!("failed to run launchctl {}: {e}", args.join(" ")),
            })?;
    if output.status.success() {
        return Ok(output);
    }
    Err(TraceDecayError::Config {
        message: format!(
            "launchctl {} failed with status {}\nstdout:\n{}\nstderr:\n{}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ),
    })
}

#[cfg(target_os = "macos")]
fn launchctl_error_is_not_loaded(message: &str) -> bool {
    [
        "No such process",
        "No such file or directory",
        "not found",
        "Could not find service",
        "service is not loaded",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}
