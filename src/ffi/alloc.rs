//! Caller-allocator (`zalloc`/`zfree`) buffer bridge — the sanctioned home of
//! the raw-pointer allocation hook.
//!
//! # Why this module exists (M6)
//!
//! zlib lets a C caller override allocation through the `z_stream`
//! `zalloc`/`zfree`/`opaque` triple (`zlib.h` L85-L86; AAP §0.6.3). Honoring that
//! contract requires calling a raw C function pointer, materializing the returned
//! raw pointer as a slice, and releasing it through the matching `zfree` on drop
//! — all `unsafe` operations.
//!
//! Per the migration's unsafe-isolation strategy (AAP §0.6.2 / §0.7.2), **all**
//! `unsafe` in the crate is confined to the `ffi` tree. This module is that
//! confinement point for the allocator hook: it defines [`CForeignBuffer`], the
//! sole implementor of the safe [`ForeignBuffer`] interface consumed by
//! [`crate::stream::AllocBuffer`]. Because `stream.rs` and the compression /
//! decompression engines see only the safe [`ForeignBuffer`] methods, they
//! contain **zero** `unsafe` — the raw-pointer work lives exclusively here.
//!
//! # Fallible allocation (M7)
//!
//! [`try_alloc_foreign`] returns [`None`] when the caller's `zalloc` reports
//! out-of-memory (or the request size is unrepresentable in the C `uInt` hook
//! ABI). It never silently falls back to the global allocator; that choice is
//! what lets the deflate/inflate initialization paths surface `Z_MEM_ERROR`
//! exactly as the C library does, rather than masking an OOM from a caller who
//! deliberately installed a bounded allocator.

use alloc::boxed::Box;
use core::ffi::{c_uint, c_void};
use core::ptr::{self, NonNull};

use crate::stream::{AllocHook, ForeignBuffer};

/// A working buffer backed by a caller-supplied C `zalloc`/`zfree` pair.
///
/// This is the concrete, `ffi`-local implementor of [`ForeignBuffer`]. It owns a
/// non-null region returned by the hook's `zalloc` and releases it through the
/// same hook's `zfree` on [`Drop`]. All of the raw-pointer `unsafe` — the slice
/// materialization in [`as_slice`](ForeignBuffer::as_slice) /
/// [`as_mut_slice`](ForeignBuffer::as_mut_slice) and the `zfree` in [`Drop`] —
/// is contained in this type so the safe core never touches it (M6).
struct CForeignBuffer<T: Copy + Default> {
    /// Non-null pointer to `len` initialized `T`s obtained from the hook's
    /// `zalloc` and zero-filled by [`try_alloc_foreign`].
    ptr: NonNull<T>,
    /// Element count (not bytes).
    len: usize,
    /// The hook whose `zfree` releases [`ptr`](Self::ptr) (via its `opaque`).
    hook: AllocHook,
}

impl<T: Copy + Default + 'static> ForeignBuffer<T> for CForeignBuffer<T> {
    #[inline]
    fn as_slice(&self) -> &[T] {
        // SAFETY: `ptr` addresses `len` contiguous, initialized `T`s obtained
        // from the hook's `zalloc` and zero-filled in `try_alloc_foreign`; the
        // region stays valid and exclusively owned until this buffer's `Drop`,
        // so a shared slice for the `&self` borrow is sound.
        unsafe { core::slice::from_raw_parts(self.ptr.as_ptr(), self.len) }
    }

    #[inline]
    fn as_mut_slice(&mut self) -> &mut [T] {
        // SAFETY: as in `as_slice`, but `&mut self` guarantees exclusive access,
        // so a unique slice over the owned region is sound.
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.len) }
    }

    fn clone_foreign(&self) -> Option<Box<dyn ForeignBuffer<T>>> {
        // Allocate a fresh foreign region through the SAME hook (matching C
        // `deflateCopy`, which `ZALLOC`s new buffers) and copy the contents in.
        // On OOM this returns `None`; the caller (`AllocBuffer::clone`) then
        // performs a sound global-allocator copy so `Clone` stays infallible.
        let mut fresh = try_alloc_foreign::<T>(self.hook, self.len)?;
        fresh.as_mut_slice().copy_from_slice(self.as_slice());
        Some(fresh)
    }
}

