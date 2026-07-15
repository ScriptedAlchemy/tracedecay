use std::fs::File;
use std::io;
use std::mem::MaybeUninit;
use std::os::windows::io::AsRawHandle;

#[derive(Clone, Copy)]
pub(crate) struct FileInformation {
    pub(crate) volume_serial_number: u32,
    pub(crate) file_index: u64,
    pub(crate) number_of_links: u32,
}

pub(crate) fn information(file: &File) -> io::Result<FileInformation> {
    let mut information = MaybeUninit::<ByHandleFileInformation>::uninit();
    // SAFETY: `file` owns a valid Windows file handle, and `information` points
    // to writable memory sized for the API's complete output structure.
    let succeeded =
        unsafe { get_file_information_by_handle(file.as_raw_handle(), information.as_mut_ptr()) };
    if succeeded == 0 {
        return Err(io::Error::last_os_error());
    }

    // SAFETY: A nonzero API result initializes every field of the output structure.
    let information = unsafe { information.assume_init() };
    Ok(FileInformation {
        volume_serial_number: information.volume_serial_number,
        file_index: (u64::from(information.file_index_high) << 32)
            | u64::from(information.file_index_low),
        number_of_links: information.number_of_links,
    })
}

#[repr(C)]
struct ByHandleFileInformation {
    _file_attributes: u32,
    _creation_time_low_date_time: u32,
    _creation_time_high_date_time: u32,
    _last_access_time_low_date_time: u32,
    _last_access_time_high_date_time: u32,
    _last_write_time_low_date_time: u32,
    _last_write_time_high_date_time: u32,
    volume_serial_number: u32,
    _file_size_high: u32,
    _file_size_low: u32,
    number_of_links: u32,
    file_index_high: u32,
    file_index_low: u32,
}

#[link(name = "kernel32")]
unsafe extern "system" {
    #[link_name = "GetFileInformationByHandle"]
    fn get_file_information_by_handle(
        file: *mut std::ffi::c_void,
        information: *mut ByHandleFileInformation,
    ) -> i32;
}
