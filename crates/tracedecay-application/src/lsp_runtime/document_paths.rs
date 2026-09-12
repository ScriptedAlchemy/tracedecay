//! Document path admission and project file access for LSP requests.

use std::ffi::OsString;
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};

use cap_std::fs::{Dir, File};
use tracedecay_lsp::analyzer::adapters::{LspAdapterDefinition, builtin_adapters};
use tracedecay_lsp::{LspRuntimeFailure, strict_file_url};
use tracedecay_runtime_core::path_safety::canonical_root_identity;
use url::Url;

pub(super) fn adapter_for_path(path: &Path) -> Option<LspAdapterDefinition> {
    let extension = path.extension()?.to_str()?;
    builtin_adapters().into_iter().find(|adapter| {
        adapter.extensions.iter().any(|candidate| {
            candidate
                .strip_prefix('.')
                .unwrap_or(candidate)
                .eq_ignore_ascii_case(extension)
        })
    })
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct ValidatedDocumentPath {
    pub(super) absolute: PathBuf,
    pub(super) relative: PathBuf,
}

#[hotpath::measure(label = "usecases.lsp.document.validate_path")]
pub(super) fn validated_document_path(
    project_root: &Path,
    root_identity: &Path,
    root_uri: &Url,
    project_dir: &Dir,
    document_uri: &str,
) -> Result<ValidatedDocumentPath, LspRuntimeFailure> {
    let url = strict_file_url(document_uri)
        .ok_or_else(|| LspRuntimeFailure::new("document-uri-invalid"))?;
    if root_uri.host_str() != url.host_str() {
        return Err(LspRuntimeFailure::new("document-outside-registered-root"));
    }
    let path = url
        .to_file_path()
        .map_err(|()| LspRuntimeFailure::new("document-uri-invalid"))?;
    // Client URIs may address the admitted root through an OS or worktree
    // alias, and the admitted root itself is whatever spelling the daemon
    // registered. Both sides go through the same identity authority so a
    // `/var` alias, a `\\?\` verbatim root, or an overlay whose suffix does
    // not exist yet still resolves beneath the root it belongs to; the
    // directory capability is retained for normalization and every
    // subsequent file open.
    let path = canonical_root_identity(&path);
    let relative = path
        .strip_prefix(root_identity)
        .map_err(|_| LspRuntimeFailure::new("document-outside-registered-root"))?;
    validate_relative_path(relative)?;
    let relative = normalize_overlay_relative(project_dir, relative)?;
    validate_relative_path(&relative)?;
    Ok(ValidatedDocumentPath {
        absolute: project_root.join(&relative),
        relative,
    })
}

pub(super) fn validate_relative_path(path: &Path) -> Result<(), LspRuntimeFailure> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(LspRuntimeFailure::new("document-path-invalid"));
    }
    Ok(())
}

#[hotpath::measure(label = "usecases.lsp.document.normalize")]
pub(super) fn normalize_overlay_relative(
    project_dir: &Dir,
    relative: &Path,
) -> Result<PathBuf, LspRuntimeFailure> {
    let mut probe = relative.to_path_buf();
    let mut missing_suffix = Vec::<OsString>::new();
    let mut canonical = loop {
        if probe.as_os_str().is_empty() {
            break PathBuf::new();
        }
        match project_dir.canonicalize(&probe) {
            Ok(path) => break path,
            Err(error) if error.kind() == ErrorKind::NotFound => {
                match project_dir.symlink_metadata(&probe) {
                    Ok(_) => {
                        return Err(LspRuntimeFailure::new("document-outside-registered-root"));
                    }
                    Err(metadata_error) if metadata_error.kind() == ErrorKind::NotFound => {}
                    Err(_) => return Err(LspRuntimeFailure::new("document-path-invalid")),
                }
                let name = probe
                    .file_name()
                    .map(OsString::from)
                    .ok_or_else(|| LspRuntimeFailure::new("document-path-invalid"))?;
                missing_suffix.push(name);
                if !probe.pop() {
                    return Err(LspRuntimeFailure::new("document-path-invalid"));
                }
            }
            Err(_) => {
                return Err(LspRuntimeFailure::new("document-outside-registered-root"));
            }
        }
    };
    for component in missing_suffix.into_iter().rev() {
        canonical.push(component);
    }
    Ok(canonical)
}

#[hotpath::measure(label = "usecases.lsp.document.open")]
pub(super) fn open_project_file(
    project_dir: &Dir,
    relative: &Path,
) -> Result<(PathBuf, File), LspRuntimeFailure> {
    validate_relative_path(relative)?;
    let canonical = project_dir.canonicalize(relative).map_err(|error| {
        if error.kind() == ErrorKind::PermissionDenied {
            LspRuntimeFailure::new("document-outside-registered-root")
        } else {
            LspRuntimeFailure::new("document-unavailable")
        }
    })?;
    validate_relative_path(&canonical)
        .map_err(|_| LspRuntimeFailure::new("document-outside-registered-root"))?;
    let file = project_dir
        .open(&canonical)
        .map_err(|_| LspRuntimeFailure::new("document-unavailable"))?;
    Ok((canonical, file))
}
