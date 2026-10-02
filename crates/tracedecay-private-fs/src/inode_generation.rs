//! Linux inode generation (`FS_IOC_GETVERSION`).
//!
//! Ext4 recycles an inode number as soon as the previous file is unlinked.
//! `CLOCK_REALTIME_COARSE` can stamp the replacement with the same ctime and
//! mtime, so path, inode, size, and timestamps do not identify the new file.
//! The generation counter advances on each allocation.

use std::fs::File;
use std::io;
#[cfg(target_os = "linux")]
use std::os::fd::AsRawFd;

/// Inode generation for `file`, when the filesystem reports one.
///
/// `Ok(None)` means this filesystem does not implement `FS_IOC_GETVERSION`.
/// Any other failure is returned so a caller does not store a missing reading
/// as a stable generation.
pub fn inode_generation(file: &File) -> io::Result<Option<u64>> {
    #[cfg(target_os = "linux")]
    {
        let mut generation: libc::c_long = 0;
        // SAFETY: `generation` is a writable `c_long`, and `FS_IOC_GETVERSION`
        // writes that width for this open file descriptor.
        let result =
            unsafe { libc::ioctl(file.as_raw_fd(), libc::FS_IOC_GETVERSION, &mut generation) };
        if result == 0 {
            return Ok(Some(generation_bits(generation)));
        }
        let error = io::Error::last_os_error();
        match error.raw_os_error() {
            Some(code)
                if code == libc::ENOTTY
                    || code == libc::EOPNOTSUPP
                    || code == libc::ENOSYS
                    || code == libc::EINVAL =>
            {
                Ok(None)
            }
            _ => Err(error),
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = file;
        Ok(None)
    }
}

#[cfg(target_os = "linux")]
fn generation_bits(generation: libc::c_long) -> u64 {
    let bytes = generation.to_le_bytes();
    let mut wide = [0u8; 8];
    wide[..bytes.len()].copy_from_slice(&bytes);
    u64::from_le_bytes(wide)
}

#[cfg(test)]
mod tests {
    #[cfg(target_os = "linux")]
    use super::inode_generation;

    #[cfg(target_os = "linux")]
    #[test]
    fn replacing_a_file_advances_inode_generation() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("rollout.jsonl");
        std::fs::write(&path, b"{}\n").unwrap();
        let Some(first) = inode_generation(&std::fs::File::open(&path).unwrap()).unwrap() else {
            // tmpfs (a container's /tmp) and ZFS do not implement
            // FS_IOC_GETVERSION; only ext4 is required to report one here.
            assert!(!is_ext4(temp.path()), "ext4 reports an inode generation");
            eprintln!(
                "skipping: the filesystem under {} reports no inode generation",
                temp.path().display()
            );
            return;
        };
        std::fs::remove_file(&path).unwrap();
        std::fs::write(&path, b"{}\n").unwrap();
        let second = inode_generation(&std::fs::File::open(&path).unwrap())
            .unwrap()
            .expect("ext4 reports an inode generation");
        assert_ne!(first, second);
    }

    #[cfg(target_os = "linux")]
    fn is_ext4(path: &std::path::Path) -> bool {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `statfs` is a plain C struct, valid when zeroed.
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `path` is NUL-terminated and `stat` is a writable `statfs`.
        assert_eq!(unsafe { libc::statfs(path.as_ptr(), &mut stat) }, 0);
        stat.f_type == libc::EXT4_SUPER_MAGIC
    }
}
