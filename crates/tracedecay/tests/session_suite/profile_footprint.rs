use std::collections::BTreeMap;
use std::path::Path;

use tempfile::TempDir;
use tracedecay_contracts::doctor::{
    DoctorEvidenceStateV1, DoctorStorageFamilyReadV1, DoctorStorageFindingKindV1,
    DoctorStorageFindingV1,
};
use tracedecay_maintenance::retention::footprint::{
    ProfileFootprintV1, census_profile_footprint, footprint_findings,
};
use tracedecay_project::test_support::host_admission::HostAdmissionTestRuntimeV1;
use tracedecay_runtime_core::text::format_bytes;
use tracedecay_sessions::admission::HostAdmissionScope;

use crate::common::{lcm_payload_message, lcm_payload_session};

const PAYLOAD_KEY: &str = "lcm-payloads";
const SESSIONS_KEY: &str = "user-sessions.db";
const PAYLOAD_BOUND_BYTES: u64 = 1024 * 1024;
const PAYLOAD_FILLER_CHARS: usize = 260 * 1024;
const PAYLOADS_TO_INGEST: usize = 6;

fn census(profile: &Path, overrides: &BTreeMap<String, u64>) -> ProfileFootprintV1 {
    census_profile_footprint(profile, &BTreeMap::new(), overrides)
}

fn entry_bytes(footprint: &ProfileFootprintV1, key: &str) -> Option<u64> {
    footprint
        .entries
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| entry.bytes)
}

fn finding_for(footprint: &ProfileFootprintV1, key: &str) -> Option<DoctorStorageFindingV1> {
    let DoctorStorageFamilyReadV1::Observed { findings } = footprint_findings(footprint) else {
        panic!("a footprint always observes its entries");
    };
    findings
        .into_iter()
        .find(|finding| finding.finding().evidence()[0].reference().as_str() == key)
}

fn directory_bytes(directory: &Path) -> u64 {
    std::fs::read_dir(directory)
        .expect("payload directory")
        .map(|entry| {
            entry
                .expect("payload entry")
                .metadata()
                .expect("metadata")
                .len()
        })
        .sum()
}

#[tokio::test]
async fn session_ingest_growth_trips_the_payload_footprint_finding() {
    let tmp = TempDir::new().unwrap();
    let profile = tmp.path().join(".tracedecay");
    let overrides = BTreeMap::from([(PAYLOAD_KEY.to_owned(), PAYLOAD_BOUND_BYTES)]);

    let before = census(&profile, &overrides);
    let payloads_before = entry_bytes(&before, PAYLOAD_KEY).unwrap_or(0);
    assert!(payloads_before <= PAYLOAD_BOUND_BYTES);
    if let Some(finding) = finding_for(&before, PAYLOAD_KEY) {
        assert_ne!(finding.finding().state(), DoctorEvidenceStateV1::Degraded);
    }
    let sessions_before = entry_bytes(&before, SESSIONS_KEY).unwrap_or(0);

    let db = HostAdmissionTestRuntimeV1::profile(&profile)
        .await
        .expect("registered LCM test runtime");
    assert!(
        db.upsert_session_for_test(
            HostAdmissionScope::Profile,
            &lcm_payload_session("cursor", "session-1"),
        )
        .await
        .unwrap_or(false)
    );
    for index in 0..PAYLOADS_TO_INGEST {
        let content = format!(
            "payload-{index}-{}",
            char::from(b'a' + u8::try_from(index).unwrap())
                .to_string()
                .repeat(PAYLOAD_FILLER_CHARS)
        );
        assert!(tracedecay_lcm::security::should_externalize(
            "tool",
            Some("tool_result"),
            &content
        ));
        let message = lcm_payload_message(
            "cursor",
            &format!("message-{index}"),
            "session-1",
            "user",
            &content,
        );
        db.lcm_ingest_raw_message_for_test(HostAdmissionScope::Profile, &message)
            .await
            .expect("externalized payload ingests");
    }

    let after = census(&profile, &overrides);
    let payloads_after =
        entry_bytes(&after, PAYLOAD_KEY).expect("ingest wrote an lcm-payloads footprint entry");
    assert!(
        payloads_after > PAYLOAD_BOUND_BYTES,
        "{payloads_after} bytes should exceed the override"
    );
    assert_eq!(
        payloads_after,
        directory_bytes(&tracedecay_lcm::payload::payload_dir(&profile))
    );
    let sessions_after =
        entry_bytes(&after, SESSIONS_KEY).expect("ingest wrote the profile session store");
    assert!(
        sessions_after > sessions_before,
        "session store {sessions_before} -> {sessions_after} bytes"
    );

    let finding = finding_for(&after, PAYLOAD_KEY).expect("lcm-payloads finding");
    assert_eq!(finding.kind(), DoctorStorageFindingKindV1::OverBudgetStore);
    assert_eq!(finding.finding().state(), DoctorEvidenceStateV1::Degraded);
    let statement = finding.finding().coverage().statement();
    assert!(
        statement.contains(&format_bytes(payloads_after)),
        "{statement}"
    );
    assert!(
        statement.contains(&format_bytes(PAYLOAD_BOUND_BYTES)),
        "{statement}"
    );

    let default_bound = census(&profile, &BTreeMap::new());
    assert_eq!(
        entry_bytes(&default_bound, PAYLOAD_KEY),
        Some(payloads_after)
    );
    assert_eq!(
        finding_for(&default_bound, PAYLOAD_KEY)
            .expect("lcm-payloads finding")
            .finding()
            .state(),
        DoctorEvidenceStateV1::HealthyCompleteCoverage
    );
}
