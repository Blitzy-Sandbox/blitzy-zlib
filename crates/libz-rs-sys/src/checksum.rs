//! The Adler-32 and CRC-32 C surface: eleven exported symbols, no arithmetic.
//!
//! This module is the boundary face of `adler32.c` and `crc32.c`. It contains no
//! checksum logic whatsoever -- every sum, every table lookup and every modular
//! multiplication happens in [`mod@zlib_rs::adler32`] and [`mod@zlib_rs::crc32`],
//! which are compiled under `#![forbid(unsafe_code)]`. What lives here is the three
//! things a safe core structurally cannot do for itself:
//!
//! 1. **Integer-width conversion.** The C API speaks `uLong`, `uInt`, `z_size_t`,
//!    `z_off_t` and `z_off64_t`; the core speaks `u32`, `&[u8]` and `i64`.
//! 2. **Null-pointer handling.** `adler32(0L, Z_NULL, 0)` is the documented way to
//!    obtain a seed, and a `&[u8]` cannot be null, so the case has to be answered
//!    before the core is reached.
//! 3. **Sentinel pass-through.** Three documented degenerate inputs have
//!    implementation-defined answers that must survive the port unchanged.
//!
//! # The eleven exports
//!
//! Measured, not assumed: building the reference C library and grouping
//! `nm -D --defined-only --extern-only` yields exactly these eleven names for this
//! module. `get_crc_table` also belongs to the checksum family in the C sources but
//! is exported from `util.rs` here, alongside the other introspection entry points.
//!
//! | Symbol | `zlib.h` | Ported from | `zlib.map` node |
//! |---|---|---|---|
//! | [`adler32`] | L1809 | `adler32.c` L128-L130 | *(base set, undecorated)* |
//! | [`adler32_z`] | L1829 | `adler32.c` L61-L125 | `ZLIB_1.2.9` |
//! | [`adler32_combine`] | L1837 / L2019 | `adler32.c` L158-L160 | `ZLIB_1.2.2` |
//! | [`adler32_combine64`] | L1982 | `adler32.c` L162-L164 | `ZLIB_1.2.3.3` |
//! | [`crc32`] | L1848 | `crc32.c` L946-L951 | *(base set, undecorated)* |
//! | [`crc32_z`] | L1866 | `crc32.c` L626-L941 | `ZLIB_1.2.9` |
//! | [`crc32_combine`] | L1874 / L2020 | `crc32.c` L982-L984 | `ZLIB_1.2.2` |
//! | [`crc32_combine64`] | L1983 | `crc32.c` L977-L979 | `ZLIB_1.2.3.3` |
//! | [`crc32_combine_gen`] | L1884 / L2021 | `crc32.c` L964-L966 | `ZLIB_1.2.12` |
//! | [`crc32_combine_gen64`] | L1984 | `crc32.c` L954-L961 | `ZLIB_1.2.12` |
//! | [`crc32_combine_op`] | L1890 | `crc32.c` L969-L974 | `ZLIB_1.2.12` |
//!
//! The `zlib.map` column records what the *linker* does, not what this file does.
//! Nine of the eleven appear in the reference library as `@@ZLIB_x.y.z`-decorated
//! symbols and two as undecorated base-set names; every one of them is written here
//! as a plain `#[no_mangle]`, and the decoration comes solely from linking with
//! `--version-script=zlib.map`. Nothing in this module can, or should, produce it.
//!
//! # Integer-width discipline
//!
//! This is the highest-risk part of the module, so the rules are stated rather than
//! left to each call site.
//!
//! * Every return type, and every `adler`/`crc`/`op` parameter, is
//!   [`uLong`] -- which is [`core::ffi::c_ulong`], because `zconf.h` L406 makes
//!   `uLong` an `unsigned long`. Measured at 8 bytes here and 4 bytes on LLP64
//!   Windows. Writing `u64` or `u32` in one of those positions would silently
//!   corrupt every checksum return on one platform or the other.
//! * The core computes in `u32`, so each wrapper narrows on the way in with
//!   `narrow_checksum` and widens on the way out with `widen_checksum`. The
//!   narrowing is information-preserving, and that is a property of the reference
//!   implementation rather than an assumption: `adler32_z` reads only
//!   `(adler >> 16) & 0xffff` and `adler & 0xffff`, `crc32_z` only
//!   `(~crc) & 0xffffffff`, `adler32_combine_` masks with `0xffff` throughout,
//!   `crc32_combine_op` masks `crc1` and `crc2` with `0xffffffff` outright, and
//!   `multmodp` walks a mask that starts at `1 << 31` and only ever shifts right,
//!   so bits at or above position 32 are never examined by any of them.
//! * `len` is `uInt` for [`adler32`] and [`crc32`] but [`z_size_t`] for
//!   [`adler32_z`] and [`crc32_z`], and that is the *only* difference between each
//!   pair. A Rust slice carries its own length, so the distinction collapses to one
//!   implementation; the conversion direction is always widening, via
//!   [`crate::types::widen`], never narrowing.
//! * [`z_off_t`] and [`z_off64_t`] are both **signed** (`zconf.h` L494-L531) and are
//!   widened by `widen_offset`, which is generic precisely so the conversion stays
//!   sign-preserving and lint-clean on a target where the alias is already `i64` as
//!   well as on one where it is `i32`. No length is ever cast to an unsigned type
//!   here; the `< 0` tests belong to the core and run on the signed value.
//!
//! # `Z_NULL` is not an empty slice
//!
//! The C API overloads its buffer argument, and conflating the two cases is the most
//! likely defect at this boundary. Every value in this table was read off the
//! reference library:
//!
//! | Call | Answer | Why |
//! |---|---|---|
//! | `adler32(0, Z_NULL, 0)` | `1` | `adler32.c` L81-L82 |
//! | `adler32(5, Z_NULL, 0)` | `1` | the seed, **not** `5` |
//! | `adler32(9, Z_NULL, 99)` | `1` | the null test ignores `len` |
//! | `adler32(5, non_null, 0)` | `5` | an ordinary zero-length update |
//! | `adler32(0xffff_ffff, non_null, 0)` | `0x000e_000e` | halves normalised |
//! | `crc32(5, Z_NULL, 0)` | `0` | `crc32.c` L627-L628, **not** `5` |
//! | `crc32(5, Z_NULL, 77)` | `0` | the null test ignores `len` |
//! | `crc32(5, non_null, 0)` | `5` | the two complements cancel |
//!
//! So a null buffer yields the family's initial value **unconditionally**, and the
//! entry point answers it directly from [`ADLER32_EMPTY`] or [`CRC32_EMPTY`] without
//! calling the core -- which is what the core's own documentation requires of this
//! layer. Handing the core an empty slice instead would be wrong in exactly the rows
//! above where the incoming value is not the seed.
//!
//! One hazard in the C source is worth recording. `adler32_z` tests `len == 1` at
//! `adler32.c` L70, *before* it tests `buf == Z_NULL` at L81 -- the comment at L80
//! calls the ordering a "deferred check for len == 1 speed" -- so `adler32(a, NULL, 1)`
//! dereferences a null pointer in C. Rust reaches the null test first and answers
//! `1`, which is the same answer C gives for every null call it survives. The
//! undefined behaviour is removed without changing a single defined result.
//!
//! # The three sentinels
//!
//! None of these is computed here; each is produced by the core and passed through.
//! They are documented at this layer because a caller reads this surface, and
//! because all three are implementation-defined answers rather than consequences of
//! the format.
//!
//! * **A negative `len2` makes the Adler-32 combine return `0xffff_ffff`**
//!   ([`ADLER32_COMBINE_INVALID`]), from `adler32.c` L138-L140: "for negative len,
//!   return invalid adler32 as a clue for debugging". The value carries information
//!   -- a genuine Adler-32 has both halves below `BASE`, so `0xffff` in either half
//!   is unreachable -- and it must **not** be unified with the CRC-32 answer.
//! * **A negative `len2` makes the CRC-32 generator return `0`**
//!   ([`CRC32_COMBINE_INVALID`]), from `crc32.c` L955-L956, which `zlib.h`
//!   L1886-L1888 documents as "len2 must be non-negative, otherwise zero is
//!   returned".
//! * **A zero operator makes [`crc32_combine_op`] return `0`**, from `crc32.c`
//!   L970-L971. Not `crc1`, not `crc2` and not `crc1 ^ crc2`; every one of those is
//!   a plausible-looking wrong answer. Because the generator answers `0` for a
//!   negative length, this is also the mechanism by which
//!   [`crc32_combine`]/[`crc32_combine64`] return `0` rather than `crc1` for a
//!   negative length.
//!
//! A zero `len2` is emphatically *not* an error: the generator returns the identity
//! operator `0x8000_0000`, which is what makes `crc32_combine64(crc1, crc2, 0)`
//! collapse to `crc1 ^ crc2` and `adler32_combine(adler1, 1, 0)` collapse to
//! `adler1`.
//!
//! # Large-file duality
//!
//! Both faces of each pair are exported as distinct symbols, and that is a
//! requirement rather than a courtesy. `zconf.h` L500-L510 derives `Z_LARGE64` from
//! `_LARGEFILE64_SOURCE` and `Z_WANT64` from `_FILE_OFFSET_BITS == 64`; `zlib.h`
//! L2001-L2003 then `#define`s the unsuffixed names *to* the `*64` ones. Which name
//! a translation unit references is therefore decided by **that translation unit's
//! own** flags, at its own compile time, and nothing this library does can influence
//! it -- compiling one caller three ways and reading its undefined symbols shows it
//! referencing `adler32_combine` twice and `adler32_combine64` once. The duality is
//! live in this repository: `CMakeLists.txt` L174 and L219 attach
//! `_LARGEFILE64_SOURCE=1` as a PUBLIC definition on both library targets, so it
//! reaches the C test drivers through the `ZLIB::ZLIB` and `ZLIB::ZLIBSTATIC`
//! aliases.
//!
//! Note one asymmetry with the `gz` layer: the three `*64` checksum entry points
//! take [`z_off64_t`] in **every** configuration, including the `Z_WANT64`
//! re-declarations at `zlib.h` L2010-L2012, whereas `gzseek64`, `gztell64` and
//! `gzoffset64` degrade to `z_off_t` there. Both faces of a pair delegate to one
//! core function, so they cannot disagree; the tests assert it anyway.
//!
//! # `simd` is output-neutral, and there is no SIMD here
//!
//! Not one line of this module selects a backend or contains vector code. The
//! `simd` feature is a pass-through to `zlib-rs/simd`
//! (`crates/libz-rs-sys/Cargo.toml` L168), the vectorised backends live in
//! `crates/zlib-rs/src/adler32/simd.rs` and `crates/zlib-rs/src/crc32/simd.rs`, and
//! the `ZLIB_RS_SIMD=0|1` toggle is an assertion checked by this crate's `build.rs`
//! -- a build script cannot switch a Cargo feature on, so the variable never selects
//! anything. This module must not key off `cfg(zlib_rs_simd)` either, because doing
//! so would *be* backend selection.
//!
//! Checksums are the one place vectorisation is admissible at all, and the reason is
//! structural: a checksum is a single scalar however it is computed, so vectorising
//! it cannot perturb an emitted byte. It may change speed only. The tests below
//! assert absolute, reference-measured values rather than self-consistency, so
//! running them with the feature on and with it off is a direct proof of that.
//!
//! # Panic and unsafe posture
//!
//! Every export is `extern "C"`, never `extern "C-unwind"`, and every one routes
//! through [`crate::panic_guard::guard`] exactly once, so a panic aborts instead of
//! unwinding into a caller that was not compiled to expect it. The forwarding that
//! the C sources do between `adler32` and `adler32_z` happens *below* the guard,
//! through a shared private body, so no call ever installs two guards.
//!
//! The four entry points that take a buffer are declared `unsafe` because they
//! dereference a caller-supplied pointer; the other seven take no pointer and are
//! safe. `unsafe` on a Rust function item changes neither the emitted symbol, the
//! calling convention, nor the generated C declaration, so the `zlib.h` contract is
//! untouched -- it only obliges a *Rust* caller to acknowledge the invariant. All of
//! the unsafety is one category: AAP §0.6.1 site 2, reconstructing a slice from a
//! `(buf, len)` pair, and it is reached through the single `buffer_slice` helper so
//! that [`core::slice::from_raw_parts`] appears once in the module and can be audited
//! once.
//!
//! [`ADLER32_EMPTY`]: crate::panic_guard::fallback::ADLER32_EMPTY
//! [`CRC32_EMPTY`]: crate::panic_guard::fallback::CRC32_EMPTY
//! [`ADLER32_COMBINE_INVALID`]: crate::panic_guard::fallback::ADLER32_COMBINE_INVALID
//! [`CRC32_COMBINE_INVALID`]: crate::panic_guard::fallback::CRC32_COMBINE_INVALID

