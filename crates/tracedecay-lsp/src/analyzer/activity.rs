#[cfg(test)]
use std::cell::Cell;
use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};

use super::adapters::LspAdapterDefinition;
use super::client::LspDocument;
use super::error::AnalyzerResult;

#[cfg(test)]
thread_local! {
    static PROJECT_ROOT_CANONICALIZATIONS: Cell<usize> = const { Cell::new(0) };
}

pub(crate) fn canonicalize_project_root(project_root: &Path) -> std::io::Result<PathBuf> {
    #[cfg(test)]
    PROJECT_ROOT_CANONICALIZATIONS.set(PROJECT_ROOT_CANONICALIZATIONS.get() + 1);
    project_root.canonicalize()
}

#[cfg(test)]
pub(crate) fn reset_project_root_canonicalization_count() {
    PROJECT_ROOT_CANONICALIZATIONS.set(0);
}

#[cfg(test)]
pub(crate) fn project_root_canonicalization_count() -> usize {
    PROJECT_ROOT_CANONICALIZATIONS.get()
}

pub fn active_languages_for_files(
    project_root: &Path,
    adapters: &[LspAdapterDefinition],
    files: &[String],
) -> BTreeSet<String> {
    let Ok(project_root) = canonicalize_project_root(project_root) else {
        return BTreeSet::new();
    };
    active_languages_for_files_from_canonical_root(&project_root, adapters, files)
}

fn active_languages_for_files_from_canonical_root(
    project_root: &Path,
    adapters: &[LspAdapterDefinition],
    files: &[String],
) -> BTreeSet<String> {
    adapters
        .iter()
        .filter(|adapter| {
            adapter_has_project_documents_from_canonical_root(project_root, adapter, files)
        })
        .map(|adapter| adapter.language.clone())
        .collect()
}

pub fn adapter_has_project_documents(
    project_root: &Path,
    adapter: &LspAdapterDefinition,
    files: &[String],
) -> bool {
    let Ok(project_root) = canonicalize_project_root(project_root) else {
        return false;
    };
    adapter_has_project_documents_from_canonical_root(&project_root, adapter, files)
}

fn adapter_has_project_documents_from_canonical_root(
    project_root: &Path,
    adapter: &LspAdapterDefinition,
    files: &[String],
) -> bool {
    files.iter().any(|file| {
        matches_adapter_extension(adapter, file)
            && adapter_workspace_root_from_canonical_root(project_root, adapter, file).is_some()
    })
}

pub async fn documents_for_adapter(
    project_root: &Path,
    adapter: &LspAdapterDefinition,
    files: Vec<String>,
) -> AnalyzerResult<Vec<LspDocument>> {
    let Ok(project_root) = canonicalize_project_root(project_root) else {
        return Ok(Vec::new());
    };
    let mut documents = Vec::new();
    for file in files {
        if !matches_adapter_extension(adapter, &file) {
            continue;
        }
        if adapter_workspace_root_from_canonical_root(&project_root, adapter, &file).is_none() {
            continue;
        }
        let Some(path) = scoped_project_file_from_canonical_root(&project_root, &file) else {
            continue;
        };
        let Ok(text) = tokio::fs::read_to_string(&path).await else {
            continue;
        };
        documents.push(LspDocument {
            language: adapter.language.clone(),
            language_id: language_id_for_file(adapter, &file),
            relative_path: file,
            text,
        });
    }
    Ok(documents)
}

fn language_id_for_file(adapter: &LspAdapterDefinition, file: &str) -> String {
    let extension = Path::new(file)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default();
    match (adapter.language.as_str(), extension) {
        ("typescript", "tsx") => "typescriptreact".to_string(),
        ("javascript", "jsx") => "javascriptreact".to_string(),
        _ => adapter.language_id.clone(),
    }
}

fn matches_adapter_extension(adapter: &LspAdapterDefinition, file: &str) -> bool {
    Path::new(file)
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            adapter
                .extensions
                .iter()
                .any(|candidate| candidate == extension)
        })
}

