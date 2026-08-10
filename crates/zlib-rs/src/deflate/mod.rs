//! The DEFLATE compressor: the public half of `deflate.c`, and the root of the
//! `deflate` module tree.
//!
//! This module implements the seventeen exported entry points of `deflate.c` --
//! `deflate` itself (L981-L1292) together with `deflateInit_`, `deflateInit2_`,
//! `deflateEnd`, `deflateSetDictionary`, `deflateGetDictionary`, `deflateCopy`,
//! `deflateReset`, `deflateResetKeep`, `deflateParams`, `deflateTune`, `deflateBound`,
//! `deflateBound_z`, `deflatePending`, `deflateUsed`, `deflatePrime` and
//! `deflateSetHeader` -- plus the three `local` helpers that only they use:
//! `deflateStateCheck` (L538-L557), `lm_init` (L682-L702) and the `RANK`-based
//! duplicate-flush guard.
//!
//! # This is plain, safe Rust
//!
//! Nothing here is `#[no_mangle]`, `extern "C"` or `#[repr(C)]`, and nothing here
//! touches a raw pointer. The crate root carries `#![forbid(unsafe_code)]`, which the
//! compiler enforces. The exported C symbols live one layer up, in
//! `crates/libz-rs-sys/src/deflate.rs`, which converts the caller's `z_stream`
//! into the values these functions take and converts the [`ReturnCode`]s back into `int`s. Three
//! things C does inside these functions are therefore *not* done here, and each is
//! called out where it belongs:
//!
//! * the `ZLIB_VERSION` and `sizeof(z_stream)` comparison at the head of
//!   `deflateInit2_` (L394-L397), which inspects the *caller's* compile-time constants;
//! * the null-pointer tests -- `strm == Z_NULL`, `strm->zalloc == 0`,
//!   `strm->zfree == 0`, `strm->state == Z_NULL`, `next_out == Z_NULL`,
//!   `dictionary == Z_NULL` -- and the `s->strm != strm` owner-identity test, all of
//!   which are pointer facts (AAP §0.6.1 category 3);
//! * freeing the state object itself and clearing `strm->state` in `deflateEnd`
//!   (L1306-L1307), since the facade is what allocated the object.
//!
//! # The C sources stay in the tree
//!
//! `deflate.c`, `deflate.h`, `zlib.h`, `zutil.h` and `zlib.map` are read-only
//! references. They remain in the repository as the differential oracle that
//! `crates/zlib-rs-differential` compiles and compares this implementation against, byte
//! for byte, across the whole level x `windowBits` x `memLevel` x strategy x flush
//! matrix.
//!
//! # Byte identity is the governing constraint
//!
//! Five things in this file are part of the observable byte stream or of a contract
//! callers size buffers with, and none of them may be "improved":
//!
//! 1. **The RFC 1950 header word** (L1031-L1046): the `CMF`/`FLG` pair, the `FLEVEL`
//!    bits derived from the level and strategy, the `FDICT` flag, and the mod-31
//!    correction.
//! 2. **The RFC 1952 gzip header** (L1066-L1208): the magic, `FLG`, `MTIME`, `XFL`, `OS`
//!    and the four optional fields, emitted through a *resumable* state machine so that a
//!    caller with a one-byte output buffer still makes progress.
//! 3. **The per-flush-mode block terminators** (L1238-L1254): `_tr_align` for
//!    `Z_PARTIAL_FLUSH`, an empty stored block for `Z_SYNC_FLUSH` and `Z_FULL_FLUSH`,
//!    nothing at all for `Z_BLOCK`.
//! 4. **The strategy-dispatch precedence** (L1217-L1220): level 0 first, then
//!    `Z_HUFFMAN_ONLY`, then `Z_RLE`, then the level's table entry.
//! 5. **[`deflate_bound_z`]'s arithmetic** (L856-L927), which callers use to size output
//!    buffers -- a bound that is too small is a buffer overflow in *caller* code.
//!
//! # No panics
//!
//! There is no `unwrap()`, no `expect()` and no panicking index outside `#[cfg(test)]`.
//! C's `Assert` becomes `debug_assert!` and C's `ERR_RETURN` becomes a [`ReturnCode`]
//! return that records the message, through [`ReturnCode::record_msg`]. Where C relies on
//! unsigned wraparound to make an expression well defined, this implementation uses a saturating
//! or checked operation that agrees with C on every reachable input and fails closed
//! elsewhere; each such place says so.
//!
//! [`deflate_bound_z`]: crate::deflate::deflate_bound_z

// The definitions closing the module documentation above are link-reference definitions, not
// prose: a module whose `mod` declaration carries an outer doc comment has its `//!` block
// resolved in the scope of the DECLARING module, so an unqualified sibling name does not
// resolve. See "Documentation lints" in `src/lib.rs` for the rule and the gate.

// Every item in this module implements a C function called `deflate*`, and the module
// is called `deflate`, so `clippy::module_name_repetitions` fires on nearly all of them.
// The C names are the API contract -- `crates/libz-rs-sys` exports them verbatim -- so
// the names are kept and the lint is relaxed for this file only, exactly as
// `inflate/mod.rs`, `deflate/state.rs` and `config.rs` do.
#![allow(clippy::module_name_repetitions)]
// The six `_tr_*` names are the C spellings of `deflate.h` L311-L318 and are kept for
// oracle traceability, so calling them trips `clippy::used_underscore_items`. The same
// relaxation, for the same reason, appears in `trees/mod.rs` and `deflate/algorithm.rs`.
// MSRV guard: `unknown_lints` comes first because `clippy::used_underscore_items` postdates the
// declared 1.80 floor, where the lint NAME is itself an `unknown_lints` error under `-D warnings`.
// Allowing `unknown_lints` in the same list makes the attribute inert on 1.80 and effective on
// current stable. Do not drop it while the floor is 1.80.
#![allow(unknown_lints, clippy::used_underscore_items)]

// `deflate_state` itself is reachable from `trees/**`, which manipulates the same struct
// in C (`trees.c`'s only `#include` is `"deflate.h"`), so this one submodule is `pub`.
pub mod state;

// Everything these five hold is `local` in C, and the sixth -- `algorithm` -- holds the
// five compressors plus the `FLUSH_BLOCK` machinery, all of it `local` as well. None of
// it becomes part of the crate's public API, mirroring the `local:` block of `zlib.map`.
//
// `mod algorithm;` resolves to the sibling file `deflate/algorithm.rs`, whose own
// submodules live in the `algorithm/` directory. There is deliberately no
// `algorithm/mod.rs`: rustc rejects a module found at both paths (E0761).
pub(crate) mod algorithm;
pub(crate) mod config_table;
pub(crate) mod hash_chain;
pub(crate) mod longest_match;
pub(crate) mod pending;
pub(crate) mod window;

// `crate::deflate::Flush` is the single canonical path to the flush enumeration: the
// one-shot wrappers in `crate::compress`, the gzip write layer in `crate::gz::write` and
// the facade in `crates/libz-rs-sys/src/deflate.rs` all reach it through here. There is
// no second flush type in this module.
pub use crate::deflate::algorithm::Flush;
// `CONFIGURATION_TABLE` must keep exactly this identifier: `crate::lib` re-exports that
// name, and `crates/zlib-rs-differential/tests/table_equality.rs` compares it
// element-for-element against the C array under that name.
pub use crate::deflate::config_table::{Config, CONFIGURATION_TABLE};
pub use crate::deflate::state::{DeflateState, Status};

// Named so that the driver's post-block handling reads like `deflate.c` L1222-L1238.
pub(crate) use crate::deflate::algorithm::BlockState;

use crate::adler32::{adler32, ADLER32_INITIAL_VALUE};
use crate::config::{
    validate_deflate_flush, validate_deflate_params_change, DeflateConfig, Z_BLOCK, Z_DEFLATED,
    Z_HUFFMAN_ONLY, Z_UNKNOWN,
};
use crate::crc32::{crc32, crc32_z};
use crate::deflate::algorithm::{rank_of_i32, CompressFunc, StreamCursors};
use crate::deflate::config_table::config_for_level;
use crate::deflate::hash_chain::{clear_hash, insert_string_no_head};
use crate::deflate::pending::{
    deflate_pending as pending_snapshot, deflate_used as used_bits, flush_pending, has_prime_room,
    put_byte, put_short_msb,
};
use crate::deflate::state::{Allocator, GzHeaderView, BUF_SIZE, MIN_MATCH, PRESET_DICT};
use crate::deflate::window::{fill_window, slide_hash};
use crate::error::ReturnCode;
use crate::read_buf::{InputCursor, OutputCursor, OutputRegion};
use crate::trees::{_tr_align, _tr_flush_bits, _tr_init, _tr_stored_block};

/// The `OS` byte of the gzip header: which operating system produced the stream.
///
/// Mirrors `OS_CODE` (`zutil.h` L98-L189), which the reference implementation resolves
/// with a ladder of `#if`s over the *compiler's* platform macros and then defaults to
/// `3`, "assume Unix" (L187-L189). `deflate.c` L1081 emits it verbatim for a gzip stream
/// with no caller-supplied header; a caller that does supply one provides its own `os`
/// field instead (L1105), so this constant is used on exactly one path.
///
/// It is defined here rather than taken from a dependency because no module in this
/// crate's dependency set carries it, and because it is only ever needed at the one
/// emission site below.
///
/// Only the values reachable on a Tier-1 Rust target are reproduced; the AAP places the
/// Amiga, VMS, Atari, OS/2, classic `MacOS`, RISC OS, `BeOS` and OS/400 build scripts out of
/// scope, so their codes (1, 2, 5, 6, 7, 13, 16 and 18) have no `cfg` here. The value is
/// **not** part of the byte-identity contract in the way the header layout is: a stream
/// is compared against a C oracle built for the same target, and both then report the
/// same number.
///
/// `WIN32 && !__CYGWIN__` (`zutil.h` L156-L158). Rust's Cygwin targets carry
/// `target_os = "cygwin"`, not `"windows"`, so this `cfg` excludes them exactly as C's
/// `!__CYGWIN__` does, and they fall through to the Unix default.
#[cfg(target_os = "windows")]
pub const OS_CODE: u8 = 10;

/// `__APPLE__` (`zutil.h` L168-L170).
#[cfg(all(not(target_os = "windows"), target_vendor = "apple"))]
pub const OS_CODE: u8 = 19;

/// The default, "assume Unix" (`zutil.h` L187-L189).
#[cfg(all(not(target_os = "windows"), not(target_vendor = "apple")))]
pub const OS_CODE: u8 = 3;

/// The parts of the caller's `z_stream` that the compressor reads and writes.
///
/// C reaches all of this through `s->strm`, the back-pointer at `deflate.h` L105. A
/// self-referential raw pointer has no safe Rust equivalent, so `deflate/state.rs`
/// deliberately omits it and the stream travels as its own value alongside the
/// `&mut DeflateState`. This is the deflate counterpart of
/// [`crate::inflate::InflateStream`], and it exists for the same reason.
///
/// # The buffers are slices plus indices
///
/// `z_stream` splits each buffer across two members (`zlib.h` L91-L95).
///
/// Here each becomes the *whole* buffer plus how far into it the stream has got.
/// `avail_in` is [`Self::avail_in`] and `avail_out` is [`Self::avail_out`]. The whole
/// buffer is kept, not just the unread tail, because `deflate_stored` rebuilds the
/// window's history by reading *backwards* from `next_in` (`deflate.c` L1766 and L1780)
/// and because output already written must stay reachable.
///
/// A C caller may legally pass a null `next_in` when `avail_in` is zero; that is an
/// empty `input` slice here. A null `next_out` is an error the facade rejects before it
/// can build one of these (`deflate.c` L990).
///
/// # Feeding a stream incrementally
///
/// ★ Because each buffer is the *whole* buffer, a caller that hands the compressor its
/// input a chunk at a time must **grow the visible slice**, never shrink an available
/// count. To expose `chunk` more bytes, widen `input` and leave `next_in` where the
/// previous call left it:
///
/// ```text
/// let limit = (stream.next_in + chunk).min(whole_input.len());
/// stream.input = &whole_input[..limit];
/// ```
///
/// and symmetrically for the output, widening `output` to
/// `&mut whole_output[..(stream.next_out + chunk).min(whole_output.len())]`.
///
/// Re-slicing from `next_in` instead -- the obvious `&whole_input[next_in..]` translation
/// of C's advancing `next_in` pointer -- is **wrong here**, because it discards the
/// already-consumed prefix that `deflate_stored` reads backwards through
/// (`deflate.c` L1766 and L1780) and it desynchronises `next_in` from the slice it
/// indexes. The same applies to the output: `deflate_stored` copies directly into the
/// caller's buffer and `flush_pending` writes at `next_out`, so the prefix must stay
/// addressable.
///
/// One consequence is worth stating because it is a property of zlib and not of this
/// implementation: when a mid-stream flush is pending, a caller offering only a handful of output
/// bytes makes no forward progress -- `deflate` re-emits the flush marker on every call.
/// `zlib.h` L326-L328 documents the contract that avoids it: keep calling with more
/// output room until `avail_out` comes back non-zero, which is what tells the caller the
/// flush has actually drained. The C implementation behaves identically.
///
/// # The scalars
///
/// `total_in` and `total_out` are `u64` because C declares them `uLong`, which is 64 bits
/// on every LP64 target; the facade converts to and from `c_ulong`, and on an LLP64
/// target that conversion truncates in exactly the places C's own 32-bit `unsigned long`
/// wraps. `msg` is an `Option<&'static str>` because every message the compressor can
/// produce is a string literal, so the facade can publish one as a `const char *` with no
/// allocation, and [`None`] is the faithful spelling of C's `Z_NULL`.
#[derive(Debug)]
pub struct DeflateStream<'i, 'o> {
    /// The whole input buffer; `z_stream.next_in` addressed its `next_in`-th byte.
    pub input: &'i [u8],
    /// How far into [`Self::input`] the stream has read. C's `next_in` advance.
    pub next_in: usize,
    /// The whole output buffer; `z_stream.next_out` addressed its `next_out`-th byte.
    ///
    /// An [`OutputRegion`] rather than a `&mut [u8]` because a C caller's `next_out` is
    /// only guaranteed *writable*: `zlib.h` L94-L95 promises `avail_out` bytes of room and
    /// says nothing about their contents, and Rust does not allow a byte slice to address a
    /// byte that holds no value. [`OutputRegion::init`] is the shape a Rust caller has and
    /// is what [`DeflateStream::new`] builds; the facade supplies
    /// [`OutputRegion::write_only`] through [`DeflateStream::with_region`]. The compressor
    /// itself never reads the output back, so the distinction costs it nothing.
    pub output: OutputRegion<'o>,
    /// How far into [`Self::output`] the stream has written. C's `next_out` advance.
    pub next_out: usize,
    /// `z_stream.total_in`: total input bytes consumed so far (`zlib.h` L93).
    pub total_in: u64,
    /// `z_stream.total_out`: total output bytes produced so far (`zlib.h` L96).
    pub total_out: u64,
    /// `z_stream.msg`: the last error message, or [`None`] for C's `Z_NULL`
    /// (`zlib.h` L98).
    pub msg: Option<&'static str>,
    /// `z_stream.adler`: the running Adler-32 or CRC-32 of the input read so far
    /// (`zlib.h` L106 and L345-L348).
    pub adler: u32,
    /// `z_stream.data_type`: `Z_BINARY`, `Z_TEXT` or `Z_UNKNOWN` (`zlib.h` L105).
    ///
    /// Informational only, and never read by the encoder: `_tr_flush_block` fills it in
    /// on a stream's first block through `detect_data_type`, in `crate::trees`
    /// (`deflate.c` L1005-L1007 and `zlib.h` L350-L353).
    pub data_type: i32,
}

impl<'i, 'o> DeflateStream<'i, 'o> {
    /// Builds a stream over `input` and `output` with both cursors at zero and every
    /// scalar at the value a freshly zeroed `z_stream` carries.
    ///
    /// A caller resuming a stream must restore `total_in`, `total_out`, `adler` and
    /// `data_type` itself; they are public fields for exactly that reason. Note in
    /// particular that `adler` starts at zero here, whereas [`deflate_reset`] seeds it to
    /// `1` for a zlib stream -- so a stream that is about to be handed to [`deflate`] must
    /// take its scalars from the [`DeflateReset`] that initialisation produced.
    #[must_use]
    pub fn new(input: &'i [u8], output: &'o mut [u8]) -> Self {
        Self::with_region(input, OutputRegion::init(output))
    }

    /// Builds a stream whose output is `region`, with both cursors at zero and every scalar
    /// at the value a freshly zeroed `z_stream` carries.
    ///
    /// The entry point `crates/libz-rs-sys` uses, because the region it has is
    /// [`OutputRegion::write_only`]: the `avail_out` bytes at a C caller's `next_out` are
    /// writable but not initialised. Everything [`DeflateStream::new`]'s documentation says
    /// about restoring the scalars applies here unchanged.
    #[must_use]
    pub fn with_region(input: &'i [u8], region: OutputRegion<'o>) -> Self {
        Self {
            input,
            next_in: 0,
            output: region,
            next_out: 0,
            total_in: 0,
            total_out: 0,
            msg: None,
            adler: 0,
            data_type: 0,
        }
    }

    /// `z_stream.avail_in`: input bytes not yet consumed (`zlib.h` L92).
    #[must_use]
    pub fn avail_in(&self) -> usize {
        self.input.len().saturating_sub(self.next_in)
    }

    /// `z_stream.avail_out`: output space not yet written (`zlib.h` L95).
    #[must_use]
    pub fn avail_out(&self) -> usize {
        self.output.len().saturating_sub(self.next_out)
    }

    /// The output bytes this stream has produced, that is `output[..next_out]`.
    #[must_use]
    pub fn written(&self) -> &[u8] {
        self.output.initialized(self.next_out)
    }

