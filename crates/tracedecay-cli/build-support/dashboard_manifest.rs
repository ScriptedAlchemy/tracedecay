use flate2::read::MultiGzDecoder;
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashSet},
    error::Error,
    fmt, fs,
    io::Read,
    path::{Component, Path},
};

pub const DASHBOARD_ASSET_MANIFEST: &str = "asset-manifest.json";
const DASHBOARD_ENTRYPOINT: &str = "index.html";

#[derive(Debug, PartialEq, Eq)]
pub struct DashboardManifest {
    /// The complete producer inventory, including selected gzip payloads.
    pub asset_paths: Vec<String>,
    /// Original route -> encoded payload. Payloads are not additional routes.
    pub gzip: BTreeMap<String, String>,
}

impl DashboardManifest {
    pub fn embedded_paths(&self) -> impl Iterator<Item = (&str, &str)> {
        let payloads = self
            .gzip
            .values()
            .map(String::as_str)
            .collect::<HashSet<_>>();
        self.asset_paths
            .iter()
            .filter(move |relative| !payloads.contains(relative.as_str()))
            .map(|relative| {
                (
                    relative.as_str(),
                    self.gzip.get(relative).unwrap_or(relative).as_str(),
                )
            })
    }
}

#[derive(Debug)]
pub struct DashboardManifestError {
    message: String,
}

impl DashboardManifestError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for DashboardManifestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl Error for DashboardManifestError {}

pub fn read_dashboard_manifest(
    app_dist: &Path,
) -> Result<DashboardManifest, DashboardManifestError> {
    read_dashboard_manifest_with(app_dist, false)
}

/// Same manifest scan, but the source tree may contain symlinks whose targets
/// are regular files or directories. Only for inputs a build system hands us
/// explicitly (`TRACEDECAY_DASHBOARD_DIST_DIR`), where sandboxed inputs arrive
/// as symlinks to action outputs and the caller still compares the staged
/// digest byte-for-byte against the producer's advertised digest. Packaged
/// and in-tree bundles keep the strict no-symlink rule.
pub fn read_dashboard_manifest_build_output(
    app_dist: &Path,
) -> Result<DashboardManifest, DashboardManifestError> {
    read_dashboard_manifest_with(app_dist, true)
}

