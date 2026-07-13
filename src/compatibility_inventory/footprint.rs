use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

mod model;

pub use model::*;

use sha2::{Digest, Sha256};

use super::model::{
    CompatibilityEntryV1, EntityDispositionV1, InventoryGatesV1, InventoryOwnersV1, RouteStatusV1,
};
pub fn collect_footprint(
    architecture_toml: &str,
    cargo_metadata_json: &str,
    mut descriptors: CheckedFootprintDescriptors,
) -> Result<FootprintSnapshot, FootprintError> {
    let architecture: ArchitectureManifest = toml::from_str(architecture_toml)
        .map_err(|error| FootprintError::Architecture(error.to_string()))?;
    let cargo: CargoMetadata = serde_json::from_str(cargo_metadata_json)
        .map_err(|error| FootprintError::CargoMetadata(error.to_string()))?;

    validate_descriptors(&descriptors)?;
    let workspace_root = Path::new(&cargo.workspace_root);
    let workspace_members: BTreeSet<&str> =
        cargo.workspace_members.iter().map(String::as_str).collect();
    let mut packages = Vec::new();
    let mut dependency_edges = Vec::new();

    for package in cargo
        .packages
        .iter()
        .filter(|package| workspace_members.contains(package.id.as_str()))
    {
        let manifest_ref = relative_ref(workspace_root, &package.manifest_path)?;
        let mut features: Vec<_> = package.features.keys().cloned().collect();
        features.sort();
        let mut targets = package
            .targets
            .iter()
            .map(|target| {
                let mut kinds = target.kind.clone();
                kinds.sort();
                kinds.dedup();
                Ok(TargetFootprint {
                    name: target.name.clone(),
                    kinds,
                    source_ref: relative_ref(workspace_root, &target.src_path)?,
                })
            })
            .collect::<Result<Vec<_>, FootprintError>>()?;
        targets.sort();

        for dependency in &package.dependencies {
            let mut features = dependency.features.clone();
            features.sort();
            features.dedup();
            dependency_edges.push(DependencyEdge {
                package: package.name.clone(),
                manifest_ref: manifest_ref.clone(),
                dependency: dependency.name.clone(),
                alias: dependency.rename.clone(),
                kind: dependency
                    .kind
                    .clone()
                    .unwrap_or_else(|| "normal".to_owned()),
                target: dependency.target.clone(),
                optional: dependency.optional,
                default_features: dependency.uses_default_features,
                features,
            });
        }

        packages.push(PackageFootprint {
            name: package.name.clone(),
            manifest_ref,
            features,
            targets,
            dependency_count: package.dependencies.len(),
        });
    }

    packages.sort_by(|left, right| left.name.cmp(&right.name));
    dependency_edges.sort();

    let mut architecture_edges = architecture.edges;
    architecture_edges.sort();
    let mut semantic_clusters = architecture.replaced_v1_clusters;
    semantic_clusters.sort_by(|left, right| left.id.cmp(&right.id));
    let mut adapters = architecture.adapter_contracts;
    for adapter in &mut adapters {
        adapter.required_fields.sort();
        adapter.required_fields.dedup();
    }
    adapters.sort_by(|left, right| left.id.cmp(&right.id));
    let mut convergence_metrics = architecture.scorecard;
    convergence_metrics.sort_by(|left, right| left.metric.cmp(&right.metric));

    validate_generated_views(&architecture.generated_views, &descriptors.generated_views)?;
    let package_count = u64::try_from(packages.len()).map_err(|_| {
        FootprintError::Architecture("workspace package count exceeds u64".to_owned())
    })?;
    if package_count > architecture.package_ceiling {
        return Err(FootprintError::Architecture(format!(
            "workspace package count {package_count} exceeds declared ceiling {}",
            architecture.package_ceiling
        )));
    }
    sort_descriptors(&mut descriptors);

    Ok(FootprintSnapshot {
        schema: FOOTPRINT_SCHEMA.to_owned(),
        package_count: packages.len(),
        rust_package_ceiling: architecture.package_ceiling,
        packages,
        dependency_edges,
        architecture_edges,
        public_items: descriptors.public_items,
        extension_points: descriptors.extension_points,
        duplicate_clusters: descriptors.duplicate_clusters,
        semantic_clusters,
        generated_views: descriptors.generated_views,
        storage: descriptors.storage,
        runtime: descriptors.runtime,
        adapters,
        negative_code: descriptors.negative_code,
        convergence_metrics,
        budgets: architecture.budgets,
    })
}

