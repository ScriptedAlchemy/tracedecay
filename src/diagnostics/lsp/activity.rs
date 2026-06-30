use std::collections::BTreeSet;
use std::path::Path;

use crate::diagnostics::lsp::adapters::LspAdapterDefinition;
use crate::diagnostics::lsp::client::LspDocument;

pub fn active_languages_for_files(
    project_root: &Path,
    adapters: &[LspAdapterDefinition],
    files: &[String],
) -> BTreeSet<String> {
    adapters
        .iter()
        .filter(|adapter| adapter_has_project_documents(project_root, adapter, files))
        .map(|adapter| adapter.language.clone())
        .collect()
}

pub fn adapter_has_project_documents(
    project_root: &Path,
    adapter: &LspAdapterDefinition,
    files: &[String],
) -> bool {
    files.iter().any(|file| {
        matches_adapter_extension(adapter, file)
            && file_has_adapter_root_marker(project_root, adapter, file)
    })
}

pub async fn documents_for_adapter(
    project_root: &Path,
    adapter: &LspAdapterDefinition,
    files: Vec<String>,
) -> crate::errors::Result<Vec<LspDocument>> {
    let mut documents = Vec::new();
    for file in files {
        if !matches_adapter_extension(adapter, &file) {
            continue;
        }
        if !file_has_adapter_root_marker(project_root, adapter, &file) {
            continue;
        }
        let path = project_root.join(&file);
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

fn file_has_adapter_root_marker(
    project_root: &Path,
    adapter: &LspAdapterDefinition,
    file: &str,
) -> bool {
    if adapter.root_markers.is_empty() {
        return true;
    }
    let path = project_root.join(file);
    let mut current = path.parent();
    while let Some(dir) = current {
        if adapter
            .root_markers
            .iter()
            .any(|marker| dir.join(marker).is_file())
        {
            return true;
        }
        if dir == project_root {
            break;
        }
        current = dir.parent();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::lsp::adapters::DiagnosticMode;

    #[tokio::test]
    async fn documents_for_adapter_requires_a_matching_root_marker(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
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
    async fn documents_for_adapter_accepts_files_under_a_matching_root_marker(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
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
