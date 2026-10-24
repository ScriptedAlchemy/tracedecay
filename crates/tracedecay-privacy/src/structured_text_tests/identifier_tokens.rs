//! Long identifiers are high-entropy per character but carry no secret: a
//! generated schema's type names, a receipt's snake_case result names, and a
//! digest manifest's path keys must be indexed as written. Credential values
//! and credential-shaped keys in the same documents must still be withheld.

use tracedecay_domain::SanitizerDispositionV1;

use super::{CodeSourceShapeV1, DetectionError, sanitize_code_source_bytes};

fn assert_structured_source_is_preserved(raw: &str) {
    let sanitized = sanitize_code_source_bytes(raw.as_bytes(), CodeSourceShapeV1::StructuredData)
        .expect("identifier-keyed structured source is indexed");

    assert_eq!(
        String::from_utf8_lossy(sanitized.sanitized_bytes()),
        raw,
        "an identifier was redacted"
    );
    assert_eq!(
        sanitized.receipt().disposition(),
        SanitizerDispositionV1::Accepted
    );
}

#[test]
fn generated_json_schema_type_names_are_not_credential_bearing_keys() {
    assert_structured_source_is_preserved(
        r##"{
  "$defs": {
    "RootScopeOutcomeV1_for_Nullable_string": {
      "oneOf": [{ "$ref": "#/$defs/WorkDuplicateAdjudicationAppendOutcomeV1" }]
    },
    "WorkDuplicateAdjudicationAppendOutcomeV1": {
      "oneOf": [
        {
          "properties": {
            "outcome": { "const": "appended", "type": "string" },
            "receipt": { "$ref": "#/$defs/ExecutionGitHubStackCapabilityReadingV1" }
          },
          "required": ["outcome", "receipt"],
          "type": "object"
        }
      ]
    }
  }
}
"##,
    );
}

#[test]
fn benchmark_receipt_result_names_are_not_credential_bearing_keys() {
    assert_structured_source_is_preserved(
        r#"{
  "target_results": {
    "clone_peak_rss_added_at_most_1_gib": null,
    "clone_peak_rss_added_at_most_1_gib_for_39582_symbols": true,
    "clone_work_added_at_most_60_seconds_for_101154_symbols": true
  }
}
"#,
    );
}

#[test]
fn digest_manifest_keyed_by_paths_is_not_credential_bearing() {
    assert_structured_source_is_preserved(
        r#"{
  "files": {
    "benchmark_data/index-bench/clone-envelope-20260915-fingerprint-staging.json": "e6d9f163e2971e2826045645f1cd71eb52b8919ea5156c169373aeeefa0ff4df",
    "dashboard/codegen/schemas/dashboard-contracts.schema.json": "3fe9098b34d5acd0ef72bf0d6376336be6f205be054b029e877547be9f92c0f6"
  }
}
"#,
    );
}

#[test]
fn rust_type_names_are_not_redacted_as_high_entropy_tokens() {
    let raw = "pub enum WorkDuplicateAdjudicationAppendOutcomeV1 {\n    \
               Appended(GitHubReadOnlyCredentialAuthorityOutcomeV1),\n}\n";

    let sanitized = sanitize_code_source_bytes(raw.as_bytes(), CodeSourceShapeV1::CodeOrProse)
        .expect("ordinary Rust sanitizes");

    assert_eq!(String::from_utf8_lossy(sanitized.sanitized_bytes()), raw);
    assert_eq!(
        sanitized.receipt().disposition(),
        SanitizerDispositionV1::Accepted
    );
}

#[test]
fn credential_values_beside_identifier_keys_are_still_redacted() {
    let api_key = ["sk", "-proj-", "Zr4mT8qW2xK9vB3nL7pY5cH1"].concat();
    let password = "hunter2";
    let aws_key = ["AKIA", "4S27TQXBVCZ5MJ6L"].concat();
    let github_token = ["ghp", "_KsY7QwT2mZ4bV9nR6cX1jH8pL3dG5fA0eUwQ"].concat();
    let opaque = "ARiz6HYp7Iq8_Jr9-KsALtBMuCNvDOwEPxFQyGRzHS0ITaJU";
    let raw = format!(
        r#"{{
  "WorkDuplicateAdjudicationAppendOutcomeV1": {{
    "api_key": "{api_key}",
    "password": "{password}",
    "aws_access_key_id": "{aws_key}",
    "remote": "https://{github_token}@github.com/acme/repo.git",
    "note": "{opaque}"
  }}
}}
"#
    );

    let sanitized = sanitize_code_source_bytes(raw.as_bytes(), CodeSourceShapeV1::StructuredData)
        .expect("credential values are redacted in place");
    let text = String::from_utf8_lossy(sanitized.sanitized_bytes());

    for secret in [&api_key, password, &aws_key, &github_token, opaque] {
        assert!(!text.contains(secret), "{secret} survived: {text}");
    }
    assert!(text.contains("WorkDuplicateAdjudicationAppendOutcomeV1"));
    assert_eq!(
        sanitized.receipt().disposition(),
        SanitizerDispositionV1::Redacted
    );
}

