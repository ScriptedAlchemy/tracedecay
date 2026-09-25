//! Test-only publication of pinned runtime configuration.
//!
//! Tests that exercise configuration-gated runtime behaviour (the LCM
//! summarizer executables, for instance) publish the setting through the same
//! pin cache production reads, instead of reaching for an environment or
//! `PATH` side channel the runtime no longer consults.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, RwLock};

use tracedecay_domain::ProjectId;
use tracedecay_domain::configuration::{
    ConfigurationLayerIdV1, ConfigurationRevisionId, ConfigurationValueV1,
    LCM_SUMMARIZER_EXECUTABLES_SETTING_KEY, LcmSummarizerExecutablesV1, SettingKey,
};
use tracedecay_domain::errors::{Result, TraceDecayError};
use tracedecay_global_db::configuration::registry::ConfigurationRegistry;
use tracedecay_global_db::configuration::resolver::{ConfigurationLayerV1, resolve_configuration};

use crate::config::{
    PinnedRuntimeConfiguration, PinnedRuntimeConfigurationCachePort, RuntimeConfigurationTarget,
    install_pinned_runtime_configuration_cache, publish_pinned_runtime_configuration,
};

/// Minimal in-process pin cache keyed by project id and root.
#[derive(Default)]
struct TestPinnedRuntimeConfigurationCache {
    pins: RwLock<Vec<PinnedRuntimeConfiguration>>,
}

impl PinnedRuntimeConfigurationCachePort for TestPinnedRuntimeConfigurationCache {
    fn publish(&self, configuration: PinnedRuntimeConfiguration) -> Result<()> {
        let mut pins = self
            .pins
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        pins.retain(|pin| pin.target().project_id != configuration.target().project_id);
        pins.push(configuration);
        Ok(())
    }

    fn cached_for_root(&self, project_root: &Path) -> Result<PinnedRuntimeConfiguration> {
        self.pins
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|pin| pin.target().project_root == project_root)
            .cloned()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!("no test pin published for root {}", project_root.display()),
            })
    }

    fn cached_for_project(&self, project_id: &ProjectId) -> Result<PinnedRuntimeConfiguration> {
        self.pins
            .read()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .iter()
            .find(|pin| &pin.target().project_id == project_id)
            .cloned()
            .ok_or_else(|| TraceDecayError::Config {
                message: format!("no test pin published for project {}", project_id.as_str()),
            })
    }
}

/// Publishes a pin for `project_id` whose only non-default setting is the
/// LCM summarizer binding. Installs the in-process test cache on first use;
/// when the composition root already installed its cache, the pin is
/// published through that one instead.
pub fn pin_lcm_summarizer_executables(
    project_id: ProjectId,
    project_root: &Path,
    executables: LcmSummarizerExecutablesV1,
) -> Result<PinnedRuntimeConfiguration> {
    let _ = install_pinned_runtime_configuration_cache(Arc::new(
        TestPinnedRuntimeConfigurationCache::default(),
    ));
    let registry = ConfigurationRegistry::core().map_err(|error| TraceDecayError::Config {
        message: format!("configuration registry unavailable: {error}"),
    })?;
    let revision_id = ConfigurationRevisionId::new(format!(
        "configuration.test.lcm-summarizers.{}",
        project_id.as_str().replace(['.', '/'], "-")
    ))
    .map_err(|error| TraceDecayError::Config {
        message: format!("test revision id: {error}"),
    })?;
    let key = SettingKey::new(LCM_SUMMARIZER_EXECUTABLES_SETTING_KEY).map_err(|error| {
        TraceDecayError::Config {
            message: format!("summarizer setting key: {error}"),
        }
    })?;
    let resolution = resolve_configuration(
        &registry,
        &[ConfigurationLayerV1 {
            layer: ConfigurationLayerIdV1::Project {
                project_id: project_id.clone(),
            },
            revision_id: revision_id.clone(),
            entries: BTreeMap::from([(
                key,
                ConfigurationValueV1::LcmSummarizerExecutables(executables),
            )]),
        }],
    )
    .map_err(|error| TraceDecayError::Config {
        message: format!("resolve test configuration: {error}"),
    })?;
    let pinned = PinnedRuntimeConfiguration::new(
        RuntimeConfigurationTarget {
            project_id,
            project_root: project_root.to_path_buf(),
        },
        revision_id,
        resolution.snapshot,
    )?;
    publish_pinned_runtime_configuration(pinned.clone())?;
    Ok(pinned)
}