use zlib_rs::{adler32 as rs_adler32, crc32 as rs_crc32};

use crate::panic_guard::{fallback, guard};
use crate::types::{uInt, uLong, widen, z_off64_t, z_off_t, z_size_t, Bytef};

// ---------------------------------------------------------------------------
// Width conversion -- the C API's integers to the core's, and back
// ---------------------------------------------------------------------------

/// Takes the low 32 bits of a `uLong`-typed checksum, check value or operator.
///
/// The reference implementation never examines a higher bit of any of them, so this
/// discards nothing a caller could observe; see the module documentation for the
/// per-function evidence. Written as an explicit mask plus a cast rather than as
/// `try_into().unwrap()`, because the library may not panic, and rather than as a
/// bare `as u32`, because the mask states the intent and mirrors the C source's own
/// `crc1 & 0xffffffff`.
///
/// The mask is also what keeps this free of `clippy::cast_possible_truncation`: with
/// it, the value is provably in range; without it, the lint fires.
///
/// ★ On i686 and on LLP64 Windows `uLong` is already `u32`, so the masked expression
/// is `u32 as u32` and three lints fire instead of none: `trivial_numeric_casts` and
/// `clippy::unnecessary_cast` on the cast, and `clippy::identity_op` on the mask, which
/// covers the full `u32` range there and so has no effect. No single spelling is clean on
/// every supported target -- `u32::from` would be a `useless_conversion` there and
/// `u32::try_from` an `unnecessary_fallible_conversion` -- so the three are allowed here,
/// scoped to this one-line function exactly as `crate::deflate`'s width helpers scope
/// theirs. The mask is kept because it is load-bearing on LP64, where it is what makes the
/// narrowing provably in range, and because it mirrors the C source's own
/// `crc1 & 0xffffffff`.
#[inline]
#[must_use]
#[allow(trivial_numeric_casts, clippy::unnecessary_cast, clippy::identity_op)]
const fn narrow_checksum(value: uLong) -> u32 {
    (value & 0xffff_ffff) as u32
}

/// Returns a 32-bit checksum at the width the C API declares.
///
/// A `From` conversion rather than a cast, so it is exact on LP64, where `c_ulong`
/// is 8 bytes, and the identity on LLP64 Windows, where it is 4. Never write `as
/// c_ulong` in this direction: it would be a trivial cast on the second of those
/// targets and a lint failure there.
// `useless_conversion` is unreachable on any target where `c_ulong` is wider than
// `u32`, which is every target this crate is built for today. The allowance covers
// LLP64 Windows, where the conversion really is the identity and the lint would
// fire; there is no cast-free spelling that suits both widths.
#[inline]
#[must_use]
#[allow(clippy::useless_conversion)]
fn widen_checksum(value: u32) -> uLong {
    uLong::from(value)
}

/// Widens a caller's byte count to the signed 64-bit length the core takes.
///
/// Serves both faces of every combine pair: [`z_off_t`] is `c_long` -- 8 bytes on
/// LP64, 4 on a 32-bit target -- while [`z_off64_t`] is always `i64`, and `Into<i64>`
/// holds for all three. `From` between signed integers is sign-preserving by
/// construction, so a negative length arrives at the core still negative and its
/// sentinel test still sees it.
///
/// **Generic on purpose, and it must stay generic.** The concrete conversion is
/// invisible to the lint pass inside a generic body, which is the only way to write
/// it once and stay clean on every target: a non-generic `i64::from(len2)` trips
/// `clippy::useless_conversion` and `len2 as i64` trips `clippy::unnecessary_cast`
/// together with `trivial_numeric_casts` wherever the alias already *is* `i64` --
/// which is this target.
#[inline]
#[must_use]
fn widen_offset<T: Into<i64>>(len2: T) -> i64 {
    len2.into()
}

// ---------------------------------------------------------------------------
// Slice reconstruction -- unsafe-site category 2
// ---------------------------------------------------------------------------

