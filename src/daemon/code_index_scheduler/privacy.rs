use tracedecay_domain::{
    ComponentVersion, LanguageId, SanitizationReceiptId, SanitizerDispositionV1, SensitivityLevelV1,
};

use crate::privacy::{
    CODE_SOURCE_SANITIZER_VERSION_V1, CodeSourceSanitizationV1, CodeSourceShapeV1,
    sanitize_code_source_bytes, verify_sanitized_json_payload,
};

use super::CodeIndexSchedulerErrorV1;

/// The sanitizer shape a captured file's registry-declared language implies.
///
/// Only declared structured data formats get whole-document field semantics
/// (and the fail-closed quarantine of an ambiguous parse). Everything else in
/// the language registry is code or prose and takes the bounded raw
/// credential scan: sniffing the shape out of the bytes misread markdown
/// frontmatter and shell assignments as malformed structured documents and
/// withheld hundreds of ordinary sources from indexing.
pub(super) fn declared_code_source_shape(language: &LanguageId) -> CodeSourceShapeV1 {
    match language.as_str() {
        "json" | "toml" | "yaml" => CodeSourceShapeV1::StructuredData,
        _ => CodeSourceShapeV1::CodeOrProse,
    }
}

pub(super) fn sanitize_code_file(
    language: &LanguageId,
    raw: &[u8],
) -> Result<(Vec<u8>, SensitivityLevelV1, SanitizationReceiptId), CodeIndexSchedulerErrorV1> {
    let sanitized: CodeSourceSanitizationV1 =
        sanitize_code_source_bytes(raw, declared_code_source_shape(language))
            .map_err(|error| CodeIndexSchedulerErrorV1::Privacy(error.to_string()))?;
    let sensitivity = match sanitized.receipt().disposition() {
        SanitizerDispositionV1::Accepted => SensitivityLevelV1::Public,
        SanitizerDispositionV1::Redacted => SensitivityLevelV1::Redacted,
        SanitizerDispositionV1::Rejected | SanitizerDispositionV1::Quarantined => {
            return Err(CodeIndexSchedulerErrorV1::Privacy(
                "durable code source carried a non-durable sanitizer disposition".to_owned(),
            ));
        }
    };
    let receipt_id = sanitized.receipt().receipt().receipt_id().clone();
    let (sanitized_bytes, receipt) = sanitized.into_parts();
    let sanitized_text = String::from_utf8(sanitized_bytes).map_err(|_| {
        CodeIndexSchedulerErrorV1::Privacy(
            "code sanitizer produced non-UTF-8 durable output".to_owned(),
        )
    })?;
    let payload = serde_json::Value::String(sanitized_text.clone());
    let revision = ComponentVersion::new(CODE_SOURCE_SANITIZER_VERSION_V1)
        .map_err(|error| CodeIndexSchedulerErrorV1::Privacy(error.to_string()))?;
    verify_sanitized_json_payload(&payload, &receipt, &revision)
        .map_err(|error| CodeIndexSchedulerErrorV1::Privacy(error.to_string()))?;
    Ok((sanitized_text.into_bytes(), sensitivity, receipt_id))
}

#[cfg(test)]
mod tests {
    use super::sanitize_code_file;
    use tracedecay_domain::{LanguageId, SensitivityLevelV1};

    fn language(id: &str) -> LanguageId {
        LanguageId::new(id).expect("valid language id")
    }

    #[test]
    fn code_file_boundary_returns_only_receipt_verified_sanitized_bytes() {
        let secret = ["sk", "-test-", "1234567890abcdef"].concat();
        let raw = format!("const TOKEN: &str = \"{secret}\";\n");

        let (sanitized, sensitivity, _) =
            sanitize_code_file(&language("rust"), raw.as_bytes()).expect("sanitize code file");

        assert!(!String::from_utf8_lossy(&sanitized).contains(&secret));
        assert_eq!(sensitivity, SensitivityLevelV1::Redacted);
    }

    /// Markdown with YAML frontmatter and shell scripts with variable
    /// assignments are code/prose, not malformed structured documents. They
    /// must sanitize cleanly instead of being withheld with a receipt
    /// construction error.
    #[test]
    fn markdown_frontmatter_and_shell_scripts_sanitize_cleanly() {
        let skill = "---\nname: automation-run-review\ndescription: \"Review runs.\"\n---\n\n\
                     # Automation run review\n\nReview automation *without* mutating it:\n";
        let (sanitized, sensitivity, _) =
            sanitize_code_file(&language("markdown"), skill.as_bytes())
                .expect("markdown with frontmatter sanitizes");
        assert_eq!(sanitized, skill.as_bytes());
        assert_eq!(sensitivity, SensitivityLevelV1::Public);

        let script = "#!/usr/bin/env bash\nset -euo pipefail\nTHRESHOLD=5\n\
                      if [ -z \"$1\" ]; then\n  echo \"usage: scan <dir>\"\nfi\n";
        let (sanitized, sensitivity, _) = sanitize_code_file(&language("bash"), script.as_bytes())
            .expect("shell script sanitizes");
        assert_eq!(sanitized, script.as_bytes());
        assert_eq!(sensitivity, SensitivityLevelV1::Public);
    }

    /// The raw credential scan still applies to code/prose: a secret embedded
    /// in a markdown document is redacted, not passed through.
    #[test]
    fn markdown_credentials_are_still_redacted() {
        let secret = ["sk", "-test-", "1234567890abcdef"].concat();
        let raw = format!("# Setup\n\nUse the token `{secret}` for local runs.\n");

        let (sanitized, sensitivity, _) = sanitize_code_file(&language("markdown"), raw.as_bytes())
            .expect("markdown with a credential sanitizes");

        assert!(!String::from_utf8_lossy(&sanitized).contains(&secret));
        assert_eq!(sensitivity, SensitivityLevelV1::Redacted);
    }

    /// A declared structured data format keeps the fail-closed quarantine,
    /// now under its truthful reason instead of a construction error.
    #[test]
    fn ambiguous_declared_structured_data_is_quarantined_with_a_truthful_reason() {
        let malformed_toml = "[table\nkey = \"value\"\n";

        let error = sanitize_code_file(&language("toml"), malformed_toml.as_bytes())
            .expect_err("ambiguous structured data stays withheld");

        let reason = error.to_string();
        assert!(
            reason.contains("quarantined an ambiguous structured document"),
            "unexpected withholding reason: {reason}"
        );
        assert!(!reason.contains("receipt construction failed"));
    }
}
