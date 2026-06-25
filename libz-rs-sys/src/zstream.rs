//! C-ABI data-type definitions for the `libz-rs-sys` FFI shim.
//!
//! This module defines the `#[repr(C)]` structs and the function-pointer
//! typedefs that mirror the canonical `zlib.h` / `zconf.h` layouts
//! **byte-for-byte on every target platform**. A C program compiled against
//! the upstream `zlib.h` and then linked against the Rust-produced
//! `libz.so` / `libz.a` must observe *identical* struct layouts, so every
//! field's name, type, and order here is load-bearing.
//!
//! Design rules enforced throughout this file (see AAP §0.6.2):
//!
//! * **C scalar types, not fixed-width Rust types.** Fields use `libc`
//!   scalars (`c_int`, `c_uint`, `c_ulong`, `c_char`, `c_void`, ...) so the
//!   layout tracks the platform C ABI. The single most important mapping is
//!   `uLong` → [`libc::c_ulong`] (8 bytes on LP64 Unix, 4 bytes on LLP64
//!   Windows); a `u64`/`u32` here would silently corrupt the ABI on one
//!   platform class.
//! * **Exact C names.** Every type is `pub` and named exactly as its C tag or
//!   typedef counterpart (`z_stream_s`, `gz_header_s`, `gzFile_s`, `uLong`,
//!   `Bytef`, ...). This keeps the sibling `cbindgen.toml` `[export.rename]`
//!   table empty and lets `cbindgen` regenerate a faithful `../include/zlib.h`
//!   (cbindgen only emits `pub` `#[repr(C)]` items).
//! * **`#[repr(C)]` on every struct**, with the C field order reproduced
//!   verbatim — no reordering, no added or removed fields.
//! * **No `unsafe` blocks and no executable logic.** This is a pure
//!   type-definition module. The function-pointer typedefs *name*
//!   `unsafe extern "C" fn` types, but a type alias is not an `unsafe` block.
//!   All real `unsafe` lives in `translate.rs` / `lib.rs`.
//! * **Self-contained.** Imports are limited to `core` and `libc`; this module
//!   never references the `zlib_rs` core crate.
//!
//! The crate root re-exports everything here (`pub use zstream::*;` or
//! `pub mod zstream;`) so the types are reachable both to `cbindgen` and to
//! C-API consumers (for example `libz_rs_sys::z_stream`).

// C type names are intentionally snake_case / mixed-case to match `zlib.h`
// exactly; suppress the Rust naming lint for the whole module so the ABI
// spellings survive `-D warnings`.
#![allow(non_camel_case_types)]

use core::ptr::{null, null_mut};

// ---------------------------------------------------------------------------
// Phase 1 — crate-scalar type aliases (mirror `zconf.h`)
//
// These aliases give the struct fields and the regenerated C header the exact
// `zlib.h` spellings. They resolve to `libc` / `core` types so the byte layout
// matches the C ABI on every supported platform.
// ---------------------------------------------------------------------------

/// C `Bytef` / `Byte`: an `unsigned char` (8 bits).
pub type Bytef = u8;

/// C `uInt`: `unsigned int` (`>= 16` bits; 32 bits on all supported targets).
pub type uInt = libc::c_uint;

/// C `uLong`: `unsigned long`.
///
/// **Critical ABI detail:** this is platform-dependent — 8 bytes on LP64 Unix,
/// 4 bytes on LLP64 Windows. It must map to [`libc::c_ulong`], never to a
/// fixed-width Rust integer such as `u64`/`u32`.
pub type uLong = libc::c_ulong;

/// C `voidpf`: a far/normal `void *` (mutable opaque pointer).
pub type voidpf = *mut libc::c_void;

/// C `voidpc`: a `const void *` (read-only opaque pointer).
pub type voidpc = *const libc::c_void;

/// C `z_crc_t` (`Z_U4`): a 32-bit unsigned integer used for CRC-32 values.
pub type z_crc_t = u32;

/// C `z_size_t`: the platform `size_t`.
pub type z_size_t = libc::size_t;

/// C `z_off_t`: the platform signed file-offset type (`off_t`).
pub type z_off_t = libc::off_t;

/// C `z_off64_t`: a 64-bit signed file offset (`off64_t` / `long long`).
pub type z_off64_t = i64;

// ---------------------------------------------------------------------------
// Phase 2 — opaque `internal_state` and the function-pointer typedefs
// ---------------------------------------------------------------------------