/// Rebuilds a caller's buffer as a slice from a `(buf, len)` pair.
///
/// The only place in this module that calls [`core::slice::from_raw_parts`], so the
/// one unsafe operation it performs is written once and audited once. Called
/// immediately on entry; only the resulting shared slice travels inward to
/// [`zlib_rs`].
///
/// [`crate::types::input_slice`] is deliberately *not* reused here. Its length
/// parameter is [`uInt`], and narrowing a [`z_size_t`] to that for [`adler32_z`] or
/// [`crc32_z`] would truncate any buffer above four gibibytes -- which is the entire
/// reason those two entry points exist.
///
/// # Neither degenerate case is a formality
///
/// `core::slice::from_raw_parts(null, 0)` is **undefined behaviour**: the pointer
/// must be non-null and aligned even for an empty slice. Both entry-point families
/// answer a null buffer before reaching this function, so the null test below is
/// defence in depth -- it is what makes a future reordering of those checks a wrong
/// answer rather than undefined behaviour. The zero-length test is what keeps a
/// caller's pointer from being assumed dereferenceable when there is nothing to
/// read.
///
/// # Safety
///
/// If `len` is non-zero, `buf` must be non-null and readable for `len` bytes, and
/// that region must stay valid and unwritten by anything else for `'a`. The returned
/// borrow's lifetime is unconstrained by the argument, which is inherent to an FFI
/// boundary: the caller of this function is responsible for not outliving the
/// buffer.
#[must_use]
unsafe fn buffer_slice<'a>(buf: *const Bytef, len: usize) -> &'a [u8] {
    if buf.is_null() || len == 0 {
        return &[];
    }
    // SAFETY: unsafe-site category 2 -- slice reconstruction. `buf` is non-null by
    // the test above, and `u8` has alignment 1, so any non-null pointer is aligned
    // for it. `len` bytes at `buf` are readable and stay unwritten for `'a` by this
    // function's documented contract, which each exported entry point inherits from
    // the `(buf, len)` pair `zlib.h` declares. `from_raw_parts` is reached exactly
    // once in this module -- here -- and only the resulting shared slice travels
    // inward; nothing writes through it, which is why a shared borrow is the right
    // shape.
    unsafe { core::slice::from_raw_parts(buf, len) }
}

// ---------------------------------------------------------------------------
// Shared bodies -- the C sources' internal forwarding, kept below the guard
// ---------------------------------------------------------------------------

/// The body shared by [`adler32`] and [`adler32_z`].
///
/// `adler32.c` L128-L130 makes `adler32` a one-line forwarder to `adler32_z`, and
/// this reproduces that -- but one level lower, so that each exported symbol
/// installs exactly one [`guard`] and no call nests two.
///
/// # Safety
///
/// As [`buffer_slice`]: if `len` is non-zero, `buf` must be non-null and readable
/// for `len` bytes for the duration of the call.
#[must_use]
unsafe fn adler32_update(adler: uLong, buf: *const Bytef, len: usize) -> uLong {
    if buf.is_null() {
        // `adler32.c` L81-L82: `if (buf == Z_NULL) return 1L;`. Unconditional in the
        // incoming checksum and in `len`, and deliberately not routed through the
        // core, which would answer with the incoming value instead.
        return fallback::ADLER32_EMPTY;
    }
    // SAFETY: unsafe-site category 2, discharged by this function's own contract:
    // `buf` is non-null by the test above and its `len` bytes are readable for the
    // duration of the call. Called once, on entry; only the safe slice goes inward.
    let data = unsafe { buffer_slice(buf, len) };
    widen_checksum(rs_adler32::adler32_z(narrow_checksum(adler), data))
}

/// The body shared by [`crc32`] and [`crc32_z`].
///
/// `crc32.c` L946-L951 makes `crc32` a forwarder to `crc32_z`, reproduced here one
/// level below the guard for the same reason as [`adler32_update`].
///
/// # Safety
///
/// As [`buffer_slice`]: if `len` is non-zero, `buf` must be non-null and readable
/// for `len` bytes for the duration of the call.
#[must_use]
unsafe fn crc32_update(crc: uLong, buf: *const Bytef, len: usize) -> uLong {
    if buf.is_null() {
        // `crc32.c` L627-L628: `if (buf == Z_NULL) return 0;`. Unlike the Adler-32
        // path this test is first in the C source too, so there is no `len == 1`
        // hazard to remove here -- only the same unconditional answer to reproduce.
        return fallback::CRC32_EMPTY;
    }
    // SAFETY: unsafe-site category 2, discharged by this function's own contract:
    // `buf` is non-null by the test above and its `len` bytes are readable for the
    // duration of the call. Called once, on entry; only the safe slice goes inward.
    let data = unsafe { buffer_slice(buf, len) };
    widen_checksum(rs_crc32::crc32_z(narrow_checksum(crc), data))
}

// ---------------------------------------------------------------------------
// Adler-32 -- `zlib.h` L1805-L1845, ported from `adler32.c`
// ---------------------------------------------------------------------------

/// Update a running Adler-32 checksum with `buf[0..len-1]` and return it.
///
/// `zlib.h` L1809: `uLong adler32(uLong adler, const Bytef *buf, uInt len);`. Ported
/// from `adler32.c` L128-L130, which is a one-line forwarder to `adler32_z`; the only
/// difference between the two is that `len` is a `uInt` here, and that difference
/// disappears once the buffer is a slice.
///
/// A fresh checksum comes from `adler32(0L, Z_NULL, 0)`, which returns `1`; `zlib.h`
/// L1812-L1814 documents that a `Z_NULL` buffer "returns the required initial value
/// for the checksum". Successive calls accumulate, so any chunking of an input yields
/// the value of one call over the whole of it.
///
/// An Adler-32 is almost as reliable as a CRC-32 and much faster to compute
/// (`zlib.h` L1816-L1817). See the module documentation for the full table of null
/// and empty answers, which are **not** the same thing.
///
/// # Safety
///
/// If `len` is non-zero, `buf` must be non-null and readable for `len` bytes for the
/// duration of the call. A null `buf` is explicitly permitted and is the documented
/// way to obtain the seed; `len` is then ignored.
#[no_mangle]
pub unsafe extern "C" fn adler32(adler: uLong, buf: *const Bytef, len: uInt) -> uLong {
    guard(|| {
        // SAFETY: this function's own contract is exactly `adler32_update`'s, and it
        // is discharged unchanged: a non-zero `len` implies `buf` is non-null and
        // readable for that many bytes. `widen` is the crate's `uInt` -> `usize`
        // conversion and cannot lose a bit.
        unsafe { adler32_update(adler, buf, widen(len)) }
    })
}

/// Update a running Adler-32 checksum with `buf[0..len-1]` and return it.
///
/// `zlib.h` L1829-L1830: `uLong adler32_z(uLong adler, const Bytef *buf, z_size_t
/// len);`, documented at L1832-L1833 as "Same as `adler32()`, but with a `size_t`
/// length. Note that a long is 32 bits on Windows." Ported from `adler32.c` L61-L125,
/// which is where the whole algorithm actually lives -- the `NMAX`-block loop, the
/// sub-16-byte short path and the single-byte fast path.
///
/// Prefer this form over [`adler32`] for a buffer that can exceed a `uInt`: the
/// length is a [`z_size_t`], so nothing has to be split to be checksummed.
///
/// # Safety
///
/// As [`adler32`]: if `len` is non-zero, `buf` must be non-null and readable for
/// `len` bytes for the duration of the call. A null `buf` returns the seed.
#[no_mangle]
pub unsafe extern "C" fn adler32_z(adler: uLong, buf: *const Bytef, len: z_size_t) -> uLong {
    guard(|| {
        // SAFETY: unsafe-site category 2 -- the obligation is this function's own and it
        // reaches `adler32_update` unchanged: a non-zero `len` implies `buf` is non-null and
        // readable for exactly that many bytes for the duration of the call, and `u8` has
        // alignment 1 so any non-null `buf` is aligned. A null `buf` is not dereferenced at
        // all -- `adler32_update` tests it and returns the seed -- so the null case carries no
        // obligation. Nothing is written through the pointer and nothing retains it. `len`
        // needs no conversion here: `z_size_t` is `size_t` and `types.rs` asserts its equality
        // with `usize` at compile time.
        unsafe { adler32_update(adler, buf, len) }
    })
}

/// Combine two Adler-32 checksums into the checksum of the concatenated sequences.
///
/// `zlib.h` L1837-L1838 and L2019: `uLong adler32_combine(uLong adler1, uLong adler2,
/// z_off_t len2);`. Ported from `adler32.c` L158-L160, which forwards to the `local`
/// `adler32_combine_` at L133-L155.
///
/// For two byte sequences `seq1` and `seq2` whose checksums are `adler1` and `adler2`,
/// and where `seq2` is `len2` bytes long, this returns the checksum of `seq1`
/// followed by `seq2` -- from those three numbers alone, touching neither sequence.
/// The length of `seq1` is not needed.
///
/// # Degenerate lengths
///
/// `zlib.h` L1844-L1845 notes that `z_off_t` is signed and that a negative `len2`
/// leaves the result with "no meaning or utility". The implementation is more
/// specific, and it is the implementation that must be reproduced: a negative `len2`
/// returns `0xffff_ffff`, an unmistakable non-checksum. A zero `len2` is not an
/// error -- `adler32_combine(adler1, 1, 0)` is `adler1`, because `1` is the checksum
/// of an empty sequence.
///
/// This and [`adler32_combine64`] delegate to one core function and therefore cannot
/// disagree.
#[no_mangle]
pub extern "C" fn adler32_combine(adler1: uLong, adler2: uLong, len2: z_off_t) -> uLong {
    guard(|| {
        widen_checksum(rs_adler32::adler32_combine(
            narrow_checksum(adler1),
            narrow_checksum(adler2),
            widen_offset(len2),
        ))
    })
}

