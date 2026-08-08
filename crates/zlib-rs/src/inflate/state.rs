//! The resumable decompression state: the Rust counterpart of `struct inflate_state`
//! (`inflate.h` L82-L126).
//!
//! The reference header describes the object this module reproduces in two lines
//! (`inflate.h` L80-L81):
//!
//! > State maintained between `inflate()` calls -- approximately 7K bytes, not
//! > including the allocated sliding window, which is up to 32K bytes.
//!
//! Both halves of that sentence are load-bearing and both are reproduced here:
//! the state is one flat allocation of roughly seven kilobytes with its three
//! scratch arrays inline, and the sliding window is a *separate*, lazily created
//! allocation of up to 32 KiB. See [the footprint section](#footprint) for why
//! the split is not a stylistic choice.
//!
//! # The field mapping
//!
//! Fields are declared in the header's order so that this file stays diffable
//! against `inflate.h`. Thirty-five C members become thirty-two Rust fields:
//! the four code-table pointer/width members collapse into one grouped type,
//! while the pointer-shaped members are replaced one for one by safe values.
//!
//! | # | `inflate.h` | C type | This module |
//! |---|---|---|---|
//! | 1 | `strm` (L83) | `z_streamp` | **replaced** by `InflateState::allocator` |
//! | 2 | `mode` (L84) | `inflate_mode` | [`Mode`] |
//! | 3 | `last` (L85) | `int` | `bool` |
//! | 4 | `wrap` (L86) | `int` | [`WrapFlags`] |
//! | 5 | `havedict` (L88) | `int` | `bool` |
//! | 6 | `flags` (L89) | `int` | `i32`, with the `-1` sentinel kept |
//! | 7 | `dmax` (L91) | `unsigned` | `u32` |
//! | 8 | `check` (L92) | `unsigned long` | `u32` |
//! | 9 | `total` (L93) | `unsigned long` | `u32` |
//! | 10 | `head` (L94) | `gz_headerp` | **replaced** by `Option<`[`GzHeaderSink`]`>` |
//! | 11 | `wbits` (L96) | `unsigned` | `u32` |
//! | 12 | `wsize` (L97) | `unsigned` | `u32` |
//! | 13 | `whave` (L98) | `unsigned` | `u32` |
//! | 14 | `wnext` (L99) | `unsigned` | `u32` |
//! | 15 | `window` (L100) | `unsigned char FAR *` | **replaced** by [`InflateWindow`] |
//! | 16 | `hold` (L102) | `unsigned long` | `u64` |
//! | 17 | `bits` (L103) | `unsigned` | `u32` |
//! | 18 | `length` (L105) | `unsigned` | `u32` |
//! | 19 | `offset` (L106) | `unsigned` | `u32` |
//! | 20 | `extra` (L108) | `unsigned` | `u32` |
//! | 21 | `lencode` (L110) | `code const FAR *` | [`CodeTables::lencode`] |
//! | 22 | `distcode` (L111) | `code const FAR *` | [`CodeTables::distcode`] |
//! | 23 | `lenbits` (L112) | `unsigned` | [`CodeTables::lenbits`] |
//! | 24 | `distbits` (L113) | `unsigned` | [`CodeTables::distbits`] |
//! | 25 | `ncode` (L115) | `unsigned` | `u32` |
//! | 26 | `nlen` (L116) | `unsigned` | `u32` |
//! | 27 | `ndist` (L117) | `unsigned` | `u32` |
//! | 28 | `have` (L118) | `unsigned` | `u32` |
//! | 29 | `next` (L119) | `code FAR *` | `usize`, an index into `codes` |
//! | 30 | `lens[320]` (L120) | `unsigned short` | `[u16; 320]`, inline |
//! | 31 | `work[288]` (L121) | `unsigned short` | `[u16; 288]`, inline |
//! | 32 | `codes[ENOUGH]` (L122) | `code` | `[Code; 1444]`, inline |
//! | 33 | `sane` (L123) | `int` | `bool` |
//! | 34 | `back` (L124) | `int` | `i32`, with the `-1` sentinel kept |
//! | 35 | `was` (L125) | `unsigned` | `u32` |
//!
//! ## Nothing here is ABI-visible
//!
//! This struct sits behind the opaque `z_stream.state` pointer, so no C caller
//! may look inside it and its layout is deliberately *not* part of the ABI. It
//! carries no `#[repr(C)]`, no `#[no_mangle]` and no `extern "C"`, and the field
//! types above were chosen for Rust rather than for byte-for-byte layout
//! agreement. The `#[repr(C)]` mirrors of the genuinely public types --
//! `z_stream`, `gz_header`, `struct gzFile_s` -- belong to the planned
//! `crates/libz-rs-sys/src/types.rs`.
//!
//! There is exactly one qualification to that, and it is worth stating loudly
//! because it is easy to miss. `test/infcover.c` includes `inflate.h` directly
//! (its L18) and at its L331 reaches through the opaque pointer:
//!
//! ```c
//! ((struct inflate_state *)strm.state)->mode = DICT;
//! ```
//!
//! `mode` sits at offset 8 of the C struct, immediately after `strm`, and is a
//! four-byte `int`. That write happens in *caller-compiled code* which this implementation
//! cannot change, so the facade -- not this module -- must present a synced
//! `{ z_streamp strm; inflate_mode mode; }` prefix at the head of whatever block
//! `z_stream.state` points at. [`InflateState::mode_tag`] and
//! [`InflateState::set_mode_tag`] are the two primitives it needs to do that
//! without reinterpreting memory: one reads the tag in the numbering C uses, the
//! other installs a tag and rejects a value that names no state.
//!
//! ## Why `strm` is not implemented
//!
//! The back-pointer at `inflate.h` L83 exists for two reasons, and neither
//! survives translation. It lets `inflateStateCheck` compare `state->strm ==
//! strm` (`inflate.c` L94), which is a raw-pointer identity test and therefore
//! belongs to `crates/libz-rs-sys`; and it lets the `ZALLOC`/`ZFREE` macros
//! (`zutil.h` L252-L254) reach `strm->zalloc`, `strm->zfree` and `strm->opaque`,
//! which here arrive by dependency injection instead. The slot is occupied by
//! `InflateState::allocator`, an injected [`Allocator`] -- AAP §0.3.3.6.
//!
//! The half of `inflateStateCheck` that *is* expressible in safe Rust is the
//! range test `state->mode < HEAD || state->mode > SYNC` (`inflate.c` L95), and
//! it lives here as [`is_live_mode_tag`].
//!
//! # Footprint
//!
//! `size_of::<InflateState>()` is a hard, externally enforced constraint rather
//! than a quality target, and the enforcing test is `test/infcover.c` L428:
//!
//! ```c
//! mem_limit(&strm, (sizeof(struct inflate_state) << 1) + 256);
//! ```
//!
//! That budget is computed from the *C* header, so it is fixed at
//! `2 * 7160 + 256` = 14576 bytes on this target no matter what this implementation does.
//! When the limit is installed, the stream already holds one state of `S` bytes
//! plus a 256-byte window (the test initialises with `windowBits` of `-8`). The
//! `inflateCopy` at L438 then asks for a second state and a second window, and
//! the test asserts the call returns `Z_MEM_ERROR`. Writing that out:
//!
//! * the copy's state is granted only while `2S + 256 <= 14576`, i.e. `S <= 7160`;
//! * the copy's window is refused only while `2S + 512 > 14576`, i.e. `S > 7032`.
//!
//! A state of 7032 bytes or fewer lets **both** allocations through, `inflateCopy`
//! returns `Z_OK`, and the assertion fails. So the constraint has a floor as well
//! as a ceiling: `7033 <= S`, and `S <= 8234` for the 15% per-stream memory bar of
//! AAP §0.8.4. (A state larger than 7160 still passes the assertion, because then
//! the *first* of the two requests is refused and the status is the same.)
//!
//! Three consequences follow, and none of them is negotiable:
//!
//! * `lens`, `work` and `codes` stay **inline**. They are 640 + 576 + 5776 = 6992
//!   of C's 7160 bytes. Moving any of them behind a `Vec` or a `Box` would drop
//!   the state under the floor *and* add allocator calls that `mem_limit` counts.
//! * The window is a **separate, lazily created** allocation, exactly as
//!   `inflate.c` L257-L262 creates it on first use inside `updatewindow`.
//! * No padding, no alignment attributes and no indirection are added for
//!   tidiness. `size_of_inflate_state_matches_the_c_footprint` pins the measured
//!   figure so that a future field change cannot drift past either bound
//!   unnoticed.
//!
//! # Freshly allocated memory is not zeroed
//!
//! The default allocator path is `malloc`, not `calloc`: `zcalloc` chooses
//! between them on `sizeof(uInt) > 2` (`zutil.c` L299-L303) and `uInt` is four
//! bytes on every target this implementation supports. A caller-supplied hook is under no
//! obligation to do better, and `test/infcover.c` L87 deliberately fills every
//! block it hands out with `0xa5` so that any code assuming zeros produces wrong
//! answers instead of passing by luck.
//!
//! The reference is written accordingly, and asymmetrically: `inflateInit2_`
//! zeroes the *state struct* with `zmemzero` (`inflate.c` L199) and `inflateCopy`
//! does the same (L1341), but the *window* is never zeroed. This implementation reproduces
//! both facts. Every field of a freshly built state has a definite value, because
//! Rust makes that the only option; and nothing ever reads a window byte on the
//! strength of the allocator having cleared it. What bounds valid window content
//! is `whave` (how many bytes are valid) together with `wnext` (where the next
//! write goes) -- nothing else.
//!
//! Safe Rust cannot hand out uninitialised memory, so a window allocated here
//! does arrive filled. That is strictly safer than C and unobservable, but it must
//! never become a correctness dependency: the HEAD commit `09a1572` fixed a real
//! defect in which an inflated `whave` let the decoder emit window bytes it should
//! have rejected, and the `whave`/`wnext` gating is what prevents that class of
//! bug. [`InflateWindow::byte_at`] is bounds-checked, but bounds are not the same
//! thing as validity: callers still owe the `whave` test.
//!
//! # Two windows, one set of accessors
//!
//! The two decompression entry points obtain their windows in opposite ways, and
//! this module has to serve both from one state type because
//! `crates/zlib-rs/src/infback.rs` reuses `InflateState` verbatim.
//!
//! | Entry point | Window |
//! |---|---|
//! | `inflate()` (`inflate.c` L257-L262) | **owned** -- `ZALLOC`ed lazily inside `updatewindow`, released by `inflateEnd` (L1159) |
//! | `inflateBack()` (`infback.c` L25-L63) | **borrowed** -- supplied by the caller, and `inflateBackEnd` frees only the state (L572-L578) |
//!
//! [`InflateWindow`] holds both shapes and exposes one set of accessors across
//! them, so `crates/zlib-rs/src/inflate/window.rs` and
//! `crates/zlib-rs/src/inflate/inffast.rs` never branch on which is in play. An
//! owned window is released to the allocator that produced it, by that block's own
//! destructor; a borrowed window is never released, because the memory is not
//! this library's to free. `test/infcover.c`'s `mem_done` (its L200-L234) reports
//! leaks, non-LIFO frees and frees of addresses it never handed out, so a mistake
//! in either direction is caught rather than tolerated.
//!
//! # Code tables are discriminated, not compared
//!
//! In C, `lencode` and `distcode` are bare `code *` pointers that may address
//! either the immutable fixed tables of `inffixed.h` or the stream's own
//! `codes[ENOUGH]` arena, and the only way to tell which is to test the pointer
//! against the arena's bounds -- which is what `inflateCopy` does so that it can
//! rebase them into the copy (`inflate.c` L1356-L1360).
//!
//! [`CodeTables`] names the two cases instead, and `next` becomes a plain index.
//! An offset is as valid in a copy as in the original, so
//! [`InflateState::try_clone_in`] performs **no rebasing at all** -- the payoff for
//! replacing a pointer comparison with a discriminant. It also makes
//! `inflateCodesUsed` (`inflate.c` L1410, `state->next - state->codes`) a direct
//! read of [`InflateState::codes_used`] with no subtraction.
//!
//! # The bit accumulator is 64 bits wide
//!
//! `hold` is `unsigned long` in C (`inflate.h` L102) and is filled by `PULLBYTE`
//! (`inflate.c` L356-L362):
//!
//! ```c
//! hold += (unsigned long)(*next++) << bits;  bits += 8;
//! ```
//!
//! `NEEDBITS(n)` pushes bytes while `bits < n`, and `n` reaches 32 -- at
//! `inflate.c` L576 for the gzip `TIME` field and at L695 for `DICTID`. Entering
//! that loop with `bits` at 31 and pushing four more bytes leaves `bits` at 39, so
//! the shift must not discard the incoming byte and a 32-bit accumulator would
//! lose data. `hold` is therefore a `u64`, which is also what `unsigned long`
//! actually is on the LP64 targets the reference is built for.
//!
//! # Visibility
//!
//! Nothing in this module is exported to C. Fields are `pub(crate)` because the
//! reference implementation adjusts them freely from `inflate.c`, `infback.c` and
//! `inffast.c`, no invariant of this type depends on any single one of them, and a
//! wall of one-line setters would obscure the correspondence with the C source
//! that behavioural fidelity depends on. The buffers are the exception: they are
//! reachable only through checked accessors, so no cursor value can cause an
//! out-of-bounds access.