    /// Applies the `z_stream` half of a reset (`deflate.c` L651-L671).
    ///
    /// [`deflate_reset_keep`] and [`deflate_reset`] return the five values C assigns to
    /// the caller's stream; this puts them in place. Unlike `inflateResetKeep`, the
    /// `adler` seed is unconditional: `deflateResetKeep` assigns it for every container,
    /// choosing `crc32(0, Z_NULL, 0)` for gzip and `adler32(0, Z_NULL, 0)` otherwise
    /// (L667-L671).
    pub fn apply_reset(&mut self, reset: DeflateReset) {
        self.total_in = reset.total_in;
        self.total_out = reset.total_out;
        self.msg = reset.msg;
        self.data_type = reset.data_type;
        self.adler = reset.adler;
    }
}

/// The `z_stream` half of `deflateResetKeep` (`deflate.c` L651-L671).
///
/// A reset touches both the compression state and the caller's stream. The state half is
/// [`DeflateState::reset_keep`] plus `lm_init`; this carries the five stream fields out
/// so that a caller which owns a `z_stream` -- or a [`DeflateStream`], through
/// [`DeflateStream::apply_reset`] -- can apply them without this module needing the
/// caller's buffers just to perform a reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeflateReset {
    /// `strm->total_in = 0` (`deflate.c` L651).
    pub total_in: u64,
    /// `strm->total_out = 0` (`deflate.c` L651).
    pub total_out: u64,
    /// `strm->msg = Z_NULL` (`deflate.c` L652).
    pub msg: Option<&'static str>,
    /// `strm->data_type = Z_UNKNOWN` (`deflate.c` L653).
    pub data_type: i32,
    /// The check-value seed: `crc32(0, Z_NULL, 0)` for a gzip stream, otherwise
    /// `adler32(0, Z_NULL, 0)` (`deflate.c` L667-L671). Those are `0` and `1`
    /// respectively, so a zlib stream reports `adler == 1` before a byte is compressed.
    pub adler: u32,
}

/// The status-validity half of `deflateStateCheck` (`deflate.c` L538-L556).
///
/// C's function rejects a stream for any of six reasons:
///
/// | C test | `deflate.c` | Where it lives |
/// |---|---|---|
/// | `strm == Z_NULL` | L540 | the facade |
/// | `strm->zalloc == 0 \|\| strm->zfree == 0` | L541 | the facade |
/// | `s == Z_NULL` | L544 | the facade |
/// | `s->strm != strm` | L544 | the facade |
/// | `s->status` is none of the eight legal values | L544-L553 | **here** |
///
/// The first three are pointer and function-pointer facts, and exist only in
/// `crates/libz-rs-sys`. The fourth is the owner-identity test, and it **cannot** be
/// ported here: `deflate/state.rs` deliberately omits the `strm` back-pointer, because a
/// self-referential raw pointer has no safe Rust equivalent. Its purpose -- rejecting a
/// state that belongs to some *other* stream, or to a stream that was freed, or to memory
/// that was never a stream at all -- is assigned to the facade by AAP §0.6.1 category 3:
/// the opaque `state` pointer must be shown to have come from this library's own
/// allocation path, by a tag check, before it is treated as state. **The check is not
/// dropped; it moves.** `crates/libz-rs-sys/src/deflate.rs` carries the allocator-non-null
/// and pointer-provenance halves, and this function is the rest.
///
/// # Why the remaining half is trivially satisfied inside the library
///
/// [`Status`] is an enum, so a `&DeflateState` is already proof that its status is one of
/// the eight values `{42, 57, 69, 73, 91, 103, 113, 666}`; an invalid status is not
/// representable. The check is therefore needed only where a *raw* status arrives, which
/// is why this takes an `i32` rather than a state reference -- the same shape as
/// [`crate::inflate::inflate_state_check`].
///
/// Returns `true` when the state must be **rejected**, matching the polarity of C's
/// non-zero return so that the two read the same way side by side.
#[must_use]
pub const fn deflate_state_check(status_raw: i32) -> bool {
    Status::from_raw(status_raw).is_none()
}

/// Creates a compression state for a full parameter set, ready to compress.
///
/// The core half of `deflateInit2_` (`deflate.c` L387-L533), declared at `zlib.h` L1908.
/// C's steps map as follows:
///
/// | C | `deflate.c` | Here |
/// |---|---|---|
/// | `version[0] != my_version[0] \|\| stream_size != sizeof(z_stream)` | L394-L397 | the facade |
/// | `strm == Z_NULL` | L398 | the facade |
/// | `strm->msg = Z_NULL` | L400 | the facade, or [`DeflateReset::msg`] |
/// | defaulting a null `zalloc`/`zfree` to `zcalloc`/`zcfree` | L401-L414 | the facade, through `crate::allocate` |
/// | `level == Z_DEFAULT_COMPRESSION` becomes 6 | L419 | [`DeflateConfig::validate`] |
/// | `windowBits < 0` sets `wrap = 0`, rejects below -15, negates | L422-L427 | `crate::config::decode_deflate_window_bits` |
/// | `windowBits > 15` sets `wrap = 2` and subtracts 16 | L429-L432 | the same |
/// | the five-way parameter rejection | L434-L438 | the same |
/// | `windowBits == 8` becomes 9, "until 256-byte window bug fixed" | L439 | the same |
/// | the state allocation, the four buffers and every field assignment | L440-L530 | [`DeflateState::with_validated_config`] |
/// | `return deflateReset(strm)` | L532 | [`deflate_reset`], called below |
///
/// The argument transformations at L419-L432 all happen *before* the validation at
/// L434-L438, and `crate::config` performs them in that order; this function does not
/// duplicate that logic, which is why it takes a [`DeflateConfig`] rather than five
/// integers.
///
/// # ★ The `z_stream` half of the reset C performs at L532
///
/// `deflateInit2_` finishes by calling `deflateReset`, so that function's effects on the
/// *caller's* `z_stream` are part of `deflateInit2_`'s observable contract: `total_in`,
/// `total_out` go to zero, `msg` to `Z_NULL`, `data_type` to `Z_UNKNOWN`, and -- the
/// easily missed one -- `adler` to `1` for a zlib or raw stream and `0` for a gzip one
/// (L651-L671). A freshly zeroed `z_stream` has `adler == 0`, so **omitting this leaves a
/// zlib stream reporting the wrong check value before any input is read**, which
/// `test/example.c` observes.
///
/// This returns only the state, matching [`crate::inflate::inflate_init2`]. A facade that
/// owns a `z_stream` obtains the stream half by calling [`deflate_reset_snapshot`] on the
/// value returned here, which computes those five fields and changes nothing.
///
/// Calling [`deflate_reset`] a second time would also produce them, and is sound -- the reset
/// is idempotent on a state that has just come out of this function -- but it repeats
/// `_tr_init` and the whole of `lm_init`, whose `CLEAR_HASH` writes 64 KiB at the default
/// `memLevel` and 128 KiB at `memLevel 9`, to arrive at values that are already there. The
/// snapshot exists so that the initialisation reset happens exactly once.
///
/// # Errors
///
/// * [`ReturnCode::STREAM_ERROR`] if `config` names an invalid combination of level,
///   method, `windowBits`, `memLevel` and strategy (`deflate.c` L434-L438).
/// * [`ReturnCode::MEM_ERROR`] if any of the four buffers cannot be allocated
///   (L508-L514). Nothing is leaked: blocks obtained before the failure are returned to
///   the same allocator, in `deflateEnd`'s order.
pub fn deflate_init2<'a, A: Allocator<'a> + Copy>(
    config: DeflateConfig,
    allocator: A,
) -> Result<DeflateState<'a, A>, ReturnCode> {
    let mut state = DeflateState::new(config, allocator)?;

    // `return deflateReset(strm);`  (L532). The state is not usable before this: it is
    // `DeflateState::with_validated_config` that stops where C's field assignments stop,
    // and `CLEAR_HASH` plus the tuning-parameter load happen only in `lm_init`.
    let _reset = deflate_reset(&mut state);

    Ok(state)
}

/// Creates a compression state with the reference defaults.
///
/// The Rust counterpart of `deflateInit_` (`deflate.c` L379-L384), declared at `zlib.h` L1907, which
/// is [`deflate_init2`] with `method` = `Z_DEFLATED`, `windowBits` = `MAX_WBITS` (15),
/// `memLevel` = `DEF_MEM_LEVEL` (8) and `strategy` = `Z_DEFAULT_STRATEGY` -- exactly the
/// combination [`DeflateConfig::new`] builds.
///
/// # Errors
///
/// [`ReturnCode::STREAM_ERROR`] for a `level` outside `0..=9` once
/// `Z_DEFAULT_COMPRESSION` has been resolved, or [`ReturnCode::MEM_ERROR`]; see
/// [`deflate_init2`].
pub fn deflate_init<'a, A: Allocator<'a> + Copy>(
    level: i32,
    allocator: A,
) -> Result<DeflateState<'a, A>, ReturnCode> {
    deflate_init2(DeflateConfig::new(level), allocator)
}

/// Resets the stream without discarding the window's contents or the hash chains.
///
/// The Rust counterpart of `deflateResetKeep` (`deflate.c` L644-L677), declared at `zlib.h` L730. In
/// C's order:
///
/// | C | `deflate.c` | Here |
/// |---|---|---|
/// | `strm->total_in = strm->total_out = 0` | L651 | [`DeflateReset::total_in`], [`DeflateReset::total_out`] |
/// | `strm->msg = Z_NULL` | L652 | [`DeflateReset::msg`] |
/// | `strm->data_type = Z_UNKNOWN` | L653 | [`DeflateReset::data_type`] |
/// | `s->pending = 0; s->pending_out = s->pending_buf` | L656-L657 | [`DeflateState::reset_keep`] |
/// | `if (s->wrap < 0) s->wrap = -s->wrap` | L659-L661 | the same |
/// | `s->status = s->wrap == 2 ? GZIP_STATE : INIT_STATE` | L662-L666 | the same |
/// | `strm->adler = s->wrap == 2 ? crc32(0,0,0) : adler32(0,0,0)` | L667-L671 | [`DeflateReset::adler`] |
/// | `s->last_flush = -2` | L672 | [`DeflateState::reset_keep`] |
/// | `_tr_init(s)` | L674 | [`crate::trees`], called below |
///
/// ★ The un-negation of `wrap` at L659-L661 is what makes a stream reusable after a
/// `Z_FINISH`: [`deflate`] negates `wrap` once the trailer has been written, "write the
/// trailer only once!" (L1288), and this restores it. Treating `wrap` as unsigned
/// anywhere would break repeated `Z_FINISH` calls.
///
/// The status half of `deflateStateCheck` (L647) has no analogue: a `&mut DeflateState` is
/// already proof that the status is one of the eight legal values. See
/// [`deflate_state_check`].
pub fn deflate_reset_keep<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>) -> DeflateReset {
    // L656-L672, all four of them: the pending cursors, the `wrap` un-negation, the
    // status, and the `-2` flush sentinel.
    state.reset_keep();

    // `_tr_init(s);`  (L674)
    _tr_init(state);

    // L651-L653 and L667-L671. `wrap` has already been un-negated above, so the choice of
    // check function sees the same value C's does -- the assignment at L667 follows the
    // un-negation at L660 for exactly that reason, and taking the snapshot *here*, after
    // `reset_keep`, is what preserves that order.
    deflate_reset_snapshot(state)
}

/// The five `z_stream` values a completed reset leaves behind, computed without changing anything.
///
/// [`deflate_reset_keep`] ends by calling this, so there is exactly one definition of what a reset
/// puts in a caller's stream and the two cannot drift apart.
///
/// # Why this exists separately
///
/// [`deflate_init2`] already performs a full [`deflate_reset`] -- it is C's `return deflateReset(strm)`
/// at `deflate.c` L532 -- but it cannot return the stream half, because it has no `z_stream` to apply
/// it to. A facade that owns one therefore needs the *value*, and the obvious way to get it is to call
/// `deflate_reset` a second time.
///
/// That second call is not free. It repeats `_tr_init`, the pending-cursor and status assignments, and
/// all of `lm_init` -- including `CLEAR_HASH`, which writes `hash_size` entries of two bytes each: at
/// the default `memLevel` of 8 that is 64 KiB cleared twice for every `deflateInit_`, and `memLevel 9`
/// makes it 128 KiB. None of it changes a single value, because a reset applied to a state that has
/// just been reset is idempotent, so all of it is waste on a path callers measure.
///
/// This function is the alternative: it reads `state.wrap` to choose the check seed and touches
/// nothing at all, so the initialisation reset happens exactly once.
///
/// # Note the ordering requirement
///
/// The seed depends on `wrap`, and `deflate` negates `wrap` after writing a trailer -- "write the
/// trailer only once!" (L1288). So this must be called on a state whose `wrap` is already in its
/// post-reset, un-negated form. That holds immediately after [`deflate_init2`], and it holds inside
/// [`deflate_reset_keep`] because `reset_keep` un-negates first. It would **not** hold if this were
/// called on a finished stream before its reset, which is why nothing here tries to be clever about
/// the sign.
#[must_use]
pub fn deflate_reset_snapshot<'a, A: Allocator<'a>>(state: &DeflateState<'a, A>) -> DeflateReset {
    DeflateReset {
        total_in: 0,
        total_out: 0,
        msg: None,
        data_type: Z_UNKNOWN,
        adler: if state.wrap == 2 {
            // `crc32(0L, Z_NULL, 0)` is 0, and `crc32(0, &[])` is 0 as well, so the two
            // spellings agree.
            crc32(0, &[])
        } else {
            // ★ `adler32(0L, Z_NULL, 0)` is **1**, not 0: `adler32_z` returns `1L`
            // outright for a null pointer (`adler32.c` L81-L82), which is RFC 1950's
            // `s1 = 1, s2 = 0`. `adler32(0, &[])` would return 0, because an empty slice
            // is an ordinary zero-length update and not `Z_NULL`, so the named constant is
            // the only correct spelling here.
            ADLER32_INITIAL_VALUE
        },
    }
}

/// Initialises the "longest match" routines for a new stream.
///
/// The Rust counterpart of `lm_init` (`deflate.c` L682-L701), reached only from [`deflate_reset`].
///
/// `s->window_size = (ulg)2L * s->w_size;` (L683) has no assignment here: the window view
/// derives its size from `w_bits` when it is constructed, so it is already `2 * w_size`
/// and nothing can have changed it. The `debug_assert!` below states that equivalence
/// where a reader of L683 will look for it.
///
/// The four tuning parameters come from `CONFIGURATION_TABLE[s->level]` (L689-L692) and
/// are byte-identity critical: together they decide which candidate matches
/// `longest_match` examines and which it accepts. `match_length` and `prev_length` both
/// start at `MIN_MATCH - 1`, i.e. 2 (L698).
pub(crate) fn lm_init<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>) {
    // `s->window_size = (ulg)2L*s->w_size;`  (L683)
    debug_assert_eq!(
        state.window_size(),
        2 * state.w_size(),
        "window_size is not 2 * w_size (deflate.c L683)"
    );

    // `CLEAR_HASH(s);`  (L685)
    clear_hash(state);

    // `s->max_lazy_match   = configuration_table[s->level].max_lazy;`   (L689)
    // `s->good_match       = configuration_table[s->level].good_length;` (L690)
    // `s->nice_match       = configuration_table[s->level].nice_length;` (L691)
    // `s->max_chain_length = configuration_table[s->level].max_chain;`   (L692)
    let config = config_for_level(state.level);
    state.set_tuning(
        usize::from(config.good_length),
        usize::from(config.max_lazy),
        i32::from(config.nice_length),
        usize::from(config.max_chain),
    );

    // L694-L700.
    state.window.strstart = 0;
    state.window.block_start = 0;
    state.window.lookahead = 0;
    state.window.insert = 0;
    state.match_length = MIN_MATCH - 1;
    state.prev_length = MIN_MATCH - 1;
    state.match_available = false;
    state.ins_h = 0;
}

/// Resets the stream completely, discarding the window's history.
///
/// The Rust counterpart of `deflateReset` (`deflate.c` L704-L711), declared at `zlib.h` L718.
///
/// C guards `lm_init` on the return value because `deflateResetKeep` can fail its state
/// check; here it cannot, so `lm_init` runs unconditionally and there is nothing to
/// propagate but the [`DeflateReset`].
pub fn deflate_reset<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>) -> DeflateReset {
    let reset = deflate_reset_keep(state);
    lm_init(state);
    reset
}

/// Installs the gzip header the compressor will emit, or clears it.
///
/// The Rust counterpart of `deflateSetHeader` (`deflate.c` L714-L719), declared at `zlib.h` L833. The
/// whole of its effect is `strm->state->gzhead = head;` (L717); the guard is
/// `deflateStateCheck(strm) || strm->state->wrap != 2` (L715), whose state-check half is
/// the facade's (see [`deflate_state_check`]) and whose `wrap` half is here.
///
/// The header is **caller-owned**: `zlib.h` L838-L855 requires the `gz_header` and every
/// string it points at to stay alive and unmodified until the header has been written,
/// which the `'a` lifetime of [`GzHeaderView`] expresses. Passing [`None`] reproduces
/// `deflateSetHeader(strm, Z_NULL)`, which C accepts and which makes the compressor emit
/// the ten-byte default header instead.
///
/// # Errors
///
/// [`ReturnCode::STREAM_ERROR`] unless the stream was initialised with a `windowBits`
/// that requested the gzip container, i.e. `wrap == 2`.
pub fn deflate_set_header<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    head: Option<GzHeaderView<'a>>,
) -> ReturnCode {
    // `strm->state->wrap != 2`  (L715). Spelled through the state's own predicate so
    // that the facade -- which must apply it *before* it reads the caller's
    // `gz_header` at all -- and this entry point can never disagree about the
    // condition.
    if !state.accepts_gzip_header() {
        return ReturnCode::STREAM_ERROR;
    }

    // `strm->state->gzhead = head;`  (L717)
    state.set_gzhead(head);
    ReturnCode::OK
}

