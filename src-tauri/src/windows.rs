// Windows-only Win32 helpers.
//
// Two things this module exists to fix:
//
// 1. The release binary uses `windows_subsystem = "windows"` (GUI subsystem)
//    so a console window doesn't flash when the GUI launches from Explorer.
//    The cost is that cmd.exe / PowerShell don't get any stdio handles when
//    they invoke `myownllm.exe` for a CLI command — every println!/eprintln!
//    silently goes to the bit bucket. AttachConsole(ATTACH_PARENT_PROCESS) +
//    SetStdHandle re-points stdio at the parent shell, which has to happen
//    BEFORE the std streams are first touched (Rust caches the OS handle on
//    first access).
//
// 2. `wmic` is deprecated and not present by default on modern Windows 10/11,
//    so the previous wmic-based RAM and disk detection fell through to the
//    placeholder values (8 GB / 50 GB). GlobalMemoryStatusEx and
//    GetDiskFreeSpaceExA have been in the Win32 API since Win2000 and need
//    no extra dependencies.

use std::ffi::c_void;

type Dword = u32;
type Bool = i32;
type Handle = *mut c_void;

const ATTACH_PARENT_PROCESS: Dword = 0xFFFF_FFFF;
const STD_INPUT_HANDLE: Dword = 0xFFFF_FFF6; // (DWORD)-10
const STD_OUTPUT_HANDLE: Dword = 0xFFFF_FFF5; // (DWORD)-11
const STD_ERROR_HANDLE: Dword = 0xFFFF_FFF4; // (DWORD)-12
const GENERIC_READ: Dword = 0x8000_0000;
const GENERIC_WRITE: Dword = 0x4000_0000;
const FILE_SHARE_READ: Dword = 0x0000_0001;
const FILE_SHARE_WRITE: Dword = 0x0000_0002;
const OPEN_EXISTING: Dword = 3;
const INVALID_HANDLE_VALUE: Handle = !0usize as Handle;

#[repr(C)]
struct MemoryStatusEx {
    dw_length: Dword,
    dw_memory_load: Dword,
    ull_total_phys: u64,
    ull_avail_phys: u64,
    ull_total_page_file: u64,
    ull_avail_page_file: u64,
    ull_total_virtual: u64,
    ull_avail_virtual: u64,
    ull_avail_extended_virtual: u64,
}

extern "system" {
    fn AttachConsole(dw_process_id: Dword) -> Bool;
    fn SetStdHandle(n_std_handle: Dword, h_handle: Handle) -> Bool;
    fn CreateFileA(
        lp_file_name: *const u8,
        dw_desired_access: Dword,
        dw_share_mode: Dword,
        lp_security_attributes: *mut c_void,
        dw_creation_disposition: Dword,
        dw_flags_and_attributes: Dword,
        h_template_file: Handle,
    ) -> Handle;
    fn GlobalMemoryStatusEx(lp_buffer: *mut MemoryStatusEx) -> Bool;
    fn GetDiskFreeSpaceExA(
        lp_directory_name: *const u8,
        lp_free_bytes_available_to_caller: *mut u64,
        lp_total_number_of_bytes: *mut u64,
        lp_total_number_of_free_bytes: *mut u64,
    ) -> Bool;
}

/// Attach to the parent console (cmd.exe, PowerShell, Windows Terminal) and
/// rewire stdio so subsequent println!/eprintln!/stdin reach the launching
/// shell. Safe to call unconditionally — when there is no parent console
/// (e.g. launched from Explorer) AttachConsole returns 0 and stdio is left
/// alone.
///
/// Must be called BEFORE any code reads or writes stdout/stdin/stderr.
pub fn attach_parent_console() {
    unsafe {
        if AttachConsole(ATTACH_PARENT_PROCESS) == 0 {
            return;
        }
        let conout = open_console(b"CONOUT$\0");
        if !conout.is_null() && conout != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_OUTPUT_HANDLE, conout);
            SetStdHandle(STD_ERROR_HANDLE, conout);
        }
        let conin = open_console(b"CONIN$\0");
        if !conin.is_null() && conin != INVALID_HANDLE_VALUE {
            SetStdHandle(STD_INPUT_HANDLE, conin);
        }
    }
}

unsafe fn open_console(name: &[u8]) -> Handle {
    CreateFileA(
        name.as_ptr(),
        GENERIC_READ | GENERIC_WRITE,
        FILE_SHARE_READ | FILE_SHARE_WRITE,
        std::ptr::null_mut(),
        OPEN_EXISTING,
        0,
        std::ptr::null_mut(),
    )
}

pub fn total_physical_memory_bytes() -> Option<u64> {
    let mut status = MemoryStatusEx {
        dw_length: std::mem::size_of::<MemoryStatusEx>() as Dword,
        dw_memory_load: 0,
        ull_total_phys: 0,
        ull_avail_phys: 0,
        ull_total_page_file: 0,
        ull_avail_page_file: 0,
        ull_total_virtual: 0,
        ull_avail_virtual: 0,
        ull_avail_extended_virtual: 0,
    };
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return None;
    }
    Some(status.ull_total_phys)
}

pub fn disk_free_bytes(path: &str) -> Option<u64> {
    let mut buf = Vec::with_capacity(path.len() + 1);
    buf.extend_from_slice(path.as_bytes());
    buf.push(0);
    let mut free_to_caller: u64 = 0;
    let mut total: u64 = 0;
    let mut total_free: u64 = 0;
    let ok = unsafe {
        GetDiskFreeSpaceExA(
            buf.as_ptr(),
            &mut free_to_caller,
            &mut total,
            &mut total_free,
        )
    };
    if ok == 0 {
        None
    } else {
        Some(free_to_caller)
    }
}
