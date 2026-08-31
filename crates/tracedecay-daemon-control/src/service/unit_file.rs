#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use tracedecay_domain::errors::{Result, TraceDecayError};

use super::runner::ServicePlatform;
use super::{
    DaemonServiceSpec, LAUNCHD_PLIST_NAME, SERVICE_TEMP_SEQUENCE, home_for_service_env,
    plist_xml_escape, plist_xml_unescape, windows_task,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AtomicServiceWriteStep {
    TempWrite,
    TempFsync,
    Rename,
    ParentFsync,
}

#[hotpath::measure(label = "daemon.service.unit.write")]
pub(super) fn atomic_replace_service_unit_with(
    service_path: &Path,
    unit: &str,
    before_step: &mut impl FnMut(AtomicServiceWriteStep) -> Result<()>,
) -> Result<()> {
    let parent = service_path
        .parent()
        .ok_or_else(|| TraceDecayError::Config {
            message: format!("service path '{}' has no parent", service_path.display()),
        })?;
    std::fs::create_dir_all(parent).map_err(|error| TraceDecayError::Config {
        message: format!(
            "failed to create service directory '{}': {error}",
            parent.display()
        ),
    })?;

    before_step(AtomicServiceWriteStep::TempWrite)?;
    let file_name = service_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("tracedecay-service");
    let sequence = SERVICE_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
    let temporary_path = parent.join(format!(
        ".{file_name}.tmp.{}.{sequence}",
        std::process::id()
    ));
    let replacement_result = (|| {
        let mut temporary = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary_path)
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to create temporary service unit '{}': {error}",
                    temporary_path.display()
                ),
            })?;
        std::io::Write::write_all(&mut temporary, unit.as_bytes()).map_err(|error| {
            TraceDecayError::Config {
                message: format!(
                    "failed to write temporary service unit '{}': {error}",
                    temporary_path.display()
                ),
            }
        })?;
        #[cfg(unix)]
        std::fs::set_permissions(&temporary_path, std::fs::Permissions::from_mode(0o644)).map_err(
            |error| TraceDecayError::Config {
                message: format!(
                    "failed to set temporary service permissions '{}': {error}",
                    temporary_path.display()
                ),
            },
        )?;

        before_step(AtomicServiceWriteStep::TempFsync)?;
        temporary
            .sync_all()
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to sync temporary service unit '{}': {error}",
                    temporary_path.display()
                ),
            })?;
        drop(temporary);

        before_step(AtomicServiceWriteStep::Rename)?;
        std::fs::rename(&temporary_path, service_path).map_err(|error| {
            TraceDecayError::Config {
                message: format!(
                    "failed to atomically replace service unit '{}': {error}",
                    service_path.display()
                ),
            }
        })?;

        before_step(AtomicServiceWriteStep::ParentFsync)?;
        #[cfg(unix)]
        std::fs::File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| TraceDecayError::Config {
                message: format!(
                    "failed to sync service directory '{}': {error}",
                    parent.display()
                ),
            })?;
        Ok(())
    })();
    if replacement_result.is_err() {
        let _ = std::fs::remove_file(&temporary_path);
    }
    replacement_result
}

pub(super) fn write_service_unit(spec: &DaemonServiceSpec) -> Result<PathBuf> {
    let service_path = service_unit_path()?;
    let unit = spec.render_unit()?;
    match ServicePlatform::current()? {
        ServicePlatform::WindowsTask => windows_task::register_task_xml(&unit)?,
        ServicePlatform::Systemd | ServicePlatform::Launchd => {
            atomic_replace_service_unit_with(&service_path, &unit, &mut |_| Ok(()))?;
        }
    }
    Ok(service_path)
}

pub fn installed_service_socket_path() -> Result<Option<PathBuf>> {
    let service_path = service_unit_path()?;
    if !service_unit_exists(&service_path)? {
        return Ok(None);
    }
    Ok(socket_path_from_unit_text(&read_service_unit(
        &service_path,
    )?))
}

pub(super) fn read_service_unit(service_path: &Path) -> Result<String> {
    match ServicePlatform::current()? {
        ServicePlatform::WindowsTask => {
            windows_task::registered_task_xml()?.ok_or_else(|| TraceDecayError::Config {
                message: format!("daemon task '{}' is not registered", service_path.display()),
            })
        }
        ServicePlatform::Systemd | ServicePlatform::Launchd => {
            std::fs::read_to_string(service_path).map_err(|e| TraceDecayError::Config {
                message: format!("failed to read service '{}': {e}", service_path.display()),
            })
        }
    }
}

