//! Build-provenance authentication for release archives.
//!
//! `SHA256SUMS` is uploaded to the same release as the archives it lists, so
//! whoever can replace one asset can replace both consistently. The release
//! workflows also attest every archive they build through GitHub artifact
//! attestations, and that proof cannot be minted by uploading assets: it is a
//! Sigstore bundle whose Fulcio certificate names the workflow run that
//! signed it and whose signature is recorded in the Rekor transparency log.
//!
//! An archive is installable only when one attestation GitHub holds for its
//! SHA-256 digest verifies against the Sigstore public-good trust root
//! (certificate chain and SCT, Rekor inclusion, DSSE signature, in-toto
//! subject digest) and its certificate names this repository's release
//! workflow for the channel, run from `master` or from the release tag. The
//! trust root is the one `sigstore-verify` embeds, so it is pinned by the
//! locked crate version and never fetched at update time.

use std::fmt;
use std::time::Duration;

use serde::Deserialize;
use sigstore_verify::trust_root::{SIGSTORE_PRODUCTION_TRUSTED_ROOT, TrustedRoot};
use sigstore_verify::types::{Bundle, Sha256Hash};
use sigstore_verify::{VerificationPolicy, Verifier};
use tracedecay_dashboard_api::cloud::ReleaseLookupError;

use super::GITHUB_REPO;
use crate::cloud;

/// The OIDC issuer of every GitHub Actions workflow identity.
const GITHUB_ACTIONS_ISSUER: &str = "https://token.actions.githubusercontent.com";

/// Where one release's attestations are looked up, and the only certificate
/// identities allowed to have signed them.
#[derive(Debug)]
pub(super) struct ReleaseProvenance {
    attestations_url: String,
    authorization: Option<String>,
    signers: [String; 2],
}

impl ReleaseProvenance {
    /// `release-beta.yml` builds prereleases and `release.yml` stable
    /// releases. Both are dispatched from `master` or run on the release tag,
    /// the same two refs the workflows accept when they re-verify retained
    /// assets.
    pub(super) fn for_release(
        api_base: &str,
        authorization: Option<&str>,
        tag: &str,
        is_beta: bool,
    ) -> Self {
        let workflow = if is_beta {
            "release-beta.yml"
        } else {
            "release.yml"
        };
        let signer = |git_ref: &str| {
            format!("https://github.com/{GITHUB_REPO}/.github/workflows/{workflow}@{git_ref}")
        };
        Self {
            attestations_url: format!("{api_base}/repos/{GITHUB_REPO}/attestations"),
            authorization: authorization.map(str::to_owned),
            signers: [
                signer("refs/heads/master"),
                signer(&format!("refs/tags/{tag}")),
            ],
        }
    }
}

/// Why a release archive's build provenance was not accepted. Every variant
/// refuses the upgrade before any release member is staged.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum AttestationRefusal {
    /// GitHub holds no attestation for the archive digest.
    Missing { digest: String },
    /// The attestation lookup failed, so nothing was verified.
    LookupFailed {
        digest: String,
        error: ReleaseLookupError,
    },
    /// The embedded Sigstore trust root could not be loaded.
    TrustRootUnusable { detail: String },
    /// GitHub holds attestations for the digest and none of them is a valid
    /// proof from this channel's release workflow.
    Rejected {
        digest: String,
        rejections: Vec<BundleRejection>,
    },
}

/// Why one attestation bundle is not proof of the archive's provenance.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum BundleRejection {
    /// The record carries no inline bundle and its `bundle_url` did not yield one.
    Unreadable { detail: String },
    /// Sigstore verification failed: certificate chain, transparency log,
    /// DSSE signature, or a subject digest other than the archive's.
    Unverified { detail: String },
    /// The bundle verifies, but its certificate names another signer.
    UntrustedSigner { identity: String },
}

impl fmt::Display for BundleRejection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unreadable { detail } => write!(formatter, "unreadable bundle ({detail})"),
            Self::Unverified { detail } => write!(formatter, "failed verification ({detail})"),
            Self::UntrustedSigner { identity } => {
                write!(formatter, "signed by untrusted identity {identity}")
            }
        }
    }
}

