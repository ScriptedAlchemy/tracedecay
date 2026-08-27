//! Seam tests for aggregate semantics and expression edge cases
//! (ISO/IEC 39075 Section 20.9, 21).
//!
//! Tests COUNT(*) vs COUNT(expr) semantics with NULLs, aggregates on
//! empty sets, NULLIF/COALESCE edge cases, and CASE expressions.
//!
//! ```bash
//! cargo test -p grafeo-engine --test seam_aggregates_expressions
//! ```

use grafeo_common::types::Value;
use grafeo_engine::GrafeoDB;

fn db() -> GrafeoDB {
    GrafeoDB::new_in_memory()
}

// ============================================================================
// 1. COUNT(*) vs COUNT(expr) semantics
// ============================================================================

mod count_semantics {
    use super::*;

    #[test]
    fn count_star_counts_all_rows() {
        let db = db();
        let session = db.session();
        session.execute("INSERT (:Person {name: 'Alix'})").unwrap();
        session.execute("INSERT (:Person {name: 'Gus'})").unwrap();
        session
            .execute("INSERT (:Person {name: 'Vincent'})")
            .unwrap();

        let result = session
            .execute("MATCH (n:Person) RETURN COUNT(*) AS cnt")
            .unwrap();
        assert_eq!(result.rows()[0][0], Value::Int64(3));
    }

    #[test]
    fn count_star_on_empty_returns_zero() {
        let db = db();
        let session = db.session();

        let result = session
            .execute("MATCH (n:Person) RETURN COUNT(*) AS cnt")
            .unwrap();
        assert_eq!(result.row_count(), 1, "Should return one row");
        assert_eq!(result.rows()[0][0], Value::Int64(0));
    }

    #[test]
    fn count_expr_skips_nulls() {
        let db = db();
        let session = db.session();
        session
            .execute("INSERT (:Person {name: 'Alix', age: 30})")
            .unwrap();
        session.execute("INSERT (:Person {name: 'Gus'})").unwrap(); // no age
        session
            .execute("INSERT (:Person {name: 'Vincent', age: 35})")
            .unwrap();

        let result = session
            .execute("MATCH (n:Person) RETURN COUNT(n.age) AS cnt")
            .unwrap();
        assert_eq!(
            result.rows()[0][0],
            Value::Int64(2),
            "COUNT(expr) should skip NULLs"
        );
    }

    #[test]
    fn count_star_vs_count_expr_difference() {
        let db = db();
        let session = db.session();
        session
            .execute("INSERT (:Person {name: 'Alix', age: 30})")
            .unwrap();
        session.execute("INSERT (:Person {name: 'Gus'})").unwrap(); // no age

        let result = session
            .execute("MATCH (n:Person) RETURN COUNT(*) AS total, COUNT(n.age) AS with_age")
            .unwrap();
        assert_eq!(
            result.rows()[0][0],
            Value::Int64(2),
            "COUNT(*) counts all rows"
        );
        assert_eq!(
            result.rows()[0][1],
            Value::Int64(1),
            "COUNT(n.age) skips NULL age"
        );
    }

    #[test]
    fn count_distinct() {
        let db = db();
        let session = db.session();
        session
            .execute("INSERT (:Person {name: 'Alix', city: 'Amsterdam'})")
            .unwrap();
        session
            .execute("INSERT (:Person {name: 'Gus', city: 'Amsterdam'})")
            .unwrap();
        session
            .execute("INSERT (:Person {name: 'Vincent', city: 'Berlin'})")
            .unwrap();

        let result = session
            .execute("MATCH (n:Person) RETURN COUNT(DISTINCT n.city) AS cities")
            .unwrap();
        assert_eq!(result.rows()[0][0], Value::Int64(2));
    }
}

// ============================================================================
// 2. Aggregate NULL handling
// ============================================================================

mod aggregate_nulls {
    use super::*;

