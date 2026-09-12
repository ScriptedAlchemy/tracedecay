use super::*;

#[allow(clippy::unwrap_used, clippy::expect_used)]
mod jsonc_tests {
    use super::*;

    #[test]
    fn parse_jsonc_plain_json() {
        let input = r#"{"key": "value", "num": 42}"#;
        let v = parse_jsonc(input);
        assert_eq!(v["key"], "value");
        assert_eq!(v["num"], 42);
    }

    #[test]
    fn parse_jsonc_line_comment() {
        let input = "{\n  // this is a comment\n  \"key\": \"val\"\n}";
        let v = parse_jsonc(input);
        assert_eq!(v["key"], "val");
    }

    #[test]
    fn parse_jsonc_block_comment() {
        let input = "{ /* block comment */ \"key\": \"val\" }";
        let v = parse_jsonc(input);
        assert_eq!(v["key"], "val");
    }

    #[test]
    fn parse_jsonc_trailing_comma_object() {
        let input = r#"{"a": 1, "b": 2,}"#;
        let v = parse_jsonc(input);
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], 2);
    }

    #[test]
    fn parse_jsonc_trailing_comma_array() {
        let input = r#"{"items": [1, 2, 3,]}"#;
        let v = parse_jsonc(input);
        assert_eq!(v["items"][2], 3);
    }

    #[test]
    fn parse_jsonc_combined() {
        let input = "{\n  // comment\n  \"x\": /* inline */ 99,\n}";
        let v = parse_jsonc(input);
        assert_eq!(v["x"], 99);
    }

    #[test]
    fn parse_jsonc_url_in_string_not_stripped() {
        // A URL containing `//` inside a string must NOT be treated as a comment.
        let input = r#"{"url": "https://example.com/path"}"#;
        let v = parse_jsonc(input);
        assert_eq!(v["url"], "https://example.com/path");
    }

    #[test]
    fn parse_jsonc_invalid_falls_back_to_empty() {
        let input = "not valid json at all !!!";
        let v = parse_jsonc(input);
        assert_eq!(v, serde_json::json!({}));
    }

    #[test]
    fn parse_jsonc_empty_string() {
        let v = parse_jsonc("");
        assert_eq!(v, serde_json::json!({}));
    }

    #[test]
    fn parse_jsonc_trailing_comma_with_whitespace() {
        let input = "{\n  \"a\": 1  ,\n}";
        let v = parse_jsonc(input);
        assert_eq!(v["a"], 1);
    }
}