/// C's `(uInt)` conversion of a possibly negative `int`, widened to the `usize` this implementation
/// stores such fields in.
///
/// `deflateTune` casts each of its three unsigned arguments with `(uInt)` (`deflate.c`
/// L825-L828) and performs **no** range validation, so a negative argument becomes a very
/// large `uInt` there. This reproduces that modular conversion exactly rather than
/// rejecting or clamping, because rejecting would add an error C does not have and
/// clamping would choose a different number.
// The sign loss is the point: it is what C's cast does.
#[allow(clippy::cast_sign_loss)]
const fn as_uint(value: i32) -> usize {
    value as u32 as usize
}

/// Overrides the four match-finding tuning parameters.
///
/// The Rust counterpart of `deflateTune` (`deflate.c` L819-L830), declared at `zlib.h` L751. All four
/// assignments are unconditional (L825-L828), and C validates **nothing**: `zlib.h`
/// L756-L766 describes the function as being for "the most fanatic optimizer", and out of
/// range values simply produce a poor or a very slow search. No validation is added here,
/// because adding it would reject calls the reference accepts.
///
/// `good_length`, `max_lazy` and `max_chain` go through C's `(uInt)` cast, reproduced by
/// `as_uint`; `nice_length` is assigned to an `int` field with no cast at all (L827) and
/// so passes straight through, negative values included.
///
/// # Errors
///
/// None once a valid state is in hand: C's only failure is the `deflateStateCheck` at
/// L823, whose remaining half cannot fail for a `&mut DeflateState`. The
/// [`ReturnCode`] is returned so that the facade has one shape for every entry point.
pub fn deflate_tune<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    good_length: i32,
    max_lazy: i32,
    nice_length: i32,
    max_chain: i32,
) -> ReturnCode {
    // L825-L828, in `set_tuning`'s parameter order.
    state.set_tuning(
        as_uint(good_length),
        as_uint(max_lazy),
        nice_length,
        as_uint(max_chain),
    );
    ReturnCode::OK
}

/// Changes the compression level and strategy of a live stream.
///
/// The Rust counterpart of `deflateParams` (`deflate.c` L774-L816), declared at `zlib.h` L723. The
/// order of its five steps is the contract, because step 3 can emit bytes:
///
/// 1. `level == Z_DEFAULT_COMPRESSION` becomes 6, then `level < 0 || level > 9 ||
///    strategy < 0 || strategy > Z_FIXED` is rejected (L784-L788) --
///    `crate::config::validate_deflate_params_change` performs exactly that pair.
/// 2. `func = configuration_table[s->level].func` (L789) -- the compressor of the level
///    the stream is running **now**, captured before anything changes.
/// 3. If the strategy or the compressor would change, *and* `s->last_flush != -2`, flush
///    the open block with `deflate(strm, Z_BLOCK)` (L791-L799). `Z_STREAM_ERROR` is
///    propagated; then, if any input remains unconsumed or any window bytes are still
///    unwritten, [`ReturnCode::BUF_ERROR`] is returned and **nothing else changes**.
/// 4. On a level change, and only when leaving level 0 with matches recorded, slide or
///    clear the hash chains; then install the level and reload the four tuning parameters
///    (L800-L813).
/// 5. `s->strategy = strategy;` unconditionally (L814).
///
/// ★ The `s->last_flush != -2` guard means "only if [`deflate`] has already been called
/// since the last reset": `-2` is the sentinel [`deflate_reset_keep`] installs (L672).
/// Without it, `deflateParams` on a fresh stream would emit an empty `Z_BLOCK` before any
/// data, changing the output.
///
/// ★ The `func != configuration_table[level].func` test at L791 compares two C function
/// *addresses*. This port compares `CompressFunc` discriminants instead, which agrees
/// exactly because the table groups the levels the same way -- `Stored` at 0, `Fast` at 1
/// to 3, `Slow` at 4 to 9. Getting that grouping wrong would make this function flush
/// where C does not, or fail to flush where C does, and either changes the emitted bytes.
///
/// # Errors
///
/// * [`ReturnCode::STREAM_ERROR`] for an out-of-range level or strategy, or propagated
///   from the internal [`deflate`] call.
/// * [`ReturnCode::BUF_ERROR`] if the open block could not be flushed completely,
///   because the output buffer was too small. `zlib.h` L735-L742 documents this and tells
///   the caller to retry with more room.
pub fn deflate_params<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    stream: &mut DeflateStream<'_, '_>,
    level: i32,
    strategy: i32,
) -> ReturnCode {
    // 1. L784-L788.
    let (level, strategy) = match validate_deflate_params_change(level, strategy) {
        Ok(pair) => pair,
        Err(code) => return code,
    };
    let level = i32::from(level);

    // 2. `func = configuration_table[s->level].func;`  (L789)
    let func = config_for_level(state.level).func;

    // 3. L791-L799.
    if (strategy != state.strategy || func != config_for_level(level).func)
        && state.last_flush != -2
    {
        // `int err = deflate(strm, Z_BLOCK);`  (L794)
        let err = deflate(state, stream, Z_BLOCK);

        // `if (err == Z_STREAM_ERROR) return err;`  (L795-L796)
        if err == ReturnCode::STREAM_ERROR {
            return err;
        }

        // `if (strm->avail_in || (s->strstart - s->block_start) + s->lookahead)`  (L797)
        //
        // C evaluates the parenthesised term in `long` arithmetic: `strstart` and
        // `lookahead` are `uInt` and are promoted to `long` to meet the signed
        // `block_start`, which "gets negative when the window is moved backwards"
        // (`deflate.h` L159-L161). `i64` reproduces that on every target, and the
        // saturating operations keep the expression total -- none of the three values can
        // come near a bound in a live stream, where all of them are below 2^17.
        let strstart = i64::try_from(state.window.strstart).unwrap_or(i64::MAX);
        let lookahead = i64::try_from(state.window.lookahead).unwrap_or(i64::MAX);
        let block_start = i64::try_from(state.window.block_start).unwrap_or(i64::MIN);
        let buffered = strstart
            .saturating_sub(block_start)
            .saturating_add(lookahead);

        if stream.avail_in() != 0 || buffered != 0 {
            return ReturnCode::BUF_ERROR;
        }
    }

    // 4. L800-L813.
    if state.level != level {
        // "s->level == 0 && s->matches != 0" -- the stream was storing, so the hash
        // chains hold positions from before the switch and must be made consistent with
        // the window before matching resumes (L801-L807). `matches == 1` means exactly one
        // window slide happened while stored, so sliding the tables once realigns them;
        // anything else and they are discarded.
        if state.level == 0 && state.matches != 0 {
            if state.matches == 1 {
                slide_hash(state);
            } else {
                clear_hash(state);
            }
            state.matches = 0;
        }

        state.level = level;
        let config = config_for_level(level);
        state.set_tuning(
            usize::from(config.good_length),
            usize::from(config.max_lazy),
            i32::from(config.nice_length),
            usize::from(config.max_chain),
        );
    }

    // 5. `s->strategy = strategy;`  (L814)
    state.strategy = strategy;
    ReturnCode::OK
}

/// Primes the window and the hash chains from a preset dictionary.
///
/// The Rust counterpart of `deflateSetDictionary` (`deflate.c` L559-L622), declared at `zlib.h` L618.
/// The eight steps run in exactly C's order, because several of them depend on the state
/// a previous one left behind:
///
/// 1. reject an invalid state, or a null `dictionary` -- the second is the facade's, since
///    a slice cannot be null (L567-L568);
/// 2. `wrap = s->wrap`, then reject `wrap == 2`, a zlib stream that is no longer in
///    `INIT_STATE`, or a non-zero `lookahead` (L570-L572);
/// 3. for a zlib stream only, fold the **whole** dictionary into `strm->adler` (L575-L576)
///    -- `zlib.h` L648-L653 makes that value the dictionary identifier the decompressor
///    checks, and says it covers the whole dictionary "even if only a subset ... is
///    actually used";
/// 4. `s->wrap = 0;` -- "avoid computing Adler-32 in `read_buf`" (L577). RFC 1950 excludes
///    dictionary bytes from the stream's own check value (`doc/rfc1950.txt`), so the window
///    load below must not disturb it;
/// 5. if the dictionary would fill the window, replace the history: clear the hash chains
///    and rewind the cursors, but **only for a raw stream**, since a zlib stream in
///    `INIT_STATE` is empty anyway (L580-L586); then keep the dictionary's **tail**, which
///    is why `zlib.h` L633-L646 tells callers to put the most useful strings last;
/// 6. load that tail into the window and link every position into the hash chains
///    (L591-L611);
/// 7. leave the last `MIN_MATCH - 1` bytes to be inserted lazily, through `insert`, and
///    reset the match bookkeeping (L612-L617);
/// 8. restore `wrap` (L620).
///
/// ★ **`total_in` grows.** Step 6 reaches the dictionary through `fill_window` and
/// therefore through `read_buf`, whose `strm->total_in += len` (L237) is unconditional.
/// The reference implementation was measured to confirm it: after
/// `deflateSetDictionary(strm, "hello", 5)` the stream reports `total_in == 5`, and after
/// a 70000-byte dictionary on a `windowBits` of -15 it reports `32768`, the window size.
/// That is why this function takes `total_in` and not only `adler`.
///
/// Those two are the only `z_stream` fields C touches, so they are passed directly rather
/// than as a whole [`DeflateStream`]: a dictionary load needs neither of the caller's
/// buffers.
///
/// # Errors
///
/// [`ReturnCode::STREAM_ERROR`] for a gzip stream, for a zlib stream on which [`deflate`]
/// has already run, or for a raw stream that is not at a block boundary -- which is the
/// condition `zlib.h` L655-L659 describes.
pub fn deflate_set_dictionary<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    adler: &mut u32,
    total_in: &mut u64,
    dictionary: &[u8],
) -> ReturnCode {
    // 2. `wrap = s->wrap;` and the three rejections (L570-L572). The condition is the
    // state's own predicate, because the facade has to apply it before it reconstructs
    // the caller's dictionary as a slice; keeping one definition is what stops the two
    // from drifting.
    let wrap = state.wrap;
    if !state.accepts_dictionary() {
        return ReturnCode::STREAM_ERROR;
    }

    // 3. `if (wrap == 1) strm->adler = adler32(strm->adler, dictionary, dictLength);`
    //    (L575-L576)
    if wrap == 1 {
        *adler = adler32(*adler, dictionary);
    }

    // 4. `s->wrap = 0;`  (L577)
    state.wrap = 0;

    // 5. `if (dictLength >= s->w_size)`  (L580-L589)
    let w_size = state.w_size();
    let dictionary = if dictionary.len() >= w_size {
        if wrap == 0 {
            // "already empty otherwise" (L581).
            clear_hash(state);
            state.window.strstart = 0;
            state.window.block_start = 0;
            state.window.insert = 0;
        }
        // `dictionary += dictLength - s->w_size;` and `dictLength = s->w_size;`
        // (L587-L588) -- "use the tail". The subtraction cannot underflow inside this
        // branch, and the slice is exactly `w_size` bytes long, so `unwrap_or` names an
        // unreachable case rather than a fallback.
        dictionary
            .get(dictionary.len() - w_size..)
            .unwrap_or(dictionary)
    } else {
        dictionary
    };

    // 6. L591-L611. C swaps `strm->avail_in`/`strm->next_in` to point at the dictionary and
    //    restores them at L618-L619; here the dictionary gets its own cursor, so the
    //    caller's input is not touched at all and there is nothing to restore. `adler` is
    //    still passed through because that is what C passes -- and, `wrap` now being 0,
    //    `read_buf` leaves it alone, which is precisely the effect step 4 bought.
    let mut input = InputCursor::new(dictionary);
    fill_window(state, &mut input, adler, total_in);

    while state.window.lookahead >= MIN_MATCH {
        // `str = s->strstart; n = s->lookahead - (MIN_MATCH-1);`  (L598-L599)
        let mut str_pos = state.window.strstart;
        let mut n = state.window.lookahead - (MIN_MATCH - 1);

        // `do { ... } while (--n);` -- at least one iteration, because `lookahead` is at
        // least `MIN_MATCH` here, so `n` is at least 1 (L600-L607).
        loop {
            insert_string_no_head(state, str_pos);
            str_pos += 1;
            n -= 1;
            if n == 0 {
                break;
            }
        }

        // L608-L610.
        state.window.strstart = str_pos;
        state.window.lookahead = MIN_MATCH - 1;
        fill_window(state, &mut input, adler, total_in);
    }

    // 7. L612-L617.
    state.window.strstart += state.window.lookahead;
    // `s->block_start = (long)s->strstart;` (L613). `strstart` is at most `2 * w_size`,
    // i.e. 65536, so the conversion is exact on every target this crate supports.
    state.window.block_start = isize::try_from(state.window.strstart).unwrap_or(isize::MAX);
    state.window.insert = state.window.lookahead;
    state.window.lookahead = 0;
    state.match_length = MIN_MATCH - 1;
    state.prev_length = MIN_MATCH - 1;
    state.match_available = false;

    // 8. `s->wrap = wrap;`  (L620)
    state.wrap = wrap;
    ReturnCode::OK
}

/// Reads back the sliding dictionary the compressor is maintaining.
///
/// The Rust counterpart of `deflateGetDictionary` (`deflate.c` L625-L641), declared at `zlib.h` L662.
/// `len = min(strstart + lookahead, w_size)` (L633-L635), and those `len` bytes are the
/// window's newest history, taken from `window[strstart + lookahead - len ..]` (L637).
///
/// Both out-parameters are independently optional, exactly as C's two `Z_NULL` tests at
/// L636 and L638 make them: a caller may ask only how much history there is. C also
/// declines the copy when `len` is zero (L636), which is reproduced.
///
/// C's `dictionary` has no length and `zlib.h` L666-L671 requires the caller to provide at
/// least 32768 bytes. This implementation additionally clamps the copy to the slice it is handed, so
/// a short buffer receives a prefix instead of overflowing; `dict_length` still reports the
/// full `len`, as C does.
///
/// # Errors
///
/// None once a valid state is in hand: C's only failure is the `deflateStateCheck` at
/// L630, whose remaining half cannot fail for a `&DeflateState`.
pub fn deflate_get_dictionary<'a, A: Allocator<'a>>(
    state: &DeflateState<'a, A>,
    dictionary: Option<&mut OutputRegion<'_>>,
    dict_length: Option<&mut u32>,
) -> ReturnCode {
    // `len = s->strstart + s->lookahead; if (len > s->w_size) len = s->w_size;`
    // (L633-L635)
    let filled = state.window.strstart + state.window.lookahead;
    let len = filled.min(state.w_size());

    // `if (dictionary != Z_NULL && len)`  (L636-L637)
    if let Some(target) = dictionary {
        if len != 0 {
            // `s->window + s->strstart + s->lookahead - len` (L637); non-negative because
            // `len <= filled`.
            let start = filled - len;
            let take = len.min(target.len());
            if let Some(source) = state.window.region(start, take) {
                let copied = target.write_slice_at(0, source);
                debug_assert!(
                    copied,
                    "deflateGetDictionary wrote outside the caller's buffer (deflate.c L637)"
                );
            } else {
                debug_assert!(
                    false,
                    "deflateGetDictionary read outside the window (deflate.c L637)"
                );
            }
        }
    }

    // `if (dictLength != Z_NULL) *dictLength = len;` (L638-L639). C's out-parameter is a
    // `uInt`; `len <= w_size <= 32768`, so the conversion is exact.
    if let Some(slot) = dict_length {
        *slot = u32::try_from(len).unwrap_or(u32::MAX);
    }

    ReturnCode::OK
}

