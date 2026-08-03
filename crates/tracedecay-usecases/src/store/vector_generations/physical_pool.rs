#[derive(Clone, Debug, PartialEq)]
struct SharedVectorBytesV1(Arc<[f32]>);

impl Serialize for SharedVectorBytesV1 {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.as_ref().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SharedVectorBytesV1 {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Vec::<f32>::deserialize(deserializer).map(|values| Self(Arc::from(values)))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct PhysicalVectorPayloadV1 {
    reuse_key: PhysicalVectorReuseKeyV1,
    values: SharedVectorBytesV1,
}

type PhysicalVectorPoolMapV1 = BTreeMap<PhysicalVectorReuseKeyV1, Weak<[f32]>>;

/// Sweep dead weak handles out of the pool once this many interns have
/// happened since the last sweep. A generation retire drops the strong
/// handles, so a sweep is what turns that retire into released memory.
const PHYSICAL_VECTOR_POOL_SWEEP_INTERVAL: usize = 4_096;

/// Hard ceiling on retained pool keys. Reaching it after a sweep means live
/// interned identities alone exceed the budget, so the pool is dropped
/// wholesale: interning stays correct without it — the next `intern` simply
/// allocates instead of sharing — and RSS is bounded by construction.
const PHYSICAL_VECTOR_POOL_MAX_ENTRIES: usize = 262_144;

#[derive(Default)]
struct PhysicalVectorPoolStateV1 {
    entries: PhysicalVectorPoolMapV1,
    interns_since_sweep: usize,
}

impl PhysicalVectorPoolStateV1 {
    fn sweep(&mut self) {
        self.entries.retain(|_, shared| shared.strong_count() > 0);
        self.interns_since_sweep = 0;
        if self.entries.len() > PHYSICAL_VECTOR_POOL_MAX_ENTRIES {
            self.entries.clear();
        }
    }
}

/// Process-wide physical byte interner. Complete projection and privacy
/// authority is part of the key, so sharing cannot cross either boundary.
///
/// Entries are weak handles, so retiring a generation already releases the
/// float payload; what used to leak was the *key* set, which grew for the
/// lifetime of the process across every project in the daemon. The pool now
/// sweeps dead entries on a fixed intern cadence and caps the live key set, so
/// a retired generation releases both its bytes and its keys.
#[derive(Clone)]
pub struct PhysicalVectorBytePoolV1 {
    entries: Arc<Mutex<PhysicalVectorPoolStateV1>>,
}

impl Default for PhysicalVectorBytePoolV1 {
    fn default() -> Self {
        static ENTRIES: std::sync::OnceLock<Arc<Mutex<PhysicalVectorPoolStateV1>>> =
            std::sync::OnceLock::new();
        Self {
            entries: Arc::clone(
                ENTRIES.get_or_init(|| Arc::new(Mutex::new(PhysicalVectorPoolStateV1::default()))),
            ),
        }
    }
}

impl PhysicalVectorBytePoolV1 {
    fn lock(
        &self,
    ) -> Result<std::sync::MutexGuard<'_, PhysicalVectorPoolStateV1>, VectorGenerationStoreErrorV1>
    {
        self.entries.lock().map_err(|_| {
            VectorGenerationStoreErrorV1::Storage(
                "physical vector byte pool lock is poisoned".to_string(),
            )
        })
    }

    fn intern(
        &self,
        reuse_key: &PhysicalVectorReuseKeyV1,
        values: &[f32],
    ) -> Result<Arc<[f32]>, VectorGenerationStoreErrorV1> {
        let mut pool = self.lock()?;
        if let Some(shared) = pool.entries.get(reuse_key).and_then(Weak::upgrade) {
            if shared.as_ref() != values {
                return Err(VectorGenerationStoreErrorV1::PhysicalVectorConflict);
            }
            return Ok(shared);
        }
        let shared: Arc<[f32]> = Arc::from(values.to_vec());
        pool.entries
            .insert(reuse_key.clone(), Arc::downgrade(&shared));
        pool.interns_since_sweep += 1;
        if pool.interns_since_sweep >= PHYSICAL_VECTOR_POOL_SWEEP_INTERVAL {
            pool.sweep();
        }
        Ok(shared)
    }

    /// Release every entry whose generation has been retired. Interning is
    /// unaffected: a swept key is re-interned on its next use.
    pub fn sweep_retired(&self) -> Result<(), VectorGenerationStoreErrorV1> {
        self.lock()?.sweep();
        Ok(())
    }

    #[cfg(test)]
    fn contains_key(&self, key: &PhysicalVectorReuseKeyV1) -> bool {
        self.entries
            .lock()
            .map(|pool| pool.entries.contains_key(key))
            .unwrap_or_default()
    }
}