// ---------------------------------------------------------------------------
// Regression tests for safe config backup / load / write
// ---------------------------------------------------------------------------
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod safe_config_tests {
    use super::*;
    use std::fs;

    /// Create a temp directory that is cleaned up on drop.
    fn tmpdir() -> tempfile::TempDir {
        tempfile::tempdir().expect("failed to create temp dir")
    }

    // ----- backup_config_file -----

    #[test]
    fn backup_returns_none_when_file_missing() {
        let dir = tmpdir();
        let path = dir.path().join("nonexistent.json");
        let result = backup_config_file(&path).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn backup_creates_bak_with_identical_content() {
        let dir = tmpdir();
        let path = dir.path().join("config.json");
        let original = r#"{"existing": "data", "nested": {"key": 1}}"#;
        fs::write(&path, original).unwrap();

        let backup = backup_config_file(&path)
            .unwrap()
            .expect("should create backup");
        assert!(backup.exists());
        assert_eq!(fs::read_to_string(&backup).unwrap(), original);
        // Original is untouched
        assert_eq!(fs::read_to_string(&path).unwrap(), original);
    }

    #[test]
    fn backup_staging_file_is_cleaned_up() {
        let dir = tmpdir();
        let path = dir.path().join("config.json");
        fs::write(&path, "{}").unwrap();

        backup_config_file(&path).unwrap();

        let staging = dir.path().join("config.json.bak.new");
        assert!(!staging.exists(), ".bak.new staging file should be removed");
    }

    // ----- load_json_file_strict -----

    #[test]
    fn strict_load_returns_empty_for_missing_file() {
        let dir = tmpdir();
        let path = dir.path().join("nope.json");
        let val = load_json_file_strict(&path).unwrap();
        assert_eq!(val, serde_json::json!({}));
    }

    #[test]
    fn strict_load_returns_empty_for_blank_file() {
        let dir = tmpdir();
        let path = dir.path().join("empty.json");
        fs::write(&path, "   \n  ").unwrap();
        let val = load_json_file_strict(&path).unwrap();
        assert_eq!(val, serde_json::json!({}));
    }

    #[test]
    fn strict_load_parses_valid_json() {
        let dir = tmpdir();
        let path = dir.path().join("valid.json");
        fs::write(&path, r#"{"hello": "world", "n": 42}"#).unwrap();
        let val = load_json_file_strict(&path).unwrap();
        assert_eq!(val["hello"], "world");
        assert_eq!(val["n"], 42);
    }

    #[test]
    fn strict_load_errors_on_invalid_json() {
        let dir = tmpdir();
        let path = dir.path().join("bad.json");
        fs::write(&path, "not json {{{").unwrap();
        let err = load_json_file_strict(&path).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cannot parse"), "error: {msg}");
        assert!(
            msg.contains("bad.json"),
            "error should mention filename: {msg}"
        );
    }

    #[test]
    fn strict_load_errors_on_truncated_json() {
        let dir = tmpdir();
        let path = dir.path().join("trunc.json");
        fs::write(&path, r#"{"key": "value", "incomplete"#).unwrap();
        assert!(load_json_file_strict(&path).is_err());
    }

    // ----- load_jsonc_file_strict -----

    #[test]
    fn strict_jsonc_load_returns_empty_for_missing() {
        let dir = tmpdir();
        let path = dir.path().join("nope.jsonc");
        let val = load_jsonc_file_strict(&path).unwrap();
        assert_eq!(val, serde_json::json!({}));
    }

    #[test]
    fn strict_jsonc_load_parses_valid_jsonc() {
        let dir = tmpdir();
        let path = dir.path().join("settings.json");
        fs::write(
            &path,
            "{\n  // comment\n  \"key\": \"val\",\n  /* block */ \"n\": 1,\n}",
        )
        .unwrap();
        let val = load_jsonc_file_strict(&path).unwrap();
        assert_eq!(val["key"], "val");
        assert_eq!(val["n"], 1);
    }

    #[test]
    fn strict_jsonc_load_errors_on_garbage() {
        let dir = tmpdir();
        let path = dir.path().join("garbage.json");
        fs::write(&path, "totally not json or jsonc !!!").unwrap();
        let err = load_jsonc_file_strict(&path).unwrap_err();
        assert!(err.to_string().contains("cannot parse"));
    }

    // ----- safe_write_json_file -----

    #[test]
    fn safe_write_creates_file_from_scratch() {
        let dir = tmpdir();
        let path = dir.path().join("new.json");
        let value = serde_json::json!({"created": true});
        safe_write_json_file(&path, &value, None).unwrap();

        let written = fs::read_to_string(&path).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&written).unwrap();
        assert_eq!(parsed["created"], true);
    }

    #[test]
    fn safe_write_replaces_existing_file_atomically() {
        let dir = tmpdir();
        let path = dir.path().join("existing.json");
        fs::write(&path, r#"{"old": true}"#).unwrap();

        let value = serde_json::json!({"new": true});
        safe_write_json_file(&path, &value, None).unwrap();

        let parsed: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(parsed["new"], true);
        assert!(parsed.get("old").is_none());
    }

    #[test]
    fn safe_write_cleans_up_new_file_on_success() {
        let dir = tmpdir();
        let path = dir.path().join("config.json");
        safe_write_json_file(&path, &serde_json::json!({}), None).unwrap();

        let new_path = dir.path().join("config.json.new");
        assert!(!new_path.exists(), ".new staging file should be removed");
    }

    #[test]
    fn safe_write_creates_parent_dirs() {
        let dir = tmpdir();
        let path = dir.path().join("deep").join("nested").join("config.json");
        safe_write_json_file(&path, &serde_json::json!({"deep": true}), None).unwrap();
        assert!(path.exists());
    }

    // ----- write_json_file (convenience wrapper) -----

    #[test]
    fn write_json_file_creates_backup_automatically() {
        let dir = tmpdir();
        let path = dir.path().join("auto.json");
        fs::write(&path, r#"{"original": true}"#).unwrap();

        write_json_file(&path, &serde_json::json!({"updated": true})).unwrap();

        // .bak should exist with original content
        let bak = dir.path().join("auto.json.bak");
        assert!(bak.exists());
        let backup_content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&bak).unwrap()).unwrap();
        assert_eq!(backup_content["original"], true);
    }

    // ----- THE KEY REGRESSION TEST -----
    // This is the exact bug the fix addresses: load_json_file silently
    // returned {} on parse failure, and the install wrote {} + tracedecay
    // back, destroying the user's config.

    #[test]
    fn invalid_json_is_never_silently_replaced() {
        let dir = tmpdir();
        let path = dir.path().join("opencode.json");
        // Simulate a file that serde_json can't parse (e.g. has trailing commas
        // that the non-strict loader would silently drop).
        let corrupted =
            r#"{"mcp": {"other_server": {"url": "http://example.com"},}, "theme": "dark",}"#;
        fs::write(&path, corrupted).unwrap();

        // The strict loader must refuse to parse this.
        let err = load_json_file_strict(&path);
        assert!(err.is_err(), "strict loader must reject invalid JSON");

        // The original file must be completely untouched.
        assert_eq!(fs::read_to_string(&path).unwrap(), corrupted);

        // Contrast: the old non-strict loader silently returns {} — this
        // is the exact behavior that destroyed configs.
        let old_style = load_json_file(&path);
        assert_eq!(
            old_style,
            serde_json::json!({}),
            "non-strict loader returns empty"
        );
    }

    #[test]
    fn full_install_cycle_preserves_existing_config() {
        // Simulate the full install cycle: backup → strict load → mutate → safe write.
        // Existing keys must be preserved.
        let dir = tmpdir();
        let path = dir.path().join("config.json");
        let original = serde_json::json!({
            "theme": "dark",
            "mcp": {
                "existing_server": {"url": "http://localhost:8080"}
            },
            "other_setting": [1, 2, 3]
        });
        fs::write(&path, serde_json::to_string_pretty(&original).unwrap()).unwrap();

        // Simulate install
        let backup = backup_config_file(&path).unwrap();
        let mut config = load_json_file_strict(&path).unwrap();
        config["mcp"]["tracedecay"] = serde_json::json!({
            "type": "local",
            "command": ["tracedecay", "serve"]
        });
        safe_write_json_file(&path, &config, backup.as_deref()).unwrap();

        // Verify
        let result: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        // TraceDecay was added
        assert!(result["mcp"]["tracedecay"].is_object());
        // Existing keys survived
        assert_eq!(result["theme"], "dark");
        assert_eq!(
            result["mcp"]["existing_server"]["url"],
            "http://localhost:8080"
        );
        assert_eq!(result["other_setting"], serde_json::json!([1, 2, 3]));

        // Backup exists with original content
        let bak_content: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(backup.unwrap()).unwrap()).unwrap();
        assert!(bak_content.get("tracedecay").is_none());
        assert_eq!(bak_content["theme"], "dark");
    }

    #[test]
    fn full_install_cycle_aborts_on_corrupt_file() {
        // If the existing config is corrupt, the install must fail without
        // touching the file. This is the core regression test.
        let dir = tmpdir();
        let path = dir.path().join("config.json");
        let corrupt_content = "{ this is not valid json at all }}}";
        fs::write(&path, corrupt_content).unwrap();

        // Backup succeeds (it just copies bytes)
        let backup = backup_config_file(&path).unwrap();
        assert!(backup.is_some());

        // Strict load fails
        let err = load_json_file_strict(&path);
        assert!(err.is_err());

        // Original file is byte-for-byte unchanged
        assert_eq!(fs::read_to_string(&path).unwrap(), corrupt_content);
        // Backup also has the same content
        assert_eq!(
            fs::read_to_string(backup.unwrap()).unwrap(),
            corrupt_content
        );
    }

    #[test]
    fn safe_write_output_is_valid_json() {
        // Verify the written file is always parseable JSON (round-trip).
        let dir = tmpdir();
        let path = dir.path().join("roundtrip.json");
        let value = serde_json::json!({
            "unicode": "héllo wörld 🦀",
            "nested": {"deep": {"array": [1, null, true, "str"]}},
            "empty_obj": {},
            "empty_arr": []
        });

        safe_write_json_file(&path, &value, None).unwrap();

        let raw = fs::read_to_string(&path).unwrap();
        let reparsed: serde_json::Value =
            serde_json::from_str(&raw).expect("written file must be valid JSON");
        assert_eq!(reparsed, value);
    }
}

#[allow(clippy::unwrap_used, clippy::expect_used)]
mod path_normalize_tests {
    use super::*;

    /// Report whether `directory`'s filesystem accepts a name that is not
    /// valid UTF-8.
    ///
    /// `cfg(unix)` is a compile gate, not a filesystem capability: APFS
    /// refuses such a name outright with `EILSEQ`, so a macOS run failed at
    /// the fixture instead of exercising the lookup. Probing keeps the
    /// coverage everywhere the bytes are really accepted and makes the skip
    /// visible where they are not.
    #[cfg(unix)]
    fn non_utf8_file_names_supported(directory: &Path) -> bool {
        use std::os::unix::ffi::OsStringExt;

        let probe = directory.join(std::ffi::OsString::from_vec(vec![b'p', 0xff]));
        match std::fs::write(&probe, b"") {
            Ok(()) => {
                let _ = std::fs::remove_file(&probe);
                true
            }
            Err(_) => false,
        }
    }

    #[cfg(unix)]
    #[test]
    fn path_lookup_preserves_non_unicode_parent_components() {
        use std::os::unix::ffi::OsStringExt;

        let dir = tempfile::tempdir().unwrap();
        if !non_utf8_file_names_supported(dir.path()) {
            println!(
                "skipping path_lookup_preserves_non_unicode_parent_components: \
                 this filesystem refuses non-UTF-8 file names"
            );
            return;
        }
        let invalid_parent = dir
            .path()
            .join(std::ffi::OsString::from_vec(vec![b'n', b'o', b'n', 0xff]));
        let executable = invalid_parent.join(tracedecay_bin_name());
        std::fs::create_dir_all(&invalid_parent).unwrap();
        std::fs::write(&executable, "").unwrap();
        let path_var = std::env::join_paths([&invalid_parent]).unwrap();

        assert_eq!(
            which_tracedecay_path_from(None, Some(path_var.as_os_str()), None),
            Some(executable)
        );
        assert!(which_tracedecay_from(None, Some(path_var.as_os_str()), None).is_none());
    }

    #[test]
    fn path_lookup_absolutizes_relative_path_entries() {
        let relative = Path::new("target").join(tracedecay_bin_name());

        let absolute = absolute_executable_path(&relative).expect("absolute path");

        assert!(absolute.is_absolute());
        assert!(absolute.ends_with(&relative));
    }

    #[cfg(windows)]
    #[test]
    fn windows_path_classification_is_case_insensitive() {
        assert!(is_tracedecay_exe(Path::new(r"C:\Tools\TraceDecay.EXE")));
        assert!(is_cargo_target_binary(
            Path::new(r"C:\Work\TARGET\DEBUG\TraceDecay.EXE"),
            None
        ));
        assert!(is_cargo_target_binary(
            Path::new(r"C:\Work\Custom\TraceDecay.EXE"),
            Some(Path::new(r"c:\work\CUSTOM"))
        ));
    }

    #[test]
    fn normalizes_windows_backslashes() {
        assert_eq!(
            normalize_path_separators(r"C:\Users\dev\scoop\shims\tracedecay.exe"),
            "C:/Users/dev/scoop/shims/tracedecay.exe"
        );
    }

    #[test]
    fn leaves_unix_paths_unchanged() {
        assert_eq!(
            normalize_path_separators("/usr/local/bin/tracedecay"),
            "/usr/local/bin/tracedecay"
        );
    }

    #[test]
    fn which_tracedecay_prefers_path_when_current_exe_is_cargo_target_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path_bin = dir.path().join("bin").join(tracedecay_bin_name());
        std::fs::create_dir_all(path_bin.parent().unwrap()).unwrap();
        std::fs::write(&path_bin, "").unwrap();
        let current_exe = dir
            .path()
            .join("checkout/target/debug")
            .join(tracedecay_bin_name());
        let path_var = std::env::join_paths([dir.path().join("bin")]).unwrap();

        let found = which_tracedecay_from(Some(&current_exe), Some(path_var.as_os_str()), None)
            .expect("PATH binary should be preferred over cargo target binary");

        assert_eq!(
            found,
            normalize_path_separators(&path_bin.to_string_lossy())
        );
    }

    #[test]
    fn which_tracedecay_prefers_path_when_current_exe_is_perf_profile_target_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path_bin = dir.path().join("bin").join(tracedecay_bin_name());
        std::fs::create_dir_all(path_bin.parent().unwrap()).unwrap();
        std::fs::write(&path_bin, "").unwrap();
        let current_exe = dir
            .path()
            .join("checkout/target/perf")
            .join(tracedecay_bin_name());
        let path_var = std::env::join_paths([dir.path().join("bin")]).unwrap();

        let found = which_tracedecay_from(Some(&current_exe), Some(path_var.as_os_str()), None)
            .expect("PATH binary should be preferred over a perf-profile cargo target binary");

        assert_eq!(
            found,
            normalize_path_separators(&path_bin.to_string_lossy())
        );
    }

    #[test]
    fn which_tracedecay_prefers_path_when_current_exe_is_custom_cargo_target_binary() {
        let dir = tempfile::tempdir().unwrap();
        let path_bin = dir.path().join("bin").join(tracedecay_bin_name());
        std::fs::create_dir_all(path_bin.parent().unwrap()).unwrap();
        std::fs::write(&path_bin, "").unwrap();
        let cargo_target_dir = dir.path().join("custom-target");
        let current_exe = cargo_target_dir.join("debug").join(tracedecay_bin_name());
        let path_var = std::env::join_paths([dir.path().join("bin")]).unwrap();

        let found = which_tracedecay_from(
            Some(&current_exe),
            Some(path_var.as_os_str()),
            Some(&cargo_target_dir),
        )
        .expect("PATH binary should be preferred over a custom cargo target binary");

        assert_eq!(
            found,
            normalize_path_separators(&path_bin.to_string_lossy())
        );
    }

    #[test]
    fn which_tracedecay_skips_cargo_target_binary_on_path() {
        let dir = tempfile::tempdir().unwrap();
        let target_bin = dir
            .path()
            .join("checkout/target/debug")
            .join(tracedecay_bin_name());
        let stable_bin = dir.path().join(".cargo/bin").join(tracedecay_bin_name());
        std::fs::create_dir_all(target_bin.parent().unwrap()).unwrap();
        std::fs::create_dir_all(stable_bin.parent().unwrap()).unwrap();
        std::fs::write(&target_bin, "").unwrap();
        std::fs::write(&stable_bin, "").unwrap();
        let path_var =
            std::env::join_paths([target_bin.parent().unwrap(), stable_bin.parent().unwrap()])
                .unwrap();

        let found = which_tracedecay_from(None, Some(path_var.as_os_str()), None)
            .expect("stable PATH binary should be found after skipping cargo target binary");

        assert_eq!(
            found,
            normalize_path_separators(&stable_bin.to_string_lossy())
        );
    }

    #[test]
    fn which_tracedecay_keeps_non_target_current_exe() {
        let dir = tempfile::tempdir().unwrap();
        let path_bin = dir.path().join("bin").join(tracedecay_bin_name());
        std::fs::create_dir_all(path_bin.parent().unwrap()).unwrap();
        std::fs::write(&path_bin, "").unwrap();
        let current_exe = dir.path().join(".cargo/bin").join(tracedecay_bin_name());
        let path_var = std::env::join_paths([dir.path().join("bin")]).unwrap();

        let found = which_tracedecay_from(Some(&current_exe), Some(path_var.as_os_str()), None)
            .expect("non-target current exe should be accepted");

        assert_eq!(
            found,
            normalize_path_separators(&current_exe.to_string_lossy())
        );
    }
}