#[test]
fn credential_material_in_object_keys_is_still_quarantined() {
    let github_token = ["ghp", "_KsY7QwT2mZ4bV9nR6cX1jH8pL3dG5fA0eUwQ"].concat();
    let opaque = "ARiz6HYp7Iq8_Jr9-KsALtBMuCNvDOwEPxFQyGRzHS0ITaJU";
    let prefixed = ["session_token_", "KsY7QwT2mZ4bV9nR6cX1jH8pL3dG5fA0eUwQ"].concat();

    for key in [github_token.as_str(), opaque, prefixed.as_str()] {
        let raw = format!("{{\"sessions\": {{\"{key}\": {{\"user\": \"ada\"}}}}}}\n");

        let error = sanitize_code_source_bytes(raw.as_bytes(), CodeSourceShapeV1::StructuredData)
            .map(|_| ())
            .expect_err("a credential-shaped key is withheld");

        assert_eq!(error, DetectionError::CredentialKeyQuarantine, "key {key}");
    }
}

/// Words joined into a passphrase are not a name: wherever the token is a
/// value or free text, the entropy check must still withhold it.
const PASSPHRASES: [&str; 7] = [
    "violin-pocket-jungle-quartz-island-778",
    "window_quartz_kitchen_silver_silver_785",
    "staplebuttonforesthammerkitchenwindow169",
    "RocketButtonStapleBridgeYellowOrange90",
    "MySuperSecretPasswordForProduction2024Admin",
    "FigalKuwozJibekPunukBomajJajicSajinKatuw42",
    "fusegunazojitikurihimabavefiwopilode7",
];

#[test]
fn passphrase_shaped_tokens_in_free_text_are_redacted() {
    for passphrase in PASSPHRASES {
        for raw in [
            format!("the deploy phrase is {passphrase} for prod\n"),
            format!("# note\n\n    {passphrase}\n"),
            format!("let s = \"{passphrase}\";\n"),
        ] {
            let sanitized =
                sanitize_code_source_bytes(raw.as_bytes(), CodeSourceShapeV1::CodeOrProse)
                    .expect("free text sanitizes");
            let text = String::from_utf8_lossy(sanitized.sanitized_bytes());

            assert!(!text.contains(passphrase), "{passphrase} survived: {text}");
            assert_eq!(
                sanitized.receipt().disposition(),
                SanitizerDispositionV1::Redacted
            );
        }
    }
}

#[test]
fn passphrase_shaped_tokens_as_json_values_are_redacted() {
    for passphrase in PASSPHRASES {
        let raw = format!(
            "{{\"WorkDuplicateAdjudicationAppendOutcomeV1\": {{\"note\": \"{passphrase}\"}}, \"list\": [\"{passphrase}\"]}}\n"
        );

        let sanitized =
            sanitize_code_source_bytes(raw.as_bytes(), CodeSourceShapeV1::StructuredData)
                .expect("a credential-shaped value is redacted in place");
        let text = String::from_utf8_lossy(sanitized.sanitized_bytes());

        assert!(!text.contains(passphrase), "{passphrase} survived: {text}");
        assert!(text.contains("WorkDuplicateAdjudicationAppendOutcomeV1"));
    }
}

#[test]
fn identifiers_in_rust_declaration_and_path_positions_are_preserved() {
    let raw = "pub enum WorkDuplicateAdjudicationAppendOutcomeV1 {\n    \
               Appended(GitHubReadOnlyCredentialAuthorityOutcomeV1),\n}\n\
               fn adjudicate() -> WorkDuplicateAdjudicationAppendOutcomeV1 {\n    \
               crate::GitHubReadOnlyCredentialAuthorityOutcomeV1::new()\n}\n\
               struct MAXIMUM_WORKFLOW_PAYLOAD_JSON_BYTES_FOR_GRAPH_V2;\n\
               type Alias = Vec<ExecutionGitHubStackCapabilityReadingV1>;\n";

    let sanitized = sanitize_code_source_bytes(raw.as_bytes(), CodeSourceShapeV1::CodeOrProse)
        .expect("ordinary Rust sanitizes");

    assert_eq!(String::from_utf8_lossy(sanitized.sanitized_bytes()), raw);
}