pub fn adapter_workspace_root(
    project_root: &Path,
    adapter: &LspAdapterDefinition,
    file: &str,
) -> Option<PathBuf> {
    let project_root = canonicalize_project_root(project_root).ok()?;
    adapter_workspace_root_from_canonical_root(&project_root, adapter, file)
}

pub(crate) fn adapter_workspace_root_from_canonical_root(
    project_root: &Path,
    adapter: &LspAdapterDefinition,
    file: &str,
) -> Option<PathBuf> {
    let path = scoped_project_file_from_canonical_root(project_root, file)?;
    if adapter.root_markers.is_empty() {
        return Some(project_root.to_path_buf());
    }
    let root_markers = adapter
        .root_markers
        .iter()
        .map(|marker| scoped_relative_path(marker))
        .collect::<Option<Vec<_>>>()?;
    let mut current = path.parent();
    while let Some(dir) = current {
        if root_markers.iter().any(|marker| dir.join(marker).exists()) {
            return Some(dir.to_path_buf());
        }
        if dir == project_root {
            break;
        }
        current = dir.parent();
    }
    None
}

fn scoped_project_file_from_canonical_root(project_root: &Path, file: &str) -> Option<PathBuf> {
    let relative = scoped_relative_path(file)?;
    let path = project_root.join(relative);
    match path.canonicalize() {
        Ok(path) => path.starts_with(project_root).then_some(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut existing = path.parent()?;
            while !existing.exists() {
                existing = existing.parent()?;
            }
            existing
                .canonicalize()
                .ok()?
                .starts_with(project_root)
                .then_some(path)
        }
        Err(_) => None,
    }
}