/// An upper bound on the compressed size of `source_len` bytes, in `size_t` units.
///
/// The Rust counterpart of `deflateBound_z` (`deflate.c` L856-L928), declared at `zlib.h` L769. C's
/// own explanation of the constants, from the comment block at L832-L855, applies verbatim
/// and is reproduced because a future reader will need it:
///
/// > For the default `windowBits` of 15 and `memLevel` of 8, this function returns a close
/// > to exact, as well as small, upper bound on the compressed size. This is an expansion
/// > of ~0.03%, plus a small constant.
/// >
/// > For any setting other than those defaults for `windowBits` and `memLevel`, one of two
/// > worst case bounds is returned. This is at most an expansion of ~4% or ~13%, plus a
/// > small constant.
/// >
/// > Both the 0.03% and 4% derive from the overhead of stored blocks. The first one is for
/// > stored blocks of 16383 bytes (`memLevel == 8`), whereas the second is for stored
/// > blocks of 127 bytes (the worst case `memLevel == 1`). The expansion results from five
/// > bytes of header for each stored block.
/// >
/// > The larger expansion of 13% results from a window size less than or equal to the
/// > symbols buffer size (`windowBits <= memLevel + 7`). In that case some of the data
/// > being compressed may have slid out of the sliding window, impeding a stored block
/// > from being emitted. Then the only choice is a fixed or dynamic block, where a fixed
/// > block limits the maximum expansion to 9 bits per 8-bit byte, plus 10 bits for every
/// > block. The smallest block size for which this can occur is 255 (`memLevel == 2`).
/// >
/// > Shifts are used to approximate divisions, for speed.
///
/// ★ **This is not advisory.** `zlib.h` L770-L780 makes it the number callers allocate
/// their output buffer with, and promises that a single `Z_FINISH` call into a buffer of
/// this size returns `Z_STREAM_END`. A bound that is too small becomes a buffer overflow
/// in *caller* code; one that is too large breaks tests that assert exact sizes. The
/// arithmetic is therefore reproduced operation for operation, with the same widths and
/// the same order.
///
/// `state` is [`None`] for C's "if can't get parameters" branch (L875-L879), which is
/// taken when `deflateStateCheck` rejects the stream. The facade decides that, since it is
/// the layer holding the pointer.
///
/// # The four saturation branches
///
/// Each of C's `if (x < sourceLen) x = (z_size_t)-1;` tests detects wraparound of the
/// unsigned addition above it. All four are reproduced, with `wrapping_add` so that the
/// detection works rather than becoming a debug-build panic -- even though none of them can
/// fire on a 64-bit target, where `sourceLen` would have to exceed roughly 2^64 * 8/9 to
/// overflow. They are kept for parity with the 16- and 32-bit targets `zconf.h` still
/// supports, where they genuinely trigger.
#[must_use]
pub fn deflate_bound_z<'a, A: Allocator<'a>>(
    state: Option<&DeflateState<'a, A>>,
    source_len: usize,
) -> usize {
    // "upper bound for fixed blocks with 9-bit literals and length 255 (memLevel == 2,
    // which is the lowest that may not use stored blocks) -- ~13% overhead plus a small
    // constant" (L860-L862).
    let mut fixedlen = source_len
        .wrapping_add(source_len >> 3)
        .wrapping_add(source_len >> 8)
        .wrapping_add(source_len >> 9)
        .wrapping_add(4);
    if fixedlen < source_len {
        fixedlen = usize::MAX;
    }

    // "upper bound for stored blocks with length 127 (memLevel == 1) -- ~4% overhead plus
    // a small constant" (L868-L869).
    let mut storelen = source_len
        .wrapping_add(source_len >> 5)
        .wrapping_add(source_len >> 7)
        .wrapping_add(source_len >> 11)
        .wrapping_add(7);
    if storelen < source_len {
        storelen = usize::MAX;
    }

    // "if can't get parameters, return larger bound plus a wrapper" (L875-L879). Eighteen
    // is the largest wrapper: a gzip header of ten bytes plus an eight-byte trailer.
    let Some(state) = state else {
        let bound = if fixedlen > storelen {
            fixedlen
        } else {
            storelen
        };
        let padded = bound.wrapping_add(18);
        return if padded < bound { usize::MAX } else { padded };
    };

    // "compute wrapper length" (L881-L914). The `switch` is over `|wrap|`, because a
    // finished stream carries a negated `wrap` (L1288) and still has the same wrapper.
    let wraplen: usize = match state.wrap.unsigned_abs() {
        // "raw deflate" (L884-L886)
        0 => 0,
        // "zlib wrapper" (L887-L888): a two-byte header plus a four-byte Adler-32, and
        // four more for the dictionary identifier when a preset dictionary is in use.
        1 => 6 + if state.window.strstart == 0 { 0 } else { 4 },
        // "gzip wrapper" (L891-L910): a ten-byte header plus an eight-byte trailer, and
        // then whatever optional fields the caller's header adds.
        2 => {
            let mut wraplen = 18;
            // "user-supplied gzip header" (L893)
            if let Some(head) = state.gzhead {
                // `wraplen += 2 + s->gzhead->extra_len;` (L895-L896). C reads the raw
                // `extra_len` here, not the 16-bit-masked length it later transmits, so
                // the slice length is the faithful value.
                if head.has_extra() {
                    wraplen += 2 + head.advertised_extra_len();
                }
                // L897-L901 and L902-L906, both counting the terminator. The view scans
                // its live borrow exactly as C scans the caller's string, and falls back
                // to the length captured at `deflateSetHeader` time once the borrow has
                // been released -- see `GzHeaderView::release_fields`.
                if head.has_name() {
                    wraplen += head.name_bound_len();
                }
                if head.has_comment() {
                    wraplen += head.comment_bound_len();
                }
                // `if (s->gzhead->hcrc) wraplen += 2;`  (L907-L908)
                if head.hcrc {
                    wraplen += 2;
                }
            }
            wraplen
        }
        // "for compiler happiness" (L912-L913). Unreachable through this implementation, where
        // `wrap` only ever holds 0, 1, 2 or a negation of one of those, but reproduced so
        // that no input can select a different answer than C's would.
        _ => 18,
    };

    // "if not default parameters, return one of the conservative bounds" (L916-L921).
    // `hash_bits` is `memLevel + 7`, so `8 + 7` is `memLevel == 8`.
    if state.w_bits() != 15 || state.hash_bits() != 8 + 7 {
        let bound = if state.w_bits() <= state.hash_bits() && state.level != 0 {
            fixedlen
        } else {
            storelen
        };
        let padded = bound.wrapping_add(wraplen);
        return if padded < bound { usize::MAX } else { padded };
    }

    // "default settings: return tight bound for that case -- ~0.03% overhead plus a small
    // constant" (L923-L927). Written with the same token order as C's
    // `+ 13 - 6 + wraplen`, which is the five-byte stored-block header per 16383-byte
    // block plus a constant, less the six bytes the zlib wrapper already accounts for.
    let bound = source_len
        .wrapping_add(source_len >> 12)
        .wrapping_add(source_len >> 14)
        .wrapping_add(source_len >> 25)
        .wrapping_add(13)
        .wrapping_sub(6)
        .wrapping_add(wraplen);
    if bound < source_len {
        usize::MAX
    } else {
        bound
    }
}

/// An upper bound on the compressed size of `source_len` bytes, in `uLong` units.
///
/// The Rust counterpart of `deflateBound` (`deflate.c` L929-L932), declared at `zlib.h` L768.
///
/// # Where the narrowing lives
///
/// C narrows a `z_size_t` to a `uLong` and saturates when that loses information. Those
/// two types have different widths on exactly one supported platform family: on LLP64
/// Windows `size_t` is 64 bits and `unsigned long` is 32. The core has no `c_ulong`, so
/// the split is:
///
/// * **here** -- `z_size_t` is `usize` and `uLong` is modelled as `u64`, the same width
///   `deflate/state.rs` gives `total_in` and `total_out`. On every target where `usize` is
///   at most 64 bits this conversion is lossless, so the saturation is retained for shape
///   rather than for effect, and `source_len` is clamped on the way in for the same reason;
/// * **the facade** -- `crates/libz-rs-sys` owes the `u64` to `c_ulong` step, which is
///   where a real LLP64 truncation happens and where `(uLong)-1` must be produced.
#[must_use]
pub fn deflate_bound<'a, A: Allocator<'a>>(
    state: Option<&DeflateState<'a, A>>,
    source_len: u64,
) -> u64 {
    // Only reachable on a target with a `usize` narrower than 64 bits, where a caller
    // cannot address `source_len` bytes in the first place; saturating gives the largest
    // bound this implementation can express, which is the same direction C's `(uLong)-1` goes.
    let requested = usize::try_from(source_len).unwrap_or(usize::MAX);
    let bound = deflate_bound_z(state, requested);
    u64::try_from(bound).unwrap_or(u64::MAX)
}

/// `usize` widened to the `u64` this module extracts header and trailer bytes from.
///
/// `u64: From<usize>` does not exist, because `usize` is not contracted to be at most 64
/// bits, so the widening goes through [`TryFrom`]. It cannot fail on any target this crate
/// supports, and the values reaching it here are all below 2^17.
fn widen(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// C's `(Byte)((value >> shift) & 0xff)`: one little-endian byte of a wider field.
///
/// The gzip header writes `MTIME` this way (`deflate.c` L1098-L1101) and both trailers
/// write the check value and `total_in` this way (L1269-L1276). Naming it once keeps the
/// sixteen call sites readable and puts the truncation argument in one place.
// The truncation is the operation: `as u8` of a shifted value is exactly C's `& 0xff`
// followed by the implicit conversion to `Byte`.
#[allow(clippy::cast_possible_truncation)]
const fn byte_at(value: u64, shift: u32) -> u8 {
    (value >> shift) as u8
}

/// The gzip `XFL` byte: how the data was compressed, per RFC 1952 §2.3.1.
///
/// Mirrors the expression `deflate.c` writes twice, identically, at L1078-L1080 and
/// L1102-L1104.
///
/// RFC 1952 assigns 2 to "compressor used maximum compression, slowest algorithm" and 4 to
/// "compressor used fastest algorithm"; every level in between reports 0. Extracted into
/// one function because the two C sites must agree and a divergence between them would be
/// a one-byte difference in the header.
fn gzip_xfl<'a, A: Allocator<'a>>(state: &DeflateState<'a, A>) -> u8 {
    if state.level == 9 {
        2
    } else if state.strategy.as_raw() >= Z_HUFFMAN_ONLY || state.level < 2 {
        4
    } else {
        0
    }
}

/// `HCRC_UPDATE(beg)` (`deflate.c` L970-L978).
///
/// The header CRC covers the header bytes as they are produced, so it must be folded in
/// *before* each flush moves them out of the pending buffer -- `beg` is where the
/// unaccounted-for bytes start, and every call site resets it to 0 after a flush.
///
/// C dereferences `s->gzhead` unconditionally here, which is sound only because the macro
/// is expanded exclusively inside the four states that a caller-supplied header makes
/// reachable. This implementation makes that structural by matching on the header's presence and
/// doing nothing when it is absent; no byte changes, because C would never have reached the
/// macro in that case.
fn hcrc_update<'a, A: Allocator<'a>>(state: &DeflateState<'a, A>, check: &mut u32, beg: usize) {
    let Some(head) = state.gzhead else {
        return;
    };
    if head.hcrc && state.pending_bytes() > beg {
        // `written_from` is `pending_buf[beg..pending]`; `beg <= pending` was just
        // established, so [`None`] names an unreachable case.
        if let Some(bytes) = state.pending.written_from(beg) {
            *check = crc32_z(*check, bytes);
        }
    }
}

/// `flush_pending(strm);` followed by the resume test that ends every header stage.
///
/// The pattern appears eight times in `deflate()` -- at L1059-L1063, L1085-L1089,
/// L1128-L1132, L1151-L1155, L1173-L1177, L1190-L1194 and L1203-L1207 -- always spelled
/// the same way.
///
/// ★ This is what makes the header a *resumable* state machine rather than straight-line
/// code. A caller with a one-byte output buffer still makes progress: each stage writes
/// what it can, hands the bytes over, and records `last_flush = -1` so that the next call
/// is not mistaken for a duplicate flush and rejected with [`ReturnCode::BUF_ERROR`]
/// (the reasoning is at L1004-L1009). Collapsing the stages would break streaming callers.
///
/// Returns `true` when the caller must return [`ReturnCode::OK`] immediately.
#[must_use]
fn flush_pending_and_yield<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    cursors: &mut StreamCursors<'_, '_>,
) -> bool {
    flush_pending(
        state,
        &mut cursors.output,
        &mut cursors.total_out,
        _tr_flush_bits,
    );
    if state.pending_bytes() == 0 {
        return false;
    }
    state.last_flush = -1;
    true
}

