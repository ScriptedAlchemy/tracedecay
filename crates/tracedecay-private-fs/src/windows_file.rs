//! Windows file-handle identity read via `GetFileInformationByHandle`.

use std::io;
use std::mem::{MaybeUninit, size_of};
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, FILE_BASIC_INFO, FileBasicInfo, GetFileInformationByHandle,
    GetFileInformationByHandleEx,
};

#[derive(Clone, Copy)]
pub struct FileInformation {
    pub volume_serial_number: u32,
    pub file_index: u64,
    pub number_of_links: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FileChangeToken {
    pub last_write_time: i64,
    pub change_time: i64,
}

/// Reads by-handle identity from any open Windows handle: `std::fs::File`,
/// and the `cap_std` `Dir`/`File` capabilities whose `Metadata` only exposes
/// the volume serial number and file index on nightly.
pub fn information<H: AsRawHandle>(file: &H) -> io::Result<FileInformation> {
    identity_information(raw_information(file)?)
}

/// Reads only the hard-link count from an open Windows file handle.
///
/// Unlike [`information`], this does not require the volume to expose a
/// durable file identity.
pub fn number_of_links<H: AsRawHandle>(file: &H) -> io::Result<u32> {
    Ok(raw_information(file)?.number_of_links)
}

fn raw_information<H: AsRawHandle>(file: &H) -> io::Result<FileInformation> {
    let mut information = MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    // SAFETY: `file` owns a valid Windows file handle, and `information` points
    // to writable memory sized for the API's complete output structure.
    let succeeded =
        unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: A nonzero API result initializes every field of the output structure.
    let information = unsafe { information.assume_init() };
    Ok(file_information(
        information.dwVolumeSerialNumber,
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
        information.nNumberOfLinks,
    ))
}

fn file_information(
    volume_serial_number: u32,
    file_index: u64,
    number_of_links: u32,
) -> FileInformation {
    FileInformation {
        volume_serial_number,
        file_index,
        number_of_links,
    }
}

fn identity_information(information: FileInformation) -> io::Result<FileInformation> {
    if information.volume_serial_number == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows by-handle identity has a zero volume serial number",
        ));
    }
    if information.file_index == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows by-handle identity has a zero file index",
        ));
    }
    if information.file_index == u64::MAX {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows by-handle identity returned the unsupported maximum file-index sentinel",
        ));
    }
    Ok(information)
}

/// Reads the native last-write and change times from one open Windows handle.
///
/// `ChangeTime` advances for in-place writes even when a caller restores
/// `LastWriteTime`, so the pair is an authoritative process-local cache
/// witness for the opened file identity.
pub fn change_token<H: AsRawHandle>(file: &H) -> io::Result<FileChangeToken> {
    let mut information = MaybeUninit::<FILE_BASIC_INFO>::uninit();
    // SAFETY: `file` owns a valid Windows file handle, and `information` points
    // to writable memory sized for the API's complete output structure.
    let succeeded = unsafe {
        GetFileInformationByHandleEx(
            file.as_raw_handle(),
            FileBasicInfo,
            information.as_mut_ptr().cast(),
            size_of::<FILE_BASIC_INFO>() as u32,
        )
    };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: A nonzero API result initializes every field of the output structure.
    let information = unsafe { information.assume_init() };
    Ok(FileChangeToken {
        last_write_time: information.LastWriteTime,
        change_time: information.ChangeTime,
    })
}

#[cfg(test)]
mod tests {
    use std::fs::{self, FileTimes, OpenOptions};
    use std::io::{ErrorKind, Seek, SeekFrom, Write};

    use tempfile::tempdir;

    use super::{change_token, file_information, identity_information};

    #[test]
    fn by_handle_identity_rejects_unprovable_and_sentinel_values() {
        let zero_volume = identity_information(file_information(0, 41, 1))
            .err()
            .expect("a zero volume serial number proves no identity");
        assert_eq!(zero_volume.kind(), ErrorKind::InvalidData);
        assert_eq!(
            zero_volume.to_string(),
            "Windows by-handle identity has a zero volume serial number"
        );

        let zero_file_index = identity_information(file_information(17, 0, 1))
            .err()
            .expect("a zero file index proves no identity");
        assert_eq!(zero_file_index.kind(), ErrorKind::InvalidData);
        assert_eq!(
            zero_file_index.to_string(),
            "Windows by-handle identity has a zero file index"
        );

        let unsupported_file_index = identity_information(file_information(17, u64::MAX, 1))
            .err()
            .expect("the unsupported file-index sentinel must fail closed");
        assert_eq!(unsupported_file_index.kind(), ErrorKind::Unsupported);
        assert_eq!(
            unsupported_file_index.to_string(),
            "Windows by-handle identity returned the unsupported maximum file-index sentinel"
        );
    }

    #[test]
    fn by_handle_identity_accepts_representative_valid_values() {
        let information = identity_information(file_information(17, 41, 2))
            .expect("a nonzero nonsentinel identity is durable");

        assert_eq!(information.volume_serial_number, 17);
        assert_eq!(information.file_index, 41);
        assert_eq!(information.number_of_links, 2);
    }

    #[test]
    fn change_token_detects_same_length_rewrite_with_restored_mtime() {
        let temp = tempdir().unwrap();
        let path = temp.path().join("parent.jsonl");
        fs::write(&path, b"old").unwrap();
        let original_mtime = fs::metadata(&path).unwrap().modified().unwrap();
        let before = change_token(&fs::File::open(&path).unwrap()).unwrap();

        let mut writer = OpenOptions::new().write(true).open(&path).unwrap();
        writer.seek(SeekFrom::Start(0)).unwrap();
        writer.write_all(b"new").unwrap();
        writer.sync_all().unwrap();
        writer
            .set_times(FileTimes::new().set_modified(original_mtime))
            .unwrap();
        drop(writer);

        let after = change_token(&fs::File::open(&path).unwrap()).unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().modified().unwrap(),
            original_mtime,
            "fixture must restore the exact Windows last-write time"
        );
        assert_eq!(after.last_write_time, before.last_write_time);
        assert_ne!(
            after.change_time, before.change_time,
            "native Windows change time must witness the in-place rewrite"
        );
    }
}
