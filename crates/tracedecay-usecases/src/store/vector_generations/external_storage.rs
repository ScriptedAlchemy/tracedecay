/// Which payload rows a load resolved, and whether it had to fall back to the
/// pre-migration inline encoding.
#[derive(Debug, Default)]
struct VectorPayloadLoadV1 {
    /// Addresses already durable in the payload table. A later write skips
    /// them, so a commit persists only the rows its own batch introduced.
    durable: BTreeSet<ContentDigest>,
    /// Collection addresses already durable in the slice table, for the same
    /// reason.
    durable_slices: BTreeSet<ContentDigest>,
    /// True when the loaded document still carried inline floats.
    migrated_inline_payloads: bool,
    /// True when the loaded document still carried an inline O(store)
    /// collection that belongs in the slice table.
    migrated_inline_collections: bool,
}

fn encode_vector_payload(values: &[f32]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(std::mem::size_of_val(values));
    for value in values {
        payload.extend_from_slice(&value.to_le_bytes());
    }
    payload
}

fn decode_vector_payload(
    output_digest: &ContentDigest,
    dimensions: i64,
    payload: &[u8],
) -> Result<Vec<f32>, VectorGenerationStoreErrorV1> {
    let width = size_of::<f32>();
    if dimensions <= 0
        || !payload.len().is_multiple_of(width)
        || usize::try_from(dimensions).ok() != Some(payload.len() / width)
    {
        return Err(VectorGenerationStoreErrorV1::Storage(format!(
            "vector payload {output_digest} has an inconsistent width"
        )));
    }
    Ok(payload
        .chunks_exact(width)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect())
}

impl FakeVectorGenerationStoreV1 {
    fn visit_vectors<'state>(&'state self, visit: &mut impl FnMut(&'state ProjectedChunkVectorV1)) {
        for generation in self.published.generations.values() {
            for vector in generation.vectors.values() {
                visit(vector);
            }
        }
        for staged in self.staged.values() {
            for vector in staged.vectors.values() {
                visit(vector);
            }
            for batch in staged.batches.iter() {
                for vector in &batch.vectors {
                    visit(vector);
                }
            }
        }
    }

    /// Refill the elided float payload of every vector row.
    ///
    /// Every write here goes through [`ExternalV1::elided_mut`]: the
    /// externalized encoding does not carry floats, so restoring them leaves
    /// the stored bytes — and therefore the collection address — unchanged.
    fn visit_vectors_mut(&mut self, visit: &mut impl FnMut(&mut ProjectedChunkVectorV1)) {
        for generation in self.published.generations.values_mut() {
            for vector in generation.vectors.elided_mut().values_mut() {
                visit(vector);
            }
        }
        for staged in self.staged.values_mut() {
            for vector in staged.vectors.elided_mut().values_mut() {
                visit(vector);
            }
            for batch in staged.batches.elided_mut().iter_mut() {
                for vector in &mut batch.vectors {
                    visit(vector);
                }
            }
        }
    }
}

/// Fill every externalized vector in `state` from `payload_table`.
///
/// Reads are paged: addresses are resolved in bounded `IN (...)` groups rather
/// than materializing the table. A missing address fails closed — a generation
/// whose floats cannot be resolved must not serve.
async fn hydrate_vector_payloads(
    database: &Database,
    payload_table: &str,
    state: &mut FakeVectorGenerationStoreV1,
) -> Result<VectorPayloadLoadV1, VectorGenerationStoreErrorV1> {
    let mut load = VectorPayloadLoadV1::default();
    let mut wanted = BTreeSet::new();
    state.visit_vectors(&mut |vector| {
        if vector.values.is_empty() {
            wanted.insert(vector.output_digest.clone());
        } else {
            load.migrated_inline_payloads = true;
        }
    });
    if wanted.is_empty() {
        return Ok(load);
    }
    let payloads = read_vector_payloads(database, payload_table, &wanted).await?;
    let mut missing = None;
    state.visit_vectors_mut(&mut |vector| {
        if !vector.values.is_empty() {
            return;
        }
        match payloads.get(&vector.output_digest) {
            Some(values) => vector.values.clone_from(values),
            None => {
                missing.get_or_insert_with(|| vector.output_digest.clone());
            }
        }
    });
    if let Some(missing) = missing {
        return Err(VectorGenerationStoreErrorV1::Storage(format!(
            "vector payload {missing} is missing from the store"
        )));
    }
    load.durable = wanted;
    Ok(load)
}

