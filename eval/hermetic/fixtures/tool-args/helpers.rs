/// Sums a slice; exists so the fixture graph has more than one file.
fn sum_all(values: &[i32]) -> i32 {
    values.iter().sum()
}

/// Doubles a value via `sum_all` so caller/callee scenarios have an edge.
fn double(value: i32) -> i32 {
    sum_all(&[value, value])
}
