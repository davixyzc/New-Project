//! The global allocator. A running program makes many small blocks, for objects, arrays, strings and
//! frames, so blocks of up to 512 bytes come from per-thread free lists, carved out of large chunks. Such a
//! block costs a few instructions to take and to give back. Larger blocks go to the system allocator.
//!
//! Small blocks are never returned to the system. A block freed by another thread joins that thread's list,
//! which is sound because every block comes from a chunk that outlives the program.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::UnsafeCell;
use std::ptr;

/// The largest block served from the free lists.
const MAX_SMALL: usize = 512;
/// Blocks are sized in multiples of this, and every one is aligned to it.
const GRAIN: usize = 16;
/// The number of size classes: one per multiple of `GRAIN` up to `MAX_SMALL`.
const CLASSES: usize = MAX_SMALL / GRAIN;
/// The memory requested from the system for the small blocks, a chunk at a time.
const CHUNK: usize = 1 << 20;

/// The free lists of one thread. `free[c]` heads the list of blocks of `c * GRAIN` bytes, linked through their
/// first word. Blocks of a class are carved from `next` up to `end`.
struct Pool {
    free: [*mut u8; CLASSES + 1],
    next: *mut u8,
    end: *mut u8,
}

thread_local! {
    static POOL: UnsafeCell<Pool> = const {
        UnsafeCell::new(Pool {
            free: [ptr::null_mut(); CLASSES + 1],
            next: ptr::null_mut(),
            end: ptr::null_mut(),
        })
    };
}

/// The allocator that the program uses. See the module documentation.
pub struct Pooled;

fn is_small(layout: Layout) -> bool {
    layout.size() <= MAX_SMALL && layout.align() <= GRAIN
}

fn class_of(size: usize) -> usize {
    size.max(1).div_ceil(GRAIN)
}

unsafe impl GlobalAlloc for Pooled {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if !is_small(layout) {
            return System.alloc(layout);
        }
        let class = class_of(layout.size());
        POOL.with(|pool| {
            let pool = &mut *pool.get();
            let head = pool.free[class];
            if !head.is_null() {
                pool.free[class] = *(head as *mut *mut u8);
                return head;
            }
            let bytes = class * GRAIN;
            if (pool.end as usize).wrapping_sub(pool.next as usize) < bytes {
                let chunk = System.alloc(Layout::from_size_align_unchecked(CHUNK, GRAIN));
                if chunk.is_null() {
                    return ptr::null_mut();
                }
                pool.next = chunk;
                pool.end = chunk.add(CHUNK);
            }
            let block = pool.next;
            pool.next = block.add(bytes);
            block
        })
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        if !is_small(layout) {
            return System.dealloc(ptr, layout);
        }
        let class = class_of(layout.size());
        POOL.with(|pool| {
            let pool = &mut *pool.get();
            *(ptr as *mut *mut u8) = pool.free[class];
            pool.free[class] = ptr;
        })
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let new_layout = Layout::from_size_align_unchecked(new_size, layout.align());
        if !is_small(layout) && !is_small(new_layout) {
            return System.realloc(ptr, layout, new_size);
        }
        let new = self.alloc(new_layout);
        if !new.is_null() {
            ptr::copy_nonoverlapping(ptr, new, layout.size().min(new_size));
            self.dealloc(ptr, layout);
        }
        new
    }
}
