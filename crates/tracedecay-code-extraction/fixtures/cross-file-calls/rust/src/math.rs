use crate::util::clamp;

pub fn total(values: &[i64]) -> i64 {
    values.iter().sum()
}

pub fn mean(values: &[i64]) -> i64 {
    total(values) / values.len() as i64
}

pub fn scale(value: i64, factor: i64) -> i64 {
    clamp(value * factor, 0, 100)
}