/// Combine two Adler-32 checksums into the checksum of the concatenated sequences.
///
/// `zlib.h` L1982: `uLong adler32_combine64(uLong, uLong, z_off64_t);`. Ported from
/// `adler32.c` L162-L164, which forwards to the same `local` helper
/// [`adler32_combine`] does.
///
/// This is the large-file face of the pair, and it exists as its own symbol because
/// `zlib.h` L2001 `#define`s `adler32_combine` to this name for any caller compiled
/// with `_FILE_OFFSET_BITS == 64`. Its `len2` is [`z_off64_t`] in every
/// configuration, including the `Z_WANT64` re-declaration at `zlib.h` L2010 -- unlike
/// `gzseek64` and its neighbours, which degrade to `z_off_t` there.
///
/// Semantics, including the `0xffff_ffff` sentinel for a negative `len2`, are
/// [`adler32_combine`]'s exactly; both delegate to one core function.
#[no_mangle]
pub extern "C" fn adler32_combine64(adler1: uLong, adler2: uLong, len2: z_off64_t) -> uLong {
    guard(|| {
        widen_checksum(rs_adler32::adler32_combine(
            narrow_checksum(adler1),
            narrow_checksum(adler2),
            widen_offset(len2),
        ))
    })
}

// ---------------------------------------------------------------------------
// CRC-32 -- `zlib.h` L1848-L1895, ported from `crc32.c`
// ---------------------------------------------------------------------------

/// Update a running CRC-32 with `buf[0..len-1]` and return it.
///
/// `zlib.h` L1848: `uLong crc32(uLong crc, const Bytef *buf, uInt len);`. Ported from
/// `crc32.c` L946-L951, a forwarder to `crc32_z`.
///
/// A fresh check value comes from `crc32(0L, Z_NULL, 0)`, which returns `0`; `zlib.h`
/// L1851-L1853 documents that a `Z_NULL` buffer returns the required initial value.
/// Pre- and post-conditioning -- the one's complement at each end -- is performed
/// inside this function, so an application must not do it itself (`zlib.h`
/// L1853-L1855). Because the two complements cancel across a call boundary,
/// successive calls accumulate and any chunking yields the one-shot value.
///
/// # Safety
///
/// If `len` is non-zero, `buf` must be non-null and readable for `len` bytes for the
/// duration of the call. A null `buf` is explicitly permitted and returns the initial
/// value; `len` is then ignored.
#[no_mangle]
pub unsafe extern "C" fn crc32(crc: uLong, buf: *const Bytef, len: uInt) -> uLong {
    guard(|| {
        // SAFETY: this function's own contract is exactly `crc32_update`'s, and it is
        // discharged unchanged: a non-zero `len` implies `buf` is non-null and
        // readable for that many bytes. `widen` is the crate's `uInt` -> `usize`
        // conversion and cannot lose a bit.
        unsafe { crc32_update(crc, buf, widen(len)) }
    })
}

/// Update a running CRC-32 with `buf[0..len-1]` and return it.
///
/// `zlib.h` L1866-L1867: `uLong crc32_z(uLong crc, const Bytef *buf, z_size_t len);`,
/// documented at L1869-L1870 as "Same as `crc32()`, but with a `size_t` length."
/// Ported from `crc32.c` L626-L941, which holds the braided word-at-a-time body and
/// the byte tail.
///
/// Prefer this form over [`crc32`] for a buffer that can exceed a `uInt`.
///
/// # Safety
///
/// As [`crc32`]: if `len` is non-zero, `buf` must be non-null and readable for `len`
/// bytes for the duration of the call. A null `buf` returns the initial value.
#[no_mangle]
pub unsafe extern "C" fn crc32_z(crc: uLong, buf: *const Bytef, len: z_size_t) -> uLong {
    guard(|| {
        // SAFETY: unsafe-site category 2 -- the obligation is this function's own and it
        // reaches `crc32_update` unchanged: a non-zero `len` implies `buf` is non-null and
        // readable for exactly that many bytes for the duration of the call, and `u8` has
        // alignment 1 so any non-null `buf` is aligned. A null `buf` is not dereferenced at
        // all -- `crc32_update` tests it and returns the initial value -- so the null case
        // carries no obligation. Nothing is written through the pointer and nothing retains
        // it. `len` needs no conversion here: `z_size_t` is `size_t` and `types.rs` asserts
        // its equality with `usize` at compile time.
        unsafe { crc32_update(crc, buf, len) }
    })
}

/// Combine two CRC-32 check values into the check value of the concatenation.
///
/// `zlib.h` L1874 and L2020: `uLong crc32_combine(uLong crc1, uLong crc2, z_off_t
/// len2);`. Ported from `crc32.c` L982-L984, a widening forwarder to
/// `crc32_combine64`.
///
/// For `seq1` and `seq2` with check values `crc1` and `crc2`, where `seq2` is `len2`
/// bytes long, this returns the check value of `seq1` followed by `seq2`. When one
/// length recurs, [`crc32_combine_gen`] and [`crc32_combine_op`] are faster, because
/// the operator is then computed once and reused (`zlib.h` L1892-L1894).
///
/// # Degenerate lengths
///
/// `zlib.h` L1880-L1881: "len2 must be non-negative, otherwise zero is returned."
/// That zero arrives through the composition rather than through a guard of its own
/// -- the generator answers `0` and [`crc32_combine_op`] answers `0` for a zero
/// operator -- so the result is `0` and **not** `crc1`. A zero `len2` yields
/// `crc1 ^ crc2`, which is the arithmetically right answer for an empty second
/// sequence.
#[no_mangle]
pub extern "C" fn crc32_combine(crc1: uLong, crc2: uLong, len2: z_off_t) -> uLong {
    guard(|| {
        widen_checksum(rs_crc32::crc32_combine(
            narrow_checksum(crc1),
            narrow_checksum(crc2),
            widen_offset(len2),
        ))
    })
}

/// Combine two CRC-32 check values into the check value of the concatenation.
///
/// `zlib.h` L1983: `uLong crc32_combine64(uLong, uLong, z_off64_t);`. Ported from
/// `crc32.c` L977-L979, which composes `crc32_combine_gen64` with
/// `crc32_combine_op`.
///
/// The large-file face of the pair, exported as its own symbol because `zlib.h` L2002
/// `#define`s `crc32_combine` to this name for a caller compiled with
/// `_FILE_OFFSET_BITS == 64`. Semantics are [`crc32_combine`]'s exactly, including the
/// `0` answer for a negative `len2` and `crc1 ^ crc2` for a zero one.
#[no_mangle]
pub extern "C" fn crc32_combine64(crc1: uLong, crc2: uLong, len2: z_off64_t) -> uLong {
    guard(|| {
        widen_checksum(rs_crc32::crc32_combine64(
            narrow_checksum(crc1),
            narrow_checksum(crc2),
            widen_offset(len2),
        ))
    })
}

/// Return the operator corresponding to a second sequence of `len2` bytes.
///
/// `zlib.h` L1884 and L2021: `uLong crc32_combine_gen(z_off_t len2);`. Ported from
/// `crc32.c` L964-L966, a widening forwarder to `crc32_combine_gen64`.
///
/// Hand the result to [`crc32_combine_op`] together with two check values. Computing
/// it once and reusing it is the whole point of the split, and is faster than
/// [`crc32_combine`] whenever the same `len2` recurs (`zlib.h` L1886-L1888,
/// L1892-L1894).
///
/// # Degenerate lengths
///
/// `zlib.h` L1887-L1888: "len2 must be non-negative, otherwise zero is returned."
/// A zero `len2` is *not* that error case -- it yields the identity operator
/// `0x8000_0000`, which is what makes [`crc32_combine64`] collapse to `crc1 ^ crc2`.
#[no_mangle]
pub extern "C" fn crc32_combine_gen(len2: z_off_t) -> uLong {
    guard(|| widen_checksum(rs_crc32::crc32_combine_gen(widen_offset(len2))))
}

