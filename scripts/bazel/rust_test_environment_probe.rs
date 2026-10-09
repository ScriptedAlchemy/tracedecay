fn main() {
    let manifest_dir =
        std::env::var("CARGO_MANIFEST_DIR").expect("rules_rust must provide CARGO_MANIFEST_DIR");
    let manifest_dir = std::path::PathBuf::from(manifest_dir);
    assert!(manifest_dir.is_absolute());
    assert_eq!(manifest_dir, std::env::current_dir().unwrap());

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

    let home = std::path::PathBuf::from(std::env::var_os("HOME").expect("isolated home"));
    assert_eq!(home, std::path::Path::new(&test_tmpdir).join("home"));
    assert!(home.is_dir());
    assert_eq!(
        std::path::PathBuf::from(std::env::var_os("CARGO_HOME").expect("isolated Cargo home")),
        std::path::Path::new(&test_tmpdir).join("cargo-home"),
    );
    let toolchain = std::env::var("RUSTUP_TOOLCHAIN").expect("pinned test toolchain");
    for variable in ["CARGO", "RUSTC", "RUSTDOC"] {
        let tool = std::path::PathBuf::from(std::env::var_os(variable).expect("fixture compiler"));
        assert!(tool.is_absolute(), "{variable}={}", tool.display());
        let output = std::process::Command::new(&tool)
            .arg("--version")
            .current_dir(&home)
            .output()
            .expect("run fixture compiler outside the workspace");
        assert!(output.status.success(), "{variable}: {:?}", output);
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains(&toolchain)
        );
    }

    // Exercise the production runner's literal executable lookup under a
    // fresh home: no rustup proxy may download a second toolchain here.
    for tool in ["cargo", "rustc", "rustdoc"] {
        let output = std::process::Command::new(tool)
            .arg("--version")
            .env("RUSTUP_HOME", home.join("empty-rustup"))
            .current_dir(&home)
            .output()
            .expect("run declared toolchain through PATH");
        assert!(output.status.success(), "{tool}: {output:?}");
        assert!(
            String::from_utf8(output.stdout)
                .unwrap()
                .contains(&toolchain)
        );
    }
    let fixture = home.join("fixture");
    // Windows test scratch lives beneath the execroot workspace. Exercise
    // ancestor workspace discovery on every host.
    std::fs::write(home.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    std::fs::create_dir_all(fixture.join("src")).unwrap();
    std::fs::write(
        fixture.join("Cargo.toml"),
        "[package]\nname = \"toolchain-probe\"\nversion = \"0.0.0\"\nedition = \"2024\"\n\n[workspace]\n",
    )
    .unwrap();
    std::fs::write(
        fixture.join("src/lib.rs"),
        "/// ```\n/// assert_eq!(toolchain_probe::answer(), 42);\n/// ```\npub fn answer() -> u32 { 42 }\n",
    ).unwrap();
    let output = std::process::Command::new("cargo")
        .args(["test", "--offline"])
        .env("RUSTUP_HOME", home.join("empty-rustup"))
        .env("CARGO_HOME", home.join("empty-cargo"))
        .current_dir(&fixture)
        .output()
        .expect("compile and doctest an isolated fixture");
    assert!(output.status.success(), "fixture compilation: {output:?}");
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("1 passed")
    );
    assert!(!home.join("empty-rustup").exists());

    for variable in ["TRACEDECAY_RUNFILE_PROBE", "CARGO_BIN_EXE_runfile-probe"] {
        let runfile = std::env::var(variable)
            .unwrap_or_else(|_| panic!("the launcher must resolve runfiles_env {}", variable));
        assert!(
            std::path::Path::new(&runfile).is_absolute(),
            "{}={}",
            variable,
            runfile,
        );
        assert_eq!(
            std::fs::read_to_string(&runfile)
                .unwrap_or_else(|error| panic!("{}={}: {}", variable, runfile, error)),
            "runfile probe\n",
        );
    }
}
