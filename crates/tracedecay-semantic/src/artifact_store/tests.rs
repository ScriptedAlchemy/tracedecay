#[cfg(test)]
mod tests {
    use super::super::manifest::*;
    use super::*;
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Duration;

    const NOW: u64 = 1_500;

    fn model_bytes() -> Vec<u8> {
        b"deterministic fake model weights".to_vec()
    }

    fn member_bytes(role: ArtifactMemberRoleV1, model: &[u8]) -> &[u8] {
        match role {
            ArtifactMemberRoleV1::Model => model,
            ArtifactMemberRoleV1::Tokenizer => b"tokenizer",
            ArtifactMemberRoleV1::Config => b"config",
            ArtifactMemberRoleV1::SpecialTokensMap => b"{}",
            ArtifactMemberRoleV1::TokenizerConfig => {
                br#"{"model_max_length": 512, "pad_token": "[PAD]"}"#
            }
            ArtifactMemberRoleV1::QueryInstruction | ArtifactMemberRoleV1::DocumentInstruction => {
                unreachable!()
            }
        }
    }

    fn manifest_for(bytes: &[u8]) -> ModelArtifactManifestV1 {
        let payload = ModelArtifactManifestPayloadV1 {
            schema: MODEL_ARTIFACT_MANIFEST_SCHEMA_V1.to_string(),
            artifact_id: "test-embed".to_string(),
            profile_kind: ArtifactProfileKindV1::Embedding,
            spdx_license: "MIT".to_string(),
            model_member: ArtifactMemberPinV1 {
                digest: Sha256DigestHex::of_bytes(bytes),
                byte_length: bytes.len() as u64,
            },
            tokenizer_digest: Sha256DigestHex::of_bytes(b"tokenizer"),
            config_digest: Sha256DigestHex::of_bytes(b"config"),
            query_instruction_digest: None,
            document_instruction_digest: None,
            members: vec![
                ArtifactPackageMemberV1 {
                    role: ArtifactMemberRoleV1::Model,
                    path: "model.onnx".to_string(),
                    digest: Sha256DigestHex::of_bytes(bytes),
                    byte_length: bytes.len() as u64,
                },
                ArtifactPackageMemberV1 {
                    role: ArtifactMemberRoleV1::Tokenizer,
                    path: "tokenizer.json".to_string(),
                    digest: Sha256DigestHex::of_bytes(b"tokenizer"),
                    byte_length: b"tokenizer".len() as u64,
                },
                ArtifactPackageMemberV1 {
                    role: ArtifactMemberRoleV1::Config,
                    path: "config.json".to_string(),
                    digest: Sha256DigestHex::of_bytes(b"config"),
                    byte_length: b"config".len() as u64,
                },
            ],
            dimensions: 384,
            metric: SemanticMetricV1::Cosine,
            normalization: EmbeddingNormalizationV1::L2,
            pooling: EmbeddingPoolingV1::Mean,
            truncation: TruncationPolicyV1 {
                side: TruncationSideV1::Right,
                max_length: 512,
            },
            precision: EmbeddingPrecisionV1::Fp32,
            runtime: RuntimeCompatibilityV1 {
                runtime: "fastembed-ort".to_string(),
                build_revision: "rev-1".to_string(),
                platforms: vec![PlatformTargetV1 {
                    os: "linux".to_string(),
                    arch: "x86_64".to_string(),
                }],
            },
            device: DeviceClassV1::Cpu,
            resource_ceiling: ResourceCeilingV1 {
                max_model_bytes: 1_000_000,
                max_tokenizer_bytes: 100_000,
                max_resident_bytes: 1_000_000_000,
                max_threads: 4,
                max_batch_size: 32,
                max_sequence_length: 512,
                load_deadline_ms: 30_000,
            },
            upstream: UpstreamSourceV1 {
                name: "test/model".to_string(),
                version: "1".to_string(),
                revision: "r1".to_string(),
            },
        };
        ModelArtifactManifestV1 { payload }
    }

    fn env() -> RuntimeEnvironmentV1 {
        RuntimeEnvironmentV1 {
            os: "linux".to_string(),
            arch: "x86_64".to_string(),
            runtime: "fastembed-ort".to_string(),
            build_revision: "rev-1".to_string(),
            available_resident_bytes: 2_000_000_000,
            available_threads: 8,
        }
    }

    fn store() -> (tempfile::TempDir, ModelArtifactStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ModelArtifactStore::open(
            dir.path().join("store"),
            RetentionPolicyV1 { grace_seconds: 100 },
        )
        .unwrap();
        (dir, store)
    }

    fn import_ok(
        store: &ModelArtifactStore,
        bytes: &[u8],
    ) -> (ModelArtifactManifestV1, Sha256DigestHex) {
        let manifest = manifest_for(bytes);
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        store.stage_chunk(&mut session, bytes, NOW).unwrap();
        for role in [
            ArtifactMemberRoleV1::Tokenizer,
            ArtifactMemberRoleV1::Config,
        ] {
            store
                .stage_member_chunk(&mut session, role, member_bytes(role, bytes), NOW)
                .unwrap();
        }
        let record = store.finalize_import(session, &manifest, NOW).unwrap();
        assert_eq!(record.state, ArtifactInventoryStateV1::Installed);
        (manifest, record.artifact_digest)
    }