fn read_dashboard_manifest_with(
    app_dist: &Path,
    allow_source_symlinks: bool,
) -> Result<DashboardManifest, DashboardManifestError> {
    let metadata_of = |path: &Path| {
        if allow_source_symlinks {
            fs::metadata(path)
        } else {
            fs::symlink_metadata(path)
        }
    };
    let app_dist_metadata = metadata_of(app_dist).map_err(|error| {
        DashboardManifestError::new(format!(
            "failed to inspect dashboard app-dist root {}: {error}",
            app_dist.display()
        ))
    })?;
    if !allow_source_symlinks && app_dist_metadata.file_type().is_symlink() {
        return Err(DashboardManifestError::new(format!(
            "dashboard app-dist root {} must not be a symlink",
            app_dist.display()
        )));
    }

    let manifest_path = app_dist.join(DASHBOARD_ASSET_MANIFEST);
    let manifest_metadata = metadata_of(&manifest_path).map_err(|error| {
        DashboardManifestError::new(format!(
            "failed to inspect dashboard asset manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    if !allow_source_symlinks && manifest_metadata.file_type().is_symlink() {
        return Err(DashboardManifestError::new(format!(
            "dashboard asset manifest {} must not be a symlink",
            manifest_path.display()
        )));
    }
    if !manifest_metadata.is_file() {
        return Err(DashboardManifestError::new(format!(
            "dashboard asset manifest {} is not a file",
            manifest_path.display()
        )));
    }
    let manifest_bytes = fs::read(&manifest_path).map_err(|error| {
        DashboardManifestError::new(format!(
            "failed to read dashboard asset manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let manifest: Value = serde_json::from_slice(&manifest_bytes).map_err(|error| {
        DashboardManifestError::new(format!(
            "failed to parse dashboard asset manifest {}: {error}",
            manifest_path.display()
        ))
    })?;
    let all_files = manifest
        .get("allFiles")
        .and_then(Value::as_array)
        .ok_or_else(|| {
            DashboardManifestError::new(format!(
                "dashboard asset manifest {} must contain an allFiles array",
                manifest_path.display()
            ))
        })?;

    let mut paths = Vec::with_capacity(all_files.len());
    let mut seen = HashSet::with_capacity(all_files.len());
    for value in all_files {
        let raw_path = value.as_str().ok_or_else(|| {
            DashboardManifestError::new(format!(
                "dashboard asset manifest {} contains a non-string allFiles entry",
                manifest_path.display()
            ))
        })?;
        let relative = normalize_manifest_path(raw_path)?;
        if !seen.insert(relative.clone()) {
            return Err(DashboardManifestError::new(format!(
                "dashboard asset manifest {} contains duplicate path {relative:?}",
                manifest_path.display()
            )));
        }
        let mut asset_path = app_dist.to_path_buf();
        let mut asset_is_file = false;
        for component in relative.split('/') {
            asset_path.push(component);
            let metadata = metadata_of(&asset_path).map_err(|error| {
                DashboardManifestError::new(format!(
                    "dashboard asset manifest lists missing file {relative:?} at {}: {error}",
                    asset_path.display()
                ))
            })?;
            if !allow_source_symlinks && metadata.file_type().is_symlink() {
                return Err(DashboardManifestError::new(format!(
                    "dashboard asset manifest path component {} must not be a symlink",
                    asset_path.display()
                )));
            }
            asset_is_file = metadata.is_file();
        }
        if !asset_is_file {
            return Err(DashboardManifestError::new(format!(
                "dashboard asset manifest entry {relative:?} is not a file at {}",
                asset_path.display()
            )));
        }
        paths.push(relative);
    }

    if !seen.contains(DASHBOARD_ENTRYPOINT) {
        return Err(DashboardManifestError::new(format!(
            "dashboard asset manifest {} must list {DASHBOARD_ENTRYPOINT}",
            manifest_path.display()
        )));
    }
    paths.sort_unstable();
    let mut gzip = BTreeMap::new();
    if let Some(mapping) = manifest.get("gzip") {
        let mapping = mapping.as_object().ok_or_else(|| {
            DashboardManifestError::new("dashboard asset manifest gzip must be an object")
        })?;
        for (source, encoded) in mapping {
            let source = normalize_manifest_path(source)?;
            let encoded = encoded.as_str().ok_or_else(|| {
                DashboardManifestError::new("dashboard gzip payload path must be a string")
            })?;
            let encoded = normalize_manifest_path(encoded)?;
            if encoded != format!("{source}.gz")
                || !seen.contains(&source)
                || !seen.contains(&encoded)
                || gzip.insert(source.clone(), encoded).is_some()
            {
                return Err(DashboardManifestError::new(format!(
                    "invalid dashboard gzip mapping for {source:?}"
                )));
            }
        }
        for (source, encoded) in &gzip {
            if gzip.contains_key(encoded) {
                return Err(DashboardManifestError::new(format!(
                    "dashboard gzip payload {encoded:?} must not be another source"
                )));
            }
            let read = |relative: &str| {
                fs::read(app_dist.join(relative)).map_err(|error| {
                    DashboardManifestError::new(format!(
                        "failed to read dashboard gzip asset {relative:?}: {error}"
                    ))
                })
            };
            let raw = read(source)?;
            let compressed = read(encoded)?;
            let mut roundtrip = Vec::new();
            MultiGzDecoder::new(compressed.as_slice())
                .take(raw.len() as u64 + 1)
                .read_to_end(&mut roundtrip)
                .map_err(|error| {
                    DashboardManifestError::new(format!(
                        "invalid dashboard gzip payload {encoded:?}: {error}"
                    ))
                })?;
            if compressed.len() >= raw.len() || roundtrip != raw {
                return Err(DashboardManifestError::new(format!(
                    "dashboard gzip payload does not match {source:?} or is not smaller"
                )));
            }
        }
    }
    Ok(DashboardManifest {
        asset_paths: paths,
        gzip,
    })
}

fn normalize_manifest_path(raw_path: &str) -> Result<String, DashboardManifestError> {
    if is_unsafe_manifest_path(raw_path) {
        return Err(unsafe_manifest_path(raw_path));
    }

    let mut segments = Vec::new();
    for component in Path::new(raw_path).components() {
        match component {
            Component::Normal(segment) => {
                let segment = segment
                    .to_str()
                    .ok_or_else(|| unsafe_manifest_path(raw_path))?;
                segments.push(segment);
            }
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(unsafe_manifest_path(raw_path));
            }
        }
    }
    if segments.is_empty() {
        return Err(unsafe_manifest_path(raw_path));
    }
    Ok(segments.join("/"))
}

fn is_unsafe_manifest_path(raw_path: &str) -> bool {
    raw_path.is_empty()
        || raw_path
            .chars()
            .any(|character| matches!(character, '\\' | '?' | '#' | '%' | ':'))
        || Path::new(raw_path).is_absolute()
        || raw_path.split('/').any(|segment| segment == "..")
}

fn unsafe_manifest_path(raw_path: &str) -> DashboardManifestError {
    DashboardManifestError::new(format!(
        "dashboard asset manifest path {raw_path:?} must be a relative path without traversal"
    ))
}