/// Compresses as much as possible, and flushes according to `flush`.
///
/// The Rust counterpart of `deflate` (`deflate.c` L981-L1290), declared at `zlib.h` L254 and documented
/// at L256-L363. It is written as one function, with C's fall-through
/// `if (s->status == ...)` chain preserved verbatim, because **every header stage must be
/// resumable**: a caller whose `avail_out` runs out mid-header re-enters at the state it
/// left off in, and the order of those tests is what makes that work.
///
/// # Structure, following the C source
///
/// | Section | `deflate.c` |
/// |---|---|
/// | entry guards | L985-L995 |
/// | flush bookkeeping | L997-L998 |
/// | pending drain and the duplicate-flush guard | L1000-L1021 |
/// | "user must not provide more input after the first FINISH" | L1023-L1026 |
/// | the RFC 1950 zlib header | L1028-L1064 |
/// | the RFC 1952 gzip header, five resumable stages | L1065-L1209 |
/// | "start a new block or continue the current one" | L1211-L1220 |
/// | post-block handling | L1222-L1261 |
/// | the trailer | L1263-L1289 |
///
/// # Guards C applies that this function does not
///
/// L985 rejects a stream whose `deflateStateCheck` fails, and L990-L991 rejects a null
/// `next_out` or a null `next_in` paired with a non-zero `avail_in`. Those are pointer
/// facts and belong to `crates/libz-rs-sys`; see [`deflate_state_check`]. A null `next_in`
/// with `avail_in == 0` is **legal** in C and corresponds to an empty `input` slice here,
/// which is accepted.
///
/// ★ `flush > Z_BLOCK` at L985 means `Z_TREES` (6) is **rejected**: it is a decompression
/// -only request. `crate::config::validate_deflate_flush` performs both halves of that
/// test.
///
/// # Returns
///
/// * [`ReturnCode::OK`] -- progress was made and the stream is not finished.
/// * [`ReturnCode::STREAM_END`] -- `flush` was `Z_FINISH` and everything has been written.
/// * [`ReturnCode::STREAM_ERROR`] -- the flush value was invalid, or more output was
///   requested after the stream finished.
/// * [`ReturnCode::BUF_ERROR`] -- no progress was possible. Not fatal; `zlib.h` L360-L363
///   says to call again with more input or more room.
// One function, ~330 lines, one `match` chain and eight resume points -- which is what
// L981-L1290 is. Splitting it would obscure the correspondence the differential suite is
// verified against, and reordering the status tests would change the emitted bytes, so the
// two complexity lints are relaxed here rather than the structure changed.
#[allow(clippy::too_many_lines, clippy::cognitive_complexity)]
pub fn deflate<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    stream: &mut DeflateStream<'_, '_>,
    flush: i32,
) -> ReturnCode {
    // A guard C has no way to express: it holds two pointers and two counts, whereas this
    // implementation holds two slices and two offsets into them, and an offset past the end of its
    // slice describes no stream at all. Rejecting it here is what lets every cursor
    // operation below be infallible.
    if stream.next_in > stream.input.len() || stream.next_out > stream.output.len() {
        return ReturnCode::STREAM_ERROR.record_msg(&mut stream.msg);
    }

    // Destructuring gives an independent borrow of each field, which is what lets the
    // cursors hold the buffers while the six scalars are read and written back.
    let DeflateStream {
        input,
        next_in,
        output,
        next_out,
        total_in,
        total_out,
        msg,
        adler,
        data_type,
    } = stream;

    // `LOAD()`: the compressors reach all of this through `s->strm` in C (`deflate.h`
    // L105). Both cursors are positioned by advancing from the start of their buffer, so
    // that the consumed prefix stays addressable -- `deflate_stored` reads *backwards*
    // from `next_in` (`deflate.c` L1766 and L1780).
    let mut cursors = {
        let mut in_cursor = InputCursor::new(input);
        let seeked_in = in_cursor.advance(*next_in);
        debug_assert!(seeked_in, "next_in is past the end of the input buffer");

        // The cursor owns its region for the duration of the call -- a complete handle on
        // the caller's buffer that hands out no view of it -- so the region is moved out of
        // the stream here and moved back by the single write-back at the end. The guard
        // above has already established that `next_out` is inside it.
        let region = core::mem::replace(output, OutputRegion::empty());
        let out_cursor = OutputCursor::from_region(region, *next_out);

        StreamCursors {
            input: in_cursor,
            output: out_cursor,
            check: *adler,
            total_in: *total_in,
            total_out: *total_out,
            data_type: *data_type,
        }
    };

    // C returns from twelve places, each of which leaves `strm` updated because it mutates
    // it directly. Here the twelve `return`s become `break 'driver`, so there is exactly
    // one place that writes the six scalars and the two offsets back -- and no path that
    // can forget to.
    let ret = 'driver: {
        // `if (deflateStateCheck(strm) || flush > Z_BLOCK || flush < 0) return
        // Z_STREAM_ERROR;`  (L985-L987)
        //
        // A plain `return`, not an `ERR_RETURN`, so `msg` is deliberately left untouched.
        let flush = match validate_deflate_flush(flush) {
            Ok(flush) => flush,
            Err(code) => break 'driver code,
        };

        // The third disjunct of L990-L992; the two pointer tests are the facade's.
        if state.status == Status::Finish && flush != Flush::Finish {
            break 'driver ReturnCode::STREAM_ERROR.record_msg(msg);
        }

        // `if (strm->avail_out == 0) ERR_RETURN(strm, Z_BUF_ERROR);`  (L995)
        if cursors.avail_out() == 0 {
            break 'driver ReturnCode::BUF_ERROR.record_msg(msg);
        }

        // `old_flush = s->last_flush; s->last_flush = flush;`  (L997-L998)
        //
        // `last_flush` is a plain `int` that also carries the two out-of-band sentinels
        // `-1` ("returned with no output room") and `-2` ("reset, never called").
        let old_flush = state.last_flush;
        state.last_flush = flush.as_raw();

        // "Flush as much pending output as possible" (L1000-L1012).
        if state.pending_bytes() != 0 {
            flush_pending(
                state,
                &mut cursors.output,
                &mut cursors.total_out,
                _tr_flush_bits,
            );
            if cursors.avail_out() == 0 {
                // "Since avail_out is 0, deflate will be called again with more output
                // space, but possibly with both pending and avail_in equal to zero. There
                // won't be anything to do, but this is not an error situation so make sure
                // we return OK instead of BUF_ERROR at next call of deflate" (L1004-L1009).
                state.last_flush = -1;
                break 'driver ReturnCode::OK;
            }
        }
        // "Make sure there is something to do and avoid duplicate consecutive flushes. For
        // repeated and useless calls with Z_FINISH, we keep returning Z_STREAM_END instead
        // of Z_BUF_ERROR." (L1014-L1021)
        //
        // ★ `RANK` is applied to the raw `int`s, so the sentinels take part in the
        // comparison: `RANK(-1)` is `-2` and `RANK(-2)` is `-4`, both below every real
        // rank, which is exactly why a call following an out-of-room return or a reset is
        // never mistaken for a repeat of the same flush.
        else if cursors.avail_in() == 0
            && flush.rank() <= rank_of_i32(old_flush)
            && flush != Flush::Finish
        {
            break 'driver ReturnCode::BUF_ERROR.record_msg(msg);
        }

        // "User must not provide more input after the first FINISH" (L1023-L1026).
        if state.status == Status::Finish && cursors.avail_in() != 0 {
            break 'driver ReturnCode::BUF_ERROR.record_msg(msg);
        }

        // A raw stream has no header at all, so it goes straight to work (L1029-L1030).
        if state.status == Status::Init && state.wrap == 0 {
            state.status = Status::Busy;
        }

        if state.status == Status::Init {
            // `uInt header = (Z_DEFLATED + ((s->w_bits - 8) << 4)) << 8;`  (L1033)
            //
            // The `CMF` byte of RFC 1950 §2.2: `CM` in the low nibble, `CINFO` -- the
            // window exponent less 8 -- in the high nibble. `Z_DEFLATED` is 8, and the
            // fallback below names an unreachable case rather than a default.
            let method = u32::try_from(Z_DEFLATED).unwrap_or(8);
            let mut header = (method + ((state.w_bits() - 8) << 4)) << 8;

            // `FLEVEL`, RFC 1950 §2.2: "the compression level" (L1036-L1043). Note that
            // any strategy from `Z_HUFFMAN_ONLY` upwards reports 0 regardless of level.
            let level_flags: u32 = if state.strategy.as_raw() >= Z_HUFFMAN_ONLY || state.level < 2 {
                0
            } else if state.level < 6 {
                1
            } else if state.level == 6 {
                2
            } else {
                3
            };
            header |= level_flags << 6;

            // `FDICT` (L1045). A non-zero `strstart` is what a preset dictionary leaves
            // behind, so it is the test for "a dictionary was set".
            if state.window.strstart != 0 {
                header |= PRESET_DICT;
            }

            // "The FCHECK value must be such that CMF and FLG, when viewed as a 16-bit
            // unsigned integer stored in MSB order, is a multiple of 31"
            // (`doc/rfc1950.txt`, §2.2). C adds `31 - (header % 31)` unconditionally, so a
            // word that is already a multiple of 31 gains a further 31 -- reproduced
            // exactly, because the low five bits are the checksum field and any other
            // value is a different stream (L1046).
            header += 31 - (header % 31);

            put_short_msb(state, header);

            // "Save the adler32 of the preset dictionary" (L1050-L1054) -- the `DICTID`
            // field of RFC 1950 §2.2, four bytes, most significant first.
            if state.window.strstart != 0 {
                put_short_msb(state, cursors.check >> 16);
                put_short_msb(state, cursors.check & 0xffff);
            }

            // `strm->adler = adler32(0L, Z_NULL, 0);`  (L1055) -- the dictionary is not
            // part of the stream's own check value, so the running Adler-32 restarts. That
            // C expression is **1**, not 0; see `deflate_reset_keep`.
            cursors.check = ADLER32_INITIAL_VALUE;
            state.status = Status::Busy;

            // "Compression must start with an empty pending buffer" (L1058-L1063).
            if flush_pending_and_yield(state, &mut cursors) {
                break 'driver ReturnCode::OK;
            }
        }

        //  The RFC 1952 gzip header -- L1065-L1209
        //
        //  `GZIP` is defined unless `NO_GZIP` is (`deflate.h` L18-L24), so every stage
        //  below is in scope for the shipped configuration.

        if state.status == Status::GzipHeader {
            // `strm->adler = crc32(0L, Z_NULL, 0);`  (L1068) -- a gzip stream checks with
            // CRC-32, not Adler-32.
            cursors.check = crc32(0, &[]);

            // `ID1`, `ID2`, `CM`: the magic 0x1f 0x8b and "deflate" (L1069-L1071), per
            // RFC 1952 §2.3.1.
            put_byte(state, 31);
            put_byte(state, 139);
            put_byte(state, 8);

            match state.gzhead {
                // "if (s->gzhead == Z_NULL)" (L1072-L1090): the ten-byte default header.
                None => {
                    // `FLG` = 0 and `MTIME` = 0, "no time stamp available" (L1073-L1077).
                    put_byte(state, 0);
                    put_byte(state, 0);
                    put_byte(state, 0);
                    put_byte(state, 0);
                    put_byte(state, 0);
                    put_byte(state, gzip_xfl(state));
                    put_byte(state, OS_CODE);
                    state.status = Status::Busy;

                    // "Compression must start with an empty pending buffer" (L1084-L1089).
                    if flush_pending_and_yield(state, &mut cursors) {
                        break 'driver ReturnCode::OK;
                    }
                }

                // The caller supplied a header (L1091-L1115).
                Some(head) => {
                    // `FLG` (L1092-L1097): `FTEXT`, `FHCRC`, `FEXTRA`, `FNAME`, `FCOMMENT`.
                    // Written as C's sum of five conditionals; the bits are disjoint, so it
                    // is the same value an `|` would give.
                    let flg = u8::from(head.text)
                        + if head.hcrc { 2 } else { 0 }
                        + if head.has_extra() { 4 } else { 0 }
                        + if head.has_name() { 8 } else { 0 }
                        + if head.has_comment() { 16 } else { 0 };
                    put_byte(state, flg);

                    // `MTIME`, four bytes, least significant first (L1098-L1101).
                    let time = u64::from(head.time);
                    put_byte(state, byte_at(time, 0));
                    put_byte(state, byte_at(time, 8));
                    put_byte(state, byte_at(time, 16));
                    put_byte(state, byte_at(time, 24));

                    // `XFL` (L1102-L1104) and `OS` (L1105).
                    put_byte(state, gzip_xfl(state));
                    // `put_byte(s, s->gzhead->os & 0xff);` -- the low byte of the
                    // two's-complement representation, which is what C's conversion to
                    // `Byte` produces for any `int`, negative values included.
                    let [os_byte, ..] = head.os.to_le_bytes();
                    put_byte(state, os_byte);

                    // `XLEN`, two bytes, least significant first (L1106-L1109). C writes
                    // the raw `extra_len`; only its low 16 bits are transmitted, which is
                    // also the number of bytes the next stage copies (L1120).
                    if head.has_extra() {
                        let extra_len = widen(head.advertised_extra_len());
                        put_byte(state, byte_at(extra_len, 0));
                        put_byte(state, byte_at(extra_len, 8));
                    }

                    // `if (s->gzhead->hcrc) strm->adler = crc32_z(strm->adler,
                    // s->pending_buf, s->pending);`  (L1110-L1112) -- the fixed part of the
                    // header, counted from the base of the buffer.
                    if head.hcrc {
                        cursors.check = crc32_z(cursors.check, state.pending.written());
                    }

                    state.gzindex = 0;
                    state.status = Status::Extra;
                }
            }
        }

        // `FEXTRA`, RFC 1952 §2.3.1.1 -- L1117-L1143.
        //
        // ★ C reaches `s->gzhead->extra` here without a null check, which is sound only
        // because this state is unreachable unless a header was installed. Matching on the
        // header makes that structural; a stream that somehow arrived here with no header
        // simply advances, which is what C would have done had it not faulted.
        if state.status == Status::Extra {
            // The transmitted field is the first `extra_len` bytes, i.e. at most 65535 of
            // them (L1120). `extra_chunk(0)` is that span; the borrow is a `Cell` slice
            // because the bytes are the caller's and the caller may write them -- see
            // `GzHeaderView`.
            if let Some(extra) = state.gzhead.and_then(|head| head.extra_chunk(0)) {
                // `ulg beg = s->pending;` -- "start of bytes to update crc" (L1119)
                let mut beg = state.pending_bytes();

                // `ulg left = (s->gzhead->extra_len & 0xffff) - s->gzindex;`  (L1120)
                //
                // Saturating rather than wrapping: C's unsigned subtraction would produce
                // an astronomical length if a caller replaced the header with a shorter one
                // between two calls, and then read far past the end of it. Zero is the
                // fail-closed answer, and no well-behaved caller can reach it because
                // `gzindex` only ever advances by bytes this loop has already copied.
                let mut left = extra.len().saturating_sub(state.gzindex);

                // `while (s->pending + left > s->pending_buf_size)`  (L1121-L1135)
                while state.pending_bytes() + left > state.pending_buf_size() {
                    // `ulg copy = s->pending_buf_size - s->pending;`  (L1122)
                    let copy = state
                        .pending_buf_size()
                        .saturating_sub(state.pending_bytes());

                    // `zmemcpy(s->pending_buf + s->pending, s->gzhead->extra + s->gzindex,
                    // copy);` and `s->pending = s->pending_buf_size;`  (L1123-L1125)
                    let chunk = extra.get(state.gzindex..).unwrap_or(&[]);
                    let chunk = chunk.get(..copy).unwrap_or(chunk);
                    let copied = state.pending.append_cells(chunk);
                    debug_assert_eq!(
                        copied, copy,
                        "the extra-field chunk did not fill the pending buffer \
                         (deflate.c L1122-L1125)"
                    );

                    // `HCRC_UPDATE(beg);`  (L1126)
                    hcrc_update(state, &mut cursors.check, beg);

                    // `s->gzindex += copy;`  (L1127)
                    state.gzindex += copied;

                    // L1128-L1132.
                    if flush_pending_and_yield(state, &mut cursors) {
                        break 'driver ReturnCode::OK;
                    }

                    // `beg = 0; left -= copy;`  (L1133-L1134)
                    beg = 0;
                    left -= copied;
                }

                // The tail: `zmemcpy(..., left); s->pending += left;`  (L1136-L1138)
                let chunk = extra.get(state.gzindex..).unwrap_or(&[]);
                let chunk = chunk.get(..left).unwrap_or(chunk);
                let copied = state.pending.append_cells(chunk);
                debug_assert_eq!(
                    copied, left,
                    "the extra-field tail did not fit in the pending buffer \
                     (deflate.c L1136-L1138)"
                );

                // `HCRC_UPDATE(beg); s->gzindex = 0;`  (L1139-L1140)
                hcrc_update(state, &mut cursors.check, beg);
                state.gzindex = 0;
            }
            state.status = Status::Name;
        }

        // `FNAME`, RFC 1952 §2.3.1.1 -- L1144-L1165. A NUL-terminated ISO 8859-1 string,
        // emitted one byte at a time so that it can resume anywhere.
        if state.status == Status::Name {
            if let Some(head) = state.gzhead.filter(GzHeaderView::has_name) {
                // `ulg beg = s->pending;`  (L1146)
                let mut beg = state.pending_bytes();

                // `do { ... } while (val != 0);`  (L1148-L1160)
                loop {
                    if state.pending_bytes() == state.pending_buf_size() {
                        // `HCRC_UPDATE(beg);` then flush, then `beg = 0;`  (L1150-L1156)
                        hcrc_update(state, &mut cursors.check, beg);
                        if flush_pending_and_yield(state, &mut cursors) {
                            break 'driver ReturnCode::OK;
                        }
                        beg = 0;
                    }

                    // `val = s->gzhead->name[s->gzindex++];`  (L1158)
                    //
                    // `GzHeaderView`'s contract is that `name` includes its terminating
                    // zero, so [`None`] means the facade built a slice without one -- the
                    // case in which C reads past the end of the caller's string. Treating
                    // it as the terminator ends the field and the loop, which is the
                    // fail-closed answer.
                    let val = head.name_at(state.gzindex).unwrap_or(0);
                    state.gzindex += 1;
                    put_byte(state, val);

                    if val == 0 {
                        break;
                    }
                }

                // `HCRC_UPDATE(beg); s->gzindex = 0;`  (L1161-L1162)
                hcrc_update(state, &mut cursors.check, beg);
                // ★ `NAME_STATE` rewinds `gzindex` and `COMMENT_STATE` does not. The
                // asymmetry is in the C source (L1162 against L1183) and is preserved: the
                // comment field is the last resumable one, so nothing reads the index
                // afterwards.
                state.gzindex = 0;
            }
            state.status = Status::Comment;
        }

        // `FCOMMENT`, RFC 1952 §2.3.1.1 -- L1166-L1186. Identical to `FNAME` except for the
        // missing `gzindex` rewind.
        if state.status == Status::Comment {
            if let Some(head) = state.gzhead.filter(GzHeaderView::has_comment) {
                let mut beg = state.pending_bytes();
                loop {
                    if state.pending_bytes() == state.pending_buf_size() {
                        hcrc_update(state, &mut cursors.check, beg);
                        if flush_pending_and_yield(state, &mut cursors) {
                            break 'driver ReturnCode::OK;
                        }
                        beg = 0;
                    }

                    // `val = s->gzhead->comment[s->gzindex++];`  (L1180)
                    let val = head.comment_at(state.gzindex).unwrap_or(0);
                    state.gzindex += 1;
                    put_byte(state, val);

                    if val == 0 {
                        break;
                    }
                }
                // `HCRC_UPDATE(beg);` and **no** rewind (L1183).
                hcrc_update(state, &mut cursors.check, beg);
            }
            state.status = Status::Hcrc;
        }

        // `FHCRC`, RFC 1952 §2.3.1.1 -- L1187-L1208. The two least significant bytes of the
        // CRC-32 of the header bytes emitted so far.
        if state.status == Status::Hcrc {
            if state.gzhead.is_some_and(|head| head.hcrc) {
                // "if (s->pending + 2 > s->pending_buf_size)" (L1189-L1195): make room for
                // both bytes, so that the pair is never split across a flush.
                if state.pending_bytes() + 2 > state.pending_buf_size()
                    && flush_pending_and_yield(state, &mut cursors)
                {
                    break 'driver ReturnCode::OK;
                }

                let check = u64::from(cursors.check);
                put_byte(state, byte_at(check, 0));
                put_byte(state, byte_at(check, 8));

                // `strm->adler = crc32(0L, Z_NULL, 0);`  (L1198) -- the header CRC is
                // finished; the stream's own CRC starts over from zero.
                cursors.check = crc32(0, &[]);
            }
            state.status = Status::Busy;

            // ★ The gzip header is now entirely in the pending buffer, so the caller's
            // `extra`, `name` and `comment` will never be read again -- and `zlib.h`
            // L836-L852 asks the application to keep them available only until exactly this
            // point. The borrows are dropped here rather than left to the state's own
            // lifetime, so a caller that frees them next does not leave this crate holding
            // dangling references. Every scalar and length survives, so `deflateBound` and
            // the `FLG` byte are unaffected; see `DeflateState::release_gzhead_fields`.
            //
            // Placed *before* the flush below, because that flush can yield out of the
            // driver: the status is already `BUSY_STATE`, so a resumed call re-enters past
            // every header state and would never reach a release placed after it.
            state.release_gzhead_fields();

            // "Compression must start with an empty pending buffer" (L1202-L1207).
            if flush_pending_and_yield(state, &mut cursors) {
                break 'driver ReturnCode::OK;
            }
        }

        if cursors.avail_in() != 0
            || state.window.lookahead != 0
            || (flush != Flush::NoFlush && state.status != Status::Finish)
        {
            // ★ The dispatch precedence at L1217-L1220 is the contract: level 0 is tested
            // *first*, so a level-0 stream stores its input even when the strategy is
            // `Z_HUFFMAN_ONLY` or `Z_RLE`; only then does the strategy get a say, and only
            // if it is neither of those does the level's table entry decide.
            let bstate = CompressFunc::select(
                state.level,
                state.strategy,
                config_for_level(state.level).func,
            )
            .call(state, &mut cursors, flush);

            // L1222-L1224.
            if matches!(bstate, BlockState::FinishStarted | BlockState::FinishDone) {
                state.status = Status::Finish;
            }

            // L1225-L1237.
            if matches!(bstate, BlockState::NeedMore | BlockState::FinishStarted) {
                if cursors.avail_out() == 0 {
                    // "avoid BUF_ERROR next call, see above" (L1227)
                    state.last_flush = -1;
                }
                // "If flush != Z_NO_FLUSH && avail_out == 0, the next call of deflate
                // should use the same flush parameter to make sure that the flush is
                // complete. So we don't have to output an empty block here, this will be
                // done at next call. This also ensures that for a very small output
                // buffer, we emit at most one empty block." (L1230-L1236)
                break 'driver ReturnCode::OK;
            }

            // L1238-L1260.
            if bstate == BlockState::BlockDone {
                if flush == Flush::PartialFlush {
                    // One empty static block, enough lookahead for the decoder (L1240).
                    _tr_align(state);
                } else if flush != Flush::Block {
                    // "FULL_FLUSH or SYNC_FLUSH" (L1241-L1242). An empty stored block;
                    // "for a full flush, this empty block will be recognized as a special
                    // marker by inflate_sync()" (L1243-L1245).
                    _tr_stored_block(state, None, 0, false);

                    if flush == Flush::FullFlush {
                        // "forget history" (L1247-L1252)
                        clear_hash(state);
                        if state.window.lookahead == 0 {
                            state.window.strstart = 0;
                            state.window.block_start = 0;
                            state.window.insert = 0;
                        }
                    }
                }

                flush_pending(
                    state,
                    &mut cursors.output,
                    &mut cursors.total_out,
                    _tr_flush_bits,
                );
                if cursors.avail_out() == 0 {
                    // "avoid BUF_ERROR at next call, see above" (L1257)
                    state.last_flush = -1;
                    break 'driver ReturnCode::OK;
                }
            }
        }

        // `if (flush != Z_FINISH) return Z_OK;`  (L1263)
        if flush != Flush::Finish {
            break 'driver ReturnCode::OK;
        }

        // `if (s->wrap <= 0) return Z_STREAM_END;`  (L1264) -- a raw stream has no trailer,
        // and a negative `wrap` means this stream has already written one.
        if state.wrap <= 0 {
            break 'driver ReturnCode::STREAM_END;
        }

        if state.wrap == 2 {
            // RFC 1952 §2.3.1: `CRC32` then `ISIZE`, each four bytes least significant
            // first, `ISIZE` being "the size of the original (uncompressed) input data
            // modulo 2^32" -- which is what taking the low four bytes of a 64-bit
            // `total_in` produces (L1268-L1277).
            let check = u64::from(cursors.check);
            put_byte(state, byte_at(check, 0));
            put_byte(state, byte_at(check, 8));
            put_byte(state, byte_at(check, 16));
            put_byte(state, byte_at(check, 24));
            put_byte(state, byte_at(cursors.total_in, 0));
            put_byte(state, byte_at(cursors.total_in, 8));
            put_byte(state, byte_at(cursors.total_in, 16));
            put_byte(state, byte_at(cursors.total_in, 24));
        } else {
            // RFC 1950 §2.2: `ADLER32`, four bytes most significant first (L1280-L1283).
            put_short_msb(state, cursors.check >> 16);
            put_short_msb(state, cursors.check & 0xffff);
        }

        // "If avail_out is zero, the application will call deflate again to flush the
        // rest." (L1284-L1287)
        flush_pending(
            state,
            &mut cursors.output,
            &mut cursors.total_out,
            _tr_flush_bits,
        );

        // `if (s->wrap > 0) s->wrap = -s->wrap;` -- "write the trailer only once!" (L1288).
        // `deflateResetKeep` un-negates it (L659-L661), which is what makes a stream
        // reusable.
        if state.wrap > 0 {
            state.wrap = -state.wrap;
        }

        // `return s->pending != 0 ? Z_OK : Z_STREAM_END;`  (L1289)
        if state.pending_bytes() == 0 {
            ReturnCode::STREAM_END
        } else {
            ReturnCode::OK
        }
    };

    // `RESTORE()`: the single write-back for all twelve of C's exit paths. The output
    // region returns to the stream here, which is the only place it can, because
    // `cursors` is consumed by this destructuring.
    let (region, produced) = cursors.output.into_region();
    *output = region;
    *next_in = cursors.input.consumed();
    *next_out = produced;
    *total_in = cursors.total_in;
    *total_out = cursors.total_out;
    *adler = cursors.check;
    *data_type = cursors.data_type;

    ret
}

