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
//! [`inflate_table`] is not part of the public API — `zlib.map`'s `ZLIB_1.2.0`
//! `local:` block (L9-L19) names it explicitly, and it is absent from the
//! reference shared library's dynamic table. It is exported here anyway, because
//! **the unmodified `test/infcover.c` does not link without it**: `cover_trees`
//! (its L618-L639) calls the C symbol directly, twice, in order to provoke the
//! `ENOUGH`-exceeded return that a well-behaved `inflate()` never can.
//!
//! The two facts coexist because they are about two different artifacts.
//! `--version-script=zlib.map` is a *shared object* link argument, so it localises
//! the name in `libz.so`; the `staticlib` archive is not linked at all, so the
//! name stays a global `T` in `libz.a`, which is what `infcover` links against
//! (`Makefile.in` L121-L122, and decisively `test/CMakeLists.txt` L95-L96, which
//! links it to `ZLIB::ZLIBSTATIC`). That is exactly what the C build produces, and
//! the 111-symbol parity diff polices both halves automatically.
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
//! | 2 — slice reconstruction | [`inflate`], [`inflateSync`], [`inflateSetDictionary`], [`inflateGetDictionary`], [`inflate_table`] |
//! | 3 — the opaque `state` round-trip | [`entry`], [`inflateEnd`], [`inflateCopy`] |
//! | 4 — invoking `zalloc`/`zfree` | [`inflateInit2_`], [`inflateEnd`], [`inflateCopy`], and every state recovery |
//! | 6 — writing through a caller's `gz_header` | [`inflateGetHeader`] and [`Session::publish_header`] |
//!
//! Category 5 does not arise: nothing here takes a C string or a `va_list`.
//! `inflate_table` handles three raw arrays rather than a stream, which is
//! category-2 work in a different shape, and it is documented as such at its own
//! site.
//!
//! # Panic discipline
//!
//! `inflate` is the library's primary untrusted-input surface: `fuzz_inflate`
//! feeds it arbitrary bytes and it must never panic, hang or over-read for any of
//! them. No `unwrap`, `expect`, `panic!` or panicking index appears in this
//! module, every fallible step answers a [`ReturnCode`], and each body is wrapped
//! by [`crate::panic_guard`] so that a panic which somehow occurred would abort
//! rather than unwind into a C caller.
//!
//! # Error messages
//!
//! All eleven texts the decoder can publish in [`z_stream::msg`] are produced by
//! [`zlib_rs::inflate`] as `&'static str` literals — `"incorrect header check"`,
//! `"unknown compression method"`, `"invalid window size"`, `"unknown header
//! flags set"`, `"header crc mismatch"`, `"invalid distance too far back"` and the
//! rest. They are forwarded verbatim, never rewritten, and always as a pointer
//! into `'static` memory, so a caller may hold the pointer for as long as it
//! likes. [`message_ptr`] is the single place that conversion happens.

// `zlib.h` names these functions in camelCase, and the names are the ABI. The
// crate root already relaxes `non_camel_case_types` for the same reason; this is the
// function-name half of it, needed because `#[no_mangle]` names must match `zlib.h`
// character for character.
#![allow(non_snake_case)]

use core::cell::Cell;
use core::ffi::{c_char, c_int, c_long, c_uint, c_ulong, c_ushort};
use core::mem::size_of;

use zlib_rs::config::{InflateConfig, DEF_WBITS};
use zlib_rs::error::ReturnCode;
use zlib_rs::inflate::inftrees::{
    inflate_table as core_inflate_table, Code, CodeType, ENOUGH_DISTS, ENOUGH_LENS,
    TABLE_INVALID_CODE, TABLE_NOT_ENOUGH,
};
use zlib_rs::inflate::state::{GzHeaderSink, StreamReset};
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

use crate::panic_guard::{fallback, guard, guard_code};
use crate::types::{
    checked_state_mut, code, code_type_from_raw, gz_headerp, input_slice, install_state,
    output_slice_mut, take_state, uInt, uLong, widen, z_stream, z_streamp, Bytef, StateBlock,
    StateKind, StreamAllocator,
};

/// The largest `flush` value [`inflate`] accepts: `Z_TREES` (`zlib.h` L178).
///
/// ★ Inflate's flush domain is **wider** than deflate's. `Z_BLOCK` (5) asks it to
/// stop at a deflate block boundary and `Z_TREES` (6) to stop immediately after a
/// block header, and both are meaningful here even though `deflate` rejects the
/// second. Values outside `0..=6` are refused.
const MAX_INFLATE_FLUSH: c_int = 6;

/// The `mode` tag a freshly created state carries: `HEAD`, i.e. 16180.
///
/// `inflate.c` L205 assigns it before `inflateReset2` runs, with the comment "to
/// pass state test in `inflateReset2()`". The same value seeds
/// [`crate::types::StateBlock`]'s prefix here, so a state passes its own tag check
/// from the moment it exists.
const HEAD_TAG: c_int = Mode::Head.as_raw();

