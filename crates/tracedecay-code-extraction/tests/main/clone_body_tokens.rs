#[cfg(feature = "lang-clojure")]
use tracedecay_code_extraction::ClojureExtractor;
#[cfg(feature = "lang-perl")]
use tracedecay_code_extraction::PerlExtractor;
use tracedecay_code_extraction::{
    CloneBodyEligibilityV1, CloneBodyTokenizationIssueV1, CloneBodyTokenizationStatusV1,
    ConservativeCloneTokenV1, LanguageExtractor, PythonExtractor, RustExtractor,
    TypeScriptExtractor,
};
use tracedecay_domain::NodeKind;

fn tokens(
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
    assert_eq!(
        artifact.clone_bodies.len(),
        1,
        "{:?}",
        artifact.result.nodes
    );
    artifact.clone_bodies[0].conservative_tokens.clone()
}

#[test]
fn rust_conservative_tokens_ignore_formatting_and_comments_but_preserve_behavior() {
    let baseline = r#"
fn publish(input: &str) -> bool {
    let parsed = parse(input);
    validate(parsed, "read")
}
"#;
    let formatting_and_comments = r#"
fn publish(input: &str) -> bool
{
    // parser-owned comments are trivia
    let parsed=parse(input); /* so is this */
    validate(
        parsed,
        "read",
    )
}
"#;

    let expected = tokens(&RustExtractor, "src/lib.rs", baseline);
    assert_eq!(
        expected,
        tokens(&RustExtractor, "src/lib.rs", formatting_and_comments)
    );
    for changed in [
        baseline.replace("\"read\"", "\"write\""),
        baseline.replace("validate(parsed", "skip_validation(parsed"),
        baseline.replace("validate(parsed, \"read\")", "!validate(parsed, \"read\")"),
        baseline.replace(
            "let parsed = parse(input);\n    validate(parsed, \"read\")",
            "validate(input, \"read\");\n    parse(input)",
        ),
        baseline.replace(
            "validate(parsed, \"read\")",
            "if parsed.is_empty() { false } else { validate(parsed, \"read\") }",
        ),
    ] {
        assert_ne!(expected, tokens(&RustExtractor, "src/lib.rs", &changed));
    }
}

#[test]
fn syntax_tokens_keep_comment_markers_inside_literals_and_javascript_asi_boundaries() {
    assert_eq!(
        tokens(
            &TypeScriptExtractor,
            "src/a.ts",
            "function invokeOnce() { invoke(\"value\"); }",
        ),
        tokens(
            &TypeScriptExtractor,
            "src/a.ts",
            "function invokeOnce()\n{\n/* formatting */ invoke(\"value\")\n}",
        )
    );

    let literal = tokens(
        &TypeScriptExtractor,
        "src/a.ts",
        r#"function parseUrl() { return "https://example.test/*literal*/"; }"#,
    );
    let literal_text = literal
        .iter()
        .filter_map(|token| match token {
            ConservativeCloneTokenV1::Syntax { text, .. } => Some(text.as_str()),
            ConservativeCloneTokenV1::StructureStart { .. }
            | ConservativeCloneTokenV1::StructureEnd { .. } => None,
        })
        .collect::<String>();
    assert!(literal_text.contains("https://example.test/*literal*/"));
    let syntax_literals = tokens(
        &TypeScriptExtractor,
        "src/a.ts",
        r#"function display(δ: string) { const pattern = /a\/b/; return `${δ}\n`; }"#,
    );
    let changed_regex = tokens(
        &TypeScriptExtractor,
        "src/a.ts",
        r#"function display(δ: string) { const pattern = /a\/c/; return `${δ}\n`; }"#,
    );
    assert_ne!(syntax_literals, changed_regex);

    assert_ne!(
        tokens(
            &TypeScriptExtractor,
            "src/a.ts",
            "function value() { return object; }",
        ),
        tokens(
            &TypeScriptExtractor,
            "src/a.ts",
            "function value() { return\nobject; }",
        )
    );
}