/// Releases everything the compression state owns, and reports whether it was mid-stream.
///
/// The Rust counterpart of `deflateEnd` (`deflate.c` L1293-L1310), declared at `zlib.h` L367.
///
/// ★ **The return value is a documented public contract**, not a formality: `zlib.h`
/// L373-L377 promises `Z_DATA_ERROR` "if the stream was freed prematurely (some input or
/// output was discarded)", and `test/infcover.c` exercises exactly that. The status is
/// therefore captured **before** anything is torn down (L1298), because after the teardown
/// there is nothing left to read it from -- and a `Drop` implementation cannot report
/// anything at all (AAP §0.3.3.8).
///
/// The memory half is `TRY_FREE` in reverse order of allocation -- `pending_buf`, `head`,
/// `prev`, `window` (L1300-L1304) -- which [`DeflateState::release`] performs, spelling the
/// order out. That order is what `test/infcover.c`'s tracking allocator requires: it
/// reports a non-LIFO free as a defect.
///
/// Two of C's steps are the facade's, because they concern memory the facade allocated and
/// a field the facade owns: `ZFREE(strm, strm->state)` (L1306) and
/// `strm->state = Z_NULL` (L1307).
///
/// ★ The state arrives by mutable reference, not by value, and that is a soundness
/// requirement rather than a style choice: a state moved into this function would carry
/// its buffers' borrows in argument position, where they are protected for the whole
/// call, and freeing memory a protected reference covers is undefined behaviour. The
/// caller keeps the state and drops it where it lives, with every buffer already
/// released. `zlib_rs::allocate::ForeignBlock` records the rule in full.
///
/// # Errors
///
/// [`ReturnCode::DATA_ERROR`] when the stream was in [`Status::Busy`], i.e. compression had
/// begun and had not been finished. `deflateEnd` still frees everything in that case; the
/// code is a report, not a refusal.
pub fn deflate_end<'a, A: Allocator<'a>>(state: &mut DeflateState<'a, A>) -> ReturnCode {
    // `status = strm->state->status;`  (L1298) -- before the teardown.
    let status = state.status();

    // L1300-L1304.
    state.release();

    // `return status == BUSY_STATE ? Z_DATA_ERROR : Z_OK;`  (L1309)
    if status == Status::Busy {
        ReturnCode::DATA_ERROR
    } else {
        ReturnCode::OK
    }
}

/// Duplicates a compression state, allocating the copy's buffers from `allocator`.
///
/// The Rust counterpart of `deflateCopy` (`deflate.c` L1317-L1377), declared at `zlib.h` L684. C copies
/// the `z_stream` wholesale (L1333), allocates a fresh `deflate_state`, copies it bytewise
/// (L1339), then re-allocates all four buffers and copies the live parts of each. The
/// bytewise struct copy plus the four buffer copies are [`DeflateState::try_clone_in`]; the
/// `z_stream` copy at L1333 is the facade's, since that is the caller's structure.
///
/// The four buffers are handled as C handles them, which is not uniformly:
///
/// | Buffer | `deflate.c` | How much |
/// |---|---|---|
/// | `window` | L1353 | `high_water` bytes -- the rest has never been written |
/// | `prev` | L1354-L1356 | `w_size` entries if the tables were slid, else `strstart - insert` |
/// | `head` | L1357 | all `hash_size` entries |
/// | `pending_buf` | L1359-L1360 | the `pending` bytes at the preserved `pending_out` offset |
/// | `sym_buf` | L1367-L1368 | the `sym_next` bytes written so far |
///
/// ★ The `prev` count is where the `slid` flag earns its place in the state: once the hash
/// tables have been slid, any entry may be live and the whole array has to come across.
/// [`DeflateState::copied_prev_entries`] is that expression.
///
/// ★ C finishes by repairing three pointers -- `ds->l_desc.dyn_tree = ds->dyn_ltree` and
/// its two siblings (L1371-L1373) -- because the bytewise copy left them aimed at the
/// *source's* arrays. This implementation has no analogue and needs none: `deflate/state.rs` models a
/// `TreeDesc` with a `StaticTreeKind` discriminant instead of a `dyn_tree` back-pointer, so
/// there is no pointer to repair and no way for a copy to keep referring to its original.
///
/// # Errors
///
/// [`ReturnCode::MEM_ERROR`] if any of the copy's four buffers cannot be allocated
/// (L1347-L1351). The source is left completely untouched, so a caller can keep using it --
/// which is what lets `test/infcover.c` force this failure with `mem_limit` and carry on.
pub fn deflate_copy<'a, 'b, A, B>(
    source: &DeflateState<'a, A>,
    allocator: B,
) -> Result<DeflateState<'b, B>, ReturnCode>
where
    'a: 'b,
    A: Allocator<'a> + Copy,
    B: Allocator<'b> + Copy,
{
    source.try_clone_in(allocator)
}

/// Inserts up to sixteen bits directly into the output bit stream.
///
/// The Rust counterpart of `deflatePrime` (`deflate.c` L745-L771), declared at `zlib.h` L816. Its
/// purpose, per `zlib.h` L820-L826, is "to start off the deflate output with the bits
/// leftover from a previous deflate stream when appending to it", so it applies to raw
/// deflate and must be used before the first [`deflate`] call after an initialisation or a
/// reset.
///
/// The guard is a conjunction of two things (L756-L758, the shipped non-`LIT_MEM` branch):
/// the `bits` range, checked here, and whether the pending buffer still has room ahead of
/// its read cursor for the two bytes a bit flush can write, which is
/// `crate::deflate::pending::has_prime_room`.
///
/// The loop moves `put = min(Buf_size - bi_valid, bits)` bits at a time and flushes after
/// each, which is what keeps `bi_valid` below 8 for every iteration after the first
/// (L760-L769). A `put` of zero is possible on the very first iteration, when a preceding
/// `send_bits` left the bit buffer exactly full at sixteen; that iteration moves nothing and
/// the flush it performs empties the buffer, so the next one makes progress. C behaves
/// identically, and the wasted iteration is reproduced rather than optimised away.
///
/// # Errors
///
/// [`ReturnCode::BUF_ERROR`] for a `bits` outside `0..=16`, or when the pending buffer's
/// read cursor has advanced too far for the bits to be inserted safely.
pub fn deflate_prime<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    bits: i32,
    value: i32,
) -> ReturnCode {
    // `if (bits < 0 || bits > 16 || s->sym_buf < s->pending_out + ((Buf_size + 7) >> 3))
    // return Z_BUF_ERROR;`  (L756-L758)
    //
    // The two comparisons keep C's spelling rather than becoming `!(0..=16).contains(&bits)`,
    // so that the guard reads exactly as `deflate.c` L756 does; the two are equivalent.
    #[allow(clippy::manual_range_contains)]
    if bits < 0 || bits > 16 || !has_prime_room(state) {
        return ReturnCode::BUF_ERROR;
    }

    let mut bits = bits;
    let mut value = value;

    // `do { ... } while (bits);`  (L760-L769)
    loop {
        // `put = Buf_size - s->bi_valid; if (put > bits) put = bits;`  (L761-L763)
        let put = (BUF_SIZE - state.bi_valid).min(bits);

        // `s->bi_buf |= (ush)((value & ((1 << put) - 1)) << s->bi_valid);`  (L764)
        //
        // The mask is non-negative, so the `&` yields a non-negative `i32` even for a
        // negative `value`, exactly as C's two's-complement `&` does. `put + bi_valid` is
        // at most `Buf_size`, so the shifted result fits in sixteen bits and the `(ush)`
        // narrowing loses nothing.
        let mask = (1_i32 << put) - 1;
        let chunk =
            u32::try_from(value & mask).unwrap_or(0) << u32::try_from(state.bi_valid).unwrap_or(0);
        state.bi_buf |= u16::try_from(chunk & 0xffff).unwrap_or(0);

        // `s->bi_valid += put;`  (L765)
        state.bi_valid += put;

        // `_tr_flush_bits(s);`  (L766)
        _tr_flush_bits(state);

        // `value >>= put; bits -= put;`  (L767-L768). Rust's `>>` on `i32` is arithmetic,
        // which is what gcc emits for C's implementation-defined signed shift.
        value >>= put;
        bits -= put;

        if bits == 0 {
            break;
        }
    }

    ReturnCode::OK
}

/// Reports the output that has been generated but not yet handed to the caller.
///
/// The Rust counterpart of `deflatePending` (`deflate.c` L722-L734), declared at `zlib.h` L786. The
/// three returned values are, in order, the pending **bytes**, the pending **bits** -- 0 to
/// 7, awaiting more bits to complete a byte -- and the status.
///
/// Both of C's out-parameters are optional (L724 and L726), and a facade with only one of
/// them simply discards the other value: neither computation has an effect, and C evaluates
/// the bits assignment before the bytes one regardless of which pointers are null.
///
/// The status is [`ReturnCode::BUF_ERROR`] in exactly one case, which `zlib.h` L798-L801
/// documents: "if an int is 16 bits and memLevel is 9, then it is possible for the number of
/// pending bytes to not fit in an unsigned", and then `*pending` is set to the maximum value
/// of an `unsigned` as well (L728-L731). One ordering subtlety survives from C and is
/// preserved by returning all three values together: the bit count is written *before* the
/// error is returned, so a caller that passed both pointers gets a valid count alongside it.
///
/// # Errors
///
/// [`ReturnCode::BUF_ERROR`] when the byte count does not fit in a C `unsigned`; see above.
#[must_use]
pub fn deflate_pending<'a, A: Allocator<'a>>(
    state: &DeflateState<'a, A>,
) -> (u32, i32, ReturnCode) {
    pending_snapshot(state)
}

/// Reports how many bits of the last byte were used at the most recent byte-boundary flush.
///
/// The Rust counterpart of `deflateUsed` (`deflate.c` L737-L742), declared at `zlib.h` L804. The value
/// is `bi_used`, which `bi_windup` maintains as `((s->bi_valid - 1) & 7) + 1` (`trees.c`
/// L187); `zlib.h` L807-L810 documents the range as "1..8, or 0 if there has not yet been a
/// flush" and explains its use -- it "helps determine the location of the last bit of a
/// deflate stream", which is what a caller needs before resuming with [`deflate_prime`].
///
/// There is no error path. C's out-parameter is optional (L739) and its only failure is the
/// `deflateStateCheck` at L738, whose remaining half cannot fail for a `&DeflateState`, so
/// this returns the number and the facade supplies the [`ReturnCode::OK`].
#[must_use]
pub fn deflate_used<'a, A: Allocator<'a>>(state: &DeflateState<'a, A>) -> i32 {
    used_bits(state)
}

#[cfg(test)]
mod tests {
    //! Unit tests for the deflate driver.
    //!
    //! These exercise the bytes this file itself decides -- the RFC 1950 header word, the
    //! RFC 1952 gzip header, the flush-rank gate, the entry guards, the `deflateBound`
    //! arithmetic and the `deflateEnd` contract -- plus round trips through
    //! `crate::inflate`. They are deliberately *not* a substitute for
    //! `crates/zlib-rs-differential/tests/byte_identical.rs`: only a comparison against the
    //! C oracle proves byte identity, and a round trip that merely decompresses proves
    //! nothing about it. What these do prove is that the specific constants and orderings
    //! this file is responsible for are the ones `deflate.c` computes.
    //!
    //! Per the crate lint policy these may use `unwrap()` and indexing freely.
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

    use super::{
        deflate, deflate_bound, deflate_bound_z, deflate_end, deflate_init2, deflate_params,
        deflate_reset, deflate_set_dictionary, deflate_state_check, DeflateState, DeflateStream,
        Status,
    };
    use crate::allocate::GlobalAllocator;
    use crate::config::{DeflateConfig, InflateConfig};
    use crate::error::ReturnCode;
    use crate::inflate::{inflate, inflate_end, inflate_init2, inflate_reset, InflateStream};
    use alloc::vec;
    use alloc::vec::Vec;

    /// `Z_NO_FLUSH` (`zlib.h` L172).
    const NO_FLUSH: i32 = 0;
    /// `Z_SYNC_FLUSH` (`zlib.h` L174).
    const SYNC_FLUSH: i32 = 2;
    /// `Z_BLOCK` (`zlib.h` L177).
    const BLOCK: i32 = 5;
    /// `Z_TREES` (`zlib.h` L178) -- an inflate-only flush that `deflate` must reject.
    const TREES: i32 = 6;
    /// `Z_FINISH` (`zlib.h` L176).
    const FINISH: i32 = 4;

    /// Builds a state for the given raw `deflateInit2_` arguments and resets it, returning
    /// the state together with the [`super::DeflateReset`] scalars a stream must adopt.
    fn open(
        level: i32,
        window_bits: i32,
        mem_level: i32,
        strategy: i32,
    ) -> (DeflateState<'static, GlobalAllocator>, super::DeflateReset) {
        let config = DeflateConfig::from_raw(level, 8, window_bits, mem_level, strategy).unwrap();
        let mut state = deflate_init2(config, GlobalAllocator).unwrap();
        let reset = deflate_reset(&mut state);
        (state, reset)
    }

    /// Compresses `input` in one call and returns the bytes, asserting `Z_STREAM_END`.
    fn compress(
        input: &[u8],
        level: i32,
        window_bits: i32,
        mem_level: i32,
        strategy: i32,
    ) -> Vec<u8> {
        let (mut state, reset) = open(level, window_bits, mem_level, strategy);
        let mut out = vec![0u8; deflate_bound_z(Some(&state), input.len()) + 64];
        let produced = {
            let mut stream = DeflateStream::new(input, &mut out);
            stream.apply_reset(reset);
            assert_eq!(
                deflate(&mut state, &mut stream, FINISH),
                ReturnCode::STREAM_END,
                "one-shot Z_FINISH must complete within deflate_bound"
            );
            stream.next_out
        };
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
        out.truncate(produced);
        out
    }

    /// Decompresses `input` through `crate::inflate` with the matching `window_bits`.
    fn decompress(input: &[u8], window_bits: i32, expected_len: usize) -> Vec<u8> {
        let mut state = inflate_init2(InflateConfig::new(window_bits), GlobalAllocator).unwrap();
        let reset = inflate_reset(&mut state);
        let mut out = vec![0u8; expected_len + 64];
        let produced = {
            let mut stream = InflateStream::new(input, &mut out);
            stream.apply_reset(reset);
            assert_eq!(
                inflate(&mut state, &mut stream, FINISH),
                ReturnCode::STREAM_END
            );
            stream.next_out
        };
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
        out.truncate(produced);
        out
    }

    #[test]
    fn zlib_header_word_matches_the_reference_for_every_level() {
        // Scope of this test, stated precisely: `windowBits == 15` and NO preset dictionary.
        // Under exactly those conditions `CMF` is 0x78, because it is
        // `Z_DEFLATED + ((w_bits - 8) << 4)` = `8 + (7 << 4)` (`deflate.c` L1021-L1024), and
        // `FLG` is `level_flags << 6` plus the mod-31 correction (L1026-L1035).
        //
        // Neither byte is fixed across configurations, so these pairs are NOT what every zlib
        // stream begins with:
        //   * `CMF` tracks the window size -- `windowBits == 9` gives `8 + (1 << 4)` = 0x18,
        //     and every exponent in between gives its own value.
        //   * `FLG` bit 5 is `FDICT`, set when `deflateSetDictionary` supplied a preset
        //     dictionary (`deflate.c` L1030-L1031), which also changes the mod-31 correction
        //     and appends the four-byte `DICTID`.
        // The pairs below are therefore the well-known ones for the DEFAULT configuration, which
        // is the configuration this test covers.
        for &(level, expected) in &[
            (1i32, [0x78u8, 0x01u8]),
            (2, [0x78, 0x5e]),
            (5, [0x78, 0x5e]),
            (6, [0x78, 0x9c]),
            (7, [0x78, 0xda]),
            (9, [0x78, 0xda]),
        ] {
            let out = compress(b"payload", level, 15, 8, 0);
            assert_eq!(&out[..2], &expected[..], "zlib header for level {level}");
            // "header += 31 - (header % 31);" (L1035) -- the two-byte big-endian word is
            // always a multiple of 31, which is what an inflate implementation checks.
            let header = (u32::from(out[0]) << 8) | u32::from(out[1]);
            assert_eq!(header % 31, 0, "header word must be a multiple of 31");
        }
    }

