use crate::math::mean;
use crate::util as u;

pub fn format_line(value: &str) -> String {
    u::normalize(value) + "\n"
}

pub fn summary(values: &[i64]) -> String {
    format_line(&mean(values).to_string())
}
