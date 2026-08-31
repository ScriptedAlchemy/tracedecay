//! CLI build script: renders the terminal logo, resolves the source
//! provenance baked into the binary, and embeds the dashboard bundle.
//!
//! This is the only build script in the workspace that watches the repository
//! or embeds dashboard assets; the composition library (`crates/tracedecay`)
//! consumes both through the typed `register_product_runtime` API instead of
//! baking its own copies.
//!
//! Rerun-edge contract. Cargo recompiles this crate whenever the script
//! reruns, so every `rerun-if-changed` path below costs a rebuild when it
//! moves and must be load-bearing. This script must never watch
//! `dashboard/app-dist`, which Rsbuild cleans and rewrites: only frontend
//! source and configuration inputs are watched.

use std::{
    error::Error,
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use sha2::{Digest, Sha256};

#[path = "build-support/dashboard_manifest.rs"]
mod dashboard_manifest;
#[path = "build-support/source_provenance.rs"]
mod source_provenance;

const DASHBOARD_BUILD_INPUTS: &[&str] = &[
    "dashboard/src",
    "dashboard/codegen/schemas",
    "dashboard/package.json",
    "dashboard/package-lock.json",
    "dashboard/postcss.config.mjs",
    "dashboard/rsbuild.config.ts",
    "dashboard/tsconfig.json",
];

/// The crate lives at `crates/tracedecay-cli`, two directories below the
/// repository root, in a checkout.
const REPOSITORY_ROOT_FROM_CRATE: &str = "../..";

/// Fixed cross-tool bundle digest contract (`scripts/check-dashboard-bundle.py`
/// implements the same algorithm): sha256 over this prefix, then for each
/// manifest-validated relative path in sorted order the UTF-8 path bytes, one
/// 0x00 byte, the file byte length as u64 little-endian, then the file bytes.
const BUNDLE_DIGEST_PREFIX: &[u8] = b"tracedecay-dashboard-bundle-v1\0";

fn generate_logo() -> Result<(), Box<dyn Error>> {
    let out_path = Path::new("src/resources/logo.ansi");
    let logo_bytes = include_bytes!("src/resources/logo.png");
    let ansi = logo_art::image_to_ansi(logo_bytes, 90);
    // Only rewrite when the content differs: `cargo package` verification
    // rejects packages whose build script modifies files in the source dir.
    if !matches!(fs::read(out_path), Ok(current) if current == ansi.as_bytes()) {
        fs::write(out_path, ansi)?;
    }
    println!("cargo::rerun-if-changed=src/resources/logo.png");
    Ok(())
}

/// The embedded dashboard bundle: manifest-validated relative paths, the
/// `include_bytes!` root the generated module uses, and the cross-tool bundle
/// digest that becomes the HTTP cache tag.
struct EmbeddedDashboard {
    asset_paths: Vec<String>,
    include_root: &'static str,
    digest_hex: String,
}

/// Builds (or verifies) and embeds the dashboard bundle.
///
/// Checkout mode builds the frontend with Rsbuild — or, when
/// `TRACEDECAY_SKIP_DASHBOARD_BUILD` is set, verifies the existing app-dist
/// against the digest `TRACEDECAY_DASHBOARD_BUNDLE_SHA256` names, so a skip
/// can never embed unproven bytes. Packaged crates carry a staged
/// `dashboard/app-dist` whose integrity Cargo's package checksums already
/// guarantee. A missing or invalid app-dist always fails the build; there is
/// no empty-assets fallback.
fn embed_dashboard(manifest_dir: &Path) -> Result<EmbeddedDashboard, Box<dyn Error>> {
    let package_local_dashboard = manifest_dir.join("dashboard");
    if package_local_dashboard.is_dir() {
        // Packaged-crate mode: release packaging staged the bundle into the
        // crate directory and Cargo's checksums are the integrity authority.
        let app_dist = package_local_dashboard.join("app-dist");
        let asset_paths = dashboard_manifest::dashboard_asset_paths(&app_dist)?;
        let digest_hex = bundle_digest(&app_dist, &asset_paths)?;
        return Ok(EmbeddedDashboard {
            asset_paths,
            include_root: "/dashboard/app-dist",
            digest_hex,
        });
    }

    let repository_root = manifest_dir.join(REPOSITORY_ROOT_FROM_CRATE);
    let dashboard = repository_root.join("dashboard");
    if !dashboard.join("package.json").is_file() {
        return Err(format!(
            "no dashboard to embed: {} has no package-local dashboard/ and {} has no \
             package.json; a tracedecay binary cannot build without its dashboard bundle",
            manifest_dir.display(),
            dashboard.display(),
        )
        .into());
    }

    for input in DASHBOARD_BUILD_INPUTS {
        println!(
            "cargo::rerun-if-changed={}",
            repository_root.join(input).display()
        );
    }
    println!("cargo::rerun-if-env-changed=TRACEDECAY_SKIP_DASHBOARD_BUILD");
    println!("cargo::rerun-if-env-changed=TRACEDECAY_DASHBOARD_BUNDLE_SHA256");

    let app_dist = dashboard.join("app-dist");
    if std::env::var_os("TRACEDECAY_SKIP_DASHBOARD_BUILD").is_some() {
        // Skip-without-proof is not allowed: the skipper must name the digest
        // of the bundle it expects this build to embed.
        let expected = required_bundle_digest_env()?;
        let asset_paths = dashboard_manifest::dashboard_asset_paths(&app_dist)?;
        let digest_hex = bundle_digest(&app_dist, &asset_paths)?;
        if digest_hex != expected {
            return Err(format!(
                "TRACEDECAY_SKIP_DASHBOARD_BUILD is set but the existing dashboard bundle \
                 at {} has digest {digest_hex}, not the expected \
                 TRACEDECAY_DASHBOARD_BUNDLE_SHA256={expected}; rebuild the dashboard or \
                 fix the expected digest",
                app_dist.display(),
            )
            .into());
        }
        return Ok(EmbeddedDashboard {
            asset_paths,
            include_root: "/../../dashboard/app-dist",
            digest_hex,
        });
    }

    if !dashboard.join("node_modules").is_dir() {
        run_npm(&dashboard, &["ci"])?;
    }
    run_npm(&dashboard, &["run", "build"])?;
    let asset_paths = dashboard_manifest::dashboard_asset_paths(&app_dist)?;
    let digest_hex = bundle_digest(&app_dist, &asset_paths)?;
    Ok(EmbeddedDashboard {
        asset_paths,
        include_root: "/../../dashboard/app-dist",
        digest_hex,
    })
}

fn required_bundle_digest_env() -> Result<String, Box<dyn Error>> {
    let Some(raw) = std::env::var_os("TRACEDECAY_DASHBOARD_BUNDLE_SHA256") else {
        return Err(
            "TRACEDECAY_SKIP_DASHBOARD_BUILD is set but TRACEDECAY_DASHBOARD_BUNDLE_SHA256 \
             is not; skipping the dashboard build requires the expected 64-hex sha256 \
             bundle digest so the embedded bytes are proven, not assumed"
                .into(),
        );
    };
    let Some(expected) = raw.to_str().map(str::to_owned) else {
        return Err(format!(
            "TRACEDECAY_DASHBOARD_BUNDLE_SHA256 is set to non-UTF-8 value {raw:?}; \
             expected a 64-character lowercase hex sha256 digest"
        )
        .into());
    };
    let well_formed = expected.len() == 64
        && expected
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !well_formed {
        return Err(format!(
            "TRACEDECAY_DASHBOARD_BUNDLE_SHA256 is set to {expected:?}, which is not a \
             64-character lowercase hex sha256 digest"
        )
        .into());
    }
    Ok(expected)
}

fn bundle_digest(
    app_dist: &Path,
    sorted_relative_paths: &[String],
) -> Result<String, Box<dyn Error>> {
    let mut hasher = Sha256::new();
    hasher.update(BUNDLE_DIGEST_PREFIX);
    for relative in sorted_relative_paths {
        let bytes = fs::read(app_dist.join(relative)).map_err(|error| {
            format!("failed to read dashboard asset {relative} for the bundle digest: {error}")
        })?;
        hasher.update(relative.as_bytes());
        hasher.update([0u8]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update(&bytes);
    }
    let mut hex = String::with_capacity(64);
    for byte in hasher.finalize() {
        let _ = write!(hex, "{byte:02x}");
    }
    Ok(hex)
}

fn run_npm(dir: &Path, args: &[&str]) -> io::Result<()> {
    let status = Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" })
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|error| {
            io::Error::other(format!("failed to run npm {}: {error}", args.join(" ")))
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "npm {} failed in {} (status {status}); the dashboard frontend must build for the binary to embed it",
            args.join(" "),
            dir.display()
        )))
    }
}

