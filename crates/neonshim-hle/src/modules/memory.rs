//! The title's own `VirtualAlloc` and `VirtualFree` (it imports them from
//! `api-ms-win-core-memory-l1-1-0.dll`; `VirtualProtect` it does not
//! import). They pass through to the host; what is added is for title
//! memory in place (`neonshim_gpu::inplace`):
//!
//! - before a free, what is in place there is let go;
//! - while a heap opened over the pages still stands (it goes once the
//!   lists that hold it land), the host refuses the free with error 87:
//!   the free is tried again for a while instead of failing;
//! - failures of either call are logged: a free that fails leaves the
//!   title's address space as it was, and a later allocation there that
//!   fails hands it a null pointer.
#![allow(non_snake_case)]
// Title code calls these through its import table.
#![allow(clippy::not_unsafe_ptr_arg_deref)]

use std::ffi::c_void;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use windows_sys::Win32::Foundation::{ERROR_INVALID_PARAMETER, GetLastError, SetLastError};
use windows_sys::Win32::System::Memory::{
    self as host, MEM_DECOMMIT, MEM_RELEASE, MEMORY_BASIC_INFORMATION, VirtualQuery,
};
use windows_sys::core::BOOL;

use crate::{HleModule, hle_export};

/// How long a free over memory in place waits for the heap there to go.
const PATIENCE: Duration = Duration::from_secs(2);
/// After this many frees that waited in vain, no more waiting: the title
/// gets the host's answer at once.
const GIVE_UPS: u64 = 4;
/// The host's page.
const PAGE: usize = 4096;

/// Frees that failed at first, of them those that a wait let through and
/// those that waited in vain; allocations that failed.
static COUNTS: [AtomicU64; 4] = [const { AtomicU64::new(0) }; 4];

/// See the module.
pub extern "system" fn VirtualFree(address: *mut c_void, size: usize, kind: u32) -> BOOL {
    free(address, size, kind)
}

/// The title's free, also for `xmem`'s own frees of title memory.
pub(crate) fn free(address: *mut c_void, size: usize, kind: u32) -> BOOL {
    let placed = neonshim_gpu::title_memory_in_place();
    if placed && let Some((at, len)) = span(address, size, kind) {
        neonshim_gpu::title_memory_freeing(at, len);
    }
    // SAFETY: the title's own arguments.
    if unsafe { host::VirtualFree(address, size, kind) } != 0 {
        return 1;
    }
    // SAFETY: reads the calling thread's last error.
    let error = unsafe { GetLastError() };
    let n = COUNTS[0].fetch_add(1, Ordering::Relaxed);
    let patient = placed && error == ERROR_INVALID_PARAMETER && COUNTS[2].load(Ordering::Relaxed) < GIVE_UPS;
    if patient {
        let start = Instant::now();
        while start.elapsed() < PATIENCE {
            std::thread::sleep(Duration::from_millis(1));
            // SAFETY: as above.
            if unsafe { host::VirtualFree(address, size, kind) } != 0 {
                if COUNTS[1].fetch_add(1, Ordering::Relaxed) < 16 {
                    tracing::info!(
                        at = format_args!("{address:p}"),
                        size,
                        kind = format_args!("{kind:#x}"),
                        waited_ms = start.elapsed().as_millis() as u64,
                        "the title's VirtualFree went through once the heap in place there went"
                    );
                }
                return 1;
            }
        }
        COUNTS[2].fetch_add(1, Ordering::Relaxed);
    }
    if n < 16 || patient {
        tracing::warn!(
            at = format_args!("{address:p}"),
            size,
            kind = format_args!("{kind:#x}"),
            error,
            in_place = placed,
            waited = patient,
            failed_so_far = n + 1,
            "the title's VirtualFree failed"
        );
    }
    // SAFETY: gives the title the error the host gave.
    unsafe { SetLastError(error) };
    0
}