/// The largest window a zlib stream can use, in bytes: `1 << MAX_WBITS` = 32768.
///
/// `zconf.h` L287 fixes `MAX_WBITS` at 15, and `zlib.h` L936-L942 requires a caller
/// of [`inflateGetDictionary`] that passes no `dictLength` to provide at least this
/// much space. It is therefore the capacity such a caller is assumed to have, and
/// since it is also the largest window the decoder can own, no history is ever lost
/// by assuming it.
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
    // a null or misaligned stream and for a half-supplied hook pair, which is the
    // `zalloc == 0 || zfree == 0` half of `inflateStateCheck`.
    let allocator = unsafe { StreamAllocator::from_stream_ptr(strm) }?;

    // SAFETY: unsafe-site category 3 -- the opaque `state` round-trip. The four
    // remaining `inflateStateCheck` tests run inside, and this function's contract
    // supplies the liveness and provenance requirements they cannot test. The
    // borrow is taken once and is the only one for `'s`.
    let block: &mut StateBlock<InflateSlot> =
        unsafe { checked_state_mut(strm, StateKind::Inflate) }?;

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
        if self.state().header_sink().is_none() {
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
    /// from the totals, which the core maintains itself.
    ///
    /// # Safety
    ///
    /// `self.strm` must be the live, validated stream [`entry`] produced, and its
    /// `(next_in, avail_in)` and `(next_out, avail_out)` pairs must describe
    /// readable and writable regions that do not overlap — the contract `zlib.h`
    /// L138-L139 places on the application.
    unsafe fn with_buffers<F>(&mut self, body: F) -> ReturnCode
    where
        F: FnOnce(
            &mut InflateState<'static, StreamAllocator>,
            &mut InflateStream<'_>,
        ) -> ReturnCode,
    {
        let strm = self.strm;

        // SAFETY: unsafe-site category 1 -- reading eight members of the caller's
        // stream through raw places. `strm` is non-null, aligned and live by
        // `entry`'s contract, so every member is in bounds; all eight are plain
        // `Copy` scalars or pointers and none is dereferenced here.
        let (next_in, avail_in, next_out, avail_out, total_in, total_out, adler, data_type) = unsafe {
            (
                core::ptr::addr_of!((*strm).next_in).read(),
                core::ptr::addr_of!((*strm).avail_in).read(),
                core::ptr::addr_of!((*strm).next_out).read(),
                core::ptr::addr_of!((*strm).avail_out).read(),
                core::ptr::addr_of!((*strm).total_in).read(),
                core::ptr::addr_of!((*strm).total_out).read(),
                core::ptr::addr_of!((*strm).adler).read(),
                core::ptr::addr_of!((*strm).data_type).read(),
            )
        };

        // SAFETY: unsafe-site category 2 -- slice reconstruction, performed exactly
        // once per call. `input_slice` and `output_slice_mut` each branch on a zero
        // length or a null pointer and return an empty slice rather than calling
        // `from_raw_parts` with one, so the legal `avail_in == 0` with
        // `next_in == Z_NULL` state is not undefined behaviour. The two regions do
        // not overlap by this function's contract, so the shared and mutable borrows
        // cannot alias.
        let input = unsafe { input_slice(next_in, avail_in) };
        // SAFETY: unsafe-site category 2, as above.
        let output = unsafe { output_slice_mut(next_out, avail_out) };

        let mut stream = InflateStream {
            input,
            next_in: 0,
            output,
            next_out: 0,
            total_in: widen_uLong(total_in),
            total_out: widen_uLong(total_out),
            msg: self.block.state().msg,
            adler: narrow_check(adler),
            data_type,
        };

        let outcome = body(&mut self.block.state_mut().state, &mut stream);

        let consumed = stream.next_in;
        let produced = stream.next_out;
        let published = core::mem::replace(&mut self.block.state_mut().msg, stream.msg);

        // SAFETY: unsafe-site category 2 -- advancing the caller's cursors. `consumed`
        // is bounded by `input.len()`, which is `avail_in` when `next_in` is non-null
        // and zero when it is null, so `add` is reached only for a non-null pointer
        // and only with an offset inside the caller's own buffer or one past its end
        // -- which is exactly what C's `next_in += have` produces once the input is
        // fully consumed. The zero guard is what keeps a null `next_in` out of `add`,
        // where an offset of zero would still be undefined behaviour.
        let advanced_in = if consumed == 0 {
            next_in
        } else {
            unsafe { next_in.add(consumed) }
        };
        // SAFETY: unsafe-site category 2, as above, with `produced` bounded by
        // `output.len()`, which is `avail_out` when `next_out` is non-null.
        let advanced_out = if produced == 0 {
            next_out
        } else {
            unsafe { next_out.add(produced) }
        };

        // SAFETY: unsafe-site category 1 -- writing the members C's epilogue writes.
        // `strm` is the same live, aligned stream read above, so every member is in
        // bounds and writable, and each write goes through a raw place so the
        // caller's pointer keeps its provenance. The `msg` write is skipped unless
        // the text changed, so an unchanged diagnostic keeps the exact pointer the
        // caller already saw -- which is what C does by never rewriting it.
        unsafe {
            core::ptr::addr_of_mut!((*strm).next_in).write(advanced_in);
            core::ptr::addr_of_mut!((*strm).avail_in)
                .write(narrow_uInt(widen(avail_in).saturating_sub(consumed)));
            core::ptr::addr_of_mut!((*strm).next_out).write(advanced_out);
            core::ptr::addr_of_mut!((*strm).avail_out)
                .write(narrow_uInt(widen(avail_out).saturating_sub(produced)));
            core::ptr::addr_of_mut!((*strm).total_in).write(narrow_uLong(stream.total_in));
            core::ptr::addr_of_mut!((*strm).total_out).write(narrow_uLong(stream.total_out));
            core::ptr::addr_of_mut!((*strm).adler).write(uLong::from(stream.adler));
            core::ptr::addr_of_mut!((*strm).data_type).write(stream.data_type);
            if published != stream.msg {
                core::ptr::addr_of_mut!((*strm).msg).write(message_ptr(stream.msg));
            }
        }

        outcome
    }

    /// Copies the parsed gzip-header scalars into the caller's `gz_header`.
    ///
    /// ★ **This is unsafe-site category 6, and it is where a documented
    /// compatibility gap is closed.** C's header states write straight through
    /// `state->head`, so the caller's structure is updated field by field as the
    /// parse proceeds (`inflate.c` L566, L577, L586-L587, L596, L601, L631, L666,
    /// L678, L687). The safe core cannot hold a raw pointer, so it parses into a
    /// [`GzHeaderSink`] and this method publishes the result. Called after every
    /// [`inflate`], because that is the only entry point that parses a header.
    ///
    /// # Why writing every scalar back is faithful, not lossy
    ///
    /// `inflateGetHeader` writes **only** `head->done = 0` and leaves the caller's
    /// other fields untouched, so a caller that pre-filled `text`, `time`, `xflags`
    /// or `os` and then decoded a stream which turned out not to be gzip sees its own
    /// values preserved. [`GzHeaderSink::new`] records that difference as an open
    /// gap and names this entry point as its owner.
    ///
    /// It is closed by *seeding*: [`inflateGetHeader`] copies the caller's current
    /// scalars into the sink before installing it, so a field the parse never assigns
    /// still holds the caller's value when it is written back here, and the round
    /// trip is the identity. Nothing is invented and nothing is cleared.
    ///
    /// # The three variable-length fields
    ///
    /// `extra`, `name` and `comment` need no copy: the sink holds them as
    /// `&[Cell<u8>]` views over the caller's own buffers, clamped to `extra_max`,
    /// `name_max` and `comm_max`, so a byte written by the `EXTRA`, `NAME` or
    /// `COMMENT` state is already in the caller's memory. What *is* published here
    /// is the three `Z_NULL` assignments the reference makes when a field is absent
    /// from the stream — L601, L631 and L666 — because a caller can observe them.
    fn publish_header(&mut self) {
        let head = self.block.state().head;
        if head.is_null() {
            return;
        }
        let Some(sink) = self.state().header_sink() else {
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
        // writes below only store `Z_NULL` into the pointer members.
        unsafe {
            core::ptr::addr_of_mut!((*head).text).write(c_int::from(sink.text));
            core::ptr::addr_of_mut!((*head).time).write(uLong::from(sink.time));
            core::ptr::addr_of_mut!((*head).xflags).write(sink.xflags);
            core::ptr::addr_of_mut!((*head).os).write(sink.os);
            core::ptr::addr_of_mut!((*head).extra_len).write(sink.extra_len);
            core::ptr::addr_of_mut!((*head).hcrc).write(c_int::from(sink.hcrc));
            core::ptr::addr_of_mut!((*head).done).write(sink.done);
            if sink.extra_is_absent() {
                core::ptr::addr_of_mut!((*head).extra).write(core::ptr::null_mut());
            }
            if sink.name_is_absent() {
                core::ptr::addr_of_mut!((*head).name).write(core::ptr::null_mut());
            }
            if sink.comment_is_absent() {
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

/// Narrows a caller's `adler` member to the 32 bits a check value occupies.
///
/// `z_stream.adler` is a `uLong` (`zlib.h` L106) but both an Adler-32 and a CRC-32
/// fit in 32 bits, so the high half is always zero on LP64 and absent on LLP64. The
/// mask makes that explicit rather than assumed, and the fallback -- unreachable
/// once masked -- keeps the conversion free of a panicking path.
#[inline]
#[must_use]
fn narrow_check(value: uLong) -> u32 {
    u32::try_from(value & uLong::from(u32::MAX)).unwrap_or(u32::MAX)
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
/// actually changed — [`Session::publish_stream`] compares against the value the
/// stream already published before calling — so it is off the hot path entirely.
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
    // SAFETY: unsafe-site category 2 -- one byte of a caller's C string. The pointer
    // is non-null by the test above and readable for at least one byte by this
    // function's contract. Only `version[0]` is read, exactly as `inflate.c` L178
    // reads it, so no NUL scan and no length is needed. The `cast` to `*const u8`
    // sidesteps the signedness of `c_char`, which is `i8` on x86-64 and `u8` on
    // several ARM targets: both are one byte with alignment one, so the read is
    // valid either way and the comparison below needs no sign-dependent cast.
    let major = unsafe { version.cast::<u8>().read() };
    // `ZLIB_VERSION` is a non-empty literal, so `first` is always `Some(b'1')`;
    // reaching for it with `first()` rather than an index keeps the panic-free
    // property structural instead of argued.
    let matches = crate::util::ZLIB_VERSION.to_bytes().first() == Some(&major);
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
/// for a null stream, a half-supplied allocator pair or an unacceptable
/// `windowBits`, or [`Z_MEM_ERROR`](ReturnCode::MEM_ERROR) if the state cannot be
/// allocated.
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
        unsafe {
            core::ptr::addr_of_mut!((*strm).msg).write(core::ptr::null());
        }

        // L183-L196: the hook defaulting. `StreamAllocator` expresses "both null,
        // use the library's own" as a mode rather than by overwriting the caller's
        // fields, and refuses a half-supplied pair -- see its own documentation for
        // why reproducing C's independent defaulting would reproduce a
        // mismatched-allocator hazard.
        // SAFETY: unsafe-site category 4 -- reads the three hook members through raw
        // places, forming no reference, so `strm` stays usable below.
        let allocator = unsafe { StreamAllocator::from_stream_ptr(strm) };
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
/// A value outside `0..=6` is refused with `Z_STREAM_ERROR`.
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
/// This is the library's primary attack surface. `fuzz/fuzz_targets/fuzz_inflate.rs`
/// drives it with arbitrary bytes, and it must never panic, hang or over-read for any
/// of them. The decoder is safe Rust throughout, so an out-of-range distance or a
/// malformed table is a `Z_DATA_ERROR` rather than a memory-safety event.
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
        // SAFETY: unsafe-site categories 1, 3 and 4 -- see `entry`. `strm` is a live
        // stream or null by this function's contract, and no other borrow exists.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };

        // ★ `flush` is validated but never rejected by the core, which treats an
        // unknown value as `Z_NO_FLUSH` -- the reference's own behaviour. C's
        // `inflate` has no flush validation at all, so rejecting an out-of-range
        // value here is the one place this function is *stricter* than C. It is safe
        // to be: `zlib.h` L172-L178 defines exactly seven values, no caller can
        // legitimately pass another, and a stream error is the documented answer for
        // a bad argument. The session is closed first so the tag is still written
        // back on this path.
        if !(0..=MAX_INFLATE_FLUSH).contains(&flush) {
            session.finish();
            return fallback::STREAM_ERROR_CODE;
        }

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

            // L497-L1152: the whole engine, with `LOAD`/`RESTORE` and the epilogue
            // handled by `with_buffers`.
            // SAFETY: the stream is the validated one and its two regions are
            // readable, writable and disjoint by this function's contract, which is
            // exactly what `with_buffers` requires.
            let outcome =
                unsafe { session.with_buffers(|state, stream| core_inflate(state, stream, flush)) };

            // The gzip header the parse may have filled. C has already written it
            // through `state->head`; this is the same publication, deferred to the
            // one point where the sink can be read back.
            session.publish_header();

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
        let Some(block) = block else {
            return fallback::STREAM_ERROR_CODE;
        };

        // Always `Z_OK`: the window is already gone, so nothing here can fail.
        core_inflate_end(block.into_state().state)
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
/// state pointer — which is the assignment [`entry`] exists to honour.
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
        // SAFETY: unsafe-site categories 1, 3 and 4 -- see `entry`.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };

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
/// `zlib.h` L936-L942 requires the caller's buffer to hold at least 32768 bytes and
/// gives the function no way to know its size. This implementation additionally
/// clamps the copy to `*dictLength` when the caller supplied one, so a caller that
/// declares a smaller buffer receives a prefix rather than an overrun. `*dictLength`
/// still reports the full `whave`, as C does, so the caller can tell it was
/// truncated.
///
/// # Safety
///
/// `strm`, if non-null, must be a live initialised [`z_stream`]. If `dictionary` is
/// non-null it must be writable for at least `*dictLength` bytes when `dictLength`
/// is non-null, and for at least 32768 bytes otherwise — the contract `zlib.h`
/// L936-L942 states. `dictLength`, if non-null, must be a readable and writable
/// `uInt`.
#[no_mangle]
pub unsafe extern "C" fn inflateGetDictionary(
    strm: z_streamp,
    dictionary: *mut Bytef,
    dictLength: *mut uInt,
) -> c_int {
    guard_code(|| {
        // L1172: the state check.
        // SAFETY: unsafe-site categories 1, 3 and 4 -- see `entry`.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };

        // The capacity the caller declared, if it declared one. C reads nothing here
        // and simply trusts the pointer; reading `*dictLength` first is what lets the
        // copy below be clamped, and it is harmless because the member is required to
        // be readable whenever it is non-null.
        // SAFETY: unsafe-site category 2 -- one `uInt` behind a caller's pointer,
        // read only after the null test and readable by this function's contract.
        let declared = if dictLength.is_null() {
            None
        } else {
            Some(unsafe { dictLength.read() })
        };

        // `zlib.h` L936-L942 fixes the guaranteed capacity at 32768 -- the largest
        // window -- for a caller that passes no length. Anything the window can hold
        // fits, so no history is ever lost by the clamp.
        let capacity = declared.map_or(MAX_WINDOW_BYTES, widen);

        let target = if dictionary.is_null() {
            // L1176's `dictionary != Z_NULL` test: the caller asked only for the
            // length, so nothing is copied.
            None
        } else {
            // SAFETY: unsafe-site category 2 -- slice reconstruction over the
            // caller's dictionary buffer, performed once. `dictionary` is non-null by
            // the test above and writable for `capacity` bytes by this function's
            // contract. The borrow cannot alias the state, which lives in a different
            // allocation, nor the `dictLength` slot, which is a `uInt` rather than
            // part of this buffer.
            Some(unsafe { output_slice_mut(dictionary, narrow_uInt(capacity)) })
        };

        session.run(|session| {
            let mut length: u32 = 0;
            let outcome = core_get_dictionary(session.state(), target, Some(&mut length));
            if !dictLength.is_null() {
                // L1182-L1183: `*dictLength = state->whave` -- the full history
                // length, even when the copy above was clamped, so the caller can
                // tell that its buffer was too small.
                // SAFETY: unsafe-site category 2 -- one `uInt` behind a caller's
                // pointer, non-null by the test above and writable by this
                // function's contract.
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
        // SAFETY: unsafe-site categories 1, 3 and 4 -- see `entry`.
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
        // SAFETY: unsafe-site categories 1, 3 and 4 -- see `entry`.
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
        // SAFETY: unsafe-site categories 1, 3 and 4 -- see `entry`.
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
        // SAFETY: unsafe-site categories 1, 3 and 4 -- see `entry`.
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
        // SAFETY: unsafe-site categories 1, 3 and 4 -- see `entry`.
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
        // SAFETY: unsafe-site categories 1, 3 and 4 -- see `entry`.
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
        // SAFETY: unsafe-site categories 1, 3 and 4 -- see `entry`.
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
        // SAFETY: unsafe-site categories 1, 3 and 4 -- see `entry`.
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
        // SAFETY: unsafe-site categories 1, 3 and 4 -- see `entry`.
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
/// # Safety
///
/// As [`inflate`], and the input region must be readable for `avail_in` bytes.
#[no_mangle]
pub unsafe extern "C" fn inflateSync(strm: z_streamp) -> c_int {
    guard_code(|| {
        // L1273: the state check.
        // SAFETY: unsafe-site categories 1, 3 and 4 -- see `entry`.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };
        session.run(|session| {
            // L1274-L1309. The `avail_in == 0 && bits < 8` guard, the accumulator
            // drain and the internal reset all belong to the core; this supplies the
            // buffers and takes the updated cursors and counters back.
            //
            // ★ The output buffer is rebuilt even though `inflateSync` never writes
            // it, because the core's reset step publishes `msg`, `data_type` and
            // `adler` through the same value. C writes those members directly, so
            // they are part of this function's observable effect too.
            // SAFETY: the stream is the validated one and its two regions satisfy
            // `with_buffers`' contract by this function's own contract.
            unsafe { session.with_buffers(core_inflate_sync) }
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
        // ★ Not a C check, and it cannot be one: C's `zmemcpy` of a struct onto
        // itself is harmless, whereas here the two `z_stream`s must be distinct for
        // the state install below to be meaningful -- installing into `source` would
        // overwrite the very state being cloned and leak it. Rejecting the aliased
        // call is the only sound answer, and no caller can want it: a stream copied
        // onto itself is a no-op the caller could simply not make.
        if core::ptr::eq(dest, source) {
            return fallback::STREAM_ERROR_CODE;
        }

        // L1336: `inflateStateCheck(source)`.
        // SAFETY: unsafe-site categories 1, 3 and 4 -- see `entry`.
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
            // `test/infcover.c`'s zone increments `notlifo` for any free that is not
            // of its most recent allocation (its L120), and `inflateEnd` releases the
            // window before the block; so the block has to be the older of the two.
            // Allocating the window first produces a "frees not LIFO" report --
            // observed, not hypothesised.
            //
            // The placeholder that reserves the block owns nothing: `inflate_init2`
            // leaves `window` absent, exactly as `inflate.c` L204 does, because the
            // window is allocated lazily by the first `inflate()` that needs one.
            // `DEF_WBITS` is always valid, so this cannot fail for a reason of its
            // own.
            let placeholder = match core_inflate_init2(InflateConfig::new(DEF_WBITS), allocator) {
                Ok(placeholder) => placeholder,
                Err(error) => return error,
            };
            let slot = InflateSlot {
                state: placeholder,
                // C copies `head` verbatim, so both streams write into the caller's
                // one `gz_header` -- which is the reference behaviour, not an
                // oversight, and the core reproduces it by copying the sink.
                head: session.block.state().head,
                msg: session.block.state().msg,
            };
            // L1365: `dest->state = (struct internal_state FAR *)copy;`.
            // SAFETY: unsafe-site categories 1, 3 and 4 -- installs the state block.
            // `dest` is non-null, aligned and, by this function's contract, a live
            // `z_stream` distinct from `source` that holds no state of its own. The
            // block is allocated through the source's hooks and can only be released
            // through the matching `zfree`.
            let installed = unsafe { install_state(dest, &allocator, tag, slot) };
            let mut installed = match installed {
                Ok(installed) => installed,
                Err(error) => return error,
            };

            // L1344-L1351 and L1353-L1364: the window, then every scalar. Only
            // `whave` bytes of the window are copied (L1363), never all `wsize` of
            // them, because the rest has never been written and copying it would
            // propagate whatever the allocator left there -- `0xa5` under the
            // coverage harness's allocator.
            let cloned = match core_inflate_copy(session.state(), allocator) {
                Ok(cloned) => cloned,
                // L1345-L1348: `ZFREE(source, copy); return Z_MEM_ERROR;`. The block
                // just installed is released and `dest->state` cleared, so the failure
                // leaks nothing and the release is of the most recent allocation,
                // hence LIFO. `dest`'s other members are still untouched, because the
                // bulk copy below has not run.
                Err(error) => {
                    // SAFETY: unsafe-site categories 3 and 4 -- releasing the block
                    // this function installed moments ago, through the same allocator
                    // and with the same `S`. `installed` is not used again afterwards.
                    drop(unsafe {
                        take_state::<InflateSlot>(dest, &allocator, StateKind::Inflate)
                    });
                    return error;
                }
            };

            // Move the real state into the reserved block. The placeholder it replaces
            // owns no window and no other allocation, so dropping it releases nothing
            // and cannot disturb the LIFO order established above.
            // SAFETY: unsafe-site category 3 -- `installed` is the block this function
            // allocated, it is initialised, and no other borrow of it exists.
            unsafe {
                installed.as_mut().state_mut().state = cloned;
            }

            // L1352: `zmemcpy(dest, source, sizeof(z_stream))`, with `state` carrying
            // the block just installed rather than the source's.
            // SAFETY: unsafe-site category 1 -- one whole-struct read and one
            // whole-struct write. `source` is the validated live stream and `read`
            // forms a raw copy rather than a reference, so the borrow this `Session`
            // holds on the state block -- a different allocation -- is undisturbed.
            // `dest` is non-null, aligned, writable for a `z_stream` and distinct
            // from `source` by this function's contract, so the write cannot overlap
            // the read. `z_stream` is `Copy` plain data with no `Drop`, so
            // overwriting the destination drops nothing.
            unsafe {
                let mut snapshot: z_stream = source.read();
                snapshot.state = installed.as_ptr().cast();
                dest.write(snapshot);
            }

            ReturnCode::OK
        })
    })
}

// ---------------------------------------------------------------------------
// The gzip header -- `inflate.c` L1219-L1231, unsafe-site category 6
// ---------------------------------------------------------------------------

/// Rebuilds one optional `gz_header` buffer as a shared slice of [`Cell`].
///
/// # ★ Why `&[Cell<u8>]` and not `&mut [u8]`
///
/// This is forced by the test suite rather than chosen. `test/infcover.c` L305-L310
/// points **all three** of `extra`, `name` and `comment` at the same `out` buffer with
/// the same length. Holding them as three `&mut [u8]` would require three aliasing
/// mutable borrows of one allocation, which is undefined behaviour and would be
/// reported by both Miri and AddressSanitizer — for a test that must pass unmodified.
///
/// A shared reference to a slice of [`Cell<u8>`] resolves it exactly: `Cell<u8>` is
/// `repr(transparent)` over `u8` so the representation is unchanged, overlapping
/// *shared* references are perfectly legal, and interior mutability makes writing
/// through them safe. [`GzHeaderSink`] documents the same reasoning from the core's
/// side.
///
/// The slice length **is** the caller's advertised capacity, which is what makes the
/// core's clamped writes unable to overrun: `extra_max`, `name_max` and `comm_max`
/// travel with their pointers instead of alongside them.
///
/// # Safety
///
/// If `buffer` is non-null it must be valid for reads and writes of `capacity` bytes
/// for as long as the header stays installed, and nothing outside this library may
/// access it concurrently. Overlap with the other two buffers is explicitly
/// permitted.
unsafe fn header_field(buffer: *mut Bytef, capacity: uInt) -> Option<&'static [Cell<u8>]> {
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
    // contract, valid for `capacity` bytes for as long as the header is installed.
    // `Cell<u8>` is `repr(transparent)` over `u8`, so the cast changes no layout and
    // the alignment requirement is one byte. A *shared* slice of `Cell` is what makes
    // this sound when two of the three ranges coincide, which is exactly what
    // `test/infcover.c` L305-L310 does.
    Some(unsafe { core::slice::from_raw_parts(buffer.cast::<Cell<u8>>(), len) })
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
/// # ★ Only `done` is written now
///
/// C's L1229 assigns `head->done = 0` and touches nothing else, so a caller that
/// pre-filled `text`, `time`, `xflags` or `os` keeps its own values if the stream
/// turns out not to be gzip. That is reproduced by *seeding* the sink from the
/// caller's current values here, so that a field the parse never assigns round-trips
/// unchanged when [`Session::publish_header`] writes it back. Nothing is invented and
/// nothing is cleared.
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
/// [`inflateInit2_`]. `head`, if non-null, must be a caller-allocated [`gz_header`]
/// that stays live as described above, whose `extra`, `name` and `comment` members
/// are each either null or valid for reads and writes of `extra_max`, `name_max` and
/// `comm_max` bytes respectively.
#[no_mangle]
pub unsafe extern "C" fn inflateGetHeader(strm: z_streamp, head: gz_headerp) -> c_int {
    guard_code(|| {
        // L1223: the state check.
        // SAFETY: unsafe-site categories 1, 3 and 4 -- see `entry`.
        let session = unsafe { entry(strm) };
        let Some(session) = session else {
            return fallback::STREAM_ERROR_CODE;
        };

        // See "Divergence" above. Checked after the state check so that an invalid
        // stream still reports `Z_STREAM_ERROR` for the same reason C reports it.
        if head.is_null() || !head.is_aligned() {
            session.finish();
            return fallback::STREAM_ERROR_CODE;
        }

        // SAFETY: unsafe-site category 6 -- reads six members of the caller's
        // `gz_header` plus the six scalars, through raw places so that no
        // `&gz_header` is formed and the caller's pointer keeps its provenance for
        // the writes `Session::publish_header` performs later. `head` is non-null and
        // aligned by the test above and live by this function's contract, so every
        // member is in bounds. All twelve are plain `Copy` data.
        let (extra, extra_max, name, name_max, comment, comm_max) = unsafe {
            (
                core::ptr::addr_of!((*head).extra).read(),
                core::ptr::addr_of!((*head).extra_max).read(),
                core::ptr::addr_of!((*head).name).read(),
                core::ptr::addr_of!((*head).name_max).read(),
                core::ptr::addr_of!((*head).comment).read(),
                core::ptr::addr_of!((*head).comm_max).read(),
            )
        };
        // SAFETY: unsafe-site category 6, as above -- the six scalars this seeds the
        // sink with, so that a field the parse never assigns is written back
        // unchanged.
        let (text, time, xflags, os, extra_len, hcrc) = unsafe {
            (
                core::ptr::addr_of!((*head).text).read(),
                core::ptr::addr_of!((*head).time).read(),
                core::ptr::addr_of!((*head).xflags).read(),
                core::ptr::addr_of!((*head).os).read(),
                core::ptr::addr_of!((*head).extra_len).read(),
                core::ptr::addr_of!((*head).hcrc).read(),
            )
        };

        // SAFETY: unsafe-site category 6 -- each buffer is null or valid for its
        // advertised capacity by this function's contract, which is exactly what
        // `header_field` requires. The three ranges are permitted to overlap, which
        // is why they become shared `Cell` slices.
        let (extra, name, comment) = unsafe {
            (
                header_field(extra, extra_max),
                header_field(name, name_max),
                header_field(comment, comm_max),
            )
        };

        let mut sink = GzHeaderSink::new(extra, name, comment);
        sink.text = text != 0;
        sink.time = narrow_check(time);
        sink.xflags = xflags;
        sink.os = os;
        sink.extra_len = extra_len;
        sink.hcrc = hcrc != 0;

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

/// The `done` value meaning "no gzip header has been read yet": zero.
///
/// `inflate.c` L1229. `zlib.h` L131-L132 documents the tri-state -- 0 not started,
/// -1 not a gzip stream, 1 complete -- and this is the value an install publishes.
const GZ_HEADER_PENDING: c_int = 0;

// ---------------------------------------------------------------------------
// ★ The internal symbol the test suite forces this library to export
// ---------------------------------------------------------------------------

/// `inflate_table` — builds a Huffman decode table from a set of code lengths.
///
/// Declared at `inftrees.h` L60-L62; ported from `inftrees.c` L46-L311, whose
/// algorithm is implemented by [`zlib_rs::inflate::inftrees::inflate_table`] and is
/// **not** reimplemented here. This function is purely the pointer-to-slice
/// adaptation.
///
/// # ★ Why an internal symbol is exported at all
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
/// | `codetype type` | a [`c_int`], validated by [`code_type_from_raw`] — never a Rust `enum`, because a C caller may pass any `int` |
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
pub unsafe extern "C" fn inflate_table(
    type_: c_int,
    lens: *mut c_ushort,
    codes: c_uint,
    table: *mut *mut code,
    bits: *mut c_uint,
    work: *mut c_ushort,
) -> c_int {
    guard(|| {
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

        // SAFETY: unsafe-site category 2 -- slice reconstruction over three of the
        // caller's arrays, each performed once. `lens` and `work` are non-null by the
        // test above and, by this function's contract, valid for `count` `u16`s;
        // `c_ushort` is `u16`, so the casts change no layout and the alignment
        // requirement is unchanged. The two do not overlap by contract, so the shared
        // and mutable borrows cannot alias. `bits` and `table` are non-null and
        // readable, and both hold plain `Copy` data that is not dereferenced here.
        let (lens_slice, work_slice, requested_bits, base) = unsafe {
            (
                core::slice::from_raw_parts(lens.cast::<u16>(), count),
                core::slice::from_raw_parts_mut(work.cast::<u16>(), count),
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
        let outcome = core_inflate_table(
            code_type,
            lens_slice,
            count,
            claimed,
            &mut cursor,
            &mut root,
            work_slice,
        );

        // ★ The core writes `cursor` and `root` only on a success path -- exactly
        // where `inftrees.c` L308-L309 writes `*table += used; *bits = root;` -- so on
        // an error return both still hold the values passed in and publishing them
        // unconditionally is identical to C's conditional write, with no branch to get
        // wrong. It also means `cursor` is zero on every error path, so no entry is
        // copied out for a table the caller must not use.
        let produced = arena.get(..cursor).unwrap_or(&[]);
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

        // SAFETY: unsafe-site category 2 -- the two in/out parameters. `bits` and
        // `table` are non-null by the test above and writable by this function's
        // contract. `cursor` is bounded by `capacity`, so advancing `base` by it stays
        // within the caller's array or one past its end -- which is what C's
        // `*table += used` produces for a table that exactly fills it.
        unsafe {
            bits.write(narrow_uInt(root));
            table.write(base.add(cursor));
        }

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
#[allow(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used
)]
mod tests {
    use super::{
        code, inflate, inflateCodesUsed, inflateCopy, inflateEnd, inflateGetDictionary,
        inflateGetHeader, inflateInit2_, inflateInit_, inflateMark, inflatePrime, inflateReset,
        inflateReset2, inflateResetKeep, inflateSetDictionary, inflateSync, inflateSyncPoint,
        inflateUndermine, inflateValidate, inflate_table, message_ptr, narrow_uLong, widen_uLong,
        z_stream, BAD_STATE_MARK, ENOUGH_DISTS, MESSAGES,
    };
    use core::ffi::{c_char, c_int, c_uint, c_ulong, c_ushort, CStr};
    use core::mem::{offset_of, size_of};

    use crate::types::{gz_header, uInt, StatePrefix, CODES, DISTS, ENOUGH, LENS};
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
        // SAFETY: `strm` is a live zeroed stream.
        for bits in [1, 7, -7, -16, 48, 100] {
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

    #[test]
    fn an_out_of_range_flush_and_a_null_output_are_both_stream_errors() {
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
                assert_eq!(
                    inflate(&mut strm, flush),
                    ReturnCode::STREAM_ERROR.as_i32(),
                    "flush = {flush}"
                );
            }
            // All seven documented values are accepted, `Z_TREES` included.
            for flush in 0..=6 {
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
        assert_eq!(size_of::<StatePrefix>(), 16);
        assert_eq!(offset_of!(StatePrefix, tag), 8);
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
        // SAFETY: as above.
        assert_eq!(
            unsafe { tag.read() },
            DICT_TAG,
            "the stream is waiting in DICT"
        );

        // L324-L325: the wrong dictionary is a data error, not a stream error.
        // SAFETY: `input` is live and readable for one byte.
        assert_eq!(
            unsafe { inflateSetDictionary(&mut strm, input.as_ptr(), 1) },
            ReturnCode::DATA_ERROR.as_i32()
        );
        // The right one -- empty -- is accepted.
        // SAFETY: `scratch` is live; a zero length reads nothing from it.
        assert_eq!(
            unsafe { inflateSetDictionary(&mut strm, scratch.as_ptr(), 0) },
            ReturnCode::OK.as_i32()
        );

        strm.next_in = core::ptr::null();
        strm.avail_in = 0;
        strm.next_out = scratch.as_mut_ptr();
        strm.avail_out = 8;
        // SAFETY: the stream is initialised and the output region is live.
        assert_eq!(
            unsafe { inflate(&mut strm, 0) },
            ReturnCode::BUF_ERROR.as_i32()
        );

        // ★ L330-L333: force the stream back into `DICT` from *outside* the library
        // and require the same two answers again. This is the assertion the whole
        // mode-tag synchronisation exists for.
        // SAFETY: the prefix layout is as documented above.
        unsafe {
            tag.write(DICT_TAG);
        }
        // SAFETY: `scratch` is live; a zero length reads nothing from it.
        assert_eq!(
            unsafe { inflateSetDictionary(&mut strm, scratch.as_ptr(), 0) },
            ReturnCode::OK.as_i32(),
            "an externally written mode = DICT must be honoured"
        );
        // SAFETY: the stream is initialised and the output region is live.
        assert_eq!(
            unsafe { inflate(&mut strm, 0) },
            ReturnCode::BUF_ERROR.as_i32()
        );

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

        let mut history = vec![0_u8; dictionary.len()];
        let mut capacity = uInt::try_from(history.len()).unwrap();
        // SAFETY: `history` is live and writable for `capacity` bytes.
        unsafe {
            assert_eq!(
                inflateGetDictionary(&mut raw, history.as_mut_ptr(), &mut capacity),
                ReturnCode::OK.as_i32()
            );
        }
        assert_eq!(history, dictionary);
        assert_eq!(usize::try_from(capacity).unwrap(), dictionary.len());

        // A short buffer receives a prefix and still learns the true length.
        let mut clipped = [0_u8; 4];
        let mut clipped_len: uInt = 4;
        // SAFETY: as above, with a deliberately small capacity.
        unsafe {
            assert_eq!(
                inflateGetDictionary(&mut raw, clipped.as_mut_ptr(), &mut clipped_len),
                ReturnCode::OK.as_i32()
            );
        }
        assert_eq!(&clipped, &dictionary[..4]);
        assert_eq!(usize::try_from(clipped_len).unwrap(), dictionary.len());

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
        let mut head = gz_header {
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
        };

        let mut strm = blank_stream();
        init(&mut strm, 15);
        // A zlib-only stream cannot accept a header: `(wrap & 2) == 0`.
        // SAFETY: the stream is initialised and `head` is live with live buffers.
        unsafe {
            assert_eq!(
                inflateGetHeader(&mut strm, &mut head),
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
            assert_eq!(
                inflateGetHeader(&mut strm, &mut head),
                ReturnCode::OK.as_i32()
            );
        }
        // ★ Only `done` is written on install; the caller's other scalars survive.
        assert_eq!(head.done, 0);
        assert_eq!(head.text, 7);
        assert_eq!(head.time, 0xdead);
        assert_eq!(head.extra_len, 99);

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
        assert_eq!(head.done, 1, "the header parsed to completion");
        assert_eq!(head.time, 0x4433_2211);
        assert_eq!(head.xflags, 0x02);
        assert_eq!(head.os, 0x03);
        assert_eq!(head.extra_len, 2);
        assert_eq!(&extra[..2], &[0xab, 0xcd]);
        assert_eq!(&name[..5], b"name\0");
        assert_eq!(&comment[..8], b"comment\0");
        assert_eq!(head.text, 0);
        assert_eq!(head.hcrc, 0);
        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
    }

    #[test]
    fn a_reset_detaches_the_installed_header() {
        // `inflateResetKeep` assigns `state->head = Z_NULL` (`inflate.c` L110), so a
        // reset must leave the caller's structure alone from then on.
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
            done: 5,
        };
        let mut strm = blank_stream();
        init(&mut strm, 47);
        // SAFETY: the stream is initialised and `head` is live.
        unsafe {
            assert_eq!(
                inflateGetHeader(&mut strm, &mut head),
                ReturnCode::OK.as_i32()
            );
            assert_eq!(head.done, 0);
            assert_eq!(inflateReset(&mut strm), ReturnCode::OK.as_i32());
        }
        head.done = 5;
        let mut out = [0_u8; 8];
        let input = [0x1f_u8, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0, 0];
        strm.next_in = input.as_ptr();
        strm.avail_in = 10;
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = 8;
        // SAFETY: as above.
        unsafe {
            let _ = inflate(&mut strm, 0);
        }
        assert_eq!(
            head.done, 5,
            "a detached header must not be written after a reset"
        );
        // SAFETY: the stream is the initialised one.
        assert_eq!(unsafe { inflateEnd(&mut strm) }, ReturnCode::OK.as_i32());
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
                inflate_table(
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
            inflate_table(
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
            inflate_table(
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
            inflate_table(
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
                    inflate_table(
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
                inflate_table(
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
                inflate_table(
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
                inflate_table(
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
                inflate_table(
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
                inflate_table(
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
}
