//! CLI build script: resolves the source provenance baked into the binary and
//! embeds the dashboard bundle.
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
//!
//! A build script has one rerun set, and the source-provenance watcher covers
//! the whole repository, so a Rust-only edit reruns this script too. The
//! frontend is therefore not rebuilt because the script ran: it is rebuilt
//! only when the content fingerprint of [`DASHBOARD_BUILD_INPUTS`] differs
//! from the one recorded beside the bundle in `OUT_DIR`.

use std::{
    error::Error,
    ffi::OsStr,
    fmt::Write as _,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use sha2::{Digest, Sha256};

#[path = "build-support/dashboard_bundle.rs"]
mod dashboard_bundle;
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

/// Bundle store under `OUT_DIR`: `<store>/staging` while a producer writes,
/// `<store>/<digest>` once validated, plus the build record.
const BUNDLE_STORE_DIR: &str = "dashboard-bundle";

/// Rsbuild reads this to redirect its output away from the checkout-global
/// `dashboard/app-dist` (see `dashboard/rsbuild.config.ts`).
const DASHBOARD_DIST_PATH_ENV: &str = "TRACEDECAY_DASHBOARD_DIST_PATH";

/// Written into `dashboard/node_modules` after a successful `npm ci`: the
/// sha256 of the `package-lock.json` that installation satisfied. The marker
/// lives beside the installed tree so every target directory and linked
/// worktree sharing that tree shares the attestation; a tree without a
/// matching marker cannot attest the current lockfile and is reinstalled.
const LOCKFILE_MARKER: &str = ".tracedecay-lockfile-sha256";

/// The embedded dashboard bundle: manifest-validated relative paths, the
/// `include_bytes!` root the generated module uses (a compile-time env var
/// plus a path under it), and the cross-tool bundle digest that becomes the
/// HTTP cache tag.
struct EmbeddedDashboard {
    asset_paths: Vec<String>,
    include_env: &'static str,
    include_root: String,
    digest_hex: String,
}

impl EmbeddedDashboard {
    fn staged(bundle: dashboard_bundle::StagedBundle) -> Self {
        Self {
            asset_paths: bundle.asset_paths,
            include_env: "OUT_DIR",
            include_root: format!("/{BUNDLE_STORE_DIR}/{}", bundle.digest_hex),
            digest_hex: bundle.digest_hex,
        }
    }
}

/// Prepares and embeds the dashboard bundle.
///
/// Checkout mode embeds an immutable, digest-named copy under `OUT_DIR`, never
/// the checkout-global `dashboard/app-dist` that `rsbuild dev` and other
/// target directories rewrite. The frontend is rebuilt — straight into the
/// store's staging directory — only when the fingerprint of its inputs
/// differs from the recorded one; `npm ci` runs only when the installed tree
/// cannot attest the current `package-lock.json`. When
/// `TRACEDECAY_SKIP_DASHBOARD_BUILD` is set the prebuilt `dashboard/app-dist`
/// is staged instead and must match the digest
/// `TRACEDECAY_DASHBOARD_BUNDLE_SHA256` names, so a skip can never embed
/// unproven bytes. Packaged crates carry a staged `dashboard/app-dist` whose
/// integrity Cargo's package checksums already guarantee. A missing or invalid
/// bundle always fails the build; there is no empty-assets fallback.
fn embed_dashboard(
    manifest_dir: &Path,
    out_dir: &Path,
) -> Result<EmbeddedDashboard, Box<dyn Error>> {
    let package_local_dashboard = manifest_dir.join("dashboard");
    if package_local_dashboard.is_dir() {
        // Packaged-crate mode: release packaging staged the bundle into the
        // crate directory and Cargo's checksums are the integrity authority.
        let app_dist = package_local_dashboard.join("app-dist");
        let asset_paths = dashboard_manifest::dashboard_asset_paths(&app_dist)?;
        let digest_hex = dashboard_bundle::bundle_digest(&app_dist, &asset_paths)?;
        return Ok(EmbeddedDashboard {
            asset_paths,
            include_env: "CARGO_MANIFEST_DIR",
            include_root: "/dashboard/app-dist".to_owned(),
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

    let store = out_dir.join(BUNDLE_STORE_DIR);
    fs::create_dir_all(&store)
        .map_err(|error| format!("failed to create {}: {error}", store.display()))?;

    if std::env::var_os("TRACEDECAY_SKIP_DASHBOARD_BUILD").is_some() {
        // Skip-without-proof is not allowed: the skipper must name the digest
        // of the bundle it expects this build to embed. Once that bundle is
        // staged, later reruns reuse it without reading app-dist again.
        let expected = required_bundle_digest_env()?;
        if let Some(bundle) = dashboard_bundle::open(&store, &expected)? {
            return Ok(EmbeddedDashboard::staged(bundle));
        }
        let app_dist = dashboard.join("app-dist");
        let bundle = dashboard_bundle::stage_copy(&store, &app_dist)?;
        if bundle.digest_hex != expected {
            return Err(format!(
                "TRACEDECAY_SKIP_DASHBOARD_BUILD is set but the prebuilt dashboard bundle \
                 at {} has digest {}, not the expected \
                 TRACEDECAY_DASHBOARD_BUNDLE_SHA256={expected}; rebuild the dashboard or \
                 fix the expected digest",
                app_dist.display(),
                bundle.digest_hex,
            )
            .into());
        }
        return Ok(EmbeddedDashboard::staged(bundle));
    }

    // Fingerprint before building: an input edited mid-build then records a
    // fingerprint that no longer matches, and the next rerun rebuilds.
    let inputs_fingerprint =
        dashboard_bundle::inputs_fingerprint(&repository_root, DASHBOARD_BUILD_INPUTS)?;
    if let Some(record) = dashboard_bundle::read_build_record(&store)
        && record.inputs_fingerprint == inputs_fingerprint
        && let Some(bundle) = dashboard_bundle::open(&store, &record.bundle_digest)?
    {
        return Ok(EmbeddedDashboard::staged(bundle));
    }

    ensure_dashboard_dependencies(&dashboard)?;
    let staging = dashboard_bundle::prepare_staging(&store)
        .map_err(|error| format!("failed to prepare {}: {error}", store.display()))?;
    run_npm(
        &dashboard,
        &["run", "build"],
        &[(DASHBOARD_DIST_PATH_ENV, staging.as_os_str())],
    )?;
    let bundle = dashboard_bundle::promote(&store)?;
    dashboard_bundle::write_build_record(
        &store,
        &dashboard_bundle::BuildRecord {
            inputs_fingerprint,
            bundle_digest: bundle.digest_hex.clone(),
        },
    )
    .map_err(|error| {
        format!(
            "failed to record the dashboard build in {}: {error}",
            store.display()
        )
    })?;
    Ok(EmbeddedDashboard::staged(bundle))
}

/// Runs `npm ci` unless `node_modules` carries the marker of the current
/// `package-lock.json`. Existence of a `node_modules` directory alone proves
/// nothing about which lockfile it satisfies.
fn ensure_dashboard_dependencies(dashboard: &Path) -> Result<(), Box<dyn Error>> {
    let lockfile = dashboard.join("package-lock.json");
    let lockfile_bytes = fs::read(&lockfile)
        .map_err(|error| format!("failed to read {}: {error}", lockfile.display()))?;
    let lockfile_digest = dashboard_bundle::hex(&Sha256::digest(&lockfile_bytes));
    let marker = dashboard.join("node_modules").join(LOCKFILE_MARKER);
    if matches!(fs::read_to_string(&marker), Ok(recorded) if recorded.trim() == lockfile_digest) {
        return Ok(());
    }
    run_npm(dashboard, &["ci"], &[])?;
    fs::write(&marker, format!("{lockfile_digest}\n"))
        .map_err(|error| format!("failed to write {}: {error}", marker.display()))?;
    Ok(())
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

fn run_npm(dir: &Path, args: &[&str], envs: &[(&str, &OsStr)]) -> io::Result<()> {
    let status = Command::new(if cfg!(windows) { "npm.cmd" } else { "npm" })
        .args(args)
        .envs(envs.iter().copied())
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
             contents: include_bytes!(concat!(env!({:?}), {include_path:?})), \
             content_type: {content_type:?} }},",
            dashboard.include_env,
        );
    }
    let _ = writeln!(code, "    ],");
    let _ = writeln!(code, "    cache_tag: {:?},", dashboard.digest_hex);
    let _ = writeln!(code, "}};");
    Ok(code)
}

fn main() -> Result<(), Box<dyn Error>> {
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

    let out_dir = PathBuf::from(std::env::var("OUT_DIR")?);
    let dashboard = embed_dashboard(&manifest_dir, &out_dir)?;
    let code = generated_module(&provenance, &dashboard)?;

    let out = out_dir.join("product_runtime_generated.rs");
    if !matches!(fs::read_to_string(&out), Ok(current) if current == code) {
        fs::write(&out, code)?;
    }
    Ok(())
}