/// Return the operator corresponding to a second sequence of `len2` bytes.
///
/// `zlib.h` L1984: `uLong crc32_combine_gen64(z_off64_t);`. Ported from `crc32.c`
/// L954-L961, which is where the `len2 < 0` test and the `x2nmodp(len2, 3)` call
/// actually live -- the literal `3` scaling bytes to bits.
///
/// The large-file face of the pair, exported as its own symbol because `zlib.h` L2003
/// `#define`s `crc32_combine_gen` to this name under `Z_WANT64`. Semantics are
/// [`crc32_combine_gen`]'s exactly.
#[no_mangle]
pub extern "C" fn crc32_combine_gen64(len2: z_off64_t) -> uLong {
    guard(|| widen_checksum(rs_crc32::crc32_combine_gen64(widen_offset(len2))))
}

/// Combine two CRC-32 check values using a precomputed operator.
///
/// `zlib.h` L1890: `uLong crc32_combine_op(uLong crc1, uLong crc2, uLong op);`. Ported
/// from `crc32.c` L969-L974. Gives the same result as [`crc32_combine`] with `op` in
/// place of `len2`, where `op` came from [`crc32_combine_gen`] (`zlib.h`
/// L1892-L1894).
///
/// It takes no length, so the large-file duality never touches it: there is one
/// symbol and no `*64` counterpart. `op` is nevertheless a `uLong`, so it is narrowed
/// exactly as a check value is.
///
/// # A zero operator returns zero
///
/// `crc32.c` L970-L971 returns `0` when `op == 0`, and this reproduces it literally.
/// It is worth naming what it does *not* return, because each is a plausible-looking
/// wrong answer: not `crc1`, not `crc2`, and not `crc1 ^ crc2`. The guard is also what
/// keeps the core's modular multiply inside its stated precondition, a zero
/// multiplier being the one input the reference cannot handle (`crc32.c` L159-L162).
#[no_mangle]
pub extern "C" fn crc32_combine_op(crc1: uLong, crc2: uLong, op: uLong) -> uLong {
    guard(|| {
        widen_checksum(rs_crc32::crc32_combine_op(
            narrow_checksum(crc1),
            narrow_checksum(crc2),
            narrow_checksum(op),
        ))
    })
}

// Test code is the one place in this crate where panicking is permitted: the root
// `clippy.toml` grants `allow-panic-in-tests`, `allow-unwrap-in-tests` and
// `allow-expect-in-tests`, and nothing below this banner is compiled into the shipped
// library. `indexing_slicing` is NOT relaxed by those keys, so the tests below index
// nothing -- every traversal is an iterator, a `chunks` walk or a `.get`.
//
// ★ Every expectation below is a value MEASURED on the reference C library, built
// out of tree from the in-tree sources and linked into a probe that printed them.
// They are absolute, not self-consistent: no test asserts merely that this
// implementation agrees with itself. That is what makes the suite a differential test
// that needs no C compiler to run, and it is also what makes running it twice -- once
// with the `simd` feature and once without -- a direct proof that vectorisation is
// output-neutral.
#[cfg(test)]
mod tests {
    use core::ptr;

    use super::{
        adler32, adler32_combine, adler32_combine64, adler32_z, buffer_slice, crc32, crc32_combine,
        crc32_combine64, crc32_combine_gen, crc32_combine_gen64, crc32_combine_op, crc32_z,
        narrow_checksum, widen_checksum, widen_offset,
    };
    use crate::panic_guard::fallback;
    use crate::types::{uLong, z_off64_t, z_off_t};

    /// The C probe's test buffer: `pattern[i] = (unsigned char)(i * 31 + 7)`.
    ///
    /// Built with a running add rather than a cast, which is exact because reduction
    /// modulo 256 is a ring homomorphism: `(i * 31 + 7) mod 256` advances by
    /// `31 mod 256` at every step.
    fn pattern(len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut byte: u8 = 7;
        for _ in 0..len {
            out.push(byte);
            byte = byte.wrapping_add(31);
        }
        out
    }

    /// Narrows a test length to the caller-side [`z_off_t`], or reports that this
    /// target's `z_off_t` cannot express it.
    ///
    /// Generic for the same reason [`widen_offset`] is: on a target where `z_off_t`
    /// already is `i64` a direct `try_from` would trip
    /// `clippy::unnecessary_fallible_conversions`, and inside a generic body the
    /// concrete type is invisible to the lint. Returning [`None`] lets the
    /// large-length rows degrade on a 32-bit target instead of failing there.
    fn as_off_t<T: TryFrom<i64>>(n: i64) -> Option<T> {
        T::try_from(n).ok()
    }

    /// True when `value` carries nothing above bit 31.
    ///
    /// Written as a mask rather than `value >> 32` so it does not overflow on LLP64
    /// Windows, where `uLong` is only 32 bits wide and the shift would panic.
    ///
    /// ★ On exactly those targets the complement is zero, so the expression is
    /// `value & 0 == 0` and both `clippy::bad_bit_mask` and `clippy::erasing_op` fire --
    /// correctly, and unhelpfully: the answer really is a constant `true` where `uLong`
    /// cannot hold a thirty-second bit. That is the intended reading, and the mask is what
    /// keeps the LP64 build meaningful, so the two are allowed here for the same
    /// target-dependence reason [`narrow_checksum`] documents.
    #[allow(clippy::bad_bit_mask, clippy::erasing_op)]
    fn fits_in_32_bits(value: uLong) -> bool {
        value & !0xffff_ffff == 0
    }

    /// `(len, adler32(1, pattern, len), crc32(0, pattern, len))`, measured on the C
    /// library.
    ///
    /// The lengths bracket every path in both algorithms: the Adler-32 single-byte
    /// fast path (1), its sub-16-byte short path (2, 3, 15), the 16-byte unrolled
    /// step (16, 17, 31, 32), the CRC braid activation threshold `N*W + W - 1 == 47`
    /// (46, 47, 48), and the Adler-32 `NMAX` block boundary of 5552 (5551, 5552,
    /// 5553).
    const PATTERN_FIXTURES: &[(usize, uLong, uLong)] = &[
        (0, 0x0000_0001, 0x0000_0000),
        (1, 0x0008_0008, 0x4c66_7a2e),
        (2, 0x0036_002e, 0xdc95_01c5),
        (3, 0x00a9_0073, 0x3f66_f9ac),
        (15, 0x3227_0721, 0x8f77_fabb),
        (16, 0x3a20_07f9, 0x0636_a895),
        (17, 0x4310_08f0, 0x71b8_d951),
        (31, 0xeb4f_0f29, 0x45d6_9b08),
        (32, 0xfb40_0ff1, 0x4923_fba6),
        (46, 0x0b64_1698, 0xde6e_9ea4),
        (47, 0x2295_1731, 0x8ab4_cd02),
        (48, 0x3a7e_17e9, 0xf93c_01d2),
        (63, 0xdfcc_1f39, 0x794b_269d),
        (64, 0xffad_1fe1, 0x84c8_6088),
        (255, 0x7a30_7e99, 0x50a2_2b05),
        (256, 0xf9b1_7f81, 0x0ce9_d363),
        (1023, 0x15b4_fd28, 0xe19e_9a00),
        (1024, 0x13d3_fe10, 0x7c32_1b5d),
        (4095, 0x2430_f782, 0x7b3f_9134),
        (4096, 0x1ca9_f86a, 0x5d1c_4ee3),
        (5551, 0x5f26_cd87, 0x6f4d_eddc),
        (5552, 0x2cf4_cdbf, 0x750a_8401),
        (5553, 0xfb0a_ce16, 0x507f_11c8),
        (8192, 0xb23b_f0e2, 0x2eb8_eea8),
    ];

    /// `test/example.c` L35: the payload the existing suite compresses.
    const HELLO: &[u8] = b"hello, hello!";
    /// `adler32(1, HELLO, 13)` and `crc32(0, HELLO, 13)`, measured.
    const HELLO_ADLER: uLong = 0x2170_0496;
    const HELLO_CRC: uLong = 0xb39a_dc9b;

    /// The same string split after `"hello, "`, so the halves can be recombined.
    const HELLO_HEAD: &[u8] = b"hello, ";
    const HELLO_TAIL: &[u8] = b"hello!";
    const HEAD_ADLER: uLong = 0x0ace_0261;
    const TAIL_ADLER: uLong = 0x0862_0236;
    const HEAD_CRC: uLong = 0x11ea_5699;
    const TAIL_CRC: uLong = 0x9a86_c960;

    /// Lengths spanning the whole combine domain: zero, one, either side of
    /// `BASE == 65521`, values past `u32::MAX`, and four negatives including
    /// `i64::MIN`.
    const COMBINE_LENGTHS: &[i64] = &[
        0,
        1,
        2,
        65_520,
        65_521,
        65_522,
        131_042,
        1_048_576,
        2_147_483_647,
        4_294_967_296,
        -1,
        -2,
        -65_521,
        i64::MIN,
    ];

