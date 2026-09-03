use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

#[cfg(feature = "hotpath-alloc")]
#[global_allocator]
static HOTPATH_ALLOCATOR: hotpath::CountingAllocator = hotpath::CountingAllocator::new();

struct IsolatedHotpathEnvironment {
    saved: Vec<(OsString, Option<OsString>)>,
}

impl IsolatedHotpathEnvironment {
    fn install() -> Self {
        let mut keys = std::env::vars_os()
            .map(|(key, _)| key)
            .filter(|key| {
                key.to_string_lossy().starts_with("HOTPATH_OUTPUT_")
                    || key == OsStr::new("HOTPATH_REPORT")
                    || key == OsStr::new("HOTPATH_REPORT_LABEL")
            })
            .collect::<BTreeSet<_>>();
        keys.insert(OsString::from("HOTPATH_METRICS_SERVER_OFF"));

        let saved = keys
            .iter()
            .map(|key| (key.clone(), std::env::var_os(key)))
            .collect();

        // SAFETY: every coverage binary contains exactly one test for the
        // active feature configuration and installs this environment before
        // the workload can start threads.
        unsafe {
            for key in keys {
                std::env::remove_var(key);
            }
            std::env::set_var("HOTPATH_METRICS_SERVER_OFF", "1");
        }

        Self { saved }
    }
}

impl Drop for IsolatedHotpathEnvironment {
    fn drop(&mut self) {
        // SAFETY: the single test has dropped its Hotpath guard and completed
        // the workload before this environment is restored.
        unsafe {
            for (key, value) in &self.saved {
                match value {
                    Some(value) => std::env::set_var(key, value),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

fn report_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!(
        "{name}-{}-hotpath-coverage.json",
        std::process::id()
    ))
}

fn collect_exact_labels(value: &serde_json::Value, labels: &mut BTreeSet<String>) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, child) in map {
                if matches!(key.as_str(), "name" | "label")
                    && let Some(label) = child.as_str()
                {
                    labels.insert(label.to_owned());
                }
                collect_exact_labels(child, labels);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_exact_labels(child, labels);
            }
        }
        _ => {}
    }
}

pub fn assert_hotpath_report(
    name: &'static str,
    report_sections: &str,
    expected_labels: &[&str],
    workload: impl FnOnce(),
) {
    assert!(
        !expected_labels.is_empty(),
        "coverage journey must declare at least one production label"
    );

    let _environment = IsolatedHotpathEnvironment::install();
    let path = report_path(name);
    let _ = std::fs::remove_file(&path);

    let guard = hotpath::HotpathGuardBuilder::new(name)
        .sections_exclude(vec![hotpath::Section::FunctionsCpu])
        .format(hotpath::Format::Json)
        .output_path(&path)
        .report(report_sections)
        .functions_limit(2_048)
        .build();
    workload();
    drop(guard);

    let report = std::fs::read_to_string(&path).expect("read Hotpath JSON report");
    let parsed: serde_json::Value =
        serde_json::from_str(&report).expect("Hotpath report must be valid JSON");
    assert!(
        parsed.is_object(),
        "Hotpath report must be a JSON object: {parsed}"
    );

    let mut labels = BTreeSet::new();
    collect_exact_labels(&parsed, &mut labels);
    assert!(
        !labels.is_empty(),
        "Hotpath report emitted no labels: {report}"
    );
    for expected in expected_labels {
        assert!(
            labels.contains(*expected),
            "Hotpath report is missing exact label {expected:?}; labels: {labels:?}"
        );
    }
}
