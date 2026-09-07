use super::*;
use crate::exact_sql::ExactSqlRow;

fn locked(message: &str) -> ExactSqlError {
    ExactSqlError::Sqlite {
        operation: "prepare query",
        code: Some(5),
        extended_code: Some(261),
        message: message.to_owned(),
    }
}

fn rows() -> ExactSqlRows {
    ExactSqlRows {
        columns: vec!["row_kind".to_owned()],
        rows: vec![ExactSqlRow {
            values: vec![ExactSqlValue::Integer(0)],
        }],
    }
}

#[test]
fn coherent_capacity_query_retries_a_released_sqlite_lock() {
    let mut attempts = 0;
    let result = coherent_capacity_query(|| {
        attempts += 1;
        if attempts < 3 {
            Err(locked("database is locked"))
        } else {
            Ok(rows())
        }
    })
    .unwrap();

    assert_eq!(attempts, 3);
    assert_eq!(result.rows.len(), 1);
}

#[test]
fn coherent_capacity_query_exhausts_its_busy_attempt_budget() {
    let mut attempts = 0;
    let error = coherent_capacity_query(|| {
        attempts += 1;
        Err(locked(if attempts == 1 {
            "original database lock"
        } else {
            "later database lock"
        }))
    })
    .unwrap_err();

    assert_eq!(attempts, usize::from(COHERENT_CAPACITY_BUSY_ATTEMPTS));
    assert!(sqlite_busy_or_locked(&error));
    assert!(matches!(
        error,
        ExactSqlError::Sqlite { message, .. } if message == "original database lock"
    ));
}
