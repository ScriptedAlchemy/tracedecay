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