/// Seal a hand-built fixture so its document can be serialized.
///
/// The store's own writers seal inside their mutation path; fixtures that
/// build state directly go through here instead.
#[cfg(test)]
fn seal_test_state(
    state: &mut FakeVectorGenerationStoreV1,
) -> BTreeMap<ContentDigest, Vec<Vec<u8>>> {
    seal_external_state(state, &BTreeSet::new()).expect("seal externalized state")
}

/// Round-trip the state document the way a restart does, standing in for the
/// slice and payload tables with the reference state still in memory.
#[cfg(test)]
fn restart_round_trip(state: &mut FakeVectorGenerationStoreV1) -> FakeVectorGenerationStoreV1 {
    let sealed = seal_test_state(state);
    let encoded = serde_json::to_string(&*state).expect("serialize vector state");
    let mut restarted: FakeVectorGenerationStoreV1 =
        serde_json::from_str(&encoded).expect("deserialize vector state");
    fill_from_sealed(&mut restarted, &sealed);
    restarted.hydrate_from(state);
    restarted
}

#[cfg(test)]
fn fill_from_sealed(
    state: &mut FakeVectorGenerationStoreV1,
    sealed: &BTreeMap<ContentDigest, Vec<Vec<u8>>>,
) {
    state
        .visit_external_slots(&mut |slot| {
            let Some(address) = slot.address().cloned() else {
                return Ok(());
            };
            slot.fill(sealed.get(&address).expect("sealed collection"))
        })
        .expect("fill externalized collections");
}

