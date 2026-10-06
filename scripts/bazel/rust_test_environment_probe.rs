fn main() {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("rules_rust must provide CARGO_MANIFEST_DIR");
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

    let target_tmpdir = std::path::PathBuf::from(
        std::env::var("CARGO_TARGET_TMPDIR").expect("the launcher must set CARGO_TARGET_TMPDIR"),
    );
    assert_eq!(
        target_tmpdir,
        std::path::Path::new(&test_tmpdir).join("cargo-target-tmp"),
    );
    assert!(target_tmpdir.is_dir());

    // `CARGO_BIN_EXE_<bin>` names carry `-`, which dash drops from its
    // children's environment unless the launcher hands them over itself.
    for variable in ["TRACEDECAY_RUNFILE_PROBE", "CARGO_BIN_EXE_runfile-probe"] {
        let runfile = std::env::var(variable)
            .unwrap_or_else(|_| panic!("the launcher must resolve runfiles_env {variable}"));
        assert!(
            std::path::Path::new(&runfile).is_absolute(),
            "{variable}={runfile}"
        );
        assert_eq!(
            std::fs::read_to_string(&runfile).ok().as_deref(),
            Some("runfile probe\n"),
            "{variable}={runfile}",
        );
    }
}
