fn main() {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR")
        .expect("rules_rust must provide CARGO_MANIFEST_DIR");
    assert_eq!(manifest_dir, "scripts");

    let test_tmpdir = std::env::var("TEST_TMPDIR").expect("Bazel must provide TEST_TMPDIR");
    assert_eq!(
        std::env::var("TRACEDECAY_DATA_DIR").as_deref(),
        Ok(format!("{test_tmpdir}/.tracedecay").as_str()),
    );
    assert_eq!(
        std::env::var("TRACEDECAY_DISABLE_GLOBAL_DB").as_deref(),
        Ok("1"),
    );

    let runfile = std::env::var("TRACEDECAY_RUNFILE_PROBE")
        .expect("the launcher must resolve runfiles_env");
    assert_eq!(
        std::fs::read_to_string(runfile).as_deref(),
        Ok("runfile probe\n"),
    );
}
