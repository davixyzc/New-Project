//! Memory for generated code. A page is writable while the code is copied in, and only then
//! made executable, so no page is ever both at once.

/// A block of executable code, unmapped when dropped.
pub struct ExecMem {
    ptr: *const u8,
    len: usize,
}

impl ExecMem {
    /// Copies `code` into fresh memory and makes it executable. `None` if the system refuses.
    pub fn new(code: &[u8]) -> Option<ExecMem> {
        let (ptr, len) = sys::map(code)?;
        Some(ExecMem { ptr, len })
    }

    pub fn ptr(&self) -> *const u8 {
        self.ptr
    }
}

impl Drop for ExecMem {
    fn drop(&mut self) {
        sys::unmap(self.ptr, self.len);
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod sys {
    use std::ffi::c_void;
    use std::os::raw::c_int;
    use std::ptr;

    const PROT_READ: c_int = 0x1;
    const PROT_WRITE: c_int = 0x2;
    const PROT_EXEC: c_int = 0x4;
    const MAP_PRIVATE: c_int = 0x02;
    #[cfg(target_os = "linux")]
    const MAP_ANONYMOUS: c_int = 0x20;
    #[cfg(target_os = "macos")]
    const MAP_ANONYMOUS: c_int = 0x1000;
    const PAGE: usize = 4096;

    extern "C" {
        fn mmap(
            addr: *mut c_void,
            len: usize,
            prot: c_int,
            flags: c_int,
            fd: c_int,
            offset: i64,
        ) -> *mut c_void;
        fn mprotect(addr: *mut c_void, len: usize, prot: c_int) -> c_int;
        fn munmap(addr: *mut c_void, len: usize) -> c_int;
    }

    /// Maps whole pages, copies `code` in, then drops write permission and grants execute.
    pub fn map(code: &[u8]) -> Option<(*const u8, usize)> {
        let len = code.len().div_ceil(PAGE).max(1) * PAGE;
        // SAFETY: an anonymous private mapping takes no pointers. Its result is checked.
        unsafe {
            let p = mmap(
                ptr::null_mut(),
                len,
                PROT_READ | PROT_WRITE,
                MAP_PRIVATE | MAP_ANONYMOUS,
                -1,
                0,
            );
            if p as isize == -1 {
                return None;
            }
            ptr::copy_nonoverlapping(code.as_ptr(), p as *mut u8, code.len());
            if mprotect(p, len, PROT_READ | PROT_EXEC) != 0 {
                munmap(p, len);
                return None;
            }
            Some((p as *const u8, len))
        }
    }

    pub fn unmap(p: *const u8, len: usize) {
        // SAFETY: `p` and `len` describe a mapping that `map` made and nothing has unmapped.
        unsafe {
            munmap(p as *mut c_void, len);
        }
    }
}

/// Other systems have no native code: `map` always refuses, so nothing is ever mapped.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod sys {
    pub fn map(_code: &[u8]) -> Option<(*const u8, usize)> {
        None
    }

    pub fn unmap(_p: *const u8, _len: usize) {}
}
