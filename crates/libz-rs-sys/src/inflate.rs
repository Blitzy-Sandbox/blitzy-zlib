//! The eighteen exported `inflate*` entry points, plus the one internal symbol
//! that the unmodified test suite forces this library to publish.
//!
//! This is the decompression half of the C ABI facade. It contributes no decoding
//! logic: every state transition, every table lookup and every check-value update
//! lives in [`zlib_rs::inflate`], which is compiled under
//! `#![forbid(unsafe_code)]`. What this module contributes is the boundary —
//! validating a caller's [`z_streamp`], rebuilding its two pointer/length pairs as
//! slices exactly once, recovering the opaque state with a tag check, and writing
//! the results back into the caller's [`z_stream`] in the same places C writes
//! them.
//!
//! # The exported surface
//!
//! Eighteen functions, matching the reference library's dynamic symbol table
//! exactly. The `inflateBack` family belongs to `infback.rs`, and the two
//! `inflateInit`/`inflateInit2` spellings are **macros** in `zlib.h` (L382 and
//! L859) over the `_`-suffixed real functions, so they are deliberately not
//! exported:
//!
//! | Symbol | `zlib.h` | Returns | Ported from |
//! |---|---|---|---|
//! | [`inflateInit_`] | L1905 | `int` | `inflate.c` L214-L217 |
//! | [`inflateInit2_`] | L1911 | `int` | `inflate.c` L173-L212 |
//! | [`inflate`] | L405 | `int` | `inflate.c` L474-L1153 |
//! | [`inflateEnd`] | L525 | `int` | `inflate.c` L1155-L1165 |
//! | [`inflateSetDictionary`] | L913 | `int` | `inflate.c` L1187-L1217 |
//! | [`inflateGetDictionary`] | L936 | `int` | `inflate.c` L1167-L1185 |
//! | [`inflateSync`] | L951 | `int` | `inflate.c` L1264-L1310 |
//! | [`inflateSyncPoint`] | L2034 | `int` | `inflate.c` L1320-L1326 |
//! | [`inflateCopy`] | L970 | `int` | `inflate.c` L1328-L1368 |
//! | [`inflateReset`] | L986 | `int` | `inflate.c` L125-L134 |
//! | [`inflateReset2`] | L997 | `int` | `inflate.c` L136-L171 |
//! | [`inflateResetKeep`] | L2039 | `int` | `inflate.c` L99-L123 |
//! | [`inflatePrime`] | L1011 | `int` | `inflate.c` L219-L236 |
//! | [`inflateMark`] | L1042 | **`long`** | `inflate.c` L1397-L1406 |
//! | [`inflateGetHeader`] | L1070 | `int` | `inflate.c` L1219-L1231 |
//! | [`inflateUndermine`] | L2036 | `int` | `inflate.c` L1370-L1382 |
//! | [`inflateValidate`] | L2037 | `int` | `inflate.c` L1384-L1395 |
//! | [`inflateCodesUsed`] | L2038 | **`unsigned long`** | `inflate.c` L1408-L1413 |
//!
//! Note the two non-`int` returns. [`inflateMark`] answers a `long`
//! ([`c_long`]) and [`inflateCodesUsed`] an `unsigned long` ([`c_ulong`]); both
//! are eight bytes on LP64 and four on LLP64 Windows, so they are spelled with
//! [`core::ffi`] aliases and never with a fixed-width integer. Every export is
//! `extern "C"` and never `extern "C-unwind"`.
//!
//! # ★ The nineteenth symbol: `inflate_table`
//!
//! `inflate_table` is not part of the public API — `zlib.map`'s `ZLIB_1.2.0`
//! `local:` block (L9-L19) names it explicitly, and it is absent from the
//! reference shared library's dynamic table. The name is provided anyway, because
//! **the unmodified `test/infcover.c` does not link without it**: `cover_trees`
//! (its L618-L639) calls the C symbol directly, twice, in order to provoke the
//! `ENOUGH`-exceeded return that a well-behaved `inflate()` never can.
//!
//! ★ **It is not this module that defines that name.** `inftrees.h` L53-L62 declares
//! the first parameter as a `codetype` enum, and a Rust `extern "C" fn` cannot spell
//! a C enum, so a translation unit that restates the prototype exactly is the only
//! way for `infcover.c` to see a declaration and a definition that agree. That unit
//! is `crates/libz-rs-sys/csrc/inftrees_shim.c`, which `build.rs` compiles and
//! archives into `libz.a`; it carries `inftrees.h`'s prototype verbatim and
//! `ZLIB_INTERNAL` visibility, and it forwards to [`_zlib_rs_inflate_table`] below.
//! What this module exports is that underscore-prefixed name, which `zlib.map`'s
//! `local: _*;` (L18) keeps hidden.
//!
//! Being absent from the shared object and present in the archive are facts about
//! two different artifacts, and both are the C build's own.
//! `--version-script=zlib.map` is a *shared object* link argument, so it localises
//! the name in `libz.so`; the `staticlib` archive is not linked at all, so the
//! name stays a `T` in `libz.a`, which is what `infcover` links against
//! (`Makefile.in` L121-L122, and decisively `test/CMakeLists.txt` L95-L96, which
//! links it to `ZLIB::ZLIBSTATIC`). Measured: `readelf -sW` reports the archive's
//! definition as FUNC GLOBAL HIDDEN, matching the C build, and the 111-symbol
//! parity diff polices both halves automatically.
//!
//! `inflate_table` is the **only** name from either `local:` block that is
//! exported. The other nine — `deflate_copyright`, `inflate_copyright`,
//! `inflate_fast`, `zcalloc`, `zcfree`, `z_errmsg`, `gz_error`, `gz_intmax` and
//! `inflate_fixed` (the last from the `ZLIB_1.3.2` block at L114-L115) — are
//! referenced by none of the three C drivers and stay crate-private in the core.
//!
//! # ★ The state behind `z_stream.state` is written by the test suite
//!
//! `test/infcover.c` includes the private `inflate.h` and reaches straight through
//! the opaque pointer:
//!
//! ```c
//! ((struct inflate_state *)strm.state)->mode = DICT;   /* L330 */
//! state->mode = SYNC;                                  /* L458, inside pull() */
//! ```
//!
//! Those assignments are compiled into *caller* object code that this port cannot
//! change, and the `inflate_mode` values are not zero-based: `HEAD` is 16180 and
//! runs consecutively to `SYNC` at 16211 (`inflate.h` L20-L52), so `DICT` is
//! **16190** and `SYNC` is **16211**. Three things follow, and all three are
//! obligations on this module:
//!
//! 1. The block installed at [`z_stream::state`] must expose a `#[repr(C)]`
//!    prefix of `{ z_streamp strm; int mode; }` at offset 0.
//!    [`crate::types::StateBlock`] provides exactly that, and this module must not
//!    wrap the state in anything that would shift it.
//! 2. Every entry point must **re-read the tag from memory on entry** and install
//!    it into the Rust state, rather than trusting the state's own copy. That is
//!    what makes a mode written by caller code between two calls take effect.
//!    [`entry`] does it once, for all of them.
//! 3. Every entry point must write the tag **back** before returning, so the
//!    C-visible slot never goes stale. [`Session::finish`] does that.
//!
//! A zero-based `Mode` would break both assertions silently, which is why
//! [`zlib_rs::inflate::Mode`] preserves the C discriminants.
//!
//! # Unsafe containment
//!
//! Every `unsafe` block below falls into one of the categories the crate
//! documentation enumerates, and names it:
//!
//! | Category | Where it appears here |
//! |---|---|
//! | 1 — stream-pointer validation | [`entry`], [`inflateInit2_`], [`inflateCopy`] |
//! | 2 — slice reconstruction | [`inflate`], [`inflateSync`], [`inflateSetDictionary`], [`inflateGetDictionary`], [`_zlib_rs_inflate_table`] |
//! | 3 — the opaque `state` round-trip | [`entry`], [`inflateEnd`], [`inflateCopy`] |
//! | 4 — invoking `zalloc`/`zfree` | [`inflateInit2_`], [`inflateEnd`], [`inflateCopy`], and every state recovery |
//! | 5 — reading a caller's C string | the `version` argument of [`inflateInit_`] and [`inflateInit2_`], read by [`version_error`] |
//! | 6 — writing through a caller's `gz_header` | [`inflateGetHeader`] and [`Session::publish_header`] |
//!
//! Category 5 is the narrowest one in the crate: exactly **one byte** of exactly one
//! C string. `inflateInit_` and `inflateInit2_` take `version: *const c_char` and
//! compare only `version[0]` against `ZLIB_VERSION`'s first byte, as `inflate.c`
//! L178 does, so no NUL scan and no length are needed. No `va_list` reaches this
//! module; the variadic surface is `gz.rs`'s alone.
//!
//! `inflate_table` handles three raw arrays rather than a stream, which is
//! category-2 work in a different shape, and it is documented as such at its own
//! site.
//!
//! # Panic discipline
//!
//! `inflate` is the library's primary untrusted-input surface: it must never panic,
//! hang or over-read for any input, however malformed, and no fuzz target in the
//! tree exercises that -- the property is structural. No `unwrap`, `expect`,
//! `panic!` or panicking index appears in this
//! module, every fallible step answers a [`ReturnCode`], and each body is wrapped
//! by [`crate::panic_guard`] so that a panic which somehow occurred would abort
//! rather than unwind into a C caller.
//!
//! # Error messages
//!
//! All **eighteen** texts the decoder can publish in [`z_stream::msg`] are produced
//! by [`zlib_rs::inflate`] as `&'static str` literals: five from header parsing
//! (`"incorrect header check"`, `"unknown compression method"`,
//! `"invalid window size"`, `"unknown header flags set"`,
//! `"header crc mismatch"`), ten from block structure, code tables and trailers,
//! and three shared decode texts including `"invalid distance too far back"`. They
//! are forwarded verbatim, never rewritten, and always as a pointer
//! into `'static` memory, so a caller may hold the pointer for as long as it
//! likes. [`message_ptr`] is the single place that conversion happens, and
//! [`MESSAGES`] is the table it scans -- eighteen entries, grouped by the core
//! module each text comes from.

// `zlib.h` names these functions in camelCase, and the names are the ABI. The
// crate root already relaxes `non_camel_case_types` for the same reason; this is the
// function-name half of it, needed because `#[no_mangle]` names must match `zlib.h`
// character for character.
#![allow(non_snake_case)]

use core::cell::Cell;
use core::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_ushort};
use core::mem::{size_of, MaybeUninit};

use zlib_rs::allocate::{Allocator, Buffer};
use zlib_rs::config::{InflateConfig, DEF_WBITS};
use zlib_rs::error::ReturnCode;
use zlib_rs::inflate::inftrees::{
    inflate_table_build as core_inflate_table_build, Code, CodeType, ENOUGH_DISTS, ENOUGH_LENS,
    TABLE_INVALID_CODE, TABLE_NOT_ENOUGH, TABLE_OK,
};
use zlib_rs::inflate::state::{GzHeaderSink, StreamReset, GZ_HEADER_PENDING};
use zlib_rs::inflate::{
    inflate as core_inflate, inflate_codes_used as core_codes_used,
    inflate_copy as core_inflate_copy, inflate_end as core_inflate_end,
    inflate_get_dictionary as core_get_dictionary, inflate_get_header as core_get_header,
    inflate_init2 as core_inflate_init2, inflate_mark as core_inflate_mark,
    inflate_prime as core_inflate_prime, inflate_reset as core_inflate_reset,
    inflate_reset2 as core_inflate_reset2, inflate_reset_keep as core_inflate_reset_keep,
    inflate_set_dictionary as core_set_dictionary, inflate_sync as core_inflate_sync,
    inflate_sync_point as core_sync_point, inflate_undermine as core_inflate_undermine,
    inflate_validate as core_inflate_validate, InflateState, InflateStream, Mode,
    INFLATE_CODES_USED_BAD_STATE, INFLATE_MARK_BAD_STATE,
};
use zlib_rs::read_buf::OutputRegion;

use crate::panic_guard::{fallback, guard, guard_code};
use crate::types::{
    checked_state_mut, code, code_type_from_raw, commit_state, copy_stream, discard_reserved_state,
    gz_headerp, input_slice, install_state, output_region, publish_state, ranges_are_disjoint,
    reserve_state, scratch_allocator, scratch_view, streams_are_disjoint, take_state, uInt, uLong,
    widen, z_stream, z_streamp, AliasScratch, Bytef, StateBlock, StateKind, StreamAllocator,
};

/// The largest documented `flush` value: `Z_TREES` (`zlib.h` L178).
///
/// ★ Inflate's flush domain is **wider** than deflate's. `Z_BLOCK` (5) asks it to
/// stop at a deflate block boundary and `Z_TREES` (6) to stop immediately after a
/// block header, and both are meaningful here even though `deflate` rejects the
/// second.
///
/// It is a documentation anchor and a test fixture, not a gate: [`inflate`] refuses
/// nothing, because `inflate.c` refuses nothing. See the note there.
#[cfg(test)]
const MAX_INFLATE_FLUSH: c_int = 6;

/// The `mode` tag a freshly created state carries: `HEAD`, i.e. 16180.
///
/// `inflate.c` L205 assigns it before `inflateReset2` runs, with the comment "to
/// pass state test in `inflateReset2()`". The same value seeds
/// [`crate::types::StateBlock`]'s prefix here, so a state passes its own tag check
/// from the moment it exists.
/// cbindgen:ignore
const HEAD_TAG: c_int = Mode::Head.as_raw();

/// The largest window a zlib stream can use, in bytes: `1 << MAX_WBITS` = 32768.
///
/// `zconf.h` L287 fixes `MAX_WBITS` at 15, and `zlib.h` L936-L942 requires a caller
/// of [`inflateGetDictionary`] that passes no `dictLength` to provide at least this
/// much space. It is therefore the capacity such a caller is assumed to have, and
/// since it is also the largest window the decoder can own, no history is ever lost
/// by assuming it.
/// cbindgen:ignore
const MAX_WINDOW_BYTES: usize = 32768;

// ---------------------------------------------------------------------------
// The state this module installs behind `z_stream.state`
// ---------------------------------------------------------------------------

/// What the facade keeps for one decompression stream.
///
/// Wrapped in a [`StateBlock`] so that the C-visible `{ strm, mode }` prefix sits
/// at offset 0; this struct is the part that follows it, where no C caller looks.
///
/// # Why the raw `gz_headerp` is stored here
///
/// C's header states write straight through `state->head` into the caller's
/// `gz_header`. The safe core cannot: it parses into a
/// [`GzHeaderSink`], which holds the three variable-length fields as
/// `&[Cell<u8>]` views over the caller's buffers but keeps the seven scalars on
/// its own side. So the facade has to remember *which* `gz_header` those scalars
/// belong to, and publish them after each [`inflate`] call. That pointer is this
/// field, and [`Session::publish_header`] is the publishing step.
///
/// The lifetime is `'static` because the block outlives every call that can see
/// it: it is released only by [`inflateEnd`], and the window it owns is reached
/// only through the state. Nothing borrowed from a caller's stack is stored.
struct InflateSlot {
    /// The safe decoder state, holding the window, the arena and every scalar.
    state: InflateState<'static, StreamAllocator>,
    /// The caller's `gz_header` from the last [`inflateGetHeader`], or null.
    ///
    /// Never dereferenced without a null test, and only ever written through with
    /// the field-by-field clamped writes [`Session::publish_header`] performs.
    head: gz_headerp,
    /// The message currently published in [`z_stream::msg`], in the core's form.
    ///
    /// ★ This has to be remembered across calls, and the reason is a behavioural
    /// requirement rather than a convenience. C writes `strm->msg` **only** at the
    /// site that detects an error and clears it only in a reset; a later
    /// `inflate()` on a stream stuck in `BAD` returns `Z_DATA_ERROR` again without
    /// touching `msg`, so the text survives. The core, by contrast, takes `msg` as
    /// an in/out field of [`InflateStream`] and writes back whatever it was given.
    /// Seeding it from this field and storing the result back is what reproduces
    /// C's "leave it alone" behaviour; seeding it from [`None`] every time would
    /// silently clear a caller's diagnostic on the second call.
    ///
    /// It is also what makes [`message_ptr`]'s table scan avoidable in the common
    /// case: the pointer is rewritten only when this value changes.
    msg: Option<&'static str>,
}

/// A validated stream plus its state, with the mode tag already synchronised.
///
/// Produced by [`entry`], which performs every check `inflateStateCheck`
/// (`inflate.c` L88-L98) performs, and consumed by [`Session::finish`], which
/// writes the tag back. Holding the two together is what stops an entry point from
/// syncing on the way in and forgetting on the way out.
struct Session<'s> {
    /// The caller's pointer, kept raw so that its provenance survives.
    strm: z_streamp,
    /// The state block, borrowed mutably for the duration of the call.
    block: &'s mut StateBlock<InflateSlot>,
    /// The allocator built from the caller's hooks.
    allocator: StreamAllocator,
}

/// Validates a stream, recovers its state, and adopts the C-visible mode tag.
///
/// This is `inflateStateCheck` (`inflate.c` L88-L98) in full, assembled from the
/// pieces the type layer provides:
///
/// | C test | Performed by |
/// |---|---|
/// | `strm == Z_NULL` | [`StreamAllocator::from_stream_ptr`] and [`checked_state_mut`] |
/// | `strm->zalloc == 0 \|\| strm->zfree == 0` | [`StreamAllocator::from_stream_ptr`] |
/// | `state == Z_NULL` | [`checked_state_mut`] |
/// | `state->strm != strm` | [`checked_state_mut`] |
/// | `state->mode < HEAD \|\| state->mode > SYNC` | [`checked_state_mut`], via [`StateKind::Inflate`] |
///
/// ★ It then does one thing C never has to: it copies the prefix tag into the Rust
/// state with `set_mode_tag`. C's `state->mode` *is* the tag, so a mode written by
/// caller code is simply the mode; here they are separate storage, and this
/// assignment is what makes `test/infcover.c`'s `->mode = DICT` (L330) and
/// `->mode = SYNC` (L458) take effect instead of being silently discarded. The tag
/// has already been range-checked, so `set_mode_tag` cannot reject it — but its
/// answer is honoured anyway rather than ignored, because a `false` would mean the
/// two range predicates had drifted apart and continuing would be worse than
/// refusing.
///
/// [`None`] means the entry point must return its "invalid stream" value.
///
/// # Safety
///
/// If `strm` is non-null it must address a live [`z_stream`] whose state, if any,
/// was installed by [`inflateInit2_`]. No other borrow of either may exist for
/// `'s`.
unsafe fn entry<'s>(strm: z_streamp) -> Option<Session<'s>> {
    // SAFETY: unsafe-site category 1 and 4 -- reads the three allocator members
    // through raw places, forming no reference to the stream, so the caller's
    // pointer keeps its provenance for the state recovery below. Returns `None` for
    // a null or misaligned stream and for a stream with a null `zalloc` or `zfree`,
    // which is the `zalloc == 0 || zfree == 0` half of `inflateStateCheck`
    // (`inflate.c` L90-L92) -- reachable exactly as C's is, because
    // `test/infcover.c`'s `mem_done` clears all three members (L231-L233).
    let allocator = unsafe { StreamAllocator::from_stream_ptr(strm) }?;

    // The slot type is a turbofish rather than a binding annotation so that the
    // `unsafe` block begins on the same line as the `let`. That is not cosmetic:
    // `clippy::undocumented_unsafe_blocks` (denied workspace-wide) looks for the
    // comment on the line preceding the *block*, and on the 1.80 floor a wrapped
    // initialiser puts the type annotation there instead, which reads as an
    // undocumented block even though the comment is right above the statement.
    // SAFETY: unsafe-site category 3 -- the opaque `state` round-trip. The four
    // remaining `inflateStateCheck` tests run inside, and this function's contract
    // supplies the liveness and provenance requirements they cannot test. The
    // borrow is taken once and is the only one for `'s`.
    let block = unsafe { checked_state_mut::<InflateSlot>(strm, StateKind::Inflate) }?;

    // Adopt whatever the C-visible slot says. See the note above on why this is not
    // redundant.
    let tag = block.tag();
    if !block.state_mut().state.set_mode_tag(tag) {
        return None;
    }

    Some(Session {
        strm,
        block,
        allocator,
    })
}

impl Session<'_> {
    /// The safe decoder state.
    fn state(&self) -> &InflateState<'static, StreamAllocator> {
        &self.block.state().state
    }

    /// The safe decoder state, mutably.
    fn state_mut(&mut self) -> &mut InflateState<'static, StreamAllocator> {
        &mut self.block.state_mut().state
    }

    /// Publishes the Rust mode back into the C-visible prefix and drops a
    /// header the state no longer holds.
    ///
    /// The other half of the synchronisation [`entry`] begins. Every path out of
    /// every entry point runs through here, so the slot a caller may read — or
    /// overwrite — is never stale.
    ///
    /// The second step keeps [`InflateSlot::head`] honest. `inflateResetKeep`
    /// assigns `state->head = Z_NULL` (`inflate.c` L110), so *every* reset — direct,
    /// via `inflateReset2`, or the internal one inside `inflateSync` — detaches the
    /// caller's `gz_header`. The core expresses that by clearing its sink, and this
    /// mirrors it by clearing the pointer, so the two can never disagree about
    /// whether a header is installed. Doing it here rather than at each reset site is
    /// what makes it impossible to miss one.
    fn finish(self) {
        let tag = self.state().mode_tag();
        if !self.state().has_header_sink() {
            self.block.state_mut().head = core::ptr::null_mut();
        }
        self.block.set_tag(tag);
    }

    /// Runs `body`, then synchronises the tag, then returns the body's value.
    ///
    /// The shape almost every entry point wants: it makes forgetting
    /// [`Session::finish`] impossible, because the value only comes back out
    /// through this method.
    fn run<T, F>(mut self, body: F) -> T
    where
        F: FnOnce(&mut Self) -> T,
    {
        let outcome = body(&mut self);
        self.finish();
        outcome
    }

    /// Writes the five `z_stream` members a reset touches.
    ///
    /// `inflateResetKeep` assigns `total_in`, `total_out`, `msg`, `data_type` and —
    /// conditionally — `adler` (`inflate.c` L104-L109). [`StreamReset`] carries
    /// them out of the core and this method puts them in the caller's stream.
    ///
    /// ★ The conditional `adler` is the easily missed one. C guards it with
    /// `if (state->wrap)` under the comment "to support ill-conceived Java test
    /// suite", so a wrapped stream reports `adler == 1` before a single byte has
    /// been decoded while a raw stream keeps whatever the caller had. The core
    /// expresses that as [`Option`], and the [`Some`] arm is the only one that
    /// writes.
    fn apply_reset(&mut self, reset: StreamReset) {
        let strm = self.strm;
        self.block.state_mut().msg = reset.msg;
        // SAFETY: unsafe-site category 1 -- writing five members of the caller's
        // stream. `strm` was established non-null, aligned and live by `entry`, so
        // every member is in bounds and writable. Each write goes through a raw
        // place, so no `&mut z_stream` is materialised and the caller's own pointer
        // keeps its provenance for the state borrow this `Session` still holds.
        unsafe {
            core::ptr::addr_of_mut!((*strm).total_in)
                .write(narrow_uLong(u64::from(reset.total_in)));
            core::ptr::addr_of_mut!((*strm).total_out)
                .write(narrow_uLong(u64::from(reset.total_out)));
            core::ptr::addr_of_mut!((*strm).msg).write(message_ptr(reset.msg));
            core::ptr::addr_of_mut!((*strm).data_type).write(reset.data_type);
            if let Some(adler) = reset.adler {
                core::ptr::addr_of_mut!((*strm).adler).write(uLong::from(adler));
            }
        }
    }

    /// Whether the caller's input and output ranges share a byte.
    ///
    /// ★ **The precondition [`Session::with_buffers`] cannot check and cannot do
    /// without.** It builds a `&[u8]` over `(next_in, avail_in)` and a `&mut [u8]` over
    /// `(next_out, avail_out)`; two such borrows over one region are undefined
    /// behaviour even if neither is read or written. C has no such constraint --
    /// `inflate.c` L305-L326 simply `LOAD()`s both into locals -- so the overlapping
    /// pair is an input this library can be handed.
    ///
    /// It is **served**, not refused. This reports the condition; the caller answers it by
    /// snapshotting the input with [`AliasScratch`] before any mutable borrow exists and
    /// passing the snapshot to `with_buffers` in the caller's place, which keeps the two
    /// borrows apart without changing what the call consumes. `deflate` does the same, and
    /// there it is not optional: `test/example.c` overlaps the pair deliberately.
    ///
    /// [`ranges_are_disjoint`] compares addresses and dereferences nothing, so this is
    /// safe to run on the raw members before any borrow exists.
    ///
    /// # Safety
    ///
    /// `self.strm` must be the live, validated stream [`entry`] produced, with its four
    /// buffer members initialised -- which `zlib.h` L136-L139 requires of the
    /// application before it calls `inflate`.
    #[must_use]
    unsafe fn buffers_overlap(&self) -> bool {
        let strm = self.strm;
        // SAFETY: unsafe-site category 1 -- four members read through raw places on a
        // non-null, aligned, live stream whose buffer members are initialised by this
        // function's contract. No reference to the stream is formed and neither pointer
        // is dereferenced.
        let (next_in, avail_in, next_out, avail_out) = unsafe {
            (
                core::ptr::addr_of!((*strm).next_in).read(),
                core::ptr::addr_of!((*strm).avail_in).read(),
                core::ptr::addr_of!((*strm).next_out).read(),
                core::ptr::addr_of!((*strm).avail_out).read(),
            )
        };
        !ranges_are_disjoint(
            next_in,
            widen(avail_in),
            next_out.cast_const(),
            widen(avail_out),
        )
    }

    /// Rebuilds the caller's buffers as slices, runs `body`, and writes the result
    /// back into the caller's [`z_stream`].
    ///
    /// This is C's `LOAD()` / `RESTORE()` pair (`inflate.c` L305-L326) together with
    /// the direct `strm->` writes of the epilogue at L1130-L1149. Both [`inflate`]
    /// and [`inflateSync`] need it, which is why it exists once here rather than
    /// twice at the call sites.
    ///
    /// # The cursor convention
    ///
    /// [`InflateStream`] takes the *whole* buffer plus a cursor, whereas C advances
    /// a pointer and decrements a count. The translation is:
    ///
    /// * `input` is the `avail_in` bytes at `next_in`, and the cursor starts at
    ///   zero, so on return the cursor **is** the number of bytes consumed;
    /// * `output` likewise, so on return the cursor is the number produced.
    ///
    /// The caller's `next_in`/`next_out` are then advanced and its
    /// `avail_in`/`avail_out` decremented by those two counts. Nothing is inferred
    /// from the totals, which the core maintains itself — and nothing is inferred from
    /// the *address* of the input either, which is what lets `input_override` replace it.
    ///
    /// # `input_override`
    ///
    /// [`Some`] substitutes that slice for a borrow of `(next_in, avail_in)`, which is how an
    /// overlapping buffer pair is served rather than refused. It must be exactly `avail_in`
    /// bytes long, because the write-back derives the caller's new `avail_in` from the cursor
    /// into it. [`AliasScratch`] produces it and documents the reasoning.
    ///
    /// # Safety
    ///
    /// `self.strm` must be the live, validated stream [`entry`] produced, and its
    /// `(next_in, avail_in)` and `(next_out, avail_out)` pairs must describe
    /// readable and writable regions. When `input_override` is [`None`] the two must
    /// additionally not overlap — the contract `zlib.h` L138-L139 places on the application.
    /// When it is [`Some`], the caller's input region is not borrowed at all and only the
    /// override's own bytes are read, so the two regions may overlap freely.
    unsafe fn with_buffers<F>(
        &mut self,
        input_override: Option<&'static [u8]>,
        body: F,
    ) -> ReturnCode
    where
        F: FnOnce(
            &mut InflateState<'static, StreamAllocator>,
            &mut InflateStream<'_>,
        ) -> ReturnCode,
    {
        let strm = self.strm;

        // ★ `adler` is **not** among the members read. `inflateResetKeep` assigns
        // `strm->adler` under `if (state->wrap)` (`inflate.c` L108-L109), so a raw
        // stream's `adler` is whatever the caller left there -- and after
        // `inflateInit2_(strm, -15)` on an uninitialised `z_stream`, that is
        // *indeterminate*. Reading it would be undefined behaviour, and there is
        // nothing to read it for: the decoder keeps its running check in its own
        // state and only ever publishes here. See [`InflateStream::adler`].
        //
        // SAFETY: unsafe-site category 1 -- reading seven members of the caller's
        // stream through raw places. `strm` is non-null, aligned and live by
        // `entry`'s contract, so every member is in bounds; all seven are plain
        // `Copy` scalars or pointers, all seven are written unconditionally by the
        // init-time reset (L104-L107) or by the caller itself, and none is
        // dereferenced here.
        let (next_in, avail_in, next_out, avail_out, total_in, total_out, data_type) = unsafe {
            (
                core::ptr::addr_of!((*strm).next_in).read(),
                core::ptr::addr_of!((*strm).avail_in).read(),
                core::ptr::addr_of!((*strm).next_out).read(),
                core::ptr::addr_of!((*strm).avail_out).read(),
                core::ptr::addr_of!((*strm).total_in).read(),
                core::ptr::addr_of!((*strm).total_out).read(),
                core::ptr::addr_of!((*strm).data_type).read(),
            )
        };

        let input = match input_override {
            // The overlapping-buffer path: the caller's input has already been copied and
            // the copy is what the decoder reads, so no borrow of the caller's input region
            // is formed at all. `AliasScratch` documents why a copy rather than a refusal.
            Some(captured) => captured,
            // SAFETY: unsafe-site category 2 -- slice reconstruction, performed exactly
            // once per call. `input_slice` branches on a zero length or a null pointer and
            // returns an empty slice rather than calling `from_raw_parts` with one, so the
            // legal `avail_in == 0` with `next_in == Z_NULL` state is not undefined
            // behaviour. On this arm the two regions do not overlap by this function's
            // contract, so the shared and mutable borrows cannot alias.
            None => unsafe { input_slice(next_in, avail_in) },
        };
        // SAFETY: unsafe-site category 2, as above -- and the output is **write-only**
        // storage rather than a byte slice, because `avail_out` bytes of room is all
        // `zlib.h` L94-L95 promises about it. `output_region` states that argument in full.
        let output = unsafe { output_region(next_out, avail_out) };

        let mut stream = InflateStream {
            input,
            next_in: 0,
            output,
            next_out: 0,
            total_in: widen_uLong(total_in),
            total_out: widen_uLong(total_out),
            msg: self.block.state().msg,
            adler: None,
            data_type,
        };

        let outcome = body(&mut self.block.state_mut().state, &mut stream);

        let consumed = stream.next_in;
        let produced = stream.next_out;
        let published = core::mem::replace(&mut self.block.state_mut().msg, stream.msg);

        // Advancing the caller's cursors, exactly as C's `strm->next_in += have` does once
        // the input is consumed. The zero guard is what keeps a null pointer out of `add`,
        // where even an offset of zero would be undefined behaviour.
        let advanced_in = if consumed == 0 {
            next_in
        } else {
            // SAFETY: unsafe-site category 2. `consumed` is non-zero here, so `next_in` is
            // non-null -- a null `next_in` yields an empty input slice and therefore a zero
            // `consumed`. And `consumed <= input.len() == avail_in`, so the result lies
            // inside the caller's own buffer or one past its end.
            unsafe { next_in.add(consumed) }
        };
        let advanced_out = if produced == 0 {
            next_out
        } else {
            // SAFETY: unsafe-site category 2, as above with `produced` bounded by
            // `output.len()`, which is `avail_out` when `next_out` is non-null and zero
            // when it is null.
            unsafe { next_out.add(produced) }
        };

        // SAFETY: unsafe-site category 1 -- writing the members C's epilogue writes.
        // `strm` is the same live, aligned stream read above, so every member is in
        // bounds and writable, and each write goes through a raw place so the
        // caller's pointer keeps its provenance. The `msg` write is skipped unless
        // the text changed, so an unchanged diagnostic keeps the exact pointer the
        // caller already saw -- which is what C does by never rewriting it. `adler`
        // is skipped unless the core assigned one, which is C's
        // `if ((state->wrap & 4) && out)` at L1144 and the `if (state->wrap)` of the
        // reset at L108: a raw stream's member is left exactly as the caller had it.
        unsafe {
            core::ptr::addr_of_mut!((*strm).next_in).write(advanced_in);
            core::ptr::addr_of_mut!((*strm).avail_in)
                .write(narrow_uInt(widen(avail_in).saturating_sub(consumed)));
            core::ptr::addr_of_mut!((*strm).next_out).write(advanced_out);
            core::ptr::addr_of_mut!((*strm).avail_out)
                .write(narrow_uInt(widen(avail_out).saturating_sub(produced)));
            core::ptr::addr_of_mut!((*strm).total_in).write(narrow_uLong(stream.total_in));
            core::ptr::addr_of_mut!((*strm).total_out).write(narrow_uLong(stream.total_out));
            if let Some(check) = stream.adler {
                core::ptr::addr_of_mut!((*strm).adler).write(uLong::from(check));
            }
            core::ptr::addr_of_mut!((*strm).data_type).write(stream.data_type);
            if published != stream.msg {
                core::ptr::addr_of_mut!((*strm).msg).write(message_ptr(stream.msg));
            }
        }

        outcome
    }

    /// Runs `body` over the caller's **input only**, writing back the six members it
    /// can touch and leaving the output pair entirely alone.
    ///
    /// ★ **The whole point is what this does *not* do.** `inflateSync`
    /// (`inflate.c` L1264-L1310) reads and writes `avail_in`, `next_in` and `total_in`,
    /// restores `total_in` and `total_out` around its internal `inflateReset`, and lets
    /// that reset write `msg`, `data_type` and -- for a wrapped stream -- `adler`
    /// (L104-L109). It **never reads or writes `next_out` or `avail_out`**. So a caller
    /// may legitimately call it on a stream whose output pair has never been
    /// initialised, or has been left pointing at freed memory since the last `inflate`;
    /// `test/example.c`'s `test_sync` does the former in spirit by driving recovery
    /// between output cycles. Rebuilding the output slice would read two indeterminate
    /// members and hand the core a `&mut [u8]` over memory the caller no longer owns,
    /// and writing the pair back would clobber values C leaves untouched.
    ///
    /// An empty output slice is the faithful stand-in: the core's `inflate_sync` reads
    /// `stream.output` never and `stream.next_out` never, and passing `&mut []`
    /// guarantees at the type level that it cannot start.
    ///
    /// # Safety
    ///
    /// `self.strm` must be the live, validated stream [`entry`] produced, and its
    /// `(next_in, avail_in)` pair must describe a readable region -- the same
    /// obligation `zlib.h` L136-L137 places on the application. Nothing is required of
    /// `next_out` or `avail_out`, which is the difference from
    /// [`Session::with_buffers`].
    unsafe fn with_input_only<F>(&mut self, body: F) -> ReturnCode
    where
        F: FnOnce(
            &mut InflateState<'static, StreamAllocator>,
            &mut InflateStream<'_>,
        ) -> ReturnCode,
    {
        let strm = self.strm;

        // SAFETY: unsafe-site category 1 -- reads the five members C reads, through raw
        // places. `strm` is non-null, aligned and live by `entry`'s contract, so each is
        // in bounds; all five are plain `Copy` scalars or pointers and none is
        // dereferenced here. `next_out` and `avail_out` are deliberately absent, and so
        // is `adler`: the init-time reset writes it only for a wrapped stream
        // (`inflate.c` L108-L109), so a raw stream's member is indeterminate and reading
        // it would be undefined behaviour. Nothing here needs its value -- the reset
        // inside `inflate_sync` produces the only value that is published.
        let (next_in, avail_in, total_in, total_out, data_type) = unsafe {
            (
                core::ptr::addr_of!((*strm).next_in).read(),
                core::ptr::addr_of!((*strm).avail_in).read(),
                core::ptr::addr_of!((*strm).total_in).read(),
                core::ptr::addr_of!((*strm).total_out).read(),
                core::ptr::addr_of!((*strm).data_type).read(),
            )
        };

        // SAFETY: unsafe-site category 2 -- slice reconstruction, once. `input_slice`
        // branches on a zero length or a null pointer and returns an empty slice rather
        // than calling `from_raw_parts` with one, so the legal `avail_in == 0` with
        // `next_in == Z_NULL` state is not undefined behaviour.
        let input = unsafe { input_slice(next_in, avail_in) };

        let mut stream = InflateStream {
            input,
            next_in: 0,
            output: OutputRegion::empty(),
            next_out: 0,
            total_in: widen_uLong(total_in),
            total_out: widen_uLong(total_out),
            msg: self.block.state().msg,
            adler: None,
            data_type,
        };

        let outcome = body(&mut self.block.state_mut().state, &mut stream);

        let consumed = stream.next_in;
        let published = core::mem::replace(&mut self.block.state_mut().msg, stream.msg);

        // Advancing the caller's input cursor, which is what C's `next_in += len` does
        // (`inflate.c` L1293).
        let advanced_in = if consumed == 0 {
            next_in
        } else {
            // SAFETY: unsafe-site category 2. `consumed` is non-zero here, so `next_in` is
            // non-null -- a null `next_in` yields an empty slice and a zero `consumed` --
            // and `consumed <= input.len() == avail_in`, so the result lies inside the
            // caller's buffer or one past its end.
            unsafe { next_in.add(consumed) }
        };

        // SAFETY: unsafe-site category 1 -- writing the six members C's `inflateSync`
        // writes, and no others. `strm` is the same live, aligned stream read above, so
        // each member is in bounds and writable, and every write goes through a raw
        // place so the caller's pointer keeps its provenance. `next_out` and `avail_out`
        // are deliberately absent: C leaves them exactly as the caller left them.
        unsafe {
            core::ptr::addr_of_mut!((*strm).next_in).write(advanced_in);
            core::ptr::addr_of_mut!((*strm).avail_in)
                .write(narrow_uInt(widen(avail_in).saturating_sub(consumed)));
            core::ptr::addr_of_mut!((*strm).total_in).write(narrow_uLong(stream.total_in));
            core::ptr::addr_of_mut!((*strm).total_out).write(narrow_uLong(stream.total_out));
            if let Some(check) = stream.adler {
                core::ptr::addr_of_mut!((*strm).adler).write(uLong::from(check));
            }
            core::ptr::addr_of_mut!((*strm).data_type).write(stream.data_type);
            if published != stream.msg {
                core::ptr::addr_of_mut!((*strm).msg).write(message_ptr(stream.msg));
            }
        }

        outcome
    }

    /// Points the installed header sink's three field views at the caller's buffers,
    /// for the duration of one core call.
    ///
    /// ★ **C reads `extra`, `extra_max`, `name`, `name_max`, `comment` and `comm_max`
    /// at the moment it writes a byte, not when the header is installed.** The three
    /// guards live inside the `EXTRA`, `NAME` and `COMMENT` states -- `inflate.c`
    /// L608-L620, L625-L634 and L640-L649 -- and each one dereferences `state->head`
    /// afresh. A caller may therefore supply a buffer, or enlarge a capacity, after
    /// `inflateGetHeader` and before the `inflate` that fills it, and the reference
    /// honours it.
    ///
    /// Reading them once and keeping the resulting views would break that, and would
    /// also hold a Rust borrow of the caller's memory between calls -- memory the
    /// caller is entitled to move or reuse. Binding here and dropping again in
    /// [`Session::unbind_header_fields`] keeps the borrow exactly one call wide.
    ///
    /// A null `head`, or a stream with no sink installed, is nothing to bind.
    ///
    /// # ★ And a finished header is nothing to bind either
    ///
    /// The three reads below are the *only* place this crate follows the caller's
    /// `gz_headerp` on a decode call, so this is where the structure's lifetime
    /// contract is honoured. `zlib.h` L1075-L1084 asks the application to keep the
    /// `gz_header` -- and whichever of its three buffers it supplied -- available
    /// while `inflate()` is reading the header, and no longer: once `head->done`
    /// is `1` for a completed gzip header, or `-1` for a stream that turned out
    /// not to be gzip, a conforming caller may free all four. C makes that safe by
    /// construction rather than by promise: every `state->head` dereference in
    /// `inflate.c` sits inside one of the nine header `case`s (L522-L688), the
    /// ladder only ever moves forward, and so after `HCRC -> TYPE` the pointer is
    /// never followed again.
    ///
    /// Binding unconditionally would break exactly that: a stream that had
    /// finished its header would still read six members of a structure the caller
    /// was entitled to have released, on every subsequent `inflate()` call, for
    /// the whole life of the stream. That is a use-after-free reachable from
    /// conforming C -- observed as a `SIGSEGV` where the reference completes, and
    /// reported by AddressSanitizer as a read of the freed `extra` member.
    ///
    /// [`InflateState::dereferences_gzip_header`] is the state's answer to "would
    /// the reference read it on this call?", and the mode it tests is the one the
    /// parse is about to *resume* in. That is the right instant to ask, because
    /// the ladder cannot re-enter a header state inside one call: `HEAD` leaves
    /// for `FLAGS`, `DICTID`, `TYPE` or `TYPEDO` and `HCRC` leaves for `TYPE`, and
    /// nothing returns. A reset does return the mode to `HEAD`, and that path is
    /// already covered from the other side -- `inflateResetKeep` clears the sink
    /// (`inflate.c` L110-L115) and [`Session::finish`] clears this crate's
    /// pointer with it, so a reset stream has no header to bind at all until the
    /// caller installs one again.
    ///
    /// # Safety
    ///
    /// The `gz_header` recorded in the slot must be live, and each of its non-null
    /// `extra`, `name` and `comment` buffers valid for the capacity it advertises, for
    /// the duration of the call this binding covers -- which is what `zlib.h`
    /// L1070-L1090 requires of a caller that installs a header. The requirement is
    /// discharged for exactly the calls that reach the reads: those whose entry
    /// mode is a header state, which are the calls during which the caller is
    /// still obliged to keep the structure alive.
    unsafe fn bind_header_fields(&mut self) {
        let head = self.block.state().head;
        if head.is_null() {
            return;
        }
        if !self.state().has_header_sink() {
            return;
        }
        // The lifetime gate described above. Checked after the two cheap pointer
        // tests and before anything is read, so that a stream whose header is
        // finished performs no access to the caller's structure whatsoever.
        if !self.state().dereferences_gzip_header() {
            return;
        }

        // SAFETY: unsafe-site category 6 -- reads the three buffer pointers of the
        // caller's `gz_header` through raw places, so no `&gz_header` is formed and the
        // caller's pointer keeps its provenance for the writes `publish_header`
        // performs. `head` is the pointer the caller passed to `inflateGetHeader`, which
        // established it non-null and aligned, and this function's contract makes it
        // live. All three are plain `Copy` pointers, and all three are members a
        // *reading* caller must initialise -- they are the inputs of the operation,
        // which is why reading them is faithful where reading `text` or `os` would not
        // be.
        let (extra, name, comment) = unsafe {
            (
                core::ptr::addr_of!((*head).extra).read(),
                core::ptr::addr_of!((*head).name).read(),
                core::ptr::addr_of!((*head).comment).read(),
            )
        };

        // Each capacity is read only once its own pointer is known non-null, which is
        // the order `zlib.h` L125, L127 and L129 imply: `extra_max` is "the space at
        // extra", so a caller that passes `Z_NULL` for `extra` need not have set it --
        // and reading it anyway would be an indeterminate read of exactly the class the
        // output scalars above are excluded for. Each of the three reads below therefore
        // sits inside its own non-null branch, and each is one `uInt` member read through
        // a raw place on the non-null, aligned, live structure established above: a
        // caller that supplied the pointer has, by the same header contract, supplied its
        // bound.
        let extra_max = if extra.is_null() {
            0
        } else {
            // SAFETY: unsafe-site category 6 -- `extra` is non-null, so `extra_max` is
            // initialised by the header contract; read as above.
            unsafe { core::ptr::addr_of!((*head).extra_max).read() }
        };
        let name_max = if name.is_null() {
            0
        } else {
            // SAFETY: unsafe-site category 6 -- `name` is non-null, so `name_max` is
            // initialised by the header contract; read as above.
            unsafe { core::ptr::addr_of!((*head).name_max).read() }
        };
        let comm_max = if comment.is_null() {
            0
        } else {
            // SAFETY: unsafe-site category 6 -- `comment` is non-null, so `comm_max` is
            // initialised by the header contract; read as above.
            unsafe { core::ptr::addr_of!((*head).comm_max).read() }
        };

        // SAFETY: unsafe-site category 6 -- each buffer is null or valid for its
        // advertised capacity by this function's contract, which is exactly what
        // `header_field` requires. The three ranges are permitted to overlap, which is
        // why they become *shared* `Cell` slices -- `test/infcover.c` L305-L310 points
        // all three at one buffer.
        let (extra, name, comment) = unsafe {
            (
                header_field(extra, extra_max),
                header_field(name, name_max),
                header_field(comment, comm_max),
            )
        };

        // Reached in place: the sink is 100-odd bytes and this runs before every call, so a
        // copy out and back would move it twice for a change to three pointers.
        self.state_mut()
            .with_header_sink(|sink| sink.rebind_fields(extra, name, comment));
    }

    /// Drops the three field views again, so that no borrow of the caller's buffers
    /// outlives the call that used them.
    ///
    /// Run after [`Session::publish_header`], which needs the absence flags the parse
    /// set; the rebind clears those flags, so the order is not interchangeable. The
    /// parsed scalars are untouched and carry the header across calls -- which is what
    /// C achieves by leaving them in the caller's structure.
    fn unbind_header_fields(&mut self) {
        self.state_mut()
            .with_header_sink(|sink| sink.rebind_fields(None, None, None));
    }

    /// Writes the gzip-header members the parse assigned into the caller's
    /// `gz_header`.
    ///
    /// ★ **This is unsafe-site category 6.** C's header states write straight through
    /// `state->head`, so the caller's structure is updated field by field as the parse
    /// proceeds (`inflate.c` L566, L577, L586-L587, L596, L601, L631, L666, L678,
    /// L687). The safe core cannot hold a raw pointer, so it parses into a
    /// [`GzHeaderSink`] and this method performs the writes. Called after every
    /// [`inflate`], because that is the only entry point that parses a header.
    ///
    /// # Only what the parse assigned, and only once
    ///
    /// C is *selective*, and the selectivity is observable. `inflateGetHeader` writes
    /// only `head->done = 0` and leaves every other member as the caller left it, so a
    /// caller that pre-filled `time` and then decoded a stream which turned out not to
    /// be gzip still sees its own value. `zlib_rs::inflate::state::HeaderUpdate` is
    /// what makes that reproducible here: it reports each member as [`Some`] exactly
    /// when the parse assigned it, so this method writes exactly where C wrote.
    ///
    /// C is also *episodic*: it performs these writes while parsing a header, not on
    /// every payload chunk. `take_header_update` returns [`None`] once nothing is
    /// outstanding, so an `inflate()` that decodes compressed data long after the
    /// header finished does no work here at all -- not one write, and not the three
    /// absent-field tests. No separate "have I published yet" flag is needed, because a
    /// finished parse simply stops assigning.
    ///
    /// # The three variable-length fields
    ///
    /// `extra`, `name` and `comment` need no copy: the sink holds them as shared
    /// `Cell` views over the caller's own buffers, clamped to `extra_max`, `name_max`
    /// and `comm_max`, so a byte written by the `EXTRA`, `NAME` or `COMMENT` state is
    /// already in the caller's memory. What *is* published here is the three `Z_NULL`
    /// assignments the reference makes when a field is absent from the stream — L601,
    /// L631 and L666 — because a caller can observe them.
    fn publish_header(&mut self) {
        let head = self.block.state().head;
        if head.is_null() {
            return;
        }
        let Some(update) = self.state_mut().take_header_update() else {
            // Nothing was assigned since the last publication: either no header is
            // installed, or the parse has finished and this `inflate()` decoded
            // payload. Either way C performed no write here, so neither does this.
            return;
        };

        // SAFETY: unsafe-site category 6 -- writing through a caller's `gz_header`.
        // `head` is non-null by the test above and is the pointer the caller passed
        // to `inflateGetHeader`, which documents that the structure must outlive the
        // decode. Every write targets one scalar member of that structure through a
        // raw place, so no `&mut gz_header` is formed and no aliasing arises even
        // though the harness in `test/infcover.c` L305-L310 points all three buffer
        // members at one buffer. No write here touches those buffers: the byte
        // writes are the core's, through length-clamped `Cell` views, and the three
        // writes below only store `Z_NULL` into the pointer members. Every write is
        // guarded by the parse having assigned that member, so a member C would have
        // left alone is left alone -- including one whose previous contents are
        // indeterminate, which is why none of them is read.
        unsafe {
            if let Some(text) = update.text() {
                core::ptr::addr_of_mut!((*head).text).write(c_int::from(text));
            }
            if let Some(time) = update.time() {
                core::ptr::addr_of_mut!((*head).time).write(uLong::from(time));
            }
            if let Some(xflags) = update.xflags() {
                core::ptr::addr_of_mut!((*head).xflags).write(xflags);
            }
            if let Some(os) = update.os() {
                core::ptr::addr_of_mut!((*head).os).write(os);
            }
            if let Some(extra_len) = update.extra_len() {
                core::ptr::addr_of_mut!((*head).extra_len).write(extra_len);
            }
            if let Some(hcrc) = update.hcrc() {
                core::ptr::addr_of_mut!((*head).hcrc).write(c_int::from(hcrc));
            }
            if let Some(done) = update.done() {
                core::ptr::addr_of_mut!((*head).done).write(done);
            }
            if update.extra_is_absent() {
                core::ptr::addr_of_mut!((*head).extra).write(core::ptr::null_mut());
            }
            if update.name_is_absent() {
                core::ptr::addr_of_mut!((*head).name).write(core::ptr::null_mut());
            }
            if update.comment_is_absent() {
                core::ptr::addr_of_mut!((*head).comment).write(core::ptr::null_mut());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Width conversions between C's integer model and the core's
// ---------------------------------------------------------------------------

/// Widens a `uLong` to the `u64` the core counts in.
///
/// Lossless in both integer models: `c_ulong` is 64 bits on LP64 and 32 on LLP64
/// Windows, and [`u64::from`] exists for both.
//
// `clippy::useless_conversion` fires here on LP64, where `uLong` already *is* `u64`
// and the conversion is the identity. Removing it would break the LLP64 build, where
// `uLong` is `u32` and the widening is real, so the conversion is target-dependent
// rather than useless and the lint is relaxed for this one function. Written as a
// comment rather than the attribute's `reason` field because lint reasons were
// stabilised in Rust 1.81 and this workspace declares `rust-version = "1.80"`.
#[inline]
#[must_use]
#[allow(clippy::useless_conversion)]
fn widen_uLong(value: uLong) -> u64 {
    u64::from(value)
}

/// Narrows a `u64` count back to C's `uLong`.
///
/// On LP64 this is the identity. On LLP64 it truncates, which is not a defect: C's
/// own `strm->total_in` is a 32-bit `unsigned long` there and wraps at exactly the
/// same point, so reproducing the wrap is reproducing the behaviour.
//
// `clippy::useless_conversion` is relaxed for the same target-dependent reason as
// `widen_uLong` above: `u64::from(uLong::MAX)` is the identity on LP64 and a genuine
// widening on LLP64.
#[inline]
#[must_use]
#[allow(clippy::useless_conversion)]
fn narrow_uLong(value: u64) -> uLong {
    // Masking to `uLong`'s width IS truncation for an unsigned type, so this is C's
    // wrap written without an `as`: on LP64 the mask is `u64::MAX` and the whole
    // expression is the identity, and on LLP64 it keeps the low 32 bits exactly as
    // C's 32-bit `unsigned long` does. Once masked the conversion cannot fail, so
    // the fallback is unreachable and exists only to keep the function total.
    uLong::try_from(value & u64::from(uLong::MAX)).unwrap_or(uLong::MAX)
}

/// Narrows a byte count back to C's `uInt`.
///
/// Every value passed here came from a `uInt` (`avail_in` or `avail_out`) and was
/// only ever decreased, so the conversion is exact. The fallback keeps the function
/// total without a panicking path; it is unreachable.
#[inline]
#[must_use]
fn narrow_uInt(value: usize) -> uInt {
    uInt::try_from(value).unwrap_or(uInt::MAX)
}

/// Turns the core's optional `&'static str` message into C's `const char *`.
///
/// The single place the conversion happens. `zlib.h` L98 documents `msg` as "last
/// error message, NULL if no error", so [`None`] becomes a null pointer and
/// [`Some`] becomes a pointer into [`MESSAGES`].
///
/// ★ A pointer into a transient buffer would be a use-after-free the moment the
/// call returned, and a pointer into the stream's own storage would dangle at
/// [`inflateEnd`]. Looking the text up in a table of `&'static CStr` is what makes
/// the published pointer valid for the whole life of the process, which is what C's
/// string literals give and what `test/example.c` L92 and `test/minigzip.c` rely on
/// when they print `strm.msg` after a failure. It also supplies the NUL that Rust's
/// `&str` does not carry.
///
/// The scan is linear over eighteen short entries and runs only when the message
/// actually changed — [`Session::with_buffers`] compares against the value the
/// stream already published before writing `msg` — so it is off the hot path
/// entirely.
///
/// An unrecognised message cannot arise from the core, whose message set is exactly
/// [`MESSAGES`] and is checked against it by a test below. Should one ever be added
/// without updating the table, the result is a null pointer rather than a wild one:
/// reporting no message is a documented state, reporting a bad pointer is not.
pub(crate) fn message_ptr(msg: Option<&'static str>) -> *const c_char {
    let Some(text) = msg else {
        return core::ptr::null();
    };
    for candidate in MESSAGES {
        if candidate.to_bytes() == text.as_bytes() {
            return candidate.as_ptr();
        }
    }
    core::ptr::null()
}

/// Every message [`zlib_rs::inflate`] can publish, as a NUL-terminated C string.
///
/// Eighteen entries, which is the complete set: five gzip and zlib header texts
/// from the core's `inflate/header.rs`, ten block and trailer texts from its
/// `inflate/mod.rs`, and three shared decode texts from its `inflate/inffast.rs`.
/// The core declares them as `&str`, which carries no NUL; these are the same texts
/// in the form a C caller can read.
///
/// The core's constants are `pub(crate)`, so this table cannot be *derived* from them
/// and drift has to be caught rather than prevented. Two tests below do that from
/// opposite directions: one asserts the table is internally consistent — eighteen
/// entries, all distinct, all non-empty, every one recovered by [`message_ptr`] — and
/// the other drives real malformed streams through [`inflate`] and asserts that a
/// `Z_DATA_ERROR` always leaves a **non-null** `msg`, which is exactly the symptom a
/// missing entry would produce.
///
/// `deflate`'s messages are a disjoint set and belong to `deflate.rs`.
/// cbindgen:ignore
const MESSAGES: &[&core::ffi::CStr] = &[
    // From the core's `inflate/header.rs` -- RFC 1950 and RFC 1952 header parsing.
    c"incorrect header check",
    c"unknown compression method",
    c"invalid window size",
    c"unknown header flags set",
    c"header crc mismatch",
    // From the core's `inflate/mod.rs` -- block structure, code tables and trailers.
    c"invalid block type",
    c"invalid stored block lengths",
    c"too many length or distance symbols",
    c"invalid code lengths set",
    c"invalid bit length repeat",
    c"invalid code -- missing end-of-block",
    c"invalid literal/lengths set",
    c"invalid distances set",
    c"incorrect data check",
    c"incorrect length check",
    // From the core's `inflate/inffast.rs` -- also produced by the slow path.
    c"invalid literal/length code",
    c"invalid distance code",
    c"invalid distance too far back",
];

// ---------------------------------------------------------------------------
// Initialisation -- `inflate.c` L173-L217
// ---------------------------------------------------------------------------

/// Checks a caller's compile-time version and `sizeof(z_stream)` against this
/// library's.
///
/// The guard both init entry points open with (`inflate.c` L178-L180), reproduced
/// including its order of operations:
///
/// 1. a null `version` is rejected **before** it is dereferenced;
/// 2. only the **first character** is compared, so `"1.2.11"` is accepted and
///    `"2.0.0"` is not — the major version is the only component whose mismatch is
///    treated as fatal;
/// 3. `stream_size` must equal `size_of::<z_stream>()`, which catches a caller
///    compiled against a different `zlib.h`.
///
/// ★ This check precedes the `strm == Z_NULL` test in C, so `inflateInit_(Z_NULL,
/// "9.9", sizeof(z_stream))` answers `Z_VERSION_ERROR` and not `Z_STREAM_ERROR`.
/// The order is observable, and `test/infcover.c` L474 relies on the same ordering
/// in the `inflateBack` family, where `inflateBackInit_(Z_NULL, 0, win, 0, 0)` is
/// asserted to give `Z_VERSION_ERROR`.
///
/// Returns [`None`] when the version pair is acceptable, or [`Some`] carrying
/// [`ReturnCode::VERSION_ERROR`] when it is not.
///
/// # Safety
///
/// If `version` is non-null it must point at a readable NUL-terminated string;
/// only its first byte is read, so a single readable byte suffices.
pub(crate) unsafe fn version_error(
    version: *const c_char,
    stream_size: c_int,
) -> Option<ReturnCode> {
    if version.is_null() {
        return Some(ReturnCode::VERSION_ERROR);
    }
    // SAFETY: unsafe-site category 5 -- reading a caller's C string. This is a
    // C-string access rather than slice reconstruction: `version` is a
    // `*const c_char` the caller owns, non-null by the test immediately above, and
    // readable for at least its first byte and valid for the duration of this call
    // by this function's contract. Only `version[0]` is read, exactly as
    // `inflate.c` L178 reads it, so the NUL terminator is never relied on, no scan
    // is performed and no length is needed -- which is why a one-byte read is the
    // whole of the obligation. Nothing retains the pointer past this statement, so
    // no lifetime beyond the call is required. The `cast` to `*const u8` sidesteps
    // the signedness of `c_char`, which is `i8` on x86-64 and `u8` on several ARM
    // targets: both are one byte with alignment one, so the read is valid either
    // way and the comparison below needs no sign-dependent cast.
    let major = unsafe { version.cast::<u8>().read() };
    // `ZLIB_VERSION`'s first byte, read from the constant that `crate::util` asserts
    // *is* that byte -- and that it is `ZLIB_VER_MAJOR` rendered in ASCII. Reaching for
    // the constant rather than `ZLIB_VERSION.to_bytes().first()` keeps the comparison
    // infallible instead of an `Option` equality whose `None` arm is unreachable, and
    // puts the derivation in one place that both this gate and `deflate`'s share.
    let matches = major == crate::util::ZLIB_VER_MAJOR_DIGIT;
    let sized = usize::try_from(stream_size).is_ok_and(|size| size == size_of::<z_stream>());
    if matches && sized {
        None
    } else {
        Some(ReturnCode::VERSION_ERROR)
    }
}

/// `inflateInit2_` — initialises a stream for a given container format and window
/// size.
///
/// Declared at `zlib.h` L1911; ported from `inflate.c` L173-L212. The real function
/// behind the `inflateInit2` macro (`zlib.h` L859), which supplies the last two
/// arguments from the *caller's* `zlib.h` so that a version or layout mismatch is
/// caught at run time.
///
/// # `windowBits`
///
/// The decode is [`zlib_rs::config`]'s and is not duplicated here:
///
/// | Request | Meaning |
/// |---|---|
/// | `8..=15` | zlib wrapper, window of `1 << windowBits` bytes |
/// | `0` | zlib wrapper, window size taken from the stream's own header |
/// | `-8..=-15` | raw deflate: no header, no trailer, no check value |
/// | `windowBits + 16` | gzip wrapper only |
/// | `windowBits + 32` | automatic zlib-or-gzip detection |
///
/// # Returns
///
/// [`Z_OK`](ReturnCode::OK), or [`Z_VERSION_ERROR`](ReturnCode::VERSION_ERROR) for a
/// version or `stream_size` mismatch, [`Z_STREAM_ERROR`](ReturnCode::STREAM_ERROR)
/// for a null stream or an unacceptable `windowBits`, or
/// [`Z_MEM_ERROR`](ReturnCode::MEM_ERROR) if the state cannot be allocated. A null
/// `zalloc` or `zfree` is not an error here: each is substituted independently and
/// published back into the caller's stream, exactly as `inflate.c` L183-L195 does.
///
/// # Safety
///
/// `strm`, if non-null, must address a caller-allocated [`z_stream`] of at least
/// `size_of::<z_stream>()` bytes that no other thread is touching, and `version`,
/// if non-null, must be a readable C string. A stream that already holds a state
/// must be passed to [`inflateEnd`] first; overwriting a live state would leak it,
/// and `test/infcover.c`'s `mem_done` reports leaks.
#[no_mangle]
pub unsafe extern "C" fn inflateInit2_(
    strm: z_streamp,
    windowBits: c_int,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    guard_code(|| {
        // L178-L180, and note that this precedes the null-stream test.
        // SAFETY: `version` is a readable C string or null by this function's
        // contract, which is exactly what `version_error` requires.
        if let Some(error) = unsafe { version_error(version, stream_size) } {
            return error;
        }

        // L181.
        if strm.is_null() || !strm.is_aligned() {
            return fallback::STREAM_ERROR_CODE;
        }

        // L182: `strm->msg = Z_NULL;` -- "in case we return an error". Done before
        // anything can fail, so every error path below leaves `msg` null exactly as
        // C does.
        // SAFETY: unsafe-site category 1 -- writing one member of the caller's
        // stream. Non-null and aligned by the guard above, live by contract, and
        // written through a raw place so the caller's pointer keeps its provenance
        // for `install_state` below.
        // L182-L195: `strm->msg = Z_NULL` and the hook defaulting, which
        // `adopt_hooks` performs together because C performs them together and in
        // that order -- a null `zalloc` becomes the library's own and clears
        // `opaque`, then a null `zfree` is decided separately -- and writes all four
        // members back, so the caller sees the substitution `zlib.h` L151-L153
        // promises.
        // SAFETY: unsafe-site categories 1 and 4 -- reads and writes four members
        // through raw places, forming no reference, so `strm` stays usable below.
        // Non-null and aligned by the guard above, live by this function's contract;
        // the members written may be indeterminate beforehand, which is sound
        // because they are written rather than read.
        let allocator = unsafe { StreamAllocator::adopt_hooks(strm) };
        let Some(allocator) = allocator else {
            return fallback::STREAM_ERROR_CODE;
        };

        // ★ L197-L210 in order, and the order is observable. C allocates the state
        // **before** it validates `windowBits`, so a request that is both
        // unallocatable and malformed reports `Z_MEM_ERROR` rather than
        // `Z_STREAM_ERROR`, and `test/infcover.c`'s `mem_high()` sees the transient
        // allocation for a rejected request. Validating first would be simpler and
        // would leave nothing to undo, but it would also change both of those, so it
        // is not done.
        //
        // L197-L200's `ZALLOC` plus `zmemzero`, with `windowBits` not yet consulted:
        // the placeholder is a `DEF_WBITS` state, which owns nothing because
        // `inflate.c` L204 leaves `window` at `Z_NULL` and the window is allocated
        // lazily by the first `inflate()` that needs one. `DEF_WBITS` is always
        // valid, so this step has no failure of its own.
        let placeholder = match core_inflate_init2(InflateConfig::new(DEF_WBITS), allocator) {
            Ok(placeholder) => placeholder,
            Err(error) => return error,
        };

        // L202-L205: `strm->state = state; state->strm = strm; state->mode = HEAD;`.
        // The prefix is seeded here, which is what C's `mode = HEAD` at L205 achieves
        // with its comment "to pass state test in `inflateReset2()`".
        // SAFETY: unsafe-site categories 3 and 4 -- installs the state block. `strm`
        // is non-null, aligned and live; `allocator` is the caller's own triple, so
        // the block can only ever be released through the matching `zfree`. Any
        // previously installed state was removed by `inflateEnd`, which is this
        // function's documented obligation on its caller.
        let installed = unsafe {
            install_state(
                strm,
                &allocator,
                StateKind::Inflate,
                HEAD_TAG,
                InflateSlot {
                    state: placeholder,
                    head: core::ptr::null_mut(),
                    msg: None,
                },
            )
        };
        if let Err(error) = installed {
            // L199-L200: `if (state == Z_NULL) return Z_MEM_ERROR;`.
            return error;
        }

        // L206: `ret = inflateReset2(strm, windowBits);`. This is where `windowBits`
        // is finally decoded and where a bad one becomes `Z_STREAM_ERROR`.
        //
        // ★ The reset is not merely a validation step. Reaching `inflateResetKeep`
        // through it is what gives `inflateInit2_` its effect on the *caller's*
        // `z_stream`: `total_in`, `total_out` and `data_type` go to zero, `msg` to
        // null, and `adler` to `wrap & 1` for a wrapped stream (L105-L109). A zlib
        // stream therefore reports `adler == 1` before a single byte has been decoded,
        // and `test/example.c` would notice if it did not.
        // SAFETY: unsafe-site categories 1, 3 and 4 -- the state was just installed
        // with this `strm` and this tag, so every check inside `entry` passes.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };
        let outcome = session.run(|session| {
            match core_inflate_reset2(session.state_mut(), InflateConfig::new(windowBits)) {
                Ok(reset) => {
                    session.apply_reset(reset);
                    ReturnCode::OK
                }
                Err(error) => error,
            }
        });

        // L207-L210: `if (ret != Z_OK) { ZFREE(strm, state); strm->state = Z_NULL; }`.
        if outcome != ReturnCode::OK {
            // SAFETY: unsafe-site categories 3 and 4 -- releasing the block this
            // function installed moments ago, through the same allocator and with the
            // same `S`. Nothing borrows it any longer, and the release is of the most
            // recent allocation, hence LIFO.
            drop(unsafe { take_state::<InflateSlot>(strm, &allocator, StateKind::Inflate) });
        }
        outcome
    })
}

/// `inflateInit_` — initialises a stream for a zlib container with the largest
/// window.
///
/// Declared at `zlib.h` L1905; ported from `inflate.c` L214-L217, whose entire body
/// is a call to [`inflateInit2_`] with `windowBits` = [`DEF_WBITS`] (15). The real
/// function behind the `inflateInit` macro (`zlib.h` L382).
///
/// # Safety
///
/// As [`inflateInit2_`].
#[no_mangle]
pub unsafe extern "C" fn inflateInit_(
    strm: z_streamp,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    // SAFETY: every requirement is `inflateInit2_`'s, and this function's contract
    // is the same one; `windowBits` is a plain integer and adds none.
    unsafe { inflateInit2_(strm, DEF_WBITS, version, stream_size) }
}

// ---------------------------------------------------------------------------
// The decoder itself -- `inflate.c` L474-L1153
// ---------------------------------------------------------------------------

/// `inflate` — decompresses as much as the available input and output space allow.
///
/// Declared at `zlib.h` L405; ported from `inflate.c` L474-L1153. The engine is
/// [`zlib_rs::inflate::inflate`]; this function is the boundary around it.
///
/// # The resumable contract
///
/// `inflate` may be called repeatedly with partial input and partial output space
/// and will resume from wherever it stopped, because the whole decoder is a state
/// machine whose position lives in the stream's state. That is the contract
/// `zlib.h` L405-L470 offers and it is preserved exactly: nothing here caches state
/// across the boundary, and the mode tag is re-read on entry and written back on
/// exit so that even a mode assigned by *caller* code between two calls is honoured.
///
/// # `flush`
///
/// ★ Inflate's flush domain is wider than deflate's, and all seven values are
/// meaningful:
///
/// | Value | Name | Effect |
/// |---|---|---|
/// | 0 | `Z_NO_FLUSH` | decode as much as possible |
/// | 1 | `Z_PARTIAL_FLUSH` | as `Z_NO_FLUSH` for inflate |
/// | 2 | `Z_SYNC_FLUSH` | as `Z_NO_FLUSH` for inflate |
/// | 3 | `Z_FULL_FLUSH` | as `Z_NO_FLUSH` for inflate |
/// | 4 | `Z_FINISH` | forbid a `Z_OK` return: no progress becomes `Z_BUF_ERROR` |
/// | 5 | `Z_BLOCK` | stop at a deflate block boundary |
/// | 6 | `Z_TREES` | stop immediately after a block header |
///
/// A value outside `0..=6` is **not** refused. It is passed through unvalidated and behaves
/// exactly as `Z_NO_FLUSH` behaves, because C's `inflate` validates `flush` nowhere: the
/// value is only ever compared against `Z_FINISH`, `Z_BLOCK` and `Z_TREES`. The ★ note
/// beside the pass-through in the body below carries the full argument, including why
/// rejecting one would be stricter than the reference and therefore wrong.
///
/// # The entry guard
///
/// `inflate.c` L494-L496 rejects three things, and the two pointer tests are this
/// function's rather than the core's: a null `next_out`, and a null `next_in`
/// combined with a non-zero `avail_in`. Note what is **not** rejected — a null
/// `next_in` with `avail_in == 0` is entirely legal and `test/infcover.c` L392-L393
/// sets exactly that state before every `inflateInit2`.
///
/// # Returns
///
/// `Z_OK`, `Z_STREAM_END`, `Z_NEED_DICT` (with the dictionary's Adler-32 left in
/// `strm->adler`), `Z_DATA_ERROR` (with the reason in `strm->msg`), `Z_STREAM_ERROR`,
/// `Z_MEM_ERROR` or `Z_BUF_ERROR`. `Z_MEM_ERROR` is deliberately **sticky**: the
/// stream stays in `MEM` and every later call reports it again, which is what
/// `test/infcover.c` L421-L422 asserts by calling twice.
///
/// # Untrusted input
///
/// This is the library's primary attack surface: it must never panic, hang or
/// over-read for **any** input, however malformed. The decoder is safe Rust
/// throughout, so an out-of-range distance or a malformed table is a `Z_DATA_ERROR`
/// rather than a memory-safety event. That property is argued structurally *and*
/// exercised: `fuzz/fuzz_targets/fuzz_inflate.rs` drives this entry point with
/// arbitrary bytes across the whole `windowBits` and `flush` space, under
/// AddressSanitizer and under a `test/infcover.c`-style tracking allocator that
/// detects leaks, non-LIFO frees and rogue frees and that induces `Z_MEM_ERROR` at
/// controlled points. It reconciles `total_in`/`total_out` against the bytes the
/// caller's buffers actually gave up and took, and it surrounds each `gz_header`
/// buffer with guard regions to catch a write past `extra_max`, `name_max` or
/// `comm_max`.
///
/// # Safety
///
/// `strm`, if non-null, must be a live [`z_stream`] initialised by [`inflateInit_`]
/// or [`inflateInit2_`], whose `(next_in, avail_in)` names a readable region and
/// whose `(next_out, avail_out)` names a writable region that does not overlap it. A
/// `gz_header` installed by [`inflateGetHeader`] must still be live, together with
/// whichever of its `extra`, `name` and `comment` buffers were non-null.
#[no_mangle]
pub unsafe extern "C" fn inflate(strm: z_streamp, flush: c_int) -> c_int {
    guard_code(|| {
        // L494: `inflateStateCheck(strm)`. Runs before the pointer tests below,
        // because `strm->next_out` cannot be read until `strm` is known good.
        // SAFETY: unsafe-site categories 1, 3 and 4. `strm` is the caller's raw
        // `z_streamp`, and nothing is dereferenced before it is checked: `entry`
        // tests non-null and alignment first and answers `None` otherwise, so a
        // null or misaligned stream returns the documented refusal instead of
        // reading memory. What this call site supplies is what no test can: a
        // non-null `strm` addresses a live `z_stream`, whose `state` -- if non-null
        // -- is a state block this library installed and whose `zalloc`/`zfree`
        // pair is either both null or both valid. `entry` then tag- and
        // owner-validates that block before treating it as state. This is the first
        // thing the body does and the returned `Session` is the only handle to the
        // stream and its state for the rest of the call, so no other borrow of
        // either can exist alongside it.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };

        // ★ `flush` is passed through exactly as the caller wrote it, unvalidated,
        // because C's `inflate` validates it nowhere. The value is only ever compared
        // against `Z_FINISH`, `Z_BLOCK` and `Z_TREES` (`inflate.c` L622, L676, L1103
        // and L1147), so any other integer behaves as `Z_NO_FLUSH` behaves -- and the
        // core compares the same three values against the same `i32`, so it reaches
        // the same answer for every input. Rejecting an out-of-range value here would
        // make this function stricter than the reference for a caller C serves happily:
        // a wrapper that forwards a flush it computed, or an old binary built against a
        // header with different spellings, would be told `Z_STREAM_ERROR` where C would
        // decompress.
        session.run(|session| {
            let strm = session.strm;
            // L494-L496: the two pointer halves of the entry guard.
            // SAFETY: unsafe-site category 1 -- reads two members of the caller's
            // stream through raw places, forming no reference. `strm` was established
            // non-null, aligned and live by `entry`.
            let (next_in, avail_in, next_out) = unsafe {
                (
                    core::ptr::addr_of!((*strm).next_in).read(),
                    core::ptr::addr_of!((*strm).avail_in).read(),
                    core::ptr::addr_of!((*strm).next_out).read(),
                )
            };
            if next_out.is_null() || (next_in.is_null() && avail_in != 0) {
                return fallback::STREAM_ERROR_CODE;
            }

            // ★ Not a C guard, and not a refusal either. C decodes happily with an
            // overlapping input and output -- `zlib.h` L138-L139 describes the two pairs
            // independently and never requires them to be distinct, and `inflate.c`
            // L305-L326 simply `LOAD()`s both into locals -- so an overlapping pair is an
            // input this library can be handed and must serve. What it cannot do is let
            // `with_buffers` build a `&[u8]` over `(next_in, avail_in)` and a `&mut [u8]`
            // over `(next_out, avail_out)`, because two such borrows over one region are
            // undefined behaviour *whether or not either is ever touched*. So the input is
            // copied first and the decoder reads the copy; `AliasScratch` documents the
            // whole argument, including why this is the same answer `deflate` gives.
            //
            // Only a failed snapshot allocation is refused, with the `Z_STREAM_ERROR` C
            // gives an unusable pointer pair.
            //
            // Nothing is dereferenced to decide it: `Session::buffers_overlap` compares
            // addresses, so it is safe to run on the raw members, which is the only point at
            // which the answer can still be acted on.
            //
            // SAFETY: the stream is the validated one `entry` produced, so its four
            // buffer members are readable as plain scalars.
            // SAFETY: unsafe-site category 4 -- `scratch_allocator`'s contract. `strm` is the
            // validated stream `entry` produced, and the three hook members are read through
            // raw places. The snapshot has to come from the stream's own allocator: it is
            // `avail_in` bytes, the caller's own number, so a custom arena must see it and a
            // `mem_limit` must be able to refuse it.
            let allocator = unsafe { scratch_allocator(strm) };
            // SAFETY: the stream is the validated one `entry` produced, so its four buffer
            // members are readable as plain scalars; nothing is dereferenced through them.
            let overlapping = unsafe { session.buffers_overlap() };
            let mut scratch = if overlapping {
                // SAFETY: unsafe-site category 2 -- `AliasScratch::capture`'s contract. The
                // stream is the validated one, its input region is readable for `avail_in`
                // bytes, and no mutable borrow of it exists yet: the only one this library
                // creates is the output borrow `with_buffers` makes below.
                let captured =
                    unsafe { AliasScratch::capture(&allocator, next_in, widen(avail_in)) };
                if captured.is_none() {
                    return fallback::STREAM_ERROR_CODE;
                }
                captured
            } else {
                None
            };
            // Held in a binding that outlives the stream view, as `scratch_view` requires.
            //
            // SAFETY: unsafe-site category 2 -- `scratch_view` fabricates `'static`; `scratch`
            // is a local of this closure and outlives the `with_buffers` call below, which is
            // the only thing that ever reads the slice.
            let captured_input = unsafe { scratch_view(scratch.as_ref()) };

            // L497-L1152: the whole engine, with `LOAD`/`RESTORE` and the epilogue
            // handled by `with_buffers`.
            // The caller's header buffers, bound for this call only.
            //
            // SAFETY: unsafe-site category 6 -- `bind_header_fields`' contract is this
            // function's: the `gz_header` a caller installed with `inflateGetHeader`,
            // and every buffer it points at, must stay live and valid for its
            // advertised capacity while the header is being read, which `zlib.h`
            // L1070-L1090 requires of the caller.
            unsafe {
                session.bind_header_fields();
            }

            // SAFETY: the stream is the validated one and its two regions are readable and
            // writable by this function's contract; they are either disjoint or the input is
            // replaced by `captured_input`, which is what `with_buffers` requires.
            let outcome = unsafe {
                session.with_buffers(captured_input, |state, stream| {
                    core_inflate(state, stream, flush)
                })
            };

            // The gzip header the parse may have filled. C has already written it
            // through `state->head`; this is the same publication, deferred to the
            // one point where the sink can be read back. Publishing comes first
            // because it needs the absence flags the parse set, and unbinding second,
            // so that no view of the caller's buffers survives this call.
            session.publish_header();
            session.unbind_header_fields();

            // The snapshot goes back to the allocator it came from once nothing borrows it:
            // `with_buffers` has returned, so the stream view it built is gone, and this is
            // still inside the call, so a tracking allocator sees one strictly nested
            // allocate/free pair.
            if let Some(scratch) = scratch.as_mut() {
                scratch.release(&allocator);
            }

            outcome
        })
    })
}

/// `inflateEnd` — releases everything a stream's state owns.
///
/// Declared at `zlib.h` L525; ported from `inflate.c` L1155-L1165, which frees the
/// window, frees the state, clears `strm->state` and answers `Z_OK`.
///
/// # ★ The release order is load-bearing
///
/// Both releases go through the **caller's** `zfree`, and the window goes back
/// **before** the state block — precisely the order of `inflate.c` L1159-L1161. That
/// is not tidiness: `test/infcover.c`'s instrumented allocator keeps its zone as a
/// most-recent-first list and increments `notlifo` for any free that is not of the
/// head (its L120), and its `mem_done` reports the count. Since the block is
/// allocated by [`inflateInit2_`] and the window later, by the first `inflate()` that
/// needs one, the window is always the newer of the two and must be freed first.
/// Reversing the two produces a "frees not LIFO" report — observed, not hypothesised.
///
/// [`inflateCopy`] preserves the same invariant from the other end, by allocating its
/// destination block before its destination window.
///
/// [`z_stream::state`] is cleared before the block is released, so no path can leave
/// the caller holding a dangling pointer — not even one on which the release itself
/// failed.
///
/// # Returns
///
/// `Z_OK`, or `Z_STREAM_ERROR` for a stream that fails the state check. A null
/// stream therefore answers `Z_STREAM_ERROR`, which `test/infcover.c` L394 asserts.
///
/// # Safety
///
/// `strm`, if non-null, must be a live [`z_stream`] whose state, if any, was
/// installed by [`inflateInit2_`] and has not already been ended.
#[no_mangle]
pub unsafe extern "C" fn inflateEnd(strm: z_streamp) -> c_int {
    guard_code(|| {
        // L1157: the state check, whose allocator half also gives us the hooks the
        // release must use.
        // SAFETY: unsafe-site category 4 -- reads the three hook members through raw
        // places. `strm` is live or null by this function's contract.
        let allocator = unsafe { StreamAllocator::from_stream_ptr(strm) };
        let Some(allocator) = allocator else {
            return fallback::STREAM_ERROR_CODE;
        };

        // L1159: `if (state->window != Z_NULL) ZFREE(strm, state->window);` -- the
        // window FIRST, so that the two frees are LIFO. See the note above. The borrow
        // is taken and dropped inside this block, before `take_state` reads the same
        // state pointer.
        {
            // SAFETY: unsafe-site category 3 -- the opaque state round-trip. The four
            // validity checks run inside, and `strm` is a live stream whose state, if
            // any, came from `install_state`, by this function's contract. The borrow
            // is the only one alive and ends with this scope.
            let block = unsafe { checked_state_mut::<InflateSlot>(strm, StateKind::Inflate) };
            let Some(block) = block else {
                return fallback::STREAM_ERROR_CODE;
            };
            block.state_mut().state.discard_window();
        }

        // L1160-L1161: `ZFREE(strm, strm->state); strm->state = Z_NULL;`.
        // `take_state` repeats the same four checks -- which cannot fail now that the
        // block above passed them -- clears the member, and releases the block.
        // SAFETY: unsafe-site categories 3 and 4 -- the opaque state round-trip and
        // the release through the caller's `zfree`. The block was produced by
        // `install_state` with this `S` and this triple, `zlib.h` L140-L142 forbids
        // the application from changing the hooks once initialised, and the borrow
        // taken above has ended.
        let block = unsafe { take_state::<InflateSlot>(strm, &allocator, StateKind::Inflate) };
        let Some(mut block) = block else {
            return fallback::STREAM_ERROR_CODE;
        };

        // Always `Z_OK`: the window is already gone, so nothing here can fail. The state
        // is reached through a borrow rather than moved out, which is the rule
        // `zlib_rs::allocate::ForeignBlock` sets for any path that can free a block.
        core_inflate_end(&mut block.state_mut().state)
    })
}

// ---------------------------------------------------------------------------
// Dictionaries -- `inflate.c` L1167-L1217
// ---------------------------------------------------------------------------

/// `inflateSetDictionary` — supplies the preset dictionary a stream asked for.
///
/// Declared at `zlib.h` L913; ported from `inflate.c` L1187-L1217. Two callers are
/// supported, and the shape of the guard at L1196 is what admits both:
///
/// * a **zlib** stream that returned `Z_NEED_DICT` and is therefore in `DICT`. Its
///   dictionary is checked against the Adler-32 the stream published in
///   `strm->adler`, and a mismatch is `Z_DATA_ERROR`.
/// * a **raw** stream, at any point before decoding starts, priming the window with
///   history. A raw stream carries no dictionary id, so there is nothing to check.
///
/// Anything else — a wrapped stream that is not waiting for a dictionary — is
/// `Z_STREAM_ERROR`, which is what `test/infcover.c` L362-L363 asserts for a freshly
/// initialised zlib stream.
///
/// # ★ A zero-length dictionary must still be able to fail
///
/// `test/infcover.c` L326-L328 sets `mem_limit(&strm, 1)` and then requires
/// `inflateSetDictionary(&strm, out, 0)` to answer `Z_MEM_ERROR`. So the
/// `dictLength == 0` case must **not** be short-circuited: it takes the same
/// allocating path as any other length, because `updatewindow` allocates the window
/// before it copies anything. Two lines later the same call must answer `Z_OK` once
/// the limit is lifted and the harness has written `mode = DICT` through the raw
/// state pointer — which is the assignment `entry` exists to honour.
///
/// # Safety
///
/// `strm`, if non-null, must be a live initialised [`z_stream`]. If `dictLength` is
/// non-zero, `dictionary` must be readable for that many bytes; a null `dictionary`
/// with a zero `dictLength` is legal and is what `test/infcover.c` L362 passes.
#[no_mangle]
pub unsafe extern "C" fn inflateSetDictionary(
    strm: z_streamp,
    dictionary: *const Bytef,
    dictLength: uInt,
) -> c_int {
    guard_code(|| {
        // L1192: the state check.
        // SAFETY: unsafe-site categories 1, 3 and 4. `strm` is the caller's raw
        // `z_streamp`, and nothing is dereferenced before it is checked: `entry`
        // tests non-null and alignment first and answers `None` otherwise, so a
        // null or misaligned stream returns the documented refusal instead of
        // reading memory. What this call site supplies is what no test can: a
        // non-null `strm` addresses a live `z_stream`, whose `state` -- if non-null
        // -- is a state block this library installed and whose `zalloc`/`zfree`
        // pair is either both null or both valid. `entry` then tag- and
        // owner-validates that block before treating it as state. This is the first
        // thing the body does and the returned `Session` is the only handle to the
        // stream and its state for the rest of the call, so no other borrow of
        // either can exist alongside it.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };

        // L1196-L1197: `if (state->wrap != 0 && state->mode != DICT) return
        // Z_STREAM_ERROR;`.
        //
        // ★ **Applied here, before the dictionary becomes a slice, because that is
        // where C applies it.** C returns at L1197 without `adler32` (L1200) or the
        // window update (L1211) having read a single dictionary byte, so a wrapped
        // stream that is not waiting for a dictionary -- exactly what
        // `test/infcover.c` L362-L363 passes -- may legitimately supply a stale or wild
        // non-null pointer and still be told `Z_STREAM_ERROR`. Reconstructing the slice
        // first would turn that documented refusal into undefined behaviour. The core
        // applies the identical predicate, so the answer cannot depend on which side
        // asked.
        if !session.state().accepts_dictionary() {
            session.finish();
            return fallback::STREAM_ERROR_CODE;
        }

        // SAFETY: unsafe-site category 2 -- slice reconstruction. `input_slice`
        // branches on a zero length or a null pointer, so the legal
        // `(Z_NULL, 0)` pair yields an empty slice rather than undefined behaviour,
        // and a non-zero length is readable by this function's contract. The
        // dictionary is only read.
        let dictionary = unsafe { input_slice(dictionary, dictLength) };

        session.run(|session| core_set_dictionary(session.state_mut(), dictionary))
    })
}

/// `inflateGetDictionary` — reads the sliding window back out as a dictionary.
///
/// Declared at `zlib.h` L936; ported from `inflate.c` L1167-L1185. The window is
/// circular, so the history comes back in two pieces — the older half at
/// `window[wnext..whave]` first, then the newer half at `window[..wnext]` — which
/// puts the bytes in stream order.
///
/// ★ Both arguments are independently optional, exactly as C's two `Z_NULL` tests at
/// L1176 and L1182 make them: a caller may pass a null `dictionary` to ask only how
/// much history there is, or a null `dictLength` to copy without being told the
/// length.
///
/// ## ★ `dictLength` is an out-parameter and is never read
///
/// `zlib.h` L936-L942 requires the caller's buffer to hold at least 32768 bytes and
/// gives this function no way to learn its actual size. `dictLength` is **not** that
/// size: it is where the length is *reported*, and `zlib.h` documents no obligation
/// on its contents beforehand, so a conforming caller may pass a pointer to an
/// uninitialised `uInt` — `uInt len; inflateGetDictionary(strm, buf, &len);` is
/// idiomatic and correct C. Reading it to obtain a capacity would therefore be
/// undefined behaviour, and would additionally truncate the result to whatever
/// happened to be in that word.
///
/// So the length is taken from the *stream*, exactly as C takes it: the core is asked
/// for `whave` with no destination at all, the destination is then built for exactly
/// that many bytes, the copy runs once, and `*dictLength` is written afterwards
/// without ever having been read. `deflateGetDictionary` consults its core twice for
/// the same reason and the note there carries the argument in full.
///
/// `whave` never exceeds the window size and hence never exceeds 32768, so the
/// destination this builds always lies inside the capacity `zlib.h` L936-L942
/// guarantees.
///
/// # Safety
///
/// `strm`, if non-null, must be a live initialised [`z_stream`]. If `dictionary` is
/// non-null it must be writable for at least the length this function reports through
/// `dictLength`, which never exceeds 32768 — the contract `zlib.h` L936-L942 states.
/// `dictLength`, if non-null, must be a valid, aligned, **writable** `uInt`; its
/// previous contents are never read and may be indeterminate.
#[no_mangle]
pub unsafe extern "C" fn inflateGetDictionary(
    strm: z_streamp,
    dictionary: *mut Bytef,
    dictLength: *mut uInt,
) -> c_int {
    guard_code(|| {
        // L1172: the state check.
        // SAFETY: unsafe-site categories 1, 3 and 4. `strm` is the caller's raw
        // `z_streamp`, and nothing is dereferenced before it is checked: `entry`
        // tests non-null and alignment first and answers `None` otherwise, so a
        // null or misaligned stream returns the documented refusal instead of
        // reading memory. What this call site supplies is what no test can: a
        // non-null `strm` addresses a live `z_stream`, whose `state` -- if non-null
        // -- is a state block this library installed and whose `zalloc`/`zfree`
        // pair is either both null or both valid. `entry` then tag- and
        // owner-validates that block before treating it as state. This is the first
        // thing the body does and the returned `Session` is the only handle to the
        // stream and its state for the rest of the call, so no other borrow of
        // either can exist alongside it.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };

        session.run(|session| {
            // The length, asked for on its own: `dictionary` is `None` and the length
            // slot is `Some`, which is the "only the length is returned" shape of
            // L1176-L1183. The call has no side effect -- it takes a shared borrow of
            // the state -- so the second call below reports the same number.
            let mut length: u32 = 0;
            let probe = core_get_dictionary(session.state(), None, Some(&mut length));
            if probe != ReturnCode::OK {
                // The core documents no failure once a valid state is in hand; the
                // status is forwarded rather than assumed so that this function can
                // never invent a `Z_OK`.
                return probe;
            }

            // `whave` is bounded by the window, and `inflateInit2_` bounds the window
            // at `1 << 15`; the clamp states that invariant where the destination is
            // built rather than trusting it from a distance. It never discards
            // history, because no reachable `whave` exceeds it.
            let capacity = usize::try_from(length).unwrap_or(0).min(MAX_WINDOW_BYTES);

            // `if (dictionary != Z_NULL && state->whave)` -- C's two conditions at
            // L1176, in C's order.
            let outcome = if dictionary.is_null() || capacity == 0 {
                ReturnCode::OK
            } else {
                // SAFETY: unsafe-site category 2 -- reconstruction of the caller's
                // dictionary buffer, performed once. `dictionary` is non-null by the
                // test above and writable for `capacity` bytes by this function's
                // contract, since `capacity` is the length the probe just reported and
                // cannot exceed 32768. It is write-only storage rather than a byte
                // slice, because a caller's dictionary buffer -- like its `next_out`
                // -- is guaranteed writable and nothing more. The region cannot alias
                // the state, which lives in a different allocation, nor the
                // `dictLength` slot, which is a `uInt` rather than part of this buffer.
                let mut target = unsafe { output_region(dictionary, narrow_uInt(capacity)) };
                core_get_dictionary(session.state(), Some(&mut target), None)
            };

            if !dictLength.is_null() {
                // L1182-L1183: `*dictLength = state->whave`, after the copy as C
                // orders it, and from the probe so that a caller which passed a null
                // `dictionary` still learns the length.
                // SAFETY: unsafe-site category 1 -- writing the caller's
                // out-parameter. Non-null by the test above, and a valid, aligned,
                // writable `uInt` by this function's contract. Written rather than
                // read, so its previous contents may be indeterminate.
                unsafe {
                    dictLength.write(length);
                }
            }
            outcome
        })
    })
}

// ---------------------------------------------------------------------------
// Resets -- `inflate.c` L99-L171
// ---------------------------------------------------------------------------

/// `inflateResetKeep` — resets the stream but keeps the window's contents.
///
/// Declared at `zlib.h` L2039; ported from `inflate.c` L99-L123. The window, `wsize`,
/// `whave` and `wnext` all survive, so decoding can restart with the previous
/// stream's history still reachable as a dictionary.
///
/// Several of the values C assigns are deliberately **not** zero: `flags` goes to
/// -1 ("raw, or no header seen yet"), `dmax` to 32768, `back` to -1, `sane` to true
/// and `mode` to `HEAD`. The caller's stream additionally gets `total_in`,
/// `total_out` and `data_type` zeroed, `msg` nulled, and — for a wrapped stream only
/// — `adler` set to `wrap & 1`.
///
/// # Safety
///
/// `strm`, if non-null, must be a live [`z_stream`] initialised by
/// [`inflateInit2_`].
#[no_mangle]
pub unsafe extern "C" fn inflateResetKeep(strm: z_streamp) -> c_int {
    guard_code(|| {
        // L102: the state check.
        // SAFETY: unsafe-site categories 1, 3 and 4. `strm` is the caller's raw
        // `z_streamp`, and nothing is dereferenced before it is checked: `entry`
        // tests non-null and alignment first and answers `None` otherwise, so a
        // null or misaligned stream returns the documented refusal instead of
        // reading memory. What this call site supplies is what no test can: a
        // non-null `strm` addresses a live `z_stream`, whose `state` -- if non-null
        // -- is a state block this library installed and whose `zalloc`/`zfree`
        // pair is either both null or both valid. `entry` then tag- and
        // owner-validates that block before treating it as state. This is the first
        // thing the body does and the returned `Session` is the only handle to the
        // stream and its state for the rest of the call, so no other borrow of
        // either can exist alongside it.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };
        session.run(|session| {
            let reset = core_inflate_reset_keep(session.state_mut());
            session.apply_reset(reset);
            ReturnCode::OK
        })
    })
}

/// `inflateReset` — resets the stream and forgets the window's contents.
///
/// Declared at `zlib.h` L986; ported from `inflate.c` L125-L134, which clears the
/// three window cursors and then does everything [`inflateResetKeep`] does. The
/// allocation itself is kept, so no reset ever reallocates and a following
/// `inflate()` reuses the buffer it already has.
///
/// # Safety
///
/// As [`inflateResetKeep`].
#[no_mangle]
pub unsafe extern "C" fn inflateReset(strm: z_streamp) -> c_int {
    guard_code(|| {
        // L128: the state check.
        // SAFETY: unsafe-site categories 1, 3 and 4. `strm` is the caller's raw
        // `z_streamp`, and nothing is dereferenced before it is checked: `entry`
        // tests non-null and alignment first and answers `None` otherwise, so a
        // null or misaligned stream returns the documented refusal instead of
        // reading memory. What this call site supplies is what no test can: a
        // non-null `strm` addresses a live `z_stream`, whose `state` -- if non-null
        // -- is a state block this library installed and whose `zalloc`/`zfree`
        // pair is either both null or both valid. `entry` then tag- and
        // owner-validates that block before treating it as state. This is the first
        // thing the body does and the returned `Session` is the only handle to the
        // stream and its state for the rest of the call, so no other borrow of
        // either can exist alongside it.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };
        session.run(|session| {
            let reset = core_inflate_reset(session.state_mut());
            session.apply_reset(reset);
            ReturnCode::OK
        })
    })
}

/// `inflateReset2` — resets the stream and changes its container format or window
/// size.
///
/// Declared at `zlib.h` L997; ported from `inflate.c` L136-L171. `windowBits` is
/// interpreted exactly as [`inflateInit2_`] interprets it, and a window already
/// allocated for a *different* exponent is released because it is the wrong size
/// (L162-L165); one allocated for the same exponent is kept.
///
/// The request is validated before anything changes, so a rejected call leaves the
/// stream exactly as it was — which is what lets `test/infcover.c` L344 reset a
/// stream to `-8` and then end it cleanly.
///
/// # Returns
///
/// `Z_OK`, or `Z_STREAM_ERROR` for a failed state check or a `windowBits` outside
/// the accepted matrix.
///
/// # Safety
///
/// As [`inflateResetKeep`].
#[no_mangle]
pub unsafe extern "C" fn inflateReset2(strm: z_streamp, windowBits: c_int) -> c_int {
    guard_code(|| {
        // L141: the state check.
        // SAFETY: unsafe-site categories 1, 3 and 4. `strm` is the caller's raw
        // `z_streamp`, and nothing is dereferenced before it is checked: `entry`
        // tests non-null and alignment first and answers `None` otherwise, so a
        // null or misaligned stream returns the documented refusal instead of
        // reading memory. What this call site supplies is what no test can: a
        // non-null `strm` addresses a live `z_stream`, whose `state` -- if non-null
        // -- is a state block this library installed and whose `zalloc`/`zfree`
        // pair is either both null or both valid. `entry` then tag- and
        // owner-validates that block before treating it as state. This is the first
        // thing the body does and the returned `Session` is the only handle to the
        // stream and its state for the rest of the call, so no other borrow of
        // either can exist alongside it.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };
        session.run(|session| {
            // L144-L170: the `windowBits` decode, the conditional window release and
            // the reset, all of which `crate::config` and the state own.
            match core_inflate_reset2(session.state_mut(), InflateConfig::new(windowBits)) {
                Ok(reset) => {
                    session.apply_reset(reset);
                    ReturnCode::OK
                }
                Err(error) => error,
            }
        })
    })
}

// ---------------------------------------------------------------------------
// Bit-level and introspection entry points
// ---------------------------------------------------------------------------

/// `inflatePrime` — inserts bits into the accumulator ahead of the stream.
///
/// Declared at `zlib.h` L1011; ported from `inflate.c` L219-L236. Used to begin
/// decoding at a bit position that is not a byte boundary, which is what a caller
/// resuming from an [`inflateMark`] position needs.
///
/// Three cases, all reproduced: `bits == 0` is a no-op that answers `Z_OK`; a
/// **negative** `bits` clears the accumulator entirely and also answers `Z_OK` —
/// `test/infcover.c` L360 asserts exactly that with `inflatePrime(&strm, -1, 0)`;
/// and a request for more than sixteen bits at once, or one that would push the
/// accumulator above thirty-two bits, is `Z_STREAM_ERROR`.
///
/// # Safety
///
/// As [`inflateResetKeep`]. `bits` and `value` are plain integers and add nothing.
#[no_mangle]
pub unsafe extern "C" fn inflatePrime(strm: z_streamp, bits: c_int, value: c_int) -> c_int {
    guard_code(|| {
        // L223: the state check, which precedes the `bits == 0` shortcut at L224.
        // SAFETY: unsafe-site categories 1, 3 and 4. `strm` is the caller's raw
        // `z_streamp`, and nothing is dereferenced before it is checked: `entry`
        // tests non-null and alignment first and answers `None` otherwise, so a
        // null or misaligned stream returns the documented refusal instead of
        // reading memory. What this call site supplies is what no test can: a
        // non-null `strm` addresses a live `z_stream`, whose `state` -- if non-null
        // -- is a state block this library installed and whose `zalloc`/`zfree`
        // pair is either both null or both valid. `entry` then tag- and
        // owner-validates that block before treating it as state. This is the first
        // thing the body does and the returned `Session` is the only handle to the
        // stream and its state for the rest of the call, so no other borrow of
        // either can exist alongside it.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };
        session.run(|session| core_inflate_prime(session.state_mut(), bits, value))
    })
}

/// `inflateMark` — reports how far into the current block decoding has got.
///
/// Declared at `zlib.h` L1042; ported from `inflate.c` L1397-L1406.
///
/// ★ The return type is **`long`**, not `int`, and the value is a **composite**
/// rather than a status code. The high bits are the number of unused bits of the
/// last consumed byte, negated — C's `state->back`, which is -1 when no code is
/// pending — and the low sixteen bits are the progress through a copy: the bytes
/// still to copy in `COPY`, the bytes already copied in `MATCH`, and zero elsewhere.
/// A negative result is therefore ordinary and must not be mistaken for an error.
///
/// A stream that fails the state check gets `-(1L << 16)` — C's L1400-L1401 — which
/// is "no bits back, no copy in progress" pushed one whole unit negative so that it
/// cannot collide with a real answer. Note that this is **not** `Z_STREAM_ERROR`.
///
/// # Safety
///
/// As [`inflateResetKeep`].
#[no_mangle]
pub unsafe extern "C" fn inflateMark(strm: z_streamp) -> c_long {
    guard(|| {
        // L1401: the state check, whose failure value is the composite above rather
        // than a status code.
        // SAFETY: unsafe-site categories 1, 3 and 4. `strm` is the caller's raw
        // `z_streamp`, and nothing is dereferenced before it is checked: `entry`
        // tests non-null and alignment first and answers `None` otherwise, so a
        // null or misaligned stream returns the documented refusal instead of
        // reading memory. What this call site supplies is what no test can: a
        // non-null `strm` addresses a live `z_stream`, whose `state` -- if non-null
        // -- is a state block this library installed and whose `zalloc`/`zfree`
        // pair is either both null or both valid. `entry` then tag- and
        // owner-validates that block before treating it as state. This is the first
        // thing the body does and the returned `Session` is the only handle to the
        // stream and its state for the rest of the call, so no other borrow of
        // either can exist alongside it.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return narrow_mark(INFLATE_MARK_BAD_STATE);
        };
        session.run(|session| narrow_mark(core_inflate_mark(session.state())))
    })
}

/// Narrows the core's `i64` mark to C's `long`.
///
/// `back` never exceeds the forty-eight bits a code can cost and the low half is a
/// sixteen-bit copy length, so the composite always fits in a 32-bit `long` and this
/// is lossless on LLP64 as well as LP64. The fallback is unreachable and answers the
/// documented bad-state value rather than an arbitrary number, so even an impossible
/// input yields something a caller can interpret.
#[inline]
#[must_use]
fn narrow_mark(mark: i64) -> c_long {
    c_long::try_from(mark).unwrap_or(BAD_STATE_MARK)
}

/// `-(1 << 16)` in C's `long`: what [`inflateMark`] answers for an unusable stream.
///
/// `inflate.c` L1400-L1401. Computed rather than written as a literal so that it is
/// the same number on both integer models.
/// cbindgen:ignore
const BAD_STATE_MARK: c_long = -(1 << 16);

/// `inflateCodesUsed` — reports how many decode-table entries the current block's
/// tables occupy.
///
/// Declared at `zlib.h` L2038; ported from `inflate.c` L1408-L1413. C computes
/// `state->next - state->codes`, a pointer difference into the inline arena; the core
/// keeps that cursor as an index, so it is read directly.
///
/// ★ The return type is **`unsigned long`**. A stream that fails the state check gets
/// `(unsigned long)-1`, i.e. all ones in whatever width `unsigned long` has on the
/// target — which is why the value travels as [`INFLATE_CODES_USED_BAD_STATE`] and is
/// narrowed rather than written as a literal.
///
/// # Safety
///
/// As [`inflateResetKeep`].
#[no_mangle]
pub unsafe extern "C" fn inflateCodesUsed(strm: z_streamp) -> c_ulong {
    guard(|| {
        // L1410: the state check.
        // SAFETY: unsafe-site categories 1, 3 and 4. `strm` is the caller's raw
        // `z_streamp`, and nothing is dereferenced before it is checked: `entry`
        // tests non-null and alignment first and answers `None` otherwise, so a
        // null or misaligned stream returns the documented refusal instead of
        // reading memory. What this call site supplies is what no test can: a
        // non-null `strm` addresses a live `z_stream`, whose `state` -- if non-null
        // -- is a state block this library installed and whose `zalloc`/`zfree`
        // pair is either both null or both valid. `entry` then tag- and
        // owner-validates that block before treating it as state. This is the first
        // thing the body does and the returned `Session` is the only handle to the
        // stream and its state for the rest of the call, so no other borrow of
        // either can exist alongside it.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return narrow_uLong(INFLATE_CODES_USED_BAD_STATE);
        };
        session.run(|session| narrow_uLong(core_codes_used(session.state())))
    })
}

/// `inflateSyncPoint` — reports whether the stream is stopped at an empty stored
/// block's length field.
///
/// Declared at `zlib.h` L2034; ported from `inflate.c` L1320-L1326. The reference's
/// own explanation (L1312-L1319): this is true at the end of a block generated by
/// `Z_SYNC_FLUSH` or `Z_FULL_FLUSH`, and one PPP implementation uses it as a safety
/// check — PPP flushes with `Z_SYNC_FLUSH` but strips the length bytes of the
/// resulting empty stored block, so on decompression it verifies that at the end of
/// an input packet `inflate` is waiting for exactly those bytes.
///
/// # Returns
///
/// 1 or 0, or `Z_STREAM_ERROR` for a failed state check. Note that the three answers
/// are distinguishable: `Z_STREAM_ERROR` is -2.
///
/// # Safety
///
/// As [`inflateResetKeep`].
#[no_mangle]
pub unsafe extern "C" fn inflateSyncPoint(strm: z_streamp) -> c_int {
    guard(|| {
        // L1324: the state check.
        // SAFETY: unsafe-site categories 1, 3 and 4. `strm` is the caller's raw
        // `z_streamp`, and nothing is dereferenced before it is checked: `entry`
        // tests non-null and alignment first and answers `None` otherwise, so a
        // null or misaligned stream returns the documented refusal instead of
        // reading memory. What this call site supplies is what no test can: a
        // non-null `strm` addresses a live `z_stream`, whose `state` -- if non-null
        // -- is a state block this library installed and whose `zalloc`/`zfree`
        // pair is either both null or both valid. `entry` then tag- and
        // owner-validates that block before treating it as state. This is the first
        // thing the body does and the returned `Session` is the only handle to the
        // stream and its state for the rest of the call, so no other borrow of
        // either can exist alongside it.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR;
        };
        session.run(|session| c_int::from(core_sync_point(session.state())))
    })
}

/// `inflateUndermine` — asks the decoder to tolerate distances that reach further
/// back than the window holds.
///
/// Declared at `zlib.h` L2036; ported from `inflate.c` L1370-L1382.
///
/// ★ **In the shipped build this function fails, and that is correct.** The
/// permissive behaviour lives behind `INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR`,
/// which is not defined, so C ignores `subvert`, forces `state->sane = 1` and returns
/// `Z_DATA_ERROR` rather than `Z_OK` (L1379-L1381). `test/infcover.c` L440 asserts
/// exactly that. It is also why the `MATCH` state's history check can never be
/// bypassed, which matters: HEAD commit `09a1572` fixed a `whave`-derived bound that
/// had let a malformed stream copy uninitialised window bytes.
///
/// # Safety
///
/// As [`inflateResetKeep`].
#[no_mangle]
pub unsafe extern "C" fn inflateUndermine(strm: z_streamp, subvert: c_int) -> c_int {
    guard_code(|| {
        // L1374: the state check.
        // SAFETY: unsafe-site categories 1, 3 and 4. `strm` is the caller's raw
        // `z_streamp`, and nothing is dereferenced before it is checked: `entry`
        // tests non-null and alignment first and answers `None` otherwise, so a
        // null or misaligned stream returns the documented refusal instead of
        // reading memory. What this call site supplies is what no test can: a
        // non-null `strm` addresses a live `z_stream`, whose `state` -- if non-null
        // -- is a state block this library installed and whose `zalloc`/`zfree`
        // pair is either both null or both valid. `entry` then tag- and
        // owner-validates that block before treating it as state. This is the first
        // thing the body does and the returned `Session` is the only handle to the
        // stream and its state for the rest of the call, so no other borrow of
        // either can exist alongside it.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };
        session.run(|session| core_inflate_undermine(session.state_mut(), subvert != 0))
    })
}

/// `inflateValidate` — turns check-value verification on or off.
///
/// Declared at `zlib.h` L2037; ported from `inflate.c` L1384-L1395. Sets or clears
/// bit 2 of `state->wrap`, the bit both the trailer comparison and the running
/// check-value update consult.
///
/// A **raw** stream is never promoted, however: C guards the set with
/// `if (check && state->wrap)`, because a raw stream has no check value to verify.
/// Asking for validation on one therefore *clears* the bit rather than setting it,
/// and still answers `Z_OK`.
///
/// # Safety
///
/// As [`inflateResetKeep`].
#[no_mangle]
pub unsafe extern "C" fn inflateValidate(strm: z_streamp, check: c_int) -> c_int {
    guard_code(|| {
        // L1388: the state check.
        // SAFETY: unsafe-site categories 1, 3 and 4. `strm` is the caller's raw
        // `z_streamp`, and nothing is dereferenced before it is checked: `entry`
        // tests non-null and alignment first and answers `None` otherwise, so a
        // null or misaligned stream returns the documented refusal instead of
        // reading memory. What this call site supplies is what no test can: a
        // non-null `strm` addresses a live `z_stream`, whose `state` -- if non-null
        // -- is a state block this library installed and whose `zalloc`/`zfree`
        // pair is either both null or both valid. `entry` then tag- and
        // owner-validates that block before treating it as state. This is the first
        // thing the body does and the returned `Session` is the only handle to the
        // stream and its state for the rest of the call, so no other borrow of
        // either can exist alongside it.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };
        session.run(|session| core_inflate_validate(session.state_mut(), check != 0))
    })
}

// ---------------------------------------------------------------------------
// Synchronisation and copying -- `inflate.c` L1264-L1368
// ---------------------------------------------------------------------------

/// `inflateSync` — skips forward to the next possible full-flush point.
///
/// Declared at `zlib.h` L951; ported from `inflate.c` L1264-L1310. This is the
/// recovery path: after a data error it discards input up to and including the next
/// `00 00 ff ff` — the tail of the empty stored block that `Z_SYNC_FLUSH` emits — and
/// prepares the state to resume at the block that follows.
///
/// The search covers the bit accumulator **before** the unread input, because up to
/// four whole bytes of the stream may already have been pulled into it and would
/// otherwise be skipped.
///
/// # Returns
///
/// * `Z_OK` — the pattern was found; `inflate` may be called again.
/// * `Z_BUF_ERROR` — no input at all and fewer than eight bits buffered, so there
///   was nothing to search.
/// * `Z_DATA_ERROR` — the available input ran out before the pattern was found. The
///   partial match is remembered, so calling again with more input continues it.
///   `test/example.c`'s `test_sync` and `test/infcover.c` L435 both assert this.
/// * `Z_STREAM_ERROR` — the state check failed.
///
/// ★ A `Z_DATA_ERROR` return leaves the stream in `SYNC`, and `inflate()` **cannot**
/// resume from that mode: it answers `Z_STREAM_ERROR` until the search completes.
/// `test/infcover.c` L436 asserts precisely that sequence.
///
/// # ★ The output pair is never touched
///
/// `inflate.c` L1264-L1310 reads and writes `avail_in`, `next_in` and `total_in`, and
/// its internal `inflateReset` writes `msg`, `data_type` and -- for a wrapped stream --
/// `adler` (L104-L109). It reads and writes **neither `next_out` nor `avail_out`**. So
/// this function requires nothing of them either: a caller recovering from a data error
/// need not have an output buffer to hand, and one whose previous output buffer has
/// since been freed is not asking for a use-after-free. See
/// [`Session::with_input_only`].
///
/// # Safety
///
/// `strm`, if non-null, must be a live [`z_stream`] initialised by [`inflateInit_`] or
/// [`inflateInit2_`], whose `(next_in, avail_in)` names a readable region. `next_out`
/// and `avail_out` need not be initialised or valid.
#[no_mangle]
pub unsafe extern "C" fn inflateSync(strm: z_streamp) -> c_int {
    guard_code(|| {
        // L1273: the state check.
        // SAFETY: unsafe-site categories 1, 3 and 4. `strm` is the caller's raw
        // `z_streamp`, and nothing is dereferenced before it is checked: `entry`
        // tests non-null and alignment first and answers `None` otherwise, so a
        // null or misaligned stream returns the documented refusal instead of
        // reading memory. What this call site supplies is what no test can: a
        // non-null `strm` addresses a live `z_stream`, whose `state` -- if non-null
        // -- is a state block this library installed and whose `zalloc`/`zfree`
        // pair is either both null or both valid. `entry` then tag- and
        // owner-validates that block before treating it as state. This is the first
        // thing the body does and the returned `Session` is the only handle to the
        // stream and its state for the rest of the call, so no other borrow of
        // either can exist alongside it.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };
        session.run(|session| {
            // L1274-L1309. The `avail_in == 0 && bits < 8` guard, the accumulator
            // drain and the internal reset all belong to the core; this supplies the
            // input and takes the updated cursor and counters back.
            //
            // ★ The **output pair is not touched at all** -- not read, not rebuilt, not
            // written -- because C touches neither `next_out` nor `avail_out` anywhere
            // in L1264-L1310. `msg`, `data_type` and `adler` still reach the caller,
            // because the internal reset publishes them through the stream view and
            // `with_input_only` writes those six members back. See that method for why
            // rebuilding the output would be reading indeterminate members.
            // SAFETY: the stream is the validated one and its input region satisfies
            // `with_input_only`'s contract by this function's own contract.
            unsafe { session.with_input_only(core_inflate_sync) }
        })
    })
}

/// `inflateCopy` — duplicates a stream so that decoding can be branched.
///
/// Declared at `zlib.h` L970; ported from `inflate.c` L1328-L1368. The use C
/// documents is scanning ahead speculatively while keeping the ability to resume from
/// where the scan began.
///
/// # ★ The whole `z_stream` is copied, not just the state
///
/// C's L1352 is `zmemcpy(dest, source, sizeof(z_stream))`, so the destination
/// inherits `next_in`, `avail_in`, `total_in`, `next_out`, `avail_out`, `total_out`,
/// `msg`, `zalloc`, `zfree`, `opaque`, `data_type`, `adler` **and** `reserved` from
/// the source before `dest->state` is replaced. Copying only the state would leave
/// the destination pointing at nothing and is the easiest part of this function to
/// get wrong.
///
/// # Allocation and failure
///
/// Both the destination's state block and its window come from the **source's**
/// hooks, because `dest` has none until the copy completes. Every allocation
/// therefore lands in the same accounting the source uses, which is what lets
/// `test/infcover.c` L438 cap the tracked zone and require `Z_MEM_ERROR`.
///
/// On failure the destination is left **completely untouched** and the source is
/// unchanged, so the harness can carry on using it — which it does. The ordering that
/// achieves this is deliberate: the state is cloned first, then the block is
/// allocated, and only once both have succeeded is anything written to `dest`.
///
/// ★ C has to repair pointers after its bulk copy — L1357-L1361 rebases
/// `lencode`/`distcode` onto the copy's own arena and L1362 rebases `next`. The core
/// needs none of it, because a table source is an *offset* into the arena rather than
/// a pointer into it, and an offset is equally valid in a copy.
///
/// # Returns
///
/// `Z_OK`, `Z_STREAM_ERROR` for a null `dest` or a source that fails the state check
/// (`inflateCopy(Z_NULL, Z_NULL)` is asserted to give this at `test/infcover.c`
/// L395), or `Z_MEM_ERROR`.
///
/// # Safety
///
/// `source`, if non-null, must be a live [`z_stream`] initialised by
/// [`inflateInit2_`]; `dest`, if non-null, must be a caller-allocated [`z_stream`]
/// that is writable, is **not** the same object as `source`, and does not already
/// hold a state — one that did would be leaked, and `test/infcover.c`'s `mem_done`
/// reports leaks.
#[no_mangle]
pub unsafe extern "C" fn inflateCopy(dest: z_streamp, source: z_streamp) -> c_int {
    guard_code(|| {
        // L1336: `dest == Z_NULL`. Checked first so that nothing is allocated for a
        // destination that could never receive it.
        if dest.is_null() || !dest.is_aligned() {
            return fallback::STREAM_ERROR_CODE;
        }
        // ★ Not a C check, and it cannot be one. Two requirements meet here. The two
        // `z_stream`s must be distinct for the state install below to be meaningful --
        // installing into `source` would overwrite the very state being cloned and leak
        // it -- and the byte copy at the end is `copy_nonoverlapping`, which is defined
        // only for disjoint regions. Pointer equality would catch only the exact
        // overlap: two correctly aligned `z_stream`s can still overlap partially, at any
        // multiple of the alignment below the struct size, so the full range comparison
        // is the test. Rejecting an overlapping pair is the only sound answer, and no
        // caller can want it: a stream copied onto itself is a no-op the caller could
        // simply not make. `deflateCopy` performs the identical test.
        if !streams_are_disjoint(dest, source) {
            return fallback::STREAM_ERROR_CODE;
        }

        // L1336: `inflateStateCheck(source)`.
        // SAFETY: unsafe-site categories 1, 3 and 4. `strm` is the caller's raw
        // `z_streamp`, and nothing is dereferenced before it is checked: `entry`
        // tests non-null and alignment first and answers `None` otherwise, so a
        // null or misaligned stream returns the documented refusal instead of
        // reading memory. What this call site supplies is what no test can: a
        // non-null `strm` addresses a live `z_stream`, whose `state` -- if non-null
        // -- is a state block this library installed and whose `zalloc`/`zfree`
        // pair is either both null or both valid. `entry` then tag- and
        // owner-validates that block before treating it as state. This is the first
        // thing the body does and the returned `Session` is the only handle to the
        // stream and its state for the rest of the call, so no other borrow of
        // either can exist alongside it.
        let session = unsafe { entry(source) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };

        session.run(|session| {
            let allocator = session.allocator;

            // L1349's `copy->strm = dest` is the prefix's `strm`, and the tag is the
            // source's current mode, because C's bulk copy carries `mode` across.
            let tag = session.state().mode_tag();

            // ★ L1341-L1343: the state copy is allocated **first** and the window
            // second, and that order is load-bearing rather than incidental.
            // `test/infcover.c`'s zone increments `notlifo` for any free that is not of
            // its most recent allocation (its L120), and `inflateEnd` releases the
            // window before the block; so the block has to be the older of the two.
            // Allocating the window first produces a "frees not LIFO" report --
            // observed, not hypothesised.
            //
            // The block is *reserved* rather than installed: nothing is written to
            // `dest` until both allocations have succeeded, which is what C does. Its
            // `zmemcpy` of the stream is at L1352, after the two allocations at L1341
            // and L1345, and `dest->state` is assigned last of all at L1365 -- so a
            // caller whose copy fails for want of memory finds `dest` exactly as it
            // left it, not half-written and then cleared.
            let slot = match reserve_state::<InflateSlot>(&allocator) {
                Ok(slot) => slot,
                // L1342-L1343: `if (copy == Z_NULL) return Z_MEM_ERROR;`, having
                // touched neither stream.
                Err(error) => return error,
            };

            // L1344-L1351 and L1353-L1364: the window, then every scalar. Only `whave`
            // bytes of the window are copied (L1363), never all `wsize` of them, because
            // the rest has never been written and copying it would propagate whatever
            // the allocator left there -- `0xa5` under the coverage harness's allocator.
            let cloned = match core_inflate_copy(session.state(), allocator) {
                Ok(cloned) => cloned,
                // L1346-L1349: `ZFREE(source, copy); return Z_MEM_ERROR;`. The
                // reservation goes back -- the most recent allocation, hence LIFO -- and
                // `dest` has not been touched at all.
                Err(error) => {
                    // SAFETY: unsafe-site categories 3 and 4 -- returning the block this
                    // function reserved moments ago to the allocator that produced it. It
                    // was never committed, so it owes no destructor, and no stream
                    // references it because none was written.
                    unsafe {
                        discard_reserved_state(&allocator, slot);
                    }
                    return error;
                }
            };

            // Both allocations have succeeded, so `dest` may now be written.
            //
            // L1352: `zmemcpy((voidpf)dest, (voidpf)source, sizeof(z_stream))` -- a
            // **byte** copy, not a typed one, and the distinction is the whole point.
            // `next_out`, `avail_out` and `reserved` are members a caller need never
            // have initialised: `zlib.h` L109 says the library never uses `reserved`,
            // and `test/infcover.c`'s streams reach `inflateCopy` with output members it
            // has not always set. Reading the structure as a `z_stream` value would read
            // every one of them -- and reading an indeterminate `*mut Bytef` is
            // undefined behaviour in Rust, not merely a garbage value. A byte copy
            // carries whatever is there without interpreting it, which is what C does
            // and what makes `dest` a complete copy.
            //
            // SAFETY: unsafe-site category 1 -- `copy_stream`'s contract. Both pointers
            // are non-null and aligned (`dest` by the test at the top of this function,
            // `source` because `entry` validated it), both cover a whole `z_stream`, and
            // the ranges are disjoint by the test at the top. The borrow this `Session`
            // holds is over the state block, a different allocation, so neither pointer
            // aliases it.
            unsafe {
                copy_stream(dest, source);
            }

            // L1353's `copy->strm = dest` is the prefix's owner, and the tag is the
            // source's mode, which C's bulk copy of the state carries across.
            //
            // SAFETY: unsafe-site categories 3 and 4 -- initialising the reserved block.
            // `slot` came from `reserve_state` on `allocator`, the same triple the window
            // above was taken from, so the copy can only be released through the matching
            // `zfree`; it has not been committed before; and `cloned` is moved in.
            unsafe {
                commit_state(
                    slot,
                    &allocator,
                    dest,
                    StateKind::Inflate,
                    tag,
                    InflateSlot {
                        state: cloned,
                        // C copies `head` verbatim, so both streams write into the
                        // caller's one `gz_header` -- the reference behaviour, not an
                        // oversight, and the core reproduces it by copying the sink.
                        head: session.block.state().head,
                        msg: session.block.state().msg,
                    },
                );
            }

            // L1365: `dest->state = (struct internal_state FAR *)copy;`, replacing the
            // source's pointer that the byte copy above put there. Last of all, exactly
            // as in C.
            //
            // SAFETY: unsafe-site categories 1 and 3 -- one member of `dest` written
            // through a raw place. `dest` is non-null, aligned and writable as
            // established above, and `slot` is the block just committed.
            unsafe {
                publish_state(dest, slot);
            }

            ReturnCode::OK
        })
    })
}

// ---------------------------------------------------------------------------
// The gzip header -- `inflate.c` L1219-L1231, unsafe-site category 6
// ---------------------------------------------------------------------------

/// Rebuilds one optional `gz_header` buffer as a shared slice of write-only cells.
///
/// # ★ Why `&[Cell<MaybeUninit<u8>>]` and not `&mut [u8]`
///
/// Two independent reasons, and each one alone would be sufficient.
///
/// **The cell.** `test/infcover.c` L305-L310 points **all three** of `extra`, `name`
/// and `comment` at the same `out` buffer with the same length. Holding them as three
/// `&mut [u8]` would require three aliasing mutable borrows of one allocation, which is
/// undefined behaviour and would be reported by both Miri and AddressSanitizer — for a
/// test that must pass unmodified. A shared reference to a slice of [`Cell`] resolves it
/// exactly: `Cell<T>` is `repr(transparent)` over `T` so the representation is
/// unchanged, overlapping *shared* references are perfectly legal, and interior
/// mutability makes writing through them safe.
///
/// **The `MaybeUninit`.** `zlib.h` L123-L129 asks the caller for *space*: `extra_max`,
/// `name_max` and `comm_max` are how much room there is, and nothing anywhere promises
/// the bytes hold values. `test/infcover.c` allocates its `out` buffer with `malloc` and
/// its instrumented allocator deliberately fills blocks with `0xa5` rather than zero, so
/// a caller reaching this function with genuinely uninitialised memory is the expected
/// case rather than a hypothetical. A `&[Cell<u8>]` over it would assert that every byte
/// is a valid `u8`, an assertion the bytes have not earned, and initialising them here
/// instead would write over up to three buffers of caller data the header may never
/// supply a single byte for. `MaybeUninit<u8>` states the truth and costs nothing:
/// it has the same size and alignment as `u8`, and `Cell::set` through it emits the same
/// store.
///
/// Nothing in the library ever reads one of these bytes back — `inflate.c`'s `EXTRA`,
/// `NAME` and `COMMENT` states only store (L607-L613, L624-L627, L659-L662) — so the
/// change is invisible to the C caller, which reads its own buffer with its own
/// knowledge of how many bytes arrived. `zlib_rs::inflate::state::HeaderField` documents
/// the same reasoning from the core's side.
///
/// The slice length **is** the caller's advertised capacity, which is what makes the
/// core's clamped writes unable to overrun: `extra_max`, `name_max` and `comm_max`
/// travel with their pointers instead of alongside them.
///
/// # Safety
///
/// If `buffer` is non-null it must be writable for `capacity` bytes for as long as the
/// header stays installed, and nothing outside this library may access it concurrently.
/// Overlap with the other two buffers is explicitly permitted.
unsafe fn header_field(
    buffer: *mut Bytef,
    capacity: uInt,
) -> Option<&'static [Cell<MaybeUninit<u8>>]> {
    if buffer.is_null() {
        // C's `Z_NULL` for the field. The core records the absence and the nine gzip
        // header states then skip the corresponding writes.
        return None;
    }
    let len = widen(capacity);
    if len == 0 {
        // A non-null pointer with a zero capacity is a real case -- a caller may
        // advertise a buffer it has no room in. It must stay `Some`, because C tests
        // the *pointer* against `Z_NULL` and the *length* separately, and an empty
        // slice reproduces both answers. `from_raw_parts` is not called for it, since
        // a zero length would make the pointer's validity irrelevant but not the
        // call legal.
        return Some(&[]);
    }
    // SAFETY: unsafe-site category 6 -- reconstructing one of a caller's `gz_header`
    // buffers. `buffer` is non-null by the test above and, by this function's
    // contract, writable for `capacity` bytes for as long as the header is installed.
    // `Cell<MaybeUninit<u8>>` is `repr(transparent)` over `MaybeUninit<u8>`, which has
    // `u8`'s size and alignment, so the cast changes no layout and the alignment
    // requirement is one byte. No element has to hold a value, which is what makes the
    // borrow legal over memory the caller only promised was writable. A *shared* slice
    // of `Cell` is what makes it sound when two of the three ranges coincide, which is
    // exactly what `test/infcover.c` L305-L310 does.
    Some(unsafe { core::slice::from_raw_parts(buffer.cast::<Cell<MaybeUninit<u8>>>(), len) })
}

/// `inflateGetHeader` — requests that a gzip header be reported into a caller's
/// structure.
///
/// Declared at `zlib.h` L1070; ported from `inflate.c` L1219-L1231. The stream must
/// have been initialised to accept a gzip header — `windowBits + 16` or
/// `windowBits + 32` — or the request is `Z_STREAM_ERROR`; that is C's
/// `(state->wrap & 2) == 0` test, and it is why `test/infcover.c` installs a header
/// only for `win == 47`.
///
/// # What the caller must keep alive
///
/// The `gz_header` **and** whichever of its `extra`, `name` and `comment` buffers are
/// non-null must stay valid and unmoved until the header is complete, the stream is
/// reset, or the stream is ended. The library holds them across calls, exactly as C
/// holds `state->head`, so freeing them early is a use-after-free in either
/// implementation.
///
/// # ★ How the fields are filled, and why nothing can overrun
///
/// The three variable-length fields are filled *during* [`inflate`], not here, and
/// every write is clamped:
///
/// * `extra` is written only when the stream advertises one, the caller's pointer is
///   non-null, and `extra_len - length < extra_max`; the copy is then clamped to
///   `extra_max - len` (`inflate.c` L614-L621);
/// * `name` accepts a byte only while `length < name_max` (L639-L642);
/// * `comment` only while `length < comm_max` (L661-L664).
///
/// Those three bounds are the historical source of gzip-header overflow defects. Here
/// they are structural rather than remembered: the capacity travels *inside* the
/// slice handed to the core, so a write past it is a bounds-checked failure rather
/// than a possibility.
///
/// # ★ Only `done` is written, and nothing at all is read
///
/// C's L1229 assigns `head->done = 0` and touches no other member -- it neither reads
/// nor writes one. That matters because `text`, `time`, `xflags`, `os`, `extra_len`,
/// `hcrc` and `done` are members the *implementation* fills when it reads a header
/// (`zlib.h` L118-L133), so a caller need never have initialised them:
/// `test/infcover.c` L290-L309 declares `gz_header head;` on the stack and assigns
/// only the three buffer pointers and their three capacities. Reading any of the other
/// seven would be reading indeterminate bytes.
///
/// So this function reads exactly the six members C's later header states read -- the
/// three pointers and the three capacities -- and writes exactly the one member C
/// writes. [`GzHeaderSink`] starts the six output scalars at [`None`], and
/// [`Session::publish_header`] writes each one only once the parse has assigned it, so
/// a member the stream never supplied keeps whatever the caller left in it. A caller
/// that pre-filled `text` or `os` therefore sees its own values preserved exactly as
/// under C, and a caller that left them uninitialised has nothing read out of them.
///
/// # ★ The `wrap & 2` guard precedes every read of `head`
///
/// `inflate.c` L1225-L1226 returns `Z_STREAM_ERROR` for a stream that was not
/// configured to decode a gzip header, and only L1228-L1229 then touch the structure.
/// A zlib or raw stream therefore never causes C to read one member of it, so such a
/// caller may legitimately pass a pointer that is stale, misaligned or wild and still
/// be told `Z_STREAM_ERROR`. This function asks the state first, for exactly that
/// reason; the core applies the identical predicate, so the two cannot disagree.
///
/// # Divergence, deliberate and documented
///
/// A **null `head`** is refused with `Z_STREAM_ERROR`. C would dereference it at
/// L1229 and crash; there is no return value to reproduce, so refusing is the only
/// behaviour available to a memory-safe implementation. No caller can want the crash
/// and no test performs it.
///
/// # Safety
///
/// `strm`, if non-null, must be a live [`z_stream`] initialised by
/// [`inflateInit2_`]. `head`, if non-null, must be a caller-allocated
/// [`gz_header`](crate::gz_header)
/// that stays live as described above, whose `extra`, `name` and `comment` members
/// are each either null or valid for reads and writes of `extra_max`, `name_max` and
/// `comm_max` bytes respectively.
#[no_mangle]
pub unsafe extern "C" fn inflateGetHeader(strm: z_streamp, head: gz_headerp) -> c_int {
    guard_code(|| {
        // L1223: the state check.
        // SAFETY: unsafe-site categories 1, 3 and 4. `strm` is the caller's raw
        // `z_streamp`, and nothing is dereferenced before it is checked: `entry`
        // tests non-null and alignment first and answers `None` otherwise, so a
        // null or misaligned stream returns the documented refusal instead of
        // reading memory. What this call site supplies is what no test can: a
        // non-null `strm` addresses a live `z_stream`, whose `state` -- if non-null
        // -- is a state block this library installed and whose `zalloc`/`zfree`
        // pair is either both null or both valid. `entry` then tag- and
        // owner-validates that block before treating it as state. This is the first
        // thing the body does and the returned `Session` is the only handle to the
        // stream and its state for the rest of the call, so no other borrow of
        // either can exist alongside it.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };

        // L1225: `if ((state->wrap & 2) == 0) return Z_STREAM_ERROR;`.
        //
        // ★ Applied before `head` is touched at all -- see the note above. The core
        // repeats the test inside `core_get_header`, so this is not the only line
        // defending it; what this placement buys is that a rejected stream causes no
        // read of the caller's structure, exactly as C's early return does.
        if !session.state().accepts_gzip_header() {
            session.finish();
            return fallback::STREAM_ERROR_CODE;
        }

        // See "Divergence" above. Checked after both C guards so that an invalid
        // stream still reports `Z_STREAM_ERROR` for the same reason C reports it.
        if head.is_null() || !head.is_aligned() {
            session.finish();
            return fallback::STREAM_ERROR_CODE;
        }

        // ★ Not one member of the caller's `gz_header` is read here, because C reads
        // none: `inflateGetHeader` stores the pointer and writes `head->done = 0`
        // (`inflate.c` L1228-L1229). The three buffers and their three capacities are
        // read later, at the start of each `inflate` call, by
        // `Session::bind_header_fields` -- which is where C reads them too, inside the
        // `EXTRA`, `NAME` and `COMMENT` states. Reading them here instead would both
        // freeze values a caller may still change and hold a borrow of the caller's
        // buffers between calls.
        let sink = GzHeaderSink::new(None, None, None);

        session.run(|session| {
            // L1225-L1230: the `wrap & 2` guard, `state->head = head` and
            // `head->done = 0`. The core owns the first two.
            let outcome = core_get_header(session.state_mut(), sink);
            if outcome != ReturnCode::OK {
                return outcome;
            }
            session.block.state_mut().head = head;
            // L1229: `head->done = 0`, and nothing else. `publish_header` will write
            // the rest once a parse has had a chance to fill it.
            // SAFETY: unsafe-site category 6 -- one scalar member of the caller's
            // `gz_header`, non-null and aligned by the test above and live by this
            // function's contract, written through a raw place.
            unsafe {
                core::ptr::addr_of_mut!((*head).done).write(GZ_HEADER_PENDING);
            }
            outcome
        })
    })
}

// ---------------------------------------------------------------------------
// ★ The internal symbol the test suite forces this library to export
// ---------------------------------------------------------------------------

/// `_zlib_rs_inflate_table` — builds a Huffman decode table from a set of code lengths.
///
/// The Rust half of `inflate_table`, declared at `inftrees.h` L60-L62 and ported from
/// `inftrees.c` L46-L311, whose algorithm is implemented by
/// [`zlib_rs::inflate::inftrees::inflate_table`] and is **not** reimplemented here.
/// This function is purely the pointer-to-slice adaptation.
///
/// # ★ Why the exported name is not `inflate_table`
///
/// The contract's first parameter is `codetype`, a C enum (`inftrees.h` L53-L58), and
/// that cannot be presented from Rust. Spelling it `c_int` is ABI-identical on every
/// target in scope but is a *different declaration*: a translation unit that
/// redeclares it alongside `inftrees.h` fails with `conflicting types`. Spelling it as
/// a `#[repr(C)]` Rust enum would render correctly and then make a C caller's
/// arbitrary `int` — nothing in the language stops a 7 — into instant undefined
/// behaviour on construction.
///
/// So the enum stops at C, where it is legal: `csrc/inftrees_shim.c` defines
/// `inflate_table` with `inftrees.h`'s own prototype and `ZLIB_INTERNAL` visibility,
/// and forwards to this function with the value widened to a plain `int`, which is
/// validated below. `crates/libz-rs-sys/build.rs` compiles that shim into the archive
/// cargo produces, so `libz.a` defines the contract name. The leading underscore here
/// is what keeps *this* name out of a shared object's dynamic table: `zlib.map`'s
/// `local:` block ends with the pattern `_*`, the same mechanism zlib uses for
/// `_tr_init` and its five siblings.
///
/// # ★ Why an internal symbol exists at all
///
/// `zlib.map`'s `ZLIB_1.2.0` `local:` block names `inflate_table`, so it is absent
/// from the reference shared library's dynamic table — and yet the unmodified
/// `test/infcover.c` **will not link without it**. Its `cover_trees` (L617-L639)
/// calls the C symbol directly, twice, to provoke the `ENOUGH`-exceeded return that a
/// well-behaved `inflate()` can never produce:
///
/// ```c
/// unsigned short lens[16], work[16];
/// code *next, table[ENOUGH_DISTS];
/// for (bits = 0; bits < 15; bits++) lens[bits] = (unsigned short)(bits + 1);
/// lens[15] = 15;
/// next = table; bits = 15;
/// ret = inflate_table(DISTS, lens, 16, &next, &bits, work);  assert(ret == 1);
/// next = table; bits = 1;
/// ret = inflate_table(DISTS, lens, 16, &next, &bits, work);  assert(ret == 1);
/// ```
///
/// The two requirements are compatible because they concern two different artifacts.
/// `--version-script=zlib.map` applies to the *shared object*, so the name is hidden
/// there; the `staticlib` archive is not linked, so the name stays a global `T` in
/// `libz.a`, which is what `infcover` links (`test/CMakeLists.txt` L95-L96 links it
/// to `ZLIB::ZLIBSTATIC`). That is exactly the pair of properties the C build has, and
/// the 111-symbol parity diff checks both.
///
/// # The parameter adaptation
///
/// | C | Here |
/// |---|---|
/// | `codetype type` | a [`c_int`] the shim widened from the enum, validated by [`code_type_from_raw`] — never a Rust `enum`, because a C caller may pass any `int` |
/// | `unsigned short *lens` | a `&[u16]` of exactly `codes` elements |
/// | `unsigned codes` | the length of both `lens` and `work` |
/// | `code **table` | in/out: `*table` is the base, advanced past the entries the call consumed |
/// | `unsigned *bits` | in/out: the requested root width in, the actual width out |
/// | `unsigned short *work` | a `&mut [u16]` of exactly `codes` elements |
///
/// ★ **The arena length is decided by `type`, not by the caller.** C's own guarantee
/// is the `ENOUGH_*` bound (`inftrees.h` L38-L51): a `CODES` or `LENS` build consumes
/// at most 852 entries from the base it was given and a `DISTS` build at most 592,
/// because exceeding that is precisely what the `+1` return reports. Those are
/// therefore the only lengths that are sound to claim, and they are what every real
/// caller provides — `inflate.c` resets `next` to the base of a `codes[ENOUGH]` arena
/// before the `LENS` build, so 1444 entries are available for a bound of 852, and 592
/// remain for the `DISTS` build that follows; `infcover.c` passes a
/// `code table[ENOUGH_DISTS]` for its `DISTS` calls.
///
/// ★ **[`Code`] is deliberately not `#[repr(C)]`**, so the caller's `code *` must not
/// be reinterpreted as a `*mut Code`. The table is built into a local buffer and the
/// entries the call produced are copied out field by field. That copy is complete: a
/// table-link `val` is an offset relative to the base of the table holding the link,
/// so it survives being moved as plain data.
///
/// # Returns
///
/// The C tri-state, unchanged and **not** a [`ReturnCode`]: `0` on success, `-1` for
/// an over-subscribed or incomplete code set, and `+1` when the table would need more
/// than `ENOUGH_LENS` or `ENOUGH_DISTS` entries. On a non-zero return neither `*table`
/// nor `*bits` is advanced, which is what C's early `return` before L308-L309 does.
///
/// A null pointer in any of the four pointer parameters, or a `type` outside
/// `0..=2`, answers `-1`. C would dereference and crash, so there is no value to
/// reproduce; `-1` is chosen because it already means "this code set cannot be
/// built", which is the outcome either way.
///
/// # Safety
///
/// `lens` and `work` must each be non-null and valid for `codes` `unsigned short`s —
/// `lens` for reads, `work` for reads and writes. `bits` must be a readable and
/// writable `unsigned`. `table` must be a readable and writable `code *`, and the
/// `code *` it holds must be valid for writes of `ENOUGH_LENS` entries for a `CODES`
/// or `LENS` build and `ENOUGH_DISTS` entries for a `DISTS` build. None of the
/// regions may overlap.
#[no_mangle]
pub unsafe extern "C" fn _zlib_rs_inflate_table(
    type_: c_int,
    lens: *mut c_ushort,
    codes: c_uint,
    table: *mut *mut code,
    bits: *mut c_uint,
    work: *mut c_ushort,
) -> c_int {
    guard(|| {
        // How many `work` elements the sort below stages in this function's own stack frame.
        //
        // Every caller that exists asks for fewer: `inflate.c` L978, L1000 and L1005 pass
        // `19`, `state->nlen` (at most 288) and `state->ndist` (at most 32) out of a
        // `state->lens[320]` array (`inflate.h` L118), and `test/infcover.c` L554 passes 16.
        // 320 is that ceiling, so the ordinary path allocates nothing at all; a `codes`
        // beyond it takes a fallible heap block instead, which keeps the stack frame bounded
        // rather than making the bound a limit on what the function accepts. 640 bytes, next
        // to the 3408 the table arena already occupies.
        //
        // Declared inside the body, and at the head of it, for two independent reasons: at
        // module scope cbindgen reports a private `const` as
        // ``Skip libz-rs-sys::MAX_STACK_WORK - (not `pub`)``, which the `make rust-header`
        // gate counts as an item it could not compare; and below the first statement
        // `clippy::items_after_statements` fires.
        const MAX_STACK_WORK: usize = 320;

        // A C caller may pass any `int` through the `codetype` parameter; nothing in
        // the language stops it, and `test/infcover.c` passes the enumerators only by
        // convention. Validating rather than transmuting is what makes taking a
        // `c_int` here sound.
        let Some(code_type) = code_type_from_raw(type_) else {
            return TABLE_INVALID_CODE;
        };
        if lens.is_null() || table.is_null() || bits.is_null() || work.is_null() {
            return TABLE_INVALID_CODE;
        }
        let Ok(count) = usize::try_from(codes) else {
            return TABLE_INVALID_CODE;
        };

        // The arena bound the code type licenses. See the note above on why this is
        // not the caller's business to state and not this function's to guess.
        let capacity = match code_type {
            CodeType::Codes | CodeType::Lens => ENOUGH_LENS,
            CodeType::Dists => ENOUGH_DISTS,
        };

        // ★ **`work` is the caller's memory, and the reference does not always write
        // it.** `inftrees.c` L154-L156 uses it as the sorted-symbol array, but that loop
        // runs only after the over-subscribed and incomplete checks have passed: all
        // four failure returns (L143, L146, L218, L287) and the no-symbols return at
        // L134 leave the array exactly as it arrived. So this wrapper may not touch it
        // either until it knows what the build did -- and it cannot borrow it as
        // `&mut [u16]` first, because a C caller hands over a buffer that is writable
        // but need not hold anything (`test/infcover.c` L554 passes an uninitialised
        // automatic array) and `&mut [u16]` is not a legal shape for indeterminate
        // memory.
        //
        // The resolution is to sort into scratch storage of this function's own and
        // then publish exactly `TableBuild::work_touched` elements of it. `MAX_STACK_WORK`
        // covers every caller that exists -- `inflate.c` never passes more than 320 and
        // `test/infcover.c` passes 16 -- and a larger request falls back to a fallible
        // heap block from the library's own allocator, which is the same routine
        // `zcalloc` is (`zutil.c` L215) and is not a caller-visible allocation.
        let mut stack_work = [0_u16; MAX_STACK_WORK];
        let mut heap_work = None;
        let work_scratch: &mut [u16] = if count <= MAX_STACK_WORK {
            match stack_work.get_mut(..count) {
                Some(scratch) => scratch,
                // Unreachable: the branch condition is exactly this bound. Written as a
                // fallible lookup because indexing is denied crate-wide.
                None => return TABLE_INVALID_CODE,
            }
        } else {
            let allocator = StreamAllocator::internal();
            let Some(mut block) = allocator.allocate_u16s(count) else {
                // A `codes` beyond `MAX_STACK_WORK` is already outside every real
                // caller, and out of memory on top of that leaves nothing to build
                // with. `-1` already means "this code set cannot be built", which is
                // the outcome either way.
                return TABLE_INVALID_CODE;
            };
            block.fill(0);
            heap_work = Some(block);
            match heap_work.as_mut().map(Buffer::as_mut_slice) {
                Some(scratch) => scratch,
                // Unreachable: assigned `Some` one line above.
                None => return TABLE_INVALID_CODE,
            }
        };

        // SAFETY: unsafe-site category 2 -- slice reconstruction over the caller's
        // `lens`, plus two scalar reads, each performed once. `lens` is non-null by the
        // test above and, by this function's contract, valid for `count` `u16`s;
        // `c_ushort` is `u16`, so the cast changes no layout and the alignment
        // requirement is unchanged. `lens` is an *input* the caller has filled, so
        // `u16` is a legal element type for the borrow, and it is shared, so it cannot
        // conflict with anything. `bits` and `table` are non-null and readable, and
        // both hold plain `Copy` data that is not dereferenced here.
        let (lens_slice, requested_bits, base) = unsafe {
            (
                core::slice::from_raw_parts(lens.cast::<u16>(), count),
                bits.read(),
                table.read(),
            )
        };
        if base.is_null() {
            return TABLE_INVALID_CODE;
        }
        let Ok(mut root) = usize::try_from(requested_bits) else {
            return TABLE_INVALID_CODE;
        };

        // The build target. A local buffer rather than a view over the caller's array,
        // because `Code` carries no `#[repr(C)]` and reinterpreting `code *` as
        // `*mut Code` would rest on a layout the core explicitly declines to promise.
        // `ENOUGH_LENS` entries is 3408 bytes, which is a comfortable stack frame for
        // a function no hot path reaches -- the core's own decoder calls the core's
        // `inflate_table` directly, never this wrapper.
        let mut arena = [Code::ZERO; ENOUGH_LENS];
        let Some(claimed) = arena.get_mut(..capacity) else {
            // Unreachable: `capacity` is 852 or 592 and `arena` is 852 long. Written
            // as a fallible lookup rather than a slice expression because indexing is
            // denied crate-wide, and answering "not enough" is the honest outcome if
            // it ever became reachable.
            return TABLE_NOT_ENOUGH;
        };

        // A cursor of zero, because `claimed` already starts at the caller's `*table`
        // rather than at the base of some larger arena. On return the cursor is
        // exactly the number of entries the call consumed.
        let mut cursor = 0_usize;
        let build = core_inflate_table_build(
            code_type,
            lens_slice,
            count,
            claimed,
            &mut cursor,
            &mut root,
            work_scratch,
        );
        let outcome = build.code;

        // ★ **A failed build still publishes what it wrote.** C builds straight into
        // the caller's array, so the two `return 1` sites at `inftrees.c` L218 and L287
        // leave every entry written before that point in the caller's memory -- while
        // `*table` and `*bits` stay exactly as they were passed in, because L308-L309 is
        // never reached. A caller can observe both halves of that, so both are
        // reproduced: the entries are copied out below, and the two in/out parameters
        // are written with the values the core left untouched, which is identical to
        // C's not writing them.
        //
        // The extent is `TableBuild::touched`, one past the highest entry the build
        // wrote. Measured against the reference for every failing input -- including
        // the two `test/infcover.c` L632 and L636 pass in, which write 0 and 1 entries
        // respectively -- the writes form a prefix with no gaps: the tables allocated
        // before the failing one are complete and the failing one is not written at all.
        //
        // On success the cursor is used instead, which is the region C fills and the
        // behaviour every passing test already pins; `touched` cannot exceed it, since
        // the cursor counts the entries the build claimed.
        let published = cursor.max(build.touched).min(capacity);
        let produced = arena.get(..published).unwrap_or(&[]);
        for (offset, entry) in produced.iter().enumerate() {
            // SAFETY: unsafe-site category 2 -- writing one entry of the caller's
            // table. `offset` is below `cursor`, which the core bounded by `capacity`,
            // and `base` is valid for `capacity` entries by this function's contract,
            // so the offset is in bounds. `base` is non-null by the test above and
            // aligned for `code` because the caller declared an array of them.
            // `code` is `#[repr(C)]` and `Copy` with no `Drop`, so the write is a
            // plain four-byte store that drops nothing, and `From<Code>` copies the
            // three fields rather than reinterpreting them.
            unsafe {
                base.add(offset).write(code::from(*entry));
            }
        }

        // ★ **The scratch array is published to exactly the extent the reference
        // writes it, and only then.** `work_touched` is zero for every return that
        // precedes `inftrees.c`'s sort loop, so those paths copy nothing back and the
        // caller's array is left byte for byte as it arrived -- which is what C does.
        // On the paths that do sort, the written positions are a gapless prefix (the
        // `offs` array is a prefix sum), so one count describes them.
        let sorted = build.work_touched.min(count);
        if let Some(published) = work_scratch.get(..sorted) {
            for (offset, &symbol) in published.iter().enumerate() {
                // SAFETY: unsafe-site category 2 -- writing one element of the caller's
                // scratch array. `offset` is below `sorted`, which is bounded by `count`,
                // and `work` is valid for writes of `count` `u16`s by this function's
                // contract; `c_ushort` is `u16`, so the cast changes no layout, and the
                // pointer is aligned for it because the caller declared an array of
                // them. No reference to that region exists: none was ever taken.
                unsafe {
                    work.cast::<u16>().add(offset).write(symbol);
                }
            }
        }

        // ★ **`*bits` and `*table` are written only on success.** `inftrees.c` reaches
        // `*table += used; *bits = root;` (L308-L309) on exactly one path, and the
        // no-symbols return at L126-L134 performs the equivalent two writes itself;
        // every failure return -- L143, L146, L218, L287 -- leaves both parameters
        // holding the values the caller passed in. That is observable, and the values
        // are not interchangeable with "unchanged": the core clamps `root` to the code
        // set's `max` and `min` (L123, L137) *before* the over-subscribed test at L139,
        // so writing it back on a `-1` return would publish a width the reference never
        // published. `narrow_uInt` would additionally truncate a `*bits` above `uInt`'s
        // range. So the guard is on the status, not on whether the value looks changed.
        if outcome == TABLE_OK {
            // SAFETY: unsafe-site category 2 -- the two in/out parameters. `bits` and
            // `table` are non-null by the test above and writable by this function's
            // contract. `cursor` is bounded by `capacity`, so advancing `base` by it
            // stays within the caller's array or one past its end -- which is what C's
            // `*table += used` produces for a table that exactly fills it.
            unsafe {
                bits.write(narrow_uInt(root));
                table.write(base.add(cursor));
            }
        }

        // The heap fallback, when there was one, goes back to the allocator that
        // produced it before this frame ends -- strictly last-in, first-out, and while
        // no borrow of it is live: `work_scratch` was last used above, and the block is
        // released through the slot so that no reference to it is in argument position.
        // `Buffer::release_to` is what checks the allocator identity.
        let allocator = StreamAllocator::internal();
        allocator.deallocate_u16s(&mut heap_work);

        outcome
    })
}

#[cfg(test)]
// The workspace denies the panic family in library code, which is the property this
// module exists to uphold at the boundary. A test that cannot assert is useless, so
// the harness opts back in here only; `clippy.toml` grants exactly this with its
// `allow-unwrap-in-tests`, `allow-expect-in-tests` and `allow-panic-in-tests` keys.
// There is deliberately no `allow-indexing-slicing-in-tests` key -- it is newer than
// the declared 1.80 floor and an unrecognised `clippy.toml` field aborts the whole
// lint run -- so the slicing allowance is taken as a module-local attribute instead.
// `used_underscore_items` joins them because these tests call
// `_zlib_rs_inflate_table`, whose leading underscore is what `zlib.map`'s `local: _*;`
// pattern hides it by and `zlib.map` is immutable. Its real caller is
// `csrc/inftrees_shim.c`, which presents the `codetype` prototype no Rust caller can
// spell; these tests are the only Rust callers it will ever have. `unknown_lints`
// comes first because the lint postdates the declared 1.80 floor, where naming it
// would otherwise be a warning of its own -- the same guard
// `crates/zlib-rs/src/deflate/algorithm.rs` uses for `_tr_init`.
#[allow(
    unknown_lints,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used,
    clippy::used_underscore_items
)]
mod tests {
    use super::{
        _zlib_rs_inflate_table, code, inflate, inflateCodesUsed, inflateCopy, inflateEnd,
        inflateGetDictionary, inflateGetHeader, inflateInit2_, inflateInit_, inflateMark,
        inflatePrime, inflateReset, inflateReset2, inflateResetKeep, inflateSetDictionary,
        inflateSync, inflateSyncPoint, inflateUndermine, inflateValidate, message_ptr,
        narrow_uLong, widen_uLong, z_stream, BAD_STATE_MARK, ENOUGH_DISTS, ENOUGH_LENS,
        MAX_INFLATE_FLUSH, MAX_WINDOW_BYTES, MESSAGES, TABLE_INVALID_CODE, TABLE_OK,
    };
    use core::ffi::{c_char, c_int, c_uint, c_ulong, c_ushort, CStr};
    use core::mem::{offset_of, size_of, MaybeUninit};

    use crate::types::{
        gz_header, gz_headerp, uInt, z_streamp, Bytef, StatePrefix, CODES, DISTS, ENOUGH, LENS,
    };
    use crate::util::ZLIB_VERSION;
    use zlib_rs::error::ReturnCode;
    use zlib_rs::inflate::{Mode, INFLATE_CODES_USED_BAD_STATE};

    /// The mode tag `test/infcover.c` L330 writes: `DICT`, i.e. 16190.
    const DICT_TAG: c_int = 16190;

    /// The mode tag `test/infcover.c` L458 writes: `SYNC`, i.e. 16211.
    const SYNC_TAG: c_int = 16211;

    /// A zeroed `z_stream`, which is what a C caller declares before initialising.
    ///
    /// `next_in` and `avail_in` are left null and zero deliberately: that pairing is
    /// legal and is exactly what `test/infcover.c` L392-L393 sets before every
    /// `inflateInit2`.
    fn blank_stream() -> z_stream {
        z_stream {
            next_in: core::ptr::null(),
            avail_in: 0,
            total_in: 0,
            next_out: core::ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: core::ptr::null(),
            state: core::ptr::null_mut(),
            zalloc: None,
            zfree: None,
            opaque: core::ptr::null_mut(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        }
    }

    /// The `version` and `stream_size` arguments the `inflateInit2` macro supplies.
    fn version_args() -> (*const c_char, c_int) {
        (
            ZLIB_VERSION.as_ptr(),
            c_int::try_from(size_of::<z_stream>()).unwrap(),
        )
    }

    /// Initialises a stream for `window_bits`, asserting success.
    fn init(strm: &mut z_stream, window_bits: c_int) {
        let (version, size) = version_args();
        // SAFETY: `strm` is a live, zeroed `z_stream` and the version pair is this
        // library's own.
        let ret = unsafe { inflateInit2_(strm, window_bits, version, size) };
        assert_eq!(ret, ReturnCode::OK.as_i32(), "inflateInit2_({window_bits})");
    }

    /// Compresses `data` with the core, so the decoder has a real stream to read.
    fn deflate_zlib(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0_u8; zlib_rs::compress::compress_bound(data.len()) + 64];
        let report = zlib_rs::compress::compress(&mut out, data);
        assert_eq!(report.code, ReturnCode::OK);
        out.truncate(report.produced);
        out
    }

    /// Returns a pointer to the C-visible mode tag behind `strm.state`.
    ///
    /// ★ This is the one thing about the state that is *deliberately* reachable from
    /// outside the library, because `test/infcover.c` includes the private
    /// `inflate.h` and writes through it -- `((struct inflate_state *)strm.state)
    /// ->mode = DICT` at its L330 and `state->mode = SYNC` at its L458. The Rust
    /// spelling of that cast is `StatePrefix`, whose `#[repr(C)]` layout places a
    /// `z_streamp` at offset 0 and the `int` tag at offset 8 -- exactly where
    /// `deflate.h` L105-L106 and `inflate.h` L83-L84 place theirs.
    /// An allocator that hands out blocks until a chosen request, then refuses.
    ///
    /// Smaller than `test/infcover.c`'s zone -- it counts rather than tracks, because
    /// what these tests need is a *named* allocation to fail, not a leak report -- but it
    /// keeps the live count so a leak is still visible.
    #[derive(Default)]
    struct Refuse {
        /// How many requests have been made.
        requests: usize,
        /// Refuse this request and every later one, counting from one; zero refuses
        /// nothing.
        deny_from: usize,
        /// The outstanding blocks, as (address, bytes) so that each can be returned to
        /// Rust's allocator with the layout it was taken with.
        blocks: Vec<(*mut u8, usize)>,
    }

    impl Refuse {
        /// How many blocks are outstanding.
        fn live(&self) -> usize {
            self.blocks.len()
        }
    }

    /// A [`Refuse`] zone on the heap, reached only through the one raw pointer it owns.
    ///
    /// ★ **A tracking zone cannot live in a local.** The pointer the test installs in
    /// `z_stream.opaque` is what the allocator hooks dereference, and a later write
    /// through the *local* -- `zone.deny_from = ...` -- is a write through the local's
    /// own tag, which invalidates every pointer derived from it. The next `zalloc` then
    /// dereferences a dead tag, and Miri rejects the test. A C caller has no such rule;
    /// this is a property of the harness, not of the library.
    ///
    /// So the zone is heap-allocated once, the raw pointer is the only handle, and every
    /// access -- the test's and the hooks' -- derives from that single pointer. That is
    /// also how a C caller holds its own zone, which is what makes the harness faithful
    /// rather than merely acceptable.
    struct Zone(*mut Refuse);

    impl Zone {
        /// Allocates a fresh zone that refuses nothing.
        fn new() -> Self {
            Self(Box::into_raw(Box::new(Refuse::default())))
        }

        /// The pointer to install as `z_stream.opaque`.
        fn as_opaque(&self) -> crate::types::voidpf {
            self.0.cast::<core::ffi::c_void>()
        }

        /// Borrows the zone through its one raw pointer, for the duration of `body`.
        fn with<R>(&self, body: impl FnOnce(&mut Refuse) -> R) -> R {
            // SAFETY: the pointer came from `Box::into_raw` in `Zone::new`, is live until
            // `Zone::drop`, and is aligned and unique. No other reference to the zone
            // exists while `body` runs: the library forms one only inside `refuse_alloc`
            // and `refuse_free`, and neither can be executing while this statement is.
            body(unsafe { &mut *self.0 })
        }
    }

    impl Drop for Zone {
        /// Releases the allocation, after asserting the zone leaked nothing.
        fn drop(&mut self) {
            // SAFETY: the pointer came from `Box::into_raw` in `Zone::new` and this is
            // the only place it is reclaimed, so it is reclaimed exactly once.
            let zone = unsafe { Box::from_raw(self.0) };
            assert_eq!(zone.live(), 0, "the zone must own nothing at the end");
        }
    }

    /// The `zalloc` half of [`Refuse`].
    unsafe extern "C" fn refuse_alloc(
        opaque: crate::types::voidpf,
        count: uInt,
        size: uInt,
    ) -> crate::types::voidpf {
        let zone = opaque.cast::<Refuse>();
        assert!(!zone.is_null(), "the zone is always supplied");
        // SAFETY: `opaque` is the `&mut Refuse` the test installed, and no other
        // reference to it exists while this call runs.
        let zone = unsafe { &mut *zone };
        zone.requests += 1;
        if zone.deny_from != 0 && zone.requests >= zone.deny_from {
            return core::ptr::null_mut();
        }
        let len = (count as usize) * (size as usize);
        let layout = core::alloc::Layout::from_size_align(len.max(1), 16).unwrap();
        // SAFETY: the layout is non-zero-sized and validly aligned.
        let ptr = unsafe { std::alloc::alloc(layout) };
        assert!(!ptr.is_null(), "the test allocator must not fail for real");
        // Never zeroed, as C's `malloc` is not.
        // SAFETY: `ptr` is a fresh allocation of `layout.size()` bytes.
        unsafe { core::ptr::write_bytes(ptr, 0xa5, layout.size()) };
        zone.blocks.push((ptr, layout.size()));
        ptr.cast()
    }

    /// The `zfree` half of [`Refuse`].
    ///
    /// Rust's allocator needs the layout a block was taken with, which C's `free` does
    /// not, so the size is looked up in [`Refuse::blocks`] -- and a free of an address the
    /// zone never handed out is a defect worth failing on, exactly as
    /// `test/infcover.c`'s `mem_free` treats one.
    unsafe extern "C" fn refuse_free(opaque: crate::types::voidpf, address: crate::types::voidpf) {
        let zone = opaque.cast::<Refuse>();
        assert!(!zone.is_null(), "the zone is always supplied");
        // SAFETY: as `refuse_alloc`.
        let zone = unsafe { &mut *zone };
        assert!(!address.is_null(), "no null is ever freed");
        let found = zone
            .blocks
            .iter()
            .position(|&(at, _)| core::ptr::eq(at.cast::<core::ffi::c_void>(), address));
        let index = found.expect("every free must be of a block this zone handed out");
        let (at, len) = zone.blocks.remove(index);
        let layout = core::alloc::Layout::from_size_align(len, 16).unwrap();
        // SAFETY: `at` came from `std::alloc::alloc` with this exact layout in
        // `refuse_alloc`, and it has just been removed from the live list, so it cannot be
        // released twice.
        unsafe { std::alloc::dealloc(at, layout) };
    }

    /// A `gz_header` on the heap, reached only through the raw pointer this returns.
    ///
    /// ★ A `gz_header` in a local, passed as `&mut head` and then written through the
    /// local again, is a Stacked Borrows violation *in the test*: the reference passed to
    /// `inflateGetHeader` is what the library's stored raw pointer derives from, and a
    /// later direct write to the local invalidates it, so the next `inflate` reading
    /// through it is undefined behaviour under Miri. A C caller has no such rule -- this
    /// is a property of the test harness, not of the library -- but a test that Miri
    /// rejects is a test that cannot be run, so every access here goes through one raw
    /// pointer with one provenance, which is also how a C caller holds it.
    ///
    /// The caller owns the allocation and must release it with [`release_header`].
    fn header_on_heap(head: gz_header) -> *mut gz_header {
        Box::into_raw(Box::new(head))
    }

    /// Releases a header [`header_on_heap`] produced.
    ///
    /// # Safety
    ///
    /// `head` must have come from [`header_on_heap`] and must not still be installed in a
    /// live stream.
    unsafe fn release_header(head: *mut gz_header) {
        // SAFETY: by this function's contract `head` came from `Box::into_raw` on a
        // `Box<gz_header>` and has not been released before.
        drop(unsafe { Box::from_raw(head) });
    }

    /// A `gz_header` with every member zeroed, as a caller that fills in only what it
    /// needs starts from.
    fn zeroed_header() -> gz_header {
        gz_header {
            text: 0,
            time: 0,
            xflags: 0,
            os: 0,
            extra: core::ptr::null_mut(),
            extra_len: 0,
            extra_max: 0,
            name: core::ptr::null_mut(),
            name_max: 0,
            comment: core::ptr::null_mut(),
            comm_max: 0,
            hcrc: 0,
            done: 0,
        }
    }

    fn mode_slot(strm: &z_stream) -> *mut c_int {
        let prefix = strm.state.cast::<StatePrefix>();
        assert!(!prefix.is_null(), "the stream must hold a state");
        // SAFETY: `strm.state` was installed by `inflateInit2_`, so it addresses a
        // `StateBlock<InflateSlot>` whose first member is a `StatePrefix`. Forming a
        // raw place for one member neither dereferences it nor creates a reference.
        unsafe { core::ptr::addr_of_mut!((*prefix).tag) }
    }

    /// Reads a `z_stream`'s `msg` back as a Rust string, or [`None`] for `Z_NULL`.
    fn msg_of(strm: &z_stream) -> Option<&'static str> {
        if strm.msg.is_null() {
            return None;
        }
        // SAFETY: every non-null `msg` this module publishes points at an entry of
        // `MESSAGES`, which is a `'static` NUL-terminated C string.
        unsafe { CStr::from_ptr(strm.msg) }.to_str().ok()
    }

    // -----------------------------------------------------------------------
    // The message table
    // -----------------------------------------------------------------------

    /// `inflate.c` L183-L195: an init with `Z_NULL` hooks writes the library's own
    /// routines into the caller's stream and clears `opaque`, and a stream in that
    /// state can be torn down through the hooks it now holds.
    ///
    /// This is what `zlib.h` L151-L153 promises -- if `zalloc` and `zfree` are set
    /// to `Z_NULL`, the init updates them to use default allocation functions -- and
    /// it is observable to any caller that reads its own structure back, or that
    /// hands the stream to `inflateCopy`, which copies all three members verbatim.
    #[test]
    fn an_init_with_null_hooks_publishes_the_librarys_own_routines() {
        let mut strm = blank_stream();
        // A non-null `opaque` the substitution must clear, and a message the init
        // must clear (`inflate.c` L182).
        strm.opaque = core::ptr::addr_of_mut!(strm.reserved).cast::<core::ffi::c_void>();
        strm.msg = c"stale".as_ptr();

        init(&mut strm, 15);

        assert!(strm.zalloc.is_some(), "inflate.c L184-L190 fills zalloc in");
        assert!(strm.zfree.is_some(), "inflate.c L191-L195 fills zfree in");
        assert!(
            strm.opaque.is_null(),
            "inflate.c L188 clears opaque with the zalloc substitution"
        );
        assert!(strm.msg.is_null(), "inflate.c L182 clears msg");
        assert!(!strm.state.is_null());

        // The published pair is what the teardown runs through, so a mismatch here
        // would be a rogue free rather than a wrong value.
        // SAFETY: `strm` is the live stream this test initialised; the teardown runs
        // through the hooks that init published.
        let ended = unsafe { inflateEnd(&mut strm) };
        assert_eq!(
            ended,
            ReturnCode::OK.as_i32(),
            "the state must be releasable through the hooks the init published"
        );
        assert!(strm.state.is_null());

        // And the reverse direction: with the members cleared again -- the state
        // `test/infcover.c`'s `mem_done` leaves (L231-L233) -- every non-init entry
        // point refuses the stream, which is `inflateStateCheck`'s first test.
        strm.zalloc = None;
        strm.zfree = None;
        // SAFETY: `strm` is a live, aligned `z_stream` whose state was released just
        // above, so this is exactly the cleared stream `inflateStateCheck` must refuse.
        let reset = unsafe { inflateReset(&mut strm) };
        assert_eq!(reset, ReturnCode::STREAM_ERROR.as_i32());
    }

    #[test]
    fn the_message_table_is_consistent_and_fully_reachable() {
        assert_eq!(MESSAGES.len(), 18, "the core produces exactly 18 texts");
        for (index, entry) in MESSAGES.iter().enumerate() {
            let text = entry.to_str().unwrap();
            assert!(!text.is_empty(), "entry {index} is empty");
            assert_eq!(
                MESSAGES.iter().filter(|other| *other == entry).count(),
                1,
                "entry {index} ({text}) is duplicated"
            );
            let published = message_ptr(Some(text));
            assert!(!published.is_null(), "{text} is not recoverable");
            // SAFETY: `message_ptr` returned a pointer into `MESSAGES`, which holds
            // `'static` NUL-terminated strings.
            let round_tripped = unsafe { CStr::from_ptr(published) };
            assert_eq!(round_tripped, *entry);
        }
    }

    #[test]
    fn message_ptr_answers_null_for_no_message_and_for_an_unknown_one() {
        assert!(message_ptr(None).is_null());
        assert!(message_ptr(Some("not a message this library produces")).is_null());
        assert!(message_ptr(Some("")).is_null());
    }

    #[test]
    fn a_data_error_always_publishes_a_non_null_message() {
        // Six malformed streams, each reaching a different family of error text: a
        // bad zlib method, a bad zlib header check, a reserved block type, mismatched
        // stored-block lengths, a corrupt trailer, and an over-subscribed code set.
        let corpus: [&[u8]; 6] = [
            &[0x77, 0x85],
            &[0x78, 0x90],
            &[0x78, 0x9c, 0x07, 0x00],
            &[0x78, 0x9c, 0x01, 0x01, 0x00, 0x01, 0x00, 0x41],
            &[0x78, 0x9c, 0x63, 0x00, 0x00, 0x00, 0x01, 0x00, 0x02],
            &[0x78, 0x9c, 0xfd, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        ];
        for (index, input) in corpus.iter().enumerate() {
            let mut strm = blank_stream();
            init(&mut strm, 15);
            let mut out = [0_u8; 64];
            strm.next_in = input.as_ptr();
            strm.avail_in = uInt::try_from(input.len()).unwrap();
            strm.next_out = out.as_mut_ptr();
            strm.avail_out = uInt::try_from(out.len()).unwrap();
            // SAFETY: the stream is initialised and both regions are live and
            // disjoint.
            let ret = unsafe { inflate(&mut strm, 0) };
            if ret == ReturnCode::DATA_ERROR.as_i32() {
                assert!(
                    !strm.msg.is_null(),
                    "corpus entry {index} reported Z_DATA_ERROR with a null msg, \
                     which means its text is missing from MESSAGES"
                );
                assert!(msg_of(&strm).is_some());
            }
            // SAFETY: `strm` is a live, stack-owned `z_stream` this test initialised and
            // has not yet ended, so `&mut strm` is non-null, aligned and its only borrow.
            // This is the last call that touches it, so nothing reads the stream after
            // its state has been released.
            assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
        }
    }

    // -----------------------------------------------------------------------
    // Initialisation, version and stream-pointer guards
    // -----------------------------------------------------------------------

    #[test]
    fn a_null_stream_is_refused_by_every_entry_point() {
        let (version, size) = version_args();
        // SAFETY: a null `z_streamp` is a documented input; every guard precedes the
        // first dereference.
        unsafe {
            assert_eq!(
                inflateInit_(core::ptr::null_mut(), version, size),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(
                inflateInit2_(core::ptr::null_mut(), 15, version, size),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            // `test/infcover.c` L393-L395, verbatim.
            assert_eq!(
                inflate(core::ptr::null_mut(), 0),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(
                inflateEnd(core::ptr::null_mut()),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(
                inflateCopy(core::ptr::null_mut(), core::ptr::null_mut()),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            for answer in [
                inflateReset(core::ptr::null_mut()),
                inflateResetKeep(core::ptr::null_mut()),
                inflateReset2(core::ptr::null_mut(), 15),
                inflatePrime(core::ptr::null_mut(), 1, 0),
                inflateSync(core::ptr::null_mut()),
                inflateSyncPoint(core::ptr::null_mut()),
                inflateUndermine(core::ptr::null_mut(), 1),
                inflateValidate(core::ptr::null_mut(), 1),
                inflateGetHeader(core::ptr::null_mut(), core::ptr::null_mut()),
                inflateSetDictionary(core::ptr::null_mut(), core::ptr::null(), 0),
                inflateGetDictionary(
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                ),
            ] {
                assert_eq!(answer, ReturnCode::STREAM_ERROR.as_i32());
            }
            // ★ These two do NOT report `Z_STREAM_ERROR`: their return types are not
            // status codes.
            assert_eq!(inflateMark(core::ptr::null_mut()), BAD_STATE_MARK);
            assert_eq!(
                inflateCodesUsed(core::ptr::null_mut()),
                narrow_uLong(INFLATE_CODES_USED_BAD_STATE)
            );
        }
    }

    #[test]
    fn a_version_or_size_mismatch_is_reported_before_the_null_stream_test() {
        let (version, size) = version_args();
        let mut strm = blank_stream();
        // SAFETY: `strm` is a live zeroed stream; the version arguments are
        // deliberately wrong.
        unsafe {
            // `test/infcover.c` L376-L377 passes "!" and expects Z_VERSION_ERROR.
            assert_eq!(
                inflateInit_(&mut strm, c"!".as_ptr(), size),
                ReturnCode::VERSION_ERROR.as_i32()
            );
            assert_eq!(
                inflateInit_(&mut strm, core::ptr::null(), size),
                ReturnCode::VERSION_ERROR.as_i32()
            );
            assert_eq!(
                inflateInit_(&mut strm, version, size - 1),
                ReturnCode::VERSION_ERROR.as_i32()
            );
            // ★ The version test precedes the `strm == Z_NULL` test, so a null stream
            // with a bad version reports the version error.
            assert_eq!(
                inflateInit_(core::ptr::null_mut(), c"9".as_ptr(), size),
                ReturnCode::VERSION_ERROR.as_i32()
            );
        }
        // A rejected init must leave the stream unusable rather than half-built.
        assert!(strm.state.is_null());
        // A same-major, different-minor version is accepted, because only the first
        // character is compared.
        assert_eq!(ZLIB_VERSION.to_bytes().first(), Some(&b'1'));
        // SAFETY: `strm` is a live, stack-owned `z_stream` this test zeroed and
        // still owns, so `&mut strm` is non-null, aligned and unaliased for the call.
        // The version pointer is a `'static` NUL-terminated literal, readable for its
        // first byte, and `size` is `size_of::<z_stream>()`.
        unsafe {
            assert_eq!(
                inflateInit_(&mut strm, c"1.2.11".as_ptr(), size),
                ReturnCode::OK.as_i32()
            );
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
    }

    #[test]
    fn an_initialised_zlib_stream_reports_the_adler_seed_before_decoding() {
        // `inflateResetKeep` sets `strm->adler = state->wrap & 1` (`inflate.c`
        // L107-L108), so a wrapped stream carries 1 and a raw stream carries 0.
        let mut wrapped = blank_stream();
        wrapped.adler = 0xdead_beef;
        init(&mut wrapped, 15);
        assert_eq!(wrapped.adler, 1);
        assert_eq!(wrapped.total_in, 0);
        assert_eq!(wrapped.total_out, 0);
        assert_eq!(wrapped.data_type, 0);
        assert!(wrapped.msg.is_null());
        assert!(!wrapped.state.is_null());

        let mut raw = blank_stream();
        raw.adler = 0xdead_beef;
        init(&mut raw, -15);
        assert_eq!(
            raw.adler, 0xdead_beef,
            "a raw stream's adler is left alone, C's `if (state->wrap)` guard"
        );

        // SAFETY: both streams are initialised and are ended exactly once.
        unsafe {
            assert_eq!(inflateEnd(&mut wrapped), ReturnCode::OK.as_i32());
            assert_eq!(inflateEnd(&mut raw), ReturnCode::OK.as_i32());
        }
        assert!(wrapped.state.is_null(), "inflateEnd clears strm->state");
        assert!(raw.state.is_null());
    }

    #[test]
    fn a_bad_window_bits_is_refused_without_installing_a_state() {
        let (version, size) = version_args();
        let mut strm = blank_stream();
        // `test/infcover.c` L371 asserts Z_STREAM_ERROR for windowBits == 1.
        for bits in [1, 7, -7, -16, 48, 100] {
            // SAFETY: `strm` is a live zeroed stream, `version` is `ZLIB_VERSION` and
            // `size` is this build's `size_of::<z_stream>()`, so the version gate is
            // satisfied and the only pointer is to a live local.
            let ret = unsafe { inflateInit2_(&mut strm, bits, version, size) };
            assert_eq!(
                ret,
                ReturnCode::STREAM_ERROR.as_i32(),
                "windowBits = {bits}"
            );
            assert!(strm.state.is_null(), "windowBits = {bits} left a state");
        }
        // The whole accepted matrix, for contrast, worked out from `inflate.c`
        // L147-L160: `wrap = (windowBits >> 4) + 5`, then `windowBits &= 15` for any
        // request below 48, and only then the `8..=15` bounds test -- which a
        // `windowBits` of zero skips entirely, because zero means "take the size from
        // the stream's own header". That is why 16 and 32 are legal even though
        // neither is in `8..=15`: they mask to zero.
        for bits in [0, 8, 9, 15, -8, -15, 16, 24, 31, 32, 40, 47] {
            let mut ok = blank_stream();
            init(&mut ok, bits);
            // SAFETY: just initialised.
            assert_eq!(unsafe { inflateEnd(&mut ok) }, ReturnCode::OK.as_i32());
        }
    }

    // -----------------------------------------------------------------------
    // Decoding
    // -----------------------------------------------------------------------

    #[test]
    fn a_zlib_stream_round_trips_through_the_exported_entry_points() {
        let payload = b"hello, hello! and a rather longer tail so a match is found";
        let compressed = deflate_zlib(payload);

        let mut strm = blank_stream();
        init(&mut strm, 15);
        let mut out = vec![0_u8; payload.len() * 2 + 16];
        strm.next_in = compressed.as_ptr();
        strm.avail_in = uInt::try_from(compressed.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = uInt::try_from(out.len()).unwrap();

        // SAFETY: the stream is initialised and both regions are live and disjoint.
        let ret = unsafe { inflate(&mut strm, 4) };
        assert_eq!(ret, ReturnCode::STREAM_END.as_i32());
        assert_eq!(usize::try_from(strm.total_in).unwrap(), compressed.len());
        assert_eq!(usize::try_from(strm.total_out).unwrap(), payload.len());
        assert_eq!(&out[..payload.len()], payload);
        assert_eq!(strm.avail_in, 0);
        assert_eq!(
            strm.avail_out as usize,
            out.len() - payload.len(),
            "avail_out is decremented by exactly what was produced"
        );
        // SAFETY: `next_in`/`next_out` were advanced within their own buffers.
        unsafe {
            assert_eq!(strm.next_in, compressed.as_ptr().add(compressed.len()));
            assert_eq!(strm.next_out, out.as_mut_ptr().add(payload.len()));
        }
        assert!(strm.msg.is_null());

        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    #[test]
    fn decoding_resumes_across_one_byte_calls() {
        let payload = b"resumability is the contract inflate offers incremental callers";
        let compressed = deflate_zlib(payload);

        let mut strm = blank_stream();
        init(&mut strm, 15);
        let mut out = vec![0_u8; payload.len() + 8];
        let mut produced = 0_usize;
        let mut ret = ReturnCode::OK.as_i32();
        for (index, byte) in compressed.iter().enumerate() {
            strm.next_in = core::ptr::from_ref(byte);
            strm.avail_in = 1;
            // SAFETY: `out` outlives the loop and `produced` never exceeds its length.
            strm.next_out = unsafe { out.as_mut_ptr().add(produced) };
            strm.avail_out = uInt::try_from(out.len() - produced).unwrap();
            // SAFETY: the stream is initialised and both regions are live.
            ret = unsafe { inflate(&mut strm, 0) };
            assert!(
                ret == ReturnCode::OK.as_i32()
                    || ret == ReturnCode::STREAM_END.as_i32()
                    || ret == ReturnCode::BUF_ERROR.as_i32(),
                "byte {index} gave {ret}"
            );
            produced = usize::try_from(strm.total_out).unwrap();
            if ret == ReturnCode::STREAM_END.as_i32() {
                break;
            }
        }
        assert_eq!(ret, ReturnCode::STREAM_END.as_i32());
        assert_eq!(produced, payload.len());
        assert_eq!(&out[..payload.len()], payload);
        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    /// An unrecognised `flush` is **not** an error, and a null output is.
    ///
    /// ★ `inflate.c` validates `flush` nowhere: the parameter reaches only
    /// `inflate_fast`'s `Z_FINISH` comparison and the `Z_BLOCK`/`Z_TREES` tests, and every
    /// other value simply behaves as `Z_NO_FLUSH`. Rejecting an out-of-range value would
    /// therefore be a divergence from the oracle rather than a hardening, so the range is
    /// asserted to be accepted here. `deflate` is the asymmetric one -- it does validate --
    /// and that difference is part of the contract, not an oversight.
    #[test]
    fn an_unknown_flush_is_accepted_and_a_null_output_is_a_stream_error() {
        let mut strm = blank_stream();
        init(&mut strm, 15);
        let mut out = [0_u8; 8];
        let input = [0x78_u8, 0x9c];
        strm.next_in = input.as_ptr();
        strm.avail_in = 2;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 8;
        // SAFETY: the stream is initialised and both regions are live.
        unsafe {
            for flush in [-1, 7, 100, c_int::MIN, c_int::MAX] {
                assert_ne!(
                    inflate(&mut strm, flush),
                    ReturnCode::STREAM_ERROR.as_i32(),
                    "flush = {flush} must be accepted, as `inflate.c` accepts it"
                );
            }
            // All seven documented values are accepted, `Z_TREES` included.
            for flush in 0..=MAX_INFLATE_FLUSH {
                let answer = inflate(&mut strm, flush);
                assert_ne!(answer, ReturnCode::STREAM_ERROR.as_i32(), "flush={flush}");
            }
            // L494-L496: a null `next_out` is refused even with input available.
            strm.next_out = core::ptr::null_mut();
            assert_eq!(inflate(&mut strm, 0), ReturnCode::STREAM_ERROR.as_i32());
            // A null `next_in` with a non-zero `avail_in` is refused too...
            strm.next_out = out.as_mut_ptr();
            strm.next_in = core::ptr::null();
            strm.avail_in = 1;
            assert_eq!(inflate(&mut strm, 0), ReturnCode::STREAM_ERROR.as_i32());
            // ...but with `avail_in == 0` it is entirely legal.
            strm.avail_in = 0;
            assert_ne!(inflate(&mut strm, 0), ReturnCode::STREAM_ERROR.as_i32());
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
    }

    #[test]
    fn a_memory_error_is_sticky_and_a_message_survives_a_second_call() {
        // The `Z_DATA_ERROR` half of `test/infcover.c` L421-L422's stickiness check:
        // once `BAD` is latched, every later call reports it again -- and `msg` keeps
        // the exact pointer it published, because C never rewrites it.
        let mut strm = blank_stream();
        init(&mut strm, 15);
        let input = [0x77_u8, 0x85];
        let mut out = [0_u8; 8];
        strm.next_in = input.as_ptr();
        strm.avail_in = 2;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 8;
        // SAFETY: the stream is initialised and both regions are live.
        let first = unsafe { inflate(&mut strm, 0) };
        assert_eq!(first, ReturnCode::DATA_ERROR.as_i32());
        let published = strm.msg;
        assert!(!published.is_null());
        let text = msg_of(&strm).unwrap();
        // SAFETY: as above.
        let second = unsafe { inflate(&mut strm, 0) };
        assert_eq!(second, ReturnCode::DATA_ERROR.as_i32());
        assert!(
            core::ptr::eq(strm.msg, published),
            "the message pointer must not be rewritten on a repeat error"
        );
        assert_eq!(msg_of(&strm), Some(text));
        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    // -----------------------------------------------------------------------
    // ★ The externally written mode tag
    // -----------------------------------------------------------------------

    #[test]
    fn a_mode_written_through_the_raw_state_pointer_is_honoured() {
        // ★ This is `test/infcover.c` L316-L333 reproduced through the ABI. The
        // harness casts `strm.state` to `struct inflate_state *` and assigns
        // `mode = DICT`, then requires `inflateSetDictionary` to answer `Z_OK` and the
        // next `inflate` to answer `Z_BUF_ERROR`. A zero-based `Mode`, or a facade
        // that cached the mode instead of re-reading it, would break both silently.
        //
        // The stream is the harness's own "need dictionary" fixture, hex
        // `8 b8 0 0 0 1` at `windowBits` 8 (its L390): CMF/FLG with FDICT set, then a
        // `DICTID` of 1 -- which is `adler32(0, Z_NULL, 0)`, and therefore exactly the
        // id an EMPTY dictionary has. That is what makes a zero-length dictionary the
        // *correct* one here rather than a degenerate case.
        let input = [0x08_u8, 0xb8, 0x00, 0x00, 0x00, 0x01];
        let mut scratch = [0_u8; 8];
        let mut strm = blank_stream();
        init(&mut strm, 8);

        // The C-visible prefix is `{ z_streamp strm; int mode; }`, which is what
        // `StatePrefix` mirrors, so the harness's `((struct inflate_state *)
        // strm.state)->mode` is a write to `StatePrefix::tag`. Pin the two numbers
        // the C header fixes before relying on them.
        // Relational first, because that is the form that holds on every target: the tag
        // follows the back-pointer with no padding, and the prefix is that pointer plus two
        // `int`s. The measured 64-bit numbers are then pinned under the width guard that
        // makes them true -- 16 and 8 where a pointer is eight bytes wide, 12 and 4 on an
        // ILP32 target, which is what `types.rs`'s own const assertions already say.
        assert_eq!(
            size_of::<StatePrefix>(),
            size_of::<z_streamp>() + 2 * size_of::<c_int>()
        );
        assert_eq!(offset_of!(StatePrefix, tag), size_of::<z_streamp>());
        if size_of::<z_streamp>() == 8 {
            assert_eq!(size_of::<StatePrefix>(), 16);
            assert_eq!(offset_of!(StatePrefix, tag), 8);
        }
        assert_eq!(size_of::<c_int>(), 4);
        let tag = mode_slot(&strm);
        // SAFETY: as above.
        assert_eq!(unsafe { tag.read() }, Mode::Head.as_raw());
        assert_eq!(Mode::Head.as_raw(), 16180);
        assert_eq!(DICT_TAG, Mode::Head.as_raw() + 10, "DICT is the 11th mode");

        strm.next_in = input.as_ptr();
        strm.avail_in = 6;
        strm.next_out = scratch.as_mut_ptr();
        strm.avail_out = 8;
        // SAFETY: the stream is initialised and both regions are live and disjoint.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert_eq!(ret, ReturnCode::NEED_DICT.as_i32());
        assert_eq!(strm.adler, 1, "the dictionary id is published in adler");
        // SAFETY: as above -- `tag` addresses the state's public prefix, which the
        // installer initialised, so the four bytes are readable and aligned.
        let observed = unsafe { tag.read() };
        assert_eq!(observed, DICT_TAG, "the stream is waiting in DICT");

        // L324-L325: the wrong dictionary is a data error, not a stream error.
        // SAFETY: `strm` is a live initialised stream and `input` is live and readable
        // for at least the one byte the length declares.
        let ret = unsafe { inflateSetDictionary(&mut strm, input.as_ptr(), 1) };
        assert_eq!(ret, ReturnCode::DATA_ERROR.as_i32());
        // The right one -- empty -- is accepted.
        // SAFETY: `strm` is a live initialised stream and `scratch` is live; a zero
        // length reads nothing from it, so no byte behind the pointer is touched.
        let ret = unsafe { inflateSetDictionary(&mut strm, scratch.as_ptr(), 0) };
        assert_eq!(ret, ReturnCode::OK.as_i32());

        strm.next_in = core::ptr::null();
        strm.avail_in = 0;
        strm.next_out = scratch.as_mut_ptr();
        strm.avail_out = 8;
        // SAFETY: the stream is initialised, its cursors were just set from live
        // locals, and `avail_in`/`avail_out` are those objects' own lengths.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert_eq!(ret, ReturnCode::BUF_ERROR.as_i32());

        // ★ L330-L333: force the stream back into `DICT` from *outside* the library
        // and require the same two answers again. This is the assertion the whole
        // mode-tag synchronisation exists for.
        // SAFETY: the prefix layout is as documented above.
        unsafe {
            tag.write(DICT_TAG);
        }
        // SAFETY: `strm` is a live initialised stream and `scratch` is live; a zero
        // length reads nothing from it. The externally written mode tag is a value the
        // entry point validates, not a pointer, so it cannot make this read unsound.
        let ret = unsafe { inflateSetDictionary(&mut strm, scratch.as_ptr(), 0) };
        assert_eq!(
            ret,
            ReturnCode::OK.as_i32(),
            "an externally written mode = DICT must be honoured"
        );
        // SAFETY: as the previous `inflate` call -- the same live stream with the same
        // live output region.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert_eq!(ret, ReturnCode::BUF_ERROR.as_i32());

        // A tag outside `HEAD..=SYNC` must make every entry point refuse the stream,
        // which is the *other* thing the tag is for: it is also the validity check.
        // SAFETY: the prefix layout is as documented above.
        unsafe {
            tag.write(0);
            assert_eq!(inflate(&mut strm, 0), ReturnCode::STREAM_ERROR.as_i32());
            assert_eq!(inflateEnd(&mut strm), ReturnCode::STREAM_ERROR.as_i32());
            // Restore a live tag so the state can be released rather than leaked.
            tag.write(Mode::Head.as_raw());
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
    }

    #[test]
    fn the_sync_mode_makes_inflate_refuse_to_resume() {
        // `test/infcover.c` L435-L436: `inflateSync` that fails leaves the stream in
        // `SYNC`, and `inflate` then answers `Z_STREAM_ERROR` rather than resuming.
        let mut strm = blank_stream();
        init(&mut strm, -8);
        let input = [0x80_u8, 0x00];
        let mut out = [0_u8; 8];
        strm.next_in = input.as_ptr();
        strm.avail_in = 2;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 8;
        // SAFETY: the stream is initialised and both regions are live.
        unsafe {
            assert_eq!(inflateSync(&mut strm), ReturnCode::DATA_ERROR.as_i32());
            assert_eq!(inflate(&mut strm, 0), ReturnCode::STREAM_ERROR.as_i32());
        }
        // The tag really is `SYNC`, which is what a caller reading it would see.
        let tag = mode_slot(&strm);
        // SAFETY: `mode_slot` returned a pointer to the live prefix's `tag` member.
        assert_eq!(unsafe { tag.read() }, SYNC_TAG);

        // Feeding the sync pattern completes the search and revives the stream.
        let pattern = [0x00_u8, 0x00, 0xff, 0xff];
        strm.next_in = pattern.as_ptr();
        strm.avail_in = 4;
        // SAFETY: as above.
        unsafe {
            assert_eq!(inflateSync(&mut strm), ReturnCode::OK.as_i32());
            let _ = inflateSyncPoint(&mut strm);
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
    }

    // -----------------------------------------------------------------------
    // Dictionaries, resets, copying and introspection
    // -----------------------------------------------------------------------

    #[test]
    fn set_dictionary_rejects_a_wrapped_stream_that_is_not_waiting_for_one() {
        // `test/infcover.c` L362-L363, including the `(Z_NULL, 0)` argument pair.
        let mut strm = blank_stream();
        init(&mut strm, 15);
        // SAFETY: the stream is initialised; a null dictionary with a zero length is
        // the documented "nothing to install" pair.
        unsafe {
            assert_eq!(
                inflateSetDictionary(&mut strm, core::ptr::null(), 0),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }

        // A raw stream, by contrast, accepts one at any time and the history is then
        // readable back.
        let mut raw = blank_stream();
        init(&mut raw, -15);
        let dictionary = b"a preset dictionary primes the window with history";
        // SAFETY: the stream is initialised and `dictionary` is live.
        unsafe {
            assert_eq!(
                inflateSetDictionary(
                    &mut raw,
                    dictionary.as_ptr(),
                    uInt::try_from(dictionary.len()).unwrap()
                ),
                ReturnCode::OK.as_i32()
            );
        }

        let mut length: uInt = 0;
        // SAFETY: the stream is initialised; only the length is requested.
        unsafe {
            assert_eq!(
                inflateGetDictionary(&mut raw, core::ptr::null_mut(), &mut length),
                ReturnCode::OK.as_i32()
            );
        }
        assert_eq!(usize::try_from(length).unwrap(), dictionary.len());

        // ★ The buffer is the 32768 bytes `zlib.h` L936-L942 requires the caller to
        // provide, because `inflate.c` L1177-L1180 copies `whave` bytes and has no way
        // to know about any smaller size. `dictLength` is an out-parameter -- L1183
        // assigns it and nothing reads it -- so it is deliberately left uninitialised
        // here, which is what a conforming caller is entitled to do and what
        // `test/infcover.c` does with the corresponding `gz_header` members.
        let mut history = vec![0_u8; MAX_WINDOW_BYTES];
        let mut written = MaybeUninit::<uInt>::uninit();
        // SAFETY: `history` is live and writable for the full window, and
        // `written` is a live, aligned, writable `uInt` that this call only writes.
        unsafe {
            assert_eq!(
                inflateGetDictionary(&mut raw, history.as_mut_ptr(), written.as_mut_ptr()),
                ReturnCode::OK.as_i32()
            );
        }
        // SAFETY: the call above assigned it, which is L1183's `*dictLength = whave`.
        let written = usize::try_from(unsafe { written.assume_init() }).unwrap();
        assert_eq!(written, dictionary.len());
        assert_eq!(&history[..written], dictionary);
        assert!(
            history[written..].iter().all(|byte| *byte == 0),
            "exactly `whave` bytes are written and not one more"
        );

        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { inflateEnd(&mut raw) }, ReturnCode::OK.as_i32());
    }

    #[test]
    fn the_three_resets_restore_the_stream_and_keep_it_usable() {
        let payload = b"reset me";
        let compressed = deflate_zlib(payload);
        let mut strm = blank_stream();
        init(&mut strm, 15);
        let mut out = vec![0_u8; 64];
        strm.next_in = compressed.as_ptr();
        strm.avail_in = uInt::try_from(compressed.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 64;
        // SAFETY: the stream is initialised and both regions are live.
        unsafe {
            assert_eq!(inflate(&mut strm, 4), ReturnCode::STREAM_END.as_i32());
            assert!(strm.total_out > 0);

            assert_eq!(inflateResetKeep(&mut strm), ReturnCode::OK.as_i32());
            assert_eq!(strm.total_in, 0);
            assert_eq!(strm.total_out, 0);
            assert_eq!(strm.adler, 1);
            assert!(strm.msg.is_null());

            assert_eq!(inflateReset(&mut strm), ReturnCode::OK.as_i32());
            // `test/infcover.c` L344 resets to raw and then ends the stream.
            assert_eq!(inflateReset2(&mut strm, -8), ReturnCode::OK.as_i32());
            assert_eq!(
                inflateReset2(&mut strm, 1),
                ReturnCode::STREAM_ERROR.as_i32(),
                "a rejected windowBits leaves the stream usable"
            );
            assert_eq!(inflateReset2(&mut strm, 15), ReturnCode::OK.as_i32());
        }

        // Still usable after all that.
        strm.next_in = compressed.as_ptr();
        strm.avail_in = uInt::try_from(compressed.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 64;
        // SAFETY: as above.
        unsafe {
            assert_eq!(inflate(&mut strm, 4), ReturnCode::STREAM_END.as_i32());
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
        assert_eq!(&out[..payload.len()], payload);
    }

    #[test]
    fn copy_duplicates_the_whole_stream_and_both_halves_decode() {
        let payload = b"branching a decode needs the whole z_stream copied, not just the state";
        let compressed = deflate_zlib(payload);

        let mut source = blank_stream();
        init(&mut source, 15);
        let mut first = vec![0_u8; payload.len() + 16];
        source.next_in = compressed.as_ptr();
        source.avail_in = uInt::try_from(compressed.len()).unwrap();
        source.next_out = first.as_mut_ptr();
        source.avail_out = uInt::try_from(first.len()).unwrap();

        let mut copy = blank_stream();
        // SAFETY: `source` is initialised and `copy` is a live, stateless `z_stream`
        // distinct from it.
        unsafe {
            assert_eq!(inflateCopy(&mut copy, &mut source), ReturnCode::OK.as_i32());
        }
        // ★ The whole `z_stream` is copied, so every member matches except `state`.
        assert_eq!(copy.next_in, source.next_in);
        assert_eq!(copy.avail_in, source.avail_in);
        assert_eq!(copy.next_out, source.next_out);
        assert_eq!(copy.avail_out, source.avail_out);
        assert_eq!(copy.total_in, source.total_in);
        assert_eq!(copy.total_out, source.total_out);
        assert_eq!(copy.adler, source.adler);
        assert_eq!(copy.data_type, source.data_type);
        assert!(!copy.state.is_null());
        assert!(!core::ptr::eq(copy.state, source.state));

        // Both decode independently to the same bytes.
        let mut second = vec![0_u8; payload.len() + 16];
        copy.next_out = second.as_mut_ptr();
        // SAFETY: both streams are initialised and their buffers are disjoint.
        unsafe {
            assert_eq!(inflate(&mut source, 4), ReturnCode::STREAM_END.as_i32());
            assert_eq!(inflate(&mut copy, 4), ReturnCode::STREAM_END.as_i32());
        }
        assert_eq!(&first[..payload.len()], payload);
        assert_eq!(&second[..payload.len()], payload);

        // SAFETY: each stream is ended exactly once.
        unsafe {
            assert_eq!(inflateEnd(&mut copy), ReturnCode::OK.as_i32());
            assert_eq!(inflateEnd(&mut source), ReturnCode::OK.as_i32());
        }

        // A destination aliasing the source is refused rather than corrupting it.
        let mut lone = blank_stream();
        init(&mut lone, 15);
        // SAFETY: `lone` is initialised; the call is deliberately aliased.
        unsafe {
            let same: *mut z_stream = &mut lone;
            assert_eq!(
                inflateCopy(same, same),
                ReturnCode::STREAM_ERROR.as_i32(),
                "an aliased copy must be refused, not performed"
            );
            assert_eq!(inflateEnd(&mut lone), ReturnCode::OK.as_i32());
        }
    }

    #[test]
    fn prime_mark_codes_used_undermine_and_validate_match_the_reference() {
        let mut strm = blank_stream();
        init(&mut strm, 15);
        // SAFETY: the stream is initialised for every call below.
        unsafe {
            // `test/infcover.c` L359-L360.
            assert_eq!(inflatePrime(&mut strm, 5, 31), ReturnCode::OK.as_i32());
            assert_eq!(inflatePrime(&mut strm, -1, 0), ReturnCode::OK.as_i32());
            assert_eq!(inflatePrime(&mut strm, 0, 0), ReturnCode::OK.as_i32());
            assert_eq!(inflatePrime(&mut strm, 16, 0), ReturnCode::OK.as_i32());
            // More than sixteen bits at once, or past thirty-two buffered.
            assert_eq!(
                inflatePrime(&mut strm, 17, 0),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(
                inflatePrime(&mut strm, 16, 0),
                ReturnCode::OK.as_i32(),
                "16 + 16 == 32 is the boundary and is accepted"
            );
            assert_eq!(
                inflatePrime(&mut strm, 1, 0),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(inflatePrime(&mut strm, -1, 0), ReturnCode::OK.as_i32());

            // ★ `inflateMark` is a composite, and `back` is -1 with no code pending,
            // so a fresh stream reports exactly `-(1 << 16)` -- the same number a bad
            // state reports, which is why it is not usable as an error test.
            assert_eq!(inflateMark(&mut strm), BAD_STATE_MARK);
            // ★ `inflateUndermine` FAILS in the shipped build:
            // `test/infcover.c` L440.
            assert_eq!(
                inflateUndermine(&mut strm, 1),
                ReturnCode::DATA_ERROR.as_i32()
            );
            assert_eq!(
                inflateUndermine(&mut strm, 0),
                ReturnCode::DATA_ERROR.as_i32()
            );
            assert_eq!(inflateValidate(&mut strm, 1), ReturnCode::OK.as_i32());
            assert_eq!(inflateValidate(&mut strm, 0), ReturnCode::OK.as_i32());
            assert_eq!(inflateSyncPoint(&mut strm), 0);
            // No tables have been built yet.
            assert_eq!(inflateCodesUsed(&mut strm), 0);
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }

        // After a real decode the arena is occupied, and the count is an
        // `unsigned long` rather than a status code.
        let compressed = deflate_zlib(&[b'x'; 4096]);
        let mut used = blank_stream();
        init(&mut used, 15);
        let mut out = vec![0_u8; 8192];
        used.next_in = compressed.as_ptr();
        used.avail_in = uInt::try_from(compressed.len()).unwrap();
        used.next_out = out.as_mut_ptr();
        used.avail_out = 8192;
        // SAFETY: the stream is initialised and both regions are live.
        unsafe {
            assert_eq!(inflate(&mut used, 4), ReturnCode::STREAM_END.as_i32());
            let count: c_ulong = inflateCodesUsed(&mut used);
            assert!(
                count <= narrow_uLong(u64::from(ENOUGH)),
                "codes used ({count}) must stay inside the {ENOUGH}-entry arena"
            );
            assert_eq!(inflateEnd(&mut used), ReturnCode::OK.as_i32());
        }
    }

    #[test]
    fn get_header_reports_a_gzip_header_and_refuses_a_zlib_stream() {
        // A gzip stream carrying a name and a comment, built by hand so the fields
        // are known: FLG = 0x1c sets FEXTRA | FNAME | FCOMMENT.
        let mut gz: Vec<u8> = vec![0x1f, 0x8b, 0x08, 0x1c, 0x11, 0x22, 0x33, 0x44, 0x02, 0x03];
        gz.extend_from_slice(&[0x02, 0x00, 0xab, 0xcd]); // XLEN = 2, then the bytes
        gz.extend_from_slice(b"name\0");
        gz.extend_from_slice(b"comment\0");
        // An empty final stored block, then CRC-32 and ISIZE of nothing.
        gz.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
        gz.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        gz.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);

        let mut extra = [0_u8; 8];
        let mut name = [0_u8; 8];
        let mut comment = [0_u8; 16];
        let head = header_on_heap(gz_header {
            text: 7,
            time: 0xdead,
            xflags: 7,
            os: 7,
            extra: extra.as_mut_ptr(),
            extra_len: 99,
            extra_max: 8,
            name: name.as_mut_ptr(),
            name_max: 8,
            comment: comment.as_mut_ptr(),
            comm_max: 16,
            hcrc: 7,
            done: 7,
        });

        let mut strm = blank_stream();
        init(&mut strm, 15);
        // A zlib-only stream cannot accept a header: `(wrap & 2) == 0`.
        // SAFETY: the stream is initialised and `head` is live with live buffers.
        unsafe {
            assert_eq!(
                inflateGetHeader(&mut strm, head),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }

        let mut strm = blank_stream();
        init(&mut strm, 47);
        // A null header is refused rather than dereferenced.
        // SAFETY: the stream is initialised; the null is the point of the call.
        unsafe {
            assert_eq!(
                inflateGetHeader(&mut strm, core::ptr::null_mut()),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(inflateGetHeader(&mut strm, head), ReturnCode::OK.as_i32());
        }
        // ★ Only `done` is written on install; the caller's other scalars survive.
        // SAFETY: `head` is the live heap header, read through the same raw pointer the
        // library holds.
        unsafe {
            assert_eq!((*head).done, 0);
            assert_eq!((*head).text, 7);
            assert_eq!((*head).time, 0xdead);
            assert_eq!((*head).extra_len, 99);
        }

        let mut out = [0_u8; 32];
        strm.next_in = gz.as_ptr();
        strm.avail_in = uInt::try_from(gz.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 32;
        // SAFETY: the stream is initialised and all regions are live and disjoint.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert!(
            ret == ReturnCode::OK.as_i32()
                || ret == ReturnCode::STREAM_END.as_i32()
                || ret == ReturnCode::BUF_ERROR.as_i32(),
            "gzip decode gave {ret}"
        );
        // SAFETY: as above -- one pointer, one provenance.
        unsafe {
            assert_eq!((*head).done, 1, "the header parsed to completion");
            assert_eq!((*head).time, 0x4433_2211);
            assert_eq!((*head).xflags, 0x02);
            assert_eq!((*head).os, 0x03);
            assert_eq!((*head).extra_len, 2);
            assert_eq!((*head).text, 0);
            assert_eq!((*head).hcrc, 0);
        }
        assert_eq!(&extra[..2], &[0xab, 0xcd]);
        assert_eq!(&name[..5], b"name\0");
        assert_eq!(&comment[..8], b"comment\0");
        // SAFETY: the stream is the initialised one, and the header is released only
        // after it, so nothing installed can outlive the allocation.
        unsafe {
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
            release_header(head);
        }
    }

    #[test]
    fn get_dictionary_never_reads_the_incoming_length() {
        // ★ `dictLength` is an OUTPUT parameter. `inflate.c` L1167-L1185 writes
        // `*dictLength = state->whave` and never reads it, so a conforming caller may pass
        // an uninitialised `uInt` -- and the amount copied is decided by `whave`, never by
        // anything the caller supplied. Reading the incoming value would be indeterminate
        // -- value UB, and using it to size a slice would be an out-of-bounds write waiting
        // for a caller with a large stack value.
        //
        // `MaybeUninit` is what makes the test real rather than decorative: under Miri, any
        // read of the slot is reported. Under a plain `cargo test` the second half still
        // catches a size taken from the caller, because the copied length is asserted
        // against `whave` rather than against the capacity offered.
        let payload = b"a dictionary the decoder will remember";
        let stream = deflate_zlib(payload);

        let mut strm = blank_stream();
        init(&mut strm, 15);
        // Fed in two pieces with a cramped output, so `updatewindow` runs and the window
        // genuinely holds history -- a single-shot decode can finish with `whave == 0` and
        // would prove nothing about the amount copied.
        let mut out = [0_u8; 64];
        strm.next_in = stream.as_ptr();
        strm.avail_in = uInt::try_from(stream.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 8;
        // SAFETY: the stream is initialised and both regions are live and disjoint.
        let first = unsafe { inflate(&mut strm, 0) };
        assert_eq!(first, ReturnCode::OK.as_i32());
        strm.next_out = out[8..].as_mut_ptr();
        strm.avail_out = 56;
        assert_eq!(
            // SAFETY: as above; the second window of the same live output buffer.
            unsafe { inflate(&mut strm, 0) },
            ReturnCode::STREAM_END.as_i32()
        );
        assert_eq!(&out[..payload.len()], payload);

        // Ask the library itself how much history it kept, rather than assuming: `whave` is
        // an implementation detail of when `updatewindow` ran, and the point of the test is
        // that the *caller* cannot influence it.
        let mut probe: uInt = 0;
        assert_eq!(
            // SAFETY: the stream is the initialised one and `probe` is a live `uInt`. A null
            // `dictionary` is the documented length-only form.
            unsafe { inflateGetDictionary(&mut strm, core::ptr::null_mut(), &mut probe) },
            ReturnCode::OK.as_i32()
        );
        let expected = probe;
        assert!(
            expected > 0,
            "the window must hold history for this to prove anything"
        );
        let n = usize::try_from(expected).unwrap();

        // The reference answer, obtained with an ordinary initialised slot.
        let mut reference = [0xff_u8; 128];
        let mut plain: uInt = 0;
        assert_eq!(
            // SAFETY: the stream is the initialised one, `reference` is live for 128 bytes
            // and `plain` is a live, initialised `uInt`.
            unsafe { inflateGetDictionary(&mut strm, reference.as_mut_ptr(), &mut plain) },
            ReturnCode::OK.as_i32()
        );
        assert_eq!(plain, expected);
        assert!(
            reference[n..].iter().all(|&b| b == 0xff),
            "nothing may be written past whave bytes"
        );

        // The same call with an UNINITIALISED length slot, exactly as C permits. It must
        // produce byte-identical results, which is only possible if the incoming value is
        // never consulted. Under Miri, reading the slot is reported outright.
        let mut dictionary = [0xff_u8; 128];
        let mut slot = MaybeUninit::<uInt>::uninit();
        assert_eq!(
            // SAFETY: the stream is the initialised one, `dictionary` is live for 128 bytes
            // and `slot` is live, aligned storage for one `uInt`. The slot's contents are
            // indeterminate, which is precisely what this asserts must never be read.
            unsafe { inflateGetDictionary(&mut strm, dictionary.as_mut_ptr(), slot.as_mut_ptr()) },
            ReturnCode::OK.as_i32()
        );
        // SAFETY: the call above wrote it, which is the only reason it may be read now.
        assert_eq!(unsafe { slot.assume_init() }, expected);
        assert_eq!(
            dictionary, reference,
            "an uninitialised slot changes nothing"
        );

        // And a hostile *initialised* slot, which catches the same defect without Miri: were
        // the incoming value read and used as a capacity, this would copy far past `whave`.
        let mut hostile: uInt = uInt::MAX;
        let mut third = [0xff_u8; 128];
        assert_eq!(
            // SAFETY: as above, with both the buffer and the slot live and initialised.
            unsafe { inflateGetDictionary(&mut strm, third.as_mut_ptr(), &mut hostile) },
            ReturnCode::OK.as_i32()
        );
        assert_eq!(
            hostile, expected,
            "the caller's value must be overwritten, not consulted"
        );
        assert_eq!(third, reference, "and it must not widen the copy");

        // With a null dictionary C writes the length and copies nothing (L1180-L1184).
        let mut length_only = MaybeUninit::<uInt>::uninit();
        assert_eq!(
            // SAFETY: as above; a null `dictionary` is the documented "length only" form.
            unsafe {
                inflateGetDictionary(&mut strm, core::ptr::null_mut(), length_only.as_mut_ptr())
            },
            ReturnCode::OK.as_i32()
        );
        // SAFETY: the call above wrote it.
        assert_eq!(unsafe { length_only.assume_init() }, expected);

        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    #[test]
    fn a_reset_detaches_the_installed_header() {
        // `inflateResetKeep` assigns `state->head = Z_NULL` (`inflate.c` L110), so a
        // reset must leave the caller's structure alone from then on.
        let head = header_on_heap(gz_header {
            done: 5,
            ..zeroed_header()
        });
        let mut strm = blank_stream();
        init(&mut strm, 47);
        // SAFETY: the stream is initialised and `head` is the live heap header, reached
        // through the one pointer the library also holds.
        unsafe {
            assert_eq!(inflateGetHeader(&mut strm, head), ReturnCode::OK.as_i32());
            assert_eq!((*head).done, 0);
            assert_eq!(inflateReset(&mut strm), ReturnCode::OK.as_i32());
            (*head).done = 5;
        }
        let mut out = [0_u8; 8];
        let input = [0x1f_u8, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0, 0];
        strm.next_in = input.as_ptr();
        strm.avail_in = 10;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 8;
        // SAFETY: as above.
        unsafe {
            let _ = inflate(&mut strm, 0);
            assert_eq!(
                (*head).done,
                5,
                "a detached header must not be written after a reset"
            );
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
            release_header(head);
        }
    }

    // -----------------------------------------------------------------------
    // ★ inflate_table -- the export `test/infcover.c` cannot link without
    // -----------------------------------------------------------------------

    #[test]
    fn inflate_table_reproduces_cover_trees_exactly() {
        // `test/infcover.c` L617-L639, transcribed argument for argument.
        let mut lens = [0_u16; 16];
        for (index, slot) in lens.iter_mut().enumerate().take(15) {
            *slot = u16::try_from(index + 1).unwrap();
        }
        lens[15] = 15;
        let mut work = [0_u16; 16];
        let mut table = vec![code::default(); ENOUGH_DISTS];

        for requested in [15_u32, 1] {
            let mut next: *mut code = table.as_mut_ptr();
            let mut bits: c_uint = requested;
            // SAFETY: `lens` and `work` are sixteen `u16`s each, `table` holds
            // `ENOUGH_DISTS` entries, and `next`/`bits` are live locals. The three
            // arrays are distinct allocations.
            let ret = unsafe {
                _zlib_rs_inflate_table(
                    DISTS,
                    lens.as_mut_ptr().cast::<c_ushort>(),
                    16,
                    &mut next,
                    &mut bits,
                    work.as_mut_ptr().cast::<c_ushort>(),
                )
            };
            assert_eq!(ret, 1, "requested root width {requested} must overflow");
            // ★ Neither in/out parameter is advanced on a non-zero return, which is
            // what C's early `return` before `inftrees.c` L308-L309 does.
            assert_eq!(next, table.as_mut_ptr());
            assert_eq!(bits, requested);
        }
    }

    // -----------------------------------------------------------------------
    // Streams whose output members were never set
    // -----------------------------------------------------------------------

    /// A stream in the shape `test/infcover.c` builds: only the members a caller must
    /// provide are written, so `next_out`, `avail_out` and `reserved` genuinely hold no
    /// value.
    ///
    /// `MaybeUninit` rather than a poison value, because that is what gives these tests
    /// teeth: reading an uninitialised member is undefined behaviour that Miri reports,
    /// where a poison value would merely be a number nobody noticed being read. C's
    /// `inflateSync`, `inflateGetDictionary`, `inflateSyncPoint` and `inflateCopy` all
    /// run correctly on such a stream, so this port must too.
    // Boxed deliberately, and `clippy::unnecessary_box_returns` is wrong about it here:
    // `inflateInit2_` records the *address* of the `z_stream` in the state block's prefix,
    // and every later entry point checks it (`inflate.c` L94's `state->strm != strm`).
    // Returning the value would move it into the caller's slot after that address was
    // taken, so the recorded owner would be stale and every subsequent call would report
    // `Z_STREAM_ERROR`. The heap allocation is what keeps the address stable across the
    // move. Written as a comment rather than the lint's `reason` field, which needs Rust
    // 1.81 while this workspace declares `rust-version = "1.80"`.
    #[allow(clippy::unnecessary_box_returns)]
    fn stream_with_no_output_members(window_bits: c_int) -> Box<MaybeUninit<z_stream>> {
        let mut raw: Box<MaybeUninit<z_stream>> = Box::new(MaybeUninit::uninit());
        let strm: *mut z_stream = raw.as_mut_ptr();
        let (version, size) = version_args();
        // SAFETY: `strm` addresses a whole `z_stream` this function owns. Each write goes
        // through a raw place, so nothing is read and no reference to the partly
        // initialised structure is formed. `inflateInit2_` reads only the three allocator
        // members, `version` and `stream_size`, all of which are written first.
        unsafe {
            core::ptr::addr_of_mut!((*strm).zalloc).write(None);
            core::ptr::addr_of_mut!((*strm).zfree).write(None);
            core::ptr::addr_of_mut!((*strm).opaque).write(core::ptr::null_mut());
            core::ptr::addr_of_mut!((*strm).next_in).write(core::ptr::null());
            core::ptr::addr_of_mut!((*strm).avail_in).write(0);
            assert_eq!(
                inflateInit2_(strm, window_bits, version, size),
                ReturnCode::OK.as_i32()
            );
        }
        raw
    }

    /// `inflateSync` works on a stream whose output members hold no value, and leaves
    /// them alone.
    ///
    /// `inflate.c` L1264-L1309 reads `avail_in`, `next_in`, `total_in` and `total_out`
    /// and writes `avail_in`, `next_in`, `total_in`, `total_out`, `msg`, `data_type` and
    /// -- for a wrapped stream -- `adler`. The output pair appears nowhere, and
    /// `zlib.h` L951-L963 asks nothing of it.
    #[test]
    fn sync_touches_no_output_member() {
        let mut raw = stream_with_no_output_members(-15);
        let strm = raw.as_mut_ptr();
        // The four-byte sync marker `test/infcover.c` L435 uses.
        let marker = [0x00_u8, 0x00, 0xff, 0xff];
        // SAFETY: the stream is the initialised one, `marker` is live for the call, and
        // every access goes through a raw place.
        unsafe {
            core::ptr::addr_of_mut!((*strm).next_in).write(marker.as_ptr());
            core::ptr::addr_of_mut!((*strm).avail_in).write(4);
            assert_eq!(inflateSync(strm), ReturnCode::OK.as_i32());
            assert_eq!(
                core::ptr::addr_of!((*strm).avail_in).read(),
                0,
                "all four marker bytes are consumed"
            );
            assert_eq!(
                core::ptr::addr_of!((*strm).total_in).read(),
                4,
                "inflate.c L1294 adds the bytes looked at, and the reset restores it"
            );
            // `inflateSyncPoint` reads the state only, so it is safe here too.
            let _ = inflateSyncPoint(strm);
            assert_eq!(inflateEnd(strm), ReturnCode::OK.as_i32());
        }
    }

    /// A raw stream's `adler` member is neither read nor written, from init to
    /// `inflateEnd`.
    ///
    /// ★ **This is the member whose initialisation C makes conditional, so it is the
    /// member the facade must never read.** `inflateResetKeep` assigns `strm->adler`
    /// under `if (state->wrap)` (`inflate.c` L108-L109) and the epilogue under
    /// `if ((state->wrap & 4) && out)` (L1144-L1146); `windowBits = -15` satisfies
    /// neither, so the reference leaves the member exactly as the caller left it --
    /// which for a caller that never set it means *indeterminate*. Loading it into the
    /// core's view on every call would therefore read an uninitialised member, which Miri
    /// reports as undefined behaviour on precisely this path.
    ///
    /// The sentinel is what makes both halves observable at once: it survives only if
    /// the value is passed over untouched, and a spurious write would replace it with
    /// the check value the decoder never computes for a raw stream.
    #[test]
    fn a_raw_stream_leaves_the_adler_member_alone_through_a_whole_decode() {
        let payload = b"raw payload, raw payload, raw payload, and again";
        let wrapped = deflate_zlib(payload);
        // RFC 1950: a zlib stream is a two-byte header, the raw DEFLATE stream, then a
        // four-byte Adler-32. The middle is exactly what `windowBits = -15` reads.
        let raw = &wrapped[2..wrapped.len() - 4];

        let mut strm = blank_stream();
        let sentinel: c_ulong = 0x0bad_f00d;
        strm.adler = sentinel;
        init(&mut strm, -15);
        assert_eq!(strm.adler, sentinel, "init left it alone (inflate.c L108)");

        let mut out = vec![0_u8; payload.len() + 64];
        strm.next_in = raw.as_ptr();
        strm.avail_in = uInt::try_from(raw.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = uInt::try_from(out.len()).unwrap();
        // SAFETY: the stream is initialised and both regions are live and disjoint.
        let ret = unsafe { inflate(&mut strm, 4) };
        assert_eq!(
            ret,
            ReturnCode::STREAM_END.as_i32(),
            "the raw stream decodes"
        );
        let produced = out.len() - usize::try_from(strm.avail_out).unwrap();
        assert_eq!(&out[..produced], payload, "and produces the payload");
        assert_eq!(
            strm.adler, sentinel,
            "and the whole decode left `adler` alone: `wrap & 4` is clear"
        );

        // A reset does not touch it either, for the same reason.
        // SAFETY: the stream holds a live state.
        unsafe {
            assert_eq!(inflateReset(&mut strm), ReturnCode::OK.as_i32());
        }
        assert_eq!(strm.adler, sentinel, "nor does the reset");
        // SAFETY: as above; the state is ended exactly once.
        unsafe {
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
    }

    /// `inflateSync` reports `Z_BUF_ERROR` for an empty stream without reading the
    /// output members either.
    #[test]
    fn sync_with_no_input_reports_buf_error() {
        let mut raw = stream_with_no_output_members(-15);
        let strm = raw.as_mut_ptr();
        // SAFETY: as above.
        unsafe {
            assert_eq!(inflateSync(strm), ReturnCode::BUF_ERROR.as_i32());
            assert_eq!(inflateEnd(strm), ReturnCode::OK.as_i32());
        }
    }

    /// `inflateCopy` duplicates a stream whose output members hold no value.
    ///
    /// `inflate.c` L1352 is `zmemcpy((voidpf)dest, (voidpf)source, sizeof(z_stream))` --
    /// a byte copy. Reading the structure as a `z_stream` value instead would read
    /// `next_out`, `avail_out` and `reserved`, and reading an indeterminate `*mut Bytef`
    /// is undefined behaviour rather than a garbage value.
    #[test]
    fn copy_of_a_stream_with_no_output_members_is_a_byte_copy() {
        let mut raw = stream_with_no_output_members(-15);
        let source = raw.as_mut_ptr();
        let mut copy = blank_stream();
        // SAFETY: both streams are live, distinct and non-overlapping; `source` holds a
        // state this library installed.
        unsafe {
            assert_eq!(
                inflateCopy(core::ptr::addr_of_mut!(copy), source),
                ReturnCode::OK.as_i32()
            );
            assert!(!copy.state.is_null(), "the copy has a state of its own");
            assert!(
                !core::ptr::eq(copy.state, core::ptr::addr_of!((*source).state).read()),
                "and it is not the source's"
            );
            assert_eq!(
                inflateEnd(core::ptr::addr_of_mut!(copy)),
                ReturnCode::OK.as_i32()
            );
            assert_eq!(inflateEnd(source), ReturnCode::OK.as_i32());
        }
    }

    /// A refused allocation inside `inflateCopy` leaves `dest` **completely** untouched.
    ///
    /// `inflate.c` allocates the state at L1341 and the window at L1345, and only once
    /// both have succeeded does it write anything to the destination: the `zmemcpy` of
    /// the stream is at L1352 and `dest->state` is assigned last, at L1365. So a caller
    /// whose copy fails for want of memory finds its structure exactly as it left it.
    /// Installing a placeholder state first and clearing it again on failure would be
    /// observable -- `dest->state` would change value and change back -- and needlessly
    /// so, which is why the destination is written only once everything has succeeded.
    #[test]
    fn a_refused_copy_leaves_the_destination_untouched() {
        for deny_from in [1_usize, 2] {
            let zone = Zone::new();
            let mut strm = blank_stream();
            strm.zalloc = Some(refuse_alloc);
            strm.zfree = Some(refuse_free);
            strm.opaque = zone.as_opaque();
            init(&mut strm, 15);

            // Decode in pieces, so the source owns a window: that is the second
            // allocation `inflateCopy` has to make.
            let payload = [b'z'; 300];
            let compressed = deflate_zlib(&payload);
            let mut chunk = [0_u8; 64];
            strm.next_in = compressed.as_ptr();
            strm.avail_in = uInt::try_from(compressed.len()).unwrap();
            loop {
                strm.next_out = chunk.as_mut_ptr();
                strm.avail_out = 64;
                // SAFETY: the stream is initialised and both regions are live.
                let ret = unsafe { inflate(&mut strm, 0) };
                if ret == ReturnCode::STREAM_END.as_i32() {
                    break;
                }
                assert_eq!(ret, ReturnCode::OK.as_i32());
            }
            let taken = zone.with(|zone| zone.requests);
            assert!(taken >= 2, "the source must own a window by now");

            // A destination filled with values nothing in this call may disturb.
            let mut dest = blank_stream();
            dest.next_in = compressed.as_ptr();
            dest.avail_in = 7;
            dest.total_in = 11;
            dest.next_out = chunk.as_mut_ptr();
            dest.avail_out = 13;
            dest.total_out = 17;
            dest.data_type = 19;
            dest.adler = 23;
            dest.reserved = 29;
            let before = dest;

            zone.with(|zone| zone.deny_from = taken + deny_from);
            // SAFETY: both streams are live, distinct and non-overlapping.
            let copied = unsafe { inflateCopy(&mut dest, &mut strm) };
            assert_eq!(
                copied,
                ReturnCode::MEM_ERROR.as_i32(),
                "request {deny_from} of the copy refused"
            );

            assert!(
                dest.next_in == before.next_in
                    && dest.avail_in == before.avail_in
                    && dest.total_in == before.total_in
                    && dest.next_out == before.next_out
                    && dest.avail_out == before.avail_out
                    && dest.total_out == before.total_out
                    && dest.msg == before.msg
                    && dest.state == before.state
                    && dest.data_type == before.data_type
                    && dest.adler == before.adler
                    && dest.reserved == before.reserved,
                "every member of dest must be untouched"
            );
            assert!(
                dest.state.is_null(),
                "including a state that was never installed"
            );

            // The source is still usable, and everything the copy took is back.
            zone.with(|zone| zone.deny_from = 0);
            // SAFETY: the stream is the initialised one.
            assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
            assert_eq!(
                zone.with(|zone| zone.live()),
                0,
                "a refused copy leaks nothing"
            );
        }
    }

    // -----------------------------------------------------------------------
    // The dictionary
    // -----------------------------------------------------------------------

    /// `inflateGetDictionary` writes `*dictLength` and never reads it, and copies the
    /// whole history rather than clamping to whatever the member happened to hold.
    ///
    /// `inflate.c` L1176-L1183 copies `state->whave` bytes and then assigns
    /// `*dictLength = state->whave`; the member is an output. `zlib.h` L936-L942 puts
    /// the capacity promise on the caller -- "32768 bytes is always enough" -- so there
    /// is nothing to clamp against. Reading the member as a capacity would truncate the
    /// copy whenever it held a small value: below it holds zero, the value a caller most
    /// naturally passes, and a clamp to zero would copy nothing at all.
    #[test]
    fn get_dictionary_writes_the_length_and_never_reads_it() {
        let payload: Vec<u8> = (0..600_u32).map(|index| (index % 251) as u8).collect();
        let compressed = deflate_zlib(&payload);

        let mut strm = blank_stream();
        init(&mut strm, 15);
        let mut produced: Vec<u8> = Vec::new();
        let mut chunk = [0_u8; 100];
        strm.next_in = compressed.as_ptr();
        strm.avail_in = uInt::try_from(compressed.len()).unwrap();
        // Decoded in 100-byte pieces, because the window is filled by `updatewindow`
        // (`inflate.c` L1128-L1133) only when output is flushed: a single-shot decode
        // into one large buffer with `Z_FINISH` leaves `whave` at zero, in C as here.
        loop {
            strm.next_out = chunk.as_mut_ptr();
            strm.avail_out = 100;
            // SAFETY: the stream is initialised and both regions are live and disjoint.
            let ret = unsafe { inflate(&mut strm, 0) };
            produced.extend_from_slice(&chunk[..100 - usize::try_from(strm.avail_out).unwrap()]);
            if ret == ReturnCode::STREAM_END.as_i32() {
                break;
            }
            assert_eq!(ret, ReturnCode::OK.as_i32());
        }
        assert_eq!(produced, payload);

        // ★ Zero, as a caller that expects an output would leave it -- and the value a
        // capacity misreading would clamp the copy to, copying nothing at all.
        let mut length: uInt = 0;
        // 0xa5 rather than zero, so that "not written" is distinguishable from "written
        // as zero" -- the payload contains zero bytes.
        let mut dictionary = vec![0xa5_u8; 32768];
        // SAFETY: the stream holds a live state; `dictionary` is 32768 bytes, the space
        // `zlib.h` L936-L942 requires; `length` is a live `uInt`.
        let ret = unsafe {
            inflateGetDictionary(
                &mut strm,
                dictionary.as_mut_ptr(),
                core::ptr::addr_of_mut!(length),
            )
        };
        assert_eq!(ret, ReturnCode::OK.as_i32());

        // 500, not 600, and the number is the reference's. Measured by running this
        // exact fixture -- 600 bytes decoded in 100-byte pieces -- against the C library:
        // it reports `dictLength = 500`, the history `updatewindow` had added by the
        // time the stream ended, and the bytes are the first 500 of the payload because
        // the window has not wrapped. A one-shot `Z_FINISH` decode into one large buffer
        // reports 0 in both implementations, which is why this test feeds chunks.
        let reported = usize::try_from(length).unwrap();
        assert_eq!(
            reported, 500,
            "the length the reference reports for this fixture"
        );
        assert_eq!(
            &dictionary[..reported],
            &payload[..reported],
            "the history is copied in order, despite *dictLength arriving as zero"
        );
        assert!(
            dictionary[reported..].iter().all(|byte| *byte == 0xa5),
            "and nothing beyond the history is written"
        );

        // Either pointer may be null, independently (L1176 and L1182).
        // SAFETY: as above; the nulls are the point of the calls.
        unsafe {
            let mut only_length: uInt = 12345;
            assert_eq!(
                inflateGetDictionary(
                    &mut strm,
                    core::ptr::null_mut(),
                    core::ptr::addr_of_mut!(only_length)
                ),
                ReturnCode::OK.as_i32()
            );
            assert_eq!(usize::try_from(only_length).unwrap(), reported);
            assert_eq!(
                inflateGetDictionary(&mut strm, dictionary.as_mut_ptr(), core::ptr::null_mut()),
                ReturnCode::OK.as_i32()
            );
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
    }

    /// The same on a stream whose output members hold no value, and with no history yet.
    #[test]
    fn get_dictionary_on_a_fresh_stream_reports_nothing_and_reads_nothing() {
        let mut raw = stream_with_no_output_members(15);
        let strm = raw.as_mut_ptr();
        let mut length: uInt = 999;
        let mut dictionary = [0_u8; 32];
        // SAFETY: the stream is the initialised one; both out-parameters are live.
        unsafe {
            assert_eq!(
                inflateGetDictionary(
                    strm,
                    dictionary.as_mut_ptr(),
                    core::ptr::addr_of_mut!(length)
                ),
                ReturnCode::OK.as_i32()
            );
            assert_eq!(length, 0, "no history yet");
            assert_eq!(inflateEnd(strm), ReturnCode::OK.as_i32());
        }
    }

    // -----------------------------------------------------------------------
    // The gzip header, bound per call
    // -----------------------------------------------------------------------

    /// A buffer supplied *after* `inflateGetHeader` is filled, and a capacity enlarged
    /// between calls is honoured.
    ///
    /// C reads `head->extra`, `head->extra_max`, `head->name`, `head->name_max`,
    /// `head->comment` and `head->comm_max` inside the `EXTRA`, `NAME` and `COMMENT`
    /// states -- `inflate.c` L608-L620, L625-L634, L640-L649 -- so the values that
    /// matter are the ones present when the bytes arrive, not when the header was
    /// installed. Reading them once, in `inflateGetHeader`, would ignore anything a caller
    /// did afterwards.
    #[test]
    fn a_header_buffer_supplied_after_get_header_is_still_filled() {
        // FLG = 0x1c: FEXTRA | FNAME | FCOMMENT.
        let mut gz: Vec<u8> = vec![0x1f, 0x8b, 0x08, 0x1c, 0x11, 0x22, 0x33, 0x44, 0x02, 0x03];
        gz.extend_from_slice(&[0x02, 0x00, 0xab, 0xcd]);
        gz.extend_from_slice(b"a-name\0");
        gz.extend_from_slice(b"a-comment\0");
        gz.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
        gz.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        gz.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);

        let head = header_on_heap(zeroed_header());

        let mut strm = blank_stream();
        init(&mut strm, 47);
        // Installed with every buffer absent.
        // SAFETY: the stream is initialised and `head` is the live heap header.
        let installed = unsafe { inflateGetHeader(&mut strm, head) };
        assert_eq!(installed, ReturnCode::OK.as_i32());

        // Supplied afterwards, which C honours and a snapshot would not.
        let mut extra = [0_u8; 8];
        let mut name = [0_u8; 16];
        let mut comment = [0_u8; 16];
        // SAFETY: `head` is the live heap header; every write goes through the same raw
        // pointer the library holds, so the provenance the library reads through stays
        // valid -- which is the whole reason the header is not a local.
        unsafe {
            (*head).extra = extra.as_mut_ptr();
            (*head).extra_max = 8;
            (*head).name = name.as_mut_ptr();
            (*head).name_max = 16;
            (*head).comment = comment.as_mut_ptr();
            (*head).comm_max = 16;
        }

        let mut out = [0_u8; 32];
        strm.next_in = gz.as_ptr();
        strm.avail_in = uInt::try_from(gz.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 32;
        // SAFETY: the stream is initialised, every region is live and disjoint, and the
        // header and its three buffers outlive the call.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert!(
            ret == ReturnCode::OK.as_i32() || ret == ReturnCode::STREAM_END.as_i32(),
            "gzip decode gave {ret}"
        );
        // SAFETY: as above.
        unsafe {
            assert_eq!((*head).done, 1, "the header parsed to completion");
            assert_eq!((*head).time, 0x4433_2211);
            assert_eq!((*head).os, 0x03);
        }
        assert_eq!(&extra[..2], &[0xab, 0xcd], "the extra field, bound late");
        assert_eq!(&name[..7], b"a-name\0", "the name, bound late");
        assert_eq!(&comment[..10], b"a-comment\0", "the comment, bound late");
        // SAFETY: the stream is the initialised one, released before the header.
        unsafe {
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
            release_header(head);
        }
    }

    /// A member the parse never assigns keeps the caller's value, and no member is read
    /// to achieve that.
    ///
    /// A zlib stream is not a gzip stream, so `inflate` sets `done` to -1 (`inflate.c`
    /// L509) and assigns nothing else. Every other member must therefore still hold what
    /// the caller put there -- including the four a decoding caller need never
    /// initialise at all.
    #[test]
    fn a_header_the_stream_does_not_carry_leaves_every_member_alone() {
        let payload = b"hello, hello!";
        let compressed = deflate_zlib(payload);

        let head = header_on_heap(gz_header {
            text: 5,
            time: 0xfeed,
            xflags: 6,
            os: 7,
            extra_len: 99,
            hcrc: 8,
            done: 4,
            ..zeroed_header()
        });

        let mut strm = blank_stream();
        // 47 accepts either container, so a zlib stream reaches the gzip-header check.
        init(&mut strm, 47);
        // SAFETY: the stream is initialised and `head` is the live heap header.
        unsafe {
            assert_eq!(inflateGetHeader(&mut strm, head), ReturnCode::OK.as_i32());
            assert_eq!((*head).done, 0, "inflate.c L1229 writes only this one");
            assert_eq!((*head).text, 5);
            assert_eq!((*head).hcrc, 8);
        }

        let mut out = [0_u8; 32];
        strm.next_in = compressed.as_ptr();
        strm.avail_in = uInt::try_from(compressed.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 32;
        // SAFETY: the stream is initialised and every region is live and disjoint.
        let finished = unsafe { inflate(&mut strm, 4) };
        assert_eq!(finished, ReturnCode::STREAM_END.as_i32());
        assert_eq!(&out[..payload.len()], payload);

        // SAFETY: as above -- one pointer, one provenance.
        unsafe {
            assert_eq!((*head).done, -1, "inflate.c L509: not a gzip stream");
            assert_eq!((*head).text, 5, "untouched");
            assert_eq!((*head).time, 0xfeed, "untouched");
            assert_eq!((*head).xflags, 6, "untouched");
            assert_eq!((*head).os, 7, "untouched");
            assert_eq!((*head).extra_len, 99, "untouched");
            assert_eq!((*head).hcrc, 8, "untouched");
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
            release_header(head);
        }
    }

    /// A build that runs out of space still leaves in the caller's array every entry
    /// it wrote before it stopped.
    ///
    /// ★ C builds straight into the caller's table, so the `return 1` at `inftrees.c`
    /// L287 leaves the entries written so far behind, so this wrapper must build straight
    /// into the caller's table too: building into scratch space and copying out nothing on
    /// failure would discard them. The expected values below are taken from the reference
    /// implementation, by filling the table with a sentinel, calling C's
    /// `inflate_table` with these exact arguments, and recording which entries changed:
    ///
    /// | root | return | entries written | contents |
    /// |---|---|---|---|
    /// | 1 | 1 | 1 | `[0] = {op 16, bits 1, val 1}` |
    /// | 2 | 1 | 3 | `{16,1,1}`, `{16,2,2}`, `{16,1,1}` |
    /// | 15 | 1 | 0 | nothing |
    ///
    /// The writes form a prefix in every case, with no gaps.
    #[test]
    fn a_table_that_runs_out_of_space_publishes_what_it_wrote() {
        let mut lens = [0_u16; 16];
        for (index, slot) in lens.iter_mut().enumerate().take(15) {
            *slot = u16::try_from(index + 1).unwrap();
        }
        lens[15] = 15;

        // The sentinel C's `code` array holds before the call. Any entry still equal to
        // it afterwards is one the build did not write.
        let sentinel = code {
            op: 0xaa,
            bits: 0xaa,
            val: 0xaaaa,
        };
        let expected: [(c_uint, &[code]); 3] = [
            (
                1,
                &[code {
                    op: 16,
                    bits: 1,
                    val: 1,
                }],
            ),
            (
                2,
                &[
                    code {
                        op: 16,
                        bits: 1,
                        val: 1,
                    },
                    code {
                        op: 16,
                        bits: 2,
                        val: 2,
                    },
                    code {
                        op: 16,
                        bits: 1,
                        val: 1,
                    },
                ],
            ),
            (15, &[]),
        ];

        for (requested, written) in expected {
            let mut work = [0_u16; 16];
            let mut table = vec![sentinel; ENOUGH_DISTS];
            let mut next: *mut code = table.as_mut_ptr();
            let mut bits: c_uint = requested;
            // SAFETY: as the test above -- three distinct arrays, each at least as long
            // as the call requires, and two live in/out locals.
            let ret = unsafe {
                _zlib_rs_inflate_table(
                    DISTS,
                    lens.as_mut_ptr().cast::<c_ushort>(),
                    16,
                    &mut next,
                    &mut bits,
                    work.as_mut_ptr().cast::<c_ushort>(),
                )
            };
            assert_eq!(ret, 1, "root {requested}");
            assert_eq!(next, table.as_mut_ptr(), "root {requested}: *table is C's");
            assert_eq!(bits, requested, "root {requested}: *bits is C's");
            assert_eq!(
                &table[..written.len()],
                written,
                "root {requested}: the entries C writes must be published"
            );
            assert!(
                table[written.len()..]
                    .iter()
                    .all(|entry| *entry == sentinel),
                "root {requested}: no entry beyond C's may be touched"
            );
        }
    }

    // GATED ON THE SHIM ACTUALLY BEING BUILT, which is not the same condition as
    // `libz-compat`.  `build.rs` compiles `csrc/inftrees_shim.c` and
    // `csrc/gzprintf_shim.c` only when BOTH `libz-compat` and `gz` are on -- the
    // gzprintf half reaches `_zlib_rs_gzprintf_begin`/`_commit`, which live behind
    // `gz`, so archiving the pair against a build without it would archive undefined
    // symbols -- and it emits `cfg(zlib_rs_gzprintf)` when it has done so.  Naming
    // `inflate_table` unconditionally therefore made the `--no-default-features
    // --features libz-compat` configuration fail to LINK its test binary, with
    // `rust-lld: error: undefined symbol: inflate_table`, which nothing built until
    // the feature matrix job did.  The cfg is the honest gate: it says "the shim
    // archive is in this link", which is exactly what this test needs.
    #[test]
    #[cfg(zlib_rs_gzprintf)]
    fn the_c_prototype_shim_is_linked_and_agrees_with_the_rust_half() {
        // ★ F4's proof, and it is a LINK first: `csrc/inftrees_shim.c` defines
        // `inflate_table` with `inftrees.h`'s own `codetype` prototype, which is the
        // declaration the unmodified `test/infcover.c` compiles against. Naming the
        // symbol here means this test binary cannot be produced unless `build.rs`
        // compiled and archived that translation unit.
        //
        // The Rust declaration below deliberately spells the first parameter `c_uint`,
        // because that is what GCC and Clang choose for an all-non-negative C enum, and
        // it is ABI-identical to the `c_int` the shim widens it to. This is a *test*
        // calling a C function, not the contract: the contract is the C prototype in
        // the shim, and no Rust spelling of it exists.
        extern "C" {
            fn inflate_table(
                type_: c_uint,
                lens: *mut c_ushort,
                codes: c_uint,
                table: *mut *mut code,
                bits: *mut c_uint,
                work: *mut c_ushort,
            ) -> c_int;
        }

        // `test/infcover.c` L617-L639 again, run through the C entry point this time,
        // so that the two halves are compared on the input the acceptance suite uses.
        let mut lens = [0_u16; 16];
        for (index, slot) in lens.iter_mut().enumerate().take(15) {
            *slot = u16::try_from(index + 1).unwrap();
        }
        lens[15] = 15;

        for requested in [15_u32, 1] {
            let mut through_c = vec![code::default(); ENOUGH_DISTS];
            let mut through_rust = vec![code::default(); ENOUGH_DISTS];
            let mut work_c = [0xa5a5_u16; 16];
            let mut work_rust = [0xa5a5_u16; 16];
            let mut next_c: *mut code = through_c.as_mut_ptr();
            let mut next_rust: *mut code = through_rust.as_mut_ptr();
            let mut bits_c: c_uint = requested;
            let mut bits_rust: c_uint = requested;

            // SAFETY: every argument is a live local of the size the prototype
            // requires -- sixteen `u16`s of `lens` and `work`, `ENOUGH_DISTS` entries
            // of table -- and the arrays are distinct allocations.
            let (from_c, from_rust) = unsafe {
                (
                    inflate_table(
                        2, // DISTS
                        lens.as_mut_ptr().cast::<c_ushort>(),
                        16,
                        &mut next_c,
                        &mut bits_c,
                        work_c.as_mut_ptr().cast::<c_ushort>(),
                    ),
                    _zlib_rs_inflate_table(
                        DISTS,
                        lens.as_mut_ptr().cast::<c_ushort>(),
                        16,
                        &mut next_rust,
                        &mut bits_rust,
                        work_rust.as_mut_ptr().cast::<c_ushort>(),
                    ),
                )
            };

            assert_eq!(from_c, from_rust, "status must agree");
            assert_eq!(from_c, 1, "requested root width {requested} must overflow");
            assert_eq!(next_c, through_c.as_mut_ptr());
            assert_eq!(next_rust, through_rust.as_mut_ptr());
            assert_eq!(bits_c, requested);
            assert_eq!(bits_rust, requested);
            assert_eq!(work_c, work_rust, "the scratch footprint must agree");
            for (through_c, through_rust) in through_c.iter().zip(through_rust.iter()) {
                assert_eq!(
                    (through_c.op, through_c.bits, through_c.val),
                    (through_rust.op, through_rust.bits, through_rust.val),
                );
            }
        }
    }

    #[test]
    fn an_early_failure_leaves_the_scratch_array_and_both_parameters_alone() {
        // ★ F5. `inftrees.c` writes `work[]` only in the sort loop at L154-L156, which
        // it reaches only after the over-subscribed test at L139-L143 and the
        // incomplete test at L145-L146 have passed. Both of those `return -1` sites,
        // and the no-symbols return at L126-L134, leave the caller's scratch array
        // exactly as it arrived -- and leave `*bits` and `*table` holding the values
        // they were passed.
        //
        // The sentinel is `0xa5a5`, the value `test/infcover.c`'s allocator fills with
        // (its L87), for the same reason it chose it: zero would hide a wrapper that
        // zeroed the array.
        const SENTINEL: u16 = 0xa5a5;
        let mut table = vec![code::default(); ENOUGH_LENS];

        // Over-subscribed: three one-bit codes cannot exist.
        let mut over = [1_u16, 1, 1];
        // Incomplete, and not the single-one-bit-code exception, so `-1` as well.
        let mut incomplete = [2_u16, 3];
        // No symbol has a length at all, which is the L126-L134 success path: it writes
        // two table entries and sets `*bits` to 1, and still does not touch `work`.
        let mut empty = [0_u16; 4];

        for (label, lens, expected) in [
            ("over-subscribed", &mut over[..], TABLE_INVALID_CODE),
            ("incomplete", &mut incomplete[..], TABLE_INVALID_CODE),
            ("no symbols", &mut empty[..], TABLE_OK),
        ] {
            let codes = c_uint::try_from(lens.len()).unwrap();
            let mut work = [SENTINEL; 8];
            let mut next: *mut code = table.as_mut_ptr();
            let mut bits: c_uint = 9;
            // SAFETY: `lens` covers `codes` elements, `work` is eight `u16`s and so
            // covers every element the reference could sort, `table` holds
            // `ENOUGH_LENS` entries, and all three are distinct allocations.
            let outcome = unsafe {
                _zlib_rs_inflate_table(
                    LENS,
                    lens.as_mut_ptr().cast::<c_ushort>(),
                    codes,
                    &mut next,
                    &mut bits,
                    work.as_mut_ptr().cast::<c_ushort>(),
                )
            };
            assert_eq!(outcome, expected, "{label}: status");
            assert!(
                work.iter().all(|&slot| slot == SENTINEL),
                "{label}: the caller's scratch array must be untouched, got {work:?}"
            );
            if expected == TABLE_OK {
                // The one path that does write both parameters, and the values are the
                // reference's: two entries consumed and a root width of one.
                assert_eq!(bits, 1, "{label}: *bits");
                // SAFETY: `table` holds `ENOUGH_LENS` entries, so two past its base is
                // in bounds.
                let two_in = unsafe { table.as_mut_ptr().add(2) };
                assert_eq!(next, two_in, "{label}: *table");
            } else {
                assert_eq!(bits, 9, "{label}: *bits must keep its entry value");
                assert_eq!(
                    next,
                    table.as_mut_ptr(),
                    "{label}: *table must keep its entry value"
                );
            }
        }
    }

    #[test]
    fn a_successful_build_publishes_exactly_the_symbols_the_reference_sorts() {
        // The complement of the test above: on a path that does sort, the elements the
        // reference writes must appear and the ones it does not must survive. A
        // three-symbol code over four slots leaves the fourth untouched, because
        // `lens[3] == 0` means symbol 3 is not coded (`inftrees.c` L156's `if`).
        const SENTINEL: u16 = 0xa5a5;
        let mut lens = [1_u16, 2, 2, 0];
        let mut work = [SENTINEL; 4];
        let mut table = vec![code::default(); ENOUGH_LENS];
        let mut next: *mut code = table.as_mut_ptr();
        let mut bits: c_uint = 7;

        // SAFETY: as the test above; `lens` and `work` are four `u16`s each.
        let outcome = unsafe {
            _zlib_rs_inflate_table(
                CODES,
                lens.as_mut_ptr().cast::<c_ushort>(),
                4,
                &mut next,
                &mut bits,
                work.as_mut_ptr().cast::<c_ushort>(),
            )
        };
        assert_eq!(outcome, TABLE_OK);
        // Sorted by length, then by symbol within a length: symbol 0 is the one-bit
        // code, symbols 1 and 2 are the two-bit codes.
        assert_eq!(work[0], 0);
        assert_eq!(work[1], 1);
        assert_eq!(work[2], 2);
        assert_eq!(
            work[3], SENTINEL,
            "an uncoded symbol contributes no scratch element"
        );
        assert_eq!(bits, 2, "the root is clamped to the code set's maximum");
    }

    #[test]
    fn inflate_table_builds_a_valid_set_and_rejects_a_broken_one() {
        let mut work = [0_u16; 32];
        let mut table = vec![code::default(); 1444];

        // A complete two-symbol code: both symbols one bit long.
        let mut lens = [1_u16, 1];
        let mut next: *mut code = table.as_mut_ptr();
        let mut bits: c_uint = 7;
        // SAFETY: `lens` and `work` cover the two codes requested, and `table` is
        // larger than either `ENOUGH_*` bound.
        let ret = unsafe {
            _zlib_rs_inflate_table(
                CODES,
                lens.as_mut_ptr().cast::<c_ushort>(),
                2,
                &mut next,
                &mut bits,
                work.as_mut_ptr().cast::<c_ushort>(),
            )
        };
        assert_eq!(ret, 0, "a complete code set builds");
        assert_eq!(bits, 1, "the root width narrows to the longest code");
        // SAFETY: `next` was advanced within `table`.
        let produced = unsafe { next.offset_from(table.as_mut_ptr()) };
        assert_eq!(produced, 2, "two entries for a two-symbol root table");
        assert_eq!(table[0].bits, 1);
        assert_eq!(table[1].bits, 1);

        // An over-subscribed set: three one-bit codes.
        let mut over = [1_u16, 1, 1];
        let mut next: *mut code = table.as_mut_ptr();
        let mut bits: c_uint = 7;
        // SAFETY: as above.
        let ret = unsafe {
            _zlib_rs_inflate_table(
                LENS,
                over.as_mut_ptr().cast::<c_ushort>(),
                3,
                &mut next,
                &mut bits,
                work.as_mut_ptr().cast::<c_ushort>(),
            )
        };
        assert_eq!(ret, -1, "an over-subscribed set is invalid");
        assert_eq!(next, table.as_mut_ptr());

        // No symbols at all: C makes a two-entry table that is guaranteed to fail at
        // decode time and reports success (`inftrees.c` L126-L134).
        let mut none = [0_u16; 4];
        let mut next: *mut code = table.as_mut_ptr();
        let mut bits: c_uint = 9;
        // SAFETY: as above.
        let ret = unsafe {
            _zlib_rs_inflate_table(
                LENS,
                none.as_mut_ptr().cast::<c_ushort>(),
                4,
                &mut next,
                &mut bits,
                work.as_mut_ptr().cast::<c_ushort>(),
            )
        };
        assert_eq!(ret, 0);
        assert_eq!(bits, 1);
        // SAFETY: `next` was advanced within `table`.
        assert_eq!(unsafe { next.offset_from(table.as_mut_ptr()) }, 2);
        assert_eq!(table[0].op, 64, "an invalid-code marker");
        assert_eq!(table[1].op, 64);
    }

    #[test]
    fn inflate_table_refuses_a_bad_code_type_or_a_null_pointer() {
        let mut lens = [1_u16, 1];
        let mut work = [0_u16; 2];
        let mut table = vec![code::default(); 1444];
        let mut base: *mut code = table.as_mut_ptr();
        let mut bits: c_uint = 7;

        // SAFETY: every non-null argument is live; the nulls are the point of each
        // call and each is tested before use.
        unsafe {
            for bad_type in [-1, 3, 4, c_int::MAX, c_int::MIN] {
                assert_eq!(
                    _zlib_rs_inflate_table(
                        bad_type,
                        lens.as_mut_ptr().cast::<c_ushort>(),
                        2,
                        &mut base,
                        &mut bits,
                        work.as_mut_ptr().cast::<c_ushort>()
                    ),
                    -1,
                    "codetype {bad_type}"
                );
            }
            assert_eq!(
                _zlib_rs_inflate_table(
                    CODES,
                    core::ptr::null_mut(),
                    2,
                    &mut base,
                    &mut bits,
                    work.as_mut_ptr().cast::<c_ushort>()
                ),
                -1
            );
            assert_eq!(
                _zlib_rs_inflate_table(
                    CODES,
                    lens.as_mut_ptr().cast::<c_ushort>(),
                    2,
                    core::ptr::null_mut(),
                    &mut bits,
                    work.as_mut_ptr().cast::<c_ushort>()
                ),
                -1
            );
            assert_eq!(
                _zlib_rs_inflate_table(
                    CODES,
                    lens.as_mut_ptr().cast::<c_ushort>(),
                    2,
                    &mut base,
                    core::ptr::null_mut(),
                    work.as_mut_ptr().cast::<c_ushort>()
                ),
                -1
            );
            assert_eq!(
                _zlib_rs_inflate_table(
                    CODES,
                    lens.as_mut_ptr().cast::<c_ushort>(),
                    2,
                    &mut base,
                    &mut bits,
                    core::ptr::null_mut()
                ),
                -1
            );
            // A null *inside* `table` is refused too.
            let mut null_base: *mut code = core::ptr::null_mut();
            assert_eq!(
                _zlib_rs_inflate_table(
                    CODES,
                    lens.as_mut_ptr().cast::<c_ushort>(),
                    2,
                    &mut null_base,
                    &mut bits,
                    work.as_mut_ptr().cast::<c_ushort>()
                ),
                -1
            );
        }
        // Nothing above may have disturbed the caller's cursors.
        assert_eq!(base, table.as_mut_ptr());
        assert_eq!(bits, 7);
        assert_eq!(lens, [1, 1]);
    }

    // -----------------------------------------------------------------------
    // Width conversions
    // -----------------------------------------------------------------------

    #[test]
    fn the_width_conversions_round_trip_within_uLong() {
        for value in [0_u64, 1, 0xffff, 0xffff_ffff] {
            assert_eq!(widen_uLong(narrow_uLong(value)), value, "value {value}");
        }
        // The widest value `uLong` can hold, whatever its width on this target.
        let widest = widen_uLong(c_ulong::MAX);
        assert_eq!(widen_uLong(narrow_uLong(widest)), widest);
        assert_eq!(narrow_uLong(INFLATE_CODES_USED_BAD_STATE), c_ulong::MAX);
        assert_eq!(BAD_STATE_MARK, -(1 << 16));
    }

    // -----------------------------------------------------------------------
    // Argument-direction and buffer-aliasing discipline
    // -----------------------------------------------------------------------

    /// A non-null pointer that must never be dereferenced.
    ///
    /// ★ What the guard-ordering assertions below are built on. Where C returns before
    /// it reads through an argument, a caller is entitled to pass a pointer that is
    /// stale, misaligned or wild and still be told `Z_STREAM_ERROR`; reading through
    /// this address would fault, and under Miri it is caught as the invalid dereference
    /// it is.
    ///
    /// An ordinary integer-to-pointer cast rather than
    /// `core::ptr::without_provenance_mut`, which is `strict_provenance` and so was
    /// stabilised in Rust 1.84 -- past this workspace's 1.80 floor.
    fn wild<T>() -> *mut T {
        0xdead_0001_usize as *mut T
    }

    /// A gzip member carrying every optional field, so a header parse has work to do.
    ///
    /// FLG = `FEXTRA | FNAME | FCOMMENT`, MTIME = `0x4433_2211`, XFL = 2, OS = 3, then a
    /// two-byte extra field, a name, a comment, an empty final stored block and the
    /// eight-byte trailer for an empty payload.
    fn gzip_with_every_field() -> Vec<u8> {
        let mut gz: Vec<u8> = vec![0x1f, 0x8b, 0x08, 0x1c, 0x11, 0x22, 0x33, 0x44, 0x02, 0x03];
        gz.extend_from_slice(&[0x02, 0x00, 0xab, 0xcd]);
        gz.extend_from_slice(b"name\0");
        gz.extend_from_slice(b"comment\0");
        gz.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
        gz.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        gz.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        gz
    }

    #[test]
    fn a_header_whose_output_members_are_uninitialised_is_never_read() {
        // ★ `test/infcover.c` L290-L309 declares `gz_header head;` on the stack and
        // assigns only `extra`, `extra_max`, `name`, `name_max`, `comment` and
        // `comm_max`. `text`, `time`, `xflags`, `os`, `extra_len`, `hcrc` and `done`
        // hold indeterminate bytes, and C never reads any of them -- `inflate.c` L1229
        // writes `done` and the header states write the rest. This reproduces that
        // caller exactly: the six input members are written through raw places and the
        // other seven are left uninitialised, so any read of one is undefined behaviour
        // that Miri reports.
        let mut extra = [0_u8; 8];
        let mut name = [0_u8; 8];
        let mut comment = [0_u8; 16];
        let mut storage = MaybeUninit::<gz_header>::uninit();
        let head: gz_headerp = storage.as_mut_ptr();
        // SAFETY: `storage` is live, aligned and large enough for a `gz_header`. Each
        // write goes through a raw place, so no reference to a partly initialised
        // structure is formed and no other member is read.
        unsafe {
            core::ptr::addr_of_mut!((*head).extra).write(extra.as_mut_ptr());
            core::ptr::addr_of_mut!((*head).extra_max).write(8);
            core::ptr::addr_of_mut!((*head).name).write(name.as_mut_ptr());
            core::ptr::addr_of_mut!((*head).name_max).write(8);
            core::ptr::addr_of_mut!((*head).comment).write(comment.as_mut_ptr());
            core::ptr::addr_of_mut!((*head).comm_max).write(16);
        }

        let gz = gzip_with_every_field();
        let mut strm = blank_stream();
        init(&mut strm, 47);
        assert_eq!(
            // SAFETY: the stream is initialised and `head` is a live, aligned `gz_header`
            // whose three buffers are live for the capacities given.
            unsafe { inflateGetHeader(&mut strm, head) },
            ReturnCode::OK.as_i32()
        );
        // L1229 writes `done` and nothing else, so this is now the one member of the
        // seven that may be read.
        // SAFETY: the call above assigned it.
        assert_eq!(unsafe { core::ptr::addr_of!((*head).done).read() }, 0);

        let mut out = [0_u8; 32];
        strm.next_in = gz.as_ptr();
        strm.avail_in = uInt::try_from(gz.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 32;
        // SAFETY: every region is live and disjoint and the stream is initialised.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert!(
            ret == ReturnCode::OK.as_i32() || ret == ReturnCode::STREAM_END.as_i32(),
            "gzip decode gave {ret}"
        );

        // Every member the parse assigns has now been written, so all seven are
        // initialised and may be read back.
        // SAFETY: the decode above published each of these.
        unsafe {
            assert_eq!(core::ptr::addr_of!((*head).done).read(), 1);
            assert_eq!(core::ptr::addr_of!((*head).text).read(), 0);
            assert_eq!(core::ptr::addr_of!((*head).time).read(), 0x4433_2211);
            assert_eq!(core::ptr::addr_of!((*head).xflags).read(), 0x02);
            assert_eq!(core::ptr::addr_of!((*head).os).read(), 0x03);
            assert_eq!(core::ptr::addr_of!((*head).extra_len).read(), 2);
            assert_eq!(core::ptr::addr_of!((*head).hcrc).read(), 0);
        }
        assert_eq!(&extra[..2], &[0xab, 0xcd]);
        assert_eq!(&name[..5], b"name\0");
        assert_eq!(&comment[..8], b"comment\0");

        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    #[test]
    fn a_null_field_whose_capacity_is_uninitialised_is_never_read() {
        // ★ `zlib.h` L125, L127 and L129 make each capacity meaningful only when its own
        // pointer is non-null -- `extra_max` is "the space at extra" -- so a caller that
        // wants only the name may pass `Z_NULL` for `extra` and `comment` and leave
        // `extra_max` and `comm_max` untouched. `inflate.c` reads a capacity only inside
        // the state that writes through the matching pointer (L613-L621, L639-L642,
        // L661-L664), so it never touches the two here.
        //
        // This is the caller the previous test does not cover: there every input member
        // was assigned, so an unconditional read of all six was indistinguishable from a
        // branched one. Here two of them are indeterminate, and the instrument for the
        // read itself is Miri, which reports
        // "using uninitialized data, but this operation requires initialized memory" for
        // an unbranched `addr_of!((*head).extra_max).read()`. What this test asserts
        // without Miri is the visible half: the parse still fills `name`, and the two
        // absent fields are simply not collected.
        let mut name = [0_u8; 8];
        let mut storage = MaybeUninit::<gz_header>::uninit();
        let head: gz_headerp = storage.as_mut_ptr();
        // SAFETY: `storage` is live, aligned and large enough for a `gz_header`. Each
        // write goes through a raw place, so no reference to a partly initialised
        // structure is formed and no other member is read. `extra_max` and `comm_max` are
        // deliberately left uninitialised, which is the point of the test.
        unsafe {
            core::ptr::addr_of_mut!((*head).extra).write(core::ptr::null_mut());
            core::ptr::addr_of_mut!((*head).name).write(name.as_mut_ptr());
            core::ptr::addr_of_mut!((*head).name_max).write(8);
            core::ptr::addr_of_mut!((*head).comment).write(core::ptr::null_mut());
        }

        let gz = gzip_with_every_field();
        let mut strm = blank_stream();
        init(&mut strm, 47);
        assert_eq!(
            // SAFETY: the stream is initialised and `head` is a live, aligned `gz_header`
            // whose only non-null buffer is live for the capacity given.
            unsafe { inflateGetHeader(&mut strm, head) },
            ReturnCode::OK.as_i32()
        );

        let mut out = [0_u8; 32];
        strm.next_in = gz.as_ptr();
        strm.avail_in = uInt::try_from(gz.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 32;
        // SAFETY: every region is live and disjoint and the stream is initialised.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert!(
            ret == ReturnCode::OK.as_i32() || ret == ReturnCode::STREAM_END.as_i32(),
            "gzip decode gave {ret}"
        );

        // The name arrived through the one buffer that was supplied, and `extra_len` is
        // still published because the stream carries it whether or not `extra` was.
        // SAFETY: the decode above published both members.
        unsafe {
            assert_eq!(core::ptr::addr_of!((*head).done).read(), 1);
            assert_eq!(core::ptr::addr_of!((*head).extra_len).read(), 2);
        }
        assert_eq!(&name[..5], b"name\0");

        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    #[test]
    fn a_completed_header_releases_the_callers_structure_and_buffers() {
        // ★ `zlib.h` L1075-L1084 asks the application to keep the `gz_header` and its
        // three buffers available while `inflate()` is reading the header. Once `done`
        // is terminal the parse will never write another member, so a conforming caller
        // may free all four -- and a library still holding the raw pointer or the three
        // borrows would then be holding dangling ones. Both are dropped, which is what
        // lets this test free everything and keep decoding.
        let gz = gzip_with_every_field();
        let mut strm = blank_stream();
        init(&mut strm, 47);

        let mut out = [0_u8; 32];
        let done = {
            // Heap storage, so dropping it really returns the pages.
            let mut extra: Vec<u8> = vec![0; 8];
            let mut name: Vec<u8> = vec![0; 8];
            let mut comment: Vec<u8> = vec![0; 16];
            let mut head = gz_header {
                text: 0,
                time: 0,
                xflags: 0,
                os: 0,
                extra: extra.as_mut_ptr(),
                extra_len: 0,
                extra_max: 8,
                name: name.as_mut_ptr(),
                name_max: 8,
                comment: comment.as_mut_ptr(),
                comm_max: 16,
                hcrc: 0,
                done: 0,
            };
            assert_eq!(
                // SAFETY: the stream is initialised and `head` is live with live buffers.
                unsafe { inflateGetHeader(&mut strm, core::ptr::addr_of_mut!(head)) },
                ReturnCode::OK.as_i32()
            );

            // Feed only the header, so the decode stops after it.
            strm.next_in = gz.as_ptr();
            strm.avail_in = uInt::try_from(gz.len()).unwrap();
            strm.next_out = out.as_mut_ptr();
            strm.avail_out = 32;
            // SAFETY: every region is live and disjoint.
            let ret = unsafe { inflate(&mut strm, 0) };
            assert!(
                ret == ReturnCode::OK.as_i32() || ret == ReturnCode::STREAM_END.as_i32(),
                "gzip decode gave {ret}"
            );
            assert_eq!(head.done, 1, "the header must be complete");
            assert_eq!(&name[..5], b"name\0");
            head.done
            // `head`, `extra`, `name` and `comment` are all dropped here.
        };
        assert_eq!(done, 1);

        // Nothing below may touch the freed structure or its freed buffers. A retained
        // pointer or borrow would make this a use-after-free, which Miri reports.
        strm.next_in = gz.as_ptr();
        strm.avail_in = uInt::try_from(gz.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 32;
        // SAFETY: the two regions are live and disjoint and the stream is initialised.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert!(
            ret == ReturnCode::STREAM_END.as_i32()
                || ret == ReturnCode::OK.as_i32()
                || ret == ReturnCode::BUF_ERROR.as_i32()
                || ret == ReturnCode::DATA_ERROR.as_i32(),
            "the second call gave {ret}"
        );
        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    /// The heap form of the case above, for a **gzip** stream: the caller releases
    /// the whole `gz_header` allocation once `done == 1`, and decoding carries on.
    ///
    /// ★ The sibling test puts the structure on the stack, where a retained read is
    /// a `stack-use-after-scope`. This one puts it in its own heap allocation and
    /// really frees it, so the same retained read is a `heap-use-after-free` --
    /// AddressSanitizer's least ambiguous verdict, and the shape a real caller
    /// produces when it `free()`s the structure it `malloc()`ed. `zlib.h`
    /// L1075-L1084 permits exactly this, and `inflate.c` never revisits a header
    /// state, so the reference is unaffected by it; before the mode gate in
    /// [`Session::bind_header_fields`] this library read six members of the freed
    /// block on every later call, which is a `SIGSEGV` for a caller whose allocator
    /// returns the pages to the kernel.
    #[test]
    fn a_freed_gzip_header_is_never_read_again() {
        let gz = gzip_with_every_field();
        let mut strm = blank_stream();
        init(&mut strm, 47);

        let mut extra: Vec<u8> = vec![0; 8];
        let mut name: Vec<u8> = vec![0; 8];
        let mut comment: Vec<u8> = vec![0; 16];
        let head = Box::into_raw(Box::new(gz_header {
            text: 0,
            time: 0,
            xflags: 0,
            os: 0,
            extra: extra.as_mut_ptr(),
            extra_len: 0,
            extra_max: 8,
            name: name.as_mut_ptr(),
            name_max: 8,
            comment: comment.as_mut_ptr(),
            comm_max: 16,
            hcrc: 0,
            done: 0,
        }));
        assert_eq!(
            // SAFETY: the stream is initialised and `head` is a live, aligned
            // `gz_header` whose three buffers are live for the capacities given.
            unsafe { inflateGetHeader(&mut strm, head) },
            ReturnCode::OK.as_i32()
        );

        let mut out = [0_u8; 32];
        strm.next_in = gz.as_ptr();
        strm.avail_in = uInt::try_from(gz.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 32;
        // SAFETY: every region is live and disjoint and the stream is initialised.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert!(
            ret == ReturnCode::OK.as_i32() || ret == ReturnCode::STREAM_END.as_i32(),
            "the header decode gave {ret}"
        );
        // SAFETY: `head` is still live here, and `done` was published by the parse.
        let done = unsafe { core::ptr::addr_of!((*head).done).read() };
        assert_eq!(done, 1, "the header must be complete before the release");
        assert_eq!(&name[..5], b"name\0");

        // The caller now does what the documentation permits: it releases the header
        // allocation and the three buffers, and keeps decoding.
        // SAFETY: `head` came from `Box::into_raw` above and is released exactly once.
        drop(unsafe { Box::from_raw(head) });
        drop(extra);
        drop(name);
        drop(comment);

        strm.next_in = gz.as_ptr();
        strm.avail_in = uInt::try_from(gz.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 32;
        // SAFETY: the two regions are live and disjoint and the stream is initialised;
        // the freed header is deliberately not passed to anything.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert!(
            ret == ReturnCode::STREAM_END.as_i32()
                || ret == ReturnCode::OK.as_i32()
                || ret == ReturnCode::BUF_ERROR.as_i32()
                || ret == ReturnCode::DATA_ERROR.as_i32(),
            "the call after the release gave {ret}"
        );
        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    /// The same release, for a stream that turns out **not** to be gzip.
    ///
    /// ★ `inflate.c` L522-L523 writes `head->done = -1` from the `HEAD` state -- the
    /// documented "there will be no gzip header information forthcoming" answer
    /// (`zlib.h` L1096-L1099) -- and that is just as terminal as `done == 1`: the
    /// mode leaves for `DICTID` or `TYPE` and never comes back, so the caller may
    /// release the structure immediately. This is the second of the three paths the
    /// missing gate left reachable, and it is the one a caller hits when it installs
    /// a header speculatively on a `windowBits + 32` auto-detecting stream and feeds
    /// it a zlib stream.
    #[test]
    fn a_freed_header_is_never_read_again_after_a_zlib_stream_declines_it() {
        let zlib = deflate_zlib(b"a zlib stream, not a gzip one");
        let mut strm = blank_stream();
        init(&mut strm, 47);

        let head = Box::into_raw(Box::new(gz_header {
            text: 0,
            time: 0,
            xflags: 0,
            os: 0,
            extra: core::ptr::null_mut(),
            extra_len: 0,
            extra_max: 0,
            name: core::ptr::null_mut(),
            name_max: 0,
            comment: core::ptr::null_mut(),
            comm_max: 0,
            hcrc: 0,
            done: 0,
        }));
        assert_eq!(
            // SAFETY: the stream is initialised and `head` is a live, aligned
            // `gz_header` with no buffers supplied.
            unsafe { inflateGetHeader(&mut strm, head) },
            ReturnCode::OK.as_i32()
        );

        let mut out = [0_u8; 64];
        strm.next_in = zlib.as_ptr();
        strm.avail_in = uInt::try_from(zlib.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 64;
        // SAFETY: every region is live and disjoint and the stream is initialised.
        // `Z_BLOCK` stops at the first block boundary, so the header is settled but
        // the stream is not finished -- there is decoding left to do afterwards.
        let ret = unsafe { inflate(&mut strm, 5) };
        assert!(
            ret == ReturnCode::OK.as_i32() || ret == ReturnCode::STREAM_END.as_i32(),
            "the zlib decode gave {ret}"
        );
        // SAFETY: `head` is still live here.
        let done = unsafe { core::ptr::addr_of!((*head).done).read() };
        assert_eq!(
            done, -1,
            "a zlib stream must report that no gzip header is coming"
        );

        // Terminal, so the caller releases it.
        // SAFETY: `head` came from `Box::into_raw` above and is released exactly once.
        drop(unsafe { Box::from_raw(head) });

        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 64;
        // SAFETY: the output region is live and disjoint from the input, and the
        // stream is initialised; the freed header is deliberately not passed on.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert!(
            ret == ReturnCode::STREAM_END.as_i32() || ret == ReturnCode::OK.as_i32(),
            "the call after the release gave {ret}"
        );
        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    #[test]
    fn get_header_on_a_non_gzip_stream_never_reads_the_header() {
        // `inflate.c` L1225-L1226 returns before L1228-L1229 touch the structure, so a
        // zlib or raw stream never causes C to read or write one member of it.
        for window_bits in [15, -15] {
            let mut strm = blank_stream();
            init(&mut strm, window_bits);
            assert_eq!(
                // SAFETY: the stream is initialised; `head` is deliberately not
                // dereferenceable and the assertion is that it is not dereferenced.
                unsafe { inflateGetHeader(&mut strm, wild::<gz_header>()) },
                ReturnCode::STREAM_ERROR.as_i32(),
                "windowBits {window_bits} cannot accept a header"
            );
            // SAFETY: the stream is the initialised one.
            assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
        }
    }

    #[test]
    fn set_dictionary_in_the_wrong_state_never_reads_the_dictionary() {
        // `inflate.c` L1196-L1197 returns before `adler32` (L1200) or the window update
        // (L1211) has read a dictionary byte, so a wrapped stream that is not waiting
        // for one leaves the caller's buffer untouched -- `test/infcover.c` L362-L363.
        let mut strm = blank_stream();
        init(&mut strm, 15);
        assert_eq!(
            // SAFETY: the stream is initialised; the dictionary pointer must not be read,
            // and the assertion is that it is not.
            unsafe { inflateSetDictionary(&mut strm, wild::<Bytef>().cast_const(), 16) },
            ReturnCode::STREAM_ERROR.as_i32()
        );
        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    #[test]
    fn overlapping_input_and_output_are_served_from_a_snapshot() {
        // ★ C decodes with an overlapping input and output -- `inflate.c` L305-L326 just
        // `LOAD()`s both into locals -- so this must too, and it does it by snapshotting the
        // input before the mutable borrow of the output exists. The decode then reads the
        // bytes the buffer held on entry, so it succeeds even though the output lands on top
        // of them. Refusing the pair would be a divergence from C for no safety benefit; see
        // `AliasScratch`, and `deflate`'s matching test, which `test/example.c` forces.
        let compressed = deflate_zlib(b"overlap me");
        let mut buffer = vec![0x5a_u8; 256];
        buffer[..compressed.len()].copy_from_slice(&compressed);

        let mut strm = blank_stream();
        init(&mut strm, 15);
        strm.next_in = buffer.as_ptr();
        strm.avail_in = uInt::try_from(compressed.len()).unwrap();
        // The output window begins inside the input, so the two share bytes.
        strm.next_out = buffer[4..].as_mut_ptr();
        strm.avail_out = 128;

        assert_eq!(
            // SAFETY: the stream is initialised and both members address `buffer`, live for
            // their counts. Overlap is permitted.
            unsafe { inflate(&mut strm, 0) },
            ReturnCode::STREAM_END.as_i32(),
            "an overlapping pair must decode, not fail"
        );
        assert_eq!(
            strm.total_in,
            crate::uLong::try_from(compressed.len()).unwrap(),
            "every input byte must be consumed"
        );
        assert_eq!(strm.total_out, 10);
        assert_eq!(
            &buffer[4..14],
            b"overlap me",
            "and the plaintext must be the bytes the buffer held on entry"
        );
        assert_eq!(strm.avail_out, 118);

        // Adjacent is not overlapping: a split buffer must decode normally. The stream above
        // ran to `Z_STREAM_END`, so it has to be reset before it will decode a second member.
        // SAFETY: the stream is the initialised one.
        // SAFETY: `strm` is the live, initialised stream this test owns.
        let reset = unsafe { inflateReset(&mut strm) };
        assert_eq!(reset, ReturnCode::OK.as_i32());
        let mut split = vec![0_u8; 512];
        split[..compressed.len()].copy_from_slice(&compressed);
        let (input, output) = split.split_at_mut(compressed.len());
        strm.next_in = input.as_ptr();
        strm.avail_in = uInt::try_from(input.len()).unwrap();
        strm.next_out = output.as_mut_ptr();
        strm.avail_out = uInt::try_from(output.len()).unwrap();
        assert_eq!(
            // SAFETY: the two halves of one split are disjoint and live for their counts.
            unsafe { inflate(&mut strm, 0) },
            ReturnCode::STREAM_END.as_i32()
        );
        assert_eq!(&output[..10], b"overlap me");
        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    #[test]
    fn sync_never_touches_the_output_members() {
        // ★ `inflate.c` L1264-L1310 reads and writes `avail_in`, `next_in` and
        // `total_in` and lets its internal reset publish `msg`, `data_type` and
        // `adler`. It reads and writes neither `next_out` nor `avail_out`. So a caller
        // may leave them pointing anywhere at all -- including at memory it has since
        // freed -- and this must still work and leave both exactly as it found them.
        let mut payload = deflate_zlib(b"a stream to resynchronise");
        // Two full-flush points, so a sync can succeed.
        let mut stream: Vec<u8> = vec![0x78, 0x9c];
        stream.extend_from_slice(&[0x00, 0x00, 0x00, 0xff, 0xff]);
        stream.append(&mut payload);

        let mut strm = blank_stream();
        init(&mut strm, 15);
        strm.next_in = stream.as_ptr();
        strm.avail_in = uInt::try_from(stream.len()).unwrap();

        // A deliberately unusable output pair, which C would never look at.
        let sentinel = wild::<Bytef>();
        strm.next_out = sentinel;
        strm.avail_out = 4242;

        // SAFETY: the stream is initialised and its input region is live for
        // `avail_in`. Nothing is required of the output pair, which is the property
        // under test.
        let ret = unsafe { inflateSync(&mut strm) };
        assert_eq!(
            ret,
            ReturnCode::OK.as_i32(),
            "the flush point in the fixture must be found"
        );
        assert!(
            core::ptr::eq(strm.next_out, sentinel),
            "next_out must be exactly as the caller left it"
        );
        assert_eq!(
            strm.avail_out, 4242,
            "avail_out must be exactly as the caller left it"
        );
        // The members C *does* write are still published.
        assert!(strm.total_in > 0);
        assert!(strm.msg.is_null(), "the internal reset clears msg");

        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    #[test]
    fn copy_refuses_two_streams_that_partly_overlap() {
        // L1352 copies `sizeof(z_stream)` bytes with `zmemcpy`, which is defined only
        // for non-overlapping regions -- in C as much as in Rust. Two correctly aligned
        // `z_stream`s can still overlap partially, so a plain inequality test is not
        // enough and the whole extent has to be compared.
        let mut source = blank_stream();
        init(&mut source, 15);

        // A byte array wide enough for two overlapping `z_stream`-aligned views.
        let mut storage = vec![0_u64; (size_of::<z_stream>() / size_of::<u64>()) * 2];
        let base: *mut z_stream = storage.as_mut_ptr().cast();
        // One `u64` further along: aligned for `z_stream`, and overlapping it.
        // SAFETY: `storage` holds two `z_stream`s' worth of `u64`s, so an offset of one
        // element is inside the allocation and the result is aligned for `z_stream`.
        let shifted: *mut z_stream = unsafe { storage.as_mut_ptr().add(1) }.cast();

        // SAFETY: `source` is the initialised stream; `base` and `shifted` are live,
        // aligned and writable for a `z_stream`. Neither destination is written,
        // because the disjointness gate refuses both calls.
        unsafe {
            assert_eq!(
                inflateCopy(base, base),
                ReturnCode::STREAM_ERROR.as_i32(),
                "a stream copied onto itself is refused"
            );
            assert_eq!(
                inflateCopy(shifted, base),
                ReturnCode::STREAM_ERROR.as_i32(),
                "and so is a partial overlap"
            );
        }

        // A genuinely disjoint destination is accepted, so the gate is not simply
        // refusing everything.
        let mut dest = blank_stream();
        assert_eq!(
            // SAFETY: `dest` and `source` are two distinct live `z_stream`s and `source`
            // holds a state this module installed.
            unsafe { inflateCopy(&mut dest, &mut source) },
            ReturnCode::OK.as_i32()
        );
        // SAFETY: both streams now hold states this module installed.
        unsafe {
            assert_eq!(inflateEnd(&mut dest), ReturnCode::OK.as_i32());
            assert_eq!(inflateEnd(&mut source), ReturnCode::OK.as_i32());
        }
    }
}

#[cfg(test)]
// The workspace denies the panic family in library code, which is the property this
// module exists to uphold at the boundary. A test that cannot assert is useless, so
// the harness opts back in here only; `clippy.toml` grants exactly this with its
// `allow-unwrap-in-tests`, `allow-expect-in-tests` and `allow-panic-in-tests` keys.
// There is deliberately no `allow-indexing-slicing-in-tests` key -- it is newer than
// the declared 1.80 floor and an unrecognised `clippy.toml` field aborts the whole
// lint run -- so the slicing allowance is taken as a module-local attribute instead.
// `used_underscore_items` joins them because these tests call
// `_zlib_rs_inflate_table`, whose leading underscore is what `zlib.map`'s `local: _*;`
// pattern hides it by and `zlib.map` is immutable. Its real caller is
// `csrc/inftrees_shim.c`, which presents the `codetype` prototype no Rust caller can
// spell; these tests are the only Rust callers it will ever have. `unknown_lints`
// comes first because the lint postdates the declared 1.80 floor, where naming it
// would otherwise be a warning of its own -- the same guard
// `crates/zlib-rs/src/deflate/algorithm.rs` uses for `_tr_init`.
#[allow(
    unknown_lints,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used,
    clippy::used_underscore_items
)]
mod tests_backend {
    use super::{
        _zlib_rs_inflate_table, code, inflate, inflateCodesUsed, inflateCopy, inflateEnd,
        inflateGetDictionary, inflateGetHeader, inflateInit2_, inflateInit_, inflateMark,
        inflatePrime, inflateReset, inflateReset2, inflateResetKeep, inflateSetDictionary,
        inflateSync, inflateSyncPoint, inflateUndermine, inflateValidate, message_ptr,
        narrow_uLong, widen_uLong, z_stream, BAD_STATE_MARK, ENOUGH_DISTS, MAX_INFLATE_FLUSH,
        MAX_WINDOW_BYTES, MESSAGES,
    };
    use core::ffi::{c_char, c_int, c_uint, c_ulong, c_ushort, CStr};
    use core::mem::{align_of, offset_of, size_of, MaybeUninit};

    use crate::types::{gz_header, uInt, z_streamp, StatePrefix, CODES, DISTS, ENOUGH, LENS};
    use crate::util::ZLIB_VERSION;
    use zlib_rs::error::ReturnCode;
    use zlib_rs::inflate::{Mode, INFLATE_CODES_USED_BAD_STATE};

    /// The mode tag `test/infcover.c` L330 writes: `DICT`, i.e. 16190.
    const DICT_TAG: c_int = 16190;

    /// The mode tag `test/infcover.c` L458 writes: `SYNC`, i.e. 16211.
    const SYNC_TAG: c_int = 16211;

    /// A zeroed `z_stream`, which is what a C caller declares before initialising.
    ///
    /// `next_in` and `avail_in` are left null and zero deliberately: that pairing is
    /// legal and is exactly what `test/infcover.c` L392-L393 sets before every
    /// `inflateInit2`.
    fn blank_stream() -> z_stream {
        z_stream {
            next_in: core::ptr::null(),
            avail_in: 0,
            total_in: 0,
            next_out: core::ptr::null_mut(),
            avail_out: 0,
            total_out: 0,
            msg: core::ptr::null(),
            state: core::ptr::null_mut(),
            zalloc: None,
            zfree: None,
            opaque: core::ptr::null_mut(),
            data_type: 0,
            adler: 0,
            reserved: 0,
        }
    }

    /// The `version` and `stream_size` arguments the `inflateInit2` macro supplies.
    fn version_args() -> (*const c_char, c_int) {
        (
            ZLIB_VERSION.as_ptr(),
            c_int::try_from(size_of::<z_stream>()).unwrap(),
        )
    }

    /// Initialises a stream for `window_bits`, asserting success.
    fn init(strm: &mut z_stream, window_bits: c_int) {
        let (version, size) = version_args();
        // SAFETY: `strm` is a live, zeroed `z_stream` and the version pair is this
        // library's own.
        let ret = unsafe { inflateInit2_(strm, window_bits, version, size) };
        assert_eq!(ret, ReturnCode::OK.as_i32(), "inflateInit2_({window_bits})");
    }

    /// Compresses `data` with the core, so the decoder has a real stream to read.
    fn deflate_zlib(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0_u8; zlib_rs::compress::compress_bound(data.len()) + 64];
        let report = zlib_rs::compress::compress(&mut out, data);
        assert_eq!(report.code, ReturnCode::OK);
        out.truncate(report.produced);
        out
    }

    /// Returns a pointer to the C-visible mode tag behind `strm.state`.
    ///
    /// ★ This is the one thing about the state that is *deliberately* reachable from
    /// outside the library, because `test/infcover.c` includes the private
    /// `inflate.h` and writes through it -- `((struct inflate_state *)strm.state)
    /// ->mode = DICT` at its L330 and `state->mode = SYNC` at its L458. The Rust
    /// spelling of that cast is `StatePrefix`, whose `#[repr(C)]` layout places a
    /// `z_streamp` at offset 0 and the `int` tag at offset 8 -- exactly where
    /// `deflate.h` L105-L106 and `inflate.h` L83-L84 place theirs.
    /// An allocator that hands out blocks until a chosen request, then refuses.
    ///
    /// Smaller than `test/infcover.c`'s zone -- it counts rather than tracks, because
    /// what these tests need is a *named* allocation to fail, not a leak report -- but it
    /// keeps the live count so a leak is still visible.
    #[derive(Default)]
    struct Refuse {
        /// How many requests have been made.
        requests: usize,
        /// Refuse this request and every later one, counting from one; zero refuses
        /// nothing.
        deny_from: usize,
        /// The outstanding blocks, as (address, bytes) so that each can be returned to
        /// Rust's allocator with the layout it was taken with.
        blocks: Vec<(*mut u8, usize)>,
    }

    impl Refuse {
        /// How many blocks are outstanding.
        fn live(&self) -> usize {
            self.blocks.len()
        }
    }

    /// A [`Refuse`] zone on the heap, reached only through the one raw pointer it owns.
    ///
    /// ★ **A tracking zone cannot live in a local.** The pointer the test installs in
    /// `z_stream.opaque` is what the allocator hooks dereference, and a later write
    /// through the *local* -- `zone.deny_from = ...` -- is a write through the local's
    /// own tag, which invalidates every pointer derived from it. The next `zalloc` then
    /// dereferences a dead tag, and Miri rejects the test. A C caller has no such rule;
    /// this is a property of the harness, not of the library.
    ///
    /// So the zone is heap-allocated once, the raw pointer is the only handle, and every
    /// access -- the test's and the hooks' -- derives from that single pointer. That is
    /// also how a C caller holds its own zone, which is what makes the harness faithful
    /// rather than merely acceptable.
    struct Zone(*mut Refuse);

    impl Zone {
        /// Allocates a fresh zone that refuses nothing.
        fn new() -> Self {
            Self(Box::into_raw(Box::new(Refuse::default())))
        }

        /// The pointer to install as `z_stream.opaque`.
        fn as_opaque(&self) -> crate::types::voidpf {
            self.0.cast::<core::ffi::c_void>()
        }

        /// Borrows the zone through its one raw pointer, for the duration of `body`.
        fn with<R>(&self, body: impl FnOnce(&mut Refuse) -> R) -> R {
            // SAFETY: the pointer came from `Box::into_raw` in `Zone::new`, is live until
            // `Zone::drop`, and is aligned and unique. No other reference to the zone
            // exists while `body` runs: the library forms one only inside `refuse_alloc`
            // and `refuse_free`, and neither can be executing while this statement is.
            body(unsafe { &mut *self.0 })
        }
    }

    impl Drop for Zone {
        /// Releases the allocation, after asserting the zone leaked nothing.
        fn drop(&mut self) {
            // SAFETY: the pointer came from `Box::into_raw` in `Zone::new` and this is
            // the only place it is reclaimed, so it is reclaimed exactly once.
            let zone = unsafe { Box::from_raw(self.0) };
            assert_eq!(zone.live(), 0, "the zone must own nothing at the end");
        }
    }

    /// The `zalloc` half of [`Refuse`].
    unsafe extern "C" fn refuse_alloc(
        opaque: crate::types::voidpf,
        count: uInt,
        size: uInt,
    ) -> crate::types::voidpf {
        let zone = opaque.cast::<Refuse>();
        assert!(!zone.is_null(), "the zone is always supplied");
        // SAFETY: `opaque` is the `&mut Refuse` the test installed, and no other
        // reference to it exists while this call runs.
        let zone = unsafe { &mut *zone };
        zone.requests += 1;
        if zone.deny_from != 0 && zone.requests >= zone.deny_from {
            return core::ptr::null_mut();
        }
        let len = (count as usize) * (size as usize);
        let layout = core::alloc::Layout::from_size_align(len.max(1), 16).unwrap();
        // SAFETY: the layout is non-zero-sized and validly aligned.
        let ptr = unsafe { std::alloc::alloc(layout) };
        assert!(!ptr.is_null(), "the test allocator must not fail for real");
        // Never zeroed, as C's `malloc` is not.
        // SAFETY: `ptr` is a fresh allocation of `layout.size()` bytes.
        unsafe { core::ptr::write_bytes(ptr, 0xa5, layout.size()) };
        zone.blocks.push((ptr, layout.size()));
        ptr.cast()
    }

    /// The `zfree` half of [`Refuse`].
    ///
    /// Rust's allocator needs the layout a block was taken with, which C's `free` does
    /// not, so the size is looked up in [`Refuse::blocks`] -- and a free of an address the
    /// zone never handed out is a defect worth failing on, exactly as
    /// `test/infcover.c`'s `mem_free` treats one.
    unsafe extern "C" fn refuse_free(opaque: crate::types::voidpf, address: crate::types::voidpf) {
        let zone = opaque.cast::<Refuse>();
        assert!(!zone.is_null(), "the zone is always supplied");
        // SAFETY: as `refuse_alloc`.
        let zone = unsafe { &mut *zone };
        assert!(!address.is_null(), "no null is ever freed");
        let found = zone
            .blocks
            .iter()
            .position(|&(at, _)| core::ptr::eq(at.cast::<core::ffi::c_void>(), address));
        let index = found.expect("every free must be of a block this zone handed out");
        let (at, len) = zone.blocks.remove(index);
        let layout = core::alloc::Layout::from_size_align(len, 16).unwrap();
        // SAFETY: `at` came from `std::alloc::alloc` with this exact layout in
        // `refuse_alloc`, and it has just been removed from the live list, so it cannot be
        // released twice.
        unsafe { std::alloc::dealloc(at, layout) };
    }

    /// A `gz_header` on the heap, reached only through the raw pointer this returns.
    ///
    /// ★ A `gz_header` in a local, passed as `&mut head` and then written through the
    /// local again, is a Stacked Borrows violation *in the test*: the reference passed to
    /// `inflateGetHeader` is what the library's stored raw pointer derives from, and a
    /// later direct write to the local invalidates it, so the next `inflate` reading
    /// through it is undefined behaviour under Miri. A C caller has no such rule -- this
    /// is a property of the test harness, not of the library -- but a test that Miri
    /// rejects is a test that cannot be run, so every access here goes through one raw
    /// pointer with one provenance, which is also how a C caller holds it.
    ///
    /// The caller owns the allocation and must release it with [`release_header`].
    fn header_on_heap(head: gz_header) -> *mut gz_header {
        Box::into_raw(Box::new(head))
    }

    /// Releases a header [`header_on_heap`] produced.
    ///
    /// # Safety
    ///
    /// `head` must have come from [`header_on_heap`] and must not still be installed in a
    /// live stream.
    unsafe fn release_header(head: *mut gz_header) {
        // SAFETY: by this function's contract `head` came from `Box::into_raw` on a
        // `Box<gz_header>` and has not been released before.
        drop(unsafe { Box::from_raw(head) });
    }

    /// A `gz_header` with every member zeroed, as a caller that fills in only what it
    /// needs starts from.
    fn zeroed_header() -> gz_header {
        gz_header {
            text: 0,
            time: 0,
            xflags: 0,
            os: 0,
            extra: core::ptr::null_mut(),
            extra_len: 0,
            extra_max: 0,
            name: core::ptr::null_mut(),
            name_max: 0,
            comment: core::ptr::null_mut(),
            comm_max: 0,
            hcrc: 0,
            done: 0,
        }
    }

    fn mode_slot(strm: &z_stream) -> *mut c_int {
        let prefix = strm.state.cast::<StatePrefix>();
        assert!(!prefix.is_null(), "the stream must hold a state");
        // SAFETY: `strm.state` was installed by `inflateInit2_`, so it addresses a
        // `StateBlock<InflateSlot>` whose first member is a `StatePrefix`. Forming a
        // raw place for one member neither dereferences it nor creates a reference.
        unsafe { core::ptr::addr_of_mut!((*prefix).tag) }
    }

    /// Reads a `z_stream`'s `msg` back as a Rust string, or [`None`] for `Z_NULL`.
    fn msg_of(strm: &z_stream) -> Option<&'static str> {
        if strm.msg.is_null() {
            return None;
        }
        // SAFETY: every non-null `msg` this module publishes points at an entry of
        // `MESSAGES`, which is a `'static` NUL-terminated C string.
        unsafe { CStr::from_ptr(strm.msg) }.to_str().ok()
    }

    // -----------------------------------------------------------------------
    // The message table
    // -----------------------------------------------------------------------

    /// `inflate.c` L183-L195: an init with `Z_NULL` hooks writes the library's own
    /// routines into the caller's stream and clears `opaque`, and a stream in that
    /// state can be torn down through the hooks it now holds.
    ///
    /// This is what `zlib.h` L151-L153 promises -- if `zalloc` and `zfree` are set
    /// to `Z_NULL`, the init updates them to use default allocation functions -- and
    /// it is observable to any caller that reads its own structure back, or that
    /// hands the stream to `inflateCopy`, which copies all three members verbatim.
    #[test]
    fn an_init_with_null_hooks_publishes_the_librarys_own_routines() {
        let mut strm = blank_stream();
        // A non-null `opaque` the substitution must clear, and a message the init
        // must clear (`inflate.c` L182).
        strm.opaque = core::ptr::addr_of_mut!(strm.reserved).cast::<core::ffi::c_void>();
        strm.msg = c"stale".as_ptr();

        init(&mut strm, 15);

        assert!(strm.zalloc.is_some(), "inflate.c L184-L190 fills zalloc in");
        assert!(strm.zfree.is_some(), "inflate.c L191-L195 fills zfree in");
        assert!(
            strm.opaque.is_null(),
            "inflate.c L188 clears opaque with the zalloc substitution"
        );
        assert!(strm.msg.is_null(), "inflate.c L182 clears msg");
        assert!(!strm.state.is_null());

        // The published pair is what the teardown runs through, so a mismatch here
        // would be a rogue free rather than a wrong value.
        // SAFETY: `strm` is the live, stack-owned stream this test initialised, and this
        // is the call that releases its state, so nothing reads it afterwards.
        let ret = unsafe { inflateEnd(&mut strm) };
        assert_eq!(
            ret,
            ReturnCode::OK.as_i32(),
            "the state must be releasable through the hooks the init published"
        );
        assert!(strm.state.is_null());

        // And the reverse direction: with the members cleared again -- the state
        // `test/infcover.c`'s `mem_done` leaves (L231-L233) -- every non-init entry
        // point refuses the stream, which is `inflateStateCheck`'s first test.
        strm.zalloc = None;
        strm.zfree = None;
        // SAFETY: `strm` is still a live, stack-owned stream; its hooks were cleared, which
        // is a value the state check inspects rather than a pointer it follows.
        let ret = unsafe { inflateReset(&mut strm) };
        assert_eq!(ret, ReturnCode::STREAM_ERROR.as_i32());
    }

    #[test]
    fn the_message_table_is_consistent_and_fully_reachable() {
        assert_eq!(MESSAGES.len(), 18, "the core produces exactly 18 texts");
        for (index, entry) in MESSAGES.iter().enumerate() {
            let text = entry.to_str().unwrap();
            assert!(!text.is_empty(), "entry {index} is empty");
            assert_eq!(
                MESSAGES.iter().filter(|other| *other == entry).count(),
                1,
                "entry {index} ({text}) is duplicated"
            );
            let published = message_ptr(Some(text));
            assert!(!published.is_null(), "{text} is not recoverable");
            // SAFETY: `message_ptr` returned a pointer into `MESSAGES`, which holds
            // `'static` NUL-terminated strings.
            let round_tripped = unsafe { CStr::from_ptr(published) };
            assert_eq!(round_tripped, *entry);
        }
    }

    #[test]
    fn message_ptr_answers_null_for_no_message_and_for_an_unknown_one() {
        assert!(message_ptr(None).is_null());
        assert!(message_ptr(Some("not a message this library produces")).is_null());
        assert!(message_ptr(Some("")).is_null());
    }

    #[test]
    fn a_data_error_always_publishes_a_non_null_message() {
        // Six malformed streams, each reaching a different family of error text: a
        // bad zlib method, a bad zlib header check, a reserved block type, mismatched
        // stored-block lengths, a corrupt trailer, and an over-subscribed code set.
        let corpus: [&[u8]; 6] = [
            &[0x77, 0x85],
            &[0x78, 0x90],
            &[0x78, 0x9c, 0x07, 0x00],
            &[0x78, 0x9c, 0x01, 0x01, 0x00, 0x01, 0x00, 0x41],
            &[0x78, 0x9c, 0x63, 0x00, 0x00, 0x00, 0x01, 0x00, 0x02],
            &[0x78, 0x9c, 0xfd, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff],
        ];
        for (index, input) in corpus.iter().enumerate() {
            let mut strm = blank_stream();
            init(&mut strm, 15);
            let mut out = [0_u8; 64];
            strm.next_in = input.as_ptr();
            strm.avail_in = uInt::try_from(input.len()).unwrap();
            strm.next_out = out.as_mut_ptr();
            strm.avail_out = uInt::try_from(out.len()).unwrap();
            // SAFETY: the stream is initialised and both regions are live and
            // disjoint.
            let ret = unsafe { inflate(&mut strm, 0) };
            if ret == ReturnCode::DATA_ERROR.as_i32() {
                assert!(
                    !strm.msg.is_null(),
                    "corpus entry {index} reported Z_DATA_ERROR with a null msg, \
                     which means its text is missing from MESSAGES"
                );
                assert!(msg_of(&strm).is_some());
            }
            // SAFETY: the stream is still the initialised one.
            assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
        }
    }

    // -----------------------------------------------------------------------
    // Initialisation, version and stream-pointer guards
    // -----------------------------------------------------------------------

    #[test]
    fn a_null_stream_is_refused_by_every_entry_point() {
        let (version, size) = version_args();
        // SAFETY: a null `z_streamp` is a documented input; every guard precedes the
        // first dereference.
        unsafe {
            assert_eq!(
                inflateInit_(core::ptr::null_mut(), version, size),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(
                inflateInit2_(core::ptr::null_mut(), 15, version, size),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            // `test/infcover.c` L393-L395, verbatim.
            assert_eq!(
                inflate(core::ptr::null_mut(), 0),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(
                inflateEnd(core::ptr::null_mut()),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(
                inflateCopy(core::ptr::null_mut(), core::ptr::null_mut()),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            for answer in [
                inflateReset(core::ptr::null_mut()),
                inflateResetKeep(core::ptr::null_mut()),
                inflateReset2(core::ptr::null_mut(), 15),
                inflatePrime(core::ptr::null_mut(), 1, 0),
                inflateSync(core::ptr::null_mut()),
                inflateSyncPoint(core::ptr::null_mut()),
                inflateUndermine(core::ptr::null_mut(), 1),
                inflateValidate(core::ptr::null_mut(), 1),
                inflateGetHeader(core::ptr::null_mut(), core::ptr::null_mut()),
                inflateSetDictionary(core::ptr::null_mut(), core::ptr::null(), 0),
                inflateGetDictionary(
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                    core::ptr::null_mut(),
                ),
            ] {
                assert_eq!(answer, ReturnCode::STREAM_ERROR.as_i32());
            }
            // ★ These two do NOT report `Z_STREAM_ERROR`: their return types are not
            // status codes.
            assert_eq!(inflateMark(core::ptr::null_mut()), BAD_STATE_MARK);
            assert_eq!(
                inflateCodesUsed(core::ptr::null_mut()),
                narrow_uLong(INFLATE_CODES_USED_BAD_STATE)
            );
        }
    }

    #[test]
    fn a_version_or_size_mismatch_is_reported_before_the_null_stream_test() {
        let (version, size) = version_args();
        let mut strm = blank_stream();
        // SAFETY: `strm` is a live zeroed stream; the version arguments are
        // deliberately wrong.
        unsafe {
            // `test/infcover.c` L376-L377 passes "!" and expects Z_VERSION_ERROR.
            assert_eq!(
                inflateInit_(&mut strm, c"!".as_ptr(), size),
                ReturnCode::VERSION_ERROR.as_i32()
            );
            assert_eq!(
                inflateInit_(&mut strm, core::ptr::null(), size),
                ReturnCode::VERSION_ERROR.as_i32()
            );
            assert_eq!(
                inflateInit_(&mut strm, version, size - 1),
                ReturnCode::VERSION_ERROR.as_i32()
            );
            // ★ The version test precedes the `strm == Z_NULL` test, so a null stream
            // with a bad version reports the version error.
            assert_eq!(
                inflateInit_(core::ptr::null_mut(), c"9".as_ptr(), size),
                ReturnCode::VERSION_ERROR.as_i32()
            );
        }
        // A rejected init must leave the stream unusable rather than half-built.
        assert!(strm.state.is_null());
        // A same-major, different-minor version is accepted, because only the first
        // character is compared.
        assert_eq!(ZLIB_VERSION.to_bytes().first(), Some(&b'1'));
        // SAFETY: as above.
        unsafe {
            assert_eq!(
                inflateInit_(&mut strm, c"1.2.11".as_ptr(), size),
                ReturnCode::OK.as_i32()
            );
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
    }

    #[test]
    fn an_initialised_zlib_stream_reports_the_adler_seed_before_decoding() {
        // `inflateResetKeep` sets `strm->adler = state->wrap & 1` (`inflate.c`
        // L107-L108), so a wrapped stream carries 1 and a raw stream carries 0.
        let mut wrapped = blank_stream();
        wrapped.adler = 0xdead_beef;
        init(&mut wrapped, 15);
        assert_eq!(wrapped.adler, 1);
        assert_eq!(wrapped.total_in, 0);
        assert_eq!(wrapped.total_out, 0);
        assert_eq!(wrapped.data_type, 0);
        assert!(wrapped.msg.is_null());
        assert!(!wrapped.state.is_null());

        let mut raw = blank_stream();
        raw.adler = 0xdead_beef;
        init(&mut raw, -15);
        assert_eq!(
            raw.adler, 0xdead_beef,
            "a raw stream's adler is left alone, C's `if (state->wrap)` guard"
        );

        // SAFETY: both streams are initialised and are ended exactly once.
        unsafe {
            assert_eq!(inflateEnd(&mut wrapped), ReturnCode::OK.as_i32());
            assert_eq!(inflateEnd(&mut raw), ReturnCode::OK.as_i32());
        }
        assert!(wrapped.state.is_null(), "inflateEnd clears strm->state");
        assert!(raw.state.is_null());
    }

    #[test]
    fn a_bad_window_bits_is_refused_without_installing_a_state() {
        let (version, size) = version_args();
        let mut strm = blank_stream();
        // `test/infcover.c` L371 asserts Z_STREAM_ERROR for windowBits == 1.
        for bits in [1, 7, -7, -16, 48, 100] {
            // SAFETY: `strm` is a live zeroed stream, `version` is `ZLIB_VERSION` and
            // `size` is this build's `size_of::<z_stream>()`, so the version gate is
            // satisfied and the only pointer is to a live local.
            let ret = unsafe { inflateInit2_(&mut strm, bits, version, size) };
            assert_eq!(
                ret,
                ReturnCode::STREAM_ERROR.as_i32(),
                "windowBits = {bits}"
            );
            assert!(strm.state.is_null(), "windowBits = {bits} left a state");
        }
        // The whole accepted matrix, for contrast, worked out from `inflate.c`
        // L147-L160: `wrap = (windowBits >> 4) + 5`, then `windowBits &= 15` for any
        // request below 48, and only then the `8..=15` bounds test -- which a
        // `windowBits` of zero skips entirely, because zero means "take the size from
        // the stream's own header". That is why 16 and 32 are legal even though
        // neither is in `8..=15`: they mask to zero.
        for bits in [0, 8, 9, 15, -8, -15, 16, 24, 31, 32, 40, 47] {
            let mut ok = blank_stream();
            init(&mut ok, bits);
            // SAFETY: `ok` is a live, stack-owned `z_stream` this test initialised and
            // has not yet ended, so `&mut ok` is non-null, aligned and its only borrow.
            // This is the last call that touches it, so nothing reads the stream after
            // its state has been released.
            assert_eq!(unsafe { inflateEnd(&mut ok) }, ReturnCode::OK.as_i32());
        }
    }

    // -----------------------------------------------------------------------
    // Decoding
    // -----------------------------------------------------------------------

    #[test]
    fn a_zlib_stream_round_trips_through_the_exported_entry_points() {
        let payload = b"hello, hello! and a rather longer tail so a match is found";
        let compressed = deflate_zlib(payload);

        let mut strm = blank_stream();
        init(&mut strm, 15);
        let mut out = vec![0_u8; payload.len() * 2 + 16];
        strm.next_in = compressed.as_ptr();
        strm.avail_in = uInt::try_from(compressed.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = uInt::try_from(out.len()).unwrap();

        // SAFETY: the stream is initialised and both regions are live and disjoint.
        let ret = unsafe { inflate(&mut strm, 4) };
        assert_eq!(ret, ReturnCode::STREAM_END.as_i32());
        assert_eq!(usize::try_from(strm.total_in).unwrap(), compressed.len());
        assert_eq!(usize::try_from(strm.total_out).unwrap(), payload.len());
        assert_eq!(&out[..payload.len()], payload);
        assert_eq!(strm.avail_in, 0);
        assert_eq!(
            strm.avail_out as usize,
            out.len() - payload.len(),
            "avail_out is decremented by exactly what was produced"
        );
        // SAFETY: `next_in`/`next_out` were advanced within their own buffers.
        unsafe {
            assert_eq!(strm.next_in, compressed.as_ptr().add(compressed.len()));
            assert_eq!(strm.next_out, out.as_mut_ptr().add(payload.len()));
        }
        assert!(strm.msg.is_null());

        // SAFETY: `strm` is a live, stack-owned `z_stream` this test initialised and
        // has not yet ended, so `&mut strm` is non-null, aligned and its only borrow.
        // This is the last call that touches it, so nothing reads the stream after
        // its state has been released.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    #[test]
    fn decoding_resumes_across_one_byte_calls() {
        let payload = b"resumability is the contract inflate offers incremental callers";
        let compressed = deflate_zlib(payload);

        let mut strm = blank_stream();
        init(&mut strm, 15);
        let mut out = vec![0_u8; payload.len() + 8];
        let mut produced = 0_usize;
        let mut ret = ReturnCode::OK.as_i32();
        for (index, byte) in compressed.iter().enumerate() {
            strm.next_in = core::ptr::from_ref(byte);
            strm.avail_in = 1;
            // SAFETY: `out` outlives the loop and `produced` never exceeds its length.
            strm.next_out = unsafe { out.as_mut_ptr().add(produced) };
            strm.avail_out = uInt::try_from(out.len() - produced).unwrap();
            // SAFETY: the stream is initialised and both regions are live.
            ret = unsafe { inflate(&mut strm, 0) };
            assert!(
                ret == ReturnCode::OK.as_i32()
                    || ret == ReturnCode::STREAM_END.as_i32()
                    || ret == ReturnCode::BUF_ERROR.as_i32(),
                "byte {index} gave {ret}"
            );
            produced = usize::try_from(strm.total_out).unwrap();
            if ret == ReturnCode::STREAM_END.as_i32() {
                break;
            }
        }
        assert_eq!(ret, ReturnCode::STREAM_END.as_i32());
        assert_eq!(produced, payload.len());
        assert_eq!(&out[..payload.len()], payload);
        // SAFETY: `strm` is a live, stack-owned `z_stream` this test initialised and
        // has not yet ended, so `&mut strm` is non-null, aligned and its only borrow.
        // This is the last call that touches it, so nothing reads the stream after
        // its state has been released.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    /// Every `flush` integer is accepted, documented or not, and a null output is
    /// still refused.
    ///
    /// ★ C's `inflate` validates `flush` nowhere. The value is compared against
    /// `Z_FINISH`, `Z_BLOCK` and `Z_TREES` and otherwise ignored, so `-1`, `7` and
    /// `INT_MAX` all behave exactly as `Z_NO_FLUSH` behaves. Refusing anything outside
    /// `0..=6` would make this port stricter than the reference for a caller C serves
    /// happily; this test is what keeps that strictness out.
    #[test]
    fn every_flush_value_is_accepted_and_a_null_output_is_not() {
        let mut strm = blank_stream();
        init(&mut strm, 15);
        let mut out = [0_u8; 8];
        let input = [0x78_u8, 0x9c];
        strm.next_in = input.as_ptr();
        strm.avail_in = 2;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 8;
        // SAFETY: the stream is initialised and both regions are live.
        unsafe {
            // Undocumented values, treated as `Z_NO_FLUSH` -- never rejected.
            for flush in [-1, MAX_INFLATE_FLUSH + 1, 100, c_int::MIN, c_int::MAX] {
                assert_ne!(
                    inflate(&mut strm, flush),
                    ReturnCode::STREAM_ERROR.as_i32(),
                    "flush = {flush} must behave as Z_NO_FLUSH, as it does in C"
                );
            }
            // All seven documented values are accepted, `Z_TREES` included.
            for flush in 0..=MAX_INFLATE_FLUSH {
                let answer = inflate(&mut strm, flush);
                assert_ne!(answer, ReturnCode::STREAM_ERROR.as_i32(), "flush={flush}");
            }
            // L494-L496: a null `next_out` is refused even with input available.
            strm.next_out = core::ptr::null_mut();
            assert_eq!(inflate(&mut strm, 0), ReturnCode::STREAM_ERROR.as_i32());
            // A null `next_in` with a non-zero `avail_in` is refused too...
            strm.next_out = out.as_mut_ptr();
            strm.next_in = core::ptr::null();
            strm.avail_in = 1;
            assert_eq!(inflate(&mut strm, 0), ReturnCode::STREAM_ERROR.as_i32());
            // ...but with `avail_in == 0` it is entirely legal.
            strm.avail_in = 0;
            assert_ne!(inflate(&mut strm, 0), ReturnCode::STREAM_ERROR.as_i32());
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
    }

    #[test]
    fn a_memory_error_is_sticky_and_a_message_survives_a_second_call() {
        // The `Z_DATA_ERROR` half of `test/infcover.c` L421-L422's stickiness check:
        // once `BAD` is latched, every later call reports it again -- and `msg` keeps
        // the exact pointer it published, because C never rewrites it.
        let mut strm = blank_stream();
        init(&mut strm, 15);
        let input = [0x77_u8, 0x85];
        let mut out = [0_u8; 8];
        strm.next_in = input.as_ptr();
        strm.avail_in = 2;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 8;
        // SAFETY: the stream is initialised and both regions are live.
        let first = unsafe { inflate(&mut strm, 0) };
        assert_eq!(first, ReturnCode::DATA_ERROR.as_i32());
        let published = strm.msg;
        assert!(!published.is_null());
        let text = msg_of(&strm).unwrap();
        // SAFETY: `strm` is the same live, stack-owned stream, still initialised and
        // not yet ended, and `&mut strm` is the only borrow of it at this point. Its
        // `next_in`/`next_out` pairs still describe the non-overlapping local buffers
        // set up above, both of which outlive the call.
        let second = unsafe { inflate(&mut strm, 0) };
        assert_eq!(second, ReturnCode::DATA_ERROR.as_i32());
        assert!(
            core::ptr::eq(strm.msg, published),
            "the message pointer must not be rewritten on a repeat error"
        );
        assert_eq!(msg_of(&strm), Some(text));
        // SAFETY: `strm` is a live, stack-owned `z_stream` this test initialised and
        // has not yet ended, so `&mut strm` is non-null, aligned and its only borrow.
        // This is the last call that touches it, so nothing reads the stream after
        // its state has been released.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    // -----------------------------------------------------------------------
    // ★ The externally written mode tag
    // -----------------------------------------------------------------------

    #[test]
    fn a_mode_written_through_the_raw_state_pointer_is_honoured() {
        // ★ This is `test/infcover.c` L316-L333 reproduced through the ABI. The
        // harness casts `strm.state` to `struct inflate_state *` and assigns
        // `mode = DICT`, then requires `inflateSetDictionary` to answer `Z_OK` and the
        // next `inflate` to answer `Z_BUF_ERROR`. A zero-based `Mode`, or a facade
        // that cached the mode instead of re-reading it, would break both silently.
        //
        // The stream is the harness's own "need dictionary" fixture, hex
        // `8 b8 0 0 0 1` at `windowBits` 8 (its L390): CMF/FLG with FDICT set, then a
        // `DICTID` of 1 -- which is `adler32(0, Z_NULL, 0)`, and therefore exactly the
        // id an EMPTY dictionary has. That is what makes a zero-length dictionary the
        // *correct* one here rather than a degenerate case.
        let input = [0x08_u8, 0xb8, 0x00, 0x00, 0x00, 0x01];
        let mut scratch = [0_u8; 8];
        let mut strm = blank_stream();
        init(&mut strm, 8);

        // The C-visible prefix is `{ z_streamp strm; int mode; }`, which is what
        // `StatePrefix` mirrors, so the harness's `((struct inflate_state *)
        // strm.state)->mode` is a write to `StatePrefix::tag`. Pin the two numbers
        // the C header fixes before relying on them.
        // Relational first, because that is the form that holds on every target: the tag
        // follows the back-pointer with no padding, and the prefix is that pointer plus two
        // `int`s. The measured 64-bit numbers are then pinned under the width guard that
        // makes them true -- 16 and 8 where a pointer is eight bytes wide, 12 and 4 on an
        // ILP32 target, which is what `types.rs`'s own const assertions already say.
        assert_eq!(
            size_of::<StatePrefix>(),
            size_of::<z_streamp>() + 2 * size_of::<c_int>()
        );
        assert_eq!(offset_of!(StatePrefix, tag), size_of::<z_streamp>());
        if size_of::<z_streamp>() == 8 {
            assert_eq!(size_of::<StatePrefix>(), 16);
            assert_eq!(offset_of!(StatePrefix, tag), 8);
        }
        assert_eq!(size_of::<c_int>(), 4);
        let tag = mode_slot(&strm);
        // SAFETY: `tag` is the pointer `mode_slot` derived from this stream's live
        // state block; the assertions immediately above pin `StatePrefix`'s size and
        // the tag's offset within it, so the read is in bounds, and the slot holds an
        // initialised `c_int` that the library wrote.
        assert_eq!(unsafe { tag.read() }, Mode::Head.as_raw());
        assert_eq!(Mode::Head.as_raw(), 16180);
        assert_eq!(DICT_TAG, Mode::Head.as_raw() + 10, "DICT is the 11th mode");

        strm.next_in = input.as_ptr();
        strm.avail_in = 6;
        strm.next_out = scratch.as_mut_ptr();
        strm.avail_out = 8;
        // SAFETY: the stream is initialised and both regions are live and disjoint.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert_eq!(ret, ReturnCode::NEED_DICT.as_i32());
        assert_eq!(strm.adler, 1, "the dictionary id is published in adler");
        // SAFETY: `tag` is the same in-bounds pointer into the same live state block,
        // which the intervening `inflate` call left installed rather than freed.
        let observed = unsafe { tag.read() };
        assert_eq!(observed, DICT_TAG, "the stream is waiting in DICT");

        // L324-L325: the wrong dictionary is a data error, not a stream error.
        // SAFETY: `strm` is the live initialised stream and `input` is live and readable
        // for at least the one byte the length declares.
        let ret = unsafe { inflateSetDictionary(&mut strm, input.as_ptr(), 1) };
        assert_eq!(ret, ReturnCode::DATA_ERROR.as_i32());
        // The right one -- empty -- is accepted.
        // SAFETY: `strm` is the live initialised stream and `scratch` is live; a zero
        // length reads nothing from behind the pointer.
        let ret = unsafe { inflateSetDictionary(&mut strm, scratch.as_ptr(), 0) };
        assert_eq!(ret, ReturnCode::OK.as_i32());

        strm.next_in = core::ptr::null();
        strm.avail_in = 0;
        strm.next_out = scratch.as_mut_ptr();
        strm.avail_out = 8;
        // SAFETY: the stream is initialised and its cursors were just set from live
        // locals, with `avail_in`/`avail_out` taken from those objects' own lengths.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert_eq!(ret, ReturnCode::BUF_ERROR.as_i32());

        // ★ L330-L333: force the stream back into `DICT` from *outside* the library
        // and require the same two answers again. This is the assertion the whole
        // mode-tag synchronisation exists for.
        // SAFETY: the prefix layout is as documented above.
        unsafe {
            tag.write(DICT_TAG);
        }
        // SAFETY: as the previous call -- the live initialised stream and a live
        // `scratch`, whose bytes a zero length never reads. The externally written mode
        // tag is a value the entry point validates, not a pointer it follows.
        let ret = unsafe { inflateSetDictionary(&mut strm, scratch.as_ptr(), 0) };
        assert_eq!(
            ret,
            ReturnCode::OK.as_i32(),
            "an externally written mode = DICT must be honoured"
        );
        // SAFETY: as the previous `inflate` call -- the same live stream with the same
        // live output region.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert_eq!(ret, ReturnCode::BUF_ERROR.as_i32());

        // A tag outside `HEAD..=SYNC` must make every entry point refuse the stream,
        // which is the *other* thing the tag is for: it is also the validity check.
        // SAFETY: the prefix layout is as documented above.
        unsafe {
            tag.write(0);
            assert_eq!(inflate(&mut strm, 0), ReturnCode::STREAM_ERROR.as_i32());
            assert_eq!(inflateEnd(&mut strm), ReturnCode::STREAM_ERROR.as_i32());
            // Restore a live tag so the state can be released rather than leaked.
            tag.write(Mode::Head.as_raw());
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
    }

    #[test]
    fn the_sync_mode_makes_inflate_refuse_to_resume() {
        // `test/infcover.c` L435-L436: `inflateSync` that fails leaves the stream in
        // `SYNC`, and `inflate` then answers `Z_STREAM_ERROR` rather than resuming.
        let mut strm = blank_stream();
        init(&mut strm, -8);
        let input = [0x80_u8, 0x00];
        let mut out = [0_u8; 8];
        strm.next_in = input.as_ptr();
        strm.avail_in = 2;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 8;
        // SAFETY: the stream is initialised and both regions are live.
        unsafe {
            assert_eq!(inflateSync(&mut strm), ReturnCode::DATA_ERROR.as_i32());
            assert_eq!(inflate(&mut strm, 0), ReturnCode::STREAM_ERROR.as_i32());
        }
        // The tag really is `SYNC`, which is what a caller reading it would see.
        let tag = mode_slot(&strm);
        // SAFETY: `mode_slot` returned a pointer to the live prefix's `tag` member.
        assert_eq!(unsafe { tag.read() }, SYNC_TAG);

        // Feeding the sync pattern completes the search and revives the stream.
        let pattern = [0x00_u8, 0x00, 0xff, 0xff];
        strm.next_in = pattern.as_ptr();
        strm.avail_in = 4;
        // SAFETY: `strm` is a live, stack-owned stream this test initialised and has
        // not ended; `&mut strm` is the only borrow of it, and each call below leaves
        // it valid for the next. `inflateEnd` is last, so nothing touches the stream
        // after its state is released.
        unsafe {
            assert_eq!(inflateSync(&mut strm), ReturnCode::OK.as_i32());
            let _ = inflateSyncPoint(&mut strm);
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
    }

    // -----------------------------------------------------------------------
    // Dictionaries, resets, copying and introspection
    // -----------------------------------------------------------------------

    #[test]
    fn set_dictionary_rejects_a_wrapped_stream_that_is_not_waiting_for_one() {
        // `test/infcover.c` L362-L363, including the `(Z_NULL, 0)` argument pair.
        let mut strm = blank_stream();
        init(&mut strm, 15);
        // SAFETY: the stream is initialised; a null dictionary with a zero length is
        // the documented "nothing to install" pair.
        unsafe {
            assert_eq!(
                inflateSetDictionary(&mut strm, core::ptr::null(), 0),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }

        // A raw stream, by contrast, accepts one at any time and the history is then
        // readable back.
        let mut raw = blank_stream();
        init(&mut raw, -15);
        let dictionary = b"a preset dictionary primes the window with history";
        // SAFETY: the stream is initialised and `dictionary` is live.
        unsafe {
            assert_eq!(
                inflateSetDictionary(
                    &mut raw,
                    dictionary.as_ptr(),
                    uInt::try_from(dictionary.len()).unwrap()
                ),
                ReturnCode::OK.as_i32()
            );
        }

        let mut length: uInt = 0;
        // SAFETY: the stream is initialised; only the length is requested.
        unsafe {
            assert_eq!(
                inflateGetDictionary(&mut raw, core::ptr::null_mut(), &mut length),
                ReturnCode::OK.as_i32()
            );
        }
        assert_eq!(usize::try_from(length).unwrap(), dictionary.len());

        let mut history = vec![0_u8; dictionary.len()];
        let mut reported = uInt::try_from(history.len()).unwrap();
        // SAFETY: `history` is live and writable for the whole history, which is
        // `dictionary.len()` bytes.
        unsafe {
            assert_eq!(
                inflateGetDictionary(&mut raw, history.as_mut_ptr(), &mut reported),
                ReturnCode::OK.as_i32()
            );
        }
        assert_eq!(history, dictionary);
        assert_eq!(usize::try_from(reported).unwrap(), dictionary.len());

        // ★ `dictLength` is an out-parameter, so the value it *held* must not affect
        // the copy. An earlier draft read it as a capacity, which both was undefined
        // behaviour for the conforming caller below and truncated the result to
        // whatever the uninitialised word happened to contain. Here the slot is seeded
        // with a deliberately hostile value -- four, far short of the real history --
        // and the whole dictionary must still arrive.
        let mut declared: uInt = 4;
        let mut full = vec![0xa5_u8; MAX_WINDOW_BYTES];
        // SAFETY: `full` holds the 32768 bytes `zlib.h` L936-L942 asks a caller for,
        // so the copy fits whatever the history turns out to be.
        unsafe {
            assert_eq!(
                inflateGetDictionary(&mut raw, full.as_mut_ptr(), &mut declared),
                ReturnCode::OK.as_i32()
            );
        }
        assert_eq!(usize::try_from(declared).unwrap(), dictionary.len());
        assert_eq!(&full[..dictionary.len()], &dictionary[..]);
        // Nothing beyond the history was touched.
        assert!(full[dictionary.len()..].iter().all(|&byte| byte == 0xa5));

        // SAFETY: `raw` is a live, stack-owned `z_stream` this test initialised and
        // has not yet ended, so `&mut raw` is non-null, aligned and its only borrow.
        // This is the last call that touches it, so nothing reads the stream after
        // its state has been released.
        assert_eq!(unsafe { inflateEnd(&mut raw) }, ReturnCode::OK.as_i32());
    }

    #[test]
    fn the_three_resets_restore_the_stream_and_keep_it_usable() {
        let payload = b"reset me";
        let compressed = deflate_zlib(payload);
        let mut strm = blank_stream();
        init(&mut strm, 15);
        let mut out = vec![0_u8; 64];
        strm.next_in = compressed.as_ptr();
        strm.avail_in = uInt::try_from(compressed.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 64;
        // SAFETY: the stream is initialised and both regions are live.
        unsafe {
            assert_eq!(inflate(&mut strm, 4), ReturnCode::STREAM_END.as_i32());
            assert!(strm.total_out > 0);

            assert_eq!(inflateResetKeep(&mut strm), ReturnCode::OK.as_i32());
            assert_eq!(strm.total_in, 0);
            assert_eq!(strm.total_out, 0);
            assert_eq!(strm.adler, 1);
            assert!(strm.msg.is_null());

            assert_eq!(inflateReset(&mut strm), ReturnCode::OK.as_i32());
            // `test/infcover.c` L344 resets to raw and then ends the stream.
            assert_eq!(inflateReset2(&mut strm, -8), ReturnCode::OK.as_i32());
            assert_eq!(
                inflateReset2(&mut strm, 1),
                ReturnCode::STREAM_ERROR.as_i32(),
                "a rejected windowBits leaves the stream usable"
            );
            assert_eq!(inflateReset2(&mut strm, 15), ReturnCode::OK.as_i32());
        }

        // Still usable after all that.
        strm.next_in = compressed.as_ptr();
        strm.avail_in = uInt::try_from(compressed.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 64;
        // SAFETY: `strm` is a live, stack-owned stream this test initialised, whose
        // input and output pointers address local buffers that outlive both calls, and
        // `&mut strm` is the only borrow of it. `inflateEnd` runs last.
        unsafe {
            assert_eq!(inflate(&mut strm, 4), ReturnCode::STREAM_END.as_i32());
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
        assert_eq!(&out[..payload.len()], payload);
    }

    #[test]
    fn copy_duplicates_the_whole_stream_and_both_halves_decode() {
        let payload = b"branching a decode needs the whole z_stream copied, not just the state";
        let compressed = deflate_zlib(payload);

        let mut source = blank_stream();
        init(&mut source, 15);
        let mut first = vec![0_u8; payload.len() + 16];
        source.next_in = compressed.as_ptr();
        source.avail_in = uInt::try_from(compressed.len()).unwrap();
        source.next_out = first.as_mut_ptr();
        source.avail_out = uInt::try_from(first.len()).unwrap();

        let mut copy = blank_stream();
        // SAFETY: `source` is initialised and `copy` is a live, stateless `z_stream`
        // distinct from it.
        unsafe {
            assert_eq!(inflateCopy(&mut copy, &mut source), ReturnCode::OK.as_i32());
        }
        // ★ The whole `z_stream` is copied, so every member matches except `state`.
        assert_eq!(copy.next_in, source.next_in);
        assert_eq!(copy.avail_in, source.avail_in);
        assert_eq!(copy.next_out, source.next_out);
        assert_eq!(copy.avail_out, source.avail_out);
        assert_eq!(copy.total_in, source.total_in);
        assert_eq!(copy.total_out, source.total_out);
        assert_eq!(copy.adler, source.adler);
        assert_eq!(copy.data_type, source.data_type);
        assert!(!copy.state.is_null());
        assert!(!core::ptr::eq(copy.state, source.state));

        // Both decode independently to the same bytes.
        let mut second = vec![0_u8; payload.len() + 16];
        copy.next_out = second.as_mut_ptr();
        // SAFETY: both streams are initialised and their buffers are disjoint.
        unsafe {
            assert_eq!(inflate(&mut source, 4), ReturnCode::STREAM_END.as_i32());
            assert_eq!(inflate(&mut copy, 4), ReturnCode::STREAM_END.as_i32());
        }
        assert_eq!(&first[..payload.len()], payload);
        assert_eq!(&second[..payload.len()], payload);

        // SAFETY: each stream is ended exactly once.
        unsafe {
            assert_eq!(inflateEnd(&mut copy), ReturnCode::OK.as_i32());
            assert_eq!(inflateEnd(&mut source), ReturnCode::OK.as_i32());
        }

        // A destination aliasing the source is refused rather than corrupting it.
        let mut lone = blank_stream();
        init(&mut lone, 15);
        // SAFETY: `lone` is initialised; the call is deliberately aliased.
        unsafe {
            let same: *mut z_stream = &mut lone;
            assert_eq!(
                inflateCopy(same, same),
                ReturnCode::STREAM_ERROR.as_i32(),
                "an aliased copy must be refused, not performed"
            );
            assert_eq!(inflateEnd(&mut lone), ReturnCode::OK.as_i32());
        }

        // A *partially* overlapping pair is refused too. Two correctly aligned
        // `z_stream`s can overlap at any multiple of the alignment below the struct
        // size, and `copy_nonoverlapping` is undefined for them, so pointer equality is
        // not a sufficient test.
        let mut pair = [blank_stream(), blank_stream()];
        init(&mut pair[0], 15);
        // SAFETY: `pair[0]` is initialised. The destination is deliberately offset by
        // one `z_stream` alignment unit into the same array, so the two ranges overlap
        // without being equal; nothing is written, because the call must be refused
        // before the copy.
        unsafe {
            let source: *mut z_stream = &mut pair[0];
            let overlapping = source.byte_add(align_of::<z_stream>());
            assert_eq!(
                inflateCopy(overlapping, source),
                ReturnCode::STREAM_ERROR.as_i32(),
                "a partially overlapping copy must be refused"
            );
            assert_eq!(inflateEnd(&mut pair[0]), ReturnCode::OK.as_i32());
        }
    }

    #[test]
    fn copy_carries_reserved_and_msg_without_ever_reading_them() {
        // ★ This is the shape `test/example.c` actually uses. It declares
        // `z_stream c_stream;` on the stack and assigns only `zalloc`, `zfree` and
        // `opaque` before calling an initialiser, so `reserved` and `msg` are genuinely
        // indeterminate at every entry point that follows -- which is what makes reading
        // the whole structure as a *value* undefined behaviour rather than merely
        // untidy, and why `inflateCopy` duplicates it with a byte copy.
        //
        // Every member `zlib.h` requires is written here through a raw place; `reserved`
        // is left untouched. Miri is the instrument that makes this test bite: it
        // reports a typed read of an uninitialised `uLong` if one is ever reintroduced,
        // where an ordinary run would simply carry whatever the stack held.
        let payload = b"reserved is the caller's, and the library must not look at it";
        let compressed = deflate_zlib(payload);

        let mut raw = MaybeUninit::<z_stream>::uninit();
        let source = raw.as_mut_ptr();
        // SAFETY: `raw` is a live, correctly aligned `z_stream`-sized allocation, so
        // every member is in bounds and writable. Each write goes through a raw place
        // and forms no reference, so the members left alone stay uninitialised rather
        // than being read.
        unsafe {
            core::ptr::addr_of_mut!((*source).next_in).write(compressed.as_ptr());
            core::ptr::addr_of_mut!((*source).avail_in)
                .write(uInt::try_from(compressed.len()).unwrap());
            core::ptr::addr_of_mut!((*source).total_in).write(0);
            core::ptr::addr_of_mut!((*source).next_out).write(core::ptr::null_mut());
            core::ptr::addr_of_mut!((*source).avail_out).write(0);
            core::ptr::addr_of_mut!((*source).total_out).write(0);
            core::ptr::addr_of_mut!((*source).state).write(core::ptr::null_mut());
            core::ptr::addr_of_mut!((*source).zalloc).write(None);
            core::ptr::addr_of_mut!((*source).zfree).write(None);
            core::ptr::addr_of_mut!((*source).opaque).write(core::ptr::null_mut());
            core::ptr::addr_of_mut!((*source).data_type).write(0);
            core::ptr::addr_of_mut!((*source).adler).write(0);
        }

        let (version, size) = version_args();
        let mut copy = MaybeUninit::<z_stream>::uninit();
        let destination = copy.as_mut_ptr();
        let mut out = vec![0_u8; payload.len() + 16];
        // SAFETY: `source` holds every member the initialiser reads and is live for the
        // whole block; `destination` is a live, aligned, writable `z_stream` that holds
        // no state and does not overlap `source`. Neither call may read `reserved`.
        unsafe {
            assert_eq!(
                inflateInit2_(source, 15, version, size),
                ReturnCode::OK.as_i32()
            );
            assert_eq!(
                inflateCopy(destination, source),
                ReturnCode::OK.as_i32(),
                "a stream with an indeterminate `reserved` must still copy"
            );

            // The copy decodes on its own, which is the observable proof that the byte
            // copy carried every member the decoder needs.
            core::ptr::addr_of_mut!((*destination).next_out).write(out.as_mut_ptr());
            core::ptr::addr_of_mut!((*destination).avail_out)
                .write(uInt::try_from(out.len()).unwrap());
            assert_eq!(inflate(destination, 4), ReturnCode::STREAM_END.as_i32());
            assert_eq!(inflateEnd(destination), ReturnCode::OK.as_i32());
            assert_eq!(inflateEnd(source), ReturnCode::OK.as_i32());
        }
        assert_eq!(&out[..payload.len()], payload);
    }

    #[test]
    fn prime_mark_codes_used_undermine_and_validate_match_the_reference() {
        let mut strm = blank_stream();
        init(&mut strm, 15);
        // SAFETY: the stream is initialised for every call below.
        unsafe {
            // `test/infcover.c` L359-L360.
            assert_eq!(inflatePrime(&mut strm, 5, 31), ReturnCode::OK.as_i32());
            assert_eq!(inflatePrime(&mut strm, -1, 0), ReturnCode::OK.as_i32());
            assert_eq!(inflatePrime(&mut strm, 0, 0), ReturnCode::OK.as_i32());
            assert_eq!(inflatePrime(&mut strm, 16, 0), ReturnCode::OK.as_i32());
            // More than sixteen bits at once, or past thirty-two buffered.
            assert_eq!(
                inflatePrime(&mut strm, 17, 0),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(
                inflatePrime(&mut strm, 16, 0),
                ReturnCode::OK.as_i32(),
                "16 + 16 == 32 is the boundary and is accepted"
            );
            assert_eq!(
                inflatePrime(&mut strm, 1, 0),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(inflatePrime(&mut strm, -1, 0), ReturnCode::OK.as_i32());

            // ★ `inflateMark` is a composite, and `back` is -1 with no code pending,
            // so a fresh stream reports exactly `-(1 << 16)` -- the same number a bad
            // state reports, which is why it is not usable as an error test.
            assert_eq!(inflateMark(&mut strm), BAD_STATE_MARK);
            // ★ `inflateUndermine` FAILS in the shipped build:
            // `test/infcover.c` L440.
            assert_eq!(
                inflateUndermine(&mut strm, 1),
                ReturnCode::DATA_ERROR.as_i32()
            );
            assert_eq!(
                inflateUndermine(&mut strm, 0),
                ReturnCode::DATA_ERROR.as_i32()
            );
            assert_eq!(inflateValidate(&mut strm, 1), ReturnCode::OK.as_i32());
            assert_eq!(inflateValidate(&mut strm, 0), ReturnCode::OK.as_i32());
            assert_eq!(inflateSyncPoint(&mut strm), 0);
            // No tables have been built yet.
            assert_eq!(inflateCodesUsed(&mut strm), 0);
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }

        // After a real decode the arena is occupied, and the count is an
        // `unsigned long` rather than a status code.
        let compressed = deflate_zlib(&[b'x'; 4096]);
        let mut used = blank_stream();
        init(&mut used, 15);
        let mut out = vec![0_u8; 8192];
        used.next_in = compressed.as_ptr();
        used.avail_in = uInt::try_from(compressed.len()).unwrap();
        used.next_out = out.as_mut_ptr();
        used.avail_out = 8192;
        // SAFETY: the stream is initialised and both regions are live.
        unsafe {
            assert_eq!(inflate(&mut used, 4), ReturnCode::STREAM_END.as_i32());
            let count: c_ulong = inflateCodesUsed(&mut used);
            assert!(
                count <= narrow_uLong(u64::from(ENOUGH)),
                "codes used ({count}) must stay inside the {ENOUGH}-entry arena"
            );
            assert_eq!(inflateEnd(&mut used), ReturnCode::OK.as_i32());
        }
    }

    #[test]
    fn get_header_reports_a_gzip_header_and_refuses_a_zlib_stream() {
        // A gzip stream carrying a name and a comment, built by hand so the fields
        // are known: FLG = 0x1c sets FEXTRA | FNAME | FCOMMENT.
        let mut gz: Vec<u8> = vec![0x1f, 0x8b, 0x08, 0x1c, 0x11, 0x22, 0x33, 0x44, 0x02, 0x03];
        gz.extend_from_slice(&[0x02, 0x00, 0xab, 0xcd]); // XLEN = 2, then the bytes
        gz.extend_from_slice(b"name\0");
        gz.extend_from_slice(b"comment\0");
        // An empty final stored block, then CRC-32 and ISIZE of nothing.
        gz.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
        gz.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        gz.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);

        let mut extra = [0_u8; 8];
        let mut name = [0_u8; 8];
        let mut comment = [0_u8; 16];
        let head = header_on_heap(gz_header {
            text: 7,
            time: 0xdead,
            xflags: 7,
            os: 7,
            extra: extra.as_mut_ptr(),
            extra_len: 99,
            extra_max: 8,
            name: name.as_mut_ptr(),
            name_max: 8,
            comment: comment.as_mut_ptr(),
            comm_max: 16,
            hcrc: 7,
            done: 7,
        });

        let mut strm = blank_stream();
        init(&mut strm, 15);
        // A zlib-only stream cannot accept a header: `(wrap & 2) == 0`.
        // SAFETY: the stream is initialised and `head` is live with live buffers.
        unsafe {
            assert_eq!(
                inflateGetHeader(&mut strm, head),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }

        let mut strm = blank_stream();
        init(&mut strm, 47);
        // A null header is refused rather than dereferenced.
        // SAFETY: the stream is initialised; the null is the point of the call.
        unsafe {
            assert_eq!(
                inflateGetHeader(&mut strm, core::ptr::null_mut()),
                ReturnCode::STREAM_ERROR.as_i32()
            );
            assert_eq!(inflateGetHeader(&mut strm, head), ReturnCode::OK.as_i32());
        }
        // ★ Only `done` is written on install; the caller's other scalars survive.
        // SAFETY: `head` is the live heap header, read through the same raw pointer the
        // library holds.
        unsafe {
            assert_eq!((*head).done, 0);
            assert_eq!((*head).text, 7);
            assert_eq!((*head).time, 0xdead);
            assert_eq!((*head).extra_len, 99);
        }

        let mut out = [0_u8; 32];
        strm.next_in = gz.as_ptr();
        strm.avail_in = uInt::try_from(gz.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 32;
        // SAFETY: the stream is initialised and all regions are live and disjoint.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert!(
            ret == ReturnCode::OK.as_i32()
                || ret == ReturnCode::STREAM_END.as_i32()
                || ret == ReturnCode::BUF_ERROR.as_i32(),
            "gzip decode gave {ret}"
        );
        // SAFETY: as above -- one pointer, one provenance.
        unsafe {
            assert_eq!((*head).done, 1, "the header parsed to completion");
            assert_eq!((*head).time, 0x4433_2211);
            assert_eq!((*head).xflags, 0x02);
            assert_eq!((*head).os, 0x03);
            assert_eq!((*head).extra_len, 2);
            assert_eq!((*head).text, 0);
            assert_eq!((*head).hcrc, 0);
        }
        assert_eq!(&extra[..2], &[0xab, 0xcd]);
        assert_eq!(&name[..5], b"name\0");
        assert_eq!(&comment[..8], b"comment\0");
        // SAFETY: the stream is the initialised one, and the header is released only
        // after it, so nothing installed can outlive the allocation.
        unsafe {
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
            release_header(head);
        }
    }

    #[test]
    fn get_header_publishes_only_what_the_stream_supplied() {
        // ★ The two halves of `gz_header` and why the split is observable.
        //
        // `zlib.h` L118-L133 makes `text`, `time`, `xflags`, `os`, `extra_len`, `hcrc`
        // and `done` *outputs*. C assigns them from inside the header states, so a
        // member the stream never supplied is never written and keeps whatever the
        // caller left there -- and a caller is entitled to have left nothing there at
        // all, which is why this library reads none of them.
        //
        // The sharpest case is a stream that turns out **not** to be gzip: C's `HEAD`
        // state assigns `state->head->done = -1` (`inflate.c` L525) and returns to
        // decoding, so exactly one member is written and the other six are the caller's.
        let payload = b"hello, hello!";
        let mut zlib_stream: Vec<u8> = vec![0x78, 0x01, 0x01, 0x0d, 0x00, 0xf2, 0xff];
        zlib_stream.extend_from_slice(payload);
        let adler = zlib_rs::adler32::adler32(1, payload);
        zlib_stream.extend_from_slice(&adler.to_be_bytes());

        let mut head = gz_header {
            text: 7,
            time: 0xdead,
            xflags: 7,
            os: 7,
            extra: core::ptr::null_mut(),
            extra_len: 99,
            extra_max: 0,
            name: core::ptr::null_mut(),
            name_max: 0,
            comment: core::ptr::null_mut(),
            comm_max: 0,
            hcrc: 7,
            done: 7,
        };
        let mut strm = blank_stream();
        init(&mut strm, 47);
        let mut out = [0_u8; 32];
        strm.next_in = zlib_stream.as_ptr();
        strm.avail_in = uInt::try_from(zlib_stream.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 32;
        // SAFETY: the stream is initialised, `head` is live with two null buffers, and
        // the input and output regions are live and disjoint.
        unsafe {
            assert_eq!(
                inflateGetHeader(&mut strm, &mut head),
                ReturnCode::OK.as_i32()
            );
            assert_eq!(head.done, 0, "install writes only `done`");
            assert_eq!(inflate(&mut strm, 4), ReturnCode::STREAM_END.as_i32());
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
        assert_eq!(&out[..payload.len()], payload);
        assert_eq!(
            head.done, -1,
            "`done` reports that this is not a gzip stream"
        );
        // The six the stream never supplied are exactly as the caller left them.
        assert_eq!(head.text, 7);
        assert_eq!(head.time, 0xdead);
        assert_eq!(head.xflags, 7);
        assert_eq!(head.os, 7);
        assert_eq!(head.extra_len, 99);
        assert_eq!(head.hcrc, 7);
    }

    #[test]
    fn a_completed_header_is_not_republished_by_later_calls() {
        // C writes the header members from inside the header states, so once a header
        // has been parsed no later `inflate()` touches the caller's structure again --
        // however many payload chunks follow. This drives a gzip member across two
        // calls, poisons the structure in between, and requires the poison to survive.
        let payload = b"hello, hello!";
        let mut gz: Vec<u8> = vec![0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0x00, 0x03];
        // One final stored block: BFINAL | BTYPE=00, then LEN and its complement.
        gz.push(0x01);
        gz.extend_from_slice(&u16::try_from(payload.len()).unwrap().to_le_bytes());
        gz.extend_from_slice(&(!u16::try_from(payload.len()).unwrap()).to_le_bytes());
        let header_and_block = gz.len();
        gz.extend_from_slice(payload);
        gz.extend_from_slice(&zlib_rs::crc32::crc32(0, payload).to_le_bytes());
        gz.extend_from_slice(&u32::try_from(payload.len()).unwrap().to_le_bytes());

        let mut head = gz_header {
            text: 0,
            time: 0,
            xflags: 0,
            os: 0,
            extra: core::ptr::null_mut(),
            extra_len: 0,
            extra_max: 0,
            name: core::ptr::null_mut(),
            name_max: 0,
            comment: core::ptr::null_mut(),
            comm_max: 0,
            hcrc: 0,
            done: 7,
        };
        let mut strm = blank_stream();
        init(&mut strm, 47);
        let mut out = [0_u8; 32];
        // First call: the whole header and the stored-block header, nothing more.
        strm.next_in = gz.as_ptr();
        strm.avail_in = uInt::try_from(header_and_block).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 32;
        // SAFETY: the stream is initialised, `head` is live with three null buffers, and
        // the input and output regions are live and disjoint for both calls.
        unsafe {
            assert_eq!(
                inflateGetHeader(&mut strm, &mut head),
                ReturnCode::OK.as_i32()
            );
            let first = inflate(&mut strm, 0);
            assert!(
                first == ReturnCode::OK.as_i32() || first == ReturnCode::BUF_ERROR.as_i32(),
                "the first call gave {first}"
            );
        }
        assert_eq!(head.done, 1, "the header completed in the first call");
        assert_eq!(head.os, 0x03);

        // Poison every member from the caller's side. A republication would overwrite
        // them with the parsed values; C would not, because it has nothing left to
        // parse.
        head.done = 42;
        head.os = 42;
        head.time = 0xfeed;
        head.text = 42;
        head.xflags = 42;
        head.hcrc = 42;
        head.extra_len = 42;

        // SAFETY: as above; the remaining input is the payload and the trailer.
        unsafe {
            // `gz.get(..)` cannot fail: `header_and_block` is a length inside `gz`.
            strm.next_in = gz.as_ptr().add(header_and_block);
            strm.avail_in = uInt::try_from(gz.len() - header_and_block).unwrap();
            assert_eq!(inflate(&mut strm, 4), ReturnCode::STREAM_END.as_i32());
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
        assert_eq!(&out[..payload.len()], payload);
        assert_eq!(head.done, 42, "a finished header must not be republished");
        assert_eq!(head.os, 42);
        assert_eq!(head.time, 0xfeed);
        assert_eq!(head.text, 42);
        assert_eq!(head.xflags, 42);
        assert_eq!(head.hcrc, 42);
        assert_eq!(head.extra_len, 42);
    }

    #[test]
    fn a_reset_detaches_the_installed_header() {
        // `inflateResetKeep` assigns `state->head = Z_NULL` (`inflate.c` L110), so a
        // reset must leave the caller's structure alone from then on.
        let head = header_on_heap(gz_header {
            done: 5,
            ..zeroed_header()
        });
        let mut strm = blank_stream();
        init(&mut strm, 47);
        // SAFETY: the stream is initialised and `head` is the live heap header, reached
        // through the one pointer the library also holds.
        unsafe {
            assert_eq!(inflateGetHeader(&mut strm, head), ReturnCode::OK.as_i32());
            assert_eq!((*head).done, 0);
            assert_eq!(inflateReset(&mut strm), ReturnCode::OK.as_i32());
            (*head).done = 5;
        }
        let mut out = [0_u8; 8];
        let input = [0x1f_u8, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0, 0];
        strm.next_in = input.as_ptr();
        strm.avail_in = 10;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 8;
        // SAFETY: `strm` is a live, stack-owned stream this test initialised, with
        // `next_in`/`next_out` describing the non-overlapping local buffers above, and
        // `&mut strm` is its only borrow. The return value is deliberately discarded:
        // the assertion that follows is about the stream's fields, not the status.
        unsafe {
            let _ = inflate(&mut strm, 0);
            assert_eq!(
                (*head).done,
                5,
                "a detached header must not be written after a reset"
            );
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
            release_header(head);
        }
    }

    // -----------------------------------------------------------------------
    // ★ inflate_table -- the export `test/infcover.c` cannot link without
    // -----------------------------------------------------------------------

    #[test]
    fn inflate_table_reproduces_cover_trees_exactly() {
        // `test/infcover.c` L617-L639, transcribed argument for argument.
        let mut lens = [0_u16; 16];
        for (index, slot) in lens.iter_mut().enumerate().take(15) {
            *slot = u16::try_from(index + 1).unwrap();
        }
        lens[15] = 15;
        let mut work = [0_u16; 16];
        let mut table = vec![code::default(); ENOUGH_DISTS];

        for requested in [15_u32, 1] {
            let mut next: *mut code = table.as_mut_ptr();
            let mut bits: c_uint = requested;
            // SAFETY: `lens` and `work` are sixteen `u16`s each, `table` holds
            // `ENOUGH_DISTS` entries, and `next`/`bits` are live locals. The three
            // arrays are distinct allocations.
            let ret = unsafe {
                _zlib_rs_inflate_table(
                    DISTS,
                    lens.as_mut_ptr().cast::<c_ushort>(),
                    16,
                    &mut next,
                    &mut bits,
                    work.as_mut_ptr().cast::<c_ushort>(),
                )
            };
            assert_eq!(ret, 1, "requested root width {requested} must overflow");
            // ★ Neither in/out parameter is advanced on a non-zero return, which is
            // what C's early `return` before `inftrees.c` L308-L309 does.
            assert_eq!(next, table.as_mut_ptr());
            assert_eq!(bits, requested);
        }
    }

    // -----------------------------------------------------------------------
    // Streams whose output members were never set
    // -----------------------------------------------------------------------

    /// A stream in the shape `test/infcover.c` builds: only the members a caller must
    /// provide are written, so `next_out`, `avail_out` and `reserved` genuinely hold no
    /// value.
    ///
    /// `MaybeUninit` rather than a poison value, because that is what gives these tests
    /// teeth: reading an uninitialised member is undefined behaviour that Miri reports,
    /// where a poison value would merely be a number nobody noticed being read. C's
    /// `inflateSync`, `inflateGetDictionary`, `inflateSyncPoint` and `inflateCopy` all
    /// run correctly on such a stream, so this port must too.
    // Boxed deliberately, and `clippy::unnecessary_box_returns` is wrong about it here:
    // `inflateInit2_` records the *address* of the `z_stream` in the state block's prefix,
    // and every later entry point checks it (`inflate.c` L94's `state->strm != strm`).
    // Returning the value would move it into the caller's slot after that address was
    // taken, so the recorded owner would be stale and every subsequent call would report
    // `Z_STREAM_ERROR`. The heap allocation is what keeps the address stable across the
    // move. Written as a comment rather than the lint's `reason` field, which needs Rust
    // 1.81 while this workspace declares `rust-version = "1.80"`.
    #[allow(clippy::unnecessary_box_returns)]
    fn stream_with_no_output_members(window_bits: c_int) -> Box<MaybeUninit<z_stream>> {
        let mut raw: Box<MaybeUninit<z_stream>> = Box::new(MaybeUninit::uninit());
        let strm: *mut z_stream = raw.as_mut_ptr();
        let (version, size) = version_args();
        // SAFETY: `strm` addresses a whole `z_stream` this function owns. Each write goes
        // through a raw place, so nothing is read and no reference to the partly
        // initialised structure is formed. `inflateInit2_` reads only the three allocator
        // members, `version` and `stream_size`, all of which are written first.
        unsafe {
            core::ptr::addr_of_mut!((*strm).zalloc).write(None);
            core::ptr::addr_of_mut!((*strm).zfree).write(None);
            core::ptr::addr_of_mut!((*strm).opaque).write(core::ptr::null_mut());
            core::ptr::addr_of_mut!((*strm).next_in).write(core::ptr::null());
            core::ptr::addr_of_mut!((*strm).avail_in).write(0);
            assert_eq!(
                inflateInit2_(strm, window_bits, version, size),
                ReturnCode::OK.as_i32()
            );
        }
        raw
    }

    /// `inflateSync` works on a stream whose output members hold no value, and leaves
    /// them alone.
    ///
    /// `inflate.c` L1264-L1309 reads `avail_in`, `next_in`, `total_in` and `total_out`
    /// and writes `avail_in`, `next_in`, `total_in`, `total_out`, `msg`, `data_type` and
    /// -- for a wrapped stream -- `adler`. The output pair appears nowhere, and
    /// `zlib.h` L951-L963 asks nothing of it.
    #[test]
    fn sync_touches_no_output_member() {
        let mut raw = stream_with_no_output_members(-15);
        let strm = raw.as_mut_ptr();
        // The four-byte sync marker `test/infcover.c` L435 uses.
        let marker = [0x00_u8, 0x00, 0xff, 0xff];
        // SAFETY: the stream is the initialised one, `marker` is live for the call, and
        // every access goes through a raw place.
        unsafe {
            core::ptr::addr_of_mut!((*strm).next_in).write(marker.as_ptr());
            core::ptr::addr_of_mut!((*strm).avail_in).write(4);
            assert_eq!(inflateSync(strm), ReturnCode::OK.as_i32());
            assert_eq!(
                core::ptr::addr_of!((*strm).avail_in).read(),
                0,
                "all four marker bytes are consumed"
            );
            assert_eq!(
                core::ptr::addr_of!((*strm).total_in).read(),
                4,
                "inflate.c L1294 adds the bytes looked at, and the reset restores it"
            );
            // `inflateSyncPoint` reads the state only, so it is safe here too.
            let _ = inflateSyncPoint(strm);
            assert_eq!(inflateEnd(strm), ReturnCode::OK.as_i32());
        }
    }

    /// A raw stream's `adler` member is neither read nor written, from init to
    /// `inflateEnd`.
    ///
    /// ★ **This is the member whose initialisation C makes conditional, so it is the
    /// member the facade must never read.** `inflateResetKeep` assigns `strm->adler`
    /// under `if (state->wrap)` (`inflate.c` L108-L109) and the epilogue under
    /// `if ((state->wrap & 4) && out)` (L1144-L1146); `windowBits = -15` satisfies
    /// neither, so the reference leaves the member exactly as the caller left it --
    /// which for a caller that never set it means *indeterminate*. Loading it into the
    /// core's view on every call would therefore read an uninitialised member, which Miri
    /// reports as undefined behaviour on precisely this path.
    ///
    /// The sentinel is what makes both halves observable at once: it survives only if
    /// the value is passed over untouched, and a spurious write would replace it with
    /// the check value the decoder never computes for a raw stream.
    #[test]
    fn a_raw_stream_leaves_the_adler_member_alone_through_a_whole_decode() {
        let payload = b"raw payload, raw payload, raw payload, and again";
        let wrapped = deflate_zlib(payload);
        // RFC 1950: a zlib stream is a two-byte header, the raw DEFLATE stream, then a
        // four-byte Adler-32. The middle is exactly what `windowBits = -15` reads.
        let raw = &wrapped[2..wrapped.len() - 4];

        let mut strm = blank_stream();
        let sentinel: c_ulong = 0x0bad_f00d;
        strm.adler = sentinel;
        init(&mut strm, -15);
        assert_eq!(strm.adler, sentinel, "init left it alone (inflate.c L108)");

        let mut out = vec![0_u8; payload.len() + 64];
        strm.next_in = raw.as_ptr();
        strm.avail_in = uInt::try_from(raw.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = uInt::try_from(out.len()).unwrap();
        // SAFETY: the stream is initialised and both regions are live and disjoint.
        let ret = unsafe { inflate(&mut strm, 4) };
        assert_eq!(
            ret,
            ReturnCode::STREAM_END.as_i32(),
            "the raw stream decodes"
        );
        let produced = out.len() - usize::try_from(strm.avail_out).unwrap();
        assert_eq!(&out[..produced], payload, "and produces the payload");
        assert_eq!(
            strm.adler, sentinel,
            "and the whole decode left `adler` alone: `wrap & 4` is clear"
        );

        // A reset does not touch it either, for the same reason.
        // SAFETY: the stream holds a live state.
        unsafe {
            assert_eq!(inflateReset(&mut strm), ReturnCode::OK.as_i32());
        }
        assert_eq!(strm.adler, sentinel, "nor does the reset");
        // SAFETY: as above; the state is ended exactly once.
        unsafe {
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
    }

    /// `inflateSync` reports `Z_BUF_ERROR` for an empty stream without reading the
    /// output members either.
    #[test]
    fn sync_with_no_input_reports_buf_error() {
        let mut raw = stream_with_no_output_members(-15);
        let strm = raw.as_mut_ptr();
        // SAFETY: as above.
        unsafe {
            assert_eq!(inflateSync(strm), ReturnCode::BUF_ERROR.as_i32());
            assert_eq!(inflateEnd(strm), ReturnCode::OK.as_i32());
        }
    }

    /// `inflateCopy` duplicates a stream whose output members hold no value.
    ///
    /// `inflate.c` L1352 is `zmemcpy((voidpf)dest, (voidpf)source, sizeof(z_stream))` --
    /// a byte copy. Reading the structure as a `z_stream` value instead would read
    /// `next_out`, `avail_out` and `reserved`, and reading an indeterminate `*mut Bytef`
    /// is undefined behaviour rather than a garbage value.
    #[test]
    fn copy_of_a_stream_with_no_output_members_is_a_byte_copy() {
        let mut raw = stream_with_no_output_members(-15);
        let source = raw.as_mut_ptr();
        let mut copy = blank_stream();
        // SAFETY: both streams are live, distinct and non-overlapping; `source` holds a
        // state this library installed.
        unsafe {
            assert_eq!(
                inflateCopy(core::ptr::addr_of_mut!(copy), source),
                ReturnCode::OK.as_i32()
            );
            assert!(!copy.state.is_null(), "the copy has a state of its own");
            assert!(
                !core::ptr::eq(copy.state, core::ptr::addr_of!((*source).state).read()),
                "and it is not the source's"
            );
            assert_eq!(
                inflateEnd(core::ptr::addr_of_mut!(copy)),
                ReturnCode::OK.as_i32()
            );
            assert_eq!(inflateEnd(source), ReturnCode::OK.as_i32());
        }
    }

    /// A refused allocation inside `inflateCopy` leaves `dest` **completely** untouched.
    ///
    /// `inflate.c` allocates the state at L1341 and the window at L1345, and only once
    /// both have succeeded does it write anything to the destination: the `zmemcpy` of
    /// the stream is at L1352 and `dest->state` is assigned last, at L1365. So a caller
    /// whose copy fails for want of memory finds its structure exactly as it left it.
    /// Installing a placeholder state first and clearing it again on failure would be
    /// observable -- `dest->state` would change value and change back -- and needlessly
    /// so, which is why the destination is written only once everything has succeeded.
    #[test]
    fn a_refused_copy_leaves_the_destination_untouched() {
        for deny_from in [1_usize, 2] {
            let zone = Zone::new();
            let mut strm = blank_stream();
            strm.zalloc = Some(refuse_alloc);
            strm.zfree = Some(refuse_free);
            strm.opaque = zone.as_opaque();
            init(&mut strm, 15);

            // Decode in pieces, so the source owns a window: that is the second
            // allocation `inflateCopy` has to make.
            let payload = [b'z'; 300];
            let compressed = deflate_zlib(&payload);
            let mut chunk = [0_u8; 64];
            strm.next_in = compressed.as_ptr();
            strm.avail_in = uInt::try_from(compressed.len()).unwrap();
            loop {
                strm.next_out = chunk.as_mut_ptr();
                strm.avail_out = 64;
                // SAFETY: the stream is initialised and both regions are live.
                let ret = unsafe { inflate(&mut strm, 0) };
                if ret == ReturnCode::STREAM_END.as_i32() {
                    break;
                }
                assert_eq!(ret, ReturnCode::OK.as_i32());
            }
            let taken = zone.with(|zone| zone.requests);
            assert!(taken >= 2, "the source must own a window by now");

            // A destination filled with values nothing in this call may disturb.
            let mut dest = blank_stream();
            dest.next_in = compressed.as_ptr();
            dest.avail_in = 7;
            dest.total_in = 11;
            dest.next_out = chunk.as_mut_ptr();
            dest.avail_out = 13;
            dest.total_out = 17;
            dest.data_type = 19;
            dest.adler = 23;
            dest.reserved = 29;
            let before = dest;

            zone.with(|zone| zone.deny_from = taken + deny_from);
            // SAFETY: both streams are live, stack-owned, distinct and non-overlapping,
            // and `strm` holds a state this test initialised.
            let ret = unsafe { inflateCopy(&mut dest, &mut strm) };
            assert_eq!(
                ret,
                ReturnCode::MEM_ERROR.as_i32(),
                "request {deny_from} of the copy refused"
            );

            assert!(
                dest.next_in == before.next_in
                    && dest.avail_in == before.avail_in
                    && dest.total_in == before.total_in
                    && dest.next_out == before.next_out
                    && dest.avail_out == before.avail_out
                    && dest.total_out == before.total_out
                    && dest.msg == before.msg
                    && dest.state == before.state
                    && dest.data_type == before.data_type
                    && dest.adler == before.adler
                    && dest.reserved == before.reserved,
                "every member of dest must be untouched"
            );
            assert!(
                dest.state.is_null(),
                "including a state that was never installed"
            );

            // The source is still usable, and everything the copy took is back.
            zone.with(|zone| zone.deny_from = 0);
            // SAFETY: the stream is the initialised one.
            assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
            assert_eq!(
                zone.with(|zone| zone.live()),
                0,
                "a refused copy leaks nothing"
            );
        }
    }

    // -----------------------------------------------------------------------
    // The dictionary
    // -----------------------------------------------------------------------

    /// `inflateGetDictionary` writes `*dictLength` and never reads it, and copies the
    /// whole history rather than clamping to whatever the member happened to hold.
    ///
    /// `inflate.c` L1176-L1183 copies `state->whave` bytes and then assigns
    /// `*dictLength = state->whave`; the member is an output. `zlib.h` L936-L942 puts
    /// the capacity promise on the caller -- "32768 bytes is always enough" -- so there
    /// is nothing to clamp against. Reading the member as a capacity would truncate the
    /// copy whenever it held a small value: below it holds zero, the value a caller most
    /// naturally passes, and a clamp to zero would copy nothing at all.
    #[test]
    fn get_dictionary_writes_the_length_and_never_reads_it() {
        let payload: Vec<u8> = (0..600_u32).map(|index| (index % 251) as u8).collect();
        let compressed = deflate_zlib(&payload);

        let mut strm = blank_stream();
        init(&mut strm, 15);
        let mut produced: Vec<u8> = Vec::new();
        let mut chunk = [0_u8; 100];
        strm.next_in = compressed.as_ptr();
        strm.avail_in = uInt::try_from(compressed.len()).unwrap();
        // Decoded in 100-byte pieces, because the window is filled by `updatewindow`
        // (`inflate.c` L1128-L1133) only when output is flushed: a single-shot decode
        // into one large buffer with `Z_FINISH` leaves `whave` at zero, in C as here.
        loop {
            strm.next_out = chunk.as_mut_ptr();
            strm.avail_out = 100;
            // SAFETY: the stream is initialised and both regions are live and disjoint.
            let ret = unsafe { inflate(&mut strm, 0) };
            produced.extend_from_slice(&chunk[..100 - usize::try_from(strm.avail_out).unwrap()]);
            if ret == ReturnCode::STREAM_END.as_i32() {
                break;
            }
            assert_eq!(ret, ReturnCode::OK.as_i32());
        }
        assert_eq!(produced, payload);

        // ★ Zero, as a caller that expects an output would leave it -- and the value a
        // capacity misreading would clamp the copy to, copying nothing at all.
        let mut length: uInt = 0;
        // 0xa5 rather than zero, so that "not written" is distinguishable from "written
        // as zero" -- the payload contains zero bytes.
        let mut dictionary = vec![0xa5_u8; 32768];
        // SAFETY: the stream holds a live state; `dictionary` is 32768 bytes, the space
        // `zlib.h` L936-L942 requires; `length` is a live `uInt`.
        let ret = unsafe {
            inflateGetDictionary(
                &mut strm,
                dictionary.as_mut_ptr(),
                core::ptr::addr_of_mut!(length),
            )
        };
        assert_eq!(ret, ReturnCode::OK.as_i32());

        // 500, not 600, and the number is the reference's. Measured by running this
        // exact fixture -- 600 bytes decoded in 100-byte pieces -- against the C library:
        // it reports `dictLength = 500`, the history `updatewindow` had added by the
        // time the stream ended, and the bytes are the first 500 of the payload because
        // the window has not wrapped. A one-shot `Z_FINISH` decode into one large buffer
        // reports 0 in both implementations, which is why this test feeds chunks.
        let reported = usize::try_from(length).unwrap();
        assert_eq!(
            reported, 500,
            "the length the reference reports for this fixture"
        );
        assert_eq!(
            &dictionary[..reported],
            &payload[..reported],
            "the history is copied in order, despite *dictLength arriving as zero"
        );
        assert!(
            dictionary[reported..].iter().all(|byte| *byte == 0xa5),
            "and nothing beyond the history is written"
        );

        // Either pointer may be null, independently (L1176 and L1182).
        // SAFETY: as above; the nulls are the point of the calls.
        unsafe {
            let mut only_length: uInt = 12345;
            assert_eq!(
                inflateGetDictionary(
                    &mut strm,
                    core::ptr::null_mut(),
                    core::ptr::addr_of_mut!(only_length)
                ),
                ReturnCode::OK.as_i32()
            );
            assert_eq!(usize::try_from(only_length).unwrap(), reported);
            assert_eq!(
                inflateGetDictionary(&mut strm, dictionary.as_mut_ptr(), core::ptr::null_mut()),
                ReturnCode::OK.as_i32()
            );
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
        }
    }

    /// The same on a stream whose output members hold no value, and with no history yet.
    #[test]
    fn get_dictionary_on_a_fresh_stream_reports_nothing_and_reads_nothing() {
        let mut raw = stream_with_no_output_members(15);
        let strm = raw.as_mut_ptr();
        let mut length: uInt = 999;
        let mut dictionary = [0_u8; 32];
        // SAFETY: the stream is the initialised one; both out-parameters are live.
        unsafe {
            assert_eq!(
                inflateGetDictionary(
                    strm,
                    dictionary.as_mut_ptr(),
                    core::ptr::addr_of_mut!(length)
                ),
                ReturnCode::OK.as_i32()
            );
            assert_eq!(length, 0, "no history yet");
            assert_eq!(inflateEnd(strm), ReturnCode::OK.as_i32());
        }
    }

    // -----------------------------------------------------------------------
    // The gzip header, bound per call
    // -----------------------------------------------------------------------

    /// A buffer supplied *after* `inflateGetHeader` is filled, and a capacity enlarged
    /// between calls is honoured.
    ///
    /// C reads `head->extra`, `head->extra_max`, `head->name`, `head->name_max`,
    /// `head->comment` and `head->comm_max` inside the `EXTRA`, `NAME` and `COMMENT`
    /// states -- `inflate.c` L608-L620, L625-L634, L640-L649 -- so the values that
    /// matter are the ones present when the bytes arrive, not when the header was
    /// installed. Reading them once, in `inflateGetHeader`, would ignore anything a caller
    /// did afterwards.
    #[test]
    fn a_header_buffer_supplied_after_get_header_is_still_filled() {
        // FLG = 0x1c: FEXTRA | FNAME | FCOMMENT.
        let mut gz: Vec<u8> = vec![0x1f, 0x8b, 0x08, 0x1c, 0x11, 0x22, 0x33, 0x44, 0x02, 0x03];
        gz.extend_from_slice(&[0x02, 0x00, 0xab, 0xcd]);
        gz.extend_from_slice(b"a-name\0");
        gz.extend_from_slice(b"a-comment\0");
        gz.extend_from_slice(&[0x01, 0x00, 0x00, 0xff, 0xff]);
        gz.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
        gz.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);

        let head = header_on_heap(zeroed_header());

        let mut strm = blank_stream();
        init(&mut strm, 47);
        // Installed with every buffer absent.
        // SAFETY: the stream is initialised and `head` is the live heap header this test
        // allocated and releases only after `inflateEnd`.
        let ret = unsafe { inflateGetHeader(&mut strm, head) };
        assert_eq!(ret, ReturnCode::OK.as_i32());

        // Supplied afterwards, which C honours and a snapshot would not.
        let mut extra = [0_u8; 8];
        let mut name = [0_u8; 16];
        let mut comment = [0_u8; 16];
        // SAFETY: `head` is the live heap header; every write goes through the same raw
        // pointer the library holds, so the provenance the library reads through stays
        // valid -- which is the whole reason the header is not a local.
        unsafe {
            (*head).extra = extra.as_mut_ptr();
            (*head).extra_max = 8;
            (*head).name = name.as_mut_ptr();
            (*head).name_max = 16;
            (*head).comment = comment.as_mut_ptr();
            (*head).comm_max = 16;
        }

        let mut out = [0_u8; 32];
        strm.next_in = gz.as_ptr();
        strm.avail_in = uInt::try_from(gz.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 32;
        // SAFETY: the stream is initialised, every region is live and disjoint, and the
        // header and its three buffers outlive the call.
        let ret = unsafe { inflate(&mut strm, 0) };
        assert!(
            ret == ReturnCode::OK.as_i32() || ret == ReturnCode::STREAM_END.as_i32(),
            "gzip decode gave {ret}"
        );
        // SAFETY: as above.
        unsafe {
            assert_eq!((*head).done, 1, "the header parsed to completion");
            assert_eq!((*head).time, 0x4433_2211);
            assert_eq!((*head).os, 0x03);
        }
        assert_eq!(&extra[..2], &[0xab, 0xcd], "the extra field, bound late");
        assert_eq!(&name[..7], b"a-name\0", "the name, bound late");
        assert_eq!(&comment[..10], b"a-comment\0", "the comment, bound late");
        // SAFETY: the stream is the initialised one, released before the header.
        unsafe {
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
            release_header(head);
        }
    }

    /// A member the parse never assigns keeps the caller's value, and no member is read
    /// to achieve that.
    ///
    /// A zlib stream is not a gzip stream, so `inflate` sets `done` to -1 (`inflate.c`
    /// L509) and assigns nothing else. Every other member must therefore still hold what
    /// the caller put there -- including the four a decoding caller need never
    /// initialise at all.
    #[test]
    fn a_header_the_stream_does_not_carry_leaves_every_member_alone() {
        let payload = b"hello, hello!";
        let compressed = deflate_zlib(payload);

        let head = header_on_heap(gz_header {
            text: 5,
            time: 0xfeed,
            xflags: 6,
            os: 7,
            extra_len: 99,
            hcrc: 8,
            done: 4,
            ..zeroed_header()
        });

        let mut strm = blank_stream();
        // 47 accepts either container, so a zlib stream reaches the gzip-header check.
        init(&mut strm, 47);
        // SAFETY: the stream is initialised and `head` is the live heap header.
        unsafe {
            assert_eq!(inflateGetHeader(&mut strm, head), ReturnCode::OK.as_i32());
            assert_eq!((*head).done, 0, "inflate.c L1229 writes only this one");
            assert_eq!((*head).text, 5);
            assert_eq!((*head).hcrc, 8);
        }

        let mut out = [0_u8; 32];
        strm.next_in = compressed.as_ptr();
        strm.avail_in = uInt::try_from(compressed.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 32;
        // SAFETY: the stream is initialised and every region is live and disjoint -- the
        // cursors were just set from `compressed` and `out`, two distinct locals, with the
        // lengths taken from those objects.
        let ret = unsafe { inflate(&mut strm, 4) };
        assert_eq!(ret, ReturnCode::STREAM_END.as_i32());
        assert_eq!(&out[..payload.len()], payload);

        // SAFETY: as above -- one pointer, one provenance.
        unsafe {
            assert_eq!((*head).done, -1, "inflate.c L509: not a gzip stream");
            assert_eq!((*head).text, 5, "untouched");
            assert_eq!((*head).time, 0xfeed, "untouched");
            assert_eq!((*head).xflags, 6, "untouched");
            assert_eq!((*head).os, 7, "untouched");
            assert_eq!((*head).extra_len, 99, "untouched");
            assert_eq!((*head).hcrc, 8, "untouched");
            assert_eq!(inflateEnd(&mut strm), ReturnCode::OK.as_i32());
            release_header(head);
        }
    }

    /// A build that runs out of space still leaves in the caller's array every entry
    /// it wrote before it stopped.
    ///
    /// ★ C builds straight into the caller's table, so the `return 1` at `inftrees.c`
    /// L287 leaves the entries written so far behind, so this wrapper must build straight
    /// into the caller's table too: building into scratch space and copying out nothing on
    /// failure would discard them. The expected values below are taken from the reference
    /// implementation, by filling the table with a sentinel, calling C's
    /// `inflate_table` with these exact arguments, and recording which entries changed:
    ///
    /// | root | return | entries written | contents |
    /// |---|---|---|---|
    /// | 1 | 1 | 1 | `[0] = {op 16, bits 1, val 1}` |
    /// | 2 | 1 | 3 | `{16,1,1}`, `{16,2,2}`, `{16,1,1}` |
    /// | 15 | 1 | 0 | nothing |
    ///
    /// The writes form a prefix in every case, with no gaps.
    #[test]
    fn a_table_that_runs_out_of_space_publishes_what_it_wrote() {
        let mut lens = [0_u16; 16];
        for (index, slot) in lens.iter_mut().enumerate().take(15) {
            *slot = u16::try_from(index + 1).unwrap();
        }
        lens[15] = 15;

        // The sentinel C's `code` array holds before the call. Any entry still equal to
        // it afterwards is one the build did not write.
        let sentinel = code {
            op: 0xaa,
            bits: 0xaa,
            val: 0xaaaa,
        };
        let expected: [(c_uint, &[code]); 3] = [
            (
                1,
                &[code {
                    op: 16,
                    bits: 1,
                    val: 1,
                }],
            ),
            (
                2,
                &[
                    code {
                        op: 16,
                        bits: 1,
                        val: 1,
                    },
                    code {
                        op: 16,
                        bits: 2,
                        val: 2,
                    },
                    code {
                        op: 16,
                        bits: 1,
                        val: 1,
                    },
                ],
            ),
            (15, &[]),
        ];

        for (requested, written) in expected {
            let mut work = [0_u16; 16];
            let mut table = vec![sentinel; ENOUGH_DISTS];
            let mut next: *mut code = table.as_mut_ptr();
            let mut bits: c_uint = requested;
            // SAFETY: as the test above -- three distinct arrays, each at least as long
            // as the call requires, and two live in/out locals.
            let ret = unsafe {
                _zlib_rs_inflate_table(
                    DISTS,
                    lens.as_mut_ptr().cast::<c_ushort>(),
                    16,
                    &mut next,
                    &mut bits,
                    work.as_mut_ptr().cast::<c_ushort>(),
                )
            };
            assert_eq!(ret, 1, "root {requested}");
            assert_eq!(next, table.as_mut_ptr(), "root {requested}: *table is C's");
            assert_eq!(bits, requested, "root {requested}: *bits is C's");
            assert_eq!(
                &table[..written.len()],
                written,
                "root {requested}: the entries C writes must be published"
            );
            assert!(
                table[written.len()..]
                    .iter()
                    .all(|entry| *entry == sentinel),
                "root {requested}: no entry beyond C's may be touched"
            );
        }
    }

    #[test]
    fn inflate_table_builds_a_valid_set_and_rejects_a_broken_one() {
        let mut work = [0_u16; 32];
        let mut table = vec![code::default(); 1444];

        // A complete two-symbol code: both symbols one bit long.
        let mut lens = [1_u16, 1];
        let mut next: *mut code = table.as_mut_ptr();
        let mut bits: c_uint = 7;
        // SAFETY: `lens` and `work` cover the two codes requested, and `table` is
        // larger than either `ENOUGH_*` bound.
        let ret = unsafe {
            _zlib_rs_inflate_table(
                CODES,
                lens.as_mut_ptr().cast::<c_ushort>(),
                2,
                &mut next,
                &mut bits,
                work.as_mut_ptr().cast::<c_ushort>(),
            )
        };
        assert_eq!(ret, 0, "a complete code set builds");
        assert_eq!(bits, 1, "the root width narrows to the longest code");
        // SAFETY: `next` was advanced within `table`.
        let produced = unsafe { next.offset_from(table.as_mut_ptr()) };
        assert_eq!(produced, 2, "two entries for a two-symbol root table");
        assert_eq!(table[0].bits, 1);
        assert_eq!(table[1].bits, 1);

        // An over-subscribed set: three one-bit codes.
        let mut over = [1_u16, 1, 1];
        let mut next: *mut code = table.as_mut_ptr();
        let mut bits: c_uint = 7;
        // SAFETY: the three pointers all address local arrays that outlive the call:
        // `over` is writable for its own length, and the code and work arrays are
        // sized for the `LENS` root as `inftrees.c` requires. `inflate_table` writes
        // only within those bounds and retains none of them.
        let ret = unsafe {
            _zlib_rs_inflate_table(
                LENS,
                over.as_mut_ptr().cast::<c_ushort>(),
                3,
                &mut next,
                &mut bits,
                work.as_mut_ptr().cast::<c_ushort>(),
            )
        };
        assert_eq!(ret, -1, "an over-subscribed set is invalid");
        assert_eq!(next, table.as_mut_ptr());

        // No symbols at all: C makes a two-entry table that is guaranteed to fail at
        // decode time and reports success (`inftrees.c` L126-L134).
        let mut none = [0_u16; 4];
        let mut next: *mut code = table.as_mut_ptr();
        let mut bits: c_uint = 9;
        // SAFETY: the three pointers all address local arrays that outlive the call:
        // `none` is writable for its own length, and the code and work arrays are
        // sized for the `LENS` root as `inftrees.c` requires. `inflate_table` writes
        // only within those bounds and retains none of them.
        let ret = unsafe {
            _zlib_rs_inflate_table(
                LENS,
                none.as_mut_ptr().cast::<c_ushort>(),
                4,
                &mut next,
                &mut bits,
                work.as_mut_ptr().cast::<c_ushort>(),
            )
        };
        assert_eq!(ret, 0);
        assert_eq!(bits, 1);
        // SAFETY: `next` was advanced within `table`.
        assert_eq!(unsafe { next.offset_from(table.as_mut_ptr()) }, 2);
        assert_eq!(table[0].op, 64, "an invalid-code marker");
        assert_eq!(table[1].op, 64);
    }

    #[test]
    fn inflate_table_refuses_a_bad_code_type_or_a_null_pointer() {
        let mut lens = [1_u16, 1];
        let mut work = [0_u16; 2];
        let mut table = vec![code::default(); 1444];
        let mut base: *mut code = table.as_mut_ptr();
        let mut bits: c_uint = 7;

        // SAFETY: every non-null argument is live; the nulls are the point of each
        // call and each is tested before use.
        unsafe {
            for bad_type in [-1, 3, 4, c_int::MAX, c_int::MIN] {
                assert_eq!(
                    _zlib_rs_inflate_table(
                        bad_type,
                        lens.as_mut_ptr().cast::<c_ushort>(),
                        2,
                        &mut base,
                        &mut bits,
                        work.as_mut_ptr().cast::<c_ushort>()
                    ),
                    -1,
                    "codetype {bad_type}"
                );
            }
            assert_eq!(
                _zlib_rs_inflate_table(
                    CODES,
                    core::ptr::null_mut(),
                    2,
                    &mut base,
                    &mut bits,
                    work.as_mut_ptr().cast::<c_ushort>()
                ),
                -1
            );
            assert_eq!(
                _zlib_rs_inflate_table(
                    CODES,
                    lens.as_mut_ptr().cast::<c_ushort>(),
                    2,
                    core::ptr::null_mut(),
                    &mut bits,
                    work.as_mut_ptr().cast::<c_ushort>()
                ),
                -1
            );
            assert_eq!(
                _zlib_rs_inflate_table(
                    CODES,
                    lens.as_mut_ptr().cast::<c_ushort>(),
                    2,
                    &mut base,
                    core::ptr::null_mut(),
                    work.as_mut_ptr().cast::<c_ushort>()
                ),
                -1
            );
            assert_eq!(
                _zlib_rs_inflate_table(
                    CODES,
                    lens.as_mut_ptr().cast::<c_ushort>(),
                    2,
                    &mut base,
                    &mut bits,
                    core::ptr::null_mut()
                ),
                -1
            );
            // A null *inside* `table` is refused too.
            let mut null_base: *mut code = core::ptr::null_mut();
            assert_eq!(
                _zlib_rs_inflate_table(
                    CODES,
                    lens.as_mut_ptr().cast::<c_ushort>(),
                    2,
                    &mut null_base,
                    &mut bits,
                    work.as_mut_ptr().cast::<c_ushort>()
                ),
                -1
            );
        }
        // Nothing above may have disturbed the caller's cursors.
        assert_eq!(base, table.as_mut_ptr());
        assert_eq!(bits, 7);
        assert_eq!(lens, [1, 1]);
    }

    // -----------------------------------------------------------------------
    // Width conversions
    // -----------------------------------------------------------------------

    #[test]
    fn the_width_conversions_round_trip_within_uLong() {
        for value in [0_u64, 1, 0xffff, 0xffff_ffff] {
            assert_eq!(widen_uLong(narrow_uLong(value)), value, "value {value}");
        }
        // The widest value `uLong` can hold, whatever its width on this target.
        let widest = widen_uLong(c_ulong::MAX);
        assert_eq!(widen_uLong(narrow_uLong(widest)), widest);
        assert_eq!(narrow_uLong(INFLATE_CODES_USED_BAD_STATE), c_ulong::MAX);
        assert_eq!(BAD_STATE_MARK, -(1 << 16));
    }

    // -----------------------------------------------------------------------
    // The overlap snapshot is the caller's allocation, not Rust's
    // -----------------------------------------------------------------------

    /// An overlapping `inflate` takes its snapshot through the caller's `zalloc`, and a
    /// caller who refuses it gets an error rather than an abort.
    ///
    /// The companion of `deflate.rs`'s pair, for the other streaming entry point. The
    /// snapshot is `avail_in` bytes -- a number the *caller* chose -- so `zlib.h`
    /// L140-L153 makes it the caller's allocator's business: a custom arena has to see
    /// it, and `test/infcover.c`'s `mem_limit()` has to be able to refuse it. Reaching
    /// past the hooks to Rust's global allocator would put a caller-sized request outside
    /// both, and would turn a refusal into a process abort.
    #[test]
    fn an_overlapping_inflate_snapshots_through_the_callers_hooks() {
        const PAYLOAD: &[u8] = b"hello, hello! hello, hello! hello, hello!";
        let compressed = deflate_zlib(PAYLOAD);

        // One buffer, read and written at once: the compressed stream sits at its front
        // and the decoder writes over it from the same address.
        let mut shared = vec![0_u8; PAYLOAD.len() + compressed.len() + 64];
        shared[..compressed.len()].copy_from_slice(&compressed);

        let zone = Zone::new();
        let mut strm = blank_stream();
        strm.zalloc = Some(refuse_alloc);
        strm.zfree = Some(refuse_free);
        strm.opaque = zone.as_opaque();
        init(&mut strm, 15);

        let after_init = zone.with(|zone| (zone.requests, zone.live()));
        strm.next_in = shared.as_ptr();
        strm.avail_in = uInt::try_from(compressed.len()).unwrap();
        strm.next_out = shared.as_mut_ptr();
        strm.avail_out = uInt::try_from(shared.len()).unwrap();

        // SAFETY: a live stream whose two buffers are both inside `shared`, which is the
        // overlap this path exists to serve.
        let status = unsafe { inflate(&mut strm, 4) };
        assert_eq!(
            status,
            ReturnCode::STREAM_END.as_i32(),
            "an overlapping pair runs, exactly as it does in C"
        );
        assert_eq!(usize::try_from(strm.total_out).unwrap(), PAYLOAD.len());
        assert_eq!(
            &shared[..PAYLOAD.len()],
            PAYLOAD,
            "the snapshot is what was decoded, so the output is the original bytes"
        );

        let after_call = zone.with(|zone| (zone.requests, zone.live()));
        assert_eq!(
            after_call.0,
            after_init.0 + 1,
            "the snapshot is one further request through the caller's zalloc"
        );
        assert_eq!(
            after_call.1, after_init.1,
            "and it was returned through the caller's zfree before inflate returned"
        );

        // A caller who refuses the snapshot is answered, not aborted. `Z_STREAM_ERROR` is
        // the status an unservable buffer pair earns, the same class of argument fault C
        // rejects for a null pair -- see `AliasScratch` in `src/types.rs`.
        // SAFETY: `strm` is the live, initialised stream this test owns.
        let reset = unsafe { inflateReset(&mut strm) };
        assert_eq!(reset, ReturnCode::OK.as_i32());
        shared[..compressed.len()].copy_from_slice(&compressed);
        zone.with(|zone| zone.deny_from = zone.requests + 1);
        strm.next_in = shared.as_ptr();
        strm.avail_in = uInt::try_from(compressed.len()).unwrap();
        strm.next_out = shared.as_mut_ptr();
        strm.avail_out = uInt::try_from(shared.len()).unwrap();
        // SAFETY: as above.
        let status = unsafe { inflate(&mut strm, 4) };
        assert_eq!(
            status,
            ReturnCode::STREAM_ERROR.as_i32(),
            "a refused snapshot is reported"
        );
        assert_eq!(strm.total_in, 0, "nothing was consumed");
        assert_eq!(strm.total_out, 0, "nothing was emitted");

        zone.with(|zone| zone.deny_from = 0);
        // SAFETY: `strm` is the live, initialised stream this test owns.
        let ended = unsafe { inflateEnd(&mut strm) };
        assert_eq!(ended, ReturnCode::OK.as_i32());
        // `Zone::drop` asserts the zone owns nothing, which is the leak check.
    }
}
