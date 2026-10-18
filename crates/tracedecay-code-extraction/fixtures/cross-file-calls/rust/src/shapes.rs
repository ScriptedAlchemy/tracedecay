use crate::math;
use crate::util::clamp;

pub fn area(width: i64, height: i64) -> i64 {
    clamp(width, 0, 100) * height
}

pub fn perimeter(width: i64, height: i64) -> i64 {
    math::total(&[clamp(width, 0, 100), height]) * 2
}