fn validate_descriptors(descriptors: &CheckedFootprintDescriptors) -> Result<(), FootprintError> {
    require_unique(
        descriptors
            .extension_points
            .iter()
            .map(|value| value.id.as_str()),
    )?;
    require_unique(
        descriptors
            .duplicate_clusters
            .iter()
            .map(|value| value.id.as_str()),
    )?;
    require_unique(
        descriptors
            .generated_views
            .iter()
            .map(|value| value.output_ref.as_str()),
    )?;
    require_unique(descriptors.storage.iter().map(|value| value.id.as_str()))?;
    require_unique(descriptors.runtime.iter().map(|value| value.id.as_str()))?;
    require_unique(
        descriptors
            .negative_code
            .iter()
            .map(|value| value.id.as_str()),
    )?;
    for public_items in &descriptors.public_items {
        validate_relative_ref(&public_items.source_ref)?;
    }
    for extension in &descriptors.extension_points {
        validate_relative_ref(&extension.source_ref)?;
    }
    for cluster in &descriptors.duplicate_clusters {
        for member in &cluster.members {
            validate_relative_ref(member)?;
        }
    }
    for view in &descriptors.generated_views {
        validate_relative_ref(&view.output_ref)?;
    }
    for store in &descriptors.storage {
        validate_relative_ref(&store.source_ref)?;
    }
    for runtime in &descriptors.runtime {
        validate_relative_ref(&runtime.binary_ref)?;
        validate_relative_ref(&runtime.evidence_ref)?;
    }
    for delta in &descriptors.negative_code {
        validate_relative_ref(&delta.evidence_ref)?;
    }
    Ok(())
}

fn validate_generated_views(
    architecture_views: &[String],
    descriptors: &[GeneratedViewDescriptor],
) -> Result<(), FootprintError> {
    for descriptor in descriptors {
        if descriptor.expected_digest.trim().is_empty()
            || descriptor.actual_digest.trim().is_empty()
        {
            return Err(FootprintError::Architecture(format!(
                "generated-view digests must be non-empty for {}",
                descriptor.output_ref
            )));
        }
        if descriptor.expected_digest != descriptor.actual_digest {
            return Err(FootprintError::Architecture(format!(
                "generated-view digest mismatch for {}: expected {}, actual {}",
                descriptor.output_ref, descriptor.expected_digest, descriptor.actual_digest
            )));
        }
    }
    let descriptors: BTreeMap<&str, &GeneratedViewDescriptor> = descriptors
        .iter()
        .map(|descriptor| (descriptor.output_ref.as_str(), descriptor))
        .collect();
    let mut known = BTreeSet::new();
    for output_ref in architecture_views {
        validate_relative_ref(output_ref)?;
        if !known.insert(output_ref.as_str()) {
            return Err(FootprintError::DuplicateDescriptor(output_ref.clone()));
        }
        descriptors.get(output_ref.as_str()).ok_or_else(|| {
            FootprintError::Architecture(format!(
                "missing generated-view descriptor for {output_ref}"
            ))
        })?;
    }
    Ok(())
}

fn sort_descriptors(descriptors: &mut CheckedFootprintDescriptors) {
    descriptors.public_items.sort();
    descriptors.extension_points.sort();
    for cluster in &mut descriptors.duplicate_clusters {
        cluster.members.sort();
        cluster.members.dedup();
    }
    descriptors.duplicate_clusters.sort();
    descriptors.generated_views.sort();
    descriptors.storage.sort();
    descriptors.runtime.sort();
    descriptors.negative_code.sort();
}

fn require_unique<'a>(values: impl IntoIterator<Item = &'a str>) -> Result<(), FootprintError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if !seen.insert(value) {
            return Err(FootprintError::DuplicateDescriptor(value.to_owned()));
        }
    }
    Ok(())
}

pub fn collect_footprint_entries(
    architecture_toml: &str,
    cargo_metadata_json: &str,
    descriptors: CheckedFootprintDescriptors,
) -> Result<Vec<CompatibilityEntryV1>, FootprintError> {
    Ok(collect_footprint(architecture_toml, cargo_metadata_json, descriptors)?.into_entries())
}

