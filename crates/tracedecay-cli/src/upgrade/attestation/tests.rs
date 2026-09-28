//! Real GitHub attestation bundles, captured from the attestations API and
//! served from loopback: the verifier runs its full Sigstore checks against
//! the embedded public-good trust root with no network access.

use serde_json::{Value, json};

use super::super::test_server::{FakeGitHub, attestations_path};
use super::{AttestationRefusal, BundleRejection, ReleaseProvenance, verify_release_attestation};

/// `tracedecay-beta-v1.0.0-beta.59-x86_64-linux.tar.gz`, attested by
/// `release-beta.yml` dispatched from `master`.
const BETA_ARCHIVE: &str = "5e3d899a2452c4d27bf86f717752b3308983410b840f8eda5bb6163381b7315b";
const BETA_BUNDLE: &str = include_str!(
    "../../../tests/fixtures/release-attestations/tracedecay-beta-v1.0.0-beta.59-x86_64-linux.sigstore.json"
);
/// `tracedecay-v0.0.74-x86_64-linux.tar.gz`, attested by `release.yml` on
/// its release tag.
const STABLE_ARCHIVE: &str = "2712ab8a7bc0b5085bf984834df541b08d9fbad0eb655657fa685d2e5af57d1d";
const STABLE_BUNDLE: &str = include_str!(
    "../../../tests/fixtures/release-attestations/tracedecay-v0.0.74-x86_64-linux.sigstore.json"
);
/// `gh_2.101.0_linux_amd64.tar.gz` from `cli/cli`, as GitHub serves it from
/// `bundle_url`: a raw snappy block.
const OTHER_REPOSITORY_ARCHIVE: &str =
    "9bca2d1c16825f109907a23307628a2f0698fbf99662b73a5cf0b020293072b8";
const OTHER_REPOSITORY_BUNDLE: &[u8] = include_bytes!(
    "../../../tests/fixtures/release-attestations/gh_2.101.0_linux_amd64.sigstore.json.sn"
);

const BETA_SIGNER: &str = "https://github.com/ScriptedAlchemy/tracedecay/.github/workflows/release-beta.yml@refs/heads/master";
const STABLE_SIGNER: &str =
    "https://github.com/ScriptedAlchemy/tracedecay/.github/workflows/release.yml@refs/tags/v0.0.74";

fn digest(hex_digest: &str) -> [u8; 32] {
    hex::decode(hex_digest).unwrap().try_into().unwrap()
}

fn listing(bundles: &[Value]) -> Vec<u8> {
    let records: Vec<Value> = bundles
        .iter()
        .map(|bundle| json!({ "bundle": bundle }))
        .collect();
    json!({ "attestations": records }).to_string().into_bytes()
}

fn bundle(json: &str) -> Value {
    serde_json::from_str(json).unwrap()
}

/// The beta bundle with one base64 character of its DSSE signature changed,
/// so the signature bytes no longer verify but the bundle still parses.
fn beta_bundle_with_broken_signature() -> Value {
    let mut bundle = bundle(BETA_BUNDLE);
    let signature = bundle["dsseEnvelope"]["signatures"][0]["sig"]
        .as_str()
        .unwrap()
        .to_owned();
    let mut chars: Vec<char> = signature.chars().collect();
    chars[12] = if chars[12] == 'A' { 'B' } else { 'A' };
    bundle["dsseEnvelope"]["signatures"][0]["sig"] = Value::String(chars.into_iter().collect());
    bundle
}

/// Serves `body` as the attestation listing for `hex_digest` and verifies
/// that digest for the `tag` release on its channel.
fn verify_served(
    hex_digest: &str,
    body: Option<Vec<u8>>,
    tag: &str,
    is_beta: bool,
) -> (Result<String, AttestationRefusal>, Vec<String>) {
    let server = FakeGitHub::bind();
    let base = server.base.clone();
    let responses = body
        .map(|body| vec![(attestations_path(hex_digest), body)])
        .unwrap_or_default();
    let requests = server.serve(responses);
    let provenance = ReleaseProvenance::for_release(&base, None, tag, is_beta);
    let outcome = verify_release_attestation(&provenance, digest(hex_digest));
    let requests = requests.lock().unwrap().clone();
    (outcome, requests)
}

#[test]
fn the_release_workflow_attestation_for_the_archive_digest_is_accepted() {
    let (outcome, requests) = verify_served(
        BETA_ARCHIVE,
        Some(listing(&[bundle(BETA_BUNDLE)])),
        "v1.0.0-beta.59",
        true,
    );

    assert_eq!(outcome, Ok(BETA_SIGNER.to_owned()));
    assert_eq!(requests, [attestations_path(BETA_ARCHIVE)]);
}

#[test]
fn a_digest_github_holds_no_attestation_for_is_refused() {
    let (not_found, _) = verify_served(BETA_ARCHIVE, None, "v1.0.0-beta.59", true);
    let (empty, _) = verify_served(BETA_ARCHIVE, Some(listing(&[])), "v1.0.0-beta.59", true);

    let missing = AttestationRefusal::Missing {
        digest: BETA_ARCHIVE.to_owned(),
    };
    assert_eq!(not_found, Err(missing));
    assert_eq!(
        empty.unwrap_err().to_string(),
        format!(
            "GitHub has no build-provenance attestation for sha256:{BETA_ARCHIVE}; refusing an \
             unattested release archive"
        )
    );
}