/// Opaque counterpart of the C `struct internal_state;` forward declaration.
///
/// The real deflate/inflate state is owned by the safe `zlib-rs` core and is
/// never visible to C callers; the C ABI only ever holds it behind a pointer
/// (`z_stream.state`). The zero-sized private field makes this type impossible
/// to construct or copy by value and keeps it FFI-opaque, exactly mirroring the
/// C contract.
#[repr(C)]
pub struct internal_state {
    _private: [u8; 0],
}

/// C `alloc_func`: `voidpf (*alloc_func)(voidpf opaque, uInt items, uInt size)`.
///
/// Wrapped in [`Option`] because a null `zalloc` is the documented "use the
/// default allocator" sentinel. The null-pointer optimization makes
/// `Option<extern "C" fn ...>` exactly pointer-sized, so this stays
/// ABI-compatible with a bare C function pointer.
pub type alloc_func = Option<
    unsafe extern "C" fn(
        opaque: *mut libc::c_void,
        items: libc::c_uint,
        size: libc::c_uint,
    ) -> *mut libc::c_void,
>;

/// C `free_func`: `void (*free_func)(voidpf opaque, voidpf address)`.
///
/// `Option`-wrapped for the same nullability/ABI reason as [`alloc_func`].
pub type free_func =
    Option<unsafe extern "C" fn(opaque: *mut libc::c_void, address: *mut libc::c_void)>;

/// C `in_func` (used by `inflateBack`):
/// `unsigned (*in_func)(void FAR *, z_const unsigned char FAR * FAR *)`.
pub type in_func = Option<unsafe extern "C" fn(*mut libc::c_void, *mut *const u8) -> libc::c_uint>;

/// C `out_func` (used by `inflateBack`):
/// `int (*out_func)(void FAR *, unsigned char FAR *, unsigned)`.
pub type out_func =
    Option<unsafe extern "C" fn(*mut libc::c_void, *mut u8, libc::c_uint) -> libc::c_int>;

// ---------------------------------------------------------------------------
// Phase 3 — `z_stream` (`struct z_stream_s`), the central streaming struct
//
// 14 fields, in the exact order and with the exact types of `zlib.h` L90-L110.
// On LP64 Unix this is 112 bytes, 8-byte aligned.
// ---------------------------------------------------------------------------

/// C `struct z_stream_s` — the public streaming state shared with applications.
///
/// Layout matches `zlib.h` byte-for-byte. `next_in`/`next_out` are the I/O
/// cursors, `avail_in`/`avail_out` the remaining byte counts, `total_in`/
/// `total_out` running totals, `msg` the last error string, `state` the opaque
/// internal state, `zalloc`/`zfree`/`opaque` the optional custom allocator
/// hooks, `data_type` the data-type guess / inflate decode state, `adler` the
/// running Adler-32 (zlib) or CRC-32 (gzip) checksum, and `reserved` unused.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct z_stream_s {
    /// `z_const Bytef *next_in` — next input byte.
    pub next_in: *const u8,
    /// `uInt avail_in` — number of bytes available at `next_in`.
    pub avail_in: libc::c_uint,
    /// `uLong total_in` — total number of input bytes read so far.
    pub total_in: libc::c_ulong,
    /// `Bytef *next_out` — next output byte will go here.
    pub next_out: *mut u8,
    /// `uInt avail_out` — remaining free space at `next_out`.
    pub avail_out: libc::c_uint,
    /// `uLong total_out` — total number of bytes output so far.
    pub total_out: libc::c_ulong,
    /// `z_const char *msg` — last error message, NULL if no error.
    pub msg: *const libc::c_char,
    /// `struct internal_state FAR *state` — not visible by applications.
    pub state: *mut internal_state,
    /// `alloc_func zalloc` — used to allocate the internal state.
    pub zalloc: alloc_func,
    /// `free_func zfree` — used to free the internal state.
    pub zfree: free_func,
    /// `voidpf opaque` — private data object passed to `zalloc`/`zfree`.
    pub opaque: *mut libc::c_void,
    /// `int data_type` — best guess about the data type (deflate), or the
    /// decoding state (inflate).
    pub data_type: libc::c_int,
    /// `uLong adler` — Adler-32 or CRC-32 value of the uncompressed data.
    pub adler: libc::c_ulong,
    /// `uLong reserved` — reserved for future use.
    pub reserved: libc::c_ulong,
}