// `InflateState`, `InflateWindow` and `InflateWrap`-adjacent names repeat the
// `inflate` module name. That is deliberate: they are re-exported at the crate
// root under exactly these names, where the module prefix is what disambiguates
// them from their deflate counterparts.
#![allow(clippy::module_name_repetitions)]

use core::cell::Cell;
use core::fmt;

use crate::allocate::{Allocator, Buffer};
use crate::config::{
    validate_inflate_back_window_bits, InflateConfig, InflateWrap, ValidatedInflateConfig,
    INFLATE_WRAP_GZIP_HEADER, INFLATE_WRAP_VERIFY_CHECK, INFLATE_WRAP_ZLIB_HEADER,
};
use crate::error::ReturnCode;
use crate::inflate::fixed_tables::{distfix, lenfix};
use crate::inflate::inftrees::{Code, CodeTableSource, CodeTables, ENOUGH};
use crate::inflate::mode::Mode;

/// Length of the code-length scratch array, from `unsigned short lens[320]`
/// (`inflate.h` L120).
///
/// It holds the literal/length and distance code lengths back to back while a
/// dynamic block header is being read -- at most `nlen + ndist` = 286 + 30 = 316
/// of them (`inflate.c` L782-L795) -- and the 19 code-length code lengths before
/// that (L802-L808). 320 covers both with room to spare, which is why the
/// reference sizes it that way rather than at 316.
pub const LENS_LEN: usize = 320;

/// Length of the table-building work area, from `unsigned short work[288]`
/// (`inflate.h` L121).
///
/// `inflate_table` sorts symbols by code length into
/// this array. 288 is the size of the largest alphabet it is asked to sort, the
/// 0..=287 literal/length alphabet of RFC 1951 §3.2.5.
pub const WORK_LEN: usize = 288;

/// The maximum match distance a freshly reset stream will accept, from
/// `state->dmax = 32768U` (`inflate.c` L113).
///
/// A zlib header narrows it to `1 << CINFO + 8` (`inflate.c` L545); a raw or gzip
/// stream leaves it at this value, which is the largest window RFC 1951 permits.
/// It is only consulted when `INFLATE_STRICT` is defined (`inflate.h` L91,
/// `inflate.c` L960-L965 and L999-L1004), which the shipped build does not define
/// -- the field is carried for fidelity and so that a strict build remains a
/// compile-time option rather than a rewrite.
pub const DMAX_DEFAULT: u32 = 32768;

/// The `flags` value meaning "raw stream, or no header seen yet", from
/// `state->flags = -1` (`inflate.c` L112).
///
/// `inflate.h` L89-L90 documents the tri-state: the gzip header's method and flag
/// bytes when a gzip header has been read, `0` once a zlib header has been read
/// (`inflate.c` L546), and this sentinel before either. `inflateSync` tests for it
/// exactly -- `if (state->flags == -1) state->wrap = 0;` (`inflate.c` L1299) --
/// which is why the field stays a signed integer rather than becoming an enum.
pub const FLAGS_NO_HEADER: i32 = -1;

/// The `back` value meaning "no length or literal code is pending", from
/// `state->back = -1` (`inflate.c` L120).
///
/// `back` counts the bits consumed by the length/literal code the decoder is in
/// the middle of, so that `inflateMark` can report how far into a block a stream
/// has got (`inflate.c` L1399-L1403). This sentinel is what `inflateMark` reports
/// as the high half of `-1 << 16` for a stream that has not started a code.
pub const BACK_UNKNOWN: i32 = -1;

/// The largest number of bits `inflatePrime` will insert in one call, from
/// `if (bits > 16 || ...)` (`inflate.c` L230).
pub const MAX_PRIME_BITS: i32 = 16;

/// The accumulator occupancy `inflatePrime` refuses to exceed, from
/// `state->bits + (uInt)bits > 32` (`inflate.c` L230).
///
/// Note that this is a limit on what `inflatePrime` may *add*, not on what the
/// accumulator can hold: `NEEDBITS(32)` legitimately drives `bits` to 39 while a
/// four-byte field is being assembled. See the module documentation.
pub const MAX_PRIME_HOLD_BITS: u32 = 32;

/// The three-bit `wrap` field: which container is accepted, and whether the
/// stream's check value is verified.
///
/// Mirrors `int wrap` (`inflate.h` L86-L87), whose comment defines the bits:
///
/// > bit 0 true for zlib, bit 1 true for gzip, bit 2 true to validate check value
///
/// This is a bit field and not an enumeration, which is why it is a newtype over
/// an integer rather than a Rust `enum`. Three things depend on that:
///
/// * `inflateReset2` computes it arithmetically -- `wrap = (windowBits >> 4) + 5`
///   (`inflate.c` L152) -- so the value 5, 6 or 7 falls out of the caller's
///   `windowBits` rather than being selected from a list.
/// * `inflateValidate` toggles bit 2 in isolation, `wrap |= 4` or `wrap &= ~4`
///   (`inflate.c` L1389-L1392), producing the live values 1, 2 and 3 that no
///   initialisation request can name.
/// * `inflateSync` zeroes the whole field when no header has been seen
///   (`inflate.c` L1299-L1302).
///
/// [`crate::config::InflateWrap`] is the *request* form -- the four values an
/// initialisation can ask for -- and [`WrapFlags::from_request`] is the bridge.
/// This type is the *live* form and accepts every combination the reference can
/// produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, PartialOrd, Ord)]
pub struct WrapFlags(i32);

impl WrapFlags {
    /// Bit 0: a zlib header and trailer are accepted (`inflate.h` L86).
    pub const ZLIB: i32 = INFLATE_WRAP_ZLIB_HEADER;

    /// Bit 1: a gzip header and trailer are accepted (`inflate.h` L86).
    pub const GZIP: i32 = INFLATE_WRAP_GZIP_HEADER;

    /// Bit 2: the stream's trailing check value is verified (`inflate.h` L87).
    pub const VERIFY_CHECK: i32 = INFLATE_WRAP_VERIFY_CHECK;

    /// Raw inflate: no header, no trailer, no check value.
    ///
    /// This is `wrap = 0`, which `inflateReset2` installs for a negative
    /// `windowBits` (`inflate.c` L149) and `inflateSync` installs when it gives up
    /// on finding a header (`inflate.c` L1300).
    pub const RAW: Self = Self(0);

    /// Wraps a raw `wrap` value.
    ///
    /// Total by design: every integer is accepted, because `inflateValidate` and
    /// `inflateSync` can leave combinations that no initialisation request names,
    /// and because bits outside the low three are simply never consulted.
    #[must_use]
    pub const fn from_raw(raw: i32) -> Self {
        Self(raw)
    }

    /// The integer the C field holds.
    #[must_use]
    pub const fn as_raw(self) -> i32 {
        self.0
    }

    /// The live flags an initialisation request installs.
    ///
    /// Reproduces `state->wrap = wrap` at `inflate.c` L168, where `wrap` came from
    /// the `(windowBits >> 4) + 5` computation at L152 or from the `wrap = 0` of
    /// L149. The `+ 5` bias is why every wrapped request starts out verifying its
    /// check value.
    #[must_use]
    pub const fn from_request(request: InflateWrap) -> Self {
        Self(request.as_inflate_wrap())
    }

    /// Whether a zlib header may be read: `state->wrap & 1` (`inflate.c` L524).
    #[must_use]
    pub const fn allows_zlib_header(self) -> bool {
        self.0 & Self::ZLIB != 0
    }

    /// Whether a gzip header may be read: `state->wrap & 2` (`inflate.c` L513).
    ///
    /// This is also the test `inflateGetHeader` applies before agreeing to fill a
    /// caller's `gz_header`: `if ((state->wrap & 2) == 0) return Z_STREAM_ERROR;`
    /// (`inflate.c` L1225).
    #[must_use]
    pub const fn allows_gzip_header(self) -> bool {
        self.0 & Self::GZIP != 0
    }

    /// Whether the check value is verified: `state->wrap & 4` (`inflate.c` L1074).
    ///
    /// Consulted before comparing the trailer (L1075, L1100), before folding
    /// header bytes into the header CRC (L568, L578, L588), and before updating
    /// `strm->adler` on the way out (L1150).
    #[must_use]
    pub const fn verifies_check_value(self) -> bool {
        self.0 & Self::VERIFY_CHECK != 0
    }

    /// Whether this is raw inflate: `state->wrap == 0` (`inflate.c` L507).
    ///
    /// The `HEAD` state uses it to skip header processing entirely, and
    /// `inflateResetKeep` uses it to decide whether to touch `strm->adler` at all
    /// (`inflate.c` L107-L108).
    #[must_use]
    pub const fn is_raw(self) -> bool {
        self.0 == 0
    }

    /// The value `inflateResetKeep` stores into `strm->adler`, which is
    /// `state->wrap & 1` (`inflate.c` L108).
    ///
    /// The reference comments the assignment "to support ill-conceived Java test
    /// suite", so the behaviour is preserved for its own sake: a zlib or
    /// auto-detecting stream reports an initial Adler-32 of 1, and a gzip stream
    /// reports 0. It is applied only when the field is non-zero, which
    /// [`InflateState::reset_keep`] encodes by returning
    /// [`StreamReset::adler`] as [`None`] for a raw stream.
    /// Written as a branch on bit 0 rather than as a cast of `self.0 & 1`: the
    /// result is then exactly 0 or 1 for every possible field value, with no
    /// signed-to-unsigned conversion to reason about.
    #[must_use]
    pub const fn adler_seed(self) -> u32 {
        if self.allows_zlib_header() {
            1
        } else {
            0
        }
    }

    /// Starts verifying the check value: `state->wrap |= 4` (`inflate.c` L1390).
    ///
    /// The reference guards this with `if (check && state->wrap)`, so a raw stream
    /// is never promoted; [`InflateState::set_validate`] applies that guard.
    ///
    /// Not a `const fn`: mutating through a `&mut` in a `const fn` needs Rust
    /// 1.83, and the crate's MSRV is 1.80.
    pub fn enable_check_validation(&mut self) {
        self.0 |= Self::VERIFY_CHECK;
    }

    /// Stops verifying the check value: `state->wrap &= ~4` (`inflate.c` L1392).
    ///
    /// Reached from `inflateValidate(strm, 0)` and from `inflateSync` on a stream
    /// that has already seen a header, where the reference notes there is "no point
    /// in computing a check value now" (`inflate.c` L1301-L1302).
    pub fn disable_check_validation(&mut self) {
        self.0 &= !Self::VERIFY_CHECK;
    }

    /// Drops every flag: `state->wrap = 0` (`inflate.c` L1300).
    ///
    /// `inflateSync` does this when `flags` is still [`FLAGS_NO_HEADER`], because
    /// the remainder of the stream has to be treated as raw deflate data.
    pub fn clear(&mut self) {
        self.0 = 0;
    }
}

impl From<InflateWrap> for WrapFlags {
    /// [`WrapFlags::from_request`] in operator form.
    fn from(request: InflateWrap) -> Self {
        Self::from_request(request)
    }
}

/// The `done` value meaning "no gzip header has been read yet", from
/// `head->done = 0` (`inflate.c` L1229).
///
/// `inflateGetHeader` stores it when a header is installed.
pub const GZ_HEADER_PENDING: i32 = 0;

/// The `done` value meaning "this stream carries no gzip header", from
/// `state->head->done = -1` (`inflate.c` L525).
///
/// `zlib.h` L1080-L1083 documents the tri-state for callers: `done` is set to
/// "-1 if there is no gzip header" and to 1 once the header has been completely
/// read. It is the reason this field is an `i32` and not a `bool`.
pub const GZ_HEADER_ABSENT: i32 = -1;

/// The `done` value meaning "the gzip header has been read in full", from
/// `state->head->done = 1` (`inflate.c` L679).
pub const GZ_HEADER_COMPLETE: i32 = 1;

