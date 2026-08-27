//! Formatting utilities for query results and other display types.
//!
//! The main entry point is [`format_result_table`], which renders columns and rows
//! as a Unicode box-drawing table suitable for terminal output, `Display` impls,
//! and binding `__str__`/`toString` methods.

use std::fmt::Write;

use crate::types::Value;

/// Maximum display width for any single column before truncation.
const MAX_COLUMN_WIDTH: usize = 40;

/// Renders a query result as a Unicode box-drawing table.
///
/// Produces output like:
/// ```text
/// ┌────────┬─────┬──────────┐
/// │ name   │ age │ city     │
/// ├────────┼─────┼──────────┤
/// │ "Alix" │ 30  │ "Berlin" │
/// │ "Gus"  │ 28  │ "Paris"  │
/// └────────┴─────┴──────────┘
/// (2 rows)
/// ```
///
/// For DDL/status-only results, returns the status message.
/// For empty results with no columns, returns `"(empty)"`.
#[must_use]
pub fn format_result_table(
    columns: &[String],
    rows: &[Vec<Value>],
    execution_time_ms: Option<f64>,
    status_message: Option<&str>,
) -> String {
    // Status-only results (DDL commands)
    if let Some(msg) = status_message
        && columns.is_empty()
    {
        return if let Some(ms) = execution_time_ms {
            format!("{msg}\n({ms:.2} ms)")
        } else {
            msg.to_string()
        };
    }

    // Empty result with no columns
    if columns.is_empty() {
        return "(empty)".to_string();
    }

    // Format all cell values to strings
    let formatted_rows: Vec<Vec<String>> = rows
        .iter()
        .map(|row| row.iter().map(|v| v.to_string()).collect())
        .collect();

    // Calculate column widths: max of header and all cell values, capped
    let widths: Vec<usize> = columns
        .iter()
        .enumerate()
        .map(|(i, col)| {
            let header_width = col.len();
            let max_cell = formatted_rows
                .iter()
                .map(|row| row.get(i).map_or(0, String::len))
                .max()
                .unwrap_or(0);
            header_width.max(max_cell).min(MAX_COLUMN_WIDTH)
        })
        .collect();

    let mut out = String::new();

    // Top border: ┌────┬────┐
    write_border(&mut out, &widths, '┌', '┬', '┐');

    // Header row: │ name │ age │
    write_row(&mut out, columns, &widths);

    // Separator: ├────┼────┤
    write_border(&mut out, &widths, '├', '┼', '┤');

    // Data rows
    for row in &formatted_rows {
        let cells: Vec<&str> = row.iter().map(String::as_str).collect();
        write_row(&mut out, &cells, &widths);
    }

    // Bottom border: └────┴────┘
    write_border(&mut out, &widths, '└', '┴', '┘');

    // Footer
    let row_count = rows.len();
    let row_label = if row_count == 1 { "row" } else { "rows" };
    if let Some(ms) = execution_time_ms {
        write!(out, "({row_count} {row_label}, {ms:.2} ms)").unwrap();
    } else {
        write!(out, "({row_count} {row_label})").unwrap();
    }

    out
}

/// Writes a horizontal border line: `┌──────┬──────┐\n`
fn write_border(out: &mut String, widths: &[usize], left: char, mid: char, right: char) {
    out.push(left);
    for (i, &w) in widths.iter().enumerate() {
        if i > 0 {
            out.push(mid);
        }
        // +2 for padding on each side
        for _ in 0..w + 2 {
            out.push('─');
        }
    }
    out.push(right);
    out.push('\n');
}

/// Writes a data row: `│ value │ value │\n`
fn write_row(out: &mut String, cells: &[impl AsRef<str>], widths: &[usize]) {
    out.push('│');
    for (i, cell) in cells.iter().enumerate() {
        let w = widths.get(i).copied().unwrap_or(0);
        let text = cell.as_ref();
        if text.len() > w {
            // Truncate with ellipsis
            let truncated: String = text.chars().take(w.saturating_sub(1)).collect();
            write!(out, " {truncated}… ").unwrap();
        } else {
            write!(out, " {text:<w$} ", w = w).unwrap();
        }
        out.push('│');
    }
    out.push('\n');
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Value;

    #[test]
    fn test_empty_result_no_columns() {
        let result = format_result_table(&[], &[], None, None);
        assert_eq!(result, "(empty)");
    }

    #[test]
    fn test_status_only() {
        let result = format_result_table(&[], &[], None, Some("Created node type 'Person'"));
        assert_eq!(result, "Created node type 'Person'");
    }

    #[test]
    fn test_status_with_timing() {
        let result = format_result_table(&[], &[], Some(1.23), Some("Created node type 'Person'"));
        assert_eq!(result, "Created node type 'Person'\n(1.23 ms)");
    }

    #[test]
    fn test_single_column_no_rows() {
        let cols = vec!["name".to_string()];
        let result = format_result_table(&cols, &[], None, None);
        assert!(result.contains("name"));
        assert!(result.contains("(0 rows)"));
    }

    #[test]
    fn test_basic_table() {
        let cols = vec!["name".to_string(), "age".to_string()];
        let rows = vec![
            vec![Value::from("Alix"), Value::Int64(30)],
            vec![Value::from("Gus"), Value::Int64(28)],
        ];
        let result = format_result_table(&cols, &rows, None, None);

        // Check structure
        assert!(result.contains("┌"));
        assert!(result.contains("┐"));
        assert!(result.contains("├"));
        assert!(result.contains("┤"));
        assert!(result.contains("└"));
        assert!(result.contains("┘"));

        // Check content
        assert!(result.contains("name"));
        assert!(result.contains("age"));
        assert!(result.contains("\"Alix\""));
        assert!(result.contains("30"));
        assert!(result.contains("\"Gus\""));
        assert!(result.contains("28"));
        assert!(result.contains("(2 rows)"));
    }

    #[test]
    fn test_single_row_label() {
        let cols = vec!["x".to_string()];
        let rows = vec![vec![Value::Int64(1)]];
        let result = format_result_table(&cols, &rows, None, None);
        assert!(result.contains("(1 row)"));
    }

    #[test]
    fn test_with_execution_time() {
        let cols = vec!["n".to_string()];
        let rows = vec![vec![Value::Int64(42)]];
        let result = format_result_table(&cols, &rows, Some(1.50), None);
        assert!(result.contains("(1 row, 1.50 ms)"));
    }

    #[test]
    fn test_null_and_bool_values() {
        let cols = vec!["a".to_string(), "b".to_string()];
        let rows = vec![vec![Value::Null, Value::Bool(true)]];
        let result = format_result_table(&cols, &rows, None, None);
        assert!(result.contains("NULL"));
        assert!(result.contains("true"));
    }

    #[test]
    fn test_long_value_truncated() {
        let cols = vec!["data".to_string()];
        let long_str = "a".repeat(100);
        let rows = vec![vec![Value::from(long_str.as_str())]];
        let result = format_result_table(&cols, &rows, None, None);
        // Should contain the truncation ellipsis
        assert!(result.contains('…'));
    }
}