/// Fill one standalone published generation read outside the writer lane.
async fn hydrate_generation_payloads_with_control(
    database: &Database,
    payload_table: &str,
    generation: &mut PublishedVectorGenerationV1,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<(), VectorGenerationStoreErrorV1> {
    ensure_vector_read_not_cancelled(is_cancelled)?;
    let wanted = generation
        .vectors
        .values()
        .filter(|vector| vector.values.is_empty())
        .map(|vector| vector.output_digest.clone())
        .collect::<BTreeSet<_>>();
    if wanted.is_empty() {
        return Ok(());
    }
    let payloads =
        read_vector_payloads_with_control(database, payload_table, &wanted, is_cancelled).await?;
    for vector in generation.vectors.values_mut() {
        ensure_vector_read_not_cancelled(is_cancelled)?;
        if !vector.values.is_empty() {
            continue;
        }
        let values = payloads.get(&vector.output_digest).ok_or_else(|| {
            VectorGenerationStoreErrorV1::Storage(format!(
                "vector payload {} is missing from the store",
                vector.output_digest
            ))
        })?;
        vector.values.clone_from(values);
    }
    Ok(())
}

async fn read_vector_payloads(
    database: &Database,
    payload_table: &str,
    wanted: &BTreeSet<ContentDigest>,
) -> Result<BTreeMap<ContentDigest, Vec<f32>>, VectorGenerationStoreErrorV1> {
    read_vector_payloads_with_control(database, payload_table, wanted, &|| false).await
}

async fn read_vector_payloads_with_control(
    database: &Database,
    payload_table: &str,
    wanted: &BTreeSet<ContentDigest>,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<BTreeMap<ContentDigest, Vec<f32>>, VectorGenerationStoreErrorV1> {
    let connection = database.engine_conn();
    let addresses = wanted.iter().cloned().collect::<Vec<_>>();
    let mut payloads = BTreeMap::new();
    for group in addresses.chunks(VECTOR_PAYLOAD_STATEMENT_ROWS) {
        ensure_vector_read_not_cancelled(is_cancelled)?;
        let placeholders = (1..=group.len())
            .map(|index| format!("?{index}"))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT output_digest, dimensions, payload
             FROM {payload_table}
             WHERE output_digest IN ({placeholders})"
        );
        let values = group
            .iter()
            .map(|digest| {
                tracedecay_runtime_core::db::engine::Value::Text(digest.as_str().to_owned())
            })
            .collect::<Vec<_>>();
        let mut rows = connection
            .query(
                &sql,
                tracedecay_runtime_core::db::engine::params_from_iter(values),
            )
            .await
            .map_err(storage_error)?;
        while let Some(row) = rows.next().await.map_err(storage_error)? {
            ensure_vector_read_not_cancelled(is_cancelled)?;
            let output_digest =
                ContentDigest::try_from(row.get::<String>(0).map_err(storage_error)?)
                    .map_err(storage_error)?;
            let dimensions = row.get::<i64>(1).map_err(storage_error)?;
            let payload = row.get::<Vec<u8>>(2).map_err(storage_error)?;
            let decoded = decode_vector_payload(&output_digest, dimensions, &payload)?;
            payloads.insert(output_digest, decoded);
        }
        drop(rows);
    }
    Ok(payloads)
}

/// Persist every payload `state` references that is not already durable.
///
/// Writes happen inside the caller's transaction, so payload rows and the
/// state pointer that names them become visible together. Rows are
/// content-addressed and inserted with `OR IGNORE`, so a retried commit is a
/// no-op rather than a conflict.
async fn write_vector_payloads(
    transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
    payload_table: &str,
    state: &FakeVectorGenerationStoreV1,
    durable: &BTreeSet<ContentDigest>,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let mut pending: BTreeMap<ContentDigest, &[f32]> = BTreeMap::new();
    state.visit_vectors(&mut |vector| {
        if !durable.contains(&vector.output_digest) {
            pending
                .entry(vector.output_digest.clone())
                .or_insert(&vector.values);
        }
    });
    if pending.is_empty() {
        return Ok(());
    }
    let rows = pending.into_iter().collect::<Vec<_>>();
    for group in rows.chunks(VECTOR_PAYLOAD_STATEMENT_ROWS) {
        let tuples = (0..group.len())
            .map(|index| {
                let base = index * 3;
                format!("(?{}, ?{}, ?{})", base + 1, base + 2, base + 3)
            })
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT OR IGNORE INTO {payload_table} (output_digest, dimensions, payload)
             VALUES {tuples}"
        );
        let mut values = Vec::with_capacity(group.len() * 3);
        for (output_digest, payload) in group {
            values.push(tracedecay_runtime_core::db::engine::Value::Text(
                output_digest.as_str().to_owned(),
            ));
            values.push(tracedecay_runtime_core::db::engine::Value::Integer(
                i64::try_from(payload.len()).map_err(storage_error)?,
            ));
            values.push(tracedecay_runtime_core::db::engine::Value::Blob(
                encode_vector_payload(payload),
            ));
        }
        transaction
            .execute_engine(
                &sql,
                tracedecay_runtime_core::db::engine::params_from_iter(values),
            )
            .await
            .map_err(storage_error)?;
    }
    Ok(())
}

type ExternalSlotVisitV1<'visit> =
    dyn FnMut(&mut dyn ExternalSlotV1) -> Result<(), VectorGenerationStoreErrorV1> + 'visit;

impl PublishedVectorGenerationV1 {
    fn visit_external_slots(
        &mut self,
        visit: &mut ExternalSlotVisitV1<'_>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        visit(&mut self.vectors)?;
        visit(&mut self.tombstones)?;
        visit(&mut self.tombstone_digests)?;
        visit(&mut self.receipts)
    }
}

impl FakeVectorGenerationStoreV1 {
    /// Every externalized collection in the state document, in a stable order.
    fn visit_external_slots(
        &mut self,
        visit: &mut ExternalSlotVisitV1<'_>,
    ) -> Result<(), VectorGenerationStoreErrorV1> {
        for staged in self.staged.values_mut() {
            visit(&mut staged.plan.expected_chunk_ids)?;
            visit(&mut staged.vectors)?;
            visit(&mut staged.tombstones)?;
            visit(&mut staged.tombstone_ids)?;
            visit(&mut staged.batches)?;
            visit(&mut staged.receipts)?;
            visit(&mut staged.committed_chunk_effects)?;
        }
        for generation in self.published.generations.values_mut() {
            generation.visit_external_slots(visit)?;
        }
        Ok(())
    }
}