/// Where the gzip header fields go while `inflate()` parses them: the safe
/// replacement for `gz_headerp head` (`inflate.h` L94).
///
/// The C field is a raw pointer to a caller-owned `gz_header` (`zlib.h`
/// L118-L133), installed by `inflateGetHeader` (`inflate.c` L1218-L1230) and
/// written through by the nine gzip header states. This crate never dereferences a
/// pointer, so the facade performs that one dereference at the `inflateGetHeader`
/// boundary, converts the caller's structure into this bounds-checked view, and
/// keeps the raw `gz_headerp` on its own side.
///
/// The scalar members are plain fields, written directly by
/// `crates/zlib-rs/src/inflate/header.rs`; the three variable-length members are
/// private and reached through the bounds-clamped writers below, so no write can
/// run past the space the caller advertised.
///
/// # ★ The three buffers are `&[Cell<u8>]`, not `&mut [u8]`
///
/// This is the one design decision here that is not obvious, and it is forced by
/// the test suite rather than chosen. `test/infcover.c` L307-L312 sets up a header
/// in which `extra`, `name` and `comment` are all given the same `out` buffer
/// and the same `len`.
///
/// All three fields therefore point at the *same* buffer. Holding them as three
/// `&mut [u8]` would therefore require the facade to produce three aliasing
/// mutable borrows of one allocation, which is undefined behaviour and would be
/// reported by Miri and by AddressSanitizer -- for a test that must pass
/// unmodified (AAP Goal 2).
///
/// A shared reference to a slice of [`Cell`] resolves it exactly. `Cell<u8>` is
/// `repr(transparent)` over `u8`, so the representation is unchanged; overlapping
/// *shared* references are perfectly legal; and interior mutability makes writing
/// through them safe. The facade's `slice::from_raw_parts` over `Cell<u8>` is
/// sound even when two of the ranges coincide, which is precisely what the C
/// contract permits and what the coverage harness does.
///
/// It has a second benefit. Shared references are [`Copy`], so this whole view is
/// [`Copy`], and `inflateCopy` can duplicate it as faithfully as C's
/// `zmemcpy(copy, state, sizeof(struct inflate_state))` duplicates the pointer
/// (`inflate.c` L1355): both streams then write into the caller's one header,
/// which is exactly the reference behaviour. No divergence, and nothing to
/// document as unsupported.
///
/// # Absent fields
///
/// When a gzip header omits a field, the reference does not merely skip it -- it
/// clears the caller's pointer: `state->head->extra = Z_NULL` (`inflate.c` L601),
/// and likewise for `name` (L631) and `comment` (L666). That is observable, so it
/// is reproduced: [`GzHeaderSink::clear_extra`] and friends drop the borrow and
/// record the fact, and [`GzHeaderSink::extra_is_absent`] and friends let the
/// facade store `Z_NULL` back into the caller's structure.
// Five booleans: `text` and `hcrc` mirror C's `int` flags, and the three
// `*_is_absent` flags record the three `Z_NULL` assignments above. Grouping them
// into a bit set would obscure the one-to-one correspondence with `zlib.h`
// L118-L133 that this view exists to preserve.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy)]
pub struct GzHeaderSink<'a> {
    /// `int text` (`zlib.h` L119): true if the compressed data is believed to be
    /// text. Written from bit 0 of the gzip `FLG` byte at `inflate.c` L566.
    pub text: bool,

    /// `uLong time` (`zlib.h` L120): the modification time, as the four
    /// little-endian bytes of the gzip `MTIME` field.
    ///
    /// Written from the whole accumulator at `inflate.c` L577, where `bits` is
    /// exactly 32, so the value never exceeds 32 bits and `u32` loses nothing.
    pub time: u32,

    /// `int xflags` (`zlib.h` L121): the gzip `XFL` byte, written masked to eight
    /// bits at `inflate.c` L586.
    pub xflags: i32,

    /// `int os` (`zlib.h` L122): the gzip `OS` byte, written as `hold >> 8` at
    /// `inflate.c` L587.
    pub os: i32,

    /// `Bytef *extra` with `uInt extra_max` (`zlib.h` L123, L125), or [`None`] for
    /// `Z_NULL`.
    ///
    /// The slice length **is** `extra_max`: pairing the pointer with its bound in
    /// one value is what makes the clamped write in
    /// [`GzHeaderSink::write_extra`] unable to overrun.
    extra: Option<&'a [Cell<u8>]>,

    /// `uInt extra_len` (`zlib.h` L124): the length the stream advertises for the
    /// extra field, written whole at `inflate.c` L596 *before* any clamping, so a
    /// caller can tell that its buffer was too small.
    pub extra_len: u32,

    /// Set once `extra` has been cleared to `Z_NULL` (`inflate.c` L601).
    extra_absent: bool,

    /// `Bytef *name` with `uInt name_max` (`zlib.h` L126-L127), or [`None`] for
    /// `Z_NULL`. The slice length **is** `name_max`.
    name: Option<&'a [Cell<u8>]>,

    /// Set once `name` has been cleared to `Z_NULL` (`inflate.c` L631).
    name_absent: bool,

    /// `Bytef *comment` with `uInt comm_max` (`zlib.h` L128-L129), or [`None`] for
    /// `Z_NULL`. The slice length **is** `comm_max`.
    comment: Option<&'a [Cell<u8>]>,

    /// Set once `comment` has been cleared to `Z_NULL` (`inflate.c` L666).
    comment_absent: bool,

    /// `int hcrc` (`zlib.h` L130): true if the header carried a CRC. Written from
    /// bit 9 of `flags` at `inflate.c` L678.
    pub hcrc: bool,

    /// `int done` (`zlib.h` L131-L132): [`GZ_HEADER_PENDING`],
    /// [`GZ_HEADER_ABSENT`] or [`GZ_HEADER_COMPLETE`].
    ///
    /// Tri-state, hence signed. See the three constants for the sites that write
    /// each value.
    pub done: i32,
}

impl<'a> GzHeaderSink<'a> {
    /// Builds the view `inflateGetHeader` installs (`inflate.c` L1218-L1230).
    ///
    /// Each argument is the caller's pointer paired with its advertised capacity:
    /// `extra` with `extra_max`, `name` with `name_max`, `comment` with
    /// `comm_max`. Pass [`None`] where the caller passed `Z_NULL`.
    ///
    /// # KNOWN COMPATIBILITY GAP -- scalar initialisation differs from C
    ///
    /// `inflateGetHeader` writes **only** `head->done = 0` (`inflate.c` L1229) and leaves every
    /// other field of the caller's structure exactly as the caller left it. This constructor
    /// instead starts every scalar at a neutral value.
    ///
    /// That difference is **observable**, and it must not be described as an improvement, as
    /// "strictly better defined", or as unobservable. A caller that pre-fills `head.text`,
    /// `head.time`, `head.xflags` or `head.os` and then decodes a stream that turns out not to be
    /// gzip sees its own values preserved under C and neutral values here. It is true that such a
    /// caller is reading fields the format never supplied -- `done` is the flag that says whether
    /// they mean anything -- but "the caller should not look" is not the same as "the caller cannot
    /// tell", and behaviour preservation is the governing constraint for this port.
    ///
    /// The gap is therefore recorded as unresolved rather than justified, and it is closable at the
    /// boundary: the facade's `inflateGetHeader` must write only `done = 0` into the caller's
    /// `gz_header`, and on completion write back only the fields the parse actually supplied,
    /// leaving the others at the caller's values. Whoever lands that entry point owns closing it.
    #[must_use]
    pub const fn new(
        extra: Option<&'a [Cell<u8>]>,
        name: Option<&'a [Cell<u8>]>,
        comment: Option<&'a [Cell<u8>]>,
    ) -> Self {
        Self {
            text: false,
            time: 0,
            xflags: 0,
            os: 0,
            extra,
            extra_len: 0,
            extra_absent: false,
            name,
            name_absent: false,
            comment,
            comment_absent: false,
            hcrc: false,
            done: GZ_HEADER_PENDING,
        }
    }

    /// `uInt extra_max` (`zlib.h` L125): the space available at `extra`, or zero
    /// when there is none.
    ///
    /// Saturates rather than wrapping. The caller's `extra_max` is a `uInt`, so a
    /// slice longer than [`u32::MAX`] cannot correspond to any real request; the
    /// saturation keeps the comparison in [`GzHeaderSink::write_extra`] total
    /// without an assertion.
    #[must_use]
    pub fn extra_max(&self) -> u32 {
        Self::capacity(self.extra)
    }

    /// `uInt name_max` (`zlib.h` L127): the space available at `name`, or zero.
    #[must_use]
    pub fn name_max(&self) -> u32 {
        Self::capacity(self.name)
    }

    /// `uInt comm_max` (`zlib.h` L129): the space available at `comment`, or zero.
    #[must_use]
    pub fn comm_max(&self) -> u32 {
        Self::capacity(self.comment)
    }

    /// The advertised capacity of one optional field.
    fn capacity(field: Option<&'a [Cell<u8>]>) -> u32 {
        field.map_or(0, |slice| u32::try_from(slice.len()).unwrap_or(u32::MAX))
    }

    /// Whether `extra` is a non-null pointer: `state->head->extra != Z_NULL`
    /// (`inflate.c` L608).
    #[must_use]
    pub const fn has_extra(&self) -> bool {
        self.extra.is_some()
    }

    /// Whether `name` is a non-null pointer: `state->head->name != Z_NULL`
    /// (`inflate.c` L625).
    #[must_use]
    pub const fn has_name(&self) -> bool {
        self.name.is_some()
    }

    /// Whether `comment` is a non-null pointer: `state->head->comment != Z_NULL`
    /// (`inflate.c` L660).
    #[must_use]
    pub const fn has_comment(&self) -> bool {
        self.comment.is_some()
    }

    /// Copies part of the extra field into the caller's buffer, clamped to
    /// `extra_max`.
    ///
    /// Reproduces `inflate.c` L607-L613 exactly.
    ///
    /// `at` is C's `len`, the offset already reached in the extra field, which the
    /// caller computes as `extra_len - length`. Nothing is copied when there is no
    /// buffer or when `at` has reached `extra_max`; otherwise as much of `bytes` as
    /// fits from `at` onwards is copied. The number of bytes stored is returned,
    /// which C discards -- the caller advances its own cursors by the full `copy`
    /// either way, because the CRC and the input cursor must advance over bytes the
    /// buffer had no room for.
    pub fn write_extra(&mut self, at: u32, bytes: &[u8]) -> usize {
        let Some(field) = self.extra else {
            return 0;
        };
        // `(len = extra_len - length) < extra_max`. `at` is already the difference,
        // and `field.len()` is `extra_max`, so this is the same test written on the
        // slice; `get(start..)` yields `None` rather than panicking when `at` is at
        // or past the end, which also covers `at` exceeding `usize` on a 16-bit
        // target.
        let start = usize::try_from(at).unwrap_or(usize::MAX);
        let Some(tail) = field.get(start..) else {
            return 0;
        };
        // `len + copy > extra_max ? extra_max - len : copy` -- take the shorter of
        // what is offered and what is left.
        let count = tail.len().min(bytes.len());
        let Some(destination) = tail.get(..count) else {
            return 0;
        };
        let Some(source) = bytes.get(..count) else {
            return 0;
        };
        for (slot, byte) in destination.iter().zip(source) {
            slot.set(*byte);
        }
        count
    }

    /// Stores one byte of the file name, reporting whether it was stored.
    ///
    /// Reproduces `inflate.c` L624-L627 exactly.
    ///
    /// In the reference the `state->length++` is *inside* the condition, so the write cursor
    /// advances only when a byte is actually stored -- and does not advance at all
    /// when no header is installed. Returning the outcome is what lets the caller
    /// reproduce that: advance `length` if and only if this returns `true`.
    ///
    /// The terminating zero is stored like any other byte, exactly as the loop at
    /// `inflate.c` L622-L628 does; `while (len && copy < have)` exits *after*
    /// emitting it.
    pub fn push_name(&mut self, at: u32, byte: u8) -> bool {
        Self::push(self.name, at, byte)
    }

    /// Stores one byte of the comment, reporting whether it was stored.
    ///
    /// The [`GzHeaderSink::push_name`] counterpart, from `inflate.c` L659-L662.
    pub fn push_comment(&mut self, at: u32, byte: u8) -> bool {
        Self::push(self.comment, at, byte)
    }

    /// The shared body of the two byte-at-a-time writers.
    fn push(field: Option<&'a [Cell<u8>]>, at: u32, byte: u8) -> bool {
        let Some(field) = field else {
            return false;
        };
        let Ok(index) = usize::try_from(at) else {
            return false;
        };
        // `state->length < state->head->name_max`, expressed as a checked lookup so
        // that the bound and the access cannot disagree.
        let Some(slot) = field.get(index) else {
            return false;
        };
        slot.set(byte);
        true
    }

