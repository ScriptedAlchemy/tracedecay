use std::ffi::OsStr;

#[cfg(unix)]
pub(crate) fn native_os_str_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::unix::ffi::OsStrExt as _;
    value.as_bytes().to_vec()
}

#[cfg(windows)]
pub(crate) fn native_os_str_bytes(value: &OsStr) -> Vec<u8> {
    use std::os::windows::ffi::OsStrExt as _;
    value.encode_wide().flat_map(u16::to_le_bytes).collect()
}

#[cfg(not(any(unix, windows)))]
pub(crate) fn native_os_str_bytes(value: &OsStr) -> Vec<u8> {
    value.as_encoded_bytes().to_vec()
}
