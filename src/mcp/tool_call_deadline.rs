//! The caller's request deadline, carried on `tools/call`.
//!
//! Every other transport already lets the caller name the deadline the daemon
//! must enforce: HTTP carries `x-tracedecay-deadline-micros`
//! (`crate::application_surface`), and the typed application-surface path sends
//! an absolute [`Deadline`] inside the invocation request. The MCP `tools/call`
//! wire — the route the CLI's compatibility tools and every MCP host take —
//! carried none, so the daemon fell back to the tool's canonical dispatch
//! ceiling for *every* caller. A caller asking for a one-second budget was
//! silently served on a thirty-second one, and the deadline-elapsed typed
//! terminals that budget exists to produce could never be observed.
//!
//! The deadline rides in the standard MCP `_meta` object on the request params,
//! under a namespaced key, as absolute UTC microseconds. It is a *request*
//! deadline, not a transport timeout: the client's own read bound must exceed it
//! by a bounded grace (see `crate::daemon::DAEMON_TOOL_RESPONSE_GRACE`) so an
//! envelope the daemon produced *because* the deadline elapsed is still read.

use serde_json::{Value, json};
use tracedecay_application::Deadline;
use tracedecay_domain::UtcMicros;

/// `_meta` key naming the caller's absolute request deadline, in UTC micros.
pub(crate) const TOOL_CALL_DEADLINE_META_KEY: &str = "tracedecay/deadline-micros";

/// The `_meta` object a client attaches to `tools/call` params.
pub(crate) fn tool_call_deadline_meta(expires_at: UtcMicros) -> Value {
    json!({ TOOL_CALL_DEADLINE_META_KEY: expires_at.0 })
}

/// The caller's request deadline, when the request declared one.
///
/// An unparseable or non-integer value is *not* a deadline: it is dropped, and
/// the daemon falls back to the tool's canonical ceiling exactly as before.
pub(crate) fn caller_tool_call_deadline(params: Option<&Value>) -> Option<Deadline> {
    let micros = params?
        .get("_meta")?
        .get(TOOL_CALL_DEADLINE_META_KEY)?
        .as_i64()?;
    Deadline::new(UtcMicros(micros)).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_declared_deadline_round_trips_through_the_meta_object() {
        let params = json!({
            "name": "tracedecay_fact_store_add",
            "arguments": {},
            "_meta": tool_call_deadline_meta(UtcMicros(1_765_000_000_000_000)),
        });
        assert_eq!(
            caller_tool_call_deadline(Some(&params)).map(|deadline| deadline.expires_at),
            Some(UtcMicros(1_765_000_000_000_000))
        );
    }

    #[test]
    fn params_without_a_declared_deadline_leave_the_ceiling_in_charge() {
        assert!(caller_tool_call_deadline(None).is_none());
        assert!(caller_tool_call_deadline(Some(&json!({"name": "t"}))).is_none());
        assert!(caller_tool_call_deadline(Some(&json!({"_meta": {}}))).is_none());
        assert!(
            caller_tool_call_deadline(Some(&json!({
                "_meta": { TOOL_CALL_DEADLINE_META_KEY: "not-an-integer" }
            })))
            .is_none()
        );
    }
}