/// Seal every externalized collection and collect the slices to write.
///
/// A slot whose address is already durable is left alone, so a mutation
/// re-encodes only what it actually changed: committing one batch writes that
/// batch's slices, not the corpus. Content addressing then makes publication
/// free — the staged collections and the published ones they become hash to
/// the same addresses, which are durable by then.
fn seal_external_state(
    state: &mut FakeVectorGenerationStoreV1,
    durable: &BTreeSet<ContentDigest>,
) -> Result<BTreeMap<ContentDigest, Vec<Vec<u8>>>, VectorGenerationStoreErrorV1> {
    let mut pending: BTreeMap<ContentDigest, Vec<Vec<u8>>> = BTreeMap::new();
    state.visit_external_slots(&mut |slot| {
        let sealed =
            slot.seal(&mut |address| !durable.contains(address) && !pending.contains_key(address))?;
        if let Some((address, slices)) = sealed {
            pending.insert(address, slices);
        }
        Ok(())
    })?;
    Ok(pending)
}

/// Fill every externalized collection in `state` from `slice_table`.
///
/// Collections are resolved one address at a time so a whole-corpus load never
/// holds every encoded collection at once, and each is verified against its
/// content address before it is parsed. A missing address fails closed.
async fn hydrate_external_state(
    database: &Database,
    slice_table: &str,
    state: &mut FakeVectorGenerationStoreV1,
) -> Result<(BTreeSet<ContentDigest>, bool), VectorGenerationStoreErrorV1> {
    let mut wanted = BTreeSet::new();
    let mut inline = false;
    state.visit_external_slots(&mut |slot| {
        match slot.address() {
            Some(address) => {
                wanted.insert(address.clone());
            }
            None => inline = true,
        }
        Ok(())
    })?;
    for address in &wanted {
        let slices = read_state_slices(database, slice_table, address).await?;
        state.visit_external_slots(&mut |slot| {
            if slot.address() == Some(address) {
                slot.fill(&slices)?;
            }
            Ok(())
        })?;
    }
    Ok((wanted, inline))
}

