//! Refresh of an already-installed TraceDecay plugin through Kimi Code's own
//! installer, reached through the token-protected local REST API that
//! `kimi web` serves (`GET`/`POST /api/v1/plugins`) on the loopback endpoint
//! it announces once bound.
//!
//! Consent boundary: the first install is the operator's interactive
//! `/plugins install`, which carries Kimi's trust confirmation. This path only
//! re-runs Kimi's installer for a plugin Kimi itself lists as installed and
//! enabled from exactly TraceDecay's staged local path. Anything else, and
//! every failure, leaves the caller's pending operator action in place.

use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tracedecay_application::http_agent::http_agent;

use crate::agents::host_cli::{HostServerChild, require_host_cli, spawn_host_server};

/// Bound on `kimi web` starting and answering its first plugin listing.
const KIMI_WEB_READY_DEADLINE: Duration = Duration::from_secs(20);

/// Bound on one plugin API request once the server answers.
const KIMI_WEB_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

const READY_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// The origin of the `Local:` URL in `kimi web`'s ready banner.
const KIMI_WEB_LOCAL_ORIGIN: &str = "http://127.0.0.1:";

/// Why the non-interactive refresh did not apply. Every variant leaves the
/// interactive `/plugins install` as the operator's step.
#[derive(Debug, thiserror::Error)]
pub(crate) enum KimiWebRefreshError {
    #[error(
        "Kimi Code does not list TraceDecay as installed and enabled from its staged source, \
         so installing it stays Kimi's interactive `/plugins install`"
    )]
    NotInstalledFromStagedSource,
    #[error("the staged Kimi plugin is unreadable: {0}")]
    StagedBundle(String),
    #[error("the `kimi` CLI is unavailable: {0}")]
    KimiUnavailable(String),
    #[error("`kimi web` could not start: {0}")]
    Start(String),
    #[error("`kimi web` exited ({status}) before serving its plugin API: {output}")]
    Exited { status: String, output: String },
    #[error("`kimi web` did not serve its plugin API within {seconds} seconds: {detail}")]
    NotReady { seconds: u64, detail: String },
    #[error("`kimi web` is unreachable: {0}")]
    Unreachable(String),
    #[error("`kimi web` rejected the server token (HTTP 401)")]
    Unauthorized,
    #[error("`kimi web` answered unexpectedly: {0}")]
    Unexpected(String),
    #[error("Kimi's installer did not report the staged plugin as installed: {0}")]
    NotRefreshed(String),
    #[error("could not stop the `kimi web` process TraceDecay started: {0}")]
    Stop(String),
}

/// Kimi's own report of the refreshed plugin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct KimiPluginRefreshV1 {
    pub version: String,
}

/// Re-run Kimi's installer for the staged TraceDecay plugin when, and only
/// when, Kimi already has it installed from that exact source.
#[tracing::instrument(name = "hosts.agent.kimi.web_refresh", level = "trace", skip_all)]
pub(crate) fn refresh_installed_plugin(
    home: &Path,
) -> Result<KimiPluginRefreshV1, KimiWebRefreshError> {
    let code_home = super::kimi_code_home(home);
    // Kimi's installer records every install in `installed.json`; without a
    // staged-source entry there Kimi's listing cannot grant consent either,
    // so no server is started.
    if !installed_json_records_staged_source(home, &code_home) {
        return Err(KimiWebRefreshError::NotInstalledFromStagedSource);
    }
    let staged = super::kimi_staged_plugin_dir(home);
    let staged_version = staged_bundle_version(&staged)?;
    let source = staged.to_str().ok_or_else(|| {
        KimiWebRefreshError::StagedBundle(format!("{} is not UTF-8", staged.display()))
    })?;
    let kimi = require_host_cli(super::KIMI_CLI, "Kimi Code plugin refresh")
        .map_err(|error| KimiWebRefreshError::KimiUnavailable(error.to_string()))?;
    // Without `--host`, Kimi binds 127.0.0.1 only. Port 0 lets the OS assign
    // one that only this `kimi web` holds; a port chosen here could be taken
    // by any local process before Kimi binds it, and Kimi itself moves past a
    // busy one. Its auto-update is disabled so a plugin refresh never
    // upgrades Kimi itself.
    let mut server = spawn_host_server(
        &kimi,
        &["web", "--no-open", "--port", "0"],
        home,
        &[
            ("KIMI_CODE_HOME", code_home.as_os_str()),
            ("KIMI_CODE_NO_AUTO_UPDATE", OsStr::new("1")),
        ],
    )
    .map_err(|error| KimiWebRefreshError::Start(error.to_string()))?;
    let refreshed = refresh_through_server(&mut server, source, &staged, &staged_version);
    let stopped = server.terminate();
    let refreshed = refreshed?;
    stopped.map_err(|error| KimiWebRefreshError::Stop(error.to_string()))?;
    Ok(refreshed)
}

