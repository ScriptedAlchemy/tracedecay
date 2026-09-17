use tracedecay_code_extraction::{
    CloneBodyRenameIssueV1, CloneBodyRenameStatusV1, ConservativeCloneTokenV1, GoExtractor,
    LanguageExtractor, PythonExtractor, RENAME_CLONE_NORMALIZATION_REVISION_V1, RustExtractor,
    TypeScriptExtractor,
};

fn rename_tokens(
    extractor: &dyn LanguageExtractor,
    path: &str,
    source: &str,
) -> Vec<ConservativeCloneTokenV1> {
    let artifact = extractor.extract_artifact(path, source);
    assert!(
        artifact.result.errors.is_empty(),
        "{:?}",
        artifact.result.errors
    );
    let body = artifact.clone_bodies.first().expect("clone body");
    assert_eq!(body.rename_status, CloneBodyRenameStatusV1::Complete);
    assert_eq!(
        body.rename_normalization_revision,
        Some(RENAME_CLONE_NORMALIZATION_REVISION_V1)
    );
    assert!(body.rename_issues.is_empty());
    body.complete_rename_tokens()
        .expect("complete rename tokens")
        .to_vec()
}

#[test]
fn reliable_local_renames_share_tokens_in_rust_typescript_and_python() {
    for (extractor, path, left, right) in [
        (
            &RustExtractor as &dyn LanguageExtractor,
            "src/lib.rs",
            "fn copy(input: &str) { let result = parse(input); validate(result, \"read\"); }",
            "fn copy(source: &str) { let parsed = parse(source); validate(parsed, \"read\"); }",
        ),
        (
            &TypeScriptExtractor,
            "src/lib.ts",
            "function copy(input: string) { const result = parse(input); return validate(result, \"read\"); }",
            "function copy(source: string) { const parsed = parse(source); return validate(parsed, \"read\"); }",
        ),
        (
            &PythonExtractor,
            "src/lib.py",
            "def copy(input):\n    result = parse(input)\n    return validate(result, \"read\")\n",
            "def copy(source):\n    parsed = parse(source)\n    return validate(parsed, \"read\")\n",
        ),
    ] {
        assert_eq!(
            rename_tokens(extractor, path, left),
            rename_tokens(extractor, path, right),
            "{path}"
        );
    }
}

#[test]
fn rename_tokens_preserve_callees_literals_properties_and_negation() {
    let baseline = "function copy(input: Item) { const result = parse(input); return validate(result.name, \"read\"); }";
    let expected = rename_tokens(&TypeScriptExtractor, "src/lib.ts", baseline);
    for changed in [
        baseline.replace("validate(", "skipValidation("),
        baseline.replace("\"read\"", "\"write\""),
        baseline.replace(".name", ".email"),
        baseline.replace("return validate", "return !validate"),
    ] {
        assert_ne!(
            expected,
            rename_tokens(&TypeScriptExtractor, "src/lib.ts", &changed)
        );
    }
}

#[test]
fn shadowed_bindings_and_nested_captures_keep_distinct_identities() {
    let left = r#"
function outer(input: number) {
    const value = input;
    const nested = (item: number) => {
        const value = item + 1;
        return value + input;
    };
    return nested(value);
}
"#;
    let right = r#"
function outer(source: number) {
    const outerValue = source;
    const callback = (entry: number) => {
        const innerValue = entry + 1;
        return innerValue + source;
    };
    return callback(outerValue);
}
"#;
    let normalized = rename_tokens(&TypeScriptExtractor, "src/lib.ts", left);
    assert_eq!(
        normalized,
        rename_tokens(&TypeScriptExtractor, "src/lib.ts", right)
    );
    let syntax_text = normalized
        .iter()
        .filter_map(|token| match token {
            ConservativeCloneTokenV1::Syntax { text, .. } => Some(text.as_str()),
            ConservativeCloneTokenV1::StructureStart { .. }
            | ConservativeCloneTokenV1::StructureEnd { .. } => None,
        })
        .collect::<Vec<_>>();
    assert!(syntax_text.contains(&"arg_0"));
    assert!(syntax_text.contains(&"arg_1"));
    assert!(syntax_text.contains(&"local_0"));
    assert!(syntax_text.contains(&"local_2"));
}

#[test]
fn rust_and_python_nested_scopes_preserve_shadowing_and_captures() {
    for (extractor, path, left, right) in [
        (
            &RustExtractor as &dyn LanguageExtractor,
            "src/lib.rs",
            r#"fn outer(input: i32) -> i32 {
                let value = input;
                let nested = |item| {
                    let value = item + 1;
                    value + input
                };
                nested(value)
            }"#,
            r#"fn outer(source: i32) -> i32 {
                let outer_value = source;
                let callback = |entry| {
                    let inner_value = entry + 1;
                    inner_value + source
                };
                callback(outer_value)
            }"#,
        ),
        (
            &PythonExtractor,
            "src/lib.py",
            "def outer(input):\n    value = input\n    def nested(item):\n        value = item + 1\n        return value + input\n    return nested(value)\n",
            "def outer(source):\n    outer_value = source\n    def nested(entry):\n        inner_value = entry + 1\n        return inner_value + source\n    return nested(outer_value)\n",
        ),
    ] {
        assert_eq!(
            rename_tokens(extractor, path, left),
            rename_tokens(extractor, path, right),
            "{path}"
        );
    }
}