    /// Clears `extra` to `Z_NULL`: `state->head->extra = Z_NULL`
    /// (`inflate.c` L601).
    ///
    /// Reached when the gzip `FLG` byte has no `FEXTRA` bit, so the stream carries
    /// no extra field at all. The borrow is dropped, so no later
    /// [`GzHeaderSink::write_extra`] can store anything, and
    /// [`GzHeaderSink::extra_is_absent`] then tells the facade to write `Z_NULL`
    /// back into the caller's structure.
    pub fn clear_extra(&mut self) {
        self.extra = None;
        self.extra_absent = true;
    }

    /// Clears `name` to `Z_NULL`: `state->head->name = Z_NULL`
    /// (`inflate.c` L631).
    pub fn clear_name(&mut self) {
        self.name = None;
        self.name_absent = true;
    }

    /// Clears `comment` to `Z_NULL`: `state->head->comment = Z_NULL`
    /// (`inflate.c` L666).
    pub fn clear_comment(&mut self) {
        self.comment = None;
        self.comment_absent = true;
    }

    /// Whether the implementation assigned `Z_NULL` to the caller's `extra` pointer.
    ///
    /// The facade uses this when it copies the view's results back: a `true` here
    /// means the stream had no extra field and the caller's pointer must be
    /// nulled, not merely left alone.
    #[must_use]
    pub const fn extra_is_absent(&self) -> bool {
        self.extra_absent
    }

    /// Whether the implementation assigned `Z_NULL` to the caller's `name` pointer.
    #[must_use]
    pub const fn name_is_absent(&self) -> bool {
        self.name_absent
    }

    /// Whether the implementation assigned `Z_NULL` to the caller's `comment` pointer.
    #[must_use]
    pub const fn comment_is_absent(&self) -> bool {
        self.comment_absent
    }

    /// Records that this stream carries no gzip header:
    /// `state->head->done = -1` (`inflate.c` L525).
    ///
    /// Reached in the `HEAD` state once the first two bytes turn out not to be the
    /// gzip magic, which is how a caller distinguishes "not a gzip stream" from
    /// "header not read yet".
    pub fn mark_absent(&mut self) {
        self.done = GZ_HEADER_ABSENT;
    }

    /// Records that the gzip header has been read in full:
    /// `state->head->done = 1` (`inflate.c` L679).
    ///
    /// Reached at the end of the `HCRC` state, after every optional field and the
    /// optional header CRC have been consumed.
    pub fn mark_complete(&mut self) {
        self.done = GZ_HEADER_COMPLETE;
    }

    /// Whether the header has been read in full, i.e. `done == 1`.
    #[must_use]
    pub const fn is_complete(&self) -> bool {
        self.done == GZ_HEADER_COMPLETE
    }
}

impl fmt::Debug for GzHeaderSink<'_> {
    /// Reports the scalars and the *capacities* of the three variable-length
    /// fields, never their contents.
    ///
    /// Written out rather than derived for two reasons: the contents are caller
    /// data, quite possibly a file name from somebody's archive, and a derived
    /// implementation would print up to three whole buffers.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GzHeaderSink")
            .field("text", &self.text)
            .field("time", &self.time)
            .field("xflags", &self.xflags)
            .field("os", &self.os)
            .field("extra_len", &self.extra_len)
            .field("extra_max", &self.extra_max())
            .field("extra_absent", &self.extra_absent)
            .field("name_max", &self.name_max())
            .field("name_absent", &self.name_absent)
            .field("comm_max", &self.comm_max())
            .field("comment_absent", &self.comment_absent)
            .field("hcrc", &self.hcrc)
            .field("done", &self.done)
            .finish()
    }
}

/// An allocator-owned window block that returns itself to its originating
/// allocator.
///
/// `inflate()` obtains this block lazily with
/// `ZALLOC(strm, 1U << state->wbits, sizeof(unsigned char))`
/// (`inflate.c` L257-L263) and `inflateEnd` releases it before the state object
/// (`inflate.c` L1159-L1161). Keeping the allocator beside the block makes that
/// pairing structural: even if the state is dropped along an error path, the
/// block still goes back through the same allocator.
struct WindowBlock<'a, A: Allocator<'a>> {
    /// [`None`] only while the destructor is returning the allocation.
    buffer: Option<Buffer<'a, u8>>,
    /// The allocator that produced `buffer`.
    allocator: A,
}

impl<'a, A: Allocator<'a>> WindowBlock<'a, A> {
    /// Takes ownership of `buffer`, which must have come from `allocator`.
    const fn new(allocator: A, buffer: Buffer<'a, u8>) -> Self {
        Self {
            buffer: Some(buffer),
            allocator,
        }
    }

    /// Number of bytes in the allocation.
    fn len(&self) -> usize {
        self.buffer.as_ref().map_or(0, Buffer::len)
    }

    /// Borrows the allocation, or an empty slice while it is being released.
    fn as_slice(&self) -> &[u8] {
        self.buffer.as_ref().map_or(&[], Buffer::as_slice)
    }

    /// Mutably borrows the allocation, or an empty slice while it is being
    /// released.
    fn as_mut_slice(&mut self) -> &mut [u8] {
        match &mut self.buffer {
            Some(buffer) => buffer.as_mut_slice(),
            None => &mut [],
        }
    }
}

impl<'a, A: Allocator<'a>> Drop for WindowBlock<'a, A> {
    /// Performs the `ZFREE(strm, state->window)` half of `inflateEnd`
    /// (`inflate.c` L1159-L1160).
    fn drop(&mut self) {
        self.allocator.try_deallocate_bytes(self.buffer.take());
    }
}

impl<'a, A: Allocator<'a>> fmt::Debug for WindowBlock<'a, A> {
    /// Reports shape only, never window contents or the allocator.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("WindowBlock")
            .field("len", &self.len())
            .finish_non_exhaustive()
    }
}

/// The storage behind [`InflateWindow`].
///
/// The variants are deliberately private so all callers use the same checked
/// accessors regardless of ownership.
enum WindowStorage<'a, A: Allocator<'a>> {
    /// `state->window == Z_NULL`: normal immediately after initialisation.
    Absent,
    /// A window allocated lazily by `inflate()` and owned by the state.
    Owned(WindowBlock<'a, A>),
    /// The caller-supplied output window used by `inflateBack()`.
    Borrowed(&'a mut [u8]),
}

/// The sliding window shared by the ordinary and callback-based inflate paths.
///
/// `inflate()` owns a lazily allocated block (`inflate.c` L257-L263), whereas
/// `inflateBack()` borrows the caller's `2**windowBits` byte buffer
/// (`infback.c` L22-L63). Both variants expose the same accessors, so the
/// decoder, `inflate/window.rs`, and `inflate/inffast.rs` never branch on
/// ownership. Dropping an owned variant releases its allocation; dropping a
/// borrowed variant only ends the borrow.
pub struct InflateWindow<'a, A: Allocator<'a>> {
    /// The ownership-discriminated storage.
    storage: WindowStorage<'a, A>,
}

impl<'a, A: Allocator<'a>> InflateWindow<'a, A> {
    /// An absent window, the `Z_NULL` value installed by `inflateInit2_`
    /// (`inflate.c` L204).
    #[must_use]
    pub const fn absent() -> Self {
        Self {
            storage: WindowStorage::Absent,
        }
    }

    /// Wraps the caller-owned window used by `inflateBackInit_`
    /// (`infback.c` L55-L59).
    ///
    /// The caller retains ownership. This value never passes the slice to
    /// [`Allocator::deallocate_bytes`].
    #[must_use]
    pub fn borrowed(window: &'a mut [u8]) -> Self {
        Self {
            storage: WindowStorage::Borrowed(window),
        }
    }

    /// Builds an owned window around an allocator block.
    const fn owned(allocator: A, buffer: Buffer<'a, u8>) -> Self {
        Self {
            storage: WindowStorage::Owned(WindowBlock::new(allocator, buffer)),
        }
    }

    /// Whether this is C's null window pointer.
    #[must_use]
    pub const fn is_absent(&self) -> bool {
        matches!(&self.storage, WindowStorage::Absent)
    }

    /// Whether the state owns the window allocation.
    #[must_use]
    pub const fn is_owned(&self) -> bool {
        matches!(&self.storage, WindowStorage::Owned(_))
    }

    /// Whether the window belongs to an `inflateBack()` caller.
    #[must_use]
    pub const fn is_borrowed(&self) -> bool {
        matches!(&self.storage, WindowStorage::Borrowed(_))
    }

    /// Number of addressable bytes, or zero while absent.
    #[must_use]
    pub fn len(&self) -> usize {
        match &self.storage {
            WindowStorage::Absent => 0,
            WindowStorage::Owned(block) => block.len(),
            WindowStorage::Borrowed(window) => window.len(),
        }
    }

    /// Whether no bytes are addressable.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Borrows the window in either ownership mode.
    ///
    /// [`None`] is the safe equivalent of `window == Z_NULL`.
    #[must_use]
    pub fn as_slice(&self) -> Option<&[u8]> {
        match &self.storage {
            WindowStorage::Absent => None,
            WindowStorage::Owned(block) => Some(block.as_slice()),
            WindowStorage::Borrowed(window) => Some(&**window),
        }
    }

    /// Mutably borrows the window in either ownership mode.
    ///
    /// [`None`] is the safe equivalent of `window == Z_NULL`.
    #[must_use]
    pub fn as_mut_slice(&mut self) -> Option<&mut [u8]> {
        match &mut self.storage {
            WindowStorage::Absent => None,
            WindowStorage::Owned(block) => Some(block.as_mut_slice()),
            WindowStorage::Borrowed(window) => Some(&mut **window),
        }
    }

    /// Reads one allocated byte, or returns [`None`] when absent or out of
    /// bounds.
    ///
    /// Allocation bounds are not validity bounds: callers must still establish
    /// from `whave` and `wnext` that the byte has been written. This accessor
    /// deliberately does not turn the allocator's fill pattern into data.
    #[must_use]
    pub fn byte_at(&self, index: usize) -> Option<u8> {
        self.as_slice()?.get(index).copied()
    }

    /// Writes one allocated byte, returning whether the index existed.
    pub fn set_byte(&mut self, index: usize, value: u8) -> bool {
        let Some(window) = self.as_mut_slice() else {
            return false;
        };
        let Some(slot) = window.get_mut(index) else {
            return false;
        };
        *slot = value;
        true
    }

    /// Releases an owned window, or merely ends a borrowed window's borrow.
    ///
    /// This is the operation `inflateReset2` needs when `wbits` changes
    /// (`inflate.c` L162-L165).
    pub fn discard(&mut self) {
        self.storage = WindowStorage::Absent;
    }

    /// Allocates a window when it is absent.
    ///
    /// Existing owned and borrowed windows are retained exactly as C retains a
    /// non-null `state->window`.
    fn ensure_owned(&mut self, len: usize, allocator: A) -> Result<(), ReturnCode> {
        if self.is_absent() {
            let buffer = allocator.allocate_bytes_or_mem_error(len, 1)?;
            self.storage = WindowStorage::Owned(WindowBlock::new(allocator, buffer));
        }
        Ok(())
    }

    /// Clones the window into allocator-owned storage, copying only `valid`
    /// prefix bytes.
    ///
    /// `inflateCopy` allocates `1 << wbits` bytes but copies only `whave` bytes
    /// (`inflate.c` L1344-L1346 and L1363). The unwritten suffix therefore keeps
    /// the destination allocator's fill pattern.
    fn try_clone_in<'b, B>(
        &self,
        allocator: B,
        valid: usize,
    ) -> Result<InflateWindow<'b, B>, ReturnCode>
    where
        B: Allocator<'b> + Copy,
    {
        let Some(source) = self.as_slice() else {
            return Ok(InflateWindow::absent());
        };
        let source = source.get(..valid).ok_or(ReturnCode::STREAM_ERROR)?;
        let buffer = allocator.allocate_bytes_or_mem_error(self.len(), 1)?;
        let mut copy = InflateWindow::owned(allocator, buffer);
        let destination = copy
            .as_mut_slice()
            .and_then(|window| window.get_mut(..valid))
            .ok_or(ReturnCode::STREAM_ERROR)?;
        destination.copy_from_slice(source);
        Ok(copy)
    }
}

impl<'a, A: Allocator<'a>> fmt::Debug for InflateWindow<'a, A> {
    /// Reports ownership and length, never bytes or allocator details.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let ownership = match &self.storage {
            WindowStorage::Absent => "absent",
            WindowStorage::Owned(_) => "owned",
            WindowStorage::Borrowed(_) => "borrowed",
        };
        f.debug_struct("InflateWindow")
            .field("ownership", &ownership)
            .field("len", &self.len())
            .finish()
    }
}