fn installed_json_records_staged_source(home: &Path, code_home: &Path) -> bool {
    super::load_json_file_strict(&super::kimi_installed_json_path(code_home)).is_ok_and(
        |installed| {
            super::kimi_installed_entry(&installed).is_some_and(|entry| {
                super::kimi_manager_has_active_staged_install(entry, home, code_home)
            })
        },
    )
}

fn staged_bundle_version(staged: &Path) -> Result<String, KimiWebRefreshError> {
    let manifest_path = staged.join(super::KIMI_PLUGIN_MANIFEST_RELATIVE);
    let manifest = std::fs::read(&manifest_path)
        .map_err(|error| error.to_string())
        .and_then(|bytes| {
            serde_json::from_slice::<Value>(&bytes).map_err(|error| error.to_string())
        })
        .map_err(|error| {
            KimiWebRefreshError::StagedBundle(format!("{}: {error}", manifest_path.display()))
        })?;
    manifest
        .get("version")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| {
            KimiWebRefreshError::StagedBundle(format!("{} has no version", manifest_path.display()))
        })
}

fn refresh_through_server(
    server: &mut HostServerChild,
    source: &str,
    staged: &Path,
    staged_version: &str,
) -> Result<KimiPluginRefreshV1, KimiWebRefreshError> {
    // No connection pooling: ureq treats an HTTP/1.0 response as reusable, so
    // the install POST could ride a connection the server is closing, and
    // ureq never resends a POST.
    let agent = http_agent(
        ureq::Agent::config_builder()
            .max_idle_connections(0)
            .timeout_global(Some(KIMI_WEB_REQUEST_TIMEOUT))
            .http_status_as_error(false)
            .proxy(None)
            .max_redirects(0)
            .build(),
    );
    let deadline = Instant::now() + KIMI_WEB_READY_DEADLINE;
    let (url, authorization, listing) = loop {
        if let Some(status) = server
            .exited()
            .map_err(|error| KimiWebRefreshError::Start(error.to_string()))?
        {
            return Err(KimiWebRefreshError::Exited {
                status: status.to_string(),
                output: server.output().trim().to_string(),
            });
        }
        let pending = match announced_endpoint(&server.output()) {
            Some(KimiWebEndpoint { port, token }) => {
                let url = format!("{KIMI_WEB_LOCAL_ORIGIN}{port}/api/v1/plugins");
                let authorization = format!("Bearer {token}");
                match envelope_data(
                    agent
                        .get(&url)
                        .header("Authorization", &authorization)
                        .call(),
                    "GET /api/v1/plugins",
                ) {
                    Ok(listing) => break (url, authorization, listing),
                    Err(KimiWebRefreshError::Unreachable(detail)) => detail,
                    Err(error) => return Err(error),
                }
            }
            None => "it has not announced its local URL".to_string(),
        };
        if Instant::now() >= deadline {
            return Err(KimiWebRefreshError::NotReady {
                seconds: KIMI_WEB_READY_DEADLINE.as_secs(),
                detail: pending,
            });
        }
        std::thread::sleep(READY_POLL_INTERVAL);
    };

    let plugins = listing
        .get("plugins")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            KimiWebRefreshError::Unexpected("the plugin listing has no plugins array".to_string())
        })?;
    let consented = plugins.iter().any(|plugin| {
        plugin.get("id").and_then(Value::as_str) == Some(super::KIMI_PLUGIN_ID)
            && plugin.get("enabled").and_then(Value::as_bool) == Some(true)
            && plugin.get("source").and_then(Value::as_str) == Some("local-path")
            && super::kimi_manager_path_matches(plugin, "originalSource", staged)
    });
    if !consented {
        return Err(KimiWebRefreshError::NotInstalledFromStagedSource);
    }

    let installed = envelope_data(
        agent
            .post(&url)
            .header("Authorization", &authorization)
            .send_json(json!({ "source": source })),
        "POST /api/v1/plugins",
    )?;
    let field = |name: &str| installed.get(name).and_then(Value::as_str);
    if field("id") != Some(super::KIMI_PLUGIN_ID)
        || field("state") != Some("ok")
        || field("version") != Some(staged_version)
        || !super::kimi_manager_path_matches(&installed, "originalSource", staged)
    {
        return Err(KimiWebRefreshError::NotRefreshed(format!(
            "expected {} {staged_version} in state ok from {source}, got {installed}",
            super::KIMI_PLUGIN_ID
        )));
    }
    Ok(KimiPluginRefreshV1 {
        version: staged_version.to_string(),
    })
}

