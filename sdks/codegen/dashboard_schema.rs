//! Export the canonical dashboard schema for contract generation.

use std::error::Error;
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn Error>> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or("usage: dashboard_schema <output-path>")?;
    let schema = tracedecay_dashboard_api::contract_schema::render_dashboard_contract_schema()?;
    std::fs::write(output, schema)?;
    Ok(())
}