#[test]
fn positional_parameters_and_supported_destructuring_keep_binding_identity() {
    assert_ne!(
        rename_tokens(
            &TypeScriptExtractor,
            "src/lib.ts",
            "function pairValues(first: string, second: string) { return pair(first, second); }",
        ),
        rename_tokens(
            &TypeScriptExtractor,
            "src/lib.ts",
            "function pairValues(second: string, first: string) { return pair(first, second); }",
        )
    );

    for (extractor, path, left, right) in [
        (
            &RustExtractor as &dyn LanguageExtractor,
            "src/lib.rs",
            "fn pair_values(input: (i32, i32)) { let (left, right) = input; pair(left, right); }",
            "fn pair_values(source: (i32, i32)) { let (first, second) = source; pair(first, second); }",
        ),
        (
            &TypeScriptExtractor,
            "src/lib.ts",
            "function pairValues(input: [number, number]) { const [left, right] = input; return pair(left, right); }",
            "function pairValues(source: [number, number]) { const [first, second] = source; return pair(first, second); }",
        ),
        (
            &PythonExtractor,
            "src/lib.py",
            "def pair_values(input):\n    left, right = input\n    return pair(left, right)\n",
            "def pair_values(source):\n    first, second = source\n    return pair(first, second)\n",
        ),
    ] {
        assert_eq!(
            rename_tokens(extractor, path, left),
            rename_tokens(extractor, path, right),
            "{path}"
        );
    }
}

#[test]
fn javascript_var_uses_function_scope() {
    assert_eq!(
        rename_tokens(
            &TypeScriptExtractor,
            "src/lib.js",
            "function copy(input) { if (input) { var value = parse(input); } return value; }",
        ),
        rename_tokens(
            &TypeScriptExtractor,
            "src/lib.js",
            "function copy(source) { if (source) { var parsed = parse(source); } return parsed; }",
        )
    );
}

#[test]
fn unsupported_dynamic_bindings_are_partial_and_keep_a_literal_stream() {
    for (extractor, path, source) in [
        (
            &RustExtractor as &dyn LanguageExtractor,
            "src/lib.rs",
            "fn copy(input: &str) { bind_local!(value, input); consume(value); }",
        ),
        (
            &TypeScriptExtractor,
            "src/lib.ts",
            "function copy(input: object) { const { value } = input; consume(value); }",
        ),
        (
            &PythonExtractor,
            "src/lib.py",
            "def copy(input):\n    exec(input)\n    return value\n",
        ),
    ] {
        let artifact = extractor.extract_artifact(path, source);
        let body = artifact.clone_bodies.first().expect("clone body");
        assert_eq!(body.rename_status, CloneBodyRenameStatusV1::Partial);
        assert_eq!(
            body.rename_normalization_revision,
            Some(RENAME_CLONE_NORMALIZATION_REVISION_V1)
        );
        assert!(body.rename_tokens.is_some());
        assert!(body.complete_rename_tokens().is_none());
        assert!(
            body.rename_issues.contains(&if path.ends_with(".py") {
                CloneBodyRenameIssueV1::DynamicBinding
            } else {
                CloneBodyRenameIssueV1::UnsupportedBindingSyntax
            }),
            "{path}: {:?}",
            body.rename_issues
        );
    }

    let artifact = PythonExtractor.extract_artifact(
        "src/lib.py",
        "def copy():\n    import package as local_package\n    return local_package.read()\n",
    );
    let body = artifact.clone_bodies.first().expect("clone body");
    assert_eq!(body.rename_status, CloneBodyRenameStatusV1::Partial);
    assert!(
        body.rename_issues
            .contains(&CloneBodyRenameIssueV1::UnsupportedBindingSyntax)
    );
}

#[test]
fn unsupported_languages_expose_no_rename_stream() {
    let artifact = GoExtractor.extract_artifact(
        "main.go",
        "package main\nfunc copy(input string) { result := parse(input); validate(result) }\n",
    );
    let body = artifact.clone_bodies.first().expect("clone body");
    assert_eq!(body.rename_normalization_revision, None);
    assert_eq!(
        body.rename_status,
        CloneBodyRenameStatusV1::UnsupportedLanguage
    );
    assert!(body.rename_tokens.is_none());
    assert!(body.complete_rename_tokens().is_none());
    assert!(body.rename_issues.is_empty());
}
