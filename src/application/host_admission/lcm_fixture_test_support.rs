use super::*;

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LcmLineageFaultForTest {
    CorruptCompatibilitySummaryText {
        node_id: String,
        text: String,
    },
    ShiftRawMessageTimestamp {
        store_id: i64,
        delta: i64,
    },
    DeleteGeneration {
        session_id: String,
        generation: i64,
    },
    ReplaceGenerationWatermarks {
        session_id: String,
        generation: i64,
        json: String,
    },
    DeleteAvailability {
        session_id: String,
        generation: i64,
        summary_id: String,
    },
    ReplaceAvailabilityHorizon {
        session_id: String,
        generation: i64,
        summary_id: String,
        source_horizon_json: String,
    },
    SetAvailability {
        session_id: String,
        generation: i64,
        summary_id: String,
        availability: String,
        reason: Option<String>,
    },
    SetGenerationFailed {
        session_id: String,
        generation: i64,
    },
    CorruptRetrievalAnchorOwner {
        summary_id: String,
    },
    ReplaceSummarySourceWithSummary {
        summary_id: String,
        ordinal: i64,
        source_summary_id: String,
    },
}

#[doc(hidden)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LcmLineageCountsForTest {
    pub active_generations: i64,
    pub total_generations: i64,
    pub summary_nodes: i64,
    pub summary_sources: i64,
    pub summary_successors: i64,
    pub cursor_keys: i64,
}

#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LcmExternalPayloadManifestTestRecord {
    pub payload_ref: String,
    pub session_id: String,
    pub payload_digest: String,
    pub manifest_json: String,
    pub receipt_id: String,
    pub created_at: i64,
    pub external_created_at: i64,
}

impl HostAdmissionTestRuntimeV1 {
    fn primary_lcm_fixture_database_for_test(&self) -> &RegisteredGlobalDb {
        self.project_registered
            .as_deref()
            .unwrap_or(self.profile_registered.as_ref())
    }

    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub async fn inject_lcm_orphan_summary_source_for_test(
        &self,
        scope: HostAdmissionScope,
        store_id: i64,
    ) -> Result<()> {
        let database = self.session_database_for_test(scope)?;
        let connection = rusqlite::Connection::open(database.db_path()).map_err(|error| {
            TraceDecayError::Database {
                operation: "open out-of-band orphan summary fixture".to_owned(),
                message: error.to_string(),
            }
        })?;
        connection
            .pragma_update(None, "foreign_keys", "OFF")
            .map_err(|error| TraceDecayError::Database {
                operation: "disable foreign keys out of band for orphan summary fixture".to_owned(),
                message: error.to_string(),
            })?;
        connection
            .execute(
                "INSERT INTO lcm_summary_sources(node_id, source_kind, source_id, ordinal)
                 VALUES ('missing-summary-owner', 'raw_message', ?1, 0)",
                [store_id.to_string()],
            )
            .map_err(|error| TraceDecayError::Database {
                operation: "insert orphan summary source fixture".to_owned(),
                message: error.to_string(),
            })?;
        Ok(())
    }