impl FootprintSnapshot {
    pub fn into_entries(self) -> Vec<CompatibilityEntryV1> {
        let mut entries = Vec::new();
        let budget_metrics = [
            (
                "rust-package-count",
                format!(
                    "actual={}; ceiling={}",
                    self.package_count, self.rust_package_ceiling
                ),
            ),
            (
                "definite-duplicate-body-lines",
                format!("maximum={}", self.budgets.definite_duplicate_body_lines),
            ),
            (
                "default-binary-ratio",
                format!("maximum={}", self.budgets.default_binary_ratio_max),
            ),
            (
                "idle-rss-ratio",
                format!("maximum={}", self.budgets.idle_rss_ratio_max),
            ),
            (
                "hot-build-ratio",
                format!("maximum={}", self.budgets.hot_build_ratio_max),
            ),
            (
                "clean-build-ratio",
                format!("maximum={}", self.budgets.clean_build_ratio_max),
            ),
            (
                "negative-code-parity",
                self.budgets.parity_replacement.clone(),
            ),
            (
                "generated-accounting",
                self.budgets.generated_accounting.clone(),
            ),
        ];
        for (metric, value) in budget_metrics {
            entries.push(entry(
                stable_id("footprint:budget", metric),
                "convergence_metric",
                format!("{metric} [{value}]"),
                vec!["architecture-boundaries.toml".to_owned()],
                "architecture",
                "restore the checked architecture budget",
                "not-applicable",
            ));
        }

        for package in self.packages {
            let package_name = package.name.clone();
            entries.push(entry(
                stable_id("footprint:package", &package.name),
                "package",
                package.name.clone(),
                vec![package.manifest_ref],
                package.name.clone(),
                "regenerate from cargo metadata",
                "not-applicable",
            ));
            for target in package.targets {
                let kind = if target.kinds.iter().any(|kind| kind == "bin") {
                    "binary"
                } else if target.kinds.iter().any(|kind| kind == "custom-build") {
                    "build_artifact"
                } else {
                    "runtime_artifact"
                };
                entries.push(entry(
                    stable_id(
                        "footprint:target",
                        &format!("{package_name}:{}:{}", target.name, target.kinds.join(",")),
                    ),
                    kind,
                    format!(
                        "{package_name}::{} [{}]",
                        target.name,
                        target.kinds.join(",")
                    ),
                    vec![target.source_ref],
                    package_name.clone(),
                    "regenerate from cargo metadata",
                    "not-applicable",
                ));
            }
        }
        for dependency in self.dependency_edges {
            entries.push(entry(
                stable_id(
                    "footprint:dependency",
                    &format!(
                        "{}:{}:{}:{}:{}",
                        dependency.package,
                        dependency.kind,
                        dependency.dependency,
                        dependency.alias.as_deref().unwrap_or(""),
                        dependency.target.as_deref().unwrap_or("")
                    ),
                ),
                "dependency",
                format!(
                    "{} -> {} [alias={}; kind={}; target={}; optional={}; default_features={}; features={}]",
                    dependency.package,
                    dependency.dependency,
                    dependency.alias.as_deref().unwrap_or(""),
                    dependency.kind,
                    dependency.target.as_deref().unwrap_or(""),
                    dependency.optional,
                    dependency.default_features,
                    dependency.features.join(",")
                ),
                vec![dependency.manifest_ref],
                dependency.package,
                "regenerate from cargo metadata",
                "not-applicable",
            ));
        }
        for dependency in self.architecture_edges {
            entries.push(entry(
                stable_id(
                    "footprint:module-edge",
                    &format!("{}:{}", dependency.from, dependency.to),
                ),
                "module_dependency",
                format!("{} -> {}", dependency.from, dependency.to),
                vec!["architecture-boundaries.toml".to_owned()],
                dependency.from,
                "regenerate from architecture-boundaries.toml",
                "not-applicable",
            ));
        }
        for count in self.public_items {
            entries.push(entry(
                stable_id("footprint:public-items", &count.owner),
                "public_item_count",
                format!("{}:{}", count.owner, count.count),
                vec![count.source_ref],
                count.owner,
                "regenerate from checked code-health descriptors",
                "not-applicable",
            ));
        }
        for extension in self.extension_points {
            entries.push(entry(
                stable_id("footprint:extension", &extension.id),
                "extension_point",
                extension.id,
                vec![extension.source_ref],
                extension.owner,
                "regenerate from checked code-health descriptors",
                "not-applicable",
            ));
        }
        for cluster in self.duplicate_clusters {
            entries.push(entry(
                stable_id("footprint:duplicate", &cluster.id),
                "duplicate_cluster",
                format!("{} [{}]", cluster.id, cluster.classification),
                cluster.members,
                cluster.owner,
                "regenerate from the checked redundancy view",
                "PR 37",
            ));
        }
        for cluster in self.semantic_clusters {
            entries.push(entry(
                stable_id("footprint:semantic", &cluster.id),
                "semantic_implementation",
                format!("{} [{}]", cluster.id, cluster.disposition),
                vec!["architecture-boundaries.toml".to_owned()],
                cluster.owner,
                "restore the checked architecture manifest",
                &cluster.delete_by_pr,
            ));
        }
        for view in self.generated_views {
            entries.push(entry(
                stable_id("footprint:generated", &view.output_ref),
                "generated_binding",
                format!(
                    "{} [expected={}; actual={}]",
                    view.output_ref, view.expected_digest, view.actual_digest
                ),
                vec![view.output_ref],
                "architecture",
                "regenerate from checked architecture inputs",
                "not-applicable",
            ));
        }
        for store in self.storage {
            entries.push(entry(
                stable_id("footprint:storage", &store.id),
                "storage_artifact",
                format!(
                    "{} [files={}; bytes={}]",
                    store.id, store.file_count, store.byte_count
                ),
                vec![store.source_ref],
                store.owner,
                "recreate from supported store metadata",
                "PR 37",
            ));
        }
        for runtime in self.runtime {
            entries.push(entry(
                stable_id("footprint:runtime", &runtime.id),
                "runtime_artifact",
                format!(
                    "{} [binary_bytes={}; idle_rss_bytes={}; startup_ms={}; hot_build_ms={}; clean_build_ms={}]",
                    runtime.id,
                    runtime.binary_bytes,
                    runtime.idle_rss_bytes,
                    runtime.startup_millis,
                    runtime.hot_build_millis,
                    runtime.clean_build_millis
                ),
                vec![runtime.binary_ref, runtime.evidence_ref],
                runtime.owner,
                "re-run the pinned footprint measurement",
                "not-applicable",
            ));
        }
        for adapter in self.adapters {
            entries.push(entry(
                stable_id("footprint:adapter", &adapter.id),
                "adapter",
                format!(
                    "{} [new_callers_forbidden={}; policy_forbidden={}; required={}]",
                    adapter.id,
                    adapter.new_callers_forbidden,
                    adapter.policy_forbidden,
                    adapter.required_fields.join(",")
                ),
                vec!["architecture-boundaries.toml".to_owned()],
                adapter.owner,
                "restore the V1 route while its bounded rollback gate is open",
                &adapter.delete_by_pr,
            ));
        }
        for delta in self.negative_code {
            entries.push(entry(
                stable_id("footprint:negative-code", &delta.id),
                "negative_code",
                format!(
                    "{} [retired_v1={}; adapter={}; handwritten_v2={}; generated_v2={}]",
                    delta.id,
                    delta.retired_v1_lines,
                    delta.adapter_lines,
                    delta.handwritten_v2_lines,
                    delta.generated_v2_lines
                ),
                vec![delta.evidence_ref],
                delta.owner,
                "recompute from the pinned base and path disposition",
                "PR 37",
            ));
        }
        for metric in self.convergence_metrics {
            entries.push(entry(
                stable_id("footprint:metric", &metric.metric),
                "convergence_metric",
                format!(
                    "{} [detector={}; target={}]",
                    metric.metric, metric.detector, metric.target
                ),
                vec!["architecture-boundaries.toml".to_owned()],
                "architecture",
                "re-run the named detector",
                "not-applicable",
            ));
        }

        for entry in &mut entries {
            entry.source_refs.sort();
            entry.source_refs.dedup();
        }
        entries.sort_by(|left, right| left.stable_id.cmp(&right.stable_id));
        entries
    }
}