    #[test]
    fn zlib_header_level_flags_follow_the_reference_ladder() {
        // The ladder at L1028-L1033: `strategy >= Z_HUFFMAN_ONLY || level < 2` => 0,
        // `level < 6` => 1, `level == 6` => 2, else 3. Recovered from bits 6-7 of FLG.
        let flags = |level: i32, strategy: i32| -> u32 {
            let out = compress(b"payload", level, 15, 8, strategy);
            (u32::from(out[1]) >> 6) & 3
        };
        assert_eq!(flags(1, 0), 0, "level 1 is level_flags 0");
        assert_eq!(flags(2, 0), 1, "level 2 is level_flags 1");
        assert_eq!(flags(5, 0), 1, "level 5 is level_flags 1");
        assert_eq!(flags(6, 0), 2, "level 6 is level_flags 2");
        assert_eq!(flags(9, 0), 3, "level 9 is level_flags 3");
        // ★ Z_HUFFMAN_ONLY (2) forces level_flags to 0 even at level 6, because the
        // `strategy >= Z_HUFFMAN_ONLY` test comes FIRST in the ladder.
        assert_eq!(flags(6, 2), 0, "Z_HUFFMAN_ONLY at level 6 is level_flags 0");
        assert_eq!(flags(6, 3), 0, "Z_RLE at level 6 is level_flags 0");
        // ★ The test is `strategy >= Z_HUFFMAN_ONLY`, and the constants are
        // Z_FILTERED = 1, Z_HUFFMAN_ONLY = 2, Z_RLE = 3, Z_FIXED = 4 (`zlib.h` L200-L204).
        // So Z_RLE and Z_FIXED are *above* the threshold and also force 0; only Z_FILTERED
        // sits below it and lets the level decide. Confirmed against the oracle, which
        // emits FLG 0x9c for strategies 0-1 and 0x01 for strategies 2-4 at level 6.
        assert_eq!(
            flags(6, 1),
            2,
            "Z_FILTERED is below Z_HUFFMAN_ONLY, so the level decides"
        );
        assert_eq!(
            flags(6, 4),
            0,
            "Z_FIXED is above Z_HUFFMAN_ONLY, so level_flags is 0"
        );
    }

    #[test]
    fn zlib_header_records_the_window_size() {
        // `(w_bits - 8) << 4` in the high nibble of CMF (L1023). `windowBits == 8` is
        // promoted to 9 by deflate_init2 (L438), so 0x18 is the smallest CMF.
        for (window_bits, expected_cmf) in [(9i32, 0x18u8), (12, 0x48), (15, 0x78)] {
            let out = compress(b"payload", 6, window_bits, 8, 0);
            assert_eq!(out[0], expected_cmf, "CMF for windowBits {window_bits}");
            assert_eq!(
                ((u32::from(out[0]) << 8) | u32::from(out[1])) % 31,
                0,
                "mod-31 correction holds for windowBits {window_bits}"
            );
        }
    }

    #[test]
    fn preset_dict_is_set_exactly_when_strstart_is_non_zero() {
        // Without a dictionary `strstart == 0` at header time, so FDICT is clear.
        let plain = compress(b"hello, hello!", 6, 15, 8, 0);
        assert_eq!(plain[1] & 0x20, 0, "FDICT must be clear with no dictionary");

        // With one, `deflate_set_dictionary` leaves `strstart != 0`, so FDICT is set and
        // the four Adler-32 bytes of the dictionary follow the two header bytes (L1038).
        let (mut state, reset) = open(6, 15, 8, 0);
        let mut adler = reset.adler;
        let mut total_in = reset.total_in;
        assert_eq!(
            deflate_set_dictionary(&mut state, &mut adler, &mut total_in, b"hello"),
            ReturnCode::OK
        );
        assert_ne!(
            state.window.strstart, 0,
            "a dictionary must advance strstart"
        );
        let dict_adler = adler;

        let mut out = vec![0u8; 512];
        let produced = {
            let mut stream = DeflateStream::new(b"hello, hello!", &mut out);
            stream.apply_reset(reset);
            stream.adler = adler;
            stream.total_in = total_in;
            assert_eq!(
                deflate(&mut state, &mut stream, FINISH),
                ReturnCode::STREAM_END
            );
            stream.next_out
        };
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
        out.truncate(produced);

        assert_ne!(out[1] & 0x20, 0, "FDICT must be set when strstart != 0");
        let header = (u32::from(out[0]) << 8) | u32::from(out[1]);
        assert_eq!(
            header % 31,
            0,
            "mod-31 correction still holds with FDICT set"
        );
        // DICTID is the dictionary's Adler-32, most significant byte first (L1038-L1040).
        assert_eq!(
            &out[2..6],
            &dict_adler.to_be_bytes()[..],
            "DICTID must be the dictionary's Adler-32, MSB first"
        );
    }

    //  The RFC 1952 gzip header with no installed gz_header
    //  (deflate.c L1053-L1075)

    #[test]
    fn gzip_header_without_a_header_struct_is_exactly_ten_bytes() {
        // "if (s->gzhead == Z_NULL)" (L1057): magic 31, 139, method 8, then FLG 0,
        // four MTIME zero bytes, XFL, OS_CODE. Ten bytes, then the deflate data.
        for &(level, strategy, xfl) in &[
            (9i32, 0i32, 2u8),
            (1, 0, 4),
            (0, 0, 4),
            (6, 2, 4),
            (6, 3, 4),
            (2, 0, 0),
            (6, 0, 0),
            (8, 0, 0),
        ] {
            let out = compress(b"payload", level, 31, 8, strategy);
            assert_eq!(
                &out[..10],
                &[31u8, 139, 8, 0, 0, 0, 0, 0, xfl, super::OS_CODE][..],
                "gzip header for level {level} strategy {strategy}"
            );
        }
    }

    #[test]
    fn gzip_trailer_is_the_crc_then_the_length_little_endian() {
        // L1240-L1248: four bytes of CRC-32 then four of `total_in`, both LSB first.
        let payload = b"hello, hello!";
        let out = compress(payload, 6, 31, 8, 0);
        let trailer = &out[out.len() - 8..];
        let expected_crc = crate::crc32::crc32(0, payload);
        assert_eq!(
            &trailer[..4],
            &expected_crc.to_le_bytes()[..],
            "gzip CRC-32"
        );
        assert_eq!(
            &trailer[4..],
            &u32::try_from(payload.len()).unwrap().to_le_bytes()[..],
            "gzip ISIZE"
        );
    }

    #[test]
    fn zlib_trailer_is_the_adler32_most_significant_byte_first() {
        // L1250-L1253: `putShortMSB(adler >> 16)` then `putShortMSB(adler & 0xffff)`.
        let payload = b"hello, hello!";
        let out = compress(payload, 6, 15, 8, 0);
        let expected = crate::adler32::adler32(super::ADLER32_INITIAL_VALUE, payload);
        assert_eq!(
            &out[out.len() - 4..],
            &expected.to_be_bytes()[..],
            "zlib Adler-32 trailer, MSB first"
        );
    }

    #[test]
    fn a_raw_stream_has_no_header_and_no_trailer() {
        // `wrap == 0` skips the header (L1019-L1020) and returns Z_STREAM_END before the
        // trailer (L1237). A raw stream of "payload" is therefore shorter than the zlib
        // one by exactly the 2 header plus 4 trailer bytes.
        let raw = compress(b"payload", 6, -15, 8, 0);
        let zlib = compress(b"payload", 6, 15, 8, 0);
        assert_eq!(
            zlib.len(),
            raw.len() + 6,
            "zlib adds 2 header + 4 trailer bytes"
        );
        assert_eq!(
            &zlib[2..zlib.len() - 4],
            &raw[..],
            "the deflate data is identical"
        );
    }

    #[test]
    fn an_out_of_range_flush_is_rejected() {
        // "if (deflateStateCheck(strm) || flush > Z_BLOCK || flush < 0)" (L986). ★ The
        // `> Z_BLOCK` half rejects Z_TREES, which is an inflate-only flush.
        for flush in [TREES, 7, i32::MAX, -1, -2, i32::MIN] {
            let (mut state, reset) = open(6, 15, 8, 0);
            let mut out = vec![0u8; 256];
            {
                let mut stream = DeflateStream::new(b"payload", &mut out);
                stream.apply_reset(reset);
                assert_eq!(
                    deflate(&mut state, &mut stream, flush),
                    ReturnCode::STREAM_ERROR,
                    "flush {flush} must be rejected"
                );
                // L986 returns before `msg` is touched, unlike the guards at L990-L995.
                assert!(stream.msg.is_none(), "flush {flush} must not set msg");
            }
            assert_eq!(deflate_end(&mut state), ReturnCode::OK);
        }
    }

    #[test]
    fn a_zero_length_output_buffer_is_a_buf_error() {
        // "if (strm->avail_out == 0) ERR_RETURN(strm, Z_BUF_ERROR);" (L995).
        let (mut state, reset) = open(6, 15, 8, 0);
        let mut out: [u8; 0] = [];
        {
            let mut stream = DeflateStream::new(b"payload", &mut out);
            stream.apply_reset(reset);
            assert_eq!(
                deflate(&mut state, &mut stream, NO_FLUSH),
                ReturnCode::BUF_ERROR
            );
            assert!(stream.msg.is_some(), "ERR_RETURN sets msg");
        }
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn a_repeated_flush_with_no_input_is_a_buf_error_but_z_block_after_no_flush_is_not() {
        // The gate at L1008-L1011:
        //   avail_in == 0 && RANK(flush) <= RANK(old_flush) && flush != Z_FINISH
        // where RANK(f) = f * 2 - (f > 4 ? 9 : 0), so RANK(Z_NO_FLUSH) = 0,
        // RANK(Z_SYNC_FLUSH) = 4 and RANK(Z_BLOCK) = 1.
        let (mut state, reset) = open(6, 15, 8, 0);
        let mut out = vec![0u8; 4096];
        let payload = b"hello, hello!";
        let mut cursor: usize;

        // First Z_SYNC_FLUSH consumes the input and emits an empty stored block.
        {
            let mut stream = DeflateStream::new(payload, &mut out);
            stream.apply_reset(reset);
            assert_eq!(deflate(&mut state, &mut stream, SYNC_FLUSH), ReturnCode::OK);
            assert_eq!(stream.avail_in(), 0, "Z_SYNC_FLUSH consumes all input");
            cursor = stream.next_out;
        }
        // ★ A second identical flush with no new input: RANK(2) <= RANK(2), so Z_BUF_ERROR.
        let after_first = cursor;
        {
            let mut stream = DeflateStream::new(payload, &mut out);
            stream.next_in = payload.len();
            stream.next_out = cursor;
            assert_eq!(
                deflate(&mut state, &mut stream, SYNC_FLUSH),
                ReturnCode::BUF_ERROR,
                "a duplicate Z_SYNC_FLUSH with no input must be Z_BUF_ERROR"
            );
            cursor = stream.next_out;
        }
        assert_eq!(cursor, after_first, "the rejected call must emit nothing");
        // Freed from BUSY_STATE, so Z_DATA_ERROR (L1309) -- verified against the oracle.
        assert_eq!(deflate_end(&mut state), ReturnCode::DATA_ERROR);

        // Z_BLOCK after Z_NO_FLUSH is allowed, because RANK(Z_BLOCK) = 1 > 0 = RANK(Z_NO_FLUSH).
        let (mut state, reset) = open(6, 15, 8, 0);
        let mut out = vec![0u8; 4096];
        let primed = {
            let mut stream = DeflateStream::new(payload, &mut out);
            stream.apply_reset(reset);
            assert_eq!(deflate(&mut state, &mut stream, NO_FLUSH), ReturnCode::OK);
            (stream.next_in, stream.next_out)
        };
        {
            let (consumed, written) = primed;
            let mut stream = DeflateStream::new(payload, &mut out);
            stream.next_in = consumed;
            stream.next_out = written;
            assert_eq!(
                deflate(&mut state, &mut stream, BLOCK),
                ReturnCode::OK,
                "Z_BLOCK outranks Z_NO_FLUSH and must not be rejected"
            );
        }
        assert_eq!(deflate_end(&mut state), ReturnCode::DATA_ERROR);
    }

    #[test]
    fn more_input_after_z_finish_is_a_buf_error() {
        // "if (s->status == FINISH_STATE && strm->avail_in != 0) ERR_RETURN(...)" (L1015).
        let payload = b"hello, hello!";
        let (mut state, reset) = open(6, 15, 8, 0);
        let mut out = vec![0u8; 4096];
        {
            let mut stream = DeflateStream::new(payload, &mut out);
            stream.apply_reset(reset);
            assert_eq!(
                deflate(&mut state, &mut stream, FINISH),
                ReturnCode::STREAM_END
            );
        }
        {
            // Now in FINISH_STATE with input still visible.
            let mut stream = DeflateStream::new(payload, &mut out);
            stream.next_out = 2048;
            assert_eq!(
                deflate(&mut state, &mut stream, FINISH),
                ReturnCode::BUF_ERROR
            );
            assert!(stream.msg.is_some());
        }
        // And a non-Z_FINISH flush in FINISH_STATE is a stream error (L992).
        {
            let mut stream = DeflateStream::new(&[], &mut out);
            stream.next_out = 2048;
            assert_eq!(
                deflate(&mut state, &mut stream, NO_FLUSH),
                ReturnCode::STREAM_ERROR
            );
        }
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn state_check_accepts_exactly_the_eight_legal_status_values() {
        // The status half of `deflateStateCheck` (L546-L555). `deflate_state_check`
        // returns `true` for REJECT, so every legal value must produce `false`.
        for status in Status::ALL {
            assert!(
                !deflate_state_check(status.as_raw()),
                "{status:?} ({}) is one of the eight legal values",
                status.as_raw()
            );
        }
        // Everything else is rejected, including the values that sit between the legal
        // ones and the obvious zero/negative sentinels.
        for raw in [
            0i32,
            1,
            41,
            43,
            56,
            58,
            68,
            70,
            72,
            74,
            90,
            92,
            102,
            104,
            112,
            114,
            665,
            667,
            -1,
            i32::MIN,
            i32::MAX,
        ] {
            assert!(
                deflate_state_check(raw),
                "{raw} is not a legal deflate status"
            );
        }
    }

    #[test]
    fn deflate_end_reports_data_error_only_from_busy_state() {
        // "return s->status == BUSY_STATE ? Z_DATA_ERROR : Z_OK;" (L1309). The C-visible
        // return code is this function's responsibility even though `Drop` owns the memory.

        // INIT_STATE: nothing has been compressed, so Z_OK.
        let (mut state, _reset) = open(6, 15, 8, 0);
        assert_eq!(state.status, Status::Init);
        assert_eq!(
            deflate_end(&mut state),
            ReturnCode::OK,
            "Z_OK from INIT_STATE"
        );

        // GZIP_STATE is likewise not BUSY.
        let (mut state, _reset) = open(6, 31, 8, 0);
        assert_eq!(state.status, Status::GzipHeader);
        assert_eq!(
            deflate_end(&mut state),
            ReturnCode::OK,
            "Z_OK from GZIP_STATE"
        );

        // BUSY_STATE: a stream abandoned mid-compression. `test/infcover.c` exercises this.
        let (mut state, reset) = open(6, 15, 8, 0);
        let mut out = vec![0u8; 4096];
        {
            let mut stream = DeflateStream::new(b"hello, hello!", &mut out);
            stream.apply_reset(reset);
            assert_eq!(deflate(&mut state, &mut stream, NO_FLUSH), ReturnCode::OK);
        }
        assert_eq!(state.status, Status::Busy);
        assert_eq!(
            deflate_end(&mut state),
            ReturnCode::DATA_ERROR,
            "Z_DATA_ERROR when a stream is freed from BUSY_STATE"
        );

        // FINISH_STATE: the stream completed, so Z_OK.
        let (mut state, reset) = open(6, 15, 8, 0);
        let mut out = vec![0u8; 4096];
        {
            let mut stream = DeflateStream::new(b"hello, hello!", &mut out);
            stream.apply_reset(reset);
            assert_eq!(
                deflate(&mut state, &mut stream, FINISH),
                ReturnCode::STREAM_END
            );
        }
        assert_eq!(state.status, Status::Finish);
        assert_eq!(
            deflate_end(&mut state),
            ReturnCode::OK,
            "Z_OK from FINISH_STATE"
        );
    }

    /// Runs `deflate_params` on a state and reports whether the driver was re-entered with
    /// `Z_BLOCK`, which `last_flush` records exactly (L998).
    fn params_flushed(
        state: &mut DeflateState<'static, GlobalAllocator>,
        out: &mut [u8],
        at: usize,
        level: i32,
        strategy: i32,
    ) -> (ReturnCode, bool, usize) {
        let empty: &[u8] = &[];
        let mut stream = DeflateStream::new(empty, out);
        stream.next_out = at;
        let code = deflate_params(state, &mut stream, level, strategy);
        (code, state.last_flush == BLOCK, stream.next_out)
    }

    #[test]
    fn params_re_enters_the_driver_only_when_it_must() {
        let payload = b"aaaaaaaaaabbbbbbbbbbccccccccccdddddddddd".repeat(20);

        // (1) `last_flush == -2` -- deflate has not been called since the reset -- so even
        // a genuine function change must NOT re-enter the driver (the `&& s->last_flush !=
        // -2` half of L791).
        let (mut state, _reset) = open(6, 15, 8, 0);
        assert_eq!(state.last_flush, -2, "reset seeds last_flush with -2");
        let mut out = vec![0u8; 1 << 16];
        let (code, flushed, written) = params_flushed(&mut state, &mut out, 0, 1, 0);
        assert_eq!(code, ReturnCode::OK);
        assert!(!flushed, "no Z_BLOCK before the first deflate call");
        assert_eq!(written, 0, "and therefore nothing emitted");
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);

        // A helper that compresses a first segment so that `last_flush == Z_NO_FLUSH`.
        let prime = |level: i32, strategy: i32| {
            let (mut state, reset) = open(level, 15, 8, strategy);
            let mut out = vec![0u8; 1 << 16];
            let at = {
                let mut stream = DeflateStream::new(&payload, &mut out);
                stream.apply_reset(reset);
                assert_eq!(deflate(&mut state, &mut stream, NO_FLUSH), ReturnCode::OK);
                stream.next_out
            };
            (state, out, at)
        };

        // (2) The level's function changes (deflate_slow -> deflate_fast), so the driver is
        // re-entered with Z_BLOCK and the pending block is emitted.
        let (mut state, mut out, at) = prime(6, 0);
        let (code, flushed, written) = params_flushed(&mut state, &mut out, at, 1, 0);
        assert_eq!(code, ReturnCode::OK);
        assert!(
            flushed,
            "slow -> fast changes the function, so Z_BLOCK is issued"
        );
        assert!(written > at, "the Z_BLOCK must emit the pending block");
        // Every primed state below is still in BUSY_STATE, so `deflateEnd` reports
        // Z_DATA_ERROR (L1309); only the un-primed state in case (1) returns Z_OK.
        assert_eq!(deflate_end(&mut state), ReturnCode::DATA_ERROR);

        // (3) Same function (levels 4-9 all map to deflate_slow) and same strategy, so no
        // re-entry: `func != configuration_table[level].func` is false.
        let (mut state, mut out, at) = prime(6, 0);
        let (code, flushed, written) = params_flushed(&mut state, &mut out, at, 5, 0);
        assert_eq!(code, ReturnCode::OK);
        assert!(!flushed, "level 6 -> 5 keeps deflate_slow, so no Z_BLOCK");
        assert_eq!(written, at, "and nothing is emitted");
        assert_eq!(deflate_end(&mut state), ReturnCode::DATA_ERROR);

        // (4) A strategy change alone is enough, even with the function unchanged, because
        // the test is `strategy != s->strategy || func != ...`.
        let (mut state, mut out, at) = prime(6, 0);
        let (code, flushed, _written) = params_flushed(&mut state, &mut out, at, 6, 1);
        assert_eq!(code, ReturnCode::OK);
        assert!(flushed, "a strategy change alone triggers Z_BLOCK");
        assert_eq!(deflate_end(&mut state), ReturnCode::DATA_ERROR);

        // (5) Out-of-range arguments are rejected before anything else (L784-L788).
        let (mut state, mut out, at) = prime(6, 0);
        for &(level, strategy) in &[(-2i32, 0i32), (10, 0), (6, -1), (6, 5)] {
            let (code, _flushed, _written) =
                params_flushed(&mut state, &mut out, at, level, strategy);
            assert_eq!(
                code,
                ReturnCode::STREAM_ERROR,
                "deflateParams({level}, {strategy}) must be Z_STREAM_ERROR"
            );
        }
        // Z_DEFAULT_COMPRESSION is normalised to 6 rather than rejected (L784-L785).
        let (code, _flushed, _written) = params_flushed(&mut state, &mut out, at, -1, 0);
        assert_eq!(
            code,
            ReturnCode::OK,
            "Z_DEFAULT_COMPRESSION becomes level 6"
        );
        assert_eq!(state.level(), 6);
        assert_eq!(deflate_end(&mut state), ReturnCode::DATA_ERROR);
    }

    //  deflateBound / deflateBound_z (deflate.c L856-L937)
    //
    //  Every expectation below was hand-computed from the C arithmetic and then
    //  confirmed against the in-tree oracle, because a bound that is too small is a
    //  buffer overflow in CALLER code and one that is too large breaks callers that
    //  assert exact sizes.

    #[test]
    fn bound_uses_the_tight_default_formula_for_default_parameters() {
        // `w_bits == 15 && hash_bits == 8 + 7` takes the tight branch (L923-L926):
        //   sourceLen + (sourceLen >> 12) + (sourceLen >> 14) + (sourceLen >> 25)
        //     + 13 - 6 + wraplen
        // with wraplen 6 for zlib, 0 for raw and 18 for gzip (L882-L913).

        // sourceLen 0: 0 + 0 + 0 + 0 + 13 - 6 + 6 = 13.
        let (mut state, _r) = open(6, 15, 8, 0);
        assert_eq!(deflate_bound_z(Some(&state), 0), 13, "zlib, sourceLen 0");
        // 13 + 0 + 0 + 0 + 13 - 6 + 6 = 26.
        assert_eq!(deflate_bound_z(Some(&state), 13), 26, "zlib, sourceLen 13");
        // 1_000_000 + 244 + 61 + 0 + 13 - 6 + 6 = 1_000_318.
        assert_eq!(
            deflate_bound_z(Some(&state), 1_000_000),
            1_000_318,
            "zlib, sourceLen 1000000"
        );
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);

        // Raw: wraplen 0, so 0 + 13 - 6 + 0 = 7.
        let (mut state, _r) = open(6, -15, 8, 0);
        assert_eq!(deflate_bound_z(Some(&state), 0), 7, "raw, sourceLen 0");
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);