    fn write_local_package(root: &Path, manifest: &ModelArtifactManifestV1, model: &[u8]) {
        for member in &manifest.payload.members {
            let path = root.join(&member.path);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, member_bytes(member.role, model)).unwrap();
        }
    }

    struct FixtureHttpsTransport {
        members: BTreeMap<String, Vec<u8>>,
        revision: String,
    }

    impl ExplicitHttpsArtifactTransportV1 for FixtureHttpsTransport {
        fn fetch_range(
            &self,
            request: &HttpsArtifactRangeRequestV1,
        ) -> Result<HttpsArtifactRangeResponseV1, ArtifactImportErrorV1> {
            let bytes = self
                .members
                .iter()
                .find_map(|(path, bytes)| request.url.ends_with(path).then_some(bytes))
                .ok_or(ArtifactImportErrorV1::MemberMismatch)?;
            let start = usize::try_from(request.offset)
                .map_err(|_| ArtifactImportErrorV1::ImmutableRangeMismatch)?;
            let count = usize::try_from(request.max_bytes)
                .map_err(|_| ArtifactImportErrorV1::ImmutableRangeMismatch)?;
            let end = start.saturating_add(count).min(bytes.len());
            Ok(HttpsArtifactRangeResponseV1 {
                offset: request.offset,
                total_length: bytes.len() as u64,
                immutable_revision: self.revision.clone(),
                bytes: bytes[start..end].to_vec(),
            })
        }
    }

    #[test]
    fn verified_manifest_import_places_atomically_and_admits() {
        let (_dir, store) = store();
        let (manifest, digest) = import_ok(&store, &model_bytes());
        let admitted = store
            .admit_for_runtime(&digest, &manifest, &env(), NOW)
            .unwrap();
        assert_eq!(admitted.artifact_digest(), &digest);
        assert!(store.artifact_path(&digest).exists());
        // Staging drained; layout is digest-addressed.
        assert_eq!(
            std::fs::read_dir(store.root.join("staging"))
                .unwrap()
                .count(),
            0
        );
        assert_eq!(
            std::fs::read(store.artifact_path(&digest)).unwrap(),
            model_bytes()
        );
    }

    #[test]
    fn explicit_local_directory_import_rejects_undeclared_members() {
        let (root, store) = store();
        let model = model_bytes();
        let manifest = manifest_for(&model);
        let package = root.path().join("package");
        write_local_package(&package, &manifest, &model);
        fs::write(package.join("undeclared.bin"), b"no").unwrap();
        assert_eq!(
            store
                .import_local_directory(&manifest, &package, NOW)
                .unwrap_err(),
            ArtifactImportErrorV1::UndeclaredMember
        );
        assert_eq!(
            store
                .inventory()
                .unwrap()
                .records
                .get(&manifest.artifact_identity_digest().to_string())
                .unwrap()
                .state,
            ArtifactInventoryStateV1::Quarantined
        );
    }

    #[test]
    fn explicit_https_import_uses_only_pinned_ranges() {
        let (_root, store) = store();
        let model = model_bytes();
        let manifest = manifest_for(&model);
        assert_eq!(
            ConfiguredHttpsArtifactSourceV1::new("http://models.example/rev", "immutable-r1")
                .unwrap_err(),
            ArtifactImportErrorV1::InvalidHttpsSource
        );
        let source =
            ConfiguredHttpsArtifactSourceV1::new("https://models.example/rev", "immutable-r1")
                .unwrap();
        let transport = FixtureHttpsTransport {
            members: manifest
                .payload
                .members
                .iter()
                .map(|member| {
                    (
                        member.path.clone(),
                        member_bytes(member.role, &model).to_vec(),
                    )
                })
                .collect(),
            revision: "immutable-r1".to_owned(),
        };
        let record = store
            .import_configured_https(&manifest, &source, &transport, None, NOW)
            .unwrap();
        assert_eq!(record.state, ArtifactInventoryStateV1::Installed);
    }

    #[test]
    fn daemon_gc_lease_never_collects_active_artifacts() {
        let (_root, store) = store();
        let (_manifest, digest) = import_ok(&store, &model_bytes());
        store
            .acquire_artifact_lease(
                &digest,
                ArtifactLeaseV1 {
                    lease_id: "active".to_owned(),
                    kind: ArtifactLeaseKindV1::Active,
                    expires_at_unix: NOW + 1_000,
                },
                NOW,
            )
            .unwrap();
        let daemon = store
            .acquire_daemon_gc_lease("daemon", NOW + 1_000, NOW)
            .unwrap();
        assert!(
            store
                .gc_with_daemon_lease(&daemon, NOW + 101)
                .unwrap()
                .is_empty()
        );
        store
            .release_artifact_lease(&digest, "active", ArtifactLeaseKindV1::Active)
            .unwrap();
        assert_eq!(
            store
                .gc_with_daemon_lease(&daemon, NOW + 102)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn daemon_gc_collects_only_the_superseded_rollback_after_rotation() {
        let (_root, store) = store();
        let (_first_manifest, first) = import_ok(&store, b"first verified model");
        store
            .activate_artifact_with_rollback(&first, "active", "rollback", NOW)
            .unwrap();
        let (_second_manifest, second) = import_ok(&store, b"second verified model");
        store
            .activate_artifact_with_rollback(&second, "active", "rollback", NOW + 1)
            .unwrap();
        let (_third_manifest, third) = import_ok(&store, b"third verified model");
        store
            .activate_artifact_with_rollback(&third, "active", "rollback", NOW + 2)
            .unwrap();

        let before_gc = store.inventory().unwrap();
        assert_eq!(
            before_gc.records.get(&first.to_string()).unwrap().state,
            ArtifactInventoryStateV1::Installed
        );
        assert_eq!(
            before_gc.records.get(&second.to_string()).unwrap().state,
            ArtifactInventoryStateV1::RetainedForRollback
        );
        assert_eq!(
            store
                .artifact_digest_for_lease("active", ArtifactLeaseKindV1::Active, NOW + 2)
                .unwrap(),
            Some(third.clone())
        );
        assert_eq!(
            store
                .artifact_digest_for_lease("rollback", ArtifactLeaseKindV1::Rollback, NOW + 2)
                .unwrap(),
            Some(second.clone())
        );

        let collected_at = NOW + 102;
        let daemon = store
            .acquire_daemon_gc_lease("daemon", collected_at + 1_000, collected_at)
            .unwrap();
        let receipts = store.gc_with_daemon_lease(&daemon, collected_at).unwrap();
        assert_eq!(
            receipts
                .iter()
                .map(|receipt| receipt.artifact_digest.clone())
                .collect::<Vec<_>>(),
            vec![first]
        );
        let after_gc = store.inventory().unwrap();
        assert!(after_gc.records.contains_key(&second.to_string()));
        assert!(after_gc.records.contains_key(&third.to_string()));
    }

    #[test]
    fn runtime_member_reader_rechecks_the_artifact_identity() {
        let (_dir, store) = store();
        let (manifest, digest) = import_ok(&store, &model_bytes());
        let admitted = store
            .admit_for_runtime(&digest, &manifest, &env(), NOW)
            .expect("admitted artifact");

        assert_eq!(
            admitted
                .read_member_bytes(ArtifactMemberRoleV1::Model)
                .expect("verified model bytes"),
            model_bytes()
        );

        std::fs::write(store.artifact_path(&digest), b"tampered model weights").unwrap();
        assert_eq!(
            admitted.read_member_bytes(ArtifactMemberRoleV1::Model),
            Err(AdmittedArtifactReadErrorV1::Corrupt)
        );
    }

    #[test]
    fn runtime_admission_rejects_tampered_inventory_record_digest() {
        let (_dir, store) = store();
        let (manifest, digest) = import_ok(&store, &model_bytes());
        let mut inventory = store.inventory().unwrap();
        inventory
            .records
            .get_mut(digest.as_str())
            .unwrap()
            .artifact_digest = Sha256DigestHex::of_bytes(b"tampered-record-digest");
        store.save_inventory(&inventory).unwrap();

        assert_eq!(
            store
                .admit_for_runtime(&digest, &manifest, &env(), NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::IdentityMismatch
        );
    }

    #[test]
    fn runtime_admission_rejects_tampered_inventory_map_key() {
        let (_dir, store) = store();
        let (manifest, digest) = import_ok(&store, &model_bytes());
        let tampered_key = Sha256DigestHex::of_bytes(b"tampered-map-key");
        let mut inventory = store.inventory().unwrap();
        let record = inventory.records.remove(digest.as_str()).unwrap();
        inventory.records.insert(tampered_key.to_string(), record);
        store.save_inventory(&inventory).unwrap();

        assert_eq!(
            store
                .admit_for_runtime(&tampered_key, &manifest, &env(), NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::IdentityMismatch
        );
    }

    #[test]
    fn corrupted_bytes_are_rejected_at_finalize_and_quarantined() {
        let (_dir, store) = store();
        let manifest = manifest_for(&model_bytes());
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        // Same length, different bytes -> digest mismatch.
        let mut corrupted = model_bytes();
        corrupted[0] ^= 0xFF;
        store.stage_chunk(&mut session, &corrupted, NOW).unwrap();
        assert!(matches!(
            store.finalize_import(session, &manifest, NOW).unwrap_err(),
            ArtifactImportErrorV1::DigestMismatch
        ));
        let inventory = store.inventory().unwrap();
        let record = inventory
            .records
            .get(&manifest.artifact_identity_digest().to_string())
            .unwrap();
        assert_eq!(record.state, ArtifactInventoryStateV1::Quarantined);
        assert!(
            !store
                .artifact_dir(&manifest.artifact_identity_digest())
                .exists()
        );
    }

    #[test]
    fn every_package_member_is_verified_before_installation() {
        let (_dir, store) = store();
        let bytes = model_bytes();
        let manifest = manifest_for(&bytes);
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        store.stage_chunk(&mut session, &bytes, NOW).unwrap();
        store
            .stage_member_chunk(
                &mut session,
                ArtifactMemberRoleV1::Tokenizer,
                member_bytes(ArtifactMemberRoleV1::Tokenizer, &bytes),
                NOW,
            )
            .unwrap();
        store
            .stage_member_chunk(&mut session, ArtifactMemberRoleV1::Config, b"confix", NOW)
            .unwrap();

        assert_eq!(
            store.finalize_import(session, &manifest, NOW).unwrap_err(),
            ArtifactImportErrorV1::DigestMismatch
        );
        let inventory = store.inventory().unwrap();
        assert_eq!(
            inventory
                .records
                .get(&manifest.artifact_identity_digest().to_string())
                .unwrap()
                .state,
            ArtifactInventoryStateV1::Quarantined
        );
    }

    #[test]
    fn wrong_length_and_size_expansion_are_rejected() {
        let (_dir, store) = store();
        let manifest = manifest_for(&model_bytes());

        // Short write -> length mismatch at finalize.
        let mut short = store.begin_import(&manifest, NOW).unwrap();
        store
            .stage_chunk(&mut short, &model_bytes()[..4], NOW)
            .unwrap();
        assert!(matches!(
            store.finalize_import(short, &manifest, NOW).unwrap_err(),
            ArtifactImportErrorV1::LengthMismatch
        ));

        // Over-long write -> size expansion rejected at stage time.
        let over_bytes = b"separate model for expansion".to_vec();
        let over_manifest = manifest_for(&over_bytes);
        let mut over = store.begin_import(&over_manifest, NOW).unwrap();
        let mut too_much = over_bytes;
        too_much.push(0);
        assert!(matches!(
            store.stage_chunk(&mut over, &too_much, NOW).unwrap_err(),
            ArtifactImportErrorV1::SizeExpansionBeyondDeclared
        ));
    }

    #[test]
    fn partial_write_resumes_and_places_atomically() {
        let (_dir, store) = store();
        let bytes = model_bytes();
        let manifest = manifest_for(&bytes);
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        let split = bytes.len() / 2;
        store
            .stage_chunk(&mut session, &bytes[..split], NOW)
            .unwrap();
        let staging_id = session.staging_id();
        assert_eq!(session.bytes_written(), split as u64);
        drop(session); // simulate interruption

        let mut resumed = store.resume_import(&manifest, &staging_id, NOW).unwrap();
        assert_eq!(resumed.bytes_written(), split as u64);
        store
            .stage_chunk(&mut resumed, &bytes[split..], NOW)
            .unwrap();
        for role in [
            ArtifactMemberRoleV1::Tokenizer,
            ArtifactMemberRoleV1::Config,
        ] {
            store
                .stage_member_chunk(&mut resumed, role, member_bytes(role, &bytes), NOW)
                .unwrap();
        }
        let record = store.finalize_import(resumed, &manifest, NOW).unwrap();
        assert_eq!(record.state, ArtifactInventoryStateV1::Installed);
        assert!(store.artifact_path(&record.artifact_digest).exists());
    }

    #[test]
    fn resume_with_mismatched_manifest_discards_staging() {
        let (_dir, store) = store();
        let bytes = model_bytes();
        let manifest = manifest_for(&bytes);
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        store.stage_chunk(&mut session, &bytes[..4], NOW).unwrap();
        let staging_id = session.staging_id();
        drop(session);

        let other = manifest_for(b"different model bytes");
        assert_eq!(
            store.resume_import(&other, &staging_id, NOW).unwrap_err(),
            ArtifactImportErrorV1::ResumeIdentityMismatch
        );
        assert!(!store.root.join("staging").join(&staging_id).exists());
    }

    #[test]
    fn resume_confines_opaque_staging_handles_without_leaking_them() {
        let (_dir, store) = store();
        let manifest = manifest_for(&model_bytes());
        let session = store.begin_import(&manifest, NOW).unwrap();
        let staging_id = session.staging_id();
        drop(session);

        let escaped = store.root.join("escaped-staging");
        std::fs::rename(store.root.join("staging").join(&staging_id), &escaped).unwrap();

        let traversal = "../escaped-staging";
        let error = store
            .resume_import(&manifest, traversal, NOW)
            .expect_err("a staging handle must not traverse outside staging");
        assert!(!error.to_string().contains(traversal));
        assert!(
            !error
                .to_string()
                .contains(&store.root.display().to_string())
        );
        assert!(
            escaped.exists(),
            "a rejected traversal must not delete data outside the staging root"
        );

        let opaque_handle = "not-a-valid-staging-handle";
        let error = store
            .resume_import(&manifest, opaque_handle, NOW)
            .expect_err("untrusted raw handle must be rejected");
        assert!(!error.to_string().contains(opaque_handle));
    }

    #[cfg(unix)]
    #[test]
    fn resume_does_not_follow_a_symlinked_staging_directory() {
        let (_dir, store) = store();
        let manifest = manifest_for(&model_bytes());
        let session = store.begin_import(&manifest, NOW).unwrap();
        let staging_id = session.staging_id();
        drop(session);

        let staging = store.root.join("staging").join(&staging_id);
        let escaped = store.root.join("escaped-staging");
        std::fs::rename(&staging, &escaped).unwrap();
        std::os::unix::fs::symlink(&escaped, &staging).unwrap();

        assert!(
            store.resume_import(&manifest, &staging_id, NOW).is_err(),
            "resuming must reject a staging path that resolves through a symlink"
        );
        assert!(escaped.exists());
    }

    #[cfg(unix)]
    #[test]
    fn recovery_reopens_staging_id_nofollow_after_enumeration_swap() {
        let (_dir, store) = store();
        let bytes = model_bytes();
        let manifest = manifest_for(&bytes);
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        store.stage_chunk(&mut session, &bytes, NOW).unwrap();
        for role in [
            ArtifactMemberRoleV1::Tokenizer,
            ArtifactMemberRoleV1::Config,
        ] {
            store
                .stage_member_chunk(&mut session, role, member_bytes(role, &bytes), NOW)
                .unwrap();
        }
        let staging_id = session.staging_id();
        let enumerated = store.staged_ids_locked().unwrap();
        let digest = manifest.artifact_identity_digest();
        let staging_root = store.root.join("staging");
        let original = staging_root.join(&staging_id);
        let held = staging_root.join("held-original");
        let replacement = staging_root.join("replacement");
        let members = session.staging_path.join("members");
        drop(session);
        std::fs::rename(members, store.artifact_dir(&digest)).unwrap();
        std::fs::rename(&original, &held).unwrap();
        std::fs::create_dir_all(replacement.join("members")).unwrap();
        std::fs::copy(
            held.join("import.meta.json"),
            replacement.join("import.meta.json"),
        )
        .unwrap();
        std::fs::write(replacement.join("sentinel"), b"replacement").unwrap();
        std::os::unix::fs::symlink("replacement", &original).unwrap();

        store.recover_staged_ids_locked(enumerated).unwrap();

        let inventory = store.inventory().unwrap();
        assert_eq!(
            inventory.records.get(digest.as_str()).unwrap().state,
            ArtifactInventoryStateV1::Staged
        );
        assert_eq!(
            std::fs::read(replacement.join("sentinel")).unwrap(),
            b"replacement"
        );
        assert!(held.exists());
    }

    #[cfg(unix)]
    #[test]
    fn held_staging_component_ignores_ambient_component_replacement() {
        let (_dir, store) = store();
        let held = store.root.join("staging-held");
        let outside = store.root.join("outside-staging");
        std::fs::rename(store.root.join("staging"), &held).unwrap();
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("sentinel"), b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, store.root.join("staging")).unwrap();

        let manifest = manifest_for(&model_bytes());
        let session = store.begin_import(&manifest, NOW).unwrap();
        assert!(held.join(session.staging_id()).exists());
        assert_eq!(std::fs::read(outside.join("sentinel")).unwrap(), b"outside");
    }

    #[cfg(unix)]
    #[test]
    fn held_root_capability_ignores_ambient_root_replacement() {
        let (dir, store) = store();
        let ambient_root = dir.path().join("store");
        let held_root = dir.path().join("store-held");
        let outside_root = dir.path().join("outside-root");
        std::fs::rename(&ambient_root, &held_root).unwrap();
        std::fs::create_dir(&outside_root).unwrap();
        std::fs::write(outside_root.join("sentinel"), b"outside").unwrap();
        std::os::unix::fs::symlink(&outside_root, &ambient_root).unwrap();

        store
            .save_inventory(&ArtifactInventoryV1::default())
            .unwrap();
        assert!(held_root.join("inventory.json").exists());
        assert_eq!(
            std::fs::read(outside_root.join("sentinel")).unwrap(),
            b"outside"
        );
        assert!(!outside_root.join("inventory.json").exists());
    }

    #[cfg(unix)]
    #[test]
    fn held_import_session_ignores_ambient_session_replacement() {
        let (_dir, store) = store();
        let bytes = model_bytes();
        let manifest = manifest_for(&bytes);
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        let ambient = store.root.join("staging").join(session.staging_id());
        let held = store.root.join("held-session");
        let outside = store.root.join("outside-session");
        std::fs::rename(&ambient, &held).unwrap();
        std::fs::create_dir_all(outside.join("members")).unwrap();
        std::fs::write(outside.join("members").join("model.onnx"), b"outside").unwrap();
        std::os::unix::fs::symlink(&outside, &ambient).unwrap();

        store.stage_chunk(&mut session, &bytes, NOW).unwrap();
        assert_eq!(
            std::fs::read(outside.join("members").join("model.onnx")).unwrap(),
            b"outside"
        );
        assert_eq!(
            std::fs::read(held.join("members").join("model.onnx")).unwrap(),
            bytes
        );
    }

    #[cfg(unix)]
    #[test]
    fn held_artifact_and_receipt_components_preserve_replacement_sentinels() {
        let (_dir, store) = store();
        let manifest = manifest_for(b"collectible component race");
        let record = store.record_for(&manifest, ArtifactInventoryStateV1::Verified, NOW, None);
        let digest = record.artifact_digest.clone();
        std::fs::create_dir_all(store.artifact_dir(&digest)).unwrap();
        let mut inventory = store.inventory().unwrap();
        inventory.records.insert(digest.to_string(), record);
        store.save_inventory(&inventory).unwrap();

        let held_artifacts = store.root.join("artifacts-held");
        let outside_artifacts = store.root.join("outside-artifacts");
        std::fs::rename(store.root.join("artifacts"), &held_artifacts).unwrap();
        std::fs::create_dir_all(outside_artifacts.join(digest.as_str())).unwrap();
        std::fs::write(
            outside_artifacts.join(digest.as_str()).join("sentinel"),
            b"artifact-outside",
        )
        .unwrap();
        std::os::unix::fs::symlink(&outside_artifacts, store.root.join("artifacts")).unwrap();

        let held_receipts = store.root.join("receipts-held");
        let outside_receipts = store.root.join("outside-receipts");
        std::fs::rename(store.root.join("receipts"), &held_receipts).unwrap();
        std::fs::create_dir(&outside_receipts).unwrap();
        std::fs::write(outside_receipts.join("sentinel"), b"receipt-outside").unwrap();
        std::os::unix::fs::symlink(&outside_receipts, store.root.join("receipts")).unwrap();

        assert_eq!(store.gc(NOW + 150).unwrap().len(), 1);
        assert_eq!(
            std::fs::read(outside_artifacts.join(digest.as_str()).join("sentinel")).unwrap(),
            b"artifact-outside"
        );
        assert_eq!(
            std::fs::read(outside_receipts.join("sentinel")).unwrap(),
            b"receipt-outside"
        );
        assert!(held_receipts.join("gc.jsonl").exists());
    }

    #[cfg(windows)]
    #[test]
    fn windows_component_handles_block_namespace_replacement() {
        let (_dir, store) = store();
        let replacement = store.root.join("replacement-staging");
        std::fs::create_dir(&replacement).unwrap();
        std::fs::write(replacement.join("sentinel"), b"outside").unwrap();

        assert!(
            std::fs::rename(store.root.join("staging"), store.root.join("staging-held")).is_err(),
            "the held Windows component handle must deny replacement"
        );
        assert_eq!(
            std::fs::read(replacement.join("sentinel")).unwrap(),
            b"outside"
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_inventory_replace_existing_is_write_through_and_leaves_no_temp() {
        let (_dir, store) = store();
        let first = ArtifactInventoryV1::default();
        store.save_inventory(&first).unwrap();

        let manifest = manifest_for(b"windows replacement");
        let record = store.record_for(&manifest, ArtifactInventoryStateV1::Verified, NOW, None);
        let mut second = ArtifactInventoryV1::default();
        second
            .records
            .insert(record.artifact_digest.to_string(), record);
        store.save_inventory(&second).unwrap();

        assert_eq!(store.inventory().unwrap(), second);
        assert!(std::fs::read_dir(&store.root).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")
        }));
    }

    #[test]
    fn reopening_recovers_an_install_interrupted_after_payload_rename() {
        let (dir, store) = store();
        let store_root = dir.path().join("store");
        let bytes = model_bytes();
        let manifest = manifest_for(&bytes);
        let mut session = store.begin_import(&manifest, NOW).unwrap();
        store.stage_chunk(&mut session, &bytes, NOW).unwrap();
        for role in [
            ArtifactMemberRoleV1::Tokenizer,
            ArtifactMemberRoleV1::Config,
        ] {
            store
                .stage_member_chunk(&mut session, role, member_bytes(role, &bytes), NOW)
                .unwrap();
        }
        let staging_id = session.staging_id();
        let digest = manifest.artifact_identity_digest();
        let members_path = session.staging_path.join("members");
        drop(session);
        std::fs::rename(members_path, store.artifact_dir(&digest)).unwrap();
        drop(store);

        let reopened =
            ModelArtifactStore::open(store_root, RetentionPolicyV1 { grace_seconds: 100 }).unwrap();
        let inventory = reopened.inventory().unwrap();
        let record = inventory
            .records
            .get(&digest.to_string())
            .expect("recovery must publish the renamed verified payload");
        assert_eq!(record.state, ArtifactInventoryStateV1::Installed);
        assert!(!reopened.root.join("staging").join(staging_id).exists());
    }

    #[test]
    fn reopening_finishes_a_serialized_gc_transaction() {
        let (dir, store) = store();
        let store_root = dir.path().join("store");
        let manifest = manifest_for(b"interrupted gc");
        let record = store.record_for(&manifest, ArtifactInventoryStateV1::Verified, NOW, None);
        let digest = record.artifact_digest.clone();
        std::fs::create_dir_all(store.artifact_dir(&digest)).unwrap();
        let mut inventory = store.inventory().unwrap();
        inventory.records.insert(digest.to_string(), record.clone());
        store.save_inventory(&inventory).unwrap();

        let journal_path = store.root.join(".artifact-store-recovery.json");
        let journal = serde_json::json!({
            "schema": "tracedecay.artifact-store-recovery.v1",
            "operation": "gc",
            "recorded_at_unix": NOW + 150,
            "records": [serde_json::to_value(&record).unwrap()],
        });
        std::fs::remove_dir_all(store.artifact_dir(&digest)).unwrap();
        std::fs::write(&journal_path, serde_json::to_vec(&journal).unwrap()).unwrap();
        drop(store);

        let reopened =
            ModelArtifactStore::open(store_root, RetentionPolicyV1 { grace_seconds: 100 }).unwrap();
        assert!(reopened.inventory().unwrap().records.is_empty());
        assert!(!journal_path.exists());
        let receipts =
            std::fs::read_to_string(reopened.root.join("receipts").join("gc.jsonl")).unwrap();
        assert_eq!(receipts.lines().count(), 1);
    }

    #[test]
    fn gc_recovery_completes_every_receipt_crash_phase_and_clears_journal() {
        for phase in 0..4 {
            let dir = tempfile::tempdir().unwrap();
            let store_root = dir.path().join("store");
            let store =
                ModelArtifactStore::open(&store_root, RetentionPolicyV1 { grace_seconds: 100 })
                    .unwrap();
            let manifest = manifest_for(format!("gc crash phase {phase}").as_bytes());
            let record = store.record_for(&manifest, ArtifactInventoryStateV1::Verified, NOW, None);
            let digest = record.artifact_digest.clone();
            std::fs::create_dir_all(store.artifact_dir(&digest)).unwrap();
            let mut inventory = store.inventory().unwrap();
            inventory.records.insert(digest.to_string(), record.clone());
            store.save_inventory(&inventory).unwrap();
            let journal = RecoveryJournalV1 {
                schema: RECOVERY_SCHEMA_V1.to_string(),
                action: RecoveryActionV1::Gc {
                    recorded_at_unix: NOW + 150,
                    records: vec![record.clone()],
                },
            };
            std::fs::write(store.recovery_path(), serde_json::to_vec(&journal).unwrap()).unwrap();

            if phase >= 1 {
                std::fs::remove_dir_all(store.artifact_dir(&digest)).unwrap();
            }
            if phase >= 2 {
                inventory.records.remove(digest.as_str());
                std::fs::write(
                    store.inventory_path(),
                    serde_json::to_vec(&inventory).unwrap(),
                )
                .unwrap();
            }
            if phase >= 3 {
                let receipt = GcReceiptV1 {
                    artifact_digest: digest.clone(),
                    removed_at_unix: NOW + 150,
                    prior_state: ArtifactInventoryStateV1::Verified,
                };
                std::fs::write(
                    store.root.join("receipts").join("gc.jsonl"),
                    format!("{}\n", serde_json::to_string(&receipt).unwrap()),
                )
                .unwrap();
            }
            drop(store);

            let reopened =
                ModelArtifactStore::open(&store_root, RetentionPolicyV1 { grace_seconds: 100 })
                    .unwrap();
            assert!(reopened.inventory().unwrap().records.is_empty());
            assert!(!reopened.recovery_path().exists());
            let receipts =
                std::fs::read_to_string(reopened.root.join("receipts").join("gc.jsonl")).unwrap();
            assert_eq!(receipts.lines().count(), 1, "crash phase {phase}");
        }
    }

    #[test]
    fn gc_recovery_discards_torn_receipt_tail_before_replay() {
        let (dir, store) = store();
        let store_root = dir.path().join("store");
        let manifest = manifest_for(b"torn receipt");
        let record = store.record_for(&manifest, ArtifactInventoryStateV1::Verified, NOW, None);
        let digest = record.artifact_digest.clone();
        let old_receipt = GcReceiptV1 {
            artifact_digest: Sha256DigestHex::of_bytes(b"old receipt"),
            removed_at_unix: NOW,
            prior_state: ArtifactInventoryStateV1::Verified,
        };
        std::fs::write(
            store.root.join("receipts").join("gc.jsonl"),
            format!(
                "{}\n{{\"artifact_digest\":",
                serde_json::to_string(&old_receipt).unwrap()
            ),
        )
        .unwrap();
        let journal = RecoveryJournalV1 {
            schema: RECOVERY_SCHEMA_V1.to_string(),
            action: RecoveryActionV1::Gc {
                recorded_at_unix: NOW + 150,
                records: vec![record],
            },
        };
        std::fs::write(store.recovery_path(), serde_json::to_vec(&journal).unwrap()).unwrap();
        drop(store);

        let reopened =
            ModelArtifactStore::open(store_root, RetentionPolicyV1 { grace_seconds: 100 }).unwrap();
        let receipts =
            std::fs::read_to_string(reopened.root.join("receipts").join("gc.jsonl")).unwrap();
        assert_eq!(receipts.lines().count(), 2);
        assert!(receipts.ends_with('\n'));
        assert!(receipts.contains(digest.as_str()));
        assert!(!reopened.recovery_path().exists());
    }

    #[cfg(unix)]
    #[test]
    fn receipt_recovery_atomically_replaces_existing_namespace_entry() {
        use std::os::unix::fs::MetadataExt;

        let (dir, store) = store();
        let store_root = dir.path().join("store");
        let manifest = manifest_for(b"atomic receipt replacement");
        let record = store.record_for(&manifest, ArtifactInventoryStateV1::Verified, NOW, None);
        let receipt_path = store.root.join("receipts").join("gc.jsonl");
        std::fs::write(&receipt_path, b"").unwrap();
        let old_inode = std::fs::metadata(&receipt_path).unwrap().ino();
        let journal = RecoveryJournalV1 {
            schema: RECOVERY_SCHEMA_V1.to_string(),
            action: RecoveryActionV1::Gc {
                recorded_at_unix: NOW + 150,
                records: vec![record],
            },
        };
        std::fs::write(store.recovery_path(), serde_json::to_vec(&journal).unwrap()).unwrap();
        drop(store);

        let reopened =
            ModelArtifactStore::open(store_root, RetentionPolicyV1 { grace_seconds: 100 }).unwrap();
        assert_ne!(std::fs::metadata(&receipt_path).unwrap().ino(), old_inode);
        assert!(!reopened.recovery_path().exists());
    }

    #[test]
    fn inventory_operations_wait_for_the_store_transaction_lock() {
        let (_dir, store) = store();
        let store = Arc::new(store);
        let worker_store = Arc::clone(&store);
        let guard = store.acquire_lock().unwrap();
        let (sent, received) = mpsc::channel();

        let worker = thread::spawn(move || {
            sent.send(worker_store.inventory().is_ok()).unwrap();
        });
        assert!(
            received.recv_timeout(Duration::from_millis(50)).is_err(),
            "a concurrent inventory read must wait for the transaction lock"
        );
        drop(guard);
        assert!(received.recv_timeout(Duration::from_secs(1)).unwrap());
        worker.join().unwrap();
    }

    #[test]
    fn revoked_and_quarantined_artifacts_disable_semantics_without_substitution() {
        let (_dir, store) = store();
        let (manifest, digest) = import_ok(&store, &model_bytes());
        store.revoke_artifact(&digest, NOW).unwrap();
        assert_eq!(
            store
                .admit_for_runtime(&digest, &manifest, &env(), NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::RevokedArtifact
        );

        // Quarantined record (from a failed import) is never admitted.
        let quarantined_manifest = manifest_for(b"quarantined model");
        let mut session = store.begin_import(&quarantined_manifest, NOW).unwrap();
        store
            .stage_chunk(&mut session, b"junk bytes here", NOW)
            .unwrap();
        let _ = store.finalize_import(session, &quarantined_manifest, NOW);
        assert!(matches!(
            store
                .admit_for_runtime(
                    &quarantined_manifest.artifact_identity_digest(),
                    &quarantined_manifest,
                    &env(),
                    NOW
                )
                .unwrap_err(),
            SemanticCapabilityDisabledV1::QuarantinedArtifact
        ));
        assert_eq!(
            store.begin_import(&quarantined_manifest, NOW).unwrap_err(),
            ArtifactImportErrorV1::StagingUnavailable,
            "quarantine is evidence, not an implicit retry or replacement"
        );
    }

    #[test]
    fn incompatible_platform_runtime_and_ceiling_disable_semantics() {
        let (_dir, store) = store();
        let (manifest, digest) = import_ok(&store, &model_bytes());

        let mut bad_platform = env();
        bad_platform.arch = "aarch64".to_string();
        assert!(matches!(
            store
                .admit_for_runtime(&digest, &manifest, &bad_platform, NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::IncompatiblePlatform
        ));

        let mut wrong_os = env();
        wrong_os.os = "windows".to_string();
        assert!(matches!(
            store
                .admit_for_runtime(&digest, &manifest, &wrong_os, NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::IncompatiblePlatform
        ));

        let mut bad_runtime = env();
        bad_runtime.build_revision = "rev-2".to_string();
        assert!(matches!(
            store
                .admit_for_runtime(&digest, &manifest, &bad_runtime, NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::IncompatibleRuntime
        ));

        let mut low_memory = env();
        low_memory.available_resident_bytes = 10;
        assert!(matches!(
            store
                .admit_for_runtime(&digest, &manifest, &low_memory, NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::ResourceCeilingExceeded
        ));
    }

    #[test]
    fn digest_readmission_rejects_runtime_evidence_mismatch() {
        let (_dir, store) = store();
        let (_manifest, digest) = import_ok(&store, &model_bytes());
        let mut runtime = env();
        runtime.build_revision = "different-runtime-build".to_owned();

        assert_eq!(
            store
                .admit_for_runtime_by_digest(&digest, &runtime)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::IncompatibleRuntime
        );
    }

    #[test]
    fn runtime_build_revision_names_the_pinned_fastembed_version() {
        let manifest = include_str!("../../Cargo.toml");
        let pinned = manifest
            .lines()
            .find_map(|line| {
                let dependency = line.trim().strip_prefix("fastembed = ")?;
                let (_, rest) = dependency.split_once("version = \"=")?;
                rest.split_once('"').map(|(version, _)| version)
            })
            .expect("tracedecay-semantic must pin an exact fastembed version");
        assert!(
            FASTEMBED_RUNTIME_BUILD_REVISION_V1.starts_with(&format!("fastembed-{pinned}+")),
            "FASTEMBED_RUNTIME_BUILD_REVISION_V1 ({FASTEMBED_RUNTIME_BUILD_REVISION_V1}) must \
             record the exact pinned fastembed version ({pinned}); a runtime upgrade must bump \
             the recorded revision so projection keys replay"
        );
    }

    #[cfg(feature = "semantic-fastembed")]
    #[test]
    fn detected_fastembed_environment_uses_process_evidence() {
        let runtime = RuntimeEnvironmentV1::detect_fastembed_process().unwrap();

        assert_eq!(runtime.os, std::env::consts::OS);
        assert_eq!(runtime.arch, std::env::consts::ARCH);
        assert_eq!(runtime.runtime, FASTEMBED_RUNTIME_FAMILY_V1);
        assert_eq!(runtime.build_revision, FASTEMBED_RUNTIME_BUILD_REVISION_V1);
        assert!(runtime.available_resident_bytes > 0);
        assert!(runtime.available_threads > 0);
    }

    #[test]
    fn digest_readmission_rejects_insufficient_process_memory() {
        let (_dir, store) = store();
        let (_manifest, digest) = import_ok(&store, &model_bytes());
        let mut runtime = env();
        runtime.available_resident_bytes = 1;

        assert_eq!(
            store
                .admit_for_runtime_by_digest(&digest, &runtime)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::ResourceCeilingExceeded
        );
    }

    #[test]
    fn digest_readmission_rejects_insufficient_process_threads() {
        let (_dir, store) = store();
        let (_manifest, digest) = import_ok(&store, &model_bytes());
        let mut runtime = env();
        runtime.available_threads = 1;

        assert_eq!(
            store
                .admit_for_runtime_by_digest(&digest, &runtime)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::ResourceCeilingExceeded
        );
    }

    #[test]
    fn lease_rotation_never_resurrects_a_revoked_prior_active_artifact() {
        let (_dir, store) = store();
        let (_first_manifest, first) = import_ok(&store, &model_bytes());
        store
            .activate_artifact_with_rollback(&first, "active", "rollback", NOW)
            .unwrap();
        store.revoke_artifact(&first, NOW + 1).unwrap();
        let (_second_manifest, second) = import_ok(&store, b"second verified model");

        store
            .activate_artifact_with_rollback(&second, "active", "rollback", NOW + 2)
            .unwrap();

        let inventory = store.inventory().unwrap();
        assert_eq!(
            inventory.records.get(&first.to_string()).unwrap().state,
            ArtifactInventoryStateV1::Revoked
        );
        assert_eq!(
            store
                .artifact_digest_for_lease("rollback", ArtifactLeaseKindV1::Rollback, NOW + 2)
                .unwrap(),
            None
        );
    }

    #[test]
    fn corrupt_on_disk_bytes_disable_semantics_at_admission() {
        let (_dir, store) = store();
        let (manifest, digest) = import_ok(&store, &model_bytes());
        // Corrupt the placed bytes after import.
        std::fs::write(store.artifact_path(&digest), b"tampered").unwrap();
        assert_eq!(
            store
                .admit_for_runtime(&digest, &manifest, &env(), NOW)
                .unwrap_err(),
            SemanticCapabilityDisabledV1::CorruptArtifact
        );
    }

    #[test]
    fn gc_collects_unreferenced_past_grace_and_appends_receipt() {
        let (_dir, store) = store();
        // Seed an unreferenced Verified record directly.
        let manifest = manifest_for(b"orphan verified artifact");
        let record = store.record_for(&manifest, ArtifactInventoryStateV1::Verified, NOW, None);
        let digest = record.artifact_digest.clone();
        std::fs::create_dir_all(store.artifact_dir(&digest)).unwrap();
        let mut inventory = store.inventory().unwrap();
        inventory.records.insert(digest.to_string(), record);
        store.save_inventory(&inventory).unwrap();

        // Within grace: retained.
        assert!(store.gc(NOW + 50).unwrap().is_empty());
        assert!(store.artifact_dir(&digest).exists());

        // Past grace: collected with an append-only receipt.
        let receipts = store.gc(NOW + 150).unwrap();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0].artifact_digest, digest);
        assert_eq!(receipts[0].prior_state, ArtifactInventoryStateV1::Verified);
        assert!(!store.artifact_dir(&digest).exists());
        let log = std::fs::read_to_string(store.root.join("receipts").join("gc.jsonl")).unwrap();
        assert_eq!(log.lines().count(), 1);
        assert!(store.inventory().unwrap().records.is_empty());
    }

    #[test]
    fn gc_preserves_retained_revoked_and_installed() {
        let (_dir, store) = store();
        let (_manifest_a, _digest_a) = import_ok(&store, &model_bytes());
        let (manifest_b, digest_b) = import_ok(&store, b"second model bytes");
        store.retain_for_rollback(&digest_b, NOW).unwrap();

        // Revoked record (separate artifact) is evidence; not collected.
        let (_manifest_c, digest_c) = import_ok(&store, b"third model bytes");
        store.revoke_artifact(&digest_c, NOW).unwrap();

        let receipts = store.gc(NOW + 10_000).unwrap();
        assert!(receipts.is_empty());
        let inventory = store.inventory().unwrap();
        assert_eq!(inventory.records.len(), 3);
        // The rollback-retained artifact still admits after GC.
        let admitted = store
            .admit_for_runtime(&digest_b, &manifest_b, &env(), NOW)
            .unwrap();
        assert_eq!(admitted.artifact_digest(), &digest_b);
    }

    #[test]
    fn artifact_filesystem_boundary_uses_safe_capability_primitives() {
        // artifact_store.rs is a thin facade over crate-private submodules
        // (paths/import/lease_admission/gc_recovery/io_primitives); scan all
        // of them so this guardrail still covers the whole store, not just
        // the facade file.
        let production = [
            include_str!("../artifact_store.rs"),
            include_str!("../artifact_store/paths.rs"),
            include_str!("../artifact_store/import.rs"),
            include_str!("../artifact_store/lease_admission.rs"),
            include_str!("../artifact_store/gc_recovery.rs"),
            include_str!("../artifact_store/io_primitives.rs"),
        ]
        .join("\n");
        assert!(production.contains("#![forbid(unsafe_code)]"));
        assert!(!production.contains("unsafe extern"));
        assert!(!production.contains("Dir::open_ambient_dir(&root"));
        assert!(!production.contains("entry.open_dir()"));
        assert!(production.contains("open_dir_nofollow(&staging_id)"));
        assert!(production.contains("fsys::quick::write"));
    }
}