fn generated_module(
    provenance: &source_provenance::ResolvedSourceProvenance,
    dashboard: &EmbeddedDashboard,
) -> Result<String, Box<dyn Error>> {
    let package_version = std::env::var("CARGO_PKG_VERSION")?;
    let dirty_suffix = if provenance.dirty { ".dirty" } else { "" };
    let build_version = format!("{package_version}+{}{dirty_suffix}", provenance.full_sha);

    let mut code = String::new();
    let _ = writeln!(
        code,
        "pub const PRODUCT_FULL_SHA: &str = {:?};",
        provenance.full_sha
    );
    let _ = writeln!(
        code,
        "pub const PRODUCT_SOURCE_DIRTY: bool = {};",
        provenance.dirty
    );
    let _ = writeln!(
        code,
        "pub const PRODUCT_BUILD_VERSION: &str = {build_version:?};"
    );
    let _ = writeln!(
        code,
        "pub static STATIC_DASHBOARD_ASSETS: tracedecay_api::StaticDashboardAssets = \
         tracedecay_api::StaticDashboardAssets {{"
    );
    let _ = writeln!(code, "    assets: &[");
    for relative in &dashboard.asset_paths {
        let content_type = match relative.rsplit('.').next().unwrap_or("") {
            "html" => "text/html; charset=utf-8",
            "js" | "mjs" => "application/javascript",
            "css" => "text/css",
            "json" | "map" => "application/json",
            "svg" => "image/svg+xml",
            "png" => "image/png",
            "ico" => "image/x-icon",
            "woff2" => "font/woff2",
            "woff" => "font/woff",
            "ttf" => "font/ttf",
            "txt" => "text/plain; charset=utf-8",
            _ => "application/octet-stream",
        };
        let include_path = format!("{}/{relative}", dashboard.include_root);
        let _ = writeln!(
            code,
            "        tracedecay_api::StaticDashboardAsset {{ path: {relative:?}, \
             contents: include_bytes!(concat!(env!(\"CARGO_MANIFEST_DIR\"), {include_path:?})), \
             content_type: {content_type:?} }},"
        );
    }
    let _ = writeln!(code, "    ],");
    let _ = writeln!(code, "    cache_tag: {:?},", dashboard.digest_hex);
    let _ = writeln!(code, "}};");
    Ok(code)
}

