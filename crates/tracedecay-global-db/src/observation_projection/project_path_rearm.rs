//! Session projection used to refuse a Codex observation whose session row
//! already stored a different concrete `project_path`, parking the queue row
//! on the terminal deadline with that collision as its last error. Projection
//! now keeps the stored path, so those rows are re-armed once per store and
//! project their messages. Collisions on any other field stay parked.

use tracedecay_runtime_core::db::Database;
use tracedecay_runtime_core::db::engine::params;
use tracedecay_runtime_core::db::schema_version;
use tracedecay_store::{PROJECTION_TERMINAL_RETRY_MICROS, ProjectionStoreResult};

use super::state::storage;

const OPERATION: &str = "rearm Codex project path collisions";
/// Recorded once the rows parked on the obsolete `project_path` collision are
/// re-armed.
const CONVERGENCE: &str = "codex-project-path-collision-rearm-v1";
const PARKED_ERROR_PATTERN: &str =
    "projection session output collided at codex/%: project_path differs%";

/// Re-arms queue rows parked terminal only because of the `project_path`
/// collision, once per store, and returns how many it re-armed.
pub(crate) async fn rearm_project_path_collisions(
    database: &Database,
) -> ProjectionStoreResult<u64> {
    let transaction = database
        .begin_write_transaction(OPERATION)
        .await
        .map_err(|error| storage(OPERATION, error))?;
    let converged = schema_version::read_version(&transaction, CONVERGENCE)
        .await
        .map_err(|error| storage(OPERATION, error))?
        .is_some();
    if converged {
        transaction
            .commit()
            .await
            .map_err(|error| storage(OPERATION, error))?;
        return Ok(0);
    }
    let rows = transaction
        .execute(
            "UPDATE projection_queue SET next_retry_at_micros = 0
             WHERE next_retry_at_micros = ?1 AND last_error LIKE ?2",
            params![PROJECTION_TERMINAL_RETRY_MICROS, PARKED_ERROR_PATTERN],
        )
        .await
        .map_err(|error| storage(OPERATION, error))?;
    schema_version::record_version(&transaction, CONVERGENCE, 1)
        .await
        .map_err(|error| storage(OPERATION, error))?;
    transaction
        .commit()
        .await
        .map_err(|error| storage(OPERATION, error))?;
    Ok(rows)
}
