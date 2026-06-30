use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyImportCandidate {
    pub module: String,
    pub symbol: String,
    pub import_file: String,
    pub line: u32,
}

pub fn candidates_from_type_only_import(
    import_text: &str,
    module_path: &str,
    import_file: &str,
    line: u32,
) -> Vec<DependencyImportCandidate> {
    if is_project_relative_import(module_path) || !is_type_only_import(import_text) {
        return Vec::new();
    }
    let Some((_, after_open)) = import_text.split_once('{') else {
        return Vec::new();
    };
    let Some((named_imports, _)) = after_open.split_once('}') else {
        return Vec::new();
    };
    named_imports
        .split(',')
        .filter_map(named_import_exported_name)
        .map(|symbol| DependencyImportCandidate {
            module: module_path.to_string(),
            symbol,
            import_file: import_file.to_string(),
            line,
        })
        .collect()
}

fn is_type_only_import(import_text: &str) -> bool {
    import_text.trim_start().starts_with("import type ")
}

fn is_project_relative_import(module_path: &str) -> bool {
    module_path.starts_with('.') || module_path.starts_with('/')
}

fn named_import_exported_name(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let without_type_prefix = trimmed.strip_prefix("type ").unwrap_or(trimmed);
    let exported = without_type_prefix
        .split_once(" as ")
        .map_or(without_type_prefix, |(name, _)| name)
        .trim();
    if exported.is_empty() {
        return None;
    }
    Some(exported.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn type_only_named_imports_become_dependency_candidates() {
        let candidates = candidates_from_type_only_import(
            "import type { Foo, Bar as Baz } from \"pkg\";",
            "pkg",
            "src/app.ts",
            7,
        );

        assert_eq!(
            candidates,
            vec![
                DependencyImportCandidate {
                    module: "pkg".to_string(),
                    symbol: "Foo".to_string(),
                    import_file: "src/app.ts".to_string(),
                    line: 7,
                },
                DependencyImportCandidate {
                    module: "pkg".to_string(),
                    symbol: "Bar".to_string(),
                    import_file: "src/app.ts".to_string(),
                    line: 7,
                },
            ]
        );
    }

    #[test]
    fn relative_and_value_imports_are_not_dependency_candidates() {
        assert!(candidates_from_type_only_import(
            "import type { Local } from \"./local\";",
            "./local",
            "src/app.ts",
            1,
        )
        .is_empty());
        assert!(candidates_from_type_only_import(
            "import { Foo } from \"pkg\";",
            "pkg",
            "src/app.ts",
            1,
        )
        .is_empty());
    }
}
