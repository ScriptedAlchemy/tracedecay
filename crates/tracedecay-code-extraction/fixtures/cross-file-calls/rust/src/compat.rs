use crate::legacy;
use crate::util::normalize as norm;

pub fn shim(text: &str) -> String {
    legacy::normalize(text)
}

pub fn upgrade(text: &str) -> String {
    norm(&shim(text))
}