/// Values the facade must write to `z_stream` after a state reset.
///
/// `inflateResetKeep` changes both the private state and the public stream
/// (`inflate.c` L104-L120). This value carries the public half without making
/// the core crate depend on the facade's ABI type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamReset {
    /// `strm->total_in = 0` (`inflate.c` L104).
    pub total_in: u32,
    /// `strm->total_out = 0` (`inflate.c` L104).
    pub total_out: u32,
    /// `strm->msg = Z_NULL` (`inflate.c` L105).
    pub msg: Option<&'static str>,
    /// `strm->data_type = 0` (`inflate.c` L106).
    pub data_type: i32,
    /// The Adler seed to store, or [`None`] when raw mode says not to touch
    /// `strm->adler` (`inflate.c` L107-L108).
    pub adler: Option<u32>,
}

impl StreamReset {
    /// Builds the stream half for the current live wrap flags.
    const fn for_wrap(wrap: WrapFlags) -> Self {
        Self {
            total_in: 0,
            total_out: 0,
            msg: None,
            data_type: 0,
            adler: if wrap.is_raw() {
                None
            } else {
                Some(wrap.adler_seed())
            },
        }
    }
}

/// State maintained between `inflate()` calls -- approximately 7K bytes, not
/// including the allocated sliding window, which is up to 32K bytes.
///
/// This is the safe-Rust mirror of `struct inflate_state` (`inflate.h` L82-L126).
/// Build it with [`InflateState::new`] for ordinary inflate or
/// [`InflateState::with_borrowed_window`] for `inflateBack`; tear it down with
/// [`InflateState::release`] or by dropping it.
///
/// The scalar and inline-array fields follow the C declaration order. They are
/// crate-visible because the decoder is a direct state machine implementation and writes
/// nearly every one; the pointer replacements stay behind checked accessors.
///
/// `Mode::Mem` is sticky for the ordinary driver: `inflate()` returns
/// `Z_MEM_ERROR` immediately every time it sees that mode (`inflate.c` L1117
/// and L1129). Only an explicit reset, `inflateSync`, or construction may leave
/// it; no passive accessor in this type changes the mode.
///
/// # Lifetimes and allocator
///
/// `'a` covers allocator blocks, a borrowed `inflateBack` window, and any
/// installed gzip-header sink. `A` is the allocator injected in place of C's
/// `z_stream` back-pointer.
#[allow(clippy::struct_excessive_bools)]
pub struct InflateState<'a, A: Allocator<'a>> {
    /// The allocator every window and table block in this state is obtained
    /// from and must be returned to. It stands in for the reference's
    /// `z_streamp strm` back-pointer (`inflate.h` L83), of which only the
    /// `ZALLOC`/`ZFREE` hooks are needed here; stream identity stays a
    /// boundary-layer concern (AAP §0.3.3.6).
    pub(crate) allocator: A,

    /// The state the decoder is currently suspended in (`inflate.h` L84).
    pub(crate) mode: Mode,

    /// Set once the block being decoded is the stream's last
    /// (`inflate.h` L85).
    pub(crate) last: bool,

    /// Which container is expected and whether its check value is validated:
    /// bit 0 zlib, bit 1 gzip, bit 2 validate (`inflate.h` L86-L87).
    pub(crate) wrap: WrapFlags,

    /// Set once a preset dictionary has been installed (`inflate.h` L88).
    pub(crate) havedict: bool,

    /// The gzip method and flag bytes, `0` for a zlib stream, or `-1` while no
    /// header has been read yet (`inflate.h` L89-L90). `-1` is a sentinel, not a
    /// value: the trailer length check and the header callback both test for it
    /// rather than for a flag bit.
    pub(crate) flags: i32,

    /// The largest match distance the zlib header permits (`inflate.h` L91).
    /// Only an `INFLATE_STRICT` build enforces it; the shipped build records it
    /// and never rejects on it.
    pub(crate) dmax: u32,

    /// The running check value, kept here rather than in the caller's stream so
    /// that a caller cannot perturb it mid-stream (`inflate.h` L92). Adler-32
    /// and CRC-32 are exactly 32 bits, so `u32` keeps the algorithm
    /// platform-independent rather than importing the boundary layer's
    /// `c_ulong`.
    pub(crate) check: u32,

    /// The running output count, kept out of the caller's stream for the same
    /// reason as [`Self::check`] (`inflate.h` L93). The trailer compares only
    /// its low 32 bits (`inflate.c` L1101), so `u32` is the complete observable
    /// value.
    pub(crate) total: u32,

    /// Where a decoded gzip header is delivered, when the caller asked for one
    /// (`inflate.h` L94). Every write through it is clamped to the buffer
    /// lengths the caller supplied, which is what makes the gzip-header write
    /// path incapable of overrunning them.
    pub(crate) head: Option<GzHeaderSink<'a>>,

    /// Base-2 logarithm of the requested window size (`inflate.h` L96).
    pub(crate) wbits: u32,

    /// The window's size in bytes, or zero while no window is in use
    /// (`inflate.h` L97).
    pub(crate) wsize: u32,

    /// How many bytes of the window hold valid history (`inflate.h` L98). A
    /// distance may only reach back this far; that check is what keeps a
    /// malformed stream from copying uninitialised window bytes into the output.
    pub(crate) whave: u32,

    /// The window's write cursor (`inflate.h` L99).
    pub(crate) wnext: u32,

    /// The sliding window: allocated on demand, or borrowed for the duration of
    /// an `inflateBack` call (`inflate.h` L100).
    pub(crate) window: InflateWindow<'a, A>,

    /// The input bit accumulator (`inflate.h` L102). `NEEDBITS(32)` can leave
    /// 39 live bits, so this is `u64` rather than the reference's
    /// `unsigned long`.
    pub(crate) hold: u64,

    /// How many low bits of [`Self::hold`] are live (`inflate.h` L103).
    pub(crate) bits: u32,

    /// The literal just decoded, or the length of the match being copied
    /// (`inflate.h` L105).
    pub(crate) length: u32,

    /// How far back in the window the match being copied starts
    /// (`inflate.h` L106).
    pub(crate) offset: u32,

    /// How many extra bits the current code still needs (`inflate.h` L108).
    pub(crate) extra: u32,

    /// The literal/length and distance decode tables in use, together with
    /// their root index widths (`inflate.h` L110-L113).
    pub(crate) tables: CodeTables,

    /// How many code-length code lengths the dynamic header announced
    /// (`inflate.h` L115).
    pub(crate) ncode: u32,

    /// How many literal/length code lengths the dynamic header announced
    /// (`inflate.h` L116).
    pub(crate) nlen: u32,

    /// How many distance code lengths the dynamic header announced
    /// (`inflate.h` L117).
    pub(crate) ndist: u32,

    /// How many code lengths of [`Self::lens`] have been read so far
    /// (`inflate.h` L118).
    pub(crate) have: u32,

    /// The next free slot in [`Self::codes`] (`inflate.h` L119) — an index
    /// rather than the reference's `code FAR *next` pointer, so that table
    /// construction cannot walk off the end of the array.
    pub(crate) next: usize,

    /// Code lengths as read from the dynamic header, before table construction
    /// (`inflate.h` L120). [`LENS_LEN`] is 320, the largest total the header
    /// can announce.
    pub(crate) lens: [u16; LENS_LEN],

    /// Scratch space for table construction (`inflate.h` L121). [`WORK_LEN`] is
    /// 288, one entry per literal/length symbol.
    pub(crate) work: [u16; WORK_LEN],

    /// Storage both decode tables are carved out of (`inflate.h` L122).
    /// [`ENOUGH`] is the proven upper bound on the entries any legal pair of
    /// tables can need, which is why construction never has to allocate.
    pub(crate) codes: [Code; ENOUGH],

    /// Whether an out-of-range distance is rejected (`inflate.h` L123).
    ///
    /// The shipped build forces this true and returns `Z_DATA_ERROR` from
    /// `inflateUndermine`; only upstream's compile-time `INFLATE_ALLOW_INVALID`
    /// branch can clear it.
    pub(crate) sane: bool,

    /// How many bits back the last unprocessed length/literal code began, or
    /// `-1` when there is no pending code (`inflate.h` L124). `-1` is a sentinel
    /// `inflateMark` reports to the caller, so it must stay distinguishable from
    /// a real bit offset of zero.
    pub(crate) back: i32,

    /// The length of the match in progress when output space ran out
    /// (`inflate.h` L125).
    pub(crate) was: u32,
}

/// Whether an integer is a live `inflate_mode` tag.
///
/// This is the safe half of `inflateStateCheck`'s
/// `state->mode < HEAD || state->mode > SYNC` test (`inflate.c` L95). The
/// pointer, hook, and owner-identity checks remain in the facade.
#[must_use]
pub const fn is_live_mode_tag(raw: i32) -> bool {
    Mode::is_valid_tag(raw)
}