/// Fill one standalone published generation read outside the writer lane.
async fn hydrate_generation_slices_with_control(
    database: &Database,
    slice_table: &str,
    generation: &mut PublishedVectorGenerationV1,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<(), VectorGenerationStoreErrorV1> {
    ensure_vector_read_not_cancelled(is_cancelled)?;
    let mut wanted = BTreeSet::new();
    generation.visit_external_slots(&mut |slot| {
        if let Some(address) = slot.address() {
            wanted.insert(address.clone());
        }
        Ok(())
    })?;
    for address in &wanted {
        ensure_vector_read_not_cancelled(is_cancelled)?;
        let slices =
            read_state_slices_with_control(database, slice_table, address, is_cancelled).await?;
        generation.visit_external_slots(&mut |slot| {
            if slot.address() == Some(address) {
                slot.fill(&slices)?;
            }
            Ok(())
        })?;
    }
    Ok(())
}

/// Read one collection's slices in ordinal order.
///
/// Paged by ordinal rather than read as one statement: a whole-corpus
/// collection has more slices than a single query may materialize, and the
/// runtime refuses such a statement outright rather than truncating it. Paging
/// keeps every statement bounded no matter how large the collection grows.
async fn read_state_slices(
    database: &Database,
    slice_table: &str,
    address: &ContentDigest,
) -> Result<Vec<Vec<u8>>, VectorGenerationStoreErrorV1> {
    read_state_slices_with_control(database, slice_table, address, &|| false).await
}

async fn read_state_slices_with_control(
    database: &Database,
    slice_table: &str,
    address: &ContentDigest,
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<Vec<Vec<u8>>, VectorGenerationStoreErrorV1> {
    let connection = database.engine_conn();
    let sql = format!(
        "SELECT ordinal, payload
         FROM {slice_table}
         WHERE collection_digest = ?1 AND ordinal >= ?2 AND ordinal < ?3
         ORDER BY ordinal"
    );
    let mut slices = Vec::new();
    loop {
        ensure_vector_read_not_cancelled(is_cancelled)?;
        let start = i64::try_from(slices.len()).map_err(storage_error)?;
        let end = start
            .checked_add(i64::try_from(VECTOR_STATE_SLICE_READ_ROWS).map_err(storage_error)?)
            .ok_or_else(|| {
                VectorGenerationStoreErrorV1::Storage(
                    "externalized state collection is implausibly large".to_owned(),
                )
            })?;
        let mut rows = connection
            .query(&sql, params![address.as_str(), start, end])
            .await
            .map_err(storage_error)?;
        let mut read = 0_usize;
        while let Some(row) = rows.next().await.map_err(storage_error)? {
            ensure_vector_read_not_cancelled(is_cancelled)?;
            let ordinal = row.get::<i64>(0).map_err(storage_error)?;
            if usize::try_from(ordinal).ok() != Some(slices.len()) {
                return Err(VectorGenerationStoreErrorV1::Storage(format!(
                    "externalized state collection {address} has a gap in its slices"
                )));
            }
            slices.push(row.get::<Vec<u8>>(1).map_err(storage_error)?);
            read += 1;
        }
        drop(rows);
        if read < VECTOR_STATE_SLICE_READ_ROWS {
            break;
        }
    }
    if slices.is_empty() {
        return Err(VectorGenerationStoreErrorV1::Storage(format!(
            "externalized state collection {address} is missing from the store"
        )));
    }
    Ok(slices)
}

fn ensure_vector_read_not_cancelled(
    is_cancelled: &(dyn Fn() -> bool + Sync),
) -> Result<(), VectorGenerationStoreErrorV1> {
    if is_cancelled() {
        Err(VectorGenerationStoreErrorV1::Cancelled)
    } else {
        Ok(())
    }
}

/// Persist sealed collection slices inside the caller's transaction.
///
/// Rows are content-addressed and inserted with `OR IGNORE`, so a retried
/// commit is a no-op rather than a conflict, and every statement carries a
/// bounded number of bounded slices.
async fn write_state_slices(
    transaction: &tracedecay_runtime_core::db::DatabaseWriteTransaction<'_>,
    slice_table: &str,
    pending: &BTreeMap<ContentDigest, Vec<Vec<u8>>>,
) -> Result<(), VectorGenerationStoreErrorV1> {
    let rows = pending
        .iter()
        .flat_map(|(address, slices)| {
            slices
                .iter()
                .enumerate()
                .map(move |(ordinal, payload)| (address, ordinal, payload))
        })
        .collect::<Vec<_>>();
    for group in rows.chunks(VECTOR_STATE_SLICE_STATEMENT_ROWS) {
        let tuples = (0..group.len())
            .map(|index| {
                let base = index * 3;
                format!("(?{}, ?{}, ?{})", base + 1, base + 2, base + 3)
            })
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "INSERT OR IGNORE INTO {slice_table} (collection_digest, ordinal, payload)
             VALUES {tuples}"
        );
        let mut values = Vec::with_capacity(group.len() * 3);
        for (address, ordinal, payload) in group {
            values.push(tracedecay_runtime_core::db::engine::Value::Text(
                address.as_str().to_owned(),
            ));
            values.push(tracedecay_runtime_core::db::engine::Value::Integer(
                i64::try_from(*ordinal).map_err(storage_error)?,
            ));
            values.push(tracedecay_runtime_core::db::engine::Value::Blob(
                (*payload).clone(),
            ));
        }
        transaction
            .execute_engine(
                &sql,
                tracedecay_runtime_core::db::engine::params_from_iter(values),
            )
            .await
            .map_err(storage_error)?;
    }
    Ok(())
}