fn main() -> Result<(), Box<dyn Error>> {
    generate_logo()?;

    let manifest_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR")?);
    let repository_root = manifest_dir.join(REPOSITORY_ROOT_FROM_CRATE);

    // Source provenance: the exact commit this binary compiles, in strict
    // source order — verified git worktree, release env, packaged VCS journal.
    println!("cargo::rerun-if-env-changed=TRACEDECAY_RELEASE_GIT_SHA");
    println!("cargo::rerun-if-changed=build-support/source_provenance.rs");
    let release_env_sha = match std::env::var_os("TRACEDECAY_RELEASE_GIT_SHA") {
        None => None,
        Some(raw) => Some(raw.into_string().map_err(|raw| {
            format!(
                "TRACEDECAY_RELEASE_GIT_SHA is set to non-UTF-8 value {raw:?}; expected a \
                 40-character lowercase hex commit sha"
            )
        })?),
    };
    let provenance =
        source_provenance::resolve(&repository_root, &manifest_dir, release_env_sha.as_deref())?;
    match &provenance.origin {
        source_provenance::ProvenanceOrigin::VerifiedGit => {
            // Repo-wide watch: the baked commit must track commits, staging,
            // and every existing worktree input, or it silently describes an
            // older tree.
            for path in source_provenance::watch_paths(&repository_root) {
                println!("cargo::rerun-if-changed={}", path.display());
            }
        }
        source_provenance::ProvenanceOrigin::ReleaseEnv => {
            // rerun-if-env-changed above is the only edge the env source needs.
        }
        source_provenance::ProvenanceOrigin::PackagedVcsInfo { manifest_file } => {
            println!("cargo::rerun-if-changed={}", manifest_file.display());
        }
    }

    let dashboard = embed_dashboard(&manifest_dir)?;
    let code = generated_module(&provenance, &dashboard)?;

    let out_dir = std::env::var("OUT_DIR")?;
    let out = Path::new(&out_dir).join("product_runtime_generated.rs");
    if !matches!(fs::read_to_string(&out), Ok(current) if current == code) {
        fs::write(&out, code)?;
    }
    Ok(())
}
