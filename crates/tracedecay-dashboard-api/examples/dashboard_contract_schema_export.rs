use std::path::PathBuf;

fn main() {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .expect("contract codegen must provide an output path");
    let schema = tracedecay_dashboard_api::contract_schema::render_dashboard_contract_schema()
        .expect("serialize dashboard contract schema");
    std::fs::write(output, schema).expect("write dashboard contract schema");
}
