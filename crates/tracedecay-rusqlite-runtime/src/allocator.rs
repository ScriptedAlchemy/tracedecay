use std::ffi::{c_int, c_void};

use rusqlite::ffi::{SQLITE_CONFIG_MALLOC, SQLITE_OK, sqlite3_config, sqlite3_mem_methods};

#[derive(Clone, Copy)]
pub struct SqliteAllocatorMethodsV1 {
    pub malloc: unsafe extern "C" fn(c_int) -> *mut c_void,
    pub free: unsafe extern "C" fn(*mut c_void),
    pub realloc: unsafe extern "C" fn(*mut c_void, c_int) -> *mut c_void,
    pub size: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub roundup: unsafe extern "C" fn(c_int) -> c_int,
    pub init: unsafe extern "C" fn(*mut c_void) -> c_int,
    pub shutdown: unsafe extern "C" fn(*mut c_void),
}

/// # Safety
///
/// Every callback must implement SQLite's allocator contract for process
/// lifetime, and this must run before SQLite initializes.
pub unsafe fn install_sqlite_allocator(methods: SqliteAllocatorMethodsV1) -> Result<(), c_int> {
    let methods = sqlite3_mem_methods {
        xMalloc: Some(methods.malloc),
        xFree: Some(methods.free),
        xRealloc: Some(methods.realloc),
        xSize: Some(methods.size),
        xRoundup: Some(methods.roundup),
        xInit: Some(methods.init),
        xShutdown: Some(methods.shutdown),
        pAppData: std::ptr::null_mut(),
    };
    // SAFETY: SQLite copies the method table before returning.
    let status = unsafe { sqlite3_config(SQLITE_CONFIG_MALLOC, &raw const methods) };
    if status == SQLITE_OK {
        Ok(())
    } else {
        Err(status)
    }
}
