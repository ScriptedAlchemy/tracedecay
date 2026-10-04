//! NTFS journal currency while every modifying handle is closed.
//!
//! USNs alone are insufficient: NTFS coalesces repeated writes through an
//! open handle. A read handle denying both write and delete sharing proves
//! those handles have closed. The journal epoch prevents a recreated journal
//! from reusing a persisted token. Unsupported volumes remain unvouched.

use std::fs::{Metadata, OpenOptions};
use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
use std::os::windows::io::AsRawHandle;
use std::path::Path;

use sha2::{Digest, Sha256};
use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
use windows_sys::Win32::System::IO::DeviceIoControl;
use windows_sys::Win32::System::Ioctl::{FSCTL_QUERY_USN_JOURNAL, FSCTL_READ_FILE_USN_DATA};

use crate::coarse_time::ChangeStamp;

pub(super) fn stamp(path: &Path, observed: &Metadata) -> Option<ChangeStamp> {
    if !observed.is_file() || observed.is_symlink() {
        return None;
    }
    // Deny write AND delete access, including already-open modifying handles.
    // With either kind still open, sharing admission fails and no token is
    // vouched for. This short-lived handle is released before callers read.
    let file = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(path)
        .ok()?;
    let actual = file.metadata().ok()?;
    if actual.len() != observed.len()
        || actual.creation_time() != observed.creation_time()
        || actual.last_write_time() != observed.last_write_time()
        || actual.file_attributes() != observed.file_attributes()
    {
        return None;
    }
    let identity = crate::windows_file::information(&file).ok()?;
    let epoch = journal_epoch(&file)?;
    let mut record = [0_u8; 512];
    // READ_FILE_USN_DATA: request only the NTFS V2 record layout.
    let versions = [2_u8, 0, 2, 0];
    let returned = control(&file, FSCTL_READ_FILE_USN_DATA, &versions, &mut record)?;
    if returned < 60 || u16::from_le_bytes(record[4..6].try_into().ok()?) != 2 {
        return None;
    }
    let record_length = usize::try_from(u32::from_le_bytes(record[..4].try_into().ok()?)).ok()?;
    if record_length < 60 || record_length > returned {
        return None;
    }
    let file_index = u64::from_le_bytes(record[8..16].try_into().ok()?);
    let usn = i64::from_le_bytes(record[24..32].try_into().ok()?);
    if file_index != identity.file_index || usn <= 0 || journal_epoch(&file)? != epoch {
        return None;
    }
    let mut digest = Sha256::new();
    digest.update(b"tracedecay.ntfs-closed-handles-witness.v1");
    digest.update(identity.volume_serial_number.to_le_bytes());
    digest.update(file_index.to_le_bytes());
    digest.update(epoch.to_le_bytes());
    digest.update(usn.to_le_bytes());
    let digest = digest.finalize();
    Some(ChangeStamp::Journal(digest[..16].try_into().ok()?))
}

fn journal_epoch(volume: &impl AsRawHandle) -> Option<u64> {
    let mut data = [0_u8; 128];
    let returned = control(volume, FSCTL_QUERY_USN_JOURNAL, &[], &mut data)?;
    if returned < 56 {
        return None;
    }
    let epoch = u64::from_le_bytes(data[..8].try_into().ok()?);
    (epoch != 0).then_some(epoch)
}

fn control(handle: &impl AsRawHandle, code: u32, input: &[u8], output: &mut [u8]) -> Option<usize> {
    let mut returned = 0_u32;
    // SAFETY: each buffer stays live for this synchronous call and its size
    // matches the supplied length. A null OVERLAPPED makes completion final.
    let succeeded = unsafe {
        DeviceIoControl(
            handle.as_raw_handle(),
            code,
            if input.is_empty() {
                std::ptr::null()
            } else {
                input.as_ptr().cast()
            },
            input.len() as u32,
            output.as_mut_ptr().cast(),
            output.len() as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if succeeded == 0 {
        return None;
    }
    let returned = usize::try_from(returned).ok()?;
    (returned <= output.len()).then_some(returned)
}

#[cfg(test)]
mod tests {
    use std::fs::FileTimes;
    use std::io::{Seek, SeekFrom, Write};

    use super::*;
    use crate::{ChangeClockReading, RewriteWitness};

    fn sample(path: &Path) -> ChangeStamp {
        let clock = ChangeClockReading::now();
        let metadata = std::fs::metadata(path).unwrap();
        RewriteWitness::native_path_stamp(path, &metadata, clock)
    }

    #[test]
    fn an_open_writer_never_vouches_for_repeated_restored_mtime_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("rollout.jsonl");
        std::fs::write(&path, b"first").unwrap();
        let original = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut writer = OpenOptions::new().write(true).open(&path).unwrap();
        let before = sample(&path);
        assert!(!before.is_settled());
        for bytes in [b"other", b"third"] {
            writer.seek(SeekFrom::Start(0)).unwrap();
            writer.write_all(bytes).unwrap();
            writer
                .set_times(FileTimes::new().set_modified(original))
                .unwrap();
            let next = sample(&path);
            assert!(!next.is_settled());
            assert_ne!(next, before);
        }
    }

    #[test]
    fn closed_writers_allow_journal_reuse_and_restored_mtime_invalidates_it() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("HEAD");
        std::fs::write(&path, b"first").unwrap();
        let original = std::fs::metadata(&path).unwrap().modified().unwrap();
        let before = sample(&path);
        let idle = sample(&path);
        if before.is_settled() {
            assert_eq!(before, idle, "idle NTFS files must reuse their witness");
        } else {
            assert_ne!(
                before, idle,
                "unsupported journals must never prove currency"
            );
        }
        {
            let mut writer = OpenOptions::new().write(true).open(&path).unwrap();
            writer.write_all(b"other").unwrap();
            writer
                .set_times(FileTimes::new().set_modified(original))
                .unwrap();
        }
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            original
        );
        let rewritten = sample(&path);
        assert_ne!(
            before, rewritten,
            "same-size, restored-mtime rewrites must invalidate the cache"
        );
        if rewritten.is_settled() {
            assert_eq!(rewritten, sample(&path));
        }
    }
}
