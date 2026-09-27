use tracedecay_contracts::retrieval::{
    AdminProjectBenchV1, AdminProjectResultV1, AdminProjectSurfaceRequestV1,
};
use tracedecay_runtime_core::config::ProfileRoot;

#[hotpath::measure(label = "cli.bench.run", future = true)]
pub(crate) async fn handle_bench(
    profile: &ProfileRoot,
    queries: Option<String>,
    json: bool,
    path: Option<String>,
    max_nodes: usize,
) -> tracedecay_domain::errors::Result<()> {
    let resolved =
        super::scope::resolve_project_scope(profile, tracedecay_configuration::resolve_path(path))
            .await?;
    let queries_toml = queries
        .map(std::fs::read_to_string)
        .transpose()
        .map_err(|error| tracedecay_domain::errors::TraceDecayError::Config {
            message: format!("failed to read query file: {error}"),
        })?;
    let AdminProjectResultV1::Bench(AdminProjectBenchV1 { output }) = super::admin_project(
        profile,
        &resolved.project_path,
        AdminProjectSurfaceRequestV1::Bench {
            queries_toml,
            json,
            max_nodes,
        },
    )
    .await?
    else {
        return Err(super::unexpected_admin_project_result());
    };
    print!("{output}");
    Ok(())
}
