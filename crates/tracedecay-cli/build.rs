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
    io::Read as _,
    io::Write as IoWrite,
    path::{Path, PathBuf},
    process::Command,
};

use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

#[path = "build-support/dashboard_bundle.rs"]
mod dashboard_bundle;
#[path = "build-support/dashboard_manifest.rs"]
mod dashboard_manifest;
#[path = "build-support/source_provenance.rs"]
mod source_provenance;
#[path = "build-support/watched_input_file.rs"]
mod watched_input_file;

const DASHBOARD_BUILD_INPUTS: &[&str] = &[
    "dashboard/src",
    "dashboard/codegen/schemas",
    "dashboard/package.json",
    "pnpm-lock.yaml",
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

/// Directory under `OUT_DIR` for gzip-compressed dashboard asset payloads.
const DASHBOARD_GZ_DIR: &str = "dashboard-gz";

/// Extensions whose on-disk form compresses enough to justify a gzip embed.
/// Already-compressed fonts/images stay identity.
fn dashboard_asset_should_gzip(relative: &str) -> bool {
    matches!(
        relative.rsplit('.').next().unwrap_or(""),
        "html" | "js" | "mjs" | "css" | "json" | "map" | "svg" | "txt"
    )
}

/// One embeddable dashboard asset after optional gzip staging.
struct EmbeddedDashboardAsset {
    relative: String,
    include_env: &'static str,
    include_path: String,
    content_type: &'static str,
    encoding: &'static str,
}

/// The embedded dashboard bundle: manifest-validated relative paths, the
/// `include_bytes!` roots the generated module uses, and the cross-tool
/// bundle digest that becomes the HTTP cache tag.
struct EmbeddedDashboard {
    assets: Vec<EmbeddedDashboardAsset>,
    digest_hex: String,
}

impl EmbeddedDashboard {
    fn staged(bundle: dashboard_bundle::StagedBundle, out_dir: &Path) -> Result<Self, Box<dyn Error>> {
        let include_root = format!("/{BUNDLE_STORE_DIR}/{}", bundle.digest_hex);
        let source_root = out_dir.join(BUNDLE_STORE_DIR).join(&bundle.digest_hex);
        Self::from_sources(
            &bundle.asset_paths,
            "OUT_DIR",
            &include_root,
            &source_root,
            out_dir,
            bundle.digest_hex,
        )
    }

    fn packaged(
        asset_paths: Vec<String>,
        digest_hex: String,
        app_dist: &Path,
        out_dir: &Path,
    ) -> Result<Self, Box<dyn Error>> {
        Self::from_sources(
            &asset_paths,
            "CARGO_MANIFEST_DIR",
            "/dashboard/app-dist",
            app_dist,
            out_dir,
            digest_hex,
        )
    }

    fn from_sources(
        asset_paths: &[String],
        identity_include_env: &'static str,
        identity_include_root: &str,
        source_root: &Path,
        out_dir: &Path,
        digest_hex: String,
    ) -> Result<Self, Box<dyn Error>> {
        let gz_root = out_dir.join(DASHBOARD_GZ_DIR);
        fs::create_dir_all(&gz_root).map_err(|error| {
            format!(
                "failed to create dashboard gzip dir {}: {error}",
                gz_root.display()
            )
        })?;
        let mut assets = Vec::with_capacity(asset_paths.len());
        for relative in asset_paths {
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
            let source_path = source_root.join(relative);
            let raw = fs::read(&source_path).map_err(|error| {
                format!(
                    "failed to read dashboard asset {} for embed: {error}",
                    source_path.display()
                )
            })?;
            if dashboard_asset_should_gzip(relative) {
                let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
                encoder.write_all(&raw).map_err(|error| {
                    format!("gzip dashboard asset {relative}: {error}")
                })?;
                let compressed = encoder.finish().map_err(|error| {
                    format!("gzip finish dashboard asset {relative}: {error}")
                })?;
                let mut roundtrip = Vec::new();
                GzDecoder::new(compressed.as_slice())
                    .read_to_end(&mut roundtrip)
                    .map_err(|error| {
                        format!("gzip roundtrip dashboard asset {relative}: {error}")
                    })?;
                if roundtrip != raw {
                    return Err(format!(
                        "gzip roundtrip for dashboard asset {relative} did not match the source bytes"
                    )
                    .into());
                }
                if compressed.len() < raw.len() {
                    let gz_relative = relative.replace('/', "__");
                    let dest = gz_root.join(&gz_relative);
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent).map_err(|error| {
                            format!(
                                "failed to create {}: {error}",
                                parent.display()
                            )
                        })?;
                    }
                    fs::write(&dest, &compressed).map_err(|error| {
                        format!(
                            "failed to write gzip dashboard asset {}: {error}",
                            dest.display()
                        )
                    })?;
                    assets.push(EmbeddedDashboardAsset {
                        relative: relative.clone(),
                        include_env: "OUT_DIR",
                        include_path: format!("/{DASHBOARD_GZ_DIR}/{gz_relative}"),
                        content_type,
                        encoding: "Gzip",
                    });
                    continue;
                }
            }
            assets.push(EmbeddedDashboardAsset {
                relative: relative.clone(),
                include_env: identity_include_env,
                include_path: format!("{identity_include_root}/{relative}"),
                content_type,
                encoding: "Identity",
            });
        }
        Ok(Self {
            assets,
            digest_hex,
        })
    }
}

