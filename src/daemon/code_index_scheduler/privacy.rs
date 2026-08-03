use tracedecay_domain::{
    ComponentVersion, SanitizationReceiptId, SanitizerDispositionV1, SensitivityLevelV1,
};

use crate::privacy::{
    CODE_SOURCE_SANITIZER_VERSION_V1, CodeSourceSanitizationV1, sanitize_code_source_bytes,
    verify_sanitized_json_payload,
};

use super::CodeIndexSchedulerErrorV1;

pub(super) fn sanitize_code_file(
    raw: &[u8],
) -> Result<(Vec<u8>, SensitivityLevelV1, SanitizationReceiptId), CodeIndexSchedulerErrorV1> {
    let sanitized: CodeSourceSanitizationV1 = sanitize_code_source_bytes(raw)
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
    use tracedecay_domain::SensitivityLevelV1;

    #[test]
    fn code_file_boundary_returns_only_receipt_verified_sanitized_bytes() {
        let secret = ["sk", "-test-", "1234567890abcdef"].concat();
        let raw = format!("const TOKEN: &str = \"{secret}\";\n");

        let (sanitized, sensitivity, _) =
            sanitize_code_file(raw.as_bytes()).expect("sanitize code file");

        assert!(!String::from_utf8_lossy(&sanitized).contains(&secret));
        assert_eq!(sensitivity, SensitivityLevelV1::Redacted);
    }
}
