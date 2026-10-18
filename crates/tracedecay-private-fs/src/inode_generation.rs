//! Linux inode generation (`FS_IOC_GETVERSION`).
//!
//! Ext4 recycles an inode number as soon as the previous file is unlinked.
//! `CLOCK_REALTIME_COARSE` can stamp the replacement with the same ctime and
//! mtime, so path, inode, size, and timestamps do not identify the new file.
//! The generation exists so NFS can tell such a reuse apart: the kernel keeps
//! the (inode number, generation) pair unique across allocations, and nothing
//! more. Ext4 advances the generation on each allocation; ZFS and btrfs stamp
//! it with the creating transaction, so two files created in one transaction
//! share it, and hand out fresh inode numbers instead. A caller that keeps the
//! generation without the inode number holds no replacement witness.

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
    use std::os::unix::ffi::OsStrExt;
    #[cfg(target_os = "linux")]
    use std::os::unix::fs::MetadataExt;
    #[cfg(target_os = "linux")]
    use std::path::Path;

    #[cfg(target_os = "linux")]
    use super::inode_generation;

    /// Writes `path` and reads back the allocation identity the kernel keeps
    /// unique: the inode number with the generation, `None` where the
    /// filesystem answers `FS_IOC_GETVERSION` with no generation at all.
    #[cfg(target_os = "linux")]
    fn write_and_identify(path: &Path) -> (u64, Option<u64>) {
        std::fs::write(path, b"{}\n").unwrap();
        let file = std::fs::File::open(path).unwrap();
        (
            file.metadata().unwrap().ino(),
            inode_generation(&file).unwrap(),
        )
    }

    /// Ext4 recycles the inode number and advances the generation; ZFS keeps
    /// the generation and allocates a new inode number; tmpfs reports no
    /// generation and allocates a new inode number. The pair differs on all
    /// of them, and neither field is required to on its own.
    #[cfg(target_os = "linux")]
    #[test]
    fn replacing_a_file_changes_its_inode_allocation_identity() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("rollout.jsonl");
        let first = write_and_identify(&path);
        std::fs::remove_file(&path).unwrap();
        let second = write_and_identify(&path);
        assert_ne!(
            first, second,
            "a replacement must not share the predecessor's identity"
        );
        if is_ext4(temp.path()) {
            assert!(
                first.1.is_some() && second.1.is_some(),
                "ext4 reports an inode generation: {first:?} {second:?}"
            );
        }
    }

    #[cfg(target_os = "linux")]
    fn is_ext4(path: &std::path::Path) -> bool {
        let path = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: `statfs` is a plain C struct, valid when zeroed.
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        // SAFETY: `path` is NUL-terminated and `stat` is a writable `statfs`.
        assert_eq!(unsafe { libc::statfs(path.as_ptr(), &mut stat) }, 0);
        stat.f_type == libc::EXT4_SUPER_MAGIC
    }
}