impl<'a, A: Allocator<'a>> InflateState<'a, A> {
    /// Returns the current mode in C's 16180..=16211 tag numbering.
    ///
    /// The facade uses this to synchronise the compatibility prefix required by
    /// `test/infcover.c` L331.
    #[must_use]
    pub const fn mode_tag(&self) -> i32 {
        self.mode.as_raw()
    }

    /// Installs a mode from C's tag numbering.
    ///
    /// Returns `false` and leaves the state untouched for any value that does not
    /// name a state.
    pub fn set_mode_tag(&mut self, raw: i32) -> bool {
        let Some(mode) = Mode::from_raw(raw) else {
            return false;
        };
        self.mode = mode;
        true
    }

    /// Returns the error latched by an error mode, if any.
    ///
    /// Repeated calls while in [`Mode::Mem`] keep returning
    /// [`ReturnCode::MEM_ERROR`], which is the non-recoverable behaviour
    /// `inflate()` documents at `inflate.c` L1129. [`Mode::Bad`] similarly maps
    /// to the already-recorded data error; the message itself belongs to the
    /// facade stream.
    #[must_use]
    pub(crate) const fn latched_error(&self) -> Option<ReturnCode> {
        match self.mode {
            Mode::Mem => Some(ReturnCode::MEM_ERROR),
            Mode::Bad => Some(ReturnCode::DATA_ERROR),
            _ => None,
        }
    }

    /// Latches a memory error and returns its public status code.
    pub(crate) fn latch_memory_error(&mut self) -> ReturnCode {
        self.mode = Mode::Mem;
        ReturnCode::MEM_ERROR
    }

    /// Applies `inflateValidate`: enable validation only for a wrapped stream,
    /// otherwise clear the validation bit (`inflate.c` L1384-L1394).
    pub fn set_validate(&mut self, check: bool) {
        if check && !self.wrap.is_raw() {
            self.wrap.enable_check_validation();
        } else {
            self.wrap.disable_check_validation();
        }
    }

    /// Installs or clears the safe gzip-header sink.
    ///
    /// The facade performs `inflateGetHeader`'s `wrap & 2` guard and builds the
    /// view from the ABI-visible `gz_header`; this method is the assignment
    /// `state->head = head` (`inflate.c` L1227).
    pub fn set_header_sink(&mut self, head: Option<GzHeaderSink<'a>>) {
        self.head = head;
    }

    /// Reads the gzip-header sink back out, or [`None`] when none is installed.
    ///
    /// The counterpart of [`InflateState::set_header_sink`], and the reason it
    /// has to exist: C's header states write **through** `state->head` directly
    /// into the caller's `gz_header` (`inflate.c` L566, L577, L586-L587, L596,
    /// L601, L631, L666, L678, L687), whereas this crate parses into the sink and
    /// therefore holds the scalars on its own side. Only the facade owns the raw
    /// `gz_headerp`, so only the facade can publish them, and it needs a way to
    /// read them. [`GzHeaderSink`] is [`Copy`], so this hands back a snapshot and
    /// the state keeps its own.
    ///
    /// The three variable-length members need no such step: they are
    /// `&[Cell<u8>]` views over the caller's own buffers, so a byte written by
    /// the `EXTRA`, `NAME` or `COMMENT` state is already in the caller's memory.
    /// What the facade takes from here is the seven scalars -- `text`, `time`,
    /// `xflags`, `os`, `extra_len`, `hcrc`, `done` -- plus the three
    /// `*_is_absent` answers that tell it to store `Z_NULL` back into the
    /// caller's structure.
    #[must_use]
    pub const fn header_sink(&self) -> Option<GzHeaderSink<'a>> {
        self.head
    }

    /// `INITBITS`: clear the input accumulator (`inflate.c` L347-L351).
    pub(crate) fn init_bits(&mut self) {
        self.hold = 0;
        self.bits = 0;
    }

    /// `PULLBYTE`: append one byte above the currently live low bits
    /// (`inflate.c` L356-L362).
    ///
    /// Returns `false` rather than losing bits if a corrupted state claims
    /// insufficient room for a whole byte. Valid inflate states need at most 39
    /// live bits and always return `true`.
    pub(crate) fn pull_byte(&mut self, byte: u8) -> bool {
        if self.bits > u64::BITS - u8::BITS {
            return false;
        }
        let Some(shifted) = u64::from(byte).checked_shl(self.bits) else {
            return false;
        };
        let Some(hold) = self.hold.checked_add(shifted) else {
            return false;
        };
        let Some(bits) = self.bits.checked_add(u8::BITS) else {
            return false;
        };
        self.hold = hold;
        self.bits = bits;
        true
    }

    /// `NEEDBITS(n)`: pull from `input` until at least `needed` bits are live
    /// (`inflate.c` L365-L371).
    ///
    /// `next` is updated only for bytes successfully consumed. Returning
    /// `false` means either the chunk ended or a corrupted accumulator could
    /// not accept another byte; the caller may resume with a later chunk
    /// without losing the bits already present.
    pub(crate) fn need_bits(&mut self, needed: u32, input: &[u8], next: &mut usize) -> bool {
        if needed > u64::BITS {
            return false;
        }
        while self.bits < needed {
            let Some(byte) = input.get(*next).copied() else {
                return false;
            };
            if !self.pull_byte(byte) {
                return false;
            }
            let Some(after) = next.checked_add(1) else {
                return false;
            };
            *next = after;
        }
        true
    }

    /// Whether at least `needed` bits are live.
    #[must_use]
    pub(crate) const fn has_bits(&self, needed: u32) -> bool {
        needed <= u64::BITS && self.bits >= needed
    }

    /// `BITS(n)`: the low `count` bits of `hold` (`inflate.c` L373-L376).
    ///
    /// Returns [`None`] unless the bits are already present and the requested
    /// value fits the 32-bit result used by the inflate state machine.
    #[must_use]
    pub(crate) fn low_bits(&self, count: u32) -> Option<u32> {
        if count > u32::BITS || !self.has_bits(count) {
            return None;
        }
        let mask = match count {
            0 => 0,
            u32::BITS => u64::from(u32::MAX),
            _ => u64::from(1_u32.checked_shl(count)?.checked_sub(1)?),
        };
        u32::try_from(self.hold & mask).ok()
    }

    /// `DROPBITS(n)`: remove `count` low bits (`inflate.c` L378-L382).
    ///
    /// Returns `false` and changes nothing if the state did not contain that
    /// many bits.
    pub(crate) fn drop_bits(&mut self, count: u32) -> bool {
        let Some(bits) = self.bits.checked_sub(count) else {
            return false;
        };
        let hold = if count == u64::BITS {
            0
        } else {
            let Some(shifted) = self.hold.checked_shr(count) else {
                return false;
            };
            shifted
        };
        self.hold = hold;
        self.bits = bits;
        true
    }

    /// `BYTEBITS`: discard zero to seven bits to reach a byte boundary
    /// (`inflate.c` L384-L389).
    pub(crate) fn byte_bits(&mut self) {
        let unaligned = self.bits & 7;
        let _ = self.drop_bits(unaligned);
    }

    /// Removes and returns the low byte, or [`None`] when fewer than eight bits
    /// are live.
    pub(crate) fn take_byte(&mut self) -> Option<u8> {
        let byte = u8::try_from(self.low_bits(u8::BITS)?).ok()?;
        if self.drop_bits(u8::BITS) {
            Some(byte)
        } else {
            None
        }
    }

    /// Implements `inflatePrime`'s accumulator update (`inflate.c` L215-L232).
    ///
    /// A negative `count` clears the accumulator; zero is a no-op. Positive
    /// requests are limited to 16 bits and may not take occupancy above 32,
    /// exactly as in C.
    pub fn prime(&mut self, count: i32, value: i32) -> ReturnCode {
        if count == 0 {
            return ReturnCode::OK;
        }
        if count < 0 {
            self.init_bits();
            return ReturnCode::OK;
        }
        if count > MAX_PRIME_BITS {
            return ReturnCode::STREAM_ERROR;
        }
        let Ok(count) = u32::try_from(count) else {
            return ReturnCode::STREAM_ERROR;
        };
        let Some(bits) = self.bits.checked_add(count) else {
            return ReturnCode::STREAM_ERROR;
        };
        if bits > MAX_PRIME_HOLD_BITS {
            return ReturnCode::STREAM_ERROR;
        }
        let Some(mask) = 1_u32.checked_shl(count).and_then(|one| one.checked_sub(1)) else {
            return ReturnCode::STREAM_ERROR;
        };
        let value = u32::from_ne_bytes(value.to_ne_bytes()) & mask;
        let Some(value) = u64::from(value).checked_shl(self.bits) else {
            return ReturnCode::STREAM_ERROR;
        };
        let Some(hold) = self.hold.checked_add(value) else {
            return ReturnCode::STREAM_ERROR;
        };
        self.hold = hold;
        self.bits = bits;
        ReturnCode::OK
    }

    /// The low 32 bits of the accumulator.
    ///
    /// Used by the gzip `TIME`, dictionary-id, and trailer paths after
    /// `NEEDBITS(32)`.
    #[must_use]
    pub(crate) fn hold_low32(&self) -> u32 {
        u32::try_from(self.hold & u64::from(u32::MAX)).unwrap_or(0)
    }

    /// Selects the shared RFC 1951 fixed tables and their 9/5-bit roots.
    ///
    /// The four assignments from `inflate_fixed` (`inftrees.c` L368-L371).
    pub(crate) fn use_fixed_tables(&mut self) {
        self.tables = CodeTables::FIXED;
    }

    /// Selects dynamic tables in this state's `codes` arena.
    ///
    /// Returns `false` without changing the state if either table starts
    /// outside the arena.
    pub(crate) fn use_dynamic_tables(
        &mut self,
        lencode_offset: usize,
        lenbits: usize,
        distcode_offset: usize,
        distbits: usize,
    ) -> bool {
        if lencode_offset >= ENOUGH || distcode_offset >= ENOUGH {
            return false;
        }
        self.tables = CodeTables {
            lencode: CodeTableSource::Dynamic {
                offset: lencode_offset,
            },
            lenbits,
            distcode: CodeTableSource::Dynamic {
                offset: distcode_offset,
            },
            distbits,
        };
        true
    }

    /// Resolves the current literal/length table.
    ///
    /// An invalid dynamic offset yields an empty slice rather than panicking;
    /// the decoder then reports invalid data through its normal checked lookup.
    #[must_use]
    pub(crate) fn lencode(&self) -> &[Code] {
        match self.tables.lencode {
            CodeTableSource::Fixed => &lenfix,
            CodeTableSource::Dynamic { offset } => self.codes.get(offset..).unwrap_or(&[]),
        }
    }

    /// Resolves the current distance table, with the same total behaviour as
    /// [`InflateState::lencode`].
    #[must_use]
    pub(crate) fn distcode(&self) -> &[Code] {
        match self.tables.distcode {
            CodeTableSource::Fixed => &distfix,
            CodeTableSource::Dynamic { offset } => self.codes.get(offset..).unwrap_or(&[]),
        }
    }

    /// Number of arena entries consumed by dynamic tables.
    ///
    /// This is `inflateCodesUsed`: `state->next - state->codes`
    /// (`inflate.c` L1406-L1411), reduced to a direct index read.
    #[must_use]
    pub const fn codes_used(&self) -> usize {
        self.next
    }

    /// Reads a logically valid history byte.
    ///
    /// When the window has not wrapped yet, only `0..whave` has been written;
    /// once `whave == wsize`, every allocated index is valid. This gate is what
    /// keeps an allocator's `0xa5` fill from becoming decompressed output.
    #[must_use]
    pub(crate) fn valid_window_byte(&self, index: usize) -> Option<u8> {
        let wsize = usize::try_from(self.wsize).ok()?;
        let whave = usize::try_from(self.whave).ok()?;
        if wsize == 0 || whave > wsize || index >= whave {
            return None;
        }
        self.window.byte_at(index)
    }
}

impl<'a, A: Allocator<'a>> InflateState<'a, A> {
    /// Constructs an ordinary inflate state from a caller request.
    ///
    /// This is the core half of `inflateInit2_`: version/stream-size checks,
    /// hook defaulting, allocation of the state object, and installation into
    /// `z_stream.state` remain facade responsibilities. The returned value is
    /// fully initialized as if C's `zmemzero` and `inflateReset2` had run.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::STREAM_ERROR`] when `config.window_bits` is outside
    /// `inflateReset2`'s accepted matrix.
    pub fn new(config: InflateConfig, allocator: A) -> Result<Self, ReturnCode> {
        let config = config.validate()?;
        Ok(Self::with_validated_config(config, allocator))
    }

    /// Constructs a state from an already validated inflate configuration.
    ///
    /// The window remains absent and will be allocated only when output history
    /// first has to be retained (`inflate.c` L257-L263).
    #[must_use]
    pub fn with_validated_config(config: ValidatedInflateConfig, allocator: A) -> Self {
        Self {
            allocator,
            mode: Mode::Head,
            last: false,
            wrap: WrapFlags::from_request(config.wrap),
            havedict: false,
            flags: FLAGS_NO_HEADER,
            dmax: DMAX_DEFAULT,
            check: 0,
            total: 0,
            head: None,
            wbits: u32::from(config.window_bits),
            wsize: 0,
            whave: 0,
            wnext: 0,
            window: InflateWindow::absent(),
            hold: 0,
            bits: 0,
            length: 0,
            offset: 0,
            extra: 0,
            tables: CodeTables::RESET,
            ncode: 0,
            nlen: 0,
            ndist: 0,
            have: 0,
            next: 0,
            lens: [0; LENS_LEN],
            work: [0; WORK_LEN],
            codes: [Code::ZERO; ENOUGH],
            sane: true,
            back: BACK_UNKNOWN,
            was: 0,
        }
    }

    /// Constructs the state used by `inflateBackInit_`, borrowing the caller's
    /// output window.
    ///
    /// `window_bits` must be 8..=15 and `window` must contain at least
    /// `2**window_bits` bytes (`infback.c` L22-L35). A longer slice is accepted
    /// but only the required prefix is borrowed, exactly matching C's implicit
    /// window extent.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::STREAM_ERROR`] for an invalid exponent, a short window, or
    /// an unrepresentable size.
    pub fn with_borrowed_window(
        window_bits: i32,
        window: &'a mut [u8],
        allocator: A,
    ) -> Result<Self, ReturnCode> {
        let window_bits = validate_inflate_back_window_bits(window_bits)?;
        let size = 1_usize
            .checked_shl(u32::from(window_bits))
            .ok_or(ReturnCode::STREAM_ERROR)?;
        let window = window.get_mut(..size).ok_or(ReturnCode::STREAM_ERROR)?;
        let mut state = Self::with_validated_config(
            ValidatedInflateConfig {
                wrap: InflateWrap::None,
                window_bits,
            },
            allocator,
        );
        state.wsize = u32::try_from(size).map_err(|_| ReturnCode::STREAM_ERROR)?;
        state.window = InflateWindow::borrowed(window);
        Ok(state)
    }

    /// Performs the private-state half of `inflateResetKeep`
    /// (`inflate.c` L99-L123) and returns the public-stream half.
    ///
    /// Decode-table root widths deliberately remain unchanged: C resets the two
    /// table pointers and `next`, but not `lenbits` or `distbits`. They are
    /// meaningless until the next table is selected.
    pub fn reset_keep(&mut self) -> StreamReset {
        self.total = 0;
        self.mode = Mode::Head;
        self.last = false;
        self.havedict = false;
        self.flags = FLAGS_NO_HEADER;
        self.dmax = DMAX_DEFAULT;
        self.head = None;
        self.init_bits();
        self.tables.lencode = CodeTableSource::Dynamic { offset: 0 };
        self.tables.distcode = CodeTableSource::Dynamic { offset: 0 };
        self.next = 0;
        self.sane = true;
        self.back = BACK_UNKNOWN;
        StreamReset::for_wrap(self.wrap)
    }

    /// Performs `inflateReset`: clear window cursors, then reset while retaining
    /// the allocation (`inflate.c` L124-L133).
    pub fn reset(&mut self) -> StreamReset {
        self.wsize = 0;
        self.whave = 0;
        self.wnext = 0;
        self.reset_keep()
    }

    /// Performs `inflateReset2`, consuming the canonical decode in
    /// [`InflateConfig::validate`] rather than duplicating its arithmetic.
    ///
    /// If the validated exponent differs, an owned window is returned to its
    /// allocator and a borrowed window is merely detached, reproducing the
    /// `ZFREE`/`Z_NULL` update at `inflate.c` L162-L165.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::STREAM_ERROR`] for an invalid request. Validation happens
    /// before mutation, so the previous state remains intact on error.
    pub fn reset2(&mut self, config: InflateConfig) -> Result<StreamReset, ReturnCode> {
        let config = config.validate()?;
        let wbits = u32::from(config.window_bits);
        if !self.window.is_absent() && self.wbits != wbits {
            self.window.discard();
        }
        self.wrap = WrapFlags::from_request(config.wrap);
        self.wbits = wbits;
        Ok(self.reset())
    }

    /// Releases the current window and clears all of its cursors.
    ///
    /// Owned storage is freed through its originating allocator; borrowed
    /// storage is never freed.
    pub fn discard_window(&mut self) {
        self.window.discard();
        self.wsize = 0;
        self.whave = 0;
        self.wnext = 0;
    }

    /// Computes `1U << wbits` without a potentially panicking or truncating
    /// shift.
    fn requested_window_size(&self) -> Result<usize, ReturnCode> {
        if self.wbits == 0 {
            return Err(ReturnCode::STREAM_ERROR);
        }
        1_usize
            .checked_shl(self.wbits)
            .ok_or(ReturnCode::STREAM_ERROR)
    }

    /// Reproduces the default-build `inflateUndermine`: keep `sane` true and
    /// report `Z_DATA_ERROR` (`inflate.c` L1369-L1382).
    pub fn undermine(&mut self, _subvert: bool) -> ReturnCode {
        self.sane = true;
        ReturnCode::DATA_ERROR
    }

    /// Computes the `strm->data_type` value from the saved state
    /// (`inflate.c` L1146-L1149).
    #[must_use]
    pub fn data_type(&self) -> i32 {
        let mut data_type = i32::try_from(self.bits).unwrap_or(i32::MAX);
        data_type = data_type.saturating_add(if self.last { 64 } else { 0 });
        data_type = data_type.saturating_add(if self.mode == Mode::Type { 128 } else { 0 });
        data_type.saturating_add(if matches!(self.mode, Mode::LenFirst | Mode::CopyBlock) {
            256
        } else {
            0
        })
    }

    /// Explicitly performs the memory half of `inflateEnd`.
    ///
    /// Dropping the value has the same effect: [`InflateWindow`] owns its
    /// allocator-backed block and releases only the owned variant. Freeing the
    /// allocation that contains this state and clearing `z_stream.state` remain
    /// facade responsibilities (`inflate.c` L1160-L1162).
    pub fn release(self) {
        drop(self);
    }
}