    /// Combine fixtures, measured: `(len2, adler32_combine, crc32_combine, gen)`.
    const COMBINE_FIXTURES: &[(i64, uLong, uLong, uLong)] = &[
        (0, 0x26fc_068a, 0x7730_c1d2, 0x8000_0000),
        (1, 0x2b91_068a, 0xc2fd_9921, 0x0080_0000),
        (2, 0x3026_068a, 0xe6fc_f7df, 0x0000_8000),
        (65_520, 0x2267_068a, 0x40d0_86d9, 0x1383_5d16),
        (65_521, 0x26fc_068a, 0x5518_a0ee, 0xf4c7_360c),
        (65_522, 0x2b91_068a, 0xedb0_cdc7, 0x0942_8b1d),
        (131_042, 0x26fc_068a, 0x67ff_775c, 0x5ac3_fe9b),
        (1_048_576, 0x72e8_068a, 0xdcd1_0e16, 0x5fde_7a4e),
        (2_147_483_647, 0xa5f8_068a, 0x1b45_bbaa, 0x6d3d_2d4d),
        (4_294_967_296, 0x2e2d_068a, 0xc2fd_9921, 0x0080_0000),
        (-1, 0xffff_ffff, 0x0000_0000, 0x0000_0000),
        (-2, 0xffff_ffff, 0x0000_0000, 0x0000_0000),
        (-65_521, 0xffff_ffff, 0x0000_0000, 0x0000_0000),
        (i64::MIN, 0xffff_ffff, 0x0000_0000, 0x0000_0000),
    ];

    /// The checksums the `COMBINE_FIXTURES` were measured with.
    const COMBINE_ADLER1: uLong = 0x2170_0496;
    const COMBINE_ADLER2: uLong = 0x058c_01f5;
    const COMBINE_CRC1: uLong = 0x6b1d_dbed;
    const COMBINE_CRC2: uLong = 0x1c2d_1a3f;

    // -----------------------------------------------------------------------
    // The private helpers
    // -----------------------------------------------------------------------

    /// The generator itself has to be right before anything asserted with it is.
    #[test]
    fn the_pattern_generator_reproduces_the_c_probes_buffer() {
        assert_eq!(pattern(0), Vec::<u8>::new());
        // i * 31 + 7 for i in 0..5, reduced modulo 256.
        assert_eq!(pattern(5), vec![7, 38, 69, 100, 131]);
        assert_eq!(pattern(9).len(), 9);
        // Position 9 is where the reduction first bites: 9 * 31 + 7 == 286 -> 30.
        assert_eq!(pattern(10).last().copied(), Some(30));
    }

    /// Narrowing keeps the low 32 bits and drops nothing else.
    #[test]
    fn narrow_checksum_keeps_exactly_the_low_thirty_two_bits() {
        assert_eq!(narrow_checksum(0), 0);
        assert_eq!(narrow_checksum(1), 1);
        assert_eq!(narrow_checksum(0xffff_ffff), 0xffff_ffff);
        // Only exercisable where `uLong` is wider than 32 bits, which is every target
        // but i686 and LLP64 Windows; on those the mask is already the identity.
        //
        // The 64-bit probe is assembled from two 32-bit halves rather than written as
        // `0x1234_5678_9abc_def0`, because a literal that wide does not fit `uLong`
        // where `uLong` is `u32` and `overflowing_literals` rejects it at compile time
        // -- before this run-time guard can skip it. `wrapping_shl` rather than `<<`
        // for the same reason: a shift of 32 is well defined for every width here and
        // needs no second `cfg` arm. `widen_checksum` carries the one-target
        // `useless_conversion` allowance, so neither half needs a cast.
        if !fits_in_32_bits(!0) {
            assert_eq!(narrow_checksum(!0), 0xffff_ffff);
            let probe = widen_checksum(0x1234_5678).wrapping_shl(32) | widen_checksum(0x9abc_def0);
            assert_eq!(narrow_checksum(probe), 0x9abc_def0);
        }
    }

    /// Widening is exact and never sets a bit above 31.
    #[test]
    fn widen_checksum_is_exact_and_stays_in_range() {
        for value in [0_u32, 1, 0xffff, 0x8000_0000, 0xffff_ffff] {
            let widened = widen_checksum(value);
            assert!(fits_in_32_bits(widened));
            assert_eq!(narrow_checksum(widened), value);
        }
    }

    /// Offset widening preserves sign, which is what keeps the sentinels reachable.
    #[test]
    fn widen_offset_preserves_sign_for_both_widths() {
        assert_eq!(widen_offset(0_i64), 0);
        assert_eq!(widen_offset(i64::MIN), i64::MIN);
        assert_eq!(widen_offset(i64::MAX), i64::MAX);
        assert_eq!(widen_offset(-1_i64), -1);
        // Through the caller-side alias, exactly as an exported entry point does.
        let minus_one: z_off_t = as_off_t(-1).expect("z_off_t holds -1 on every target");
        assert_eq!(widen_offset(minus_one), -1);
        let zero: z_off64_t = 0;
        assert_eq!(widen_offset(zero), 0);
    }

    /// The one unsafe operation in the module refuses to build a dangling slice.
    #[test]
    fn buffer_slice_never_hands_from_raw_parts_a_null_pointer() {
        // SAFETY: the contract is vacuous for a zero length, and a null pointer with
        // any length takes the early return without reading -- which is the property
        // under test.
        unsafe {
            assert!(buffer_slice(ptr::null(), 0).is_empty());
            assert!(buffer_slice(ptr::null(), 99).is_empty());
        }
        let data = pattern(4);
        // SAFETY: `data` is live for the whole block and `4` is its exact length; the
        // zero-length call is in range for any live pointer.
        unsafe {
            assert!(buffer_slice(data.as_ptr(), 0).is_empty());
            assert_eq!(buffer_slice(data.as_ptr(), 4), data.as_slice());
        }
    }

    // -----------------------------------------------------------------------
    // `Z_NULL` versus an empty buffer
    // -----------------------------------------------------------------------

    /// The fallback constants must be the values the C sources return.
    #[test]
    fn the_fallback_constants_match_the_reference() {
        assert_eq!(fallback::ADLER32_EMPTY, 1, "adler32.c L82 returns 1L");
        assert_eq!(fallback::CRC32_EMPTY, 0, "crc32.c L628 returns 0");
        assert_eq!(
            fallback::ADLER32_COMBINE_INVALID,
            0xffff_ffff,
            "adler32.c L140 returns 0xffffffffUL"
        );
        assert_eq!(
            fallback::CRC32_COMBINE_INVALID,
            0,
            "crc32.c L956 returns 0, and it must not be unified with the Adler-32 one"
        );
    }

    /// A null buffer yields the family seed regardless of the incoming value or `len`.
    #[test]
    fn a_null_buffer_yields_the_family_initial_value() {
        // SAFETY: a null `buf` is explicitly permitted by `zlib.h` L1813-L1814 and
        // L1851-L1853; nothing is read, which is exactly what is being asserted.
        unsafe {
            for seed in [0, 1, 5, 9, 0xffff_ffff] {
                for len in [0, 1, 7, 99] {
                    assert_eq!(adler32(seed, ptr::null(), len), 1);
                    assert_eq!(crc32(seed, ptr::null(), len), 0);
                }
                for len in [0, 40, 90] {
                    assert_eq!(adler32_z(seed, ptr::null(), len), 1);
                    assert_eq!(crc32_z(seed, ptr::null(), len), 0);
                }
            }
        }
    }

    /// `adler32(a, NULL, 1)` is undefined behaviour in C; here it is simply the seed.
    ///
    /// `adler32.c` L70 tests `len == 1` before L81 tests `buf == Z_NULL` -- the
    /// comment at L80 calls it a "deferred check for len == 1 speed" -- so the C entry
    /// point dereferences the null pointer. Reaching the null test first removes the
    /// hazard without changing any defined result.
    #[test]
    fn the_deferred_null_check_hazard_is_removed() {
        // SAFETY: nothing is dereferenced; the null test is reached first, which is
        // the whole point of the test.
        unsafe {
            assert_eq!(adler32(7, ptr::null(), 1), 1);
            assert_eq!(adler32_z(7, ptr::null(), 1), 1);
        }
    }

