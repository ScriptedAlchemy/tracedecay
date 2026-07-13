use clap::{Parser, ValueEnum};
use sha2::{Digest, Sha256};
use std::error::Error;
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, ExitCode};
use tracedecay::compatibility_inventory::baseline::load_envelope_path;
use tracedecay::compatibility_inventory::footprint::CheckedFootprintDescriptors;
use tracedecay::compatibility_inventory::model::{
    CompatibilityInventoryEnvelopeV1, InventoryRunMetadata,
};
use tracedecay::compatibility_inventory::render::{
    render_compact_markdown, semantic_snapshot_digest,
};
use tracedecay::compatibility_inventory::{GenerateInventoryOptions, generate_inventory};

const ARCHITECTURE_BOUNDARIES: &str = include_str!("../../architecture-boundaries.toml");

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum OutputFormat {
    #[default]
    Json,
    Markdown,
}

#[derive(Debug, Parser)]
#[command(about = "Generate the deterministic V1 compatibility inventory")]
struct Args {
    #[arg(long, value_enum, default_value_t)]
    format: OutputFormat,

    #[arg(long, value_name = "PATH", conflicts_with = "write")]
    check: Option<PathBuf>,

    #[arg(long, value_name = "PATH", conflicts_with = "check")]
    write: Option<PathBuf>,

    #[arg(long, value_name = "PATH")]
    cargo_metadata: Option<PathBuf>,

    #[arg(long, value_name = "PATH")]
    footprint_descriptors: PathBuf,

    #[arg(long)]
    commit: String,

    #[arg(long)]
    generated_at: String,