pub(super) fn service_unit_exists(service_path: &Path) -> Result<bool> {
    match ServicePlatform::current()? {
        ServicePlatform::WindowsTask => windows_task::task_exists(),
        ServicePlatform::Systemd | ServicePlatform::Launchd => Ok(service_path.exists()),
    }
}

#[hotpath::measure(label = "daemon.service.unit.remove")]
pub(super) fn remove_service_unit(service_path: &Path) -> Result<()> {
    match ServicePlatform::current()? {
        ServicePlatform::WindowsTask => windows_task::delete(),
        ServicePlatform::Systemd | ServicePlatform::Launchd => {
            match std::fs::remove_file(service_path) {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(TraceDecayError::Config {
                    message: format!(
                        "failed to remove service '{}': {error}",
                        service_path.display()
                    ),
                }),
            }
        }
    }
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

fn remote_tls_from_args<'a>(
    mut args: impl Iterator<Item = &'a str>,
) -> Result<Option<crate::RemoteBrainTlsConfig>> {
    let mut listen = None;
    let mut certificate_chain = None;
    let mut private_key = None;
    while let Some(arg) = args.next() {
        match arg {
            "--remote-listen" => set_unique_argument(
                &mut listen,
                args.next().map(str::to_owned),
                "--remote-listen",
            )?,
            "--remote-tls-cert" => set_unique_argument(
                &mut certificate_chain,
                args.next().map(PathBuf::from),
                "--remote-tls-cert",
            )?,
            "--remote-tls-key" => set_unique_argument(
                &mut private_key,
                args.next().map(PathBuf::from),
                "--remote-tls-key",
            )?,
            _ => {
                if let Some(value) = arg.strip_prefix("--remote-listen=") {
                    set_unique_argument(&mut listen, Some(value.to_owned()), "--remote-listen")?;
                } else if let Some(value) = arg.strip_prefix("--remote-tls-cert=") {
                    set_unique_argument(
                        &mut certificate_chain,
                        Some(PathBuf::from(value)),
                        "--remote-tls-cert",
                    )?;
                } else if let Some(value) = arg.strip_prefix("--remote-tls-key=") {
                    set_unique_argument(
                        &mut private_key,
                        Some(PathBuf::from(value)),
                        "--remote-tls-key",
                    )?;
                }
            }
        }
    }
    let listen = listen
        .map(|value| {
            value.parse().map_err(|_| TraceDecayError::Config {
                message: "installed daemon service has an invalid Remote Brain listener address"
                    .to_string(),
            })
        })
        .transpose()?;
    let remote_tls =
        crate::RemoteBrainTlsConfig::from_optional_parts(listen, certificate_chain, private_key)?;
    super::validate_managed_remote_tls(remote_tls.as_ref())?;
    Ok(remote_tls)
}

pub(super) fn set_unique_argument<T>(
    slot: &mut Option<T>,
    value: Option<T>,
    name: &str,
) -> Result<()> {
    let value = value.ok_or_else(|| TraceDecayError::Config {
        message: format!("installed daemon service is missing a value for {name}"),
    })?;
    if slot.replace(value).is_some() {
        return Err(TraceDecayError::Config {
            message: format!("installed daemon service repeats {name}"),
        });
    }
    Ok(())
}

fn systemd_exec_tokens(exec_start: &str) -> Result<Vec<String>> {
    let mut tokens = Vec::new();
    let mut token = String::new();
    let mut chars = exec_start.chars().peekable();
    let mut quoted = false;
    while let Some(ch) = chars.next() {
        match ch {
            '"' => quoted = !quoted,
            '\\' if quoted => {
                let escaped = chars.next().ok_or_else(|| TraceDecayError::Config {
                    message: "installed daemon service has a truncated quoted argument".to_string(),
                })?;
                if !matches!(escaped, '\\' | '"') {
                    return Err(TraceDecayError::Config {
                        message:
                            "installed daemon service has an unsupported quoted argument escape"
                                .to_string(),
                    });
                }
                token.push(escaped);
            }
            ch if ch.is_whitespace() && !quoted => {
                if !token.is_empty() {
                    tokens.push(token.replace("%%", "%").replace("$$", "$"));
                    token = String::new();
                }
            }
            _ => token.push(ch),
        }
    }
    if quoted {
        return Err(TraceDecayError::Config {
            message: "installed daemon service has an unterminated quoted argument".to_string(),
        });
    }
    if !token.is_empty() {
        tokens.push(token.replace("%%", "%").replace("$$", "$"));
    }
    Ok(tokens)
}

