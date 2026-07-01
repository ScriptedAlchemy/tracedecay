use crate::cloud;
use crate::errors::{Result, TraceDecayError};

#[derive(serde::Deserialize)]
struct CrateResponse {
    #[serde(rename = "crate")]
    krate: CrateMetadata,
    versions: Vec<CrateVersion>,
}

#[derive(serde::Deserialize)]
struct CrateMetadata {
    max_stable_version: Option<String>,
    max_version: String,
}

#[derive(serde::Deserialize)]
struct CrateVersion {
    num: String,
    yanked: bool,
}

pub(super) fn fetch_latest_version(is_beta: bool) -> Result<String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(std::time::Duration::from_secs(30)))
        .build()
        .into();
    let response: CrateResponse = agent
        .get("https://crates.io/api/v1/crates/tracedecay")
        .header("User-Agent", "tracedecay")
        .call()
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to reach crates.io: {e}"),
        })?
        .body_mut()
        .read_json()
        .map_err(|e| TraceDecayError::Config {
            message: format!("failed to parse crates.io metadata: {e}"),
        })?;
    latest_version_from_response(response, is_beta)
}

fn latest_version_from_response(response: CrateResponse, is_beta: bool) -> Result<String> {
    if is_beta {
        return newest_version(&response.versions, true)
            .map(ToString::to_string)
            .ok_or_else(|| TraceDecayError::Config {
                message: "no installable beta version found on crates.io".to_string(),
            });
    }

    if let Some(stable) = response
        .krate
        .max_stable_version
        .filter(|v| !v.contains('-'))
    {
        return Ok(stable);
    }
    if !response.krate.max_version.contains('-') {
        return Ok(response.krate.max_version);
    }
    newest_version(&response.versions, false)
        .map(ToString::to_string)
        .ok_or_else(|| TraceDecayError::Config {
            message: "no installable stable version found on crates.io".to_string(),
        })
}

fn newest_version(versions: &[CrateVersion], prerelease: bool) -> Option<&str> {
    let mut best: Option<&str> = None;
    for version in versions
        .iter()
        .filter(|v| !v.yanked && v.num.contains('-') == prerelease)
    {
        if best.is_none_or(|current| cloud::is_newer_version(current, &version.num)) {
            best = Some(&version.num);
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crate_response(
        max_stable_version: Option<&str>,
        max_version: &str,
        versions: &[(&str, bool)],
    ) -> CrateResponse {
        CrateResponse {
            krate: CrateMetadata {
                max_stable_version: max_stable_version.map(str::to_string),
                max_version: max_version.to_string(),
            },
            versions: versions
                .iter()
                .map(|(num, yanked)| CrateVersion {
                    num: (*num).to_string(),
                    yanked: *yanked,
                })
                .collect(),
        }
    }

    #[test]
    fn beta_selection_uses_newest_non_yanked_prerelease() -> Result<()> {
        let response = crate_response(
            Some("0.0.17"),
            "0.0.18-beta.2",
            &[
                ("0.0.18-beta.1", false),
                ("0.0.19-beta.1", true),
                ("0.0.17", false),
                ("0.0.18-beta.2", false),
            ],
        );

        let latest = latest_version_from_response(response, true)?;

        assert_eq!(latest, "0.0.18-beta.2");
        Ok(())
    }

    #[test]
    fn stable_selection_falls_back_to_newest_non_yanked_stable() -> Result<()> {
        let response = crate_response(
            None,
            "0.0.19-beta.1",
            &[
                ("0.0.17", false),
                ("0.0.19-beta.1", false),
                ("0.0.18", true),
                ("0.0.18", false),
            ],
        );

        let latest = latest_version_from_response(response, false)?;

        assert_eq!(latest, "0.0.18");
        Ok(())
    }
}