fn scoped_relative_path(path: &str) -> Option<&Path> {
    let path = Path::new(path);
    (!path.as_os_str().is_empty()
        && path
            .components()
            .all(|component| matches!(component, Component::CurDir | Component::Normal(_))))
    .then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyzer::adapters::DiagnosticMode;
    #[cfg(unix)]
    use std::os::unix::fs::symlink;

    #[tokio::test]
    async fn documents_for_adapter_requires_a_matching_root_marker()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let project_root = temp.path();
        let source_path = project_root.join("src/lib.fake");
        let source_parent = source_path.parent().ok_or("source path has no parent")?;
        tokio::fs::create_dir_all(source_parent).await?;
        tokio::fs::write(&source_path, "fake source").await?;
        let adapter = fake_adapter("fake-root");

        let documents =
            documents_for_adapter(project_root, &adapter, vec!["src/lib.fake".to_string()]).await?;

        assert!(
            documents.is_empty(),
            "adapter without a root marker should not open project documents"
        );
        Ok(())
    }

    #[tokio::test]
    async fn documents_for_adapter_accepts_files_under_a_matching_root_marker()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let project_root = temp.path();
        let package_root = project_root.join("package");
        let source_path = package_root.join("src/lib.fake");
        let source_parent = source_path.parent().ok_or("source path has no parent")?;
        tokio::fs::create_dir_all(source_parent).await?;
        tokio::fs::write(package_root.join("fake-root"), "").await?;
        tokio::fs::write(&source_path, "fake source").await?;
        let adapter = fake_adapter("fake-root");

        let documents = documents_for_adapter(
            project_root,
            &adapter,
            vec!["package/src/lib.fake".to_string()],
        )
        .await?;

        assert_eq!(documents.len(), 1);
        assert_eq!(documents[0].relative_path, "package/src/lib.fake");
        Ok(())
    }

    #[tokio::test]
    async fn documents_for_adapter_accepts_directory_root_markers()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        let temp = tempfile::tempdir()?;
        let project_root = temp.path();
        let package_root = project_root.join("package");
        let source_path = package_root.join("src/lib.fake");
        let source_parent = source_path.parent().ok_or("source path has no parent")?;
        tokio::fs::create_dir_all(source_parent).await?;
        tokio::fs::create_dir(package_root.join(".fake-root")).await?;
        tokio::fs::write(&source_path, "fake source").await?;
        let adapter = fake_adapter(".fake-root");

        let documents = documents_for_adapter(
            project_root,
            &adapter,
            vec!["package/src/lib.fake".to_string()],
        )
        .await?;

        assert_eq!(documents.len(), 1);
        assert_eq!(
            adapter_workspace_root(project_root, &adapter, "package/src/lib.fake"),
            Some(package_root)
        );
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn activity_boundaries_normalize_linked_root_once_and_reject_symlink_escape()
    -> std::result::Result<(), Box<dyn std::error::Error>> {
        PROJECT_ROOT_CANONICALIZATIONS.set(0);
        let temp = tempfile::tempdir()?;
        let project = temp.path().join("project");
        let linked_root = temp.path().join("linked-project");
        let outside = temp.path().join("outside.fake");
        tokio::fs::create_dir_all(project.join("src")).await?;
        tokio::fs::write(project.join("src/one.fake"), "one").await?;
        tokio::fs::write(project.join("src/two.fake"), "two").await?;
        tokio::fs::write(&outside, "outside").await?;
        symlink(&project, &linked_root)?;
        symlink(&outside, project.join("src/escape.fake"))?;
        let mut adapter = fake_adapter("");
        adapter.root_markers.clear();
        let mut second_adapter = adapter.clone();
        second_adapter.language = "second-fake".to_string();
        let adapters = [adapter.clone(), second_adapter];
        let traversal = "../outside.fake".to_string();
        let absolute_outside = outside.to_string_lossy().into_owned();

        let documents = documents_for_adapter(
            &linked_root,
            &adapter,
            vec![
                "src/one.fake".to_string(),
                "src/escape.fake".to_string(),
                traversal.clone(),
                absolute_outside.clone(),
                "src/two.fake".to_string(),
            ],
        )
        .await?;
        let document_observation = (
            documents
                .iter()
                .map(|document| document.relative_path.as_str())
                .collect::<Vec<_>>(),
            PROJECT_ROOT_CANONICALIZATIONS.get(),
        );
        let files = vec![
            "src/escape.fake".to_string(),
            traversal.clone(),
            absolute_outside.clone(),
            "src/one.fake".to_string(),
        ];
        PROJECT_ROOT_CANONICALIZATIONS.set(0);
        let adapter_observation = (
            adapter_has_project_documents(&linked_root, &adapter, &files),
            PROJECT_ROOT_CANONICALIZATIONS.get(),
        );
        PROJECT_ROOT_CANONICALIZATIONS.set(0);
        let language_observation = (
            active_languages_for_files(&linked_root, &adapters, &files),
            PROJECT_ROOT_CANONICALIZATIONS.get(),
        );
        let escaped_files = vec!["src/escape.fake".to_string(), traversal, absolute_outside];
        PROJECT_ROOT_CANONICALIZATIONS.set(0);
        let escaped_adapter_observation = (
            adapter_has_project_documents(&linked_root, &adapter, &escaped_files),
            PROJECT_ROOT_CANONICALIZATIONS.get(),
        );
        PROJECT_ROOT_CANONICALIZATIONS.set(0);
        let escaped_language_observation = (
            active_languages_for_files(&linked_root, &adapters, &escaped_files),
            PROJECT_ROOT_CANONICALIZATIONS.get(),
        );

        assert_eq!(
            (
                document_observation,
                adapter_observation,
                language_observation,
                escaped_adapter_observation,
                escaped_language_observation,
            ),
            (
                (vec!["src/one.fake", "src/two.fake"], 1),
                (true, 1),
                (
                    BTreeSet::from(["fake".to_string(), "second-fake".to_string()]),
                    1,
                ),
                (false, 1),
                (BTreeSet::new(), 1),
            ),
            "every public activity boundary must normalize one linked root once and reject files resolving outside it"
        );
        Ok(())
    }

    fn fake_adapter(root_marker: &str) -> LspAdapterDefinition {
        LspAdapterDefinition {
            language: "fake".to_string(),
            language_id: "fake".to_string(),
            command: "fake-ls".to_string(),
            args: Vec::new(),
            extensions: vec!["fake".to_string()],
            root_markers: vec![root_marker.to_string()],
            install_options: Vec::new(),
            diagnostics: DiagnosticMode::Push,
        }
    }
}