/// The loopback endpoint a running `kimi web` serves its API on.
#[derive(Debug, PartialEq, Eq)]
struct KimiWebEndpoint {
    port: u16,
    token: String,
}

/// The endpoint from a complete `Local:    http://127.0.0.1:<port>/#token=<token>`
/// line of `kimi web`'s ready banner, which Kimi prints only once it holds
/// the port.
fn announced_endpoint(output: &str) -> Option<KimiWebEndpoint> {
    output
        .split_inclusive('\n')
        .filter_map(|line| line.strip_suffix('\n'))
        .find_map(|line| {
            let (_, url) = line.split_once(KIMI_WEB_LOCAL_ORIGIN)?;
            let (port, fragment) = url.split_once("/#token=")?;
            let port = port.parse::<u16>().ok().filter(|port| *port != 0)?;
            let token = fragment.split_whitespace().next()?;
            Some(KimiWebEndpoint {
                port,
                token: token.to_string(),
            })
        })
}

/// The `data` of a successful Kimi envelope (`{"code": 0, "data": ...}`).
fn envelope_data(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
    route: &str,
) -> Result<Value, KimiWebRefreshError> {
    let mut response =
        response.map_err(|error| KimiWebRefreshError::Unreachable(format!("{route}: {error}")))?;
    match response.status().as_u16() {
        200 => {}
        401 => return Err(KimiWebRefreshError::Unauthorized),
        status => {
            return Err(KimiWebRefreshError::Unexpected(format!(
                "{route} returned HTTP {status}"
            )));
        }
    }
    let envelope = response.body_mut().read_json::<Value>().map_err(|error| {
        KimiWebRefreshError::Unexpected(format!("{route} returned a non-JSON body: {error}"))
    })?;
    if envelope.get("code").and_then(Value::as_i64) != Some(0) {
        return Err(KimiWebRefreshError::Unexpected(format!(
            "{route} failed: {envelope}"
        )));
    }
    envelope
        .get("data")
        .cloned()
        .ok_or_else(|| KimiWebRefreshError::Unexpected(format!("{route} returned no data")))
}

#[cfg(test)]
mod tests {
    use super::{KimiWebEndpoint, announced_endpoint};

    const BANNER: &str = "\n  Kimi server ready  2.1.1\n\n  Local:    http://127.0.0.1:51234/#token=abc-123\n  Network:  off  use --host to enable\n";

    #[test]
    fn announced_endpoint_reads_the_local_line_of_the_ready_banner() {
        assert_eq!(
            announced_endpoint(BANNER),
            Some(KimiWebEndpoint {
                port: 51234,
                token: "abc-123".to_string(),
            })
        );
    }

    #[test]
    fn announced_endpoint_waits_for_the_whole_local_line() {
        let partial = &BANNER[..BANNER.find("abc-123").unwrap() + "abc".len()];
        assert_eq!(announced_endpoint(partial), None);
        assert_eq!(
            announced_endpoint("  Local:    http://127.0.0.1:51234/\n"),
            None,
            "a banner without a token announces nothing TraceDecay can use"
        );
    }
}