fn entry(
    stable_id: String,
    kind: &str,
    canonical_name: String,
    source_refs: Vec<String>,
    v2_owner: impl Into<String>,
    recovery: &str,
    delete_by_pr: &str,
) -> CompatibilityEntryV1 {
    CompatibilityEntryV1 {
        stable_id,
        kind: kind.to_owned(),
        canonical_name,
        source_refs,
        platform: "all".to_owned(),
        route_status: RouteStatusV1::V2Shadow,
        entity_disposition: EntityDispositionV1::Retained,
        platform_disposition: None,
        owners: InventoryOwnersV1 {
            v1_owner: "root".to_owned(),
            v2_owner: v2_owner.into(),
        },
        readers: vec![],
        writers: vec![],
        tests: vec!["compatibility_inventory_footprint_is_deterministic".to_owned()],
        gates: InventoryGatesV1 {
            parity_gate: "PR3-FOOTPRINT-PARITY".to_owned(),
            cutover_gate: "PR37-CUTOVER".to_owned(),
        },
        recovery: recovery.to_owned(),
        delete_by_pr: delete_by_pr.to_owned(),
    }
}

fn stable_id(prefix: &str, key: &str) -> String {
    let mut component = String::with_capacity(key.len());
    for byte in key.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b':' | b'-') {
            component.push(char::from(byte));
        } else {
            component.push('_');
            component.push_str(&format!("{byte:02x}"));
        }
    }
    let candidate = format!("{prefix}:{component}");
    if candidate.len() <= 128 {
        candidate
    } else {
        let digest = Sha256::digest(candidate.as_bytes());
        format!("{prefix}:{}", hex::encode(digest))
    }
}