/// Prepares and embeds the dashboard bundle.
///
/// Checkout mode embeds an immutable, digest-named copy under `OUT_DIR`, never
/// the checkout-global `dashboard/app-dist` that `rsbuild dev` and other
/// target directories rewrite. The frontend is rebuilt, straight into the
/// store's staging directory, only when the fingerprint of its inputs
/// differs from the recorded one. This script never installs dependencies.
/// `pnpm install` owns them, together with the Cargo sources this build
/// compiles, and `pnpm run` (`verifyDepsBeforeRun: error`) refuses to build
/// against a `node_modules` tree that no longer matches `pnpm-lock.yaml`. When
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
        return EmbeddedDashboard::packaged(asset_paths, digest_hex, &app_dist, out_dir);
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
    watched_input_file::WatchedInputFile::from_env("TRACEDECAY_DASHBOARD_BUNDLE_SHA256_FILE")
        .emit();
    println!("cargo::rerun-if-env-changed=TRACEDECAY_DASHBOARD_DIST_DIR");

    let store = out_dir.join(BUNDLE_STORE_DIR);
    fs::create_dir_all(&store)
        .map_err(|error| format!("failed to create {}: {error}", store.display()))?;

    if std::env::var_os("TRACEDECAY_SKIP_DASHBOARD_BUILD").is_some() {
        // Skip-without-proof is not allowed: the skipper must name the digest
        // of the bundle it expects this build to embed. Once that bundle is
        // staged, later reruns reuse it without reading app-dist again.
        let expected = required_bundle_digest_env()?;
        if let Some(bundle) = dashboard_bundle::open(&store, &expected)? {
            return EmbeddedDashboard::staged(bundle, out_dir);
        }
        // TRACEDECAY_DASHBOARD_DIST_DIR names a prebuilt bundle outside the
        // checkout layout (Bazel's js_run_binary output tree). Without it the
        // staged copy comes from dashboard/app-dist as usual.
        let app_dist = match std::env::var_os("TRACEDECAY_DASHBOARD_DIST_DIR") {
            Some(dir) => {
                let dir = PathBuf::from(dir);
                // A build system's output tree materializes its inputs as
                // symlinks; the digest compare below still fails closed on
                // any byte that is not the bundle the digest was taken over.
                let bundle = dashboard_bundle::stage_copy_build_output(&store, &dir)?;
                if bundle.digest_hex != expected {
                    return Err(format!(
                        "TRACEDECAY_SKIP_DASHBOARD_BUILD is set but the prebuilt dashboard bundle \
                         at {} has digest {}, not the expected \
                         TRACEDECAY_DASHBOARD_BUNDLE_SHA256={expected}; rebuild the dashboard or \
                         fix the expected digest",
                        dir.display(),
                        bundle.digest_hex,
                    )
                    .into());
                }
                return EmbeddedDashboard::staged(bundle, out_dir);
            }
            None => dashboard.join("app-dist"),
        };
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
        return EmbeddedDashboard::staged(bundle, out_dir);
    }

    // Fingerprint before building: an input edited mid-build then records a
    // fingerprint that no longer matches, and the next rerun rebuilds.
    let inputs_fingerprint =
        dashboard_bundle::inputs_fingerprint(&repository_root, DASHBOARD_BUILD_INPUTS)?;
    if let Some(record) = dashboard_bundle::read_build_record(&store)
        && record.inputs_fingerprint == inputs_fingerprint
        && let Some(bundle) = dashboard_bundle::open(&store, &record.bundle_digest)?
    {
        return EmbeddedDashboard::staged(bundle, out_dir);
    }

    let staging = dashboard_bundle::prepare_staging(&store)
        .map_err(|error| format!("failed to prepare {}: {error}", store.display()))?;
    run_pnpm(
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
    EmbeddedDashboard::staged(bundle, out_dir)
}

