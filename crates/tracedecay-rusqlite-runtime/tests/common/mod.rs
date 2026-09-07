/// Platform-absolute fixture root: the work and registered-root contracts
/// require `Path::is_absolute`, which a bare `/...` literal fails on Windows.
pub fn fixture_abs_root(posix: &str) -> String {
    if cfg!(windows) {
        format!("C:{}", posix.replace('/', "\\"))
    } else {
        posix.to_owned()
    }
}