impl<'a, A> InflateState<'a, A>
where
    A: Allocator<'a> + Copy,
{
    /// Ensures the sliding window exists and is sized for `wbits`.
    ///
    /// This is the allocation/initialisation prefix of `updatewindow`
    /// (`inflate.c` L246-L271). The allocation is lazy; an existing borrowed or
    /// owned window is used unchanged. When `wsize` is zero, the three window
    /// cursors are initialized exactly as C initializes them.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::MEM_ERROR`] if allocation fails, or
    /// [`ReturnCode::STREAM_ERROR`] if `wbits` cannot name the existing window.
    pub fn ensure_window(&mut self) -> Result<&mut [u8], ReturnCode> {
        let size = self.requested_window_size()?;
        self.window.ensure_owned(size, self.allocator)?;
        if self.window.len() != size {
            return Err(ReturnCode::STREAM_ERROR);
        }
        if self.wsize == 0 {
            self.wsize = u32::try_from(size).map_err(|_| ReturnCode::STREAM_ERROR)?;
            self.wnext = 0;
            self.whave = 0;
        }
        self.window.as_mut_slice().ok_or(ReturnCode::STREAM_ERROR)
    }

    /// Clones this state using the destination allocator.
    ///
    /// This is `inflateCopy` (`inflate.c` L1328-L1367) with raw pointer fix-ups
    /// removed. Dynamic table sources are offsets into `codes`, so the plain
    /// field copy already points at the copied arena; fixed sources remain the
    /// shared static tables. If a window exists, a new full-size allocation is
    /// made and only its first `whave` bytes are copied, exactly as L1363 does.
    ///
    /// # Errors
    ///
    /// [`ReturnCode::MEM_ERROR`] if the destination window allocation fails.
    /// [`ReturnCode::STREAM_ERROR`] if a corrupted source claims more valid
    /// window bytes than it owns.
    pub fn try_clone_in<'b, B>(&self, allocator: B) -> Result<InflateState<'b, B>, ReturnCode>
    where
        'a: 'b,
        B: Allocator<'b> + Copy,
    {
        let valid = usize::try_from(self.whave).map_err(|_| ReturnCode::STREAM_ERROR)?;
        let window = self.window.try_clone_in(allocator, valid)?;
        let head: Option<GzHeaderSink<'b>> = self.head;
        Ok(InflateState {
            allocator,
            mode: self.mode,
            last: self.last,
            wrap: self.wrap,
            havedict: self.havedict,
            flags: self.flags,
            dmax: self.dmax,
            check: self.check,
            total: self.total,
            head,
            wbits: self.wbits,
            wsize: self.wsize,
            whave: self.whave,
            wnext: self.wnext,
            window,
            hold: self.hold,
            bits: self.bits,
            length: self.length,
            offset: self.offset,
            extra: self.extra,
            tables: self.tables,
            ncode: self.ncode,
            nlen: self.nlen,
            ndist: self.ndist,
            have: self.have,
            next: self.next,
            lens: self.lens,
            work: self.work,
            codes: self.codes,
            sane: self.sane,
            back: self.back,
            was: self.was,
        })
    }
}

impl<'a, A: Allocator<'a>> fmt::Debug for InflateState<'a, A> {
    /// Reports resumability scalars and buffer shapes, never allocator state,
    /// inline table contents, window bytes, or caller header bytes.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InflateState")
            .field("mode", &self.mode)
            .field("last", &self.last)
            .field("wrap", &self.wrap)
            .field("havedict", &self.havedict)
            .field("flags", &self.flags)
            .field("dmax", &self.dmax)
            .field("check", &self.check)
            .field("total", &self.total)
            .field("head", &self.head)
            .field("wbits", &self.wbits)
            .field("wsize", &self.wsize)
            .field("whave", &self.whave)
            .field("wnext", &self.wnext)
            .field("window", &self.window)
            .field("hold", &self.hold)
            .field("bits", &self.bits)
            .field("length", &self.length)
            .field("offset", &self.offset)
            .field("extra", &self.extra)
            .field("tables", &self.tables)
            .field("ncode", &self.ncode)
            .field("nlen", &self.nlen)
            .field("ndist", &self.ndist)
            .field("have", &self.have)
            .field("next", &self.next)
            .field("lens_len", &self.lens.len())
            .field("work_len", &self.work.len())
            .field("codes_len", &self.codes.len())
            .field("sane", &self.sane)
            .field("back", &self.back)
            .field("was", &self.was)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used
)]
mod tests {
    use super::{
        is_live_mode_tag, GzHeaderSink, InflateState, StreamReset, WrapFlags, BACK_UNKNOWN,
        DMAX_DEFAULT, FLAGS_NO_HEADER, LENS_LEN, WORK_LEN,
    };
    use crate::allocate::{Allocator, AllocatorId, Buffer, GlobalAllocator, Opaque};
    use crate::config::{InflateConfig, InflateWrap};
    use crate::error::ReturnCode;
    use crate::inflate::inftrees::{Code, CodeTableSource, CodeTables, ENOUGH};
    use crate::inflate::mode::Mode;
    use core::cell::{Cell, RefCell};
    use core::mem::size_of;

    type TestState = InflateState<'static, GlobalAllocator>;

    const C_FIELDS: [&str; 35] = [
        "strm", "mode", "last", "wrap", "havedict", "flags", "dmax", "check", "total", "head",
        "wbits", "wsize", "whave", "wnext", "window", "hold", "bits", "length", "offset", "extra",
        "lencode", "distcode", "lenbits", "distbits", "ncode", "nlen", "ndist", "have", "next",
        "lens", "work", "codes", "sane", "back", "was",
    ];

    fn new_state(window_bits: i32) -> TestState {
        InflateState::new(InflateConfig::new(window_bits), GlobalAllocator).unwrap()
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Event {
        Unused,
        Allocate(usize),
        Free(usize),
    }

    const EVENT_CAPACITY: usize = 16;

    struct RecordingAllocator {
        events: RefCell<[Event; EVENT_CAPACITY]>,
        count: Cell<usize>,
        fill: u8,
    }

    impl RecordingAllocator {
        fn new(fill: u8) -> Self {
            Self {
                events: RefCell::new([Event::Unused; EVENT_CAPACITY]),
                count: Cell::new(0),
                fill,
            }
        }

        fn record(&self, event: Event) {
            let index = self.count.get();
            let mut events = self.events.borrow_mut();
            if let Some(slot) = events.get_mut(index) {
                *slot = event;
                self.count.set(index.saturating_add(1));
            }
        }

        fn event(&self, index: usize) -> Option<Event> {
            self.events.borrow().get(index).copied()
        }

        fn len(&self) -> usize {
            self.count.get()
        }

        fn clear(&self) {
            self.events.replace([Event::Unused; EVENT_CAPACITY]);
            self.count.set(0);
        }
    }

    impl<'a> Allocator<'a> for RecordingAllocator {
        fn id(&self) -> AllocatorId {
            // Test storage comes from `Buffer::try_global`; using its identity
            // lets this recorder observe calls around the real safe allocator.
            AllocatorId::GLOBAL
        }

        fn opaque(&self) -> Opaque {
            Opaque::NULL
        }

        fn allocate_bytes(&self, items: usize, size: usize) -> Option<Buffer<'a, u8>> {
            let len = items.checked_mul(size)?;
            self.record(Event::Allocate(len));
            Buffer::try_global(len, self.fill)
        }

        fn allocate_u16s(&self, items: usize) -> Option<Buffer<'a, u16>> {
            let bytes = items.checked_mul(size_of::<u16>())?;
            self.record(Event::Allocate(bytes));
            Buffer::try_global(items, u16::from_ne_bytes([self.fill, self.fill]))
        }

        fn deallocate_bytes(&self, buffer: Buffer<'a, u8>) {
            self.record(Event::Free(buffer.len()));
            GlobalAllocator.deallocate_bytes(buffer);
        }

