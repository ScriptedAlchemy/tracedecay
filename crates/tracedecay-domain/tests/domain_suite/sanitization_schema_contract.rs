use schemars::schema_for;
use serde_json::Value;
use tracedecay_domain::SanitizationReceiptV1;

#[test]
fn sanitization_receipt_schema_preserves_the_closed_nested_authority() {
    let schema = serde_json::to_value(schema_for!(SanitizationReceiptV1))
        .expect("sanitization receipt schema");
    let properties = schema["properties"]
        .as_object()
        .expect("sanitization receipt properties");

    assert_eq!(schema["additionalProperties"], Value::Bool(false));
    assert_eq!(
        properties.keys().map(String::as_str).collect::<Vec<_>>(),
        ["disposition", "payload", "receipt", "sensitivity"]
    );

    let definitions = schema["$defs"]
        .as_object()
        .expect("nested sanitization definitions");
    for (definition, fields) in [
        (
            "SanitizationReceiptRefV1",
            ["receipt_id", "sanitizer_version"].as_slice(),
        ),
        ("PayloadReferenceV1", ["byte_len", "digest"].as_slice()),
    ] {
        let properties = definitions[definition]["properties"]
            .as_object()
            .expect("nested sanitization authority properties");
        assert_eq!(definitions[definition]["additionalProperties"], false);
        assert_eq!(
            properties.keys().map(String::as_str).collect::<Vec<_>>(),
            fields
        );
    }
}