#[allow(clippy::unwrap_used)]
mod local_install_safety_tests {
    use super::*;

    fn start_paused_host_write(
        path: &Path,
        contents: &'static [u8],
    ) -> (
        TestHostConfigWritePauseController,
        std::thread::JoinHandle<std::result::Result<(), String>>,
    ) {
        let pause = pause_next_host_config_write_at_publication(path);
        let path = path.to_path_buf();
        let writer = std::thread::spawn(move || {
            safe_write_bytes_file(&path, contents, None).map_err(|error| error.to_string())
        });
        pause.wait_until_reached();
        (pause, writer)
    }

    #[test]
    fn windows_hook_command_quotes_windows_paths_with_spaces() {
        let command = hook_command_for_platform(
            r"C:\Program Files\tracedecay\tracedecay.exe",
            "hook-test",
            true,
        );

        assert_eq!(
            command,
            r#""C:/Program Files/tracedecay/tracedecay.exe" hook-test"#
        );
    }

    #[test]
    fn posix_hook_command_keeps_single_quote_escaping() {
        let command = hook_command_for_platform("/tmp/tracedecay's/bin", "hook-test", false);

        assert_eq!(command, "'/tmp/tracedecay'\\''s/bin' hook-test");
    }

    #[cfg(unix)]
    #[test]
    fn project_local_safe_path_rejects_symlinked_target() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let outside = dir.path().join("outside.md");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&outside, "outside").unwrap();
        symlink(&outside, project.join("AGENTS.md")).unwrap();

        let err = ensure_project_local_safe_path(&project, &project.join("AGENTS.md")).unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "error should clearly identify the symlink risk: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn project_local_safe_path_rejects_symlinked_parent() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        symlink(&outside, project.join(".codex")).unwrap();

        let err = ensure_project_local_safe_path(&project, &project.join(".codex/config.toml"))
            .unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "error should clearly identify the symlink risk: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn project_local_safe_path_allows_new_file_under_canonicalized_project_alias() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let actual = dir.path().join("actual");
        let alias = dir.path().join("alias");
        let project = actual.join("project");
        std::fs::create_dir_all(&project).unwrap();
        symlink(&actual, &alias).unwrap();

        let alias_project = alias.join("project");
        ensure_project_local_safe_path(&alias_project, &alias_project.join(".codex/config.toml"))
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn project_local_safe_path_reports_symlink_under_canonicalized_project_alias() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let actual = dir.path().join("actual");
        let alias = dir.path().join("alias");
        let project = actual.join("project");
        let outside = dir.path().join("outside.md");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(&outside, "outside").unwrap();
        symlink(&actual, &alias).unwrap();
        symlink(&outside, project.join("AGENTS.md")).unwrap();

        let alias_project = alias.join("project");
        let err = ensure_project_local_safe_path(&alias_project, &alias_project.join("AGENTS.md"))
            .unwrap_err();
        assert!(
            err.to_string().contains("symlink"),
            "error should clearly identify the symlink risk: {err}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn shared_host_writer_refuses_symlinked_config_for_every_integration() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("operator-config.json");
        let config = dir.path().join("host-config.json");
        std::fs::write(&outside, b"operator bytes").unwrap();
        symlink(&outside, &config).unwrap();

        let error = safe_write_bytes_file(&config, b"tracedecay bytes", None).unwrap_err();
        assert!(
            error.to_string().contains("unsafe host metadata path"),
            "shared writer must surface its cross-host symlink refusal: {error}"
        );
        assert_eq!(std::fs::read(&outside).unwrap(), b"operator bytes");
    }

    #[test]
    fn shared_host_writer_serializes_a_competing_canonical_writer() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("AGENTS.md");
        std::fs::write(&config, b"original").unwrap();
        let (pause, first) = start_paused_host_write(&config, b"first");
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        let second_path = config.clone();
        let second = std::thread::spawn(move || {
            let result = safe_write_bytes_file(&second_path, b"second", None)
                .map_err(|error| error.to_string());
            finished_tx.send(()).unwrap();
            result
        });

        assert!(
            finished_rx
                .recv_timeout(std::time::Duration::from_millis(150))
                .is_err(),
            "a competing canonical writer must wait for the path lock"
        );
        pause.resume();
        first.join().unwrap().unwrap();
        second.join().unwrap().unwrap();
        assert_eq!(std::fs::read(&config).unwrap(), b"second");
    }

    #[test]
    fn shared_host_writer_refuses_a_foreign_edit_at_publication_boundary() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("AGENTS.md");
        std::fs::write(&config, b"original").unwrap();
        let pause = pause_next_host_config_write_at_publication(&config);
        let writer_path = config.clone();
        let writer = std::thread::spawn(move || {
            safe_write_bytes_file(&writer_path, b"tracedecay", None)
                .map_err(|error| error.to_string())
        });
        pause.wait_until_reached();

        std::fs::write(&config, b"foreign").unwrap();
        pause.resume();
        let error = writer.join().unwrap().unwrap_err();

        assert!(error.contains("changed since it was read"), "{error}");
        assert_eq!(std::fs::read(&config).unwrap(), b"foreign");
    }

    #[cfg(unix)]
    #[test]
    fn shared_host_writer_retains_both_files_when_published_staging_is_mutated() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("AGENTS.md");
        std::fs::write(&config, b"original").unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
        let publication = pause_next_host_config_write_at_publication(&config);
        let published = pause_next_host_config_write_after_publication(&config);
        let writer_path = config.clone();
        let writer = std::thread::spawn(move || {
            safe_write_bytes_file(&writer_path, b"tracedecay", None)
                .map_err(|error| error.to_string())
        });
        publication.wait_until_reached();

        std::fs::write(&config, b"foreign at boundary").unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o640)).unwrap();
        publication.resume();
        published.wait_until_reached();

        std::fs::write(&config, b"foreign after publication").unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o644)).unwrap();
        published.resume();
        let error = writer.join().unwrap().unwrap_err();

        assert!(error.contains("retained"), "{error}");
        assert_eq!(std::fs::read(&config).unwrap(), b"foreign at boundary");
        assert_eq!(std::fs::metadata(&config).unwrap().mode() & 0o777, 0o640);
        let retained = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(std::result::Result::ok)
            .map(|entry| entry.path())
            .find(|path| {
                path != &config
                    && std::fs::read(path)
                        .is_ok_and(|contents| contents == b"foreign after publication")
            })
            .expect("the ambiguous published staging file must remain recoverable");
        assert_eq!(std::fs::metadata(retained).unwrap().mode() & 0o777, 0o644);
    }

    #[cfg(unix)]
    #[test]
    fn shared_host_writer_refuses_a_symlink_swap_at_publication() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("AGENTS.md");
        let outside = dir.path().join("outside.md");
        std::fs::write(&config, b"original").unwrap();
        std::fs::write(&outside, b"outside").unwrap();
        let (pause, writer) = start_paused_host_write(&config, b"tracedecay");

        std::fs::remove_file(&config).unwrap();
        symlink(&outside, &config).unwrap();
        pause.resume();
        let error = writer.join().unwrap().unwrap_err();

        assert!(error.contains("unsafe host metadata path"), "{error}");
        assert_eq!(std::fs::read(&outside).unwrap(), b"outside");
    }

    #[cfg(unix)]
    #[test]
    fn shared_host_writer_refuses_a_metadata_change_at_publication() {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};

        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("AGENTS.md");
        std::fs::write(&config, b"original").unwrap();
        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o600)).unwrap();
        let (pause, writer) = start_paused_host_write(&config, b"tracedecay");

        std::fs::set_permissions(&config, std::fs::Permissions::from_mode(0o640)).unwrap();
        pause.resume();
        let error = writer.join().unwrap().unwrap_err();

        assert!(error.contains("changed since it was read"), "{error}");
        assert_eq!(std::fs::read(&config).unwrap(), b"original");
        assert_eq!(std::fs::metadata(&config).unwrap().mode() & 0o777, 0o640);
    }

    #[test]
    fn shared_host_writer_refuses_a_missing_file_create_at_publication() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("AGENTS.md");
        let (pause, writer) = start_paused_host_write(&config, b"tracedecay");

        std::fs::write(&config, b"foreign create").unwrap();
        pause.resume();
        let error = writer.join().unwrap().unwrap_err();

        assert!(error.contains("changed since it was read"), "{error}");
        assert_eq!(std::fs::read(&config).unwrap(), b"foreign create");
    }
}