/// C `typedef struct z_stream_s ... z_stream;`.
pub type z_stream = z_stream_s;

/// C `typedef z_stream FAR *z_streamp;`.
pub type z_streamp = *mut z_stream;

impl Default for z_stream_s {
    /// Returns an all-zero / all-null instance, mirroring the idiomatic C
    /// pattern `z_stream strm; memset(&strm, 0, sizeof strm);`.
    ///
    /// Constructed with explicit `null`/`null_mut`/`None`/`0` literals so the
    /// module stays free of `unsafe` (no `core::mem::zeroed`).
    fn default() -> Self {
        Self {
            next_in: null(),
            avail_in: 0,
            total_in: 0,
            next_out: null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: null(),
            state: null_mut(),
            zalloc: None,
            zfree: None,
            opaque: null_mut(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 4 — `gz_header` (`struct gz_header_s`)
//
// 13 fields, in the exact order and with the exact types of `zlib.h`
// L117-L132. This is the C-facing twin of the safe core's `GzHeader`
// (which uses `Option<Vec<u8>>`); here every field is a raw pointer or
// integer to match the C ABI. On LP64 Unix this is 80 bytes, 8-byte aligned.
// ---------------------------------------------------------------------------

/// C `struct gz_header_s` — gzip header information passed to and from zlib
/// routines (see RFC 1952). Layout matches `zlib.h` byte-for-byte.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct gz_header_s {
    /// `int text` — true if compressed data believed to be text.
    pub text: libc::c_int,
    /// `uLong time` — modification time.
    pub time: libc::c_ulong,
    /// `int xflags` — extra flags (not used when writing a gzip file).
    pub xflags: libc::c_int,
    /// `int os` — operating system.
    pub os: libc::c_int,
    /// `Bytef *extra` — pointer to extra field or NULL if none.
    pub extra: *mut u8,
    /// `uInt extra_len` — extra field length (valid if `extra != NULL`).
    pub extra_len: libc::c_uint,
    /// `uInt extra_max` — space at `extra` (only when reading header).
    pub extra_max: libc::c_uint,
    /// `Bytef *name` — pointer to zero-terminated file name or NULL.
    pub name: *mut u8,
    /// `uInt name_max` — space at `name` (only when reading header).
    pub name_max: libc::c_uint,
    /// `Bytef *comment` — pointer to zero-terminated comment or NULL.
    pub comment: *mut u8,
    /// `uInt comm_max` — space at `comment` (only when reading header).
    pub comm_max: libc::c_uint,
    /// `int hcrc` — true if there was or will be a header CRC.
    pub hcrc: libc::c_int,
    /// `int done` — true when done reading gzip header (not used when writing).
    pub done: libc::c_int,
}

/// C `typedef struct gz_header_s ... gz_header;`.
pub type gz_header = gz_header_s;

/// C `typedef gz_header FAR *gz_headerp;`.
pub type gz_headerp = *mut gz_header;

impl Default for gz_header_s {
    /// Returns an all-zero / all-null instance, analogous to
    /// [`z_stream_s::default`]. Built from explicit literals (no `unsafe`).
    fn default() -> Self {
        Self {
            text: 0,
            time: 0,
            xflags: 0,
            os: 0,
            extra: null_mut(),
            extra_len: 0,
            extra_max: 0,
            name: null_mut(),
            name_max: 0,
            comment: null_mut(),
            comm_max: 0,
            hcrc: 0,
            done: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Phase 5 — `gzFile_s` and `gzFile` (the `gzgetc` macro fast-path struct)
// ---------------------------------------------------------------------------

/// C `struct gzFile_s` — the abbreviated, **prefix-compatible header** of the
/// gzip file descriptor.
///
/// `zlib.h` deliberately exposes only these first three fields so the C
/// `gzgetc(g)` macro can inline its fast path
/// `((g)->have ? ((g)->have--, (g)->pos++, *((g)->next)++) : (gzgetc)(g))`.
///
/// **Contract for `translate.rs` / `lib.rs`:** the *real* gz state is much
/// larger and is owned by the safe core's `GzState`. The shim must allocate a
/// `Box`ed full-state value whose first three fields are layout-compatible with
/// `gzFile_s` (`have`, `next`, `pos`, in this order), and only ever hand a
/// `*mut gzFile_s` to C. That way the inlined C macro and the `gzgetc`
/// fallback function agree on where `have`/`next`/`pos` live. C callers must
/// never touch these fields except through the `gzgetc` macro.
///
/// These type definitions are intentionally **ungated**: `cbindgen` emits them
/// whenever `!Z_SOLO` so the generated header is complete. Only the gz
/// *functions* in `lib.rs` are `#[cfg(feature = "gz-io")]`-gated, not these
/// types.
#[repr(C)]
pub struct gzFile_s {
    /// `unsigned have` — bytes available in the inline read-ahead buffer.
    pub have: libc::c_uint,
    /// `unsigned char *next` — cursor into the inline read-ahead buffer.
    pub next: *mut u8,
    /// `z_off64_t pos` — current position in the uncompressed data.
    pub pos: z_off64_t,
}

/// C `typedef struct gzFile_s *gzFile;` — the semi-opaque gzip file descriptor.
pub type gzFile = *mut gzFile_s;

#[cfg(test)]
mod tests {
    use super::*;
    use core::mem::{align_of, size_of};

    // -- Portable invariants (hold on every target) -----------------------

    /// Nullable C function pointers must stay exactly pointer-sized so the
    /// `Option<unsafe extern "C" fn ...>` representation is ABI-compatible with
    /// a bare C function pointer (relies on the null-pointer optimization).
    #[test]
    fn function_pointer_typedefs_are_pointer_sized() {
        assert_eq!(size_of::<alloc_func>(), size_of::<*const libc::c_void>());
        assert_eq!(size_of::<alloc_func>(), size_of::<usize>());
        assert_eq!(size_of::<free_func>(), size_of::<usize>());
        assert_eq!(size_of::<in_func>(), size_of::<usize>());
        assert_eq!(size_of::<out_func>(), size_of::<usize>());
    }

    /// The opaque internal state must be zero-sized (it only ever exists behind
    /// a pointer).
    #[test]
    fn internal_state_is_zero_sized() {
        assert_eq!(size_of::<internal_state>(), 0);
    }

    /// The ABI structs must be pointer-aligned on every platform (their largest
    /// members are pointers / `c_ulong`).
    #[test]
    fn structs_are_pointer_aligned() {
        assert_eq!(align_of::<z_stream_s>(), align_of::<usize>());
        assert_eq!(align_of::<gz_header_s>(), align_of::<usize>());
        assert_eq!(align_of::<gzFile_s>(), align_of::<usize>());
    }

    /// Scalar aliases must resolve to the expected widths.
    #[test]
    fn scalar_aliases_have_expected_widths() {
        assert_eq!(size_of::<Bytef>(), 1);
        assert_eq!(size_of::<z_crc_t>(), 4);
        assert_eq!(size_of::<z_off64_t>(), 8);
        assert_eq!(size_of::<voidpf>(), size_of::<usize>());
        assert_eq!(size_of::<voidpc>(), size_of::<usize>());
    }

    /// `Default` must build a fully zero/null instance without `unsafe`.
    #[test]
    fn z_stream_default_is_zeroed() {
        let s = z_stream::default();
        assert!(s.next_in.is_null());
        assert!(s.next_out.is_null());
        assert!(s.msg.is_null());
        assert!(s.state.is_null());
        assert!(s.opaque.is_null());
        assert!(s.zalloc.is_none());
        assert!(s.zfree.is_none());
        assert_eq!(s.avail_in, 0);
        assert_eq!(s.total_in, 0);
        assert_eq!(s.avail_out, 0);
        assert_eq!(s.total_out, 0);
        assert_eq!(s.data_type, 0);
        assert_eq!(s.adler, 0);
        assert_eq!(s.reserved, 0);
    }

    /// `Default` must build a fully zero/null `gz_header` without `unsafe`.
    #[test]
    fn gz_header_default_is_zeroed() {
        let h = gz_header::default();
        assert_eq!(h.text, 0);
        assert_eq!(h.time, 0);
        assert_eq!(h.xflags, 0);
        assert_eq!(h.os, 0);
        assert!(h.extra.is_null());
        assert_eq!(h.extra_len, 0);
        assert_eq!(h.extra_max, 0);
        assert!(h.name.is_null());
        assert_eq!(h.name_max, 0);
        assert!(h.comment.is_null());
        assert_eq!(h.comm_max, 0);
        assert_eq!(h.hcrc, 0);
        assert_eq!(h.done, 0);
    }

    /// Field order must be strictly increasing (a cheap reorder tripwire that
    /// holds on every platform regardless of padding).
    #[test]
    fn z_stream_field_offsets_are_monotonic() {
        use core::mem::offset_of;
        let offs = [
            offset_of!(z_stream_s, next_in),
            offset_of!(z_stream_s, avail_in),
            offset_of!(z_stream_s, total_in),
            offset_of!(z_stream_s, next_out),
            offset_of!(z_stream_s, avail_out),
            offset_of!(z_stream_s, total_out),
            offset_of!(z_stream_s, msg),
            offset_of!(z_stream_s, state),
            offset_of!(z_stream_s, zalloc),
            offset_of!(z_stream_s, zfree),
            offset_of!(z_stream_s, opaque),
            offset_of!(z_stream_s, data_type),
            offset_of!(z_stream_s, adler),
            offset_of!(z_stream_s, reserved),
        ];
        assert_eq!(offs[0], 0);
        for w in offs.windows(2) {
            assert!(w[1] > w[0], "z_stream_s fields out of order: {offs:?}");
        }
    }

    // -- LP64 Unix exact layout (the primary deployment target) -----------
    //
    // These exact offsets/sizes are taken from a gcc probe against the
    // canonical `zlib.h` on a 64-bit Unix host (`uLong` == 8 bytes). They are
    // gated to LP64 non-Windows because LLP64 Windows (`uLong` == 4 bytes)
    // legitimately produces different offsets; a cross-platform hard-coded
    // assert would be wrong there.

    #[cfg(all(target_pointer_width = "64", not(windows)))]
    #[test]
    fn z_stream_lp64_layout_matches_c() {
        use core::mem::offset_of;
        assert_eq!(size_of::<z_stream_s>(), 112);
        assert_eq!(offset_of!(z_stream_s, next_in), 0);
        assert_eq!(offset_of!(z_stream_s, avail_in), 8);
        assert_eq!(offset_of!(z_stream_s, total_in), 16);
        assert_eq!(offset_of!(z_stream_s, next_out), 24);
        assert_eq!(offset_of!(z_stream_s, avail_out), 32);
        assert_eq!(offset_of!(z_stream_s, total_out), 40);
        assert_eq!(offset_of!(z_stream_s, msg), 48);
        assert_eq!(offset_of!(z_stream_s, state), 56);
        assert_eq!(offset_of!(z_stream_s, zalloc), 64);
        assert_eq!(offset_of!(z_stream_s, zfree), 72);
        assert_eq!(offset_of!(z_stream_s, opaque), 80);
        assert_eq!(offset_of!(z_stream_s, data_type), 88);
        assert_eq!(offset_of!(z_stream_s, adler), 96);
        assert_eq!(offset_of!(z_stream_s, reserved), 104);
    }

    #[cfg(all(target_pointer_width = "64", not(windows)))]
    #[test]
    fn gz_header_lp64_layout_matches_c() {
        use core::mem::offset_of;
        assert_eq!(size_of::<gz_header_s>(), 80);
        assert_eq!(offset_of!(gz_header_s, text), 0);
        assert_eq!(offset_of!(gz_header_s, time), 8);
        assert_eq!(offset_of!(gz_header_s, xflags), 16);
        assert_eq!(offset_of!(gz_header_s, os), 20);
        assert_eq!(offset_of!(gz_header_s, extra), 24);
        assert_eq!(offset_of!(gz_header_s, extra_len), 32);
        assert_eq!(offset_of!(gz_header_s, extra_max), 36);
        assert_eq!(offset_of!(gz_header_s, name), 40);
        assert_eq!(offset_of!(gz_header_s, name_max), 48);
        assert_eq!(offset_of!(gz_header_s, comment), 56);
        assert_eq!(offset_of!(gz_header_s, comm_max), 64);
        assert_eq!(offset_of!(gz_header_s, hcrc), 68);
        assert_eq!(offset_of!(gz_header_s, done), 72);
    }

    #[cfg(all(target_pointer_width = "64", not(windows)))]
    #[test]
    fn gz_file_lp64_layout_matches_c() {
        use core::mem::offset_of;
        assert_eq!(size_of::<gzFile_s>(), 24);
        assert_eq!(offset_of!(gzFile_s, have), 0);
        assert_eq!(offset_of!(gzFile_s, next), 8);
        assert_eq!(offset_of!(gzFile_s, pos), 16);
    }
}
