use std::io::Write;
use tempfile::NamedTempFile;
use tracedecay_runtime_core::sync::*;

#[test]
fn test_read_source_file_utf8() {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"fn main() {}").unwrap();
    let content = read_source_file(f.path()).unwrap();
    assert_eq!(content, "fn main() {}");
}

#[test]
fn test_read_source_file_utf8_bom() {
    let mut f = NamedTempFile::new().unwrap();
    f.write_all(b"\xEF\xBB\xBFfn main() {}").unwrap();
    let content = read_source_file(f.path()).unwrap();
    assert_eq!(content, "fn main() {}");
}

#[test]
fn test_read_source_file_utf16_le() {
    let mut f = NamedTempFile::new().unwrap();
    // UTF-16 LE BOM + "hi"
    f.write_all(b"\xFF\xFE\x68\x00\x69\x00").unwrap();
    let content = read_source_file(f.path()).unwrap();
    assert_eq!(content, "hi");
}

#[test]
fn test_read_source_file_utf16_be() {
    let mut f = NamedTempFile::new().unwrap();
    // UTF-16 BE BOM + "hi"
    f.write_all(b"\xFE\xFF\x00\x68\x00\x69").unwrap();
    let content = read_source_file(f.path()).unwrap();
    assert_eq!(content, "hi");
}

#[test]
fn test_read_source_file_invalid_encoding() {
    let mut f = NamedTempFile::new().unwrap();
    // Invalid UTF-8 sequence without any BOM
    f.write_all(b"\x80\x81\x82\x83").unwrap();
    let error = read_source_file(f.path()).unwrap_err();
    assert_eq!(
        error.to_string(),
        "invalid utf-8 sequence of 1 bytes from index 0"
    );
}
