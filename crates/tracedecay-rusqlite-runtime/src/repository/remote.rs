use rusqlite::{OptionalExtension, Savepoint, params};
use tracedecay_store::{RemoteObservationReplayWriteV1, RemoteWriterFenceInstallV1};

use super::support::{encode, invalid};

pub(super) fn verify_and_seed_writer_fence(
    savepoint: &Savepoint<'_>,
    write: &RemoteObservationReplayWriteV1,
) -> rusqlite::Result<()> {
    let writer_json = encode(&write.writer_fence)?;
    let capture_sequence = i64::try_from(write.capture_sequence)
        .map_err(|_| invalid("remote capture sequence exceeds SQLite INTEGER"))?;
    savepoint.execute(
        "INSERT INTO remote_writer_fences (
            authority_key, writer_fence_json, frontier_sequence, updated_at
         ) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(authority_key) DO NOTHING",
        params![
            write.authority_key.as_str(),
            writer_json,
            capture_sequence,
            write.captured_at.0,
        ],
    )?;
    let stored = savepoint.query_row(
        "SELECT writer_fence_json FROM remote_writer_fences WHERE authority_key = ?1",
        [write.authority_key.as_str()],
        |row| row.get::<_, String>(0),
    )?;
    if stored != writer_json {
        return Err(invalid("remote writer fence is stale"));
    }
    savepoint.execute(
        "UPDATE remote_writer_fences
         SET frontier_sequence = max(frontier_sequence, ?1), updated_at = max(updated_at, ?2)
         WHERE authority_key = ?3 AND writer_fence_json = ?4",
        params![
            capture_sequence,
            write.captured_at.0,
            write.authority_key.as_str(),
            writer_json,
        ],
    )?;
    Ok(())
}

pub(super) fn persist_remote_observation_event(
    savepoint: &Savepoint<'_>,
    write: &RemoteObservationReplayWriteV1,
) -> rusqlite::Result<()> {
    let writer_json = encode(&write.writer_fence)?;
    let enrollment_revision = i64::try_from(write.enrollment_revision)
        .map_err(|_| invalid("remote enrollment revision exceeds SQLite INTEGER"))?;
    let policy_revision = i64::try_from(write.policy_revision)
        .map_err(|_| invalid("remote policy revision exceeds SQLite INTEGER"))?;
    let capture_sequence = i64::try_from(write.capture_sequence)
        .map_err(|_| invalid("remote capture sequence exceeds SQLite INTEGER"))?;
    let inserted = savepoint.execute(
        "INSERT INTO remote_observation_events (
            event_id, frame_digest, enrollment_id, enrollment_revision, node_id,
            policy_revision, capture_sequence, previous_event_id, observation_id,
            writer_fence_json, captured_at, idempotency_key, command_digest
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13
         )
         ON CONFLICT(event_id) DO NOTHING",
        params![
            write.event_id.as_str(),
            write.frame_digest.as_str(),
            write.enrollment_id.as_str(),
            enrollment_revision,
            write.node_id.as_str(),
            policy_revision,
            capture_sequence,
            write.previous_event_id.as_deref(),
            write.observation.observation().observation_id().as_str(),
            writer_json,
            write.captured_at.0,
            write.event_id.as_str(),
            write.command_digest.as_str(),
        ],
    )?;
    if inserted != 0 {
        return Ok(());
    }
    let stored = savepoint
        .query_row(
            "SELECT frame_digest, enrollment_id, enrollment_revision, node_id,
                    policy_revision, capture_sequence, previous_event_id, observation_id,
                    writer_fence_json, captured_at, idempotency_key, command_digest
             FROM remote_observation_events WHERE event_id = ?1",
            [write.event_id.as_str()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, String>(10)?,
                    row.get::<_, String>(11)?,
                ))
            },
        )
        .optional()?;
    let expected = (
        write.frame_digest.as_str().to_owned(),
        write.enrollment_id.as_str().to_owned(),
        enrollment_revision,
        write.node_id.as_str().to_owned(),
        policy_revision,
        capture_sequence,
        write.previous_event_id.clone(),
        write
            .observation
            .observation()
            .observation_id()
            .as_str()
            .to_owned(),
        writer_json,
        write.captured_at.0,
        write.event_id.clone(),
        write.command_digest.as_str().to_owned(),
    );
    if stored.as_ref() != Some(&expected) {
        return Err(invalid("remote observation event identity collision"));
    }
    Ok(())
}

