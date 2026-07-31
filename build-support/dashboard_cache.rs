use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::Path;

const DASHBOARD_SOURCE_ROOTS: &[&str] = &["dashboard/src", "dashboard/codegen/schemas"];
const DASHBOARD_SOURCE_FILES: &[&str] = &[
    "dashboard/package.json",
    "dashboard/package-lock.json",
    "dashboard/rsbuild.config.ts",
    "dashboard/tsconfig.json",
];

fn collect_files_relative(root: &Path) -> Vec<String> {
    fn walk(base: &Path, directory: &Path, files: &mut Vec<String>) {
        let Ok(entries) = fs::read_dir(directory) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(base, &path, files);
            } else if path.is_file()
                && let Ok(relative) = path.strip_prefix(base)
            {
                files.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }

    let mut files = Vec::new();
    walk(root, root, &mut files);
    files.sort();
    files
}

pub fn source_inputs() -> impl Iterator<Item = &'static str> {
    DASHBOARD_SOURCE_ROOTS
        .iter()
        .chain(DASHBOARD_SOURCE_FILES)
        .copied()
}

pub fn source_stamp(repository_root: &Path) -> String {
    let mut hasher = DefaultHasher::new();
    for input in source_inputs() {
        let input_path = repository_root.join(input);
        if input_path.is_dir() {
            for relative in collect_files_relative(&input_path) {
                relative.hash(&mut hasher);
                if let Ok(bytes) = fs::read(input_path.join(&relative)) {
                    bytes.hash(&mut hasher);
                }
            }
        } else {
            input.hash(&mut hasher);
            if let Ok(bytes) = fs::read(input_path) {
                bytes.hash(&mut hasher);
            }
        }
    }
    format!("{:016x}", hasher.finish())
}

pub fn dist_is_fresh(repository_root: &Path, expected_stamp: &str) -> bool {
    let app_dist = repository_root.join("dashboard/app-dist");
    fs::read_to_string(app_dist.join(".source-stamp"))
        .is_ok_and(|current| current.trim() == expected_stamp)
        && app_dist.join("index.html").is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn seed_dashboard(root: &Path) {
        for directory in [
            "dashboard/src",
            "dashboard/codegen/schemas",
            "dashboard/app-dist",
        ] {
            fs::create_dir_all(root.join(directory)).expect("create dashboard fixture directory");
        }
        for (path, contents) in [
            ("dashboard/src/app.tsx", "export const app = 'current';"),
            ("dashboard/codegen/schemas/api.json", "{}"),
            ("dashboard/package.json", "{}"),
            ("dashboard/package-lock.json", "{}"),
            ("dashboard/rsbuild.config.ts", "export default {};"),
            ("dashboard/tsconfig.json", "{}"),
            ("dashboard/app-dist/index.html", "<main>current</main>"),
        ] {
            fs::write(root.join(path), contents).expect("write dashboard fixture");
        }
    }

    #[test]
    fn changed_input_invalidates_bundle_and_missing_entrypoint_stays_stale() {
        let fixture = TempDir::new().expect("dashboard cache fixture");
        seed_dashboard(fixture.path());
        let initial_stamp = source_stamp(fixture.path());
        fs::write(
            fixture.path().join("dashboard/app-dist/.source-stamp"),
            &initial_stamp,
        )
        .expect("write source stamp");
        assert!(dist_is_fresh(fixture.path(), &initial_stamp));

        fs::write(
            fixture.path().join("dashboard/src/app.tsx"),
            "export const app = 'changed';",
        )
        .expect("change dashboard source");
        let changed_stamp = source_stamp(fixture.path());
        assert_ne!(changed_stamp, initial_stamp);
        assert!(
            !dist_is_fresh(fixture.path(), &changed_stamp),
            "an input change must never reuse the previously stamped bundle"
        );

        fs::write(
            fixture.path().join("dashboard/app-dist/.source-stamp"),
            &changed_stamp,
        )
        .expect("write rebuilt stamp");
        assert!(dist_is_fresh(fixture.path(), &changed_stamp));
        fs::remove_file(fixture.path().join("dashboard/app-dist/index.html"))
            .expect("remove bundle entrypoint");
        assert!(
            !dist_is_fresh(fixture.path(), &changed_stamp),
            "a matching stamp cannot make a missing bundle fresh"
        );
    }
}