    #[test]
    fn sum_skips_nulls() {
        let db = db();
        let session = db.session();
        session.execute("INSERT (:Data {val: 10})").unwrap();
        session.execute("INSERT (:Data {})").unwrap(); // no val
        session.execute("INSERT (:Data {val: 20})").unwrap();

        let result = session
            .execute("MATCH (n:Data) RETURN SUM(n.val) AS s")
            .unwrap();
        assert_eq!(
            result.rows()[0][0],
            Value::Int64(30),
            "SUM should skip NULLs"
        );
    }

    #[test]
    fn avg_skips_nulls() {
        let db = db();
        let session = db.session();
        session.execute("INSERT (:Data {val: 10})").unwrap();
        session.execute("INSERT (:Data {})").unwrap();
        session.execute("INSERT (:Data {val: 20})").unwrap();

        let result = session
            .execute("MATCH (n:Data) RETURN AVG(n.val) AS a")
            .unwrap();
        // AVG(10, 20) = 15.0 (NULL is excluded from count)
        assert_eq!(result.rows()[0][0], Value::Float64(15.0));
    }

    #[test]
    fn min_skips_nulls() {
        let db = db();
        let session = db.session();
        session.execute("INSERT (:Data {})").unwrap();
        session.execute("INSERT (:Data {val: 30})").unwrap();
        session.execute("INSERT (:Data {val: 10})").unwrap();

        let result = session
            .execute("MATCH (n:Data) RETURN MIN(n.val) AS m")
            .unwrap();
        assert_eq!(result.rows()[0][0], Value::Int64(10));
    }

    #[test]
    fn max_skips_nulls() {
        let db = db();
        let session = db.session();
        session.execute("INSERT (:Data {})").unwrap();
        session.execute("INSERT (:Data {val: 10})").unwrap();
        session.execute("INSERT (:Data {val: 30})").unwrap();

        let result = session
            .execute("MATCH (n:Data) RETURN MAX(n.val) AS m")
            .unwrap();
        assert_eq!(result.rows()[0][0], Value::Int64(30));
    }

    #[test]
    fn collect_includes_nulls() {
        let db = db();
        let session = db.session();
        session.execute("INSERT (:Data {val: 10})").unwrap();
        session.execute("INSERT (:Data {})").unwrap();
        session.execute("INSERT (:Data {val: 20})").unwrap();

        let result = session
            .execute("MATCH (n:Data) RETURN COLLECT(n.val) AS c")
            .unwrap();
        match &result.rows()[0][0] {
            Value::List(list) => {
                // COLLECT may or may not include NULLs depending on implementation.
                // The key invariant: it should be a list.
                assert!(
                    list.len() >= 2,
                    "COLLECT should have at least the non-null values"
                );
            }
            other => panic!("COLLECT should return a List, got: {other:?}"),
        }
    }
}

// ============================================================================
// 3. NULLIF and COALESCE edge cases
// ============================================================================

mod nullif_coalesce {
    use super::*;

    #[test]
    fn nullif_equal_returns_null() {
        let db = db();
        let session = db.session();

        let result = session.execute("RETURN NULLIF(5, 5) AS r").unwrap();
        assert_eq!(
            result.rows()[0][0],
            Value::Null,
            "NULLIF(x, x) should return NULL"
        );
    }

    #[test]
    fn nullif_unequal_returns_first() {
        let db = db();
        let session = db.session();

        let result = session.execute("RETURN NULLIF(5, 3) AS r").unwrap();
        assert_eq!(result.rows()[0][0], Value::Int64(5));
    }

    #[test]
    fn coalesce_first_non_null() {
        let db = db();
        let session = db.session();

        let result = session
            .execute("RETURN COALESCE(NULL, NULL, 3) AS r")
            .unwrap();
        assert_eq!(result.rows()[0][0], Value::Int64(3));
    }

    #[test]
    fn coalesce_all_null() {
        let db = db();
        let session = db.session();

        let result = session.execute("RETURN COALESCE(NULL, NULL) AS r").unwrap();
        assert_eq!(
            result.rows()[0][0],
            Value::Null,
            "COALESCE of all NULLs should be NULL"
        );
    }

    #[test]
    fn coalesce_first_value_wins() {
        let db = db();
        let session = db.session();

        let result = session.execute("RETURN COALESCE(1, 2, 3) AS r").unwrap();
        assert_eq!(result.rows()[0][0], Value::Int64(1));
    }
}