        fn deallocate_u16s(&self, buffer: Buffer<'a, u16>) {
            let bytes = buffer.len().saturating_mul(size_of::<u16>());
            self.record(Event::Free(bytes));
            GlobalAllocator.deallocate_u16s(buffer);
        }
    }

    #[test]
    fn all_inflate_header_fields_are_represented() {
        let state = new_state(15);
        // One reference per Rust field, in the order of `inflate.h` L83-L125.
        // `tables` accounts for C's lencode/distcode/lenbits/distbits quartet.
        let rust_fields = (
            &state.allocator,
            &state.mode,
            &state.last,
            &state.wrap,
            &state.havedict,
            &state.flags,
            &state.dmax,
            &state.check,
            &state.total,
            &state.head,
            &state.wbits,
            &state.wsize,
            &state.whave,
            &state.wnext,
            &state.window,
            &state.hold,
            &state.bits,
            &state.length,
            &state.offset,
            &state.extra,
            &state.tables,
            &state.ncode,
            &state.nlen,
            &state.ndist,
            &state.have,
            &state.next,
            &state.lens,
            &state.work,
            &state.codes,
            &state.sane,
            &state.back,
            &state.was,
        );
        let _ = rust_fields;
        assert_eq!(C_FIELDS.len(), 35);
    }

    #[test]
    fn inline_arrays_and_state_footprint_match_the_c_budget() {
        let state = new_state(15);
        assert_eq!(state.lens.len(), LENS_LEN);
        assert_eq!(state.work.len(), WORK_LEN);
        assert_eq!(state.codes.len(), ENOUGH);

        let measured = size_of::<TestState>();
        let shared_allocator = size_of::<InflateState<'static, &'static GlobalAllocator>>();
        let dynamic_allocator = size_of::<InflateState<'static, &'static dyn Allocator<'static>>>();
        // C is 7160 bytes on x86_64. test/infcover.c L428 requires this state
        // to stay above 7032 bytes, while AAP §0.8.4 caps it at 115% (8234), so
        // the range assertions below are the contract. The three exact sizes are
        // pinned on 64-bit targets so that a change in field layout, or in how
        // much space an allocator handle occupies, has to be acknowledged here
        // rather than silently drifting toward either bound.
        if cfg!(target_pointer_width = "64") {
            assert_eq!(measured, 7280);
            assert_eq!(shared_allocator, 7296);
            assert_eq!(dynamic_allocator, 7312);
        }
        assert!(
            (7033..=8234).contains(&measured),
            "InflateState footprint {measured} escaped the 7033..=8234 contract"
        );
        assert!((7033..=8234).contains(&shared_allocator));
        assert!((7033..=8234).contains(&dynamic_allocator));
    }

    #[test]
    fn construction_and_reset_values_match_inflate_reset_keep() {
        let mut state = new_state(15);
        assert_eq!(state.mode, Mode::Head);
        assert!(!state.last);
        assert_eq!(state.wrap, WrapFlags::from_request(InflateWrap::Zlib));
        assert!(!state.havedict);
        assert_eq!(state.flags, FLAGS_NO_HEADER);
        assert_eq!(state.dmax, DMAX_DEFAULT);
        assert_eq!(state.check, 0);
        assert_eq!(state.total, 0);
        assert!(state.head.is_none());
        assert_eq!(state.wbits, 15);
        assert_eq!(state.wsize, 0);
        assert_eq!(state.whave, 0);
        assert_eq!(state.wnext, 0);
        assert!(state.window.is_absent());
        assert_eq!(state.hold, 0);
        assert_eq!(state.bits, 0);
        assert_eq!(state.length, 0);
        assert_eq!(state.offset, 0);
        assert_eq!(state.extra, 0);
        assert_eq!(state.tables, CodeTables::RESET);
        assert_eq!(state.ncode, 0);
        assert_eq!(state.nlen, 0);
        assert_eq!(state.ndist, 0);
        assert_eq!(state.have, 0);
        assert_eq!(state.next, 0);
        assert!(state.lens.iter().all(|value| *value == 0));
        assert!(state.work.iter().all(|value| *value == 0));
        assert!(state.codes.iter().all(|code| *code == Code::ZERO));
        assert!(state.sane);
        assert_eq!(state.back, BACK_UNKNOWN);
        assert_eq!(state.was, 0);

        state.total = 99;
        state.mode = Mode::Mem;
        state.last = true;
        state.havedict = true;
        state.flags = 7;
        state.dmax = 1;
        state.head = Some(GzHeaderSink::new(None, None, None));
        state.hold = 0x55;
        state.bits = 7;
        state.tables = CodeTables::FIXED;
        state.next = 42;
        state.sane = false;
        state.back = 9;
        state.check = 0x1234_5678;
        state.tables.lenbits = 11;
        state.tables.distbits = 6;

        assert_eq!(
            state.reset_keep(),
            StreamReset {
                total_in: 0,
                total_out: 0,
                msg: None,
                data_type: 0,
                adler: Some(1),
            }
        );
        assert_eq!(state.mode, Mode::Head);
        assert!(!state.last);
        assert!(!state.havedict);
        assert_eq!(state.flags, FLAGS_NO_HEADER);
        assert_eq!(state.dmax, DMAX_DEFAULT);
        assert!(state.head.is_none());
        assert_eq!(state.hold, 0);
        assert_eq!(state.bits, 0);
        assert_eq!(state.tables.lencode, CodeTableSource::Dynamic { offset: 0 });
        assert_eq!(
            state.tables.distcode,
            CodeTableSource::Dynamic { offset: 0 }
        );
        assert_eq!(state.tables.lenbits, 11);
        assert_eq!(state.tables.distbits, 6);
        assert_eq!(state.next, 0);
        assert!(state.sane);
        assert_eq!(state.back, BACK_UNKNOWN);
        assert_eq!(state.check, 0x1234_5678);
    }

    fn assert_reset2(request: i32, wrap: InflateWrap, wbits: u32) {
        let mut state = new_state(15);
        let reset = state.reset2(InflateConfig::new(request)).unwrap();
        assert_eq!(state.wrap, WrapFlags::from_request(wrap));
        assert_eq!(state.wbits, wbits);
        assert_eq!(
            reset.adler,
            if wrap == InflateWrap::None {
                None
            } else {
                Some(WrapFlags::from_request(wrap).adler_seed())
            }
        );
    }

    #[test]
    fn reset2_acceptance_matrix_matches_inflate_c() {
        assert_eq!(
            new_state(15).reset2(InflateConfig::new(-16)),
            Err(ReturnCode::STREAM_ERROR)
        );
        for request in -15..=-8 {
            let wbits = u32::try_from(-request).unwrap();
            assert_reset2(request, InflateWrap::None, wbits);
        }
        for request in -7..=-1 {
            assert_eq!(
                new_state(15).reset2(InflateConfig::new(request)),
                Err(ReturnCode::STREAM_ERROR)
            );
        }
        assert_reset2(0, InflateWrap::Zlib, 0);
        for request in 8..=15 {
            assert_reset2(request, InflateWrap::Zlib, u32::try_from(request).unwrap());
        }
        for request in 24..=31 {
            assert_reset2(
                request,
                InflateWrap::Gzip,
                u32::try_from(request & 15).unwrap(),
            );
        }
        for request in 40..=47 {
            assert_reset2(
                request,
                InflateWrap::ZlibOrGzip,
                u32::try_from(request & 15).unwrap(),
            );
        }
        for request in [48, 49, i32::MAX] {
            assert_eq!(
                new_state(15).reset2(InflateConfig::new(request)),
                Err(ReturnCode::STREAM_ERROR)
            );
        }
    }

    #[test]
    fn allocator_round_trip_is_lifo_and_borrowed_window_is_never_freed() {
        let recorder = RecordingAllocator::new(0xa5);
        let state_size = size_of::<InflateState<'static, &'static RecordingAllocator>>();
        let state_block = recorder.allocate_bytes(1, state_size).unwrap();
        let mut state = InflateState::new(InflateConfig::new(-8), &recorder).unwrap();
        assert_eq!(state.ensure_window().unwrap().len(), 256);
        state.release();
        recorder.deallocate_bytes(state_block);

        assert_eq!(recorder.len(), 4);
        assert_eq!(recorder.event(0), Some(Event::Allocate(state_size)));
        assert_eq!(recorder.event(1), Some(Event::Allocate(256)));
        assert_eq!(recorder.event(2), Some(Event::Free(256)));
        assert_eq!(recorder.event(3), Some(Event::Free(state_size)));

        recorder.clear();
        let state_block = recorder.allocate_bytes(1, state_size).unwrap();
        let mut caller_window = [0_u8; 256];
        let state = InflateState::with_borrowed_window(8, &mut caller_window, &recorder).unwrap();
        assert!(state.window.is_borrowed());
        state.release();
        recorder.deallocate_bytes(state_block);

        assert_eq!(recorder.len(), 2);
        assert_eq!(recorder.event(0), Some(Event::Allocate(state_size)));
        assert_eq!(recorder.event(1), Some(Event::Free(state_size)));
    }

    #[test]
    fn allocator_fill_is_not_logically_valid_window_history() {
        let recorder = RecordingAllocator::new(0xa5);
        let mut state = InflateState::new(InflateConfig::new(-8), &recorder).unwrap();
        state.ensure_window().unwrap();
        assert_eq!(state.window.byte_at(0), Some(0xa5));
        assert_eq!(state.valid_window_byte(0), None);
        assert!(state.window.set_byte(0, 0x4d));
        assert_eq!(state.valid_window_byte(0), None);
        state.whave = state.wsize.saturating_add(1);
        assert_eq!(state.valid_window_byte(0), None);
        state.whave = 1;
        assert_eq!(state.valid_window_byte(0), Some(0x4d));
        assert_eq!(state.valid_window_byte(1), None);
    }

    #[test]
    fn inflate_copy_copies_only_whave_window_bytes() {
        let source_allocator = RecordingAllocator::new(0xa5);
        let destination_allocator = RecordingAllocator::new(0x5a);
        let mut source = InflateState::new(InflateConfig::new(-8), &source_allocator).unwrap();
        source.ensure_window().unwrap();
        assert!(source.window.set_byte(0, 1));
        assert!(source.window.set_byte(1, 2));
        assert!(source.window.set_byte(5, 0xee));
        source.whave = 2;
        source.next = 17;
        assert!(source.use_dynamic_tables(3, 9, 11, 6));

        let copy = source.try_clone_in(&destination_allocator).unwrap();
        assert_eq!(copy.window.byte_at(0), Some(1));
        assert_eq!(copy.window.byte_at(1), Some(2));
        assert_eq!(copy.window.byte_at(5), Some(0x5a));
        assert_eq!(copy.next, 17);
        assert_eq!(copy.tables, source.tables);
    }

    #[test]
    fn gzip_header_buffers_may_alias_and_are_bounds_clamped() {
        let shared: [Cell<u8>; 4] = core::array::from_fn(|_| Cell::new(0));
        let mut sink = GzHeaderSink::new(Some(&shared), Some(&shared), Some(&shared));
        assert_eq!(sink.write_extra(2, &[1, 2, 3]), 2);
        assert_eq!(shared[2].get(), 1);
        assert_eq!(shared[3].get(), 2);
        assert!(sink.push_name(0, b'n'));
        assert!(sink.push_comment(1, b'c'));
        assert_eq!(shared[0].get(), b'n');
        assert_eq!(shared[1].get(), b'c');
        assert!(!sink.push_name(4, b'x'));
        sink.clear_extra();
        assert!(sink.extra_is_absent());
        assert_eq!(sink.write_extra(0, &[9]), 0);
    }

    #[test]
    fn need_bits_32_resumes_across_a_chunk_boundary() {
        let mut state = new_state(15);
        let mut next = 0;
        assert!(!state.need_bits(32, &[0x78], &mut next));
        assert_eq!(next, 1);
        assert_eq!(state.bits, 8);

        next = 0;
        assert!(state.need_bits(32, &[0x56, 0x34, 0x12], &mut next));
        assert_eq!(next, 3);
        assert_eq!(state.bits, 32);
        assert_eq!(state.hold_low32(), 0x1234_5678);
        assert_eq!(state.low_bits(32), Some(0x1234_5678));
        assert!(state.drop_bits(32));
        assert_eq!(state.bits, 0);

        assert_eq!(state.prime(7, 0x55), ReturnCode::OK);
        next = 0;
        assert!(state.need_bits(32, &[1, 2, 3, 4], &mut next));
        assert_eq!(state.bits, 39);
    }

    #[test]
    fn prime_enforces_the_reference_bounds() {
        let mut state = new_state(15);
        assert_eq!(state.prime(16, 0xabcd), ReturnCode::OK);
        assert_eq!(state.bits, 16);
        assert_eq!(state.prime(16, 0x1234), ReturnCode::OK);
        assert_eq!(state.bits, 32);
        assert_eq!(state.prime(1, 1), ReturnCode::STREAM_ERROR);
        assert_eq!(state.prime(17, 1), ReturnCode::STREAM_ERROR);
        assert_eq!(state.prime(-1, 0), ReturnCode::OK);
        assert_eq!(state.bits, 0);
        assert_eq!(state.hold, 0);
    }

    #[test]
    fn fixed_and_dynamic_code_sources_resolve_without_pointer_rebasing() {
        let mut state = new_state(15);
        state.codes[4] = Code::new(16, 3, 9);
        state.codes[12] = Code::new(17, 4, 10);
        assert!(state.use_dynamic_tables(4, 9, 12, 6));
        assert_eq!(state.lencode().first(), Some(&Code::new(16, 3, 9)));
        assert_eq!(state.distcode().first(), Some(&Code::new(17, 4, 10)));
        state.use_fixed_tables();
        assert_eq!(state.lencode().len(), 512);
        assert_eq!(state.distcode().len(), 32);
        assert!(!state.use_dynamic_tables(ENOUGH, 9, 0, 6));
    }

    #[test]
    fn mem_mode_and_mode_tags_are_sticky_and_validated() {
        let mut state = new_state(15);
        assert!(is_live_mode_tag(Mode::Head.as_raw()));
        assert!(is_live_mode_tag(Mode::Sync.as_raw()));
        assert!(!is_live_mode_tag(Mode::Head.as_raw() - 1));
        assert!(!state.set_mode_tag(i32::MIN));
        assert_eq!(state.mode, Mode::Head);
        assert_eq!(state.latch_memory_error(), ReturnCode::MEM_ERROR);
        assert_eq!(state.latched_error(), Some(ReturnCode::MEM_ERROR));
        assert_eq!(state.latched_error(), Some(ReturnCode::MEM_ERROR));
        assert_eq!(state.mode, Mode::Mem);
        state.reset();
        assert_eq!(state.mode, Mode::Head);
        state.bits = u32::MAX;
        state.last = true;
        state.mode = Mode::Type;
        assert_eq!(state.data_type(), i32::MAX);
    }

    #[test]
    fn reset2_releases_only_when_window_bits_change() {
        let recorder = RecordingAllocator::new(0xa5);
        let mut state = InflateState::new(InflateConfig::new(-8), &recorder).unwrap();
        state.ensure_window().unwrap();
        assert_eq!(recorder.len(), 1);
        state.reset2(InflateConfig::new(-8)).unwrap();
        assert_eq!(recorder.len(), 1);
        assert!(state.window.is_owned());
        state.reset2(InflateConfig::new(-9)).unwrap();
        assert_eq!(recorder.len(), 2);
        assert_eq!(recorder.event(1), Some(Event::Free(256)));
        assert!(state.window.is_absent());
    }
}