    /// A zero-length update over a real pointer is not `Z_NULL`.
    #[test]
    fn an_empty_buffer_is_an_ordinary_zero_length_update() {
        let data = pattern(8);
        let ptr = data.as_ptr();
        // SAFETY: `data` outlives the block, and a zero length reads nothing.
        unsafe {
            // Measured on the C library; the incoming value comes back, with the
            // Adler-32 halves normalised.
            assert_eq!(adler32(0, ptr, 0), 0);
            assert_eq!(adler32(1, ptr, 0), 1);
            assert_eq!(adler32(5, ptr, 0), 5);
            assert_eq!(adler32(0xffff_ffff, ptr, 0), 0x000e_000e);
            assert_eq!(crc32(0, ptr, 0), 0);
            assert_eq!(crc32(5, ptr, 0), 5);
            assert_eq!(crc32(0xffff_ffff, ptr, 0), 0xffff_ffff);
            // The zero-value rows are the only ones where the null answer and the
            // empty answer coincide -- which is why `adler32(0L, Z_NULL, 0)` and
            // `crc32(0L, Z_NULL, 0)` are usable idioms at all.
            assert_eq!(adler32(1, ptr, 0), adler32(1, ptr::null(), 0));
            assert_eq!(crc32(0, ptr, 0), crc32(0, ptr::null(), 0));
            assert_ne!(adler32(5, ptr, 0), adler32(5, ptr::null(), 0));
            assert_ne!(crc32(5, ptr, 0), crc32(5, ptr::null(), 0));
        }
    }

    // -----------------------------------------------------------------------
    // Buffer entry points against reference-measured values
    // -----------------------------------------------------------------------

    /// Every fixture length must reproduce the C library's answer exactly.
    #[test]
    fn the_buffer_entry_points_match_the_reference_at_every_boundary() {
        for &(len, expect_adler, expect_crc) in PATTERN_FIXTURES {
            let data = pattern(len);
            let ptr = data.as_ptr();
            // SAFETY: `data` holds exactly `len` live bytes and outlives the calls.
            unsafe {
                assert_eq!(
                    adler32(1, ptr, u32::try_from(len).unwrap()),
                    expect_adler,
                    "adler32 len={len}"
                );
                assert_eq!(
                    crc32(0, ptr, u32::try_from(len).unwrap()),
                    expect_crc,
                    "crc32 len={len}"
                );
                // The `_z` face must agree with the `uInt` face for every length.
                assert_eq!(adler32_z(1, ptr, len), expect_adler, "adler32_z len={len}");
                assert_eq!(crc32_z(0, ptr, len), expect_crc, "crc32_z len={len}");
            }
        }
    }

    /// The single-byte fast path (`adler32.c` L70-L78) against a measured value.
    #[test]
    fn a_single_byte_matches_the_reference() {
        let data = [0x5a_u8];
        let ptr = data.as_ptr();
        // SAFETY: one live byte, one byte read.
        unsafe {
            assert_eq!(adler32(1, ptr, 1), 0x005b_005b);
            assert_eq!(crc32(0, ptr, 1), 0x59bc_5767);
            assert_eq!(adler32_z(1, ptr, 1), 0x005b_005b);
            assert_eq!(crc32_z(0, ptr, 1), 0x59bc_5767);
        }
    }

    /// The payload `test/example.c` L35 uses, and the dictionary at L40.
    #[test]
    fn the_example_c_payload_matches_the_reference() {
        // SAFETY: both slices are `'static` and their lengths are exact.
        unsafe {
            assert_eq!(adler32(1, HELLO.as_ptr(), 13), HELLO_ADLER);
            assert_eq!(crc32(0, HELLO.as_ptr(), 13), HELLO_CRC);
            // With the trailing NUL, which is what `example.c` actually compresses.
            let with_nul = b"hello, hello!\0";
            assert_eq!(adler32(1, with_nul.as_ptr(), 14), 0x2606_0496);
            assert_eq!(crc32(0, with_nul.as_ptr(), 14), 0xb56c_3f9d);
            // `example.c` L40's preset dictionary, whose Adler-32 is the `dictId`
            // that `deflateSetDictionary` reports and `inflateSetDictionary` checks.
            let dictionary = b"hello";
            assert_eq!(adler32(1, dictionary.as_ptr(), 5), 0x062c_0215);
            assert_eq!(crc32(0, dictionary.as_ptr(), 5), 0x3610_a686);
        }
    }

    /// Accumulating in chunks must equal one call over the whole input.
    ///
    /// Run at one mebibyte so the CRC braid and several `NMAX` reductions are crossed,
    /// and with a chunk size coprime to every internal block size so no boundary is
    /// accidentally respected.
    #[test]
    fn chunked_accumulation_equals_the_one_shot_value() {
        const TOTAL: usize = 1 << 20;
        let data = pattern(TOTAL);
        let one_shot_adler: uLong = 0x3da8_7789;
        let one_shot_crc: uLong = 0xd424_bdc1;

        // SAFETY: `data` holds exactly `TOTAL` live bytes throughout.
        unsafe {
            assert_eq!(adler32_z(1, data.as_ptr(), TOTAL), one_shot_adler);
            assert_eq!(crc32_z(0, data.as_ptr(), TOTAL), one_shot_crc);
        }

        for chunk in [1_usize, 7, 16, 47, 5552] {
            let mut adler: uLong = 1;
            let mut crc: uLong = 0;
            for piece in data.chunks(chunk) {
                let len = u32::try_from(piece.len()).unwrap();
                // SAFETY: `piece` is a live sub-slice of `data` of exactly `len`
                // bytes, and `data` outlives the loop.
                unsafe {
                    adler = adler32(adler, piece.as_ptr(), len);
                    crc = crc32(crc, piece.as_ptr(), len);
                }
            }
            assert_eq!(adler, one_shot_adler, "adler32 chunk={chunk}");
            assert_eq!(crc, one_shot_crc, "crc32 chunk={chunk}");
        }
    }

    /// The high half of a `uLong` argument must be ignored, as the C bodies ignore it.
    ///
    /// ★ The three high halves are built with [`u32::checked_shl`] on a `uLong`, never
    /// written as `1 << 32`. `uLong` is `c_ulong`, which is **32 bits on LLP64 Windows
    /// and on every ILP32 target**, and a constant `1 << 32` on a 32-bit type is
    /// `deny(arithmetic_overflow)` -- a hard *build* failure. The early return above
    /// does not prevent it: the lint fires at codegen on the expression itself, not on
    /// whether control ever reaches it, so an i686 or LLP64 `cargo build --tests` failed
    /// before a single ABI test could run. `checked_shl` states the same fact without
    /// writing an expression the target cannot hold -- on a 32-bit `uLong` it answers
    /// `None` for every seed, which is precisely the "there is no high half" the early
    /// return reports.
    #[test]
    fn the_high_bits_of_a_ulong_argument_are_ignored() {
        /// Bit position the low half ends at: the boundary C's `(unsigned)` truncation cuts.
        const HIGH_HALF_SHIFT: u32 = 32;

        if fits_in_32_bits(!0) {
            // LLP64 Windows: `uLong` has no high half, so there is nothing to ignore.
            return;
        }
        let data = pattern(64);
        let ptr = data.as_ptr();

        let highs: Vec<uLong> = [1_u64, 0xffff_ffff, 0xdead_beef]
            .into_iter()
            .filter_map(|seed| uLong::try_from(seed).ok()?.checked_shl(HIGH_HALF_SHIFT))
            .collect();
        assert_eq!(
            highs.len(),
            3,
            "a uLong wider than 32 bits must admit all three high halves; the early \
             return above is what covers a 32-bit uLong"
        );

        for high in highs {
            for low in [0, 1, 5, 0xffff, 0x1234_abcd, 0xffff_ffff] {
                // SAFETY: 64 live bytes, 64 bytes read, `data` outlives the loop.
                unsafe {
                    assert_eq!(adler32(high | low, ptr, 64), adler32(low, ptr, 64));
                    assert_eq!(crc32(high | low, ptr, 64), crc32(low, ptr, 64));
                }
                assert_eq!(
                    adler32_combine(high | low, high | low, 100),
                    adler32_combine(low, low, 100)
                );
                assert_eq!(
                    crc32_combine(high | low, high | low, 100),
                    crc32_combine(low, low, 100)
                );
                let op = crc32_combine_gen(100);
                assert_eq!(
                    crc32_combine_op(high | low, high | low, high | op),
                    crc32_combine_op(low, low, op)
                );
            }
        }
    }