/// Reads the `--socket` argument back from an `ExecStart=` line with the same
/// quote-aware tokenizer the renderer's escaping targets, so quoted paths
/// (whitespace, `%%`, `$$`) round-trip exactly. A line the tokenizer rejects
/// yields `None` — callers fall back to the default socket path, and the
/// refresh journey surfaces the typed parse error through
/// [`remote_tls_from_service_unit`] on the same line.
pub(super) fn socket_path_from_service_unit(unit: &str) -> Option<PathBuf> {
    unit.lines()
        .filter_map(|line| line.trim().strip_prefix("ExecStart="))
        .find_map(|exec_start| {
            let tokens = systemd_exec_tokens(exec_start).ok()?;
            socket_path_from_args(tokens.iter().map(String::as_str))
        })
}

pub(super) fn remote_tls_from_service_unit(
    unit: &str,
) -> Result<Option<crate::RemoteBrainTlsConfig>> {
    let Some(exec_start) = unit
        .lines()
        .find_map(|line| line.trim().strip_prefix("ExecStart="))
    else {
        return Ok(None);
    };
    let tokens = systemd_exec_tokens(exec_start)?;
    remote_tls_from_args(tokens.iter().map(String::as_str))
}

pub(super) fn socket_path_from_launchd_plist(plist: &str) -> Option<PathBuf> {
    let program_arguments_start = plist.find("<key>ProgramArguments</key>")?;
    let arguments_text = &plist[program_arguments_start..];
    let array_start = arguments_text.find("<array>")? + "<array>".len();
    let after_array_start = &arguments_text[array_start..];
    let array_end = after_array_start.find("</array>")?;
    let array_text = &after_array_start[..array_end];
    let strings = plist_string_values(array_text);

    socket_path_from_args(strings.iter().map(String::as_str))
}

pub(super) fn remote_tls_from_launchd_plist(
    plist: &str,
) -> Result<Option<crate::RemoteBrainTlsConfig>> {
    let Some(program_arguments_start) = plist.find("<key>ProgramArguments</key>") else {
        return Ok(None);
    };
    let arguments_text = &plist[program_arguments_start..];
    let array_start = arguments_text
        .find("<array>")
        .ok_or_else(|| TraceDecayError::Config {
            message: "installed launchd daemon service has malformed program arguments".to_string(),
        })?
        + "<array>".len();
    let after_array_start = &arguments_text[array_start..];
    let array_end = after_array_start
        .find("</array>")
        .ok_or_else(|| TraceDecayError::Config {
            message: "installed launchd daemon service has malformed program arguments".to_string(),
        })?;
    let strings = plist_string_values(&after_array_start[..array_end]);
    remote_tls_from_args(strings.iter().map(String::as_str))
}

pub(super) fn launchd_plist_env_value(plist: &str, name: &str) -> Option<String> {
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

pub(super) fn socket_path_from_unit_text(unit: &str) -> Option<PathBuf> {
    match ServicePlatform::current().ok()? {
        ServicePlatform::Systemd => socket_path_from_service_unit(unit),
        ServicePlatform::Launchd => socket_path_from_launchd_plist(unit),
        ServicePlatform::WindowsTask => windows_task::profile_root_from_task_xml(unit)
            .map(|profile_root| profile_root.join("daemon.sock")),
    }
}

pub(super) fn remote_tls_from_unit_text(unit: &str) -> Result<Option<crate::RemoteBrainTlsConfig>> {
    match ServicePlatform::current()? {
        ServicePlatform::Systemd => remote_tls_from_service_unit(unit),
        ServicePlatform::Launchd => remote_tls_from_launchd_plist(unit),
        ServicePlatform::WindowsTask => windows_task::remote_tls_from_task_xml(unit),
    }
}

pub(super) fn service_unit_path() -> Result<PathBuf> {
    match ServicePlatform::current()? {
        ServicePlatform::Systemd => systemd_user_service_path(),
        ServicePlatform::Launchd => launchd_user_service_path(),
        ServicePlatform::WindowsTask => windows_task::task_path(),
    }
}

fn systemd_user_service_path() -> Result<PathBuf> {
    let config_home = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|home| home.join(".config")))
        .ok_or_else(|| TraceDecayError::Config {
            message: "could not determine XDG config directory".to_string(),
        })?;
    Ok(config_home.join("systemd/user").join(crate::SERVICE_NAME))
}

fn launchd_user_service_path() -> Result<PathBuf> {
    let home = home_for_service_env()?;
    Ok(home.join("Library/LaunchAgents").join(LAUNCHD_PLIST_NAME))
}