#[test]
fn rust_macro_trailing_comma_remains_semantic_syntax() {
    assert_ne!(
        tokens(
            &RustExtractor,
            "src/lib.rs",
            "fn choose(value: i32) { choose!(value); }",
        ),
        tokens(
            &RustExtractor,
            "src/lib.rs",
            "fn choose(value: i32) { choose!(value,); }",
        )
    );
}

#[cfg(feature = "lang-perl")]
#[test]
fn perl_comments_are_parser_trivia() {
    assert_eq!(
        tokens(
            &PerlExtractor,
            "src/main.pl",
            "sub work { my $value = 1; # first comment\n return $value; }",
        ),
        tokens(
            &PerlExtractor,
            "src/main.pl",
            "sub work { my $value = 1; # changed comment\n return $value; }",
        )
    );
}

#[cfg(feature = "lang-clojure")]
#[test]
fn callable_without_a_body_field_is_typed_partial() {
    let artifact =
        ClojureExtractor.extract_artifact("src/core.clj", "(defn work [value] (+ value 1))");
    let body = artifact.clone_bodies.first().expect("partial clone body");
    assert_eq!(
        body.tokenization_status,
        CloneBodyTokenizationStatusV1::Partial
    );
    assert!(
        body.tokenization_issues
            .contains(&CloneBodyTokenizationIssueV1::BodyBoundaryUnavailable)
    );
    assert_eq!(
        body.eligibility,
        CloneBodyEligibilityV1::ExcludedIncompleteTokenization
    );
}

#[test]
fn python_significant_indentation_changes_structural_tokens() {
    let inside = r#"
def process(allowed):
    if allowed:
        commit()
        notify()
"#;
    let outside = r#"
def process(allowed):
    if allowed:
        commit()
    notify()
"#;
    assert_ne!(
        tokens(&PythonExtractor, "src/main.py", inside),
        tokens(&PythonExtractor, "src/main.py", outside)
    );
}

#[test]
fn automatic_discovery_minimum_is_thirty_non_trivia_tokens() {
    let artifact = |body: &str| {
        RustExtractor.extract_artifact("src/lib.rs", &format!("fn body() {{ {body} }}"))
    };
    let twenty_nine = artifact("foo(); foo(); foo(); foo(); foo(); foo(); foo()");
    let thirty = artifact("foo(); foo(); foo(); foo(); foo(); foo(); foo();");
    let thirty_one = artifact("foo(); foo(); foo(); foo(); foo(); foo(); !foo();");

    for (artifact, count, eligibility) in [
        (
            twenty_nine,
            29,
            CloneBodyEligibilityV1::ExcludedTooSmall { minimum_tokens: 30 },
        ),
        (thirty, 30, CloneBodyEligibilityV1::Eligible),
        (thirty_one, 31, CloneBodyEligibilityV1::Eligible),
    ] {
        let body = &artifact.clone_bodies[0];
        assert_eq!(body.non_trivia_token_count, count);
        assert_eq!(body.eligibility, eligibility);
    }
}

#[test]
fn clone_bodies_bind_to_method_and_stable_arrow_occurrences() {
    for (artifact, expected_kind, expected_language) in [
        (
            RustExtractor.extract_artifact(
                "src/store.rs",
                "struct Store; impl Store { fn read(&self) { load(); } }",
            ),
            NodeKind::Method,
            "rust",
        ),
        (
            TypeScriptExtractor.extract_artifact("src/store.ts", "const read = () => { load(); };"),
            NodeKind::ArrowFunction,
            "typescript",
        ),
    ] {
        let body = artifact.clone_bodies.first().expect("clone body");
        let callable = artifact
            .result
            .nodes
            .iter()
            .find(|node| node.kind == expected_kind)
            .expect("callable occurrence");
        assert_eq!(body.symbol_occurrence_id, callable.id);
        assert_eq!(body.symbol_kind, expected_kind);
        assert_eq!(body.language, expected_language);
        assert!(!body.body_span.is_empty());
    }
}
