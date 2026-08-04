use super::*;

#[doc(hidden)]
pub struct InputReadEvidence {
    pub(super) source_graph: GraphStoreEvidence,
    pub(super) target_graph: GraphStoreEvidence,
    pub sessions: crate::sqlite_read_snapshot::SnapshotSet,
    pub(super) session_fingerprints: BTreeMap<PathBuf, String>,
}

pub(super) struct GraphStoreEvidence {
    pub(super) identities: sqlite::GraphLogicalIdentities,
    pub(super) fingerprints: BTreeMap<PathBuf, String>,
    generations: BTreeMap<PathBuf, crate::sqlite_read_snapshot::SourceGeneration>,
    peak_scratch_bytes: u64,
}

impl InputReadEvidence {
    pub(super) fn validate(
        &self,
        source_graphs: &[PathBuf],
        target_graphs: &[PathBuf],
        session_paths: &[PathBuf],
    ) -> Result<()> {
        self.source_graph.validate(source_graphs)?;
        self.target_graph.validate(target_graphs)?;
        self.sessions
            .validate_sources_unchanged()
            .map_err(io_error)?;
        for path in session_paths {
            self.sessions.get(path).map_err(io_error)?;
            if !self.session_fingerprints.contains_key(path) {
                return Err(config_error(
                    "session database set changed after inspection",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn validate_content(
        &self,
        source_graphs: &[PathBuf],
        target_graphs: &[PathBuf],
        session_paths: &[PathBuf],
    ) -> Result<()> {
        self.source_graph.validate_content(source_graphs)?;
        self.target_graph.validate_content(target_graphs)?;
        validate_path_set(
            self.session_fingerprints.keys(),
            session_paths,
            "session database set changed after inspection",
        )?;
        for path in session_paths {
            let expected = self
                .session_fingerprints
                .get(path)
                .ok_or_else(|| config_error("session database set changed after inspection"))?;
            let current =
                crate::sqlite_read_snapshot::family_fingerprint(path).map_err(io_error)?;
            if &current != expected {
                return Err(config_error(format!(
                    "SQLite database family '{}' content changed after inspection",
                    path.display()
                )));
            }
        }
        Ok(())
    }

    #[doc(hidden)]
    pub fn retained_database_count(&self) -> usize {
        self.sessions.database_count()
    }

    #[doc(hidden)]
    pub fn peak_graph_scratch_bytes(&self) -> u64 {
        self.source_graph
            .peak_scratch_bytes
            .max(self.target_graph.peak_scratch_bytes)
    }
}

impl GraphStoreEvidence {
    fn validate(&self, expected_paths: &[PathBuf]) -> Result<()> {
        validate_path_set(
            self.fingerprints.keys(),
            expected_paths,
            "graph database set changed after inspection",
        )?;
        for generation in self.generations.values() {
            generation.validate().map_err(io_error)?;
        }
        Ok(())
    }

    fn validate_content(&self, expected_paths: &[PathBuf]) -> Result<()> {
        validate_path_set(
            self.fingerprints.keys(),
            expected_paths,
            "graph database set changed after inspection",
        )?;
        for path in expected_paths {
            let expected = self
                .fingerprints
                .get(path)
                .ok_or_else(|| config_error("graph database set changed after inspection"))?;
            let current =
                crate::sqlite_read_snapshot::family_fingerprint(path).map_err(io_error)?;
            if &current != expected {
                return Err(config_error(format!(
                    "SQLite database family '{}' content changed after inspection",
                    path.display()
                )));
            }
        }
        Ok(())
    }
}

fn validate_path_set<'a>(
    actual: impl Iterator<Item = &'a PathBuf>,
    expected: &[PathBuf],
    message: &str,
) -> Result<()> {
    let actual = actual.cloned().collect::<BTreeSet<_>>();
    let expected = expected.iter().cloned().collect::<BTreeSet<_>>();
    if actual == expected {
        Ok(())
    } else {
        Err(config_error(message))
    }
}

pub(super) async fn capture_input_evidence(
    source_graphs: &[PathBuf],
    target_graphs: &[PathBuf],
    session_paths: &[PathBuf],
    scratch_root: &Path,
) -> Result<InputReadEvidence> {
    let source_graph = capture_graph_evidence(source_graphs, scratch_root).await?;
    let target_graph = capture_graph_evidence(target_graphs, scratch_root).await?;
    let sessions =
        crate::sqlite_read_snapshot::SnapshotSet::capture_in(session_paths, scratch_root)
            .await
            .map_err(io_error)?;
    let mut session_fingerprints = BTreeMap::new();
    for path in session_paths {
        sessions.get(path).map_err(io_error)?;
        let fingerprint =
            crate::sqlite_read_snapshot::family_fingerprint(path).map_err(io_error)?;
        session_fingerprints.insert(path.clone(), fingerprint);
    }
    sessions.validate_sources_unchanged().map_err(io_error)?;
    Ok(InputReadEvidence {
        source_graph,
        target_graph,
        sessions,
        session_fingerprints,
    })
}

async fn capture_graph_evidence(
    paths: &[PathBuf],
    scratch_root: &Path,
) -> Result<GraphStoreEvidence> {
    let mut identities = sqlite::GraphLogicalIdentities::default();
    let mut fingerprints = BTreeMap::new();
    let mut generations = BTreeMap::new();
    let mut peak_scratch_bytes = 0_u64;
    for path in paths {
        let snapshot = crate::sqlite_read_snapshot::open_in(path, scratch_root)
            .await
            .map_err(io_error)?;
        peak_scratch_bytes = peak_scratch_bytes.max(snapshot.copied_bytes());
        sqlite::quick_check_connection(snapshot.connection(), path).await?;
        sqlite::extend_graph_identities(snapshot.connection(), &mut identities).await?;
        let fingerprint =
            crate::sqlite_read_snapshot::family_fingerprint(path).map_err(io_error)?;
        snapshot.validate_source().map_err(io_error)?;
        generations.insert(path.clone(), snapshot.source_generation());
        fingerprints.insert(path.clone(), fingerprint);
    }
    Ok(GraphStoreEvidence {
        identities,
        fingerprints,
        generations,
        peak_scratch_bytes,
    })
}
