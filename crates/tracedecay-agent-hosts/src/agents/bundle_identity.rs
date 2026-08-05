use std::collections::BTreeSet;
use std::path::{Component, Path};

use sha2::{Digest, Sha256};

use crate::errors::{Result, TraceDecayError};

const DIGEST_DOMAIN: &[u8] = b"tracedecay.rendered-host-bundle.v1";

pub(crate) fn rendered_bundle_content_digest(
    files: &[(&str, String)],
) -> Result<([u8; 32], Vec<String>)> {
    let mut files = files.iter().collect::<Vec<_>>();
    files.sort_by(|(left, _), (right, _)| left.cmp(right));
    let mut digest = Sha256::new();
    digest.update(DIGEST_DOMAIN);
    let mut relatives = Vec::with_capacity(files.len());
    for (relative, contents) in files {
        validate_relative_path(relative)?;
        update_digest(&mut digest, relative, contents.as_bytes());
        relatives.push((*relative).to_string());
    }
    Ok((digest.finalize().into(), relatives))
}

pub(crate) fn observed_bundle_content_digest(
    root: &Path,
    relatives: &[String],
) -> Result<Option<[u8; 32]>> {
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(config_error(format!(
                "unsafe native plugin bundle root {}",
                root.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(config_error(format!(
                "could not inspect native plugin bundle root {}: {error}",
                root.display()
            )));
        }
    }

    let mut relatives = relatives.iter().collect::<Vec<_>>();
    relatives.sort();
    let mut digest = Sha256::new();
    digest.update(DIGEST_DOMAIN);
    for relative in relatives {
        validate_relative_path(relative)?;
        let Some(bytes) = read_regular_bundle_file(root, relative)? else {
            return Ok(None);
        };
        update_digest(&mut digest, relative, &bytes);
    }
    Ok(Some(digest.finalize().into()))
}

pub(crate) fn observed_bundle_discovery_matches(
    source_root: &Path,
    cache_root: &Path,
    expected_relatives: &[String],
    discovery_roots: &[&str],
) -> Result<bool> {
    let Some(source_paths) = discovered_bundle_paths(source_root, discovery_roots)? else {
        return Ok(false);
    };
    let Some(cache_paths) = discovered_bundle_paths(cache_root, discovery_roots)? else {
        return Ok(false);
    };
    if source_paths != cache_paths {
        return Ok(false);
    }
    let expected = expected_relatives
        .iter()
        .map(String::as_str)
        .collect::<BTreeSet<_>>();
    if source_paths
        .iter()
        .any(|relative| !expected.contains(relative.as_str()))
    {
        return Ok(false);
    }
    let Some(source_digest) = observed_bundle_content_digest(source_root, &source_paths)? else {
        return Ok(false);
    };
    let Some(cache_digest) = observed_bundle_content_digest(cache_root, &cache_paths)? else {
        return Ok(false);
    };
    Ok(source_digest == cache_digest)
}

fn discovered_bundle_paths(root: &Path, discovery_roots: &[&str]) -> Result<Option<Vec<String>>> {
    match std::fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(config_error(format!(
                "unsafe native plugin bundle root {}",
                root.display()
            )));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(config_error(format!(
                "could not inspect native plugin bundle root {}: {error}",
                root.display()
            )));
        }
    }
    let mut paths = Vec::new();
    for relative in discovery_roots {
        validate_relative_path(relative)?;
        collect_discovered_files(root, &root.join(relative), &mut paths)?;
    }
    paths.sort();
    paths.dedup();
    Ok(Some(paths))
}

fn collect_discovered_files(root: &Path, path: &Path, paths: &mut Vec<String>) -> Result<()> {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            return Err(config_error(format!(
                "refusing symlinked native plugin discovery path {}",
                path.display()
            )));
        }
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(config_error(format!(
                "could not inspect native plugin discovery path {}: {error}",
                path.display()
            )));
        }
    };
    if metadata.is_file() {
        let relative = bundle_relative_path(root, path)?;
        if is_auto_discovered_entrypoint(&relative) {
            paths.push(relative);
        }
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(config_error(format!(
            "native plugin discovery path is not a file or directory: {}",
            path.display()
        )));
    }
    for entry in std::fs::read_dir(path).map_err(|error| {
        config_error(format!(
            "could not read native plugin discovery directory {}: {error}",
            path.display()
        ))
    })? {
        let entry = entry.map_err(|error| {
            config_error(format!(
                "could not inspect an entry under {}: {error}",
                path.display()
            ))
        })?;
        collect_discovered_files(root, &entry.path(), paths)?;
    }
    Ok(())
}

fn is_auto_discovered_entrypoint(relative: &str) -> bool {
    let path = Path::new(relative);
    if relative.starts_with("skills/") {
        return path.file_name().and_then(|name| name.to_str()) == Some("SKILL.md");
    }
    if relative.starts_with("agents/") || relative.starts_with("commands/") {
        return path.extension().and_then(|extension| extension.to_str()) == Some("md");
    }
    if relative.starts_with("hooks/")
        || relative.starts_with(".claude-plugin/")
        || relative.starts_with(".codex-plugin/")
    {
        return path.extension().and_then(|extension| extension.to_str()) == Some("json");
    }
    false
}

fn bundle_relative_path(root: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(root).map_err(|error| {
        config_error(format!(
            "native plugin discovery path {} escaped {}: {error}",
            path.display(),
            root.display()
        ))
    })?;
    let mut parts = Vec::new();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(config_error(format!(
                "unsafe native plugin discovery path {}",
                path.display()
            )));
        };
        parts.push(component.to_str().ok_or_else(|| {
            config_error(format!(
                "native plugin discovery path is not UTF-8: {}",
                path.display()
            ))
        })?);
    }
    Ok(parts.join("/"))
}

fn read_regular_bundle_file(root: &Path, relative: &str) -> Result<Option<Vec<u8>>> {
    let mut path = root.to_path_buf();
    for component in Path::new(relative).components() {
        let Component::Normal(component) = component else {
            return Err(config_error(format!(
                "unsafe native plugin bundle path {relative:?}"
            )));
        };
        path.push(component);
        match std::fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(config_error(format!(
                    "refusing symlinked native plugin bundle path {}",
                    path.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(config_error(format!(
                    "could not inspect native plugin bundle path {}: {error}",
                    path.display()
                )));
            }
        }
    }
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        config_error(format!(
            "could not inspect native plugin bundle file {}: {error}",
            path.display()
        ))
    })?;
    if !metadata.is_file() {
        return Err(config_error(format!(
            "native plugin bundle path is not a file: {}",
            path.display()
        )));
    }
    std::fs::read(&path).map(Some).map_err(|error| {
        config_error(format!(
            "could not read native plugin bundle file {}: {error}",
            path.display()
        ))
    })
}

fn validate_relative_path(relative: &str) -> Result<()> {
    let path = Path::new(relative);
    if relative.is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(config_error(format!(
            "unsafe native plugin bundle path {relative:?}"
        )));
    }
    Ok(())
}

fn update_digest(digest: &mut Sha256, relative: &str, bytes: &[u8]) {
    digest.update((relative.len() as u64).to_be_bytes());
    digest.update(relative.as_bytes());
    digest.update((bytes.len() as u64).to_be_bytes());
    digest.update(bytes);
}

fn config_error(message: String) -> TraceDecayError {
    TraceDecayError::Config { message }
}