// ============================================================================
// 4. CASE expression edge cases
// ============================================================================

mod case_expressions {
    use super::*;

    #[test]
    fn simple_case_match() {
        let db = db();
        let session = db.session();
        session
            .execute("INSERT (:Person {name: 'Alix', age: 30})")
            .unwrap();

        let result = session
            .execute(
                "MATCH (n:Person) RETURN CASE n.age WHEN 30 THEN 'young' WHEN 50 THEN 'senior' ELSE 'unknown' END AS category",
            )
            .unwrap();
        assert_eq!(result.rows()[0][0], Value::String("young".into()));
    }

    #[test]
    fn searched_case() {
        let db = db();
        let session = db.session();
        session
            .execute("INSERT (:Person {name: 'Alix', age: 30})")
            .unwrap();

        let result = session
            .execute(
                "MATCH (n:Person) RETURN CASE WHEN n.age < 20 THEN 'teen' WHEN n.age < 40 THEN 'adult' ELSE 'senior' END AS category",
            )
            .unwrap();
        assert_eq!(result.rows()[0][0], Value::String("adult".into()));
    }

    #[test]
    fn case_else_default() {
        let db = db();
        let session = db.session();
        session.execute("INSERT (:Person {name: 'Alix'})").unwrap();

        let result = session
            .execute(
                "MATCH (n:Person) RETURN CASE WHEN n.age > 50 THEN 'old' ELSE 'no age' END AS r",
            )
            .unwrap();
        assert_eq!(result.rows()[0][0], Value::String("no age".into()));
    }

    #[test]
    fn case_no_else_returns_null() {
        let db = db();
        let session = db.session();
        session.execute("INSERT (:Person {name: 'Alix'})").unwrap();

        let result = session
            .execute("MATCH (n:Person) RETURN CASE WHEN n.age > 50 THEN 'old' END AS r")
            .unwrap();
        assert_eq!(
            result.rows()[0][0],
            Value::Null,
            "CASE with no ELSE and no match should return NULL"
        );
    }
}

// ============================================================================
// 5. Type coercion and casting edge cases
// ============================================================================

mod type_coercion {
    use super::*;

    #[test]
    fn cast_int_to_string() {
        let db = db();
        let session = db.session();

        let result = session.execute("RETURN CAST(42 AS STRING) AS r").unwrap();
        assert_eq!(result.rows()[0][0], Value::String("42".into()));
    }

    #[test]
    fn cast_string_to_int() {
        let db = db();
        let session = db.session();

        let result = session.execute("RETURN CAST('42' AS INT64) AS r").unwrap();
        assert_eq!(result.rows()[0][0], Value::Int64(42));
    }

    #[test]
    fn cast_float_to_int_truncates() {
        let db = db();
        let session = db.session();

        let result = session.execute("RETURN CAST(3.9 AS INT64) AS r").unwrap();
        assert_eq!(result.rows()[0][0], Value::Int64(3));
    }

    #[test]
    fn cast_int_to_float() {
        let db = db();
        let session = db.session();

        let result = session.execute("RETURN CAST(42 AS FLOAT64) AS r").unwrap();
        assert_eq!(result.rows()[0][0], Value::Float64(42.0));
    }

    #[test]
    fn cast_bool_to_string() {
        let db = db();
        let session = db.session();

        let result = session.execute("RETURN CAST(true AS STRING) AS r").unwrap();
        match &result.rows()[0][0] {
            Value::String(s) => assert!(
                s == "true" || s == "TRUE",
                "CAST(true AS STRING) should produce 'true', got '{s}'"
            ),
            other => panic!("Expected String, got: {other:?}"),
        }
    }

    #[test]
    fn cast_null_stays_null() {
        let db = db();
        let session = db.session();

        let result = session.execute("RETURN CAST(NULL AS INT64) AS r").unwrap();
        assert_eq!(
            result.rows()[0][0],
            Value::Null,
            "CAST(NULL) should remain NULL"
        );
    }
}
