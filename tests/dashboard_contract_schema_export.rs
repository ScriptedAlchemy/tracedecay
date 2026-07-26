use std::path::PathBuf;

const OUTPUT_ENV: &str = "TRACEDECAY_DASHBOARD_CONTRACT_SCHEMA_OUT";

#[test]
#[ignore = "invoked by dashboard contracts:generate/check"]
fn writes_dashboard_contract_schema() {
    let output = std::env::var_os(OUTPUT_ENV)
        .map(PathBuf::from)
        .expect("contract codegen must provide an output path");
    let schema = tracedecay::dashboard::contract_schema::render_dashboard_contract_schema()
        .expect("serialize dashboard contract schema");
    std::fs::write(&output, schema).expect("write dashboard contract schema");
}