impl<T: Copy + Default> Drop for CForeignBuffer<T> {
    fn drop(&mut self) {
        if let Some(zfree) = self.hook.zfree() {
            // SAFETY: `ptr` was returned by this same hook's `zalloc` and has not
            // been freed before — this buffer is its unique owner. `zfree` is the
            // caller's matching deallocator and `opaque` its cookie, exactly as
            // zlib's `ZFREE(strm, addr)` expands to `(*zfree)(opaque, addr)`.
            unsafe { zfree(self.hook.opaque(), self.ptr.as_ptr() as *mut c_void) };
        }
    }
}

/// Allocates a zero-initialized buffer of `count` elements of `T` through the
/// caller's active `zalloc`, returned as a boxed [`ForeignBuffer`] — or [`None`]
/// when the request cannot be honored (an unrepresentable size, or the caller's
/// `zalloc` reporting out-of-memory).
///
/// This is the sanctioned home of the raw allocator-hook invocation and the
/// post-allocation zero fill (reproducing C `zcalloc`'s `zmemzero`).
/// [`crate::stream::AllocBuffer::try_zeroed`] delegates here via
/// [`AllocHook::try_alloc_zeroed`] so the safe core contains no `unsafe` (M6).
/// Returning [`None`] on `zalloc` OOM — rather than silently using the global
/// allocator — is what lets the init paths surface `Z_MEM_ERROR` (M7).
///
/// # Preconditions
///
/// Only meaningful for an active hook and a non-zero `count`; both are guaranteed
/// by the sole caller ([`AllocBuffer::try_zeroed`](crate::stream::AllocBuffer::try_zeroed)),
/// which handles the null-hook / empty-request fast paths itself. A
/// `debug_assert!` documents and checks the contract without cost in release
/// builds.
pub(crate) fn try_alloc_foreign<T: Copy + Default + 'static>(
    hook: AllocHook,
    count: usize,
) -> Option<Box<dyn ForeignBuffer<T>>> {
    debug_assert!(
        hook.is_active() && count > 0,
        "try_alloc_foreign is only called for an active hook and a non-zero count"
    );

    // Both halves of an active hook are present, but re-check `zalloc` to obtain
    // the function pointer without unwrapping.
    let zalloc = hook.zalloc()?;

    // The C hook takes `items: uInt` and `size: uInt` (both `c_uint`). Guard the
    // width and the multiplication so we never pass a truncated size to the hook;
    // an unrepresentable request is treated as an allocation failure (`None`).
    let elem = core::mem::size_of::<T>();
    let items = c_uint::try_from(count).ok()?;
    let size = c_uint::try_from(elem).ok()?;
    u64::from(items).checked_mul(u64::from(size))?;

    // SAFETY: `zalloc` is a caller-supplied `alloc_func` taken from a valid
    // `z_stream`; per the zlib contract it allocates `items * size` bytes
    // suitably aligned for the element type (or returns null). `opaque` is the
    // caller's cookie, forwarded verbatim; only the returned pointer is inspected
    // here — the hook is not dereferenced in any other way.
    let raw = unsafe { zalloc(hook.opaque(), items, size) } as *mut T;
    let ptr = NonNull::new(raw)?;

    // SAFETY: `zalloc` returned a non-null region of at least
    // `count * size_of::<T>()` bytes (its documented contract), so zero-writing
    // `count` elements is in-bounds. `T: Copy` has no drop glue (overwriting the
    // uninitialized region is sound), and an all-zero bit pattern is a valid
    // value for the integer element types the engines use (`u8`/`u16`/`u32`).
    unsafe { ptr::write_bytes(ptr.as_ptr(), 0u8, count) };

    Some(Box::new(CForeignBuffer {
        ptr,
        len: count,
        hook,
    }))
}