fn required_bundle_digest_env() -> Result<String, Box<dyn Error>> {
    let expected = match std::env::var_os("TRACEDECAY_DASHBOARD_BUNDLE_SHA256") {
        Some(raw) => raw.into_string().map_err(|raw| {
            format!(
                "TRACEDECAY_DASHBOARD_BUNDLE_SHA256 is set to non-UTF-8 value {raw:?}; \
                 expected a 64-character lowercase hex sha256 digest"
            )
        })?,
        None => {
            // Build systems that produce the digest as an action output
            // (Bazel) hand over a file path, not a literal value.
            let Some(file) = std::env::var_os("TRACEDECAY_DASHBOARD_BUNDLE_SHA256_FILE") else {
                return Err("TRACEDECAY_SKIP_DASHBOARD_BUILD is set but neither \
                     TRACEDECAY_DASHBOARD_BUNDLE_SHA256 nor \
                     TRACEDECAY_DASHBOARD_BUNDLE_SHA256_FILE is; skipping the dashboard \
                     build requires the expected 64-hex sha256 bundle digest so the \
                     embedded bytes are proven, not assumed"
                    .into());
            };
            let contents = fs::read_to_string(&file).map_err(|error| {
                format!(
                    "failed to read TRACEDECAY_DASHBOARD_BUNDLE_SHA256_FILE {}: {error}",
                    Path::new(&file).display()
                )
            })?;
            contents.trim().to_owned()
        }
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

fn run_pnpm(dir: &Path, args: &[&str], envs: &[(&str, &OsStr)]) -> io::Result<()> {
    let spawn = |program: &str| {
        Command::new(program)
            .args(args)
            .envs(envs.iter().copied())
            .current_dir(dir)
            .status()
    };
    // Standalone pnpm installs `pnpm.exe`; npm and Corepack install a
    // `pnpm.cmd` shim, which `Command` does not resolve on its own.
    let status = match spawn("pnpm") {
        Err(error) if cfg!(windows) && error.kind() == io::ErrorKind::NotFound => spawn("pnpm.cmd"),
        result => result,
    }
    .map_err(|error| {
        io::Error::other(format!(
            "failed to run pnpm {}: {error}; install pnpm and run `pnpm install` at the \
             repository root",
            args.join(" ")
        ))
    })?;
    if status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "pnpm {} failed in {} (status {status}); the dashboard frontend must build for the \
             binary to embed it (run `pnpm install` at the repository root if dependencies are \
             out of date)",
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
    for asset in &dashboard.assets {
        let _ = writeln!(
            code,
            "        tracedecay_api::StaticDashboardAsset {{ path: {:?}, \
             contents: include_bytes!(concat!(env!({:?}), {:?})), \
             content_type: {:?}, \
             encoding: tracedecay_api::StaticAssetEncoding::{} }},",
            asset.relative,
            asset.include_env,
            asset.include_path,
            asset.content_type,
            asset.encoding,
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
    // source order, verified git worktree, release env, packaged VCS journal.
    println!("cargo::rerun-if-env-changed=TRACEDECAY_RELEASE_GIT_SHA");
    watched_input_file::WatchedInputFile::from_env("TRACEDECAY_RELEASE_GIT_SHA_FILE").emit();
    println!("cargo::rerun-if-changed=build-support/source_provenance.rs");
    println!("cargo::rerun-if-changed=build-support/watched_input_file.rs");
    let release_env_provenance = match std::env::var_os("TRACEDECAY_RELEASE_GIT_SHA") {
        None => match std::env::var_os("TRACEDECAY_RELEASE_GIT_SHA_FILE") {
            // Same provenance source, carried by a file so build systems can
            // pass an action output instead of a literal env value.
            None => None,
            Some(file) => Some(
                fs::read_to_string(&file)
                    .map_err(|error| {
                        format!(
                            "failed to read TRACEDECAY_RELEASE_GIT_SHA_FILE {}: {error}",
                            Path::new(&file).display()
                        )
                    })?
                    .trim()
                    .to_owned(),
            ),
        },
        Some(raw) => Some(raw.into_string().map_err(|raw| {
            format!(
                "TRACEDECAY_RELEASE_GIT_SHA is set to non-UTF-8 value {raw:?}; expected a \
                 40-character lowercase hex commit sha optionally followed by `.dirty`"
            )
        })?),
    };
    let provenance = source_provenance::resolve(
        &repository_root,
        &manifest_dir,
        release_env_provenance.as_deref(),
    )?;
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