        // Gzip with no installed header: wraplen 18, so 0 + 13 - 6 + 18 = 25.
        let (mut state, _r) = open(6, 31, 8, 0);
        assert_eq!(deflate_bound_z(Some(&state), 0), 25, "gzip, sourceLen 0");
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn bound_adds_four_bytes_once_a_dictionary_has_advanced_strstart() {
        // `case 1: wraplen = 6 + (s->strstart ? 4 : 0);` (L886) -- the four DICTID bytes.
        let (mut state, reset) = open(6, 15, 8, 0);
        let mut adler = reset.adler;
        let mut total_in = reset.total_in;
        assert_eq!(
            deflate_bound_z(Some(&state), 13),
            26,
            "before the dictionary"
        );
        assert_eq!(
            deflate_set_dictionary(&mut state, &mut adler, &mut total_in, b"hello"),
            ReturnCode::OK
        );
        assert_eq!(
            deflate_bound_z(Some(&state), 13),
            30,
            "after the dictionary: +4"
        );
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn bound_falls_back_to_a_conservative_formula_for_non_default_parameters() {
        // `if (s->w_bits != 15 || s->hash_bits != 8 + 7)` (L919-L922) picks
        //   fixedlen when `w_bits <= hash_bits && level != 0`, else storelen
        // where fixedlen = sl + (sl>>3) + (sl>>8) + (sl>>9) + 4
        //   and storelen = sl + (sl>>5) + (sl>>7) + (sl>>11) + 7.

        // memLevel 1 => hash_bits 8; `15 <= 8` is false, so storelen:
        //   1000 + 31 + 7 + 0 + 7 = 1045, plus wraplen 6 = 1051.
        let (mut state, _r) = open(6, 15, 1, 0);
        assert_eq!(
            deflate_bound_z(Some(&state), 1000),
            1051,
            "memLevel 1 uses storelen"
        );
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);

        // windowBits 9 with memLevel 8 => w_bits 9 <= hash_bits 15 and level != 0, so
        // fixedlen: 1000 + 125 + 3 + 1 + 4 = 1133, plus wraplen 6 = 1139.
        let (mut state, _r) = open(6, 9, 8, 0);
        assert_eq!(
            deflate_bound_z(Some(&state), 1000),
            1139,
            "windowBits 9 uses fixedlen"
        );
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);

        // ★ The same parameters at level 0 fall back to storelen, because the condition
        // includes `&& s->level`: 1045 + 6 = 1051.
        let (mut state, _r) = open(0, 9, 8, 0);
        assert_eq!(
            deflate_bound_z(Some(&state), 1000),
            1051,
            "level 0 uses storelen"
        );
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn bound_without_parameters_returns_the_larger_bound_plus_a_wrapper() {
        // "if can't get parameters, return larger bound plus a wrapper" (L878-L881):
        //   max(fixedlen, storelen) + 18. For 1000: max(1133, 1045) + 18 = 1151.
        assert_eq!(deflate_bound_z::<GlobalAllocator>(None, 1000), 1151);
        // sourceLen 0: max(4, 7) + 18 = 25.
        assert_eq!(deflate_bound_z::<GlobalAllocator>(None, 0), 25);
    }

    #[test]
    fn bound_saturates_instead_of_wrapping() {
        // All four `(z_size_t)-1` guards (L865, L872, L880, L927). Reproduced even where
        // they cannot trigger on a 64-bit target, for portability parity; `usize::MAX`
        // makes every one of them fire.
        assert_eq!(
            deflate_bound_z::<GlobalAllocator>(None, usize::MAX),
            usize::MAX,
            "invalid state saturates rather than wrapping"
        );
        let (mut state, _r) = open(6, 15, 8, 0);
        assert_eq!(
            deflate_bound_z(Some(&state), usize::MAX),
            usize::MAX,
            "the tight default branch saturates too"
        );
        // And the conservative branch.
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
        let (mut state, _r) = open(6, 15, 1, 0);
        assert_eq!(deflate_bound_z(Some(&state), usize::MAX), usize::MAX);
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn bound_and_bound_z_agree() {
        // `deflateBound` is `deflateBound_z` narrowed to `uLong` (L929-L932).
        let (mut state, _r) = open(6, 15, 8, 0);
        for source_len in [0u64, 1, 13, 1000, 65_536, 1_000_000] {
            assert_eq!(
                deflate_bound(Some(&state), source_len),
                deflate_bound_z(Some(&state), usize::try_from(source_len).unwrap()) as u64,
                "bound and bound_z must agree at {source_len}"
            );
        }
        assert_eq!(deflate_bound(Some(&state), 0), 13);
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    }

    // 3 window forms x 10 levels x 4 payloads, one of them 4 KiB, is 120 whole compressions.
    // Miri interprets rather than executes, at roughly four orders of magnitude the cost, so this
    // one test would dominate a Miri job several times over -- and it can contribute nothing to it:
    // the crate carries `#![forbid(unsafe_code)]`, so the only faults Miri can surface here are
    // integer overflow and an out-of-bounds index, and the compressors are walked end to end by
    // the cheaper tests in this module which do run in full. Skipped under Miri only, following the
    // convention `adler32/combine.rs` documents for its whole-corpus tests; nothing is skipped
    // under a normal `cargo test`.
    #[cfg_attr(
        miri,
        ignore = "120 whole compressions; bound agreement, no UB coverage"
    )]
    #[test]
    fn a_one_call_finish_always_fits_inside_the_bound() {
        // The contract at `zlib.h` L338-L343: a single `Z_FINISH` call succeeds when the
        // output buffer is at least `deflateBound` bytes. `compress` allocates exactly
        // `deflate_bound_z(..)` (plus slack it never needs) and asserts Z_STREAM_END, so
        // this checks the produced size really is within the bound -- including the
        // empty-input case, where the bound is a pure constant.
        for &window_bits in &[-15i32, 15, 31] {
            for level in 0..=9 {
                for &payload in &[
                    &b""[..],
                    &b"a"[..],
                    &b"hello, hello!"[..],
                    &[0x5au8; 4096][..],
                ] {
                    let (mut state, _r) = open(level, window_bits, 8, 0);
                    let bound = deflate_bound_z(Some(&state), payload.len());
                    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
                    let out = compress(payload, level, window_bits, 8, 0);
                    assert!(
                        out.len() <= bound,
                        "level {level} wb {window_bits} len {}: produced {} > bound {bound}",
                        payload.len(),
                        out.len()
                    );
                }
            }
        }
    }

    #[cfg_attr(
        miri,
        ignore = "900 whole round trips; behavioural agreement, no UB coverage"
    )]
    #[test]
    fn every_level_and_container_round_trips() {
        // 6 window forms x 10 levels x 5 strategies x 3 memory levels over a 4 KiB payload is 900
        // whole round trips. Under Miri -- which interprets rather than executes, at roughly four
        // orders of magnitude the cost -- that is hours for a test that can surface nothing Miri
        // looks for: the crate carries `#![forbid(unsafe_code)]`, so only integer overflow and an
        // out-of-bounds index are reachable, and both are debug-profile panics that this same test
        // catches under a normal `cargo test`. The compressors are still walked end to end under
        // Miri by the cheaper cases in this module. Skipped under Miri only, following the
        // convention `adler32/combine.rs` documents; nothing is skipped otherwise.
        //
        // A payload with runs, text and a long-distance repeat, so that every strategy
        // function has something to find.
        let mut payload = Vec::new();
        payload.extend_from_slice(b"the quick brown fox jumps over the lazy dog\n");
        payload.extend(core::iter::repeat(b'z').take(1000));
        payload.extend_from_slice(&(0u32..3000).map(|i| (i % 7) as u8).collect::<Vec<u8>>());
        payload.extend_from_slice(b"the quick brown fox jumps over the lazy dog\n");

        for &window_bits in &[-15i32, -9, 9, 15, 25, 31] {
            for level in 0..=9 {
                for &strategy in &[0i32, 1, 2, 3, 4] {
                    for &mem_level in &[1i32, 8, 9] {
                        let deflated = compress(&payload, level, window_bits, mem_level, strategy);
                        let restored = decompress(&deflated, window_bits, payload.len());
                        assert_eq!(
                            restored, payload,
                            "round trip level {level} wb {window_bits} ml {mem_level} strategy {strategy}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn the_example_c_dictionary_case_round_trips() {
        // The exact payload and dictionary from `test/example.c`'s
        // `test_dict_deflate`/`test_dict_inflate`. Note that example.c passes
        // `sizeof(dictionary)`, i.e. six bytes INCLUDING the NUL, so that is what is used.
        const HELLO: &[u8] = b"hello, hello!";
        const DICTIONARY: &[u8] = b"hello\0";

        let (mut state, reset) = open(9, 15, 8, 0);
        let mut adler = reset.adler;
        let mut total_in = reset.total_in;
        assert_eq!(
            deflate_set_dictionary(&mut state, &mut adler, &mut total_in, DICTIONARY),
            ReturnCode::OK
        );
        let dict_adler = adler;
        // `deflateSetDictionary` loads the dictionary through `fill_window`/`read_buf`,
        // which counts it in `total_in` (confirmed against the oracle: six bytes here).
        assert_eq!(total_in, DICTIONARY.len() as u64);

        let mut out = vec![0u8; 512];
        let produced = {
            let mut stream = DeflateStream::new(HELLO, &mut out);
            stream.apply_reset(reset);
            stream.adler = adler;
            stream.total_in = total_in;
            assert_eq!(
                deflate(&mut state, &mut stream, FINISH),
                ReturnCode::STREAM_END
            );
            stream.next_out
        };
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);
        out.truncate(produced);

        // Now inflate it: the FDICT bit makes `inflate` stop with Z_NEED_DICT and report
        // the dictionary's Adler-32, exactly as `test_dict_inflate` expects.
        let mut istate = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        let ireset = inflate_reset(&mut istate);
        let mut restored = vec![0u8; 256];
        let produced = {
            let mut stream = InflateStream::new(&out, &mut restored);
            stream.apply_reset(ireset);
            assert_eq!(
                inflate(&mut istate, &mut stream, NO_FLUSH),
                ReturnCode::NEED_DICT,
                "a preset-dictionary stream must ask for the dictionary"
            );
            assert_eq!(stream.adler, Some(dict_adler), "and report its Adler-32");
            assert_eq!(
                crate::inflate::inflate_set_dictionary(&mut istate, DICTIONARY),
                ReturnCode::OK
            );
            assert_eq!(
                inflate(&mut istate, &mut stream, NO_FLUSH),
                ReturnCode::STREAM_END
            );
            stream.next_out
        };
        assert_eq!(inflate_end(&mut istate), ReturnCode::OK);
        restored.truncate(produced);
        assert_eq!(
            restored, HELLO,
            "the dictionary round trip must restore the payload"
        );
    }

    #[test]
    fn a_sync_flush_boundary_is_recoverable_by_the_decompressor() {
        // Z_SYNC_FLUSH must leave the output at a byte boundary with an empty stored block,
        // so a decompressor can read everything written so far (L1222-L1223 emits
        // `_tr_stored_block` for every flush but Z_PARTIAL_FLUSH and Z_BLOCK).
        let first = b"the quick brown fox ";
        let second = b"jumps over the lazy dog";
        let (mut state, reset) = open(6, 15, 8, 0);
        let mut out = vec![0u8; 4096];

        let at_flush = {
            let mut stream = DeflateStream::new(first, &mut out);
            stream.apply_reset(reset);
            assert_eq!(deflate(&mut state, &mut stream, SYNC_FLUSH), ReturnCode::OK);
            stream.next_out
        };
        // The sync marker ends every Z_SYNC_FLUSH: an empty stored block, 00 00 FF FF.
        assert_eq!(
            &out[at_flush - 4..at_flush],
            &[0x00u8, 0x00, 0xff, 0xff][..],
            "Z_SYNC_FLUSH must end with the empty stored block"
        );

        let total = {
            let mut whole = Vec::from(&first[..]);
            whole.extend_from_slice(second);
            let mut stream = DeflateStream::new(&whole, &mut out);
            stream.next_in = first.len();
            stream.next_out = at_flush;
            // `total_in`/`adler` must carry over; the compressor keeps the rest in `state`.
            stream.total_in = first.len() as u64;
            stream.adler = crate::adler32::adler32(super::ADLER32_INITIAL_VALUE, first);
            assert_eq!(
                deflate(&mut state, &mut stream, FINISH),
                ReturnCode::STREAM_END
            );
            stream.next_out
        };
        assert_eq!(deflate_end(&mut state), ReturnCode::OK);

        let mut expected = Vec::from(&first[..]);
        expected.extend_from_slice(second);
        assert_eq!(decompress(&out[..total], 15, expected.len()), expected);
    }
}