impl fmt::Display for AttestationRefusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Missing { digest } => write!(
                formatter,
                "GitHub has no build-provenance attestation for sha256:{digest}; refusing an \
                 unattested release archive"
            ),
            Self::LookupFailed { digest, error } => write!(
                formatter,
                "cannot look up the build-provenance attestation for sha256:{digest}, {error}; \
                 refusing an unattested release archive"
            ),
            Self::TrustRootUnusable { detail } => write!(
                formatter,
                "the embedded Sigstore trust root is unusable ({detail}); refusing an \
                 unattested release archive"
            ),
            Self::Rejected { digest, rejections } => {
                write!(
                    formatter,
                    "no build-provenance attestation for sha256:{digest} proves a release \
                     workflow build: "
                )?;
                for (index, rejection) in rejections.iter().enumerate() {
                    let separator = if index == 0 { "" } else { "; " };
                    write!(
                        formatter,
                        "{separator}attestation {}: {rejection}",
                        index + 1
                    )?;
                }
                Ok(())
            }
        }
    }
}

#[derive(Deserialize)]
struct AttestationListing {
    attestations: Vec<AttestationRecord>,
}

#[derive(Deserialize)]
struct AttestationRecord {
    bundle: Option<Bundle>,
    bundle_url: Option<String>,
}

/// Verifies that `digest` is the SHA-256 of an archive this channel's
/// release workflow built, and returns the certificate identity that signed
/// it.
pub(super) fn verify_release_attestation(
    provenance: &ReleaseProvenance,
    digest: [u8; 32],
) -> Result<String, AttestationRefusal> {
    let digest_hex = hex::encode(digest);
    let url = format!("{}/sha256:{digest_hex}", provenance.attestations_url);
    let listing: Option<AttestationListing> = cloud::get_release_json(
        &url,
        provenance.authorization.as_deref(),
        Duration::from_secs(30),
    )
    .map_err(|error| AttestationRefusal::LookupFailed {
        digest: digest_hex.clone(),
        error,
    })?;
    let records = listing.map_or_else(Vec::new, |listing| listing.attestations);
    if records.is_empty() {
        return Err(AttestationRefusal::Missing { digest: digest_hex });
    }

    let verifier = TrustedRoot::from_json(SIGSTORE_PRODUCTION_TRUSTED_ROOT)
        .map_err(|error| error.to_string())
        .and_then(|root| Verifier::new(&root).map_err(|error| error.to_string()))
        .map_err(|detail| AttestationRefusal::TrustRootUnusable { detail })?;
    let policy = VerificationPolicy::with_issuer(GITHUB_ACTIONS_ISSUER);
    let artifact = Sha256Hash::from_bytes(digest);

    let mut rejections = Vec::with_capacity(records.len());
    for record in records {
        let bundle = match record_bundle(record) {
            Ok(bundle) => bundle,
            Err(detail) => {
                rejections.push(BundleRejection::Unreadable { detail });
                continue;
            }
        };
        let verified = match verifier.verify(artifact, &bundle, &policy) {
            Ok(verified) => verified,
            Err(error) => {
                rejections.push(BundleRejection::Unverified {
                    detail: error.to_string(),
                });
                continue;
            }
        };
        let identity = verified.identity().unwrap_or_default();
        if provenance.signers.iter().any(|signer| signer == identity) {
            return Ok(identity.to_owned());
        }
        rejections.push(BundleRejection::UntrustedSigner {
            identity: identity.to_owned(),
        });
    }
    Err(AttestationRefusal::Rejected {
        digest: digest_hex,
        rejections,
    })
}

/// The record's bundle: inline, or downloaded from `bundle_url`, where GitHub
/// serves it as one raw snappy block. The bundle URL is presigned storage, so
/// no GitHub credential is sent to it.
fn record_bundle(record: AttestationRecord) -> Result<Bundle, String> {
    if let Some(bundle) = record.bundle {
        return Ok(bundle);
    }
    let url = record
        .bundle_url
        .ok_or("the record has neither `bundle` nor `bundle_url`")?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .into();
    let compressed = agent
        .get(&url)
        .header("User-Agent", "tracedecay")
        .call()
        .and_then(|mut response| response.body_mut().read_to_vec())
        .map_err(|error| format!("bundle_url download failed: {error}"))?;
    let json = snap::raw::Decoder::new()
        .decompress_vec(&compressed)
        .map_err(|error| format!("bundle_url is not a snappy block: {error}"))?;
    serde_json::from_slice(&json).map_err(|error| format!("bundle_url is not a bundle: {error}"))
}

#[cfg(test)]
mod tests;
