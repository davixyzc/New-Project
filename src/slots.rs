//! The values of an object, and the variables that a closure captures. The first few live in the record that holds them,
//! so a small object or closure is one allocation, and a longer one has a heap buffer, as a `Vec` does.

use std::alloc::{alloc, dealloc, handle_alloc_error, Layout};
use std::mem::{offset_of, MaybeUninit};
use std::ops::{Deref, DerefMut};
use std::ptr;

use crate::value::Value;

/// The number of values that a record keeps inline.
pub const INLINE: usize = 2;

/// The values, in order. While they fit inline, they live in `inline`, and `buf` is the address of `inline` once the record
/// is placed (see `fixup`), which is where native code reads them. A record that moves after it is placed must be fixed up
/// again, so a record is placed where it stays, and only then read by native code.
#[repr(C)]
pub struct Slots {
    /// The first value of a heap buffer. While the values are inline, it is the address of `inline`, as set by `fixup`.
    buf: *mut Value,
    len: usize,
    /// The number of values that fit: `INLINE` while the values are inline, and more than that on the heap.
    cap: usize,
    inline: [MaybeUninit<Value>; INLINE],
}

// Native code steps over the values by their size, so a value must be exactly that big.
const _: () = assert!(std::mem::size_of::<Value>() == 16);

/// Where the parts of a `Slots` sit, as offsets from the start of it.
pub struct SlotsLayout {
    pub buf: usize,
    pub len: usize,
    pub cap: usize,
}

/// The layout of a `Slots`, for native code that reads its values in place.
pub fn layout() -> SlotsLayout {
    SlotsLayout {
        buf: offset_of!(Slots, buf),
        len: offset_of!(Slots, len),
        cap: offset_of!(Slots, cap),
    }
}

impl Slots {
    /// Empty storage with room for `capacity` values, inline when they fit there.
    #[inline]
    pub fn with_capacity(capacity: usize) -> Slots {
        let mut slots = Slots {
            buf: ptr::null_mut(),
            len: 0,
            cap: INLINE,
            inline: [const { MaybeUninit::uninit() }; INLINE],
        };
        if capacity > INLINE {
            slots.grow(capacity);
        }
        slots
    }

    /// Points `buf` at the inline values, once the record is where it stays.
    pub fn fixup(&mut self) {
        if self.cap == INLINE {
            self.buf = self.inline.as_mut_ptr() as *mut Value;
        }
    }

    #[inline]
    fn ptr(&self) -> *const Value {
        if self.cap > INLINE {
            self.buf
        } else {
            self.inline.as_ptr() as *const Value
        }
    }

    #[inline]
    fn ptr_mut(&mut self) -> *mut Value {
        if self.cap > INLINE {
            self.buf
        } else {
            self.inline.as_mut_ptr() as *mut Value
        }
    }

    /// Makes room for at least `min` values. The values move to a heap buffer when they no longer fit inline.
    #[cold]
    fn grow(&mut self, min: usize) {
        if min <= self.cap {
            return;
        }
        let cap = min.max(2 * self.cap);
        let layout = Layout::array::<Value>(cap).expect("the values of an object fit in memory");
        // SAFETY: `layout` has a size. The new buffer is checked before anything is written to it, and the `len` values
        // move into it bitwise, so the inline storage, if it held them, is not dropped. A heap buffer is freed with the
        // layout that made it.
        unsafe {
            let buf = alloc(layout) as *mut Value;
            if buf.is_null() {
                handle_alloc_error(layout);
            }
            ptr::copy_nonoverlapping(self.ptr(), buf, self.len);
            if self.cap > INLINE {
                dealloc(
                    self.buf as *mut u8,
                    Layout::array::<Value>(self.cap).unwrap(),
                );
            }
            self.buf = buf;
        }
        self.cap = cap;
    }

    #[inline]
    pub fn push(&mut self, v: Value) {
        if self.len == self.cap {
            self.grow(self.len + 1);
        }
        // SAFETY: there is room for one more value after the last one, since `grow` made it if there was none.
        unsafe { self.ptr_mut().add(self.len).write(v) };
        self.len += 1;
    }

    /// Removes the value at `i`, which moves the values after it down one place, as `Vec::remove` does.
    pub fn remove(&mut self, i: usize) -> Value {
        assert!(i < self.len, "the index is out of bounds");
        // SAFETY: `i` is one of the initialised values. It is read out first, and the values after it move down over
        // it, so the length, which shrinks by one, still counts only initialised values.
        unsafe {
            let p = self.ptr_mut();
            let v = p.add(i).read();
            ptr::copy(p.add(i + 1), p.add(i), self.len - i - 1);
            self.len -= 1;
            v
        }
    }
}

impl Deref for Slots {
    type Target = [Value];

    fn deref(&self) -> &[Value] {
        // SAFETY: the first `len` values are initialised, and `ptr` points to them.
        unsafe { std::slice::from_raw_parts(self.ptr(), self.len) }
    }
}

impl DerefMut for Slots {
    fn deref_mut(&mut self) -> &mut [Value] {
        let (p, len) = (self.ptr_mut(), self.len);
        // SAFETY: the first `len` values are initialised, and `p` points to them, with no other reference to them.
        unsafe { std::slice::from_raw_parts_mut(p, len) }
    }
}

impl Clone for Slots {
    fn clone(&self) -> Slots {
        let mut copy = Slots::with_capacity(self.len);
        for v in self.iter() {
            copy.push(v.clone());
        }
        copy
    }
}

impl Drop for Slots {
    fn drop(&mut self) {
        // SAFETY: the first `len` values are initialised, and each is dropped once. A heap buffer is freed with the layout
        // that `grow` made it with.
        unsafe {
            ptr::drop_in_place(ptr::slice_from_raw_parts_mut(self.ptr_mut(), self.len));
            if self.cap > INLINE {
                dealloc(
                    self.buf as *mut u8,
                    Layout::array::<Value>(self.cap).unwrap(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn values_stay_in_order_across_the_move_to_the_heap() {
        let mut slots = Slots::with_capacity(0);
        for i in 0..(3 * INLINE) {
            slots.push(Value::Num(i as f64));
        }
        assert_eq!(slots.len(), 3 * INLINE);
        assert!(slots
            .iter()
            .enumerate()
            .all(|(i, v)| matches!(v, Value::Num(n) if *n == i as f64)));
    }

    #[test]
    fn remove_moves_the_values_after_it_down() {
        let mut slots = Slots::with_capacity(0);
        for i in 0..3 {
            slots.push(Value::Num(i as f64));
        }
        let removed = slots.remove(0);
        assert!(matches!(removed, Value::Num(n) if n == 0.0));
        assert!(matches!(slots[0], Value::Num(n) if n == 1.0));
        assert!(matches!(slots[1], Value::Num(n) if n == 2.0));
        assert_eq!(slots.len(), 2);
    }

    #[test]
    fn a_clone_owns_its_values() {
        let mut slots = Slots::with_capacity(0);
        slots.push(Value::Num(1.0));
        let mut copy = slots.clone();
        copy[0] = Value::Num(2.0);
        assert!(matches!(slots[0], Value::Num(n) if n == 1.0));
        assert!(matches!(copy[0], Value::Num(n) if n == 2.0));
    }
}