    #[doc(hidden)]
    #[cfg(any(test, feature = "test-transport"))]
    pub async fn inject_lcm_foreign_orphan_debt_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<()> {
        let database = self.session_database_for_test(scope)?;
        let connection = rusqlite::Connection::open(database.db_path()).map_err(|error| {
            TraceDecayError::Database {
                operation: "open out-of-band orphan debt fixture".to_owned(),
                message: error.to_string(),
            }
        })?;
        connection
            .pragma_update(None, "foreign_keys", "OFF")
            .map_err(|error| TraceDecayError::Database {
                operation: "disable foreign keys out of band for orphan debt fixture".to_owned(),
                message: error.to_string(),
            })?;
        connection
            .execute(
                "INSERT INTO lcm_maintenance_debt(
                    provider, conversation_id, debt_id, debt_kind, from_store_id, to_store_id
                 )
                 VALUES ('cursor', 'lcm-doctor-debt-other', 'orphan-debt', 'raw_backlog', 1, 2)",
                (),
            )
            .map_err(|error| TraceDecayError::Database {
                operation: "insert foreign orphan debt fixture".to_owned(),
                message: error.to_string(),
            })?;
        Ok(())
    }

    #[doc(hidden)]
    pub async fn clear_lcm_schema_migration_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<()> {
        let transaction = self
            .session_database_for_test(scope)?
            .begin_write_transaction()
            .await?;
        transaction
            .execute(
                "DELETE FROM session_schema_migrations WHERE name = 'lcm'",
                (),
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "clear lcm schema migration fixture".to_owned(),
                message: error.to_string(),
            })?;
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "clear lcm schema migration fixture".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn poison_lcm_raw_projection_for_test(
        &self,
        scope: HostAdmissionScope,
        store_id: i64,
        poison: &str,
    ) -> Result<()> {
        let transaction = self
            .session_database_for_test(scope)?
            .begin_write_transaction()
            .await?;
        transaction
            .execute(
                "UPDATE lcm_raw_messages
                 SET content = ?2, snippet_text = ?2, index_text = ?2
                 WHERE store_id = ?1",
                crate::db::engine::params![store_id, poison],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "poison lcm raw projection fixture".to_owned(),
                message: error.to_string(),
            })?;
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "poison lcm raw projection fixture".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn set_lcm_schema_migration_version_for_test(
        &self,
        scope: HostAdmissionScope,
        version: i64,
    ) -> Result<()> {
        let transaction = self
            .session_database_for_test(scope)?
            .begin_write_transaction()
            .await?;
        transaction
            .execute(
                "UPDATE session_schema_migrations SET version = ?1 WHERE name = 'lcm'",
                [version],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "set lcm schema migration fixture version".to_owned(),
                message: error.to_string(),
            })?;
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "set lcm schema migration fixture version".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn lcm_schema_migration_version_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<Option<i64>> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await?;
        let mut rows = snapshot
            .query(
                "SELECT version FROM session_schema_migrations WHERE name = 'lcm'",
                (),
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "query lcm schema migration fixture version".to_owned(),
                message: error.to_string(),
            })?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read lcm schema migration fixture version".to_owned(),
                message: error.to_string(),
            })?
        else {
            return Ok(None);
        };
        row.get::<i64>(0)
            .map(Some)
            .map_err(|error| TraceDecayError::Database {
                operation: "decode lcm schema migration fixture version".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn set_lcm_schema_migration_applied_at_for_test(
        &self,
        scope: HostAdmissionScope,
        applied_at: i64,
    ) -> Result<()> {
        let transaction = self
            .session_database_for_test(scope)?
            .begin_write_transaction()
            .await?;
        transaction
            .execute(
                "UPDATE session_schema_migrations
                 SET applied_at = ?1
                 WHERE name = 'lcm'",
                [applied_at],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "set lcm schema migration fixture applied_at".to_owned(),
                message: error.to_string(),
            })?;
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "set lcm schema migration fixture applied_at".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn lcm_schema_migration_applied_at_for_test(
        &self,
        scope: HostAdmissionScope,
    ) -> Result<Option<i64>> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await?;
        let mut rows = snapshot
            .query(
                "SELECT applied_at
                 FROM session_schema_migrations
                 WHERE name = 'lcm'",
                (),
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "query lcm schema migration fixture applied_at".to_owned(),
                message: error.to_string(),
            })?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read lcm schema migration fixture applied_at".to_owned(),
                message: error.to_string(),
            })?
        else {
            return Ok(None);
        };
        row.get::<i64>(0)
            .map(Some)
            .map_err(|error| TraceDecayError::Database {
                operation: "decode lcm schema migration fixture applied_at".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn set_lcm_compression_debt_insert_failure_for_test(
        &self,
        scope: HostAdmissionScope,
        enabled: bool,
    ) -> Result<()> {
        let statement = if enabled {
            "CREATE TRIGGER abort_compression_debt_insert
             BEFORE INSERT ON lcm_maintenance_debt
             BEGIN
                SELECT RAISE(ABORT, 'forced maintenance debt failure');
             END;"
        } else {
            "DROP TRIGGER IF EXISTS abort_compression_debt_insert;"
        };
        let transaction = self
            .session_database_for_test(scope)?
            .begin_write_transaction()
            .await?;
        transaction
            .execute_batch(statement)
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "configure LCM compression-debt failure".to_owned(),
                message: error.to_string(),
            })?;
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "configure LCM compression-debt failure".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn set_lcm_late_summary_projection_failure_for_test(
        &self,
        scope: HostAdmissionScope,
        enabled: bool,
    ) -> Result<()> {
        let statement = if enabled {
            "CREATE TRIGGER abort_late_summary_projection
             BEFORE INSERT ON lcm_summary_nodes
             BEGIN
                SELECT RAISE(ABORT, 'forced late summary projection failure');
             END;"
        } else {
            "DROP TRIGGER IF EXISTS abort_late_summary_projection;"
        };
        let transaction = self
            .session_database_for_test(scope)?
            .begin_write_transaction()
            .await?;
        transaction
            .execute_batch(statement)
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "configure late LCM summary projection failure".to_owned(),
                message: error.to_string(),
            })?;
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "configure late LCM summary projection failure".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn replace_lcm_summary_source_for_test(
        &self,
        scope: HostAdmissionScope,
        node_id: &str,
        source_node_id: &str,
    ) -> Result<()> {
        let database = self.session_database_for_test(scope)?;
        let transaction = database.begin_write_transaction().await?;
        transaction
            .execute(
                "DELETE FROM lcm_summary_sources WHERE node_id = ?1",
                (node_id,),
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "delete LCM summary source fixture".to_owned(),
                message: error.to_string(),
            })?;
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "delete LCM summary source fixture".to_owned(),
                message: error.to_string(),
            })?;

        let transaction = database.begin_write_transaction().await?;
        transaction
            .execute(
                "INSERT INTO lcm_summary_sources (node_id, source_kind, source_id, ordinal)
                 VALUES (?1, 'summary_node', ?2, 0)",
                (node_id, source_node_id),
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "replace LCM summary source fixture".to_owned(),
                message: error.to_string(),
            })?;
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "replace LCM summary source fixture".to_owned(),
                message: error.to_string(),
            })
    }

    async fn lcm_session_row_count_for_test(
        &self,
        scope: HostAdmissionScope,
        table: &'static str,
        session_id: &str,
    ) -> Result<i64> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "open LCM session row-count snapshot".to_owned(),
                message: error.to_string(),
            })?;
        let mut rows = snapshot
            .query(
                &format!("SELECT COUNT(*) FROM {table} WHERE session_id = ?1"),
                (session_id,),
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "query LCM session row count".to_owned(),
                message: error.to_string(),
            })?;
        let row = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read LCM session row count".to_owned(),
                message: error.to_string(),
            })?
            .ok_or_else(|| TraceDecayError::Database {
                operation: "read LCM session row count".to_owned(),
                message: "count query returned no row".to_owned(),
            })?;
        row.get::<i64>(0)
            .map_err(|error| TraceDecayError::Database {
                operation: "decode LCM session row count".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn session_summary_node_count_for_test(
        &self,
        scope: HostAdmissionScope,
        session_id: &str,
    ) -> Result<i64> {
        self.lcm_session_row_count_for_test(scope, "session_summary_nodes", session_id)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_raw_message_count_for_test(
        &self,
        scope: HostAdmissionScope,
        session_id: &str,
    ) -> Result<i64> {
        self.lcm_session_row_count_for_test(scope, "lcm_raw_messages", session_id)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_summary_node_count_for_test(
        &self,
        scope: HostAdmissionScope,
        session_id: &str,
    ) -> Result<i64> {
        self.lcm_session_row_count_for_test(scope, "lcm_summary_nodes", session_id)
            .await
    }

    #[doc(hidden)]
    pub async fn wipe_lcm_raw_fts_for_test(
        &self,
        scope: HostAdmissionScope,
        message_id: Option<&str>,
    ) -> Result<()> {
        let transaction = self
            .session_database_for_test(scope)?
            .begin_write_transaction()
            .await?;
        let result = match message_id {
            Some(message_id) => {
                transaction
                    .execute(
                        "DELETE FROM lcm_raw_messages_fts
                         WHERE rowid = (
                             SELECT store_id FROM lcm_raw_messages
                             WHERE provider = 'cursor' AND message_id = ?1
                         )",
                        [message_id],
                    )
                    .await
            }
            None => {
                transaction
                    .execute("DELETE FROM lcm_raw_messages_fts", ())
                    .await
            }
        };
        result.map_err(|error| TraceDecayError::Database {
            operation: "wipe registered LCM raw-message FTS fixture".to_owned(),
            message: error.to_string(),
        })?;
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "wipe registered LCM raw-message FTS fixture".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn lcm_raw_store_id_for_test(
        &self,
        scope: HostAdmissionScope,
        provider: &str,
        message_id: &str,
    ) -> Result<Option<i64>> {
        let snapshot = self
            .session_database_for_test(scope)?
            .read_snapshot()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "open registered lcm raw store id snapshot".to_owned(),
                message: error.to_string(),
            })?;
        let mut rows = snapshot
            .query(
                "SELECT store_id FROM lcm_raw_messages
                 WHERE provider = ?1 AND message_id = ?2",
                crate::db::engine::params![provider, message_id],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "query registered lcm raw store id".to_owned(),
                message: error.to_string(),
            })?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read registered lcm raw store id".to_owned(),
                message: error.to_string(),
            })?
        else {
            return Ok(None);
        };
        row.get::<i64>(0)
            .map(Some)
            .map_err(|error| TraceDecayError::Database {
                operation: "decode registered lcm raw store id".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn lcm_insert_summary_node_for_test(
        &self,
        scope: HostAdmissionScope,
        draft: crate::sessions::lcm::LcmSummaryNodeDraft,
    ) -> std::result::Result<crate::sessions::lcm::LcmSummaryNode, crate::sessions::lcm::LcmError>
    {
        let database = self
            .session_database_for_test(scope)
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        let transaction = database
            .begin_write_transaction()
            .await
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        let publisher =
            crate::global_db::session_temporal_operations::GlobalDbLcmSummaryPublication::new(
                &transaction,
            );
        let summary = crate::sessions::lcm::dag::insert_summary_node(&publisher, draft).await?;
        transaction.commit().await?;
        Ok(summary)
    }

    #[doc(hidden)]
    pub async fn lcm_update_lifecycle_for_test(
        &self,
        scope: HostAdmissionScope,
        update: crate::sessions::lcm::LcmLifecycleUpdate,
    ) -> std::result::Result<crate::sessions::lcm::LcmLifecycleState, crate::sessions::lcm::LcmError>
    {
        let database = self
            .session_database_for_test(scope)
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        let transaction = database
            .begin_write_transaction()
            .await
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        let state =
            crate::sessions::lcm::compression::update_lifecycle(&transaction, update).await?;
        transaction.commit().await?;
        Ok(state)
    }

    #[doc(hidden)]
    pub async fn lcm_publish_immutable_summary_for_test(
        &self,
        scope: HostAdmissionScope,
        publication: crate::sessions::lcm::types::LcmImmutableSummaryPublication,
    ) -> std::result::Result<
        crate::sessions::lcm::types::LcmSummaryPublicationReceipt,
        crate::sessions::lcm::LcmError,
    > {
        let database = self
            .session_database_for_test(scope)
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        let transaction = database
            .begin_write_transaction()
            .await
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        let receipt = crate::global_db::session_temporal_operations::publish_immutable_summary(
            &transaction,
            publication,
        )
        .await?;
        transaction.commit().await?;
        Ok(receipt)
    }

    #[doc(hidden)]
    pub async fn apply_lcm_lineage_fault_for_test(
        &self,
        fault: LcmLineageFaultForTest,
    ) -> Result<()> {
        let fault = match fault {
            LcmLineageFaultForTest::DeleteGeneration {
                session_id,
                generation,
            } => {
                return self
                    .delete_lcm_generation_out_of_band_for_test(&session_id, generation)
                    .await;
            }
            fault => fault,
        };
        self.apply_lcm_lineage_fault_prelude_for_test(&fault)
            .await?;
        let transaction = self
            .primary_lcm_fixture_database_for_test()
            .begin_write_transaction()
            .await?;
        let result = match fault {
            LcmLineageFaultForTest::CorruptCompatibilitySummaryText { node_id, text } => {
                transaction
                    .execute(
                        "UPDATE lcm_summary_nodes SET summary_text = ?2 WHERE node_id = ?1",
                        crate::db::engine::params![node_id, text],
                    )
                    .await
            }
            LcmLineageFaultForTest::ShiftRawMessageTimestamp { store_id, delta } => {
                transaction
                    .execute(
                        "UPDATE lcm_raw_messages
                         SET timestamp = timestamp + ?2
                         WHERE store_id = ?1",
                        crate::db::engine::params![store_id, delta],
                    )
                    .await
            }
            LcmLineageFaultForTest::DeleteGeneration { .. } => unreachable!(),
            LcmLineageFaultForTest::ReplaceGenerationWatermarks {
                session_id,
                generation,
                json,
            } => {
                transaction
                    .execute(
                        "UPDATE session_temporal_generations
                         SET frozen_watermarks_json = ?3
                         WHERE session_id = ?1 AND generation = ?2",
                        crate::db::engine::params![session_id, generation, json],
                    )
                    .await
            }
            LcmLineageFaultForTest::DeleteAvailability {
                session_id,
                generation,
                summary_id,
            } => {
                transaction
                    .execute(
                        "DELETE FROM session_summary_availability
                         WHERE session_id = ?1 AND generation = ?2 AND summary_id = ?3",
                        crate::db::engine::params![session_id, generation, summary_id],
                    )
                    .await
            }
            LcmLineageFaultForTest::ReplaceAvailabilityHorizon {
                session_id,
                generation,
                summary_id,
                source_horizon_json,
            } => {
                transaction
                    .execute(
                        "UPDATE session_summary_availability
                         SET source_horizon_json = ?4
                         WHERE session_id = ?1 AND generation = ?2 AND summary_id = ?3",
                        crate::db::engine::params![
                            session_id,
                            generation,
                            summary_id,
                            source_horizon_json
                        ],
                    )
                    .await
            }
            LcmLineageFaultForTest::SetAvailability {
                session_id,
                generation,
                summary_id,
                availability,
                reason,
            } => {
                transaction
                    .execute(
                        "UPDATE session_summary_availability
                         SET source_horizon_json = (
                                SELECT source_horizon_json FROM session_summary_nodes
                                WHERE summary_id = ?3
                             ),
                             availability = ?4,
                             reason = ?5
                         WHERE session_id = ?1 AND generation = ?2 AND summary_id = ?3",
                        crate::db::engine::params![
                            session_id,
                            generation,
                            summary_id,
                            availability,
                            reason
                        ],
                    )
                    .await
            }
            LcmLineageFaultForTest::SetGenerationFailed {
                session_id,
                generation,
            } => {
                transaction
                    .execute(
                        "UPDATE session_temporal_generations
                         SET state = 'failed',
                             completed_at = COALESCE(completed_at, activated_at, ready_at, created_at)
                         WHERE session_id = ?1 AND generation = ?2",
                        crate::db::engine::params![session_id, generation],
                    )
                    .await
            }
            LcmLineageFaultForTest::CorruptRetrievalAnchorOwner { summary_id } => {
                transaction
                    .execute(
                        "UPDATE retrieval_anchors
                         SET owner_json =
                               '{\"kind\":\"session\",\"provider\":\"wrong\",\"session_id\":\"wrong\"}',
                             anchor_json = json_set(
                                 anchor_json,
                                 '$.owner',
                                 json(
                                     '{\"kind\":\"session\",\"provider\":\"wrong\",\"session_id\":\"wrong\"}'
                                 )
                             )
                         WHERE anchor_id = (
                             SELECT summary_anchor_id FROM session_summary_nodes
                             WHERE summary_id = ?1
                         )",
                        [summary_id],
                    )
                    .await
            }
            LcmLineageFaultForTest::ReplaceSummarySourceWithSummary {
                summary_id,
                ordinal,
                source_summary_id,
            } => {
                transaction
                    .execute(
                        "UPDATE session_summary_sources
                         SET source_kind = 'summary',
                             source_anchor_id = NULL,
                             source_summary_id = ?3
                         WHERE summary_id = ?1 AND source_ordinal = ?2",
                        crate::db::engine::params![summary_id, ordinal, source_summary_id],
                    )
                    .await
            }
        };
        result.map_err(|error| TraceDecayError::Database {
            operation: "apply bounded lcm lineage fault fixture".to_owned(),
            message: error.to_string(),
        })?;
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "apply bounded lcm lineage fault fixture".to_owned(),
                message: error.to_string(),
            })
    }

    async fn apply_lcm_lineage_fault_prelude_for_test(
        &self,
        fault: &LcmLineageFaultForTest,
    ) -> Result<()> {
        let (statement, operation) = match fault {
            LcmLineageFaultForTest::ReplaceGenerationWatermarks { .. } => (
                "DROP TRIGGER IF EXISTS session_temporal_generations_state_guard_v1",
                "prepare changed lcm watermarks fixture",
            ),
            LcmLineageFaultForTest::SetGenerationFailed { .. } => (
                "DROP TRIGGER IF EXISTS session_temporal_generations_state_guard_v1",
                "prepare failed lcm generation fixture",
            ),
            LcmLineageFaultForTest::CorruptRetrievalAnchorOwner { .. } => (
                "DROP TRIGGER IF EXISTS retrieval_anchors_immutable_update;
                 DROP TRIGGER IF EXISTS observation_retrieval_anchors_immutable_update;",
                "prepare corrupt lcm retrieval owner fixture",
            ),
            LcmLineageFaultForTest::ReplaceSummarySourceWithSummary { .. } => (
                "DROP TRIGGER IF EXISTS session_summary_sources_immutable_update_v1",
                "prepare corrupt lcm summary source fixture",
            ),
            _ => return Ok(()),
        };
        let transaction = self
            .primary_lcm_fixture_database_for_test()
            .begin_write_transaction()
            .await?;
        transaction
            .execute_batch(statement)
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: operation.to_owned(),
                message: error.to_string(),
            })?;
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: operation.to_owned(),
                message: error.to_string(),
            })
    }

    async fn delete_lcm_generation_out_of_band_for_test(
        &self,
        session_id: &str,
        generation: i64,
    ) -> Result<()> {
        let database = self.primary_lcm_fixture_database_for_test();
        let transaction = database.begin_write_transaction().await?;
        transaction
            .execute(
                "DROP TRIGGER IF EXISTS session_temporal_generations_delete_guard_v1",
                (),
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "prepare missing lcm generation fixture".to_owned(),
                message: error.to_string(),
            })?;
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "prepare missing lcm generation fixture".to_owned(),
                message: error.to_string(),
            })?;

        let connection = rusqlite::Connection::open(database.db_path()).map_err(|error| {
            TraceDecayError::Database {
                operation: "prepare missing lcm generation fixture".to_owned(),
                message: error.to_string(),
            }
        })?;
        connection
            .pragma_update(None, "foreign_keys", "OFF")
            .map_err(|error| TraceDecayError::Database {
                operation: "prepare missing lcm generation fixture".to_owned(),
                message: error.to_string(),
            })?;
        connection
            .execute(
                "DELETE FROM session_temporal_generations
                 WHERE session_id = ?1 AND generation = ?2",
                (session_id, generation),
            )
            .map(|_| ())
            .map_err(|error| TraceDecayError::Database {
                operation: "apply bounded lcm lineage fault fixture".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn lcm_lineage_counts_for_test(
        &self,
        session_id: Option<&str>,
    ) -> Result<LcmLineageCountsForTest> {
        let snapshot = self
            .primary_lcm_fixture_database_for_test()
            .read_snapshot()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "open lcm lineage count snapshot".to_owned(),
                message: error.to_string(),
            })?;
        let mut rows = snapshot
            .query(
                "SELECT
                    (SELECT COUNT(*) FROM session_temporal_generations
                     WHERE state = 'active' AND (?1 IS NULL OR session_id = ?1)),
                    (SELECT COUNT(*) FROM session_temporal_generations
                     WHERE ?1 IS NULL OR session_id = ?1),
                    (SELECT COUNT(*) FROM session_summary_nodes
                     WHERE ?1 IS NULL OR session_id = ?1),
                    (SELECT COUNT(*) FROM session_summary_sources source
                     JOIN session_summary_nodes node ON node.summary_id = source.summary_id
                     WHERE ?1 IS NULL OR node.session_id = ?1),
                    (SELECT COUNT(*) FROM session_summary_successors successor
                     JOIN session_summary_nodes node
                       ON node.summary_id = successor.successor_summary_id
                     WHERE ?1 IS NULL OR node.session_id = ?1),
                    (SELECT COUNT(*) FROM session_query_cursor_keys)",
                [session_id],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "query lcm lineage counts".to_owned(),
                message: error.to_string(),
            })?;
        let row = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read lcm lineage counts".to_owned(),
                message: error.to_string(),
            })?
            .ok_or_else(|| TraceDecayError::Database {
                operation: "read lcm lineage counts".to_owned(),
                message: "count query returned no row".to_owned(),
            })?;
        let value = |index| {
            row.get::<i64>(index)
                .map_err(|error| TraceDecayError::Database {
                    operation: "decode lcm lineage counts".to_owned(),
                    message: error.to_string(),
                })
        };
        Ok(LcmLineageCountsForTest {
            active_generations: value(0)?,
            total_generations: value(1)?,
            summary_nodes: value(2)?,
            summary_sources: value(3)?,
            summary_successors: value(4)?,
            cursor_keys: value(5)?,
        })
    }

    #[doc(hidden)]
    pub async fn lcm_raw_message_search_fields_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> std::result::Result<Option<(String, String)>, crate::sessions::lcm::LcmError> {
        let snapshot = self
            .primary_lcm_fixture_database_for_test()
            .read_snapshot()
            .await
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        let mut rows = snapshot
            .query(
                "SELECT snippet_text, index_text
                 FROM lcm_raw_messages
                 WHERE provider = ?1 AND message_id = ?2",
                crate::db::engine::params![provider, message_id],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(Some((row.get(0)?, row.get(1)?)))
    }

    #[doc(hidden)]
    pub async fn lcm_raw_message_fts_count_for_test(
        &self,
        query: &str,
    ) -> std::result::Result<i64, crate::sessions::lcm::LcmError> {
        let snapshot = self
            .primary_lcm_fixture_database_for_test()
            .read_snapshot()
            .await
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        let mut rows = snapshot
            .query(
                "SELECT COUNT(*)
                 FROM lcm_raw_messages_fts
                 WHERE lcm_raw_messages_fts MATCH ?1",
                [query],
            )
            .await?;
        let row = rows.next().await?.ok_or_else(|| {
            crate::sessions::lcm::LcmError::Db("COUNT(*) returned no row".to_owned())
        })?;
        Ok(row.get(0)?)
    }

    #[doc(hidden)]
    pub async fn lcm_raw_message_metadata_json_for_test(
        &self,
        provider: &str,
        message_id: &str,
    ) -> std::result::Result<Option<Option<String>>, crate::sessions::lcm::LcmError> {
        let snapshot = self
            .primary_lcm_fixture_database_for_test()
            .read_snapshot()
            .await
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        let mut rows = snapshot
            .query(
                "SELECT metadata_json
                 FROM lcm_raw_messages
                 WHERE provider = ?1 AND message_id = ?2",
                crate::db::engine::params![provider, message_id],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(Some(row.get(0)?))
    }

    #[doc(hidden)]
    pub async fn lcm_delete_external_payload_for_test(
        &self,
        scope: HostAdmissionScope,
        payload_ref: &str,
        opts: &crate::sessions::lcm::payload::DeleteOpts,
    ) -> std::result::Result<
        crate::sessions::lcm::payload::DeleteOutcome,
        crate::sessions::lcm::LcmError,
    > {
        let database = self
            .session_database_for_test(scope)
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        let storage_root = database.db_path().parent().ok_or_else(|| {
            crate::sessions::lcm::LcmError::Db(
                "registered session database has no storage root".to_owned(),
            )
        })?;
        let transaction = database
            .begin_write_transaction()
            .await
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        let prepared = crate::sessions::lcm::payload::delete_external_payload_in_transaction(
            &transaction,
            storage_root,
            payload_ref,
            opts,
        )
        .await?;
        transaction.commit().await?;

        let mut outcome = prepared.outcome;
        if prepared.pending_removal_bytes.is_some() {
            let transaction = database
                .begin_write_transaction()
                .await
                .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
            let drained = crate::sessions::lcm::gc::drain_pending_payload_delete_in_transaction(
                &transaction,
                storage_root,
                payload_ref,
            )
            .await;
            match drained {
                Ok(removed) => {
                    transaction.commit().await?;
                    outcome.file_removed = removed.is_some();
                    outcome.bytes_freed = removed.unwrap_or_default();
                }
                Err(error) => {
                    let _ = transaction.rollback().await;
                    tracing::warn!(
                        payload_ref,
                        %error,
                        "payload metadata deletion committed; deferred payload file removal remains pending"
                    );
                }
            }
        }
        Ok(outcome)
    }

    #[doc(hidden)]
    pub async fn lcm_external_payload_manifest_for_test(
        &self,
        payload_ref: &str,
    ) -> std::result::Result<
        Option<LcmExternalPayloadManifestTestRecord>,
        crate::sessions::lcm::LcmError,
    > {
        let snapshot = self
            .primary_lcm_fixture_database_for_test()
            .read_snapshot()
            .await
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        let mut rows = snapshot
            .query(
                "SELECT manifest.payload_ref, manifest.session_id,
                        manifest.payload_digest, manifest.manifest_json,
                        receipt.receipt_id, manifest.created_at, external.created_at
                 FROM session_external_payload_manifests manifest
                 JOIN sanitization_receipts receipt
                   ON receipt.receipt_id = manifest.receipt_id
                 JOIN lcm_external_payloads external
                   ON external.payload_ref = manifest.payload_ref
                 WHERE manifest.payload_ref = ?1",
                [payload_ref],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(Some(LcmExternalPayloadManifestTestRecord {
            payload_ref: row.get(0)?,
            session_id: row.get(1)?,
            payload_digest: row.get(2)?,
            manifest_json: row.get(3)?,
            receipt_id: row.get(4)?,
            created_at: row.get(5)?,
            external_created_at: row.get(6)?,
        }))
    }

    #[doc(hidden)]
    pub async fn lcm_summary_publication_receipt_id_for_test(
        &self,
        summary_id: &str,
    ) -> std::result::Result<Option<String>, crate::sessions::lcm::LcmError> {
        let snapshot = self
            .primary_lcm_fixture_database_for_test()
            .read_snapshot()
            .await
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        let mut rows = snapshot
            .query(
                "SELECT json_extract(publication_json, '$.receipt_id')
                 FROM session_summary_nodes
                 WHERE summary_id = ?1",
                [summary_id],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Ok(None);
        };
        Ok(row.get(0)?)
    }

    #[doc(hidden)]
    pub async fn replace_lcm_external_payload_manifest_for_test(
        &self,
        payload_ref: &str,
        replacement: &LcmExternalPayloadManifestTestRecord,
    ) -> std::result::Result<(), crate::sessions::lcm::LcmError> {
        let transaction = self
            .primary_lcm_fixture_database_for_test()
            .begin_write_transaction()
            .await
            .map_err(|error| crate::sessions::lcm::LcmError::Db(error.to_string()))?;
        transaction
            .execute_batch("DROP TRIGGER session_external_payload_manifests_immutable_update_v1;")
            .await?;
        transaction
            .execute(
                "UPDATE session_external_payload_manifests
                 SET session_id = ?2, payload_digest = ?3, manifest_json = ?4,
                     receipt_id = ?5, created_at = ?6
                 WHERE payload_ref = ?1",
                crate::db::engine::params![
                    payload_ref,
                    replacement.session_id.as_str(),
                    replacement.payload_digest.as_str(),
                    replacement.manifest_json.as_str(),
                    replacement.receipt_id.as_str(),
                    replacement.created_at,
                ],
            )
            .await?;
        transaction
            .execute_batch(
                "CREATE TRIGGER session_external_payload_manifests_immutable_update_v1
                 BEFORE UPDATE ON session_external_payload_manifests BEGIN
                     SELECT RAISE(ABORT, 'session external payload manifests are immutable');
                 END;",
            )
            .await?;
        transaction.commit().await?;
        Ok(())
    }

    #[doc(hidden)]
    pub async fn lcm_lifecycle_state_for_test(
        &self,
        provider: &str,
        conversation_id: &str,
    ) -> std::result::Result<crate::sessions::lcm::LcmLifecycleState, crate::sessions::lcm::LcmError>
    {
        let snapshot = self
            .primary_lcm_fixture_database_for_test()
            .read_snapshot()
            .await?;
        crate::sessions::lcm::compression::lifecycle_state(&snapshot, provider, conversation_id)
            .await
    }

    #[doc(hidden)]
    pub async fn lcm_summary_successor_edges_for_test(&self) -> Result<Vec<(String, String)>> {
        let snapshot = self
            .primary_lcm_fixture_database_for_test()
            .read_snapshot()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "open registered LCM successor-edge snapshot".to_owned(),
                message: error.to_string(),
            })?;
        let mut rows = snapshot
            .query(
                "SELECT predecessor_summary_id, successor_summary_id
                 FROM session_summary_successors
                 ORDER BY predecessor_summary_id, successor_summary_id",
                (),
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "query registered LCM successor edges".to_owned(),
                message: error.to_string(),
            })?;
        let mut edges = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read registered LCM successor edge".to_owned(),
                message: error.to_string(),
            })?
        {
            let predecessor = row
                .get::<String>(0)
                .map_err(|error| TraceDecayError::Database {
                    operation: "decode registered LCM predecessor".to_owned(),
                    message: error.to_string(),
                })?;
            let successor = row
                .get::<String>(1)
                .map_err(|error| TraceDecayError::Database {
                    operation: "decode registered LCM successor".to_owned(),
                    message: error.to_string(),
                })?;
            edges.push((predecessor, successor));
        }
        Ok(edges)
    }

    #[doc(hidden)]
    pub async fn lcm_active_summary_availability_for_test(
        &self,
        session_id: &str,
    ) -> Result<Vec<(String, String)>> {
        let snapshot = self
            .primary_lcm_fixture_database_for_test()
            .read_snapshot()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "open registered LCM availability snapshot".to_owned(),
                message: error.to_string(),
            })?;
        let mut rows = snapshot
            .query(
                "SELECT availability.summary_id, availability.availability
                 FROM session_summary_availability AS availability
                 JOIN session_temporal_generations AS generation
                   ON generation.session_id = availability.session_id
                  AND generation.generation = availability.generation
                 WHERE availability.session_id = ?1
                   AND generation.state = 'active'
                 ORDER BY availability.summary_id",
                crate::db::engine::params![session_id],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "query registered LCM active summary availability".to_owned(),
                message: error.to_string(),
            })?;
        let mut labels = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "read registered LCM active summary availability".to_owned(),
                message: error.to_string(),
            })?
        {
            labels.push((
                row.get(0).map_err(|error| TraceDecayError::Database {
                    operation: "decode registered LCM availability summary".to_owned(),
                    message: error.to_string(),
                })?,
                row.get(1).map_err(|error| TraceDecayError::Database {
                    operation: "decode registered LCM availability label".to_owned(),
                    message: error.to_string(),
                })?,
            ));
        }
        Ok(labels)
    }

    #[doc(hidden)]
    pub async fn insert_lcm_poison_summary_for_test(
        &self,
        poison_node_id: &str,
        predecessor_node_id: &str,
    ) -> Result<()> {
        let transaction = self
            .primary_lcm_fixture_database_for_test()
            .begin_write_transaction()
            .await?;
        transaction
            .execute(
                "INSERT INTO lcm_summary_nodes (
                    node_id, provider, conversation_id, session_id, depth,
                    summary_text, summary_hash, summary_token_count, source_token_count,
                    source_time_start, source_time_end, expand_hint, metadata_json, created_at
                 )
                 SELECT ?1, provider, conversation_id, session_id, depth + 1000,
                        'unpublishable poison', 'poison-hash', summary_token_count,
                        source_token_count, source_time_start, source_time_end,
                        expand_hint, metadata_json, created_at + 1000000
                 FROM lcm_summary_nodes
                 WHERE node_id = ?2",
                crate::db::engine::params![poison_node_id, predecessor_node_id],
            )
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "insert registered LCM poison summary".to_owned(),
                message: error.to_string(),
            })?;
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "insert registered LCM poison summary".to_owned(),
                message: error.to_string(),
            })
    }

    #[doc(hidden)]
    pub async fn install_lcm_summary_insert_abort_trigger_for_test(&self) -> Result<()> {
        self.set_lcm_summary_insert_abort_trigger_for_test(true)
            .await
    }

    #[doc(hidden)]
    pub async fn remove_lcm_summary_insert_abort_trigger_for_test(&self) -> Result<()> {
        self.set_lcm_summary_insert_abort_trigger_for_test(false)
            .await
    }

    async fn set_lcm_summary_insert_abort_trigger_for_test(&self, enabled: bool) -> Result<()> {
        let statement = if enabled {
            "CREATE TRIGGER fail_codex_summary_successor
             BEFORE INSERT ON lcm_summary_nodes
             BEGIN
                SELECT RAISE(ABORT, 'forced summary successor failure');
             END;"
        } else {
            "DROP TRIGGER IF EXISTS fail_codex_summary_successor;"
        };
        let transaction = self
            .primary_lcm_fixture_database_for_test()
            .begin_write_transaction()
            .await?;
        transaction
            .execute_batch(statement)
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "configure registered LCM summary failure".to_owned(),
                message: error.to_string(),
            })?;
        transaction
            .commit()
            .await
            .map_err(|error| TraceDecayError::Database {
                operation: "configure registered LCM summary failure".to_owned(),
                message: error.to_string(),
            })
    }
}
