//! Windows file-handle identity read via `GetFileInformationByHandle`.

use std::io;
use std::mem::MaybeUninit;
use std::os::windows::io::AsRawHandle;

use windows_sys::Win32::Storage::FileSystem::{
    BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
};

#[derive(Clone, Copy)]
pub struct FileInformation {
    pub volume_serial_number: u32,
    pub file_index: u64,
    pub number_of_links: u32,
}

/// Reads by-handle identity from any open Windows handle: `std::fs::File`,
/// and the `cap_std` `Dir`/`File` capabilities whose `Metadata` only exposes
/// the volume serial number and file index on nightly.
pub fn information<H: AsRawHandle>(file: &H) -> io::Result<FileInformation> {
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
    validate_file_information(
        information.dwVolumeSerialNumber,
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
        information.nNumberOfLinks,
    )
}

fn validate_file_information(
    volume_serial_number: u32,
    file_index: u64,
    number_of_links: u32,
) -> io::Result<FileInformation> {
    if volume_serial_number == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows by-handle identity has a zero volume serial number",
        ));
    }
    if file_index == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "Windows by-handle identity has a zero file index",
        ));
    }
    if file_index == u64::MAX {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Windows by-handle identity returned the unsupported maximum file-index sentinel",
        ));
    }
    Ok(FileInformation {
        volume_serial_number,
        file_index,
        number_of_links,
    })
}

#[cfg(test)]
mod tests {
    use std::io::ErrorKind;

    use super::validate_file_information;

    #[test]
    fn by_handle_identity_rejects_unprovable_and_sentinel_values() {
        let zero_volume = validate_file_information(0, 41, 1)
            .err()
            .expect("a zero volume serial number proves no identity");
        assert_eq!(zero_volume.kind(), ErrorKind::InvalidData);
        assert_eq!(
            zero_volume.to_string(),
            "Windows by-handle identity has a zero volume serial number"
        );

        let zero_file_index = validate_file_information(17, 0, 1)
            .err()
            .expect("a zero file index proves no identity");
        assert_eq!(zero_file_index.kind(), ErrorKind::InvalidData);
        assert_eq!(
            zero_file_index.to_string(),
            "Windows by-handle identity has a zero file index"
        );

        let unsupported_file_index = validate_file_information(17, u64::MAX, 1)
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
        let information = validate_file_information(17, 41, 2)
            .expect("a nonzero nonsentinel identity is durable");

        assert_eq!(information.volume_serial_number, 17);
        assert_eq!(information.file_index, 41);
        assert_eq!(information.number_of_links, 2);
    }
}
