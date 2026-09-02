use tracedecay_runtime_core::db::engine::Value;

/// SQL scope filter rendered for one provider/session status scope.
///
/// The historical `(?1 = 'all' OR provider = ?1)` disjunction is not sargable:
/// SQLite cannot reduce an OR against a bound parameter to an index range, so
/// every consumer degraded to a full scan of the content-bearing tables and
/// paid for the whole store instead of the requested scope. On a multi-GB
/// session store that scan deterministically outlives the tool dispatch
/// deadline (issue #767). Rendering the exact predicate for the scope keeps
/// each read on the `(provider, session_id, …)` indexes.
///
/// The unbounded `all` scope renders no predicate at all (instead of a
/// constant-true one), which keeps SQLite's bare `COUNT(*)` optimization —
/// counting via the table b-tree instead of walking every index entry under a
/// tautology — available to the census queries.
///
/// Column names come from call-site literals; scope values are always bound.
pub(super) struct LcmScopeSql {
    terms: Vec<String>,
    values: Vec<Value>,
}

impl LcmScopeSql {
    pub(super) fn new(
        provider_column: &str,
        session_column: &str,
        provider: &str,
        session_id: Option<&str>,
    ) -> Self {
        let mut terms = Vec::new();
        let mut values = Vec::new();
        if provider != "all" {
            terms.push(format!("{provider_column} = ?"));
            values.push(Value::Text(provider.to_owned()));
        }
        if let Some(session_id) = session_id {
            terms.push(format!("{session_column} = ?"));
            values.push(Value::Text(session_id.to_owned()));
        }
        Self { terms, values }
    }

    /// `WHERE a = ? AND b = ?`, or empty for the unbounded scope.
    pub(super) fn where_clause(&self) -> String {
        if self.terms.is_empty() {
            String::new()
        } else {
            format!("WHERE {}", self.terms.join(" AND "))
        }
    }

    /// ` AND a = ? AND b = ?` for appending to an existing `WHERE`, or empty
    /// for the unbounded scope.
    pub(super) fn and_clause(&self) -> String {
        self.terms
            .iter()
            .map(|term| format!(" AND {term}"))
            .collect()
    }

    pub(super) fn values(&self) -> &[Value] {
        &self.values
    }

    pub(super) fn into_values(self) -> Vec<Value> {
        self.values
    }
}

#[cfg(test)]
mod tests {
    use super::LcmScopeSql;

    #[test]
    fn unbounded_scope_omits_predicates_and_binds_nothing() {
        let scope = LcmScopeSql::new("provider", "session_id", "all", None);
        assert!(scope.where_clause().is_empty());
        assert!(scope.and_clause().is_empty());
        assert!(scope.values().is_empty());
    }

    #[test]
    fn provider_scope_is_sargable_equality() {
        let scope = LcmScopeSql::new("provider", "session_id", "cursor", None);
        assert_eq!(scope.where_clause(), "WHERE provider = ?");
        assert_eq!(scope.and_clause(), " AND provider = ?");
        assert_eq!(scope.values().len(), 1);
    }

    #[test]
    fn session_scope_adds_both_equalities() {
        let scope = LcmScopeSql::new("provider", "session_id", "cursor", Some("session-a"));
        assert_eq!(
            scope.where_clause(),
            "WHERE provider = ? AND session_id = ?"
        );
        assert_eq!(scope.values().len(), 2);
    }
}