/// See the module.
pub extern "system" fn VirtualAlloc(address: *const c_void, size: usize, kind: u32, protect: u32) -> *mut c_void {
    // SAFETY: the title's own arguments.
    let at = unsafe { host::VirtualAlloc(address, size, kind, protect) };
    if at.is_null() {
        // SAFETY: reads the calling thread's last error.
        let error = unsafe { GetLastError() };
        if COUNTS[3].fetch_add(1, Ordering::Relaxed) < 16 {
            tracing::warn!(
                at = format_args!("{address:p}"),
                size,
                kind = format_args!("{kind:#x}"),
                protect = format_args!("{protect:#x}"),
                error,
                in_place = neonshim_gpu::title_memory_in_place(),
                "the title's VirtualAlloc failed"
            );
        }
        // SAFETY: gives the title the error the host gave.
        unsafe { SetLastError(error) };
    }
    at
}

/// The bytes a free touches: for a release (and a decommit of size 0 at
/// an allocation's base) the whole allocation, else the pages of
/// `[address, address + size)`.
fn span(address: *mut c_void, size: usize, kind: u32) -> Option<(usize, usize)> {
    let at = address as usize;
    if at == 0 {
        return None;
    }
    if kind & MEM_DECOMMIT != 0 && size != 0 {
        let start = at & !(PAGE - 1);
        return Some((start, (at + size).next_multiple_of(PAGE) - start));
    }
    if kind & (MEM_RELEASE | MEM_DECOMMIT) == 0 {
        return None;
    }
    let mut len = 0usize;
    loop {
        let mut info = MEMORY_BASIC_INFORMATION::default();
        // SAFETY: queries our own address space into a local structure.
        let got = unsafe { VirtualQuery((at + len) as *const c_void, &mut info, size_of::<MEMORY_BASIC_INFORMATION>()) };
        if got == 0 || info.AllocationBase as usize != at || info.RegionSize == 0 {
            break;
        }
        len += info.RegionSize;
    }
    (len != 0).then_some((at, len))
}

pub(crate) fn module() -> HleModule {
    HleModule {
        name: "memory",
        dlls: &["api-ms-win-core-memory-*"],
        exports: vec![hle_export!(VirtualAlloc), hle_export!(VirtualFree)],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, MEM_RESERVE, PAGE_READWRITE};

    #[test]
    fn a_release_spans_the_whole_allocation() {
        // SAFETY: a fresh allocation of our own, freed below.
        let base = unsafe { host::VirtualAlloc(std::ptr::null(), 16 * PAGE, MEM_RESERVE, PAGE_READWRITE) };
        assert!(!base.is_null());
        // SAFETY: commits part of it: two regions of different states.
        assert!(!unsafe { host::VirtualAlloc(base, 4 * PAGE, MEM_COMMIT, PAGE_READWRITE) }.is_null());
        assert_eq!(span(base, 0, MEM_RELEASE), Some((base as usize, 16 * PAGE)));
        assert_eq!(span(base.cast::<u8>().wrapping_add(PAGE + 8).cast(), 10, MEM_DECOMMIT), Some((base as usize + PAGE, PAGE)));
        assert_eq!(free(base, 0, MEM_RELEASE), 1);
        assert_eq!(span(std::ptr::null_mut(), 0, MEM_RELEASE), None);
    }

    #[test]
    fn a_failed_free_keeps_the_hosts_error() {
        // A release with a size is refused with error 87.
        let base = unsafe { host::VirtualAlloc(std::ptr::null(), PAGE, MEM_RESERVE | MEM_COMMIT, PAGE_READWRITE) };
        assert!(!base.is_null());
        assert_eq!(free(base, PAGE, MEM_RELEASE), 0);
        // SAFETY: reads this thread's last error.
        assert_eq!(unsafe { GetLastError() }, ERROR_INVALID_PARAMETER);
        assert_eq!(free(base, 0, MEM_RELEASE), 1);
    }

    #[test]
    fn the_title_import_is_ours() {
        assert_eq!(
            crate::registry::lookup("api-ms-win-core-memory-l1-1-0.dll", "VirtualFree"),
            Some(VirtualFree as *const () as usize)
        );
        assert!(crate::registry::lookup("api-ms-win-core-memory-l1-1-0.dll", "VirtualQueryEx").is_none());
    }
}