pub(super) fn install_writer_fence(
    savepoint: &Savepoint<'_>,
    install: &RemoteWriterFenceInstallV1,
) -> rusqlite::Result<()> {
    install
        .validate()
        .map_err(|error| invalid(&error.to_string()))?;
    let expected_json = encode(&install.expected)?;
    let replacement_json = encode(&install.replacement)?;
    let changed = savepoint.execute(
        "UPDATE remote_writer_fences
         SET writer_fence_json = ?1, updated_at = ?2
         WHERE authority_key = ?3 AND writer_fence_json = ?4",
        params![
            replacement_json,
            install.installed_at.0,
            install.authority_key.as_str(),
            expected_json,
        ],
    )?;
    if changed == 1 {
        return Ok(());
    }
    let stored = savepoint
        .query_row(
            "SELECT writer_fence_json
             FROM remote_writer_fences WHERE authority_key = ?1",
            [install.authority_key.as_str()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if stored.as_ref() == Some(&replacement_json) {
        Ok(())
    } else {
        Err(invalid("remote writer fence compare-and-swap failed"))
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tracedecay_domain::{ManifestDigest, RemoteWriterFenceV1, UtcMicros, canonical_sha256};

    use super::*;

    fn fence(epoch: u64, placement_revision: u64, node_id: &str) -> RemoteWriterFenceV1 {
        serde_json::from_value(json!({
            "brain_id": "brain.remote",
            "shard_id": "shard.remote",
            "generation_id": "generation.remote",
            "placement_revision": placement_revision,
            "authority_epoch": epoch,
            "authority_node_id": node_id,
        }))
        .unwrap()
    }

    fn authority_key() -> ManifestDigest {
        canonical_sha256(&(
            "tracedecay.remote-recovery-authority.v1",
            "brain.remote",
            "shard.remote",
            "generation.remote",
        ))
        .unwrap()
    }

    fn install() -> RemoteWriterFenceInstallV1 {
        RemoteWriterFenceInstallV1 {
            project_id: tracedecay_domain::ProjectId::new("project.remote").unwrap(),
            target_binding: serde_json::from_value(json!({
                "shard_id": {
                    "brain_id": "brain.local",
                    "profile_id": "profile.local",
                    "scope": {
                        "kind": "project_sessions",
                        "project_id": "project.remote"
                    }
                },
                "incarnation": 1,
                "authority_epoch": 1
            }))
            .unwrap(),
            authority_key: authority_key(),
            expected: fence(11, 1, "node.old"),
            replacement: fence(12, 2, "node.new"),
            installed_at: UtcMicros(20),
        }
    }

    fn connection() -> rusqlite::Connection {
        let connection = rusqlite::Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE remote_writer_fences (
                    authority_key TEXT PRIMARY KEY,
                    writer_fence_json TEXT NOT NULL CHECK(json_valid(writer_fence_json)),
                    frontier_sequence INTEGER NOT NULL CHECK(frontier_sequence >= 0),
                    updated_at INTEGER NOT NULL
                ) STRICT;",
            )
            .unwrap();
        connection
    }

    #[test]
    fn writer_fence_install_is_exactly_replayable() {
        let mut connection = connection();
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        let install = install();
        savepoint
            .execute(
                "INSERT INTO remote_writer_fences VALUES (?1, ?2, 7, 10)",
                rusqlite::params![
                    install.authority_key.as_str(),
                    encode(&install.expected).unwrap(),
                ],
            )
            .unwrap();

        install_writer_fence(&savepoint, &install).unwrap();
        install_writer_fence(&savepoint, &install).unwrap();

        let stored: (String, i64) = savepoint
            .query_row(
                "SELECT writer_fence_json, frontier_sequence FROM remote_writer_fences",
                (),
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(stored.0, encode(&install.replacement).unwrap());
        assert_eq!(stored.1, 7);
    }

    #[test]
    fn writer_fence_install_rejects_missing_authority_without_seeding() {
        let mut connection = connection();
        let mut transaction = connection.transaction().unwrap();
        let savepoint = transaction.savepoint().unwrap();
        let install = install();

        assert!(install_writer_fence(&savepoint, &install).is_err());
        let stored: i64 = savepoint
            .query_row("SELECT count(*) FROM remote_writer_fences", (), |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(stored, 0);
    }

    #[test]
    fn writer_fence_install_rejects_a_different_project_binding() {
        let mut install = install();
        install.target_binding = serde_json::from_value(json!({
            "shard_id": {
                "brain_id": "brain.local",
                "profile_id": "profile.local",
                "scope": {
                    "kind": "project_sessions",
                    "project_id": "project.other"
                }
            },
            "incarnation": 1,
            "authority_epoch": 1
        }))
        .unwrap();

        assert!(install.validate().is_err());
    }
}