    /// No entry point may return a value that does not fit an Adler-32 or CRC-32.
    #[test]
    fn every_return_value_fits_in_thirty_two_bits() {
        let data = pattern(5553);
        let ptr = data.as_ptr();
        // SAFETY: 5553 live bytes, 5553 bytes read.
        let observed = unsafe {
            [
                adler32(0xffff_ffff, ptr, 5553),
                crc32(0xffff_ffff, ptr, 5553),
                adler32_z(0xffff_ffff, ptr, 5553),
                crc32_z(0xffff_ffff, ptr, 5553),
                adler32(0, ptr::null(), 0),
                crc32(0, ptr::null(), 0),
            ]
        };
        for value in observed {
            assert!(fits_in_32_bits(value), "{value:#x} exceeds 32 bits");
        }
        for &len2 in COMBINE_LENGTHS {
            let combines = [
                adler32_combine64(COMBINE_ADLER1, COMBINE_ADLER2, len2),
                crc32_combine64(COMBINE_CRC1, COMBINE_CRC2, len2),
                crc32_combine_gen64(len2),
                crc32_combine_op(COMBINE_CRC1, COMBINE_CRC2, crc32_combine_gen64(len2)),
            ];
            for value in combines {
                assert!(
                    fits_in_32_bits(value),
                    "{value:#x} exceeds 32 bits, len2={len2}"
                );
            }
        }
    }

    // -----------------------------------------------------------------------
    // Combine entry points, sentinels and large-file duality
    // -----------------------------------------------------------------------

    /// Every combine fixture must reproduce the C library's answer exactly.
    #[test]
    fn the_combine_entry_points_match_the_reference() {
        for &(len2, expect_adler, expect_crc, expect_gen) in COMBINE_FIXTURES {
            assert_eq!(
                adler32_combine64(COMBINE_ADLER1, COMBINE_ADLER2, len2),
                expect_adler,
                "adler32_combine64 len2={len2}"
            );
            assert_eq!(
                crc32_combine64(COMBINE_CRC1, COMBINE_CRC2, len2),
                expect_crc,
                "crc32_combine64 len2={len2}"
            );
            assert_eq!(
                crc32_combine_gen64(len2),
                expect_gen,
                "crc32_combine_gen64 len2={len2}"
            );
        }
    }

    /// The three documented sentinels, each asserted against its named constant.
    #[test]
    fn a_negative_length_produces_the_documented_sentinels() {
        for &len2 in COMBINE_LENGTHS.iter().filter(|n| **n < 0) {
            // `adler32.c` L138-L140 -- an unmistakable non-checksum, NOT zero.
            assert_eq!(
                adler32_combine64(COMBINE_ADLER1, COMBINE_ADLER2, len2),
                fallback::ADLER32_COMBINE_INVALID,
                "adler32_combine64 len2={len2}"
            );
            // `crc32.c` L955-L956, and `zlib.h` L1886-L1888.
            assert_eq!(
                crc32_combine_gen64(len2),
                fallback::CRC32_COMBINE_INVALID,
                "crc32_combine_gen64 len2={len2}"
            );
            // Reached through the composition, which is why it is 0 and not `crc1`.
            assert_eq!(
                crc32_combine64(COMBINE_CRC1, COMBINE_CRC2, len2),
                fallback::CRC32_COMBINE_INVALID,
                "crc32_combine64 len2={len2}"
            );
            assert_ne!(
                crc32_combine64(COMBINE_CRC1, COMBINE_CRC2, len2),
                COMBINE_CRC1
            );
        }
        // The two sentinels are different values and must never be unified.
        assert_ne!(
            fallback::ADLER32_COMBINE_INVALID,
            fallback::CRC32_COMBINE_INVALID
        );
    }

    /// `crc32_combine_op` answers zero for a zero operator, and nothing else.
    #[test]
    fn a_zero_operator_returns_zero() {
        let observed = crc32_combine_op(COMBINE_CRC1, COMBINE_CRC2, 0);
        assert_eq!(observed, fallback::CRC32_COMBINE_INVALID);
        // Each of these is a plausible-looking wrong answer; `crc32.c` L970-L971
        // returns none of them.
        assert_ne!(observed, COMBINE_CRC1);
        assert_ne!(observed, COMBINE_CRC2);
        assert_ne!(observed, COMBINE_CRC1 ^ COMBINE_CRC2);
    }

    /// A zero length is the identity, not an error.
    #[test]
    fn a_zero_length_is_the_identity_operator() {
        // `crc32_combine_gen(0)` is `x^0 == 1` in the reflected representation.
        assert_eq!(crc32_combine_gen64(0), 0x8000_0000);
        assert_eq!(
            crc32_combine64(COMBINE_CRC1, COMBINE_CRC2, 0),
            COMBINE_CRC1 ^ COMBINE_CRC2
        );
        // `1` is the Adler-32 of an empty sequence, so combining with it is a no-op.
        assert_eq!(adler32_combine64(COMBINE_ADLER1, 1, 0), COMBINE_ADLER1);
    }

    /// Both faces of every pair must agree for every length in the domain.
    ///
    /// Structural, because each pair delegates to one core function -- but the whole
    /// point of exporting two symbols is that a caller may reach either, so it is
    /// asserted rather than assumed.
    #[test]
    fn the_unsuffixed_and_64_bit_faces_agree() {
        for &len2 in COMBINE_LENGTHS {
            let Some(narrow) = as_off_t::<z_off_t>(len2) else {
                // This target's `z_off_t` cannot express the value; a C caller on it
                // would reach the `*64` face instead, which is covered above.
                continue;
            };
            assert_eq!(
                adler32_combine(COMBINE_ADLER1, COMBINE_ADLER2, narrow),
                adler32_combine64(COMBINE_ADLER1, COMBINE_ADLER2, len2),
                "adler32_combine pair, len2={len2}"
            );
            assert_eq!(
                crc32_combine(COMBINE_CRC1, COMBINE_CRC2, narrow),
                crc32_combine64(COMBINE_CRC1, COMBINE_CRC2, len2),
                "crc32_combine pair, len2={len2}"
            );
            assert_eq!(
                crc32_combine_gen(narrow),
                crc32_combine_gen64(len2),
                "crc32_combine_gen pair, len2={len2}"
            );
        }
    }

    /// Splitting the generator out of the combine must not change the answer.
    #[test]
    fn the_operator_path_matches_the_direct_combine() {
        for &len2 in COMBINE_LENGTHS {
            let op = crc32_combine_gen64(len2);
            assert_eq!(
                crc32_combine_op(COMBINE_CRC1, COMBINE_CRC2, op),
                crc32_combine64(COMBINE_CRC1, COMBINE_CRC2, len2),
                "operator path, len2={len2}"
            );
        }
    }

    /// Combining the halves of a real string must reproduce the whole string's value.
    ///
    /// The end-to-end property the combine family exists for, checked against
    /// independently measured checksums of `"hello, "`, `"hello!"` and
    /// `"hello, hello!"`.
    #[test]
    fn combining_two_halves_reproduces_the_whole_string() {
        // SAFETY: all three slices are `'static` and their lengths are exact.
        unsafe {
            assert_eq!(adler32(1, HELLO_HEAD.as_ptr(), 7), HEAD_ADLER);
            assert_eq!(adler32(1, HELLO_TAIL.as_ptr(), 6), TAIL_ADLER);
            assert_eq!(crc32(0, HELLO_HEAD.as_ptr(), 7), HEAD_CRC);
            assert_eq!(crc32(0, HELLO_TAIL.as_ptr(), 6), TAIL_CRC);
        }

        let tail_len: z_off_t = as_off_t(6).expect("z_off_t holds 6 on every target");
        assert_eq!(
            adler32_combine(HEAD_ADLER, TAIL_ADLER, tail_len),
            HELLO_ADLER
        );
        assert_eq!(crc32_combine(HEAD_CRC, TAIL_CRC, tail_len), HELLO_CRC);
        assert_eq!(adler32_combine64(HEAD_ADLER, TAIL_ADLER, 6), HELLO_ADLER);
        assert_eq!(crc32_combine64(HEAD_CRC, TAIL_CRC, 6), HELLO_CRC);
        assert_eq!(
            crc32_combine_op(HEAD_CRC, TAIL_CRC, crc32_combine_gen64(6)),
            HELLO_CRC
        );

        // Accumulating the two halves in sequence is the other route to the same
        // value, and it must not disagree with the combine route.
        // SAFETY: `HELLO_HEAD` and `HELLO_TAIL` are `'static` byte strings, so both pointers
        // are non-null, aligned for `u8` and readable for the whole of the program's life; 7
        // and 6 are their exact lengths. Nothing is written through either pointer.
        unsafe {
            let running = adler32(1, HELLO_HEAD.as_ptr(), 7);
            assert_eq!(adler32(running, HELLO_TAIL.as_ptr(), 6), HELLO_ADLER);
            let running = crc32(0, HELLO_HEAD.as_ptr(), 7);
            assert_eq!(crc32(running, HELLO_TAIL.as_ptr(), 6), HELLO_CRC);
        }
    }
}