fn relative_ref(workspace_root: &Path, absolute: &str) -> Result<String, FootprintError> {
    let absolute = Path::new(absolute);
    let relative = absolute
        .strip_prefix(workspace_root)
        .map_err(|_| FootprintError::UnsafeReference(absolute.display().to_string()))?;
    let reference = relative.to_string_lossy().replace('\\', "/");
    validate_relative_ref(&reference)?;
    Ok(reference)
}

fn validate_relative_ref(reference: &str) -> Result<(), FootprintError> {
    let path = Path::new(reference);
    if reference.is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir
                    | Component::RootDir
                    | Component::Prefix(_)
                    | Component::CurDir
            )
        })
    {
        return Err(FootprintError::UnsafeReference(reference.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARCHITECTURE: &str = r#"
package_ceiling = 12
generated_views = ["docs/generated/dag.md"]

[[edges]]
from = "application"
to = "domain"

[budgets]
definite_duplicate_body_lines = 10
default_binary_ratio_max = 1.25
idle_rss_ratio_max = 1.25
hot_build_ratio_max = 1.25
clean_build_ratio_max = 1.5
parity_replacement = "smaller"
generated_accounting = "separate"

[[replaced_v1_clusters]]
id = "z-cluster"
owner = "domain"
disposition = "replace"
delete_by_pr = "PR 22A"

[[adapter_contracts]]
id = "compat"
owner = "root"
delete_by_pr = "PR 37"
new_callers_forbidden = true
policy_forbidden = true
required_fields = ["owner", "adapter_id"]

[[scorecard]]
metric = "generated-contract-drift"
detector = "generated-check"
target = "0"
"#;

    fn cargo_metadata(packages: &str, members: &str) -> String {
        format!(
            r#"{{"packages":[{packages}],"workspace_members":[{members}],"workspace_root":"/repo"}}"#
        )
    }

    fn package(name: &str) -> String {
        format!(
            r#"{{"id":"path+file:///repo#{name}@1.0.0","name":"{name}","manifest_path":"/repo/{name}/Cargo.toml","dependencies":[{{"name":"serde","rename":null,"kind":null,"optional":false,"uses_default_features":true,"features":["derive"]}}],"targets":[{{"name":"{name}","kind":["lib"],"src_path":"/repo/{name}/src/lib.rs"}}],"features":{{"default":[]}}}}"#
        )
    }

    fn checked_descriptors() -> CheckedFootprintDescriptors {
        CheckedFootprintDescriptors {
            generated_views: vec![GeneratedViewDescriptor {
                output_ref: "docs/generated/dag.md".into(),
                expected_digest: "sha256:checked".into(),
                actual_digest: "sha256:checked".into(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn collection_is_sorted_and_strips_workspace_absolute_paths() {
        let second = package("second");
        let first = package("first");
        let cargo = cargo_metadata(
            &format!("{second},{first}"),
            r#""path+file:///repo#second@1.0.0","path+file:///repo#first@1.0.0""#,
        );

        let snapshot = collect_footprint(ARCHITECTURE, &cargo, checked_descriptors()).unwrap();

        assert_eq!(snapshot.package_count, 2);
        assert_eq!(snapshot.packages[0].name, "first");
        assert_eq!(snapshot.packages[0].manifest_ref, "first/Cargo.toml");
        assert_eq!(
            snapshot.packages[0].targets[0].source_ref,
            "first/src/lib.rs"
        );
        assert_eq!(
            snapshot.adapters[0].required_fields,
            ["adapter_id", "owner"]
        );
        assert_eq!(
            snapshot.generated_views[0].output_ref,
            "docs/generated/dag.md"
        );

        let entries = snapshot.into_entries();
        assert!(
            entries
                .windows(2)
                .all(|pair| pair[0].stable_id < pair[1].stable_id)
        );
        assert!(
            entries
                .iter()
                .flat_map(|entry| &entry.source_refs)
                .all(|path| {
                    !Path::new(path).is_absolute() && !path.split('/').any(|part| part == "..")
                })
        );
    }

    #[test]
    fn output_is_byte_identical_for_reordered_checked_descriptors() {
        let package = package("only");
        let cargo = cargo_metadata(&package, r#""path+file:///repo#only@1.0.0""#);
        let left = CheckedFootprintDescriptors {
            public_items: vec![
                NamedCount {
                    owner: "z".into(),
                    count: 2,
                    source_ref: "src/z.rs".into(),
                },
                NamedCount {
                    owner: "a".into(),
                    count: 1,
                    source_ref: "src/a.rs".into(),
                },
            ],
            ..checked_descriptors()
        };
        let mut right = left.clone();
        right.public_items.reverse();

        let left = collect_footprint(ARCHITECTURE, &cargo, left).unwrap();
        let right = collect_footprint(ARCHITECTURE, &cargo, right).unwrap();

        assert_eq!(
            serde_json::to_vec(&left).unwrap(),
            serde_json::to_vec(&right).unwrap()
        );
    }

    #[test]
    fn rejects_private_or_parent_traversal_references() {
        let package = package("only");
        let cargo = cargo_metadata(&package, r#""path+file:///repo#only@1.0.0""#);
        let descriptors = CheckedFootprintDescriptors {
            storage: vec![StorageFootprint {
                id: "store".into(),
                owner: "store".into(),
                file_count: 1,
                byte_count: 2,
                source_ref: "/home/user/.tracedecay/private.db".into(),
            }],
            ..Default::default()
        };

        assert!(matches!(
            collect_footprint(ARCHITECTURE, &cargo, descriptors),
            Err(FootprintError::UnsafeReference(_))
        ));
    }

    #[test]
    fn rejects_missing_or_empty_generated_view_digests() {
        let package = package("only");
        let cargo = cargo_metadata(&package, r#""path+file:///repo#only@1.0.0""#);

        assert!(matches!(
            collect_footprint(ARCHITECTURE, &cargo, Default::default()),
            Err(FootprintError::Architecture(message))
                if message.contains("missing generated-view descriptor")
        ));

        for clear_expected in [true, false] {
            let mut descriptors = checked_descriptors();
            if clear_expected {
                descriptors.generated_views[0].expected_digest.clear();
            } else {
                descriptors.generated_views[0].actual_digest.clear();
            }
            assert!(matches!(
                collect_footprint(ARCHITECTURE, &cargo, descriptors),
                Err(FootprintError::Architecture(message))
                    if message.contains("digests must be non-empty")
            ));
        }
    }

    #[test]
    fn rejects_generated_view_digest_mismatch() {
        let package = package("only");
        let cargo = cargo_metadata(&package, r#""path+file:///repo#only@1.0.0""#);
        let mut descriptors = checked_descriptors();
        descriptors.generated_views[0].actual_digest = "sha256:drifted".into();

        assert!(matches!(
            collect_footprint(ARCHITECTURE, &cargo, descriptors),
            Err(FootprintError::Architecture(message))
                if message.contains("generated-view digest mismatch")
        ));
    }

    #[test]
    fn rejects_package_count_above_declared_ceiling() {
        let architecture = ARCHITECTURE.replacen("package_ceiling = 12", "package_ceiling = 1", 1);
        let first = package("first");
        let second = package("second");
        let cargo = cargo_metadata(
            &format!("{first},{second}"),
            r#""path+file:///repo#first@1.0.0","path+file:///repo#second@1.0.0""#,
        );

        assert!(matches!(
            collect_footprint(&architecture, &cargo, checked_descriptors()),
            Err(FootprintError::Architecture(message))
                if message.contains("exceeds declared ceiling")
        ));
    }
}
