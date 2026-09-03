use std::path::Path;

use crate::config::TraceDecayConfig;
use crate::tracedecay::TraceDecay;
use tracedecay_domain::errors::{Result, TraceDecayError};

fn parse_counter(key: &'static str, value: Option<String>) -> Result<u64> {
    let Some(value) = value else {
        return Ok(0);
    };
    value
        .parse::<u64>()
        .map_err(|error| TraceDecayError::Database {
            operation: format!("read {key}"),
            message: format!("persisted {key} counter is invalid: {error}"),
        })
}

impl TraceDecay {
    /// Returns the persisted tokens-saved counter.
    #[hotpath::measure(label = "daemon.store_meta.read_tokens_saved", future = true)]
    pub async fn get_tokens_saved(&self) -> Result<u64> {
        parse_counter("tokens_saved", self.db.get_metadata("tokens_saved").await?)
    }

    /// Persists the tokens-saved counter to the database.
    #[hotpath::measure(label = "daemon.store_meta.write_tokens_saved", future = true)]
    pub async fn set_tokens_saved(&self, value: u64) -> Result<()> {
        self.db
            .set_metadata("tokens_saved", &value.to_string())
            .await
    }

    /// Returns the resettable project-local token counter.
    ///
    /// This is separate from the main `tokens_saved` counter and can be
    /// independently reset via [`Self::reset_local_counter`].
    #[hotpath::measure(label = "daemon.store_meta.read_local_counter", future = true)]
    pub async fn get_local_counter(&self) -> Result<u64> {
        parse_counter(
            "local_counter",
            self.db.get_metadata("local_counter").await?,
        )
    }

    /// Resets the project-local token counter to zero.
    #[hotpath::measure(label = "daemon.store_meta.reset_local_counter", future = true)]
    pub async fn reset_local_counter(&self) -> Result<()> {
        self.db.set_metadata("local_counter", "0").await
    }

    /// Increments the project-local token counter by the given amount.
    #[hotpath::measure(label = "daemon.store_meta.add_local_counter", future = true)]
    pub async fn add_local_counter(&self, delta: u64) -> Result<()> {
        let transaction = self.db.begin_write_transaction("add local counter").await?;
        let current = self.get_local_counter().await?;
        let updated = current
            .checked_add(delta)
            .ok_or_else(|| TraceDecayError::Database {
                operation: "add local counter".to_owned(),
                message: "local_counter overflowed u64".to_owned(),
            })?;
        self.db
            .set_metadata_unguarded(&transaction, "local_counter", &updated.to_string())
            .await?;
        transaction.commit().await
    }

    /// Checkpoints the WAL and closes the database connection.
    #[hotpath::measure(label = "daemon.store_meta.checkpoint", future = true)]
    pub async fn checkpoint(&self) -> Result<()> {
        self.db.checkpoint().await
    }

    /// Consumes the code graph and closes the database connection.
    pub fn close(self) {
        self.db.close();
    }

    /// Run the quick integrity check and return the first problem row, if any.
    #[hotpath::measure(label = "daemon.store_meta.quick_check", future = true)]
    pub(crate) async fn quick_check_report(&self) -> Result<Option<String>> {
        self.db.quick_check_report().await
    }

    /// Returns a reference to the current configuration.
    pub fn get_config(&self) -> &TraceDecayConfig {
        &self.config
    }

    /// Returns the project root path.
    pub fn project_root(&self) -> &Path {
        &self.project_root
    }
}