#[test]
fn a_tampered_archive_is_refused_even_when_served_the_released_bundle() {
    let tampered = "0".repeat(64);

    let (outcome, _) = verify_served(
        &tampered,
        Some(listing(&[bundle(BETA_BUNDLE)])),
        "v1.0.0-beta.59",
        true,
    );

    assert_eq!(
        outcome,
        Err(AttestationRefusal::Rejected {
            digest: tampered,
            rejections: vec![BundleRejection::Unverified {
                detail: "Verification error: artifact hash does not match any subject in \
                         attestation"
                    .to_owned(),
            }],
        })
    );
}

#[test]
fn a_bundle_for_a_different_subject_is_refused() {
    // Signed by the stable workflow on the stable tag the provenance trusts,
    // but its subject is the v0.0.74 archive, not the beta archive.
    let (outcome, _) = verify_served(
        BETA_ARCHIVE,
        Some(listing(&[bundle(STABLE_BUNDLE)])),
        "v0.0.74",
        false,
    );

    assert_eq!(
        outcome,
        Err(AttestationRefusal::Rejected {
            digest: BETA_ARCHIVE.to_owned(),
            rejections: vec![BundleRejection::Unverified {
                detail: "Verification error: artifact hash does not match any subject in \
                         attestation"
                    .to_owned(),
            }],
        })
    );
}

#[test]
fn a_bundle_from_another_release_workflow_is_refused() {
    let stable_listing = listing(&[bundle(STABLE_BUNDLE)]);

    let (on_its_channel, _) = verify_served(
        STABLE_ARCHIVE,
        Some(stable_listing.clone()),
        "v0.0.74",
        false,
    );
    let (on_the_beta_channel, _) =
        verify_served(STABLE_ARCHIVE, Some(stable_listing), "v0.0.74", true);

    assert_eq!(on_its_channel, Ok(STABLE_SIGNER.to_owned()));
    assert_eq!(
        on_the_beta_channel,
        Err(AttestationRefusal::Rejected {
            digest: STABLE_ARCHIVE.to_owned(),
            rejections: vec![BundleRejection::UntrustedSigner {
                identity: STABLE_SIGNER.to_owned(),
            }],
        })
    );
}

#[test]
fn a_bundle_from_another_repository_is_refused() {
    let server = FakeGitHub::bind();
    let base = server.base.clone();
    let stored = format!("{base}/storage/gh.json.sn");
    let body = json!({ "attestations": [{ "bundle": null, "bundle_url": stored }] });
    let requests = server.serve(vec![
        (
            attestations_path(OTHER_REPOSITORY_ARCHIVE),
            body.to_string().into_bytes(),
        ),
        (
            "/storage/gh.json.sn".to_owned(),
            OTHER_REPOSITORY_BUNDLE.to_vec(),
        ),
    ]);
    let provenance = ReleaseProvenance::for_release(&base, None, "v1.0.0-beta.59", true);

    let outcome = verify_release_attestation(&provenance, digest(OTHER_REPOSITORY_ARCHIVE));

    assert_eq!(
        outcome,
        Err(AttestationRefusal::Rejected {
            digest: OTHER_REPOSITORY_ARCHIVE.to_owned(),
            rejections: vec![BundleRejection::UntrustedSigner {
                identity:
                    "https://github.com/cli/cli/.github/workflows/deployment.yml@refs/heads/trunk"
                        .to_owned(),
            }],
        })
    );
    assert_eq!(
        *requests.lock().unwrap(),
        [
            attestations_path(OTHER_REPOSITORY_ARCHIVE),
            "/storage/gh.json.sn".to_owned()
        ]
    );
}

#[test]
fn a_bundle_with_a_broken_signature_is_refused() {
    let (outcome, _) = verify_served(
        BETA_ARCHIVE,
        Some(listing(&[beta_bundle_with_broken_signature()])),
        "v1.0.0-beta.59",
        true,
    );

    assert_eq!(
        outcome,
        Err(AttestationRefusal::Rejected {
            digest: BETA_ARCHIVE.to_owned(),
            rejections: vec![BundleRejection::Unverified {
                detail: "Verification error: DSSE signature verification failed: Verification \
                         error: ECDSA P-256 SHA-256 signature invalid"
                    .to_owned(),
            }],
        })
    );
}

#[test]
fn one_valid_release_attestation_among_rejected_ones_is_accepted() {
    let (outcome, _) = verify_served(
        BETA_ARCHIVE,
        Some(listing(&[
            beta_bundle_with_broken_signature(),
            bundle(BETA_BUNDLE),
        ])),
        "v1.0.0-beta.59",
        true,
    );

    assert_eq!(outcome, Ok(BETA_SIGNER.to_owned()));
}
