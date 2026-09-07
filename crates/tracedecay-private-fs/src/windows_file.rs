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
    Ok(FileInformation {
        volume_serial_number: information.dwVolumeSerialNumber,
        file_index: (u64::from(information.nFileIndexHigh) << 32)
            | u64::from(information.nFileIndexLow),
        number_of_links: information.nNumberOfLinks,
    })
}
