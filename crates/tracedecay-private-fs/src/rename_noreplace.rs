#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::OsStr;
use std::io;
#[cfg(windows)]
use std::path::Path;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn rename_noreplace_at(
    from_parent: std::os::fd::RawFd,
    from: &OsStr,
    to_parent: std::os::fd::RawFd,
    to: &OsStr,
) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;

    let from = std::ffi::CString::new(from.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))?;
    let to = std::ffi::CString::new(to.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "NUL path"))?;
    #[cfg(target_os = "linux")]
    let result = unsafe {
        libc::renameat2(
            from_parent,
            from.as_ptr(),
            to_parent,
            to.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let result = unsafe {
        libc::renameatx_np(
            from_parent,
            from.as_ptr(),
            to_parent,
            to.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(windows)]
pub(crate) fn rename_noreplace_paths(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};

    let wide = |path: &Path| {
        path.as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>()
    };
    let source = wide(source);
    let destination = wide(destination);
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