    #[arg(long)]
    watermark: String,
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct CliError(String);

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("compatibility-inventory: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: Args) -> Result<(), Box<dyn Error>> {
    let cargo_metadata = match &args.cargo_metadata {
        Some(path) => std::fs::read_to_string(path)?,
        None => query_cargo_metadata()?,
    };
    let footprint_descriptor_bytes = std::fs::read(&args.footprint_descriptors)?;
    let mut footprint_descriptors =
        serde_json::from_slice::<CheckedFootprintDescriptors>(&footprint_descriptor_bytes)?;
    refresh_generated_view_digests(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &mut footprint_descriptors,
    )?;
    let measured_footprint_descriptor_bytes = serde_json::to_vec(&footprint_descriptors)?;
    let inventory = generate_inventory(GenerateInventoryOptions {
        architecture_toml: ARCHITECTURE_BOUNDARIES,
        cargo_metadata_json: &cargo_metadata,
        footprint_descriptors,
    })?;

    let envelope = CompatibilityInventoryEnvelopeV1 {
        metadata: InventoryRunMetadata {
            binary: "compatibility-inventory".to_owned(),
            commit: args.commit,
            generated_at: args.generated_at,
            watermark: args.watermark,
            source_set_digest: source_set_digest(
                &cargo_metadata,
                &measured_footprint_descriptor_bytes,
            ),
            semantic_snapshot_digest: semantic_snapshot_digest(&inventory)?,
        },
        inventory,
    };
    envelope.validate()?;

    if let Some(path) = &args.check {
        let baseline = load_envelope_path(path)?;
        if baseline.inventory != envelope.inventory {
            return Err(Box::new(CliError(format!(
                "{} is stale (expected {}, generated {})",
                path.display(),
                semantic_snapshot_digest(&baseline.inventory)?,
                envelope.metadata.semantic_snapshot_digest,
            ))));
        }
    }

    let bytes = match args.format {
        OutputFormat::Json => {
            let mut bytes = serde_json::to_vec(&envelope)?;
            bytes.push(b'\n');
            bytes
        }
        OutputFormat::Markdown => render_compact_markdown(&envelope.inventory)?.into_bytes(),
    };

    if let Some(path) = &args.write {
        write_file(path, &bytes)?;
    } else {
        io::stdout().write_all(&bytes)?;
    }
    Ok(())
}

fn refresh_generated_view_digests(
    repo_root: &Path,
    descriptors: &mut CheckedFootprintDescriptors,
) -> Result<(), CliError> {
    let canonical_root = repo_root.canonicalize().map_err(|error| {
        CliError(format!(
            "failed to resolve repository root {}: {error}",
            repo_root.display()
        ))
    })?;

    for view in &mut descriptors.generated_views {
        let relative = Path::new(&view.output_ref);
        if relative.as_os_str().is_empty()
            || relative
                .components()
                .any(|component| !matches!(component, Component::Normal(_)))
        {
            return Err(CliError(format!(
                "unsafe generated-view output_ref {:?}",
                view.output_ref
            )));
        }

        let candidate = canonical_root.join(relative);
        let resolved = candidate.canonicalize().map_err(|error| {
            CliError(format!(
                "generated-view output_ref {:?} is missing or unreadable: {error}",
                view.output_ref
            ))
        })?;
        if !resolved.starts_with(&canonical_root) || !resolved.is_file() {
            return Err(CliError(format!(
                "unsafe generated-view output_ref {:?}",
                view.output_ref
            )));
        }

        let bytes = std::fs::read(&resolved).map_err(|error| {
            CliError(format!(
                "generated-view output_ref {:?} is unreadable: {error}",
                view.output_ref
            ))
        })?;
        view.actual_digest = format!("sha256:{}", hex::encode(Sha256::digest(bytes)));
    }
    Ok(())
}

fn source_set_digest(cargo_metadata: &str, footprint_descriptors: &[u8]) -> String {
    let mut digest = Sha256::new();
    digest.update(ARCHITECTURE_BOUNDARIES.as_bytes());
    digest.update([0]);
    digest.update(cargo_metadata.as_bytes());
    digest.update([0]);
    digest.update(footprint_descriptors);
    format!("sha256:{}", hex::encode(digest.finalize()))
}

fn query_cargo_metadata() -> Result<String, Box<dyn Error>> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let output = Command::new(cargo)
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()?;
    if !output.status.success() {
        return Err(Box::new(CliError(format!(
            "cargo metadata failed with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim(),
        ))));
    }
    String::from_utf8(output.stdout).map_err(Into::into)
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<(), io::Error> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracedecay::compatibility_inventory::footprint::GeneratedViewDescriptor;

    fn descriptors(output_ref: &str, actual_digest: &str) -> CheckedFootprintDescriptors {
        CheckedFootprintDescriptors {
            generated_views: vec![GeneratedViewDescriptor {
                output_ref: output_ref.to_owned(),
                expected_digest: "sha256:fixture-pin".to_owned(),
                actual_digest: actual_digest.to_owned(),
            }],
            ..CheckedFootprintDescriptors::default()
        }
    }

    #[test]
    fn generated_view_digest_is_measured_from_repo_output() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("generated")).unwrap();
        std::fs::write(root.path().join("generated/view.md"), b"measured bytes").unwrap();
        let mut checked = descriptors("generated/view.md", "sha256:fixture-actual");

        refresh_generated_view_digests(root.path(), &mut checked).unwrap();

        assert_eq!(
            checked.generated_views[0].actual_digest,
            format!("sha256:{}", hex::encode(Sha256::digest(b"measured bytes")))
        );
        assert_eq!(
            checked.generated_views[0].expected_digest,
            "sha256:fixture-pin"
        );
    }

    #[test]
    fn generated_view_measurement_rejects_unsafe_and_missing_paths() {
        let root = tempfile::tempdir().unwrap();
        assert!(
            refresh_generated_view_digests(
                root.path(),
                &mut descriptors("../outside.md", "ignored")
            )
            .is_err()
        );
        assert!(
            refresh_generated_view_digests(
                root.path(),
                &mut descriptors("generated/missing.md", "ignored")
            )
            .is_err()
        );
    }
}
