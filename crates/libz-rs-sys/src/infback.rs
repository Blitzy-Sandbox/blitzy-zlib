//! The three exported `inflateBack*` entry points: raw-DEFLATE decompression
//! driven by two caller-supplied C function pointers.
//!
//! This is the callback half of the C ABI facade. Like [`crate::inflate`] it
//! contributes no decoding logic — every state transition, table lookup and bound
//! check lives in [`zlib_rs::infback`], which is compiled under
//! `#![forbid(unsafe_code)]`. What this module contributes is the boundary:
//! validating a caller's [`z_streamp`], rebuilding its staged input *and its
//! window* as slices exactly once, adapting two C function pointers to the core's
//! two callback traits, and writing back the two `z_stream` members that
//! `infback.c`'s epilogue writes.
//!
//! It is the hardest FFI shape in the crate after `gzprintf`, for three reasons
//! that have nothing to do with the decoder: the two callbacks have **opposite**
//! failure conventions, the window is **the caller's** rather than the library's,
//! and the unmodified test suite **writes the decoder's mode from inside one of
//! the callbacks**. Each has its own section below.
//!
//! # The exported surface
//!
//! Three functions, matching the reference library's dynamic symbol table
//! exactly. All three sit in `zlib.map`'s `ZLIB_1.2.0` `global:` block (its
//! L1-L8), so the shared library decorates them `@@ZLIB_1.2.0`; that decoration
//! comes from the `--version-script` link argument this crate's `build.rs` emits,
//! never from anything here.
//!
//! | Symbol | `zlib.h` | Returns | Ported from |
//! |---|---|---|---|
//! | [`inflateBackInit_`] | L1913 | `int` | `infback.c` L25-L64 |
//! | [`inflateBack`] | L1138 | `int` | `infback.c` L191-L570 |
//! | [`inflateBackEnd`] | L1208 | `int` | `infback.c` L572-L579 |
//!
//! ★ **`inflateBackInit` is deliberately not exported.** `zlib.h` L1113 shows its
//! signature inside a `/* … */` prose block and L2016-L2018 defines it as a macro
//! over `inflateBackInit_`, supplying `ZLIB_VERSION` and `sizeof(z_stream)` from
//! the *caller's* header. It is absent from the reference library's dynamic table,
//! so exporting it would fail the 111-symbol parity diff — exactly as
//! `deflateInit`, `deflateInit2`, `inflateInit` and `inflateInit2` are absent for
//! the same reason.
//!
//! # ★ The two callbacks fail in opposite directions
//!
//! `infback.c` L182-L185 states the contract, and it is genuinely
//! counter-intuitive:
//!
//! > `in()` should return zero on failure. `out()` should return non-zero on
//! > failure. If either `in()` or `out()` fails, than `inflateBack()` returns a
//! > `Z_BUF_ERROR`.
//!
//! So:
//!
//! | C callback | Fails when it returns | Adapter here | Core trait result |
//! |---|---|---|---|
//! | `in_func` (`zlib.h` L1134-L1135) | **zero** | [`CallerInput`] | [`None`] from [`InflateBackInput::next_chunk`] |
//! | `out_func` (`zlib.h` L1136) | **non-zero** | [`CallerOutput`] | [`Err`]`(`[`OutputFailure`]`)` from [`InflateBackOutput::write_out`] |
//!
//! Either failure is [`ReturnCode::BUF_ERROR`] — never `Z_STREAM_ERROR` and never
//! `Z_DATA_ERROR` — and `zlib.h` L1199-L1202 makes `strm->next_in` the documented
//! discriminator: it "will be `Z_NULL` only if `in()` returned an error". The two
//! conversions are performed at their own two sites, in the two adapters, rather
//! than normalised to one convention and inverted once, so that a reviewer sees
//! each polarity next to the C prototype it implements.
//!
//! `in_desc` and `out_desc` are opaque cookies (`infback.c` L177-L180). They are
//! passed straight through and never examined — and a **null** descriptor is
//! legal and meaningful, because both of `test/infcover.c`'s callbacks branch on
//! it (its L453 and L467).
//!
//! # ★ The window belongs to the caller
//!
//! `inflate()` allocates and owns its window lazily. `inflateBack` never allocates
//! one: `inflateBackInit_` stores the caller's pointer (`infback.c` L59) and
//! `zlib.h` L1141-L1144 explains why — it "avoids copying between the output and
//! the sliding window by simply making the window itself the output buffer".
//!
//! Three consequences, all of them obligations on this module:
//!
//! 1. `windowBits` is restricted to `8..=15` and must be validated **before**
//!    `1 << windowBits` is evaluated, so that a caller's `0`, `64` or `-15`
//!    becomes `Z_STREAM_ERROR` rather than a shift overflow.
//!    `test/infcover.c` L479 passes `0`.
//! 2. The window is rebuilt as a borrowed `&mut [u8]` of exactly `1 << windowBits`
//!    bytes, once, on entry to [`inflateBackInit_`], and the core borrows it for
//!    the state's whole life. That is [`zlib_rs::infback::inflate_back_init`]'s
//!    signature, which takes `&'a mut [u8]` rather than an owned buffer.
//! 3. The caller must therefore keep that buffer alive, and unaliased, until
//!    [`inflateBackEnd`]. `test/infcover.c` L475 uses a 32768-byte stack array
//!    (`unsigned char win[32768];`), which satisfies the requirement for exactly
//!    as long as `cover_back` is on the stack. The obligation is stated in
//!    [`inflateBackInit_`]'s own `# Safety` section, because no Rust lifetime can
//!    express it across an `extern "C"` boundary.
//!
//! The window's *contents* are irrelevant: `test/infcover.c` L87 fills every
//! allocation with `0xa5` precisely to catch code that assumes zeros. What makes
//! reading it safe is `whave`, which starts at zero (`infback.c` L217) and only
//! reaches the full window size once `ROOM()` has flushed it (L156). That is the
//! subject of the baseline commit, `09a1572` "Fix `inflateBack()` bug that would
//! fail to detect a too far back", and nothing here widens any `whave`-derived
//! bound.
//!
//! # ★ Argument validation order is observable
//!
//! `test/infcover.c` L477-L482 pins it:
//!
//! ```c
//! ret = inflateBackInit_(Z_NULL, 0, win, 0, 0);  assert(ret == Z_VERSION_ERROR);
//! ret = inflateBackInit(Z_NULL, 0, win);         assert(ret == Z_STREAM_ERROR);
//! ret = inflateBack(Z_NULL, Z_NULL, Z_NULL, Z_NULL, Z_NULL);
//!                                               assert(ret == Z_STREAM_ERROR);
//! ret = inflateBackEnd(Z_NULL);                 assert(ret == Z_STREAM_ERROR);
//! ```
//!
//! The first two calls differ only in their last two arguments and must answer
//! *different* codes, so in [`inflateBackInit_`] the version and `stream_size`
//! gate has to fire **before** the null-stream test — `infback.c` L30-L32 ahead of
//! L33-L35. A null `version` counts as a version failure and must be rejected
//! without being dereferenced. That gate is [`crate::inflate::version_error`],
//! shared with the two `inflateInit` spellings so the comparison exists once.
//!
//! The third call also shows why the two callbacks are
//! `Option<unsafe extern "C" fn(…)>` rather than bare function pointers: all five
//! arguments are `Z_NULL`, and a non-nullable `extern "C" fn` parameter holding a
//! null would already be undefined behaviour before the body ran.
//!
//! # ★ The mode is written from inside a callback
//!
//! `test/infcover.c`'s `pull()` (its L447-L460) reaches through the opaque state
//! pointer it was handed as `in_desc`:
//!
//! ```c
//! state = (void *)((z_stream *)desc)->state;
//! if (state != Z_NULL)
//!     state->mode = SYNC;     /* force an otherwise impossible situation */
//! ```
//!
//! and `cover_back` then asserts that
//! `inflateBack(&strm, pull, &strm, push, Z_NULL)` answers **`Z_STREAM_ERROR`**
//! (its L497-L498). In C that works because `state->mode` is one memory location
//! and `for (;;) switch (state->mode)` re-reads it every turn, so the write is
//! seen by the `default` arm — "can't happen, but makes compilers happy",
//! `infback.c` L554-L557.
//!
//! Reproducing it here takes three separate things:
//!
//! 1. **The prefix.** The block installed at [`z_stream::state`](crate::types::z_stream::state) exposes a
//!    `#[repr(C)]` `{ z_streamp strm; int mode; }` head — [`crate::types::StatePrefix`]
//!    — so the caller's write lands at offset 8 with width 4, where `inflate.h`
//!    L82-L84 puts `mode`. This module must not wrap the state in anything that
//!    would shift it, and does not.
//! 2. **Re-reading, not caching.** The `inflate_mode` values are not zero-based:
//!    `HEAD` is 16180 and runs consecutively to `SYNC` at 16211, so the byte
//!    pattern written is **16211**. [`ModeWatch`] holds the address of that slot
//!    and re-reads it **after every callback return**. A cached copy would lose
//!    the write and the assertion would fail silently.
//! 3. **Answering it the way C does.** `inflateBack` drives only six of the
//!    thirty-two modes — `TYPE`, `STORED`, `TABLE`, `LEN`, `DONE` and `BAD` — and
//!    every other value, `SYNC` included, is C's `default` arm and therefore
//!    `Z_STREAM_ERROR`. [`drivable`] is that test, written over
//!    [`zlib_rs::inflate::Mode`] so it cannot drift from the core's own dispatch.
//!
//! ## The one residual difference, stated plainly
//!
//! [`zlib_rs::infback`] cannot see the write at all: safe Rust offers no way to
//! hold `&mut InflateState` while `inflate_back` already holds it, so the core
//! documents its own catch-all as unreachable and keeps it only for correctness.
//! Detection is therefore entirely this module's, and it happens at the earliest
//! moment C could act on the write — the instant the callback returns.
//!
//! C can act *later*. Its `switch` arms assign `state->mode` on nearly every path,
//! so a write made during a `PULL()` inside `case TYPE` is overwritten by the
//! `state->mode = LEN` two statements later and never seen; only `case LEN`'s
//! length-and-distance path (L476-L543) ends without assigning, which is the path
//! `cover_back` actually trips. So: for a stream whose callbacks leave the slot
//! alone — every ordinary caller, and `test/infcover.c`'s own `try()` at its L563
//! — the two are identical. For a caller that deliberately corrupts its own
//! decoder state, both answer `Z_STREAM_ERROR`; this one may stop earlier, having
//! decoded fewer bytes and made fewer `out()` calls. Refusing promptly is also the
//! safer of the two responses to state corruption, so the difference is not a
//! reluctant compromise.
//!
//! A callback that writes a mode `inflateBack` *can* drive is not emulated: C
//! would honour it as a state transition and this treats it as noise. No
//! documented interface exposes the mode, `test/infcover.c` writes only `SYNC`
//! here, and honouring it would mean handing the core a mode from outside
//! mid-call, which is the very thing the safe boundary exists to prevent.
//!
//! # What is written back into the caller's `z_stream`
//!
//! ★ Fewer members than one might expect, and the reference is precise about it.
//! `inflateBack`'s epilogue (`infback.c` L560-L569) assigns exactly two:
//!
//! ```c
//!   inf_leave:
//!     if (left < state->wsize) { … }
//!     strm->next_in = next;
//!     strm->avail_in = have;
//! ```
//!
//! `total_in`, `total_out`, `avail_out`, `adler` and `data_type` are **not**
//! touched — `inflateBack` keeps no running totals, because the callbacks are
//! where a caller accumulates them (`infback.c` L177-L180 says as much). `msg` is
//! cleared at L214 and re-published by whichever arm sets it, which is what makes
//! `test/infcover.c` L567's `assert(strcmp(id, strm.msg) == 0)` pass for all ten
//! of its `Z_DATA_ERROR` cases; the texts come from
//! [`crate::inflate::message_ptr`], the crate's single message table.
//!
//! # Unsafe containment
//!
//! Every `unsafe` block below falls into one of the categories the crate
//! documentation enumerates, and names it:
//!
//! | Category | Where it appears here |
//! |---|---|
//! | 1 — stream-pointer validation | [`inflateBackInit_`], [`inflateBack`], [`inflateBackEnd`], [`mode_slot`] |
//! | 2 — slice reconstruction | [`inflateBackInit_`] (the caller's window), [`inflateBack`] (the staged input), [`CallerInput::next_chunk`] (an `in()` chunk) |
//! | 3 — the opaque `state` round-trip | [`mode_slot`], [`ModeWatch::observe`], [`inflateBack`], [`inflateBackEnd`] |
//! | 4 — invoking `zalloc`/`zfree` | [`inflateBackInit_`], [`inflateBackEnd`] |
//! | 5 — C strings | the `version` argument of [`inflateBackInit_`], read by [`crate::inflate::version_error`] |
//!
//! Calling the caller's `in()` and `out()` is category 4's reasoning applied to a
//! different pair of hooks: a caller-supplied C function pointer, non-null by
//! construction because `None` was rejected before the core was entered, and
//! matching the `zlib.h` L1134-L1136 prototype it is typed with. Category 6 does
//! not arise — nothing here touches a `gz_header`.
//!
//! # Panic discipline
//!
//! Nothing here may unwind, and this module is where that matters most in the
//! crate: Rust calls *into* C callbacks which are free to call back *into* Rust.
//! All three exports are `extern "C"` and never `extern "C-unwind"`, so a panic
//! aborts instead of crossing the boundary; each body is additionally wrapped by
//! [`crate::panic_guard::guard_code`] with `Z_STREAM_ERROR` as its documented
//! failure value; and the root release profile sets `panic = "abort"`. There is no
//! `unwrap`, `expect`, `panic!` or panicking index outside `#[cfg(test)]`, and
//! every fallible step answers a [`ReturnCode`], so an abort should be unreachable
//! rather than merely contained.

// `zlib.h` names these functions in camelCase and in lowerCamelCase parameters,
// and both are the ABI: cbindgen regenerates the header from these signatures and
// the result is diffed against `zlib.h` character for character. The crate root
// relaxes `non_camel_case_types` for the type half of the same rule.
#![allow(non_snake_case)]

use core::cell::Cell;
use core::ffi::{c_char, c_int, c_uint, c_void};

use zlib_rs::config::validate_inflate_back_window_bits;
use zlib_rs::error::ReturnCode;
use zlib_rs::infback::{
    inflate_back as core_inflate_back, inflate_back_end as core_inflate_back_end,
    inflate_back_init as core_inflate_back_init, InflateBackInput, InflateBackOutput,
    OutputFailure,
};
use zlib_rs::inflate::{InflateState, Mode};

use crate::inflate::{message_ptr, version_error};
use crate::panic_guard::{fallback, guard_code};
use crate::types::{
    checked_state_mut, in_func, input_slice, install_state, out_func, output_slice_mut, take_state,
    uInt, widen, z_streamp, Bytef, StateKind, StatePrefix, StreamAllocator,
};

// ---------------------------------------------------------------------------
// The state this module installs behind `z_stream.state`
// ---------------------------------------------------------------------------

/// What the facade keeps for one `inflateBack` session.
///
/// Wrapped in a [`crate::types::StateBlock`] so that the C-visible
/// `{ strm, mode }` prefix sits at offset 0; this struct is the part that follows
/// it, where no C caller looks.
///
/// One field, and that is the whole point of the type. `infback.c` L51 performs a
/// single `ZALLOC` and allocates **no** window, so an `inflateBack` stream owns
/// exactly one block for its entire life and the LIFO discipline
/// `test/infcover.c`'s `mem_free` polices (its L136) is satisfied trivially. The
/// decoder state itself owns nothing further: `lens`, `work` and `codes` are
/// inline arrays in [`InflateState`], and the window is borrowed from the caller.
///
/// The lifetime is `'static` because the block outlives every call that can see
/// it — it is released only by [`inflateBackEnd`]. The window it *borrows* is the
/// caller's, and keeping that borrow valid is the caller's documented obligation;
/// see [`inflateBackInit_`]'s `# Safety` section.
struct BackSlot {
    /// The safe decoder state, holding every scalar and the borrowed window.
    state: InflateState<'static, StreamAllocator>,
}

// ---------------------------------------------------------------------------
// The C-visible mode slot -- unsafe-site category 3
// ---------------------------------------------------------------------------

/// Locates the C-visible mode tag inside the block [`z_stream::state`](crate::types::z_stream::state) addresses.
///
/// The Rust spelling of `test/infcover.c`'s own cast,
/// `((struct inflate_state *)strm.state)->mode`: [`StatePrefix`] is the first
/// member of the `#[repr(C)]` [`crate::types::StateBlock`], so it lies at offset 0
/// and its `tag` member lies at offset 8 with width 4 — exactly where `inflate.h`
/// L82-L84 puts `strm` and `mode`.
///
/// ★ No reference to the block is formed, by either this function or its result.
/// `addr_of_mut!` produces a raw place, and the pointer it derives covers only the
/// four tag bytes. That is what makes it safe to read and write the tag while
/// [`core_inflate_back`] holds a `&mut` over the decoder state, which lives past
/// the prefix at offset 16: the two byte ranges are disjoint, the pointer is
/// derived from the block address rather than from that borrow, and the borrow of
/// the enclosing [`crate::types::StateBlock`] is never used again once the tag
/// pointer is in play. [`crate::types::validated_state_ptr`] reads the same prefix
/// the same way and for the same reason.
///
/// Returns [`None`] for C's `strm->state == Z_NULL` (`infback.c` L209) and for a
/// misaligned state pointer, which is the case a caller's stray write would
/// produce and which no tag test could catch.
///
/// # Safety
///
/// `strm` must be non-null, aligned, and address a live [`z_stream`](crate::types::z_stream). Its `state`
/// member, if non-null, must address a block installed by [`inflateBackInit_`] and
/// therefore beginning with an initialised [`StatePrefix`].
unsafe fn mode_slot(strm: z_streamp) -> Option<*mut c_int> {
    // SAFETY: unsafe-site category 1 -- reads one plain-pointer member of the
    // caller's stream through a raw place. `strm` is non-null, aligned and live by
    // this function's contract. `addr_of!` plus `read` forms no reference, so the
    // caller's own pointer keeps its provenance for the state recovery that
    // follows.
    let block = unsafe { core::ptr::addr_of!((*strm).state).read() };
    let prefix = block.cast::<StatePrefix>();
    if prefix.is_null() || !prefix.is_aligned() {
        return None;
    }
    // SAFETY: unsafe-site category 3 -- the opaque `state` round-trip. `prefix` is
    // non-null and aligned for `StatePrefix`, and by this function's contract those
    // bytes hold an initialised one. `addr_of_mut!` forms a raw place for a single
    // plain-data member: nothing is dereferenced and no reference to the block is
    // created, so this cannot disturb any borrow the decoder state is under.
    Some(unsafe { core::ptr::addr_of_mut!((*prefix).tag) })
}

/// The six modes `inflateBack`'s `switch` implements, as a predicate on a raw tag.
///
/// `infback.c` L226-L558 has arms for `TYPE`, `STORED`, `TABLE`, `LEN`, `DONE` and
/// `BAD` and nothing else — it has no `LENLENS` or `CODELENS` arm, because unlike
/// `inflate.c` it reads the whole code-length alphabet inline and never has to
/// suspend mid-alphabet. Every other value falls to `default`, which is
/// `Z_STREAM_ERROR` (L554-L557).
///
/// Written over [`Mode`] rather than over integer literals so that the set cannot
/// drift from [`zlib_rs::infback`]'s own dispatch, and written out variant by
/// variant rather than as a range so that adding a state to [`Mode`] is a
/// compile-time decision here rather than a silent reclassification.
///
/// A tag that names no state at all — the case a caller's stray write most likely
/// produces — is not drivable either, and reaches C's `default` by the same route.
fn drivable(tag: c_int) -> bool {
    match Mode::from_raw(tag) {
        Some(Mode::Type | Mode::Stored | Mode::Table | Mode::Len | Mode::Done | Mode::Bad) => true,
        Some(_) | None => false,
    }
}

/// Watches the C-visible mode slot across a callback, and latches a hostile write.
///
/// One of these is shared by the two adapters for the duration of a single
/// [`inflateBack`] call, which is why the interior mutability is a [`Cell`]: both
/// adapters need `&self` access to the same latch while the core holds each of
/// them by value.
#[derive(Debug)]
struct ModeWatch {
    /// The four tag bytes at offset 8 of the state block, from [`mode_slot`].
    ///
    /// Cached exactly as `infback.c` L211 caches `state` in a local, so that a
    /// callback which overwrites `strm->state` cannot redirect subsequent reads —
    /// C's local is likewise immune. It is the tag *value* that is re-read, never
    /// the pointer.
    slot: *mut c_int,

    /// The first tag observed that `inflateBack` cannot drive, if any.
    ///
    /// The *first* is kept rather than the last, because C's dispatch stops at the
    /// first such value it sees and never looks again.
    latched: Cell<Option<c_int>>,
}

impl ModeWatch {
    /// Begins watching `slot`, with nothing latched.
    const fn new(slot: *mut c_int) -> Self {
        Self {
            slot,
            latched: Cell::new(None),
        }
    }

    /// Publishes `tag` into the C-visible slot.
    ///
    /// Used twice per call: once before the core runs, to mirror C's
    /// `state->mode = TYPE` at `infback.c` L215 — so a callback that *reads* the
    /// slot sees a live mode rather than the previous call's leftover — and once
    /// after, so the slot a caller may inspect between calls is never stale.
    fn publish(&self, tag: c_int) {
        // SAFETY: unsafe-site category 3 -- the opaque `state` round-trip. `slot`
        // came from `mode_slot`, so it addresses the four in-bounds, aligned,
        // initialised tag bytes of a block this library installed, and it is a raw
        // pointer into plain data rather than a reference. The decoder state is
        // borrowed elsewhere but begins past the prefix, so the two writes can
        // never overlap.
        unsafe { self.slot.write(tag) }
    }

    /// Re-reads the slot after a callback returned and reports whether decoding
    /// may continue.
    ///
    /// ★ This is the "re-read, do not cache" half of the mode-write contract in the
    /// module documentation. A drivable tag means the slot still names a state
    /// `inflateBack` implements, so nothing has been disturbed. Anything else is
    /// latched and `false` is returned, which makes the calling adapter decline;
    /// [`inflateBack`] then reports `Z_STREAM_ERROR`, which is what C's `default`
    /// arm reports for the same value.
    fn observe(&self) -> bool {
        // SAFETY: unsafe-site category 3 -- as `publish`. A read of four plain-data
        // bytes through a raw pointer into a block this library installed, disjoint
        // from the decoder state's own bytes. The value may have been changed by the
        // callback that just returned, which is precisely why it is read here
        // instead of remembered.
        let tag = unsafe { self.slot.read() };
        if drivable(tag) {
            return true;
        }
        if self.latched.get().is_none() {
            self.latched.set(Some(tag));
        }
        false
    }

    /// The latched tag, or [`None`] if every observation was drivable.
    fn latched(&self) -> Option<c_int> {
        self.latched.get()
    }
}

// ---------------------------------------------------------------------------
// The two callback adapters
// ---------------------------------------------------------------------------

/// Adapts C's `in_func` to [`InflateBackInput`].
///
/// `unsigned (*in_func)(void FAR *, z_const unsigned char FAR * FAR *)`
/// (`zlib.h` L1134-L1135): the callback writes a pointer to its bytes through the
/// second argument and returns how many there are.
#[derive(Debug)]
struct CallerInput<'w> {
    /// The caller's `in`, already established non-null by [`inflateBack`].
    call: unsafe extern "C" fn(*mut c_void, *mut *const u8) -> c_uint,
    /// The caller's `in_desc`, passed through unexamined. May be null.
    desc: *mut c_void,
    /// The shared mode watch, consulted the instant `call` returns.
    watch: &'w ModeWatch,
}

impl<'i> InflateBackInput<'i> for CallerInput<'_> {
    /// Calls `in()` once and returns whatever it made available.
    ///
    /// ★ **`in()` fails by returning zero** (`infback.c` L182), so a zero count
    /// becomes [`None`] — which the core turns into C's `next = Z_NULL; ret =
    /// Z_BUF_ERROR` at L104-L106. `test/infcover.c`'s `pull` does exactly that once
    /// its four-byte array is exhausted (its L460).
    ///
    /// The returned slice's lifetime is not tied to `&mut self`, and that is the
    /// core trait's design rather than an accident: `infback.c` L172-L174 requires
    /// the application not to change the bytes "until `in()` is called again or
    /// `inflateBack()` returns", which is exactly the promise a `&'i [u8]`
    /// independent of this borrow expresses.
    fn next_chunk(&mut self) -> Option<&'i [u8]> {
        // C's `in()` is documented to *write* through its second argument, so the
        // pointer starts null and is only trusted if a non-zero count comes back.
        // `test/infcover.c`'s `pull` leaves it untouched on failure, and `zlib.h`
        // L1170-L1172 confirms "`buf` is ignored in that case".
        let mut buf: *const Bytef = core::ptr::null();

        // SAFETY: category 4's reasoning applied to the `in()` hook. `call` is a
        // caller-supplied C function pointer matching the `zlib.h` L1134-L1135
        // prototype it is typed with, and it is non-null because `inflateBack`
        // refused a `None` before this adapter was built. `desc` is passed through
        // opaquely, exactly as `infback.c` L102 passes `in_desc`, and may be null.
        // `buf` is a live local for the duration of the call, so the callback's write
        // through it is in bounds and correctly typed.
        let count = unsafe { (self.call)(self.desc, core::ptr::addr_of_mut!(buf)) };

        // ★ Re-read the mode NOW, before the count is even looked at: the callback
        // has had its chance to write the slot, and this is the earliest point C's
        // dispatch could act on it. See the module documentation.
        if !self.watch.observe() {
            return None;
        }

        // L103: a zero count is failure, whatever `buf` holds.
        if count == 0 {
            return None;
        }
        // A non-zero count with a null pointer is a callback that broke its own
        // contract. Refusing is the only safe reading, and it maps to the same
        // `Z_BUF_ERROR` the callback would have got by returning zero honestly.
        if buf.is_null() {
            return None;
        }

        // SAFETY: unsafe-site category 2 -- slice reconstruction. `buf` is non-null
        // by the test above and trivially aligned for `u8`. `count` is the length the
        // callback itself published for the region it pointed at, and `infback.c`
        // L172-L174 obliges the application to leave those bytes unchanged until the
        // next call or until `inflateBack` returns, which is the stability this
        // borrow requires. The library only reads through it, so a shared slice is
        // the right shape.
        Some(unsafe { core::slice::from_raw_parts(buf, widen(count)) })
    }
}

/// Adapts C's `out_func` to [`InflateBackOutput`].
///
/// `int (*out_func)(void FAR *, unsigned char FAR *, unsigned)` (`zlib.h` L1136):
/// the callback is handed a prefix of the window and returns a status.
#[derive(Debug)]
struct CallerOutput<'w> {
    /// The caller's `out`, already established non-null by [`inflateBack`].
    call: unsafe extern "C" fn(*mut c_void, *mut Bytef, c_uint) -> c_int,
    /// The caller's `out_desc`, passed through unexamined. May be null.
    desc: *mut c_void,
    /// The shared mode watch, consulted the instant `call` returns.
    watch: &'w ModeWatch,
}

impl InflateBackOutput for CallerOutput<'_> {
    /// Hands `data` to `out()` once.
    ///
    /// ★ **`out()` fails by returning non-zero** (`infback.c` L183) — the opposite
    /// of the input side. `test/infcover.c`'s `push` is `return desc != Z_NULL;`
    /// (its L467), written deliberately so that passing a non-null `out_desc`
    /// forces a failure, and `cover_back` L492-L495 asserts the resulting
    /// `Z_BUF_ERROR`.
    ///
    /// # Errors
    ///
    /// [`OutputFailure`] when `out()` returned non-zero, when the mode slot was
    /// disturbed, or when `data` is longer than C's `unsigned` can express. The
    /// last cannot happen — the slice is never longer than the window, itself at
    /// most 32768 bytes (`zlib.h` L1177-L1178) — and is answered rather than
    /// asserted so the property stays structural.
    fn write_out(&mut self, data: &[u8]) -> Result<(), OutputFailure> {
        let Ok(len) = c_uint::try_from(data.len()) else {
            return Err(OutputFailure);
        };

        // C's prototype takes `unsigned char FAR *`, so a mutable pointer has to be
        // handed over even though the callback must not write through it: `zlib.h`
        // L1174-L1175 obliges the application not to change the window until
        // `inflateBack` returns, and the C implementation itself passes
        // `state->window`, an equally writable pointer. A callback that writes is
        // breaking that contract, which is recorded in `inflateBack`'s `# Safety`.
        let buf = data.as_ptr().cast_mut();

        // SAFETY: category 4's reasoning applied to the `out()` hook. `call` is a
        // caller-supplied C function pointer matching the `zlib.h` L1136 prototype
        // it is typed with, non-null because `inflateBack` refused a `None` before
        // this adapter was built. `desc` is opaque and may be null, exactly as
        // `infback.c` L157 passes `out_desc`. `buf` and `len` describe a live,
        // readable region owned by the decoder for the duration of the call --
        // `data` is a live borrow -- and an empty `data` yields `len == 0`, for which
        // the callback is required to read nothing.
        let status = unsafe { (self.call)(self.desc, buf, len) };

        // ★ Re-read the mode NOW, for the same reason as on the input side.
        if !self.watch.observe() {
            return Err(OutputFailure);
        }

        // L157: non-zero is failure.
        if status == 0 {
            Ok(())
        } else {
            Err(OutputFailure)
        }
    }
}

// ---------------------------------------------------------------------------
// Narrowing helpers
// ---------------------------------------------------------------------------

/// Narrows a byte count to `avail_in`'s `uInt`, saturating rather than wrapping.
///
/// Every count this is asked to narrow is already a length that came *from* a
/// `uInt` — the caller's own `avail_in`, or the `c_uint` an `in()` callback
/// returned — so the saturation is unreachable. It is written as a saturation
/// rather than a cast because a silent wrap would understate the unused input and
/// make a caller re-feed bytes the decoder had already consumed, and rather than
/// an `unwrap` because library paths in this crate do not panic.
fn narrow_avail(count: usize) -> uInt {
    uInt::try_from(count).unwrap_or(uInt::MAX)
}

// ---------------------------------------------------------------------------
// Initialisation -- `infback.c` L25-L64
// ---------------------------------------------------------------------------

/// `inflateBackInit_` — prepares a stream to be decoded by [`inflateBack`], over a
/// window the caller supplies.
///
/// Declared at `zlib.h` L1913; ported from `infback.c` L25-L64. The real function
/// behind the `inflateBackInit` macro (`zlib.h` L1113 and L2016-L2018), which
/// supplies the last two arguments from the *caller's* `zlib.h` so that a version
/// or layout mismatch is caught at run time rather than corrupting memory.
///
/// # Parameters
///
/// * `windowBits` — the base-two logarithm of the window size, `8..=15`
///   (`zlib.h` L1117-L1121). ★ Narrower than every other window argument in the
///   library: there is no `0` for "read it from the header", no negative form for
///   "raw", and no `+16`/`+32` gzip form, because `inflateBack` decodes raw DEFLATE
///   only and has no header to read a size from. `zlib.h` L1120-L1123 adds the
///   practical advice that "windowBits must be 15 and a 32K byte window must be
///   supplied to be able to decompress general deflate streams".
/// * `window` — a caller-supplied buffer of at least `1 << windowBits` bytes,
///   which serves as both the sliding window and the output buffer
///   (`zlib.h` L1141-L1144). Its contents are irrelevant; nothing is read from it
///   before something has been written there.
/// * `version`, `stream_size` — the macro-supplied identity pair.
///
/// # Returns
///
/// [`Z_OK`](ReturnCode::OK), or:
///
/// * [`Z_VERSION_ERROR`](ReturnCode::VERSION_ERROR) for a version or `stream_size`
///   mismatch, **including a null `version`**, and reported ahead of every other
///   failure — `test/infcover.c` L477 asserts exactly that with an otherwise
///   entirely invalid argument list;
/// * [`Z_STREAM_ERROR`](ReturnCode::STREAM_ERROR) for a null or misaligned `strm`,
///   a null `window`, a `windowBits` outside `8..=15`, or a half-supplied
///   `zalloc`/`zfree` pair;
/// * [`Z_MEM_ERROR`](ReturnCode::MEM_ERROR) if the state block cannot be
///   allocated.
///
/// # ★ `windowBits` is validated before it is shifted
///
/// `1 << windowBits` for a `windowBits` of `0`, `64` or `-15` is either wrong or
/// undefined, and `test/infcover.c` L479 passes `0`. The range test therefore runs
/// first, through [`validate_inflate_back_window_bits`], and the shift is
/// evaluated only on a value already known to lie in `8..=15`.
///
/// # ★ Exactly one allocation
///
/// C allocates the state and *nothing else* (`infback.c` L51-L53): the window is
/// the caller's. So an `inflateBack` stream holds one block from here until
/// [`inflateBackEnd`], and the "frees not LIFO" report `test/infcover.c`'s
/// `mem_done` can produce (its L223) is unreachable for this family by
/// construction — unlike `inflate()`, whose window is a second, later allocation
/// that has to be released first.
///
/// The block is *not* zeroed, matching C, which has no `zmemzero` here even though
/// `inflateInit2_` does. It does not need one: the value moved into it is fully
/// initialised, which is strictly stronger than C's "every field is assigned
/// before it is read".
///
/// # Safety
///
/// * `strm`, if non-null, must address a caller-allocated [`z_stream`](crate::types::z_stream) of at least
///   `size_of::<z_stream>()` bytes that no other thread is touching, and must not
///   already hold a state — pass it to [`inflateBackEnd`] first, because
///   overwriting a live [`z_stream::state`](crate::types::z_stream::state) leaks it and `test/infcover.c`'s
///   `mem_done` reports leaks.
/// * `version`, if non-null, must point at a readable NUL-terminated string; only
///   its first byte is read.
/// * ★ `window` must be non-null and writable for `1 << windowBits` bytes, and
///   **those bytes must stay valid, and unaliased by anything else, until
///   [`inflateBackEnd`] returns** — not merely until this call returns. The state
///   borrows them for its whole life, which is what `infback.c` L59 does by storing
///   the bare pointer. No Rust lifetime can carry that across an `extern "C"`
///   boundary, so it is an obligation on the caller;
///   `test/infcover.c` L475 discharges it with a 32768-byte array that outlives
///   every call in `cover_back`.
/// * `zalloc`/`zfree`, if supplied, must behave as `zlib.h` L85-L86 describes, and
///   must not be changed before [`inflateBackEnd`]: the block allocated here can
///   only be released through the matching `zfree`.
#[no_mangle]
pub unsafe extern "C" fn inflateBackInit_(
    strm: z_streamp,
    windowBits: c_int,
    window: *mut Bytef,
    version: *const c_char,
    stream_size: c_int,
) -> c_int {
    guard_code(|| {
        // L30-L32, and note that this precedes the null-stream test: the first two
        // assertions of `test/infcover.c`'s `cover_back` differ only in these two
        // arguments and must answer different codes.
        // SAFETY: unsafe-site category 5 -- `version` is a readable C string or null
        // by this function's contract, which is exactly what `version_error`
        // requires. A null is rejected inside, before any dereference.
        if let Some(error) = unsafe { version_error(version, stream_size) } {
            return error;
        }

        // L33-L35, whose four conditions all produce `Z_STREAM_ERROR`. The window
        // extent is decoded here as well, because it is needed to rebuild the
        // caller's buffer as a slice and because the shift must not be evaluated on
        // an unvalidated exponent.
        if strm.is_null() || !strm.is_aligned() || window.is_null() {
            return fallback::STREAM_ERROR_CODE;
        }
        let Ok(bits) = validate_inflate_back_window_bits(windowBits) else {
            return fallback::STREAM_ERROR_CODE;
        };
        // `bits` is now known to lie in `8..=15`, so both steps succeed; they are
        // written as fallible conversions rather than casts so that the bound is
        // enforced by the code and not only by the comment.
        let Some(size) = 1_usize.checked_shl(u32::from(bits)) else {
            return fallback::STREAM_ERROR_CODE;
        };
        let Ok(extent) = uInt::try_from(size) else {
            return fallback::STREAM_ERROR_CODE;
        };

        // L36: `strm->msg = Z_NULL;` -- "in case we return an error". ★ Placed after
        // the guards above, exactly as C places it, so a rejected `windowBits` leaves
        // a previous message standing rather than clearing it.
        // SAFETY: unsafe-site category 1 -- writing one member of the caller's
        // stream. Non-null and aligned by the guard above, live by contract, and
        // written through a raw place so the caller's pointer keeps its provenance
        // for `install_state` below.
        unsafe {
            core::ptr::addr_of_mut!((*strm).msg).write(core::ptr::null());
        }

        // L37-L50: the hook defaulting. `StreamAllocator` expresses "both null, use
        // the library's own" as a mode rather than by overwriting the caller's
        // fields, and refuses a half-supplied pair -- see its own documentation for
        // why reproducing C's independent defaulting would reproduce a
        // mismatched-allocator hazard. `test/infcover.c` exercises both ends: L485
        // supplies a tracking pair, and L502 supplies neither because `mem_done`
        // cleared all three members (its L231-L233).
        // SAFETY: unsafe-site category 4 -- reads the three hook members through raw
        // places, forming no reference, so `strm` stays usable below.
        let allocator = unsafe { StreamAllocator::from_stream_ptr(strm) };
        let Some(allocator) = allocator else {
            return fallback::STREAM_ERROR_CODE;
        };

        // L59: `state->window = window;` -- the caller's buffer, rebuilt as a slice
        // once and then borrowed by the state for its whole life.
        // SAFETY: unsafe-site category 2 -- slice reconstruction. `window` is
        // non-null by the guard above and trivially aligned for `u8`; `extent` is
        // `1 << windowBits` with `windowBits` already inside `8..=15`, so it is a
        // count this function's contract makes writable. The `'static` lifetime is
        // the caller's obligation from the `# Safety` section: the bytes must outlive
        // the state, which lives until `inflateBackEnd`. Called exactly once, so no
        // second mutable view of the window can exist.
        let borrowed: &'static mut [u8] = unsafe { output_slice_mut(window, extent) };

        // L56-L62: `dmax`, `wbits`, `wsize`, `window`, `wnext`, `whave` and `sane`,
        // all set by the core's constructor. It re-validates `windowBits` and the
        // window extent, which is why the two cannot disagree.
        let state = match core_inflate_back_init(windowBits, borrowed, allocator) {
            Ok(state) => state,
            Err(error) => return error,
        };

        // L51-L55: `ZALLOC` of the state, `Z_MEM_ERROR` on failure, then
        // `strm->state = state`. The prefix is seeded with the mode the freshly
        // built state actually carries, so the block passes its own tag check from
        // the moment it exists -- C gets away with leaving `mode` at whatever the
        // uninitialised allocation held only because its own checks never look at an
        // `inflateBack` state's mode.
        // SAFETY: unsafe-site categories 3 and 4 -- installs the state block. `strm`
        // is non-null, aligned and live; `allocator` is the caller's own triple, so
        // the block can only ever be released through the matching `zfree`. Any
        // previously installed state was removed by the caller, which is this
        // function's documented obligation on it.
        let tag = state.mode_tag();
        match unsafe { install_state(strm, &allocator, tag, BackSlot { state }) } {
            Ok(_) => ReturnCode::OK,
            // L53: `if (state == Z_NULL) return Z_MEM_ERROR;`.
            Err(error) => error,
        }
    })
}

// ---------------------------------------------------------------------------
// The driver -- `infback.c` L191-L570
// ---------------------------------------------------------------------------

/// `inflateBack` — decompresses one complete raw DEFLATE stream through two
/// callbacks.
///
/// Declared at `zlib.h` L1138-L1140; ported from `infback.c` L191-L570. A single
/// call decodes a whole raw stream: `inflateBack` does not suspend and resume the
/// way [`crate::inflate::inflate`] does, so it resets `mode`, `last`, `whave` and
/// the bit accumulator at entry (`infback.c` L214-L223) and a stream that already
/// decoded one is immediately reusable (`zlib.h` L1150-L1152).
///
/// # Where the input comes from
///
/// Two sources, in this order:
///
/// 1. whatever the caller staged in `next_in`/`avail_in` before the call. C's
///    `have = next != Z_NULL ? strm->avail_in : 0` (`infback.c` L219) means a null
///    `next_in` contributes nothing *regardless* of `avail_in`, and that exact
///    shape is reproduced here. `test/infcover.c` L487-L488 relies on the staged
///    path, setting `avail_in = 2` and `next_in = "\x03"` before the call;
/// 2. `in()`, called only once the staged bytes are exhausted — C's
///    `if (have == 0)` guard inside `PULL()` (L101).
///
/// # Returns
///
/// * [`Z_STREAM_END`](ReturnCode::STREAM_END) — a complete stream was decoded;
/// * [`Z_DATA_ERROR`](ReturnCode::DATA_ERROR) — the stream is malformed, and
///   `strm->msg` says how;
/// * [`Z_BUF_ERROR`](ReturnCode::BUF_ERROR) — `in()` returned zero or `out()`
///   returned non-zero. `strm->next_in` discriminates: `zlib.h` L1199-L1202 makes
///   it `Z_NULL` "only if `in()` returned an error";
/// * [`Z_STREAM_ERROR`](ReturnCode::STREAM_ERROR) — the stream or the state is not
///   usable, either callback is null, or the decoder was left in a mode
///   `inflateBack` does not implement.
///
/// ★ Never [`Z_OK`](ReturnCode::OK): `zlib.h` L1204 states outright that
/// "`inflateBack()` cannot return `Z_OK`".
///
/// # ★ A null `in` or `out` is `Z_STREAM_ERROR`, not a crash
///
/// C would call through the null pointer the first time it needed the callback.
/// `infback.c` L188-L189 already reserves `Z_STREAM_ERROR` for the case where "the
/// input parameters are not correct", so that is what a `None` answers here, and
/// the check sits *after* the stream check so that
/// `inflateBack(Z_NULL, Z_NULL, Z_NULL, Z_NULL, Z_NULL)` still reports the code
/// `test/infcover.c` L480-L481 asserts.
///
/// # What is written back
///
/// `next_in` and `avail_in`, from `infback.c` L567-L568, plus `msg`, cleared at
/// L214 and re-published by whichever arm set it. Nothing else: `total_in`,
/// `total_out`, `avail_out`, `adler` and `data_type` are untouched, because
/// `inflateBack` keeps no totals — accumulating them is what `in_desc` and
/// `out_desc` are for (`infback.c` L177-L180).
///
/// # Safety
///
/// * `strm`, if non-null, must address a live [`z_stream`](crate::types::z_stream) whose state, if any, was
///   installed by [`inflateBackInit_`] and not yet ended, and whose `window` buffer
///   is still valid and unaliased.
/// * If `avail_in` is non-zero and `next_in` is non-null, `next_in` must be
///   readable for `avail_in` bytes.
/// * `in`, if non-null, must obey `zlib.h` L1134-L1135: on success it sets its
///   second argument to a readable region of the length it returns, and leaves
///   those bytes unchanged until it is called again or this function returns.
/// * `out`, if non-null, must obey `zlib.h` L1136 and must **not write** through
///   the pointer it is handed, which addresses the caller's own window; `zlib.h`
///   L1174-L1175 forbids changing that buffer until this function returns.
/// * Neither callback may unwind into this library, and neither may free or
///   reallocate the stream, its state or its window.
#[no_mangle]
pub unsafe extern "C" fn inflateBack(
    strm: z_streamp,
    r#in: in_func,
    in_desc: *mut c_void,
    out: out_func,
    out_desc: *mut c_void,
) -> c_int {
    guard_code(|| {
        // L209: `strm == Z_NULL`. Precedes every dereference, because a null stream
        // is a documented input rather than an error condition.
        if strm.is_null() || !strm.is_aligned() {
            return fallback::STREAM_ERROR_CODE;
        }

        // L209: `strm->state == Z_NULL`, and at the same time the address of the
        // C-visible mode slot -- taken before any borrow of the block exists, which
        // is what keeps the two accesses independent for the rest of the call.
        // SAFETY: unsafe-site categories 1 and 3 -- `strm` is non-null, aligned and
        // live by the guard above and by this function's contract, and its state, if
        // any, was installed by `inflateBackInit_`. No reference is formed.
        let slot = unsafe { mode_slot(strm) };
        let Some(slot) = slot else {
            return fallback::STREAM_ERROR_CODE;
        };

        // L188-L189: "the input parameters are not correct". C has no such test and
        // would call through the null; see the note above on why refusing is both
        // safe and within the documented contract.
        let (Some(call_in), Some(call_out)) = (r#in, out) else {
            return fallback::STREAM_ERROR_CODE;
        };

        // L214: `strm->msg = Z_NULL;`. Done before the loop, as C does, so a callback
        // that inspects the stream sees a cleared message rather than the previous
        // call's.
        // SAFETY: unsafe-site category 1 -- writing one member of the caller's
        // stream, non-null, aligned and live as established above, through a raw
        // place.
        unsafe {
            core::ptr::addr_of_mut!((*strm).msg).write(core::ptr::null());
        }

        // L218-L219: the staged input. `None` is C's `next == Z_NULL`, which
        // contributes nothing whatever `avail_in` says.
        // SAFETY: unsafe-site categories 1 and 2 -- reads the two input members
        // through raw places, then rebuilds them as a slice exactly once. `next_in`
        // is readable for `avail_in` bytes by this function's contract, and the
        // library only reads through the result.
        let staged = unsafe {
            let next_in = core::ptr::addr_of!((*strm).next_in).read();
            if next_in.is_null() {
                None
            } else {
                let avail_in = core::ptr::addr_of!((*strm).avail_in).read();
                Some(input_slice(next_in, avail_in))
            }
        };

        // L211: `state = (struct inflate_state FAR *)strm->state;`.
        // SAFETY: unsafe-site category 3 -- the opaque `state` round-trip. The owner
        // and tag checks run inside, and this function's contract supplies the
        // liveness and provenance requirements they cannot test. The borrow is taken
        // once, and the enclosing block is deliberately never touched again once
        // `state` has been reborrowed out of it, so that it cannot conflict with the
        // raw accesses `slot` performs on the prefix.
        let block = unsafe { checked_state_mut::<BackSlot>(strm, StateKind::Inflate) };
        let Some(block) = block else {
            return fallback::STREAM_ERROR_CODE;
        };
        let state = &mut block.state_mut().state;

        // L215: `state->mode = TYPE;`, published into the C-visible slot too, so a
        // callback that reads the mode sees a live value. The core performs the same
        // assignment on its own copy at the top of `inflate_back`.
        let watch = ModeWatch::new(slot);
        watch.publish(Mode::Type.as_raw());

        // L226-L568: the state machine and its epilogue, both inside the core.
        let result = core_inflate_back(
            state,
            staged,
            CallerInput {
                call: call_in,
                desc: in_desc,
                watch: &watch,
            },
            CallerOutput {
                call: call_out,
                desc: out_desc,
                watch: &watch,
            },
        );

        // L554-L557: a mode `inflateBack` does not implement is `Z_STREAM_ERROR`,
        // and it carries no message -- C's `default` arm sets none, and L214 already
        // cleared `strm->msg`. Reached only when a callback wrote the slot; see the
        // module documentation for how far this can and cannot follow C.
        let (code, msg) = match watch.latched() {
            Some(_) => (ReturnCode::STREAM_ERROR, None),
            None => (result.code, result.msg),
        };

        // The C-visible slot is left holding the mode a caller would find in C: the
        // value it wrote itself, when that names a state, and otherwise the mode the
        // decoder actually stopped in. Keeping it inside the accepted range is what
        // lets `inflateBackEnd` still recognise the block, which C -- whose
        // `inflateBackEnd` inspects no mode at all -- gets for free.
        let final_tag = match watch.latched() {
            Some(tag) if Mode::from_raw(tag).is_some() => tag,
            Some(_) | None => state.mode_tag(),
        };
        watch.publish(final_tag);

        // L567-L568: `strm->next_in = next; strm->avail_in = have;`. `None` is C's
        // `next = Z_NULL` from a failed `in()` (L104), and C leaves `have` at zero on
        // that path because `PULL()` only runs when it is already zero.
        let (next_in, avail_in) = match result.next_in {
            Some(rest) => (rest.as_ptr(), narrow_avail(rest.len())),
            None => (core::ptr::null(), 0),
        };
        // SAFETY: unsafe-site category 1 -- writing three members of the caller's
        // stream, which is non-null, aligned and live as established above. Each
        // write goes through a raw place, so no `&mut z_stream` is materialised. The
        // published `next_in` is either null or a pointer into the region the caller
        // staged or a callback supplied, and `msg`, when non-null, addresses a
        // `'static` NUL-terminated literal the caller may hold indefinitely.
        unsafe {
            core::ptr::addr_of_mut!((*strm).next_in).write(next_in);
            core::ptr::addr_of_mut!((*strm).avail_in).write(avail_in);
            core::ptr::addr_of_mut!((*strm).msg).write(message_ptr(msg));
        }

        code
    })
}

// ---------------------------------------------------------------------------
// Teardown -- `infback.c` L572-L579
// ---------------------------------------------------------------------------

/// `inflateBackEnd` — releases everything [`inflateBackInit_`] allocated.
///
/// Declared at `zlib.h` L1208; ported from `infback.c` L572-L579, whose whole body
/// is a state check, a `ZFREE(strm, strm->state)` and `strm->state = Z_NULL`.
///
/// One block goes back, through the **caller's** `zfree`: the state. The window is
/// not released here and is not released by C either, because the caller owns it
/// (`zlib.h` L1210 — "All memory allocated by `inflateBackInit()` is freed", and
/// the window never was). Once this returns, the caller's window buffer is no
/// longer borrowed and may be freed or reused.
///
/// [`z_stream::state`](crate::types::z_stream::state) is cleared *before* the block is released, so no path can
/// leave the caller holding a dangling pointer — not even one on which the release
/// itself failed.
///
/// # Returns
///
/// [`Z_OK`](ReturnCode::OK), or [`Z_STREAM_ERROR`](ReturnCode::STREAM_ERROR) for a
/// stream that fails the state check. A null stream therefore answers
/// `Z_STREAM_ERROR`, which `test/infcover.c` L482 asserts, and a stream whose state
/// this library did not install is refused rather than freed.
///
/// C's third condition, `strm->zfree == (free_func)0`, cannot be reached through a
/// correctly used stream: `inflateBackInit_` installs the default hooks when the
/// caller supplied none (`infback.c` L45-L50), so `zfree` is never null afterwards.
/// The equivalent guard here is [`StreamAllocator::from_stream_ptr`] refusing a
/// half-supplied pair, which additionally rejects the mismatched case C would
/// happily act on.
///
/// # Safety
///
/// `strm`, if non-null, must address a live [`z_stream`](crate::types::z_stream) whose state, if any, was
/// installed by [`inflateBackInit_`] and has not already been ended, and whose
/// `zalloc`/`zfree`/`opaque` triple is the one that was in place at that time —
/// `zlib.h` L140-L142 forbids an application from changing it once initialised.
#[no_mangle]
pub unsafe extern "C" fn inflateBackEnd(strm: z_streamp) -> c_int {
    guard_code(|| {
        // L573: the state check, whose allocator half also yields the hooks the
        // release must use.
        // SAFETY: unsafe-site category 4 -- reads the three hook members through raw
        // places. `strm` is live or null by this function's contract.
        let allocator = unsafe { StreamAllocator::from_stream_ptr(strm) };
        let Some(allocator) = allocator else {
            return fallback::STREAM_ERROR_CODE;
        };

        // L575-L576: `ZFREE(strm, strm->state); strm->state = Z_NULL;`. `take_state`
        // runs the same owner and tag checks, clears the member first, and then
        // releases the block through the caller's `zfree`. No window release
        // precedes it, because there is no window to release -- see the note above,
        // and `inflateBackInit_`'s "exactly one allocation".
        // SAFETY: unsafe-site categories 3 and 4 -- the opaque state round-trip and
        // the release through the caller's `zfree`. The block was produced by
        // `install_state` with this `S` and this triple, by this function's
        // contract, and nothing borrows it.
        let block = unsafe { take_state::<BackSlot>(strm, &allocator, StateKind::Inflate) };
        let Some(block) = block else {
            return fallback::STREAM_ERROR_CODE;
        };

        // Always `Z_OK`. The decoder state owns no heap buffer of its own -- its
        // arrays are inline and its window is the caller's -- so ending it cannot
        // fail, which is exactly why the core's `inflate_back_end` returns a constant.
        core_inflate_back_end(block.into_state().state)
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[allow(clippy::undocumented_unsafe_blocks)]
mod tests {
    use super::{
        drivable, inflateBack, inflateBackEnd, inflateBackInit_, narrow_avail, BackSlot, ModeWatch,
    };
    use core::alloc::Layout;
    use core::ffi::{c_char, c_int, c_uint, c_void, CStr};
    use core::mem::size_of;

    use crate::types::{
        alloc_func, free_func, uInt, voidpf, z_stream, Bytef, StateBlock, StatePrefix,
    };
    use crate::util::ZLIB_VERSION;
    use zlib_rs::error::ReturnCode;
    use zlib_rs::inflate::Mode;

    /// The mode tag `test/infcover.c` L459 writes from inside `pull()`: `SYNC`.
    ///
    /// Spelled as the literal the C enum resolves to rather than as
    /// `Mode::Sync.as_raw()`, because the whole point is that *caller* code computes
    /// it from `inflate.h` and this library has to accept whatever that produces.
    const SYNC_TAG: c_int = 16211;

    /// `Z_DEFLATED`, `zlib.h` L196.
    const Z_DEFLATED: c_int = 8;
    /// `Z_FINISH`, `zlib.h` L177.
    const Z_FINISH: c_int = 4;
    /// `Z_DEFAULT_STRATEGY`, `zlib.h` L206.
    const Z_DEFAULT_STRATEGY: c_int = 0;
    /// Raw DEFLATE with the largest window: the only `windowBits` an
    /// `inflateBack` window of 32768 bytes can read in general.
    const RAW_15: c_int = -15;

    /// The four bytes `test/infcover.c` L450 hands out one at a time.
    const DAT: [u8; 4] = [0x63, 0, 2, 0];

    // -----------------------------------------------------------------------
    // Harness
    // -----------------------------------------------------------------------

    /// A zeroed `z_stream`, which is what a C caller declares before initialising.
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

    /// The `version` and `stream_size` pair the `inflateBackInit` macro supplies.
    fn version_args() -> (*const c_char, c_int) {
        (
            ZLIB_VERSION.as_ptr(),
            c_int::try_from(size_of::<z_stream>()).unwrap(),
        )
    }

    /// `inflateBackInit(strm, windowBits, window)` — the `zlib.h` L2016-L2018 macro.
    fn back_init(strm: *mut z_stream, window_bits: c_int, window: &mut [u8]) -> c_int {
        let (version, size) = version_args();
        unsafe { inflateBackInit_(strm, window_bits, window.as_mut_ptr(), version, size) }
    }

    /// Stages `input` in a stream's `next_in`/`avail_in`, as a C caller would.
    fn stage(strm: &mut z_stream, input: &[u8]) {
        strm.next_in = input.as_ptr();
        strm.avail_in = uInt::try_from(input.len()).unwrap();
    }

    /// Reads a stream's `msg` back as a Rust string, or [`None`] for `Z_NULL`.
    fn msg_of(strm: &z_stream) -> Option<&'static str> {
        if strm.msg.is_null() {
            return None;
        }
        unsafe { CStr::from_ptr(strm.msg) }.to_str().ok()
    }

    /// The C-visible mode tag behind `strm.state`, as `test/infcover.c` reaches it.
    fn mode_tag(strm: &z_stream) -> c_int {
        let prefix = strm.state.cast::<StatePrefix>();
        assert!(!prefix.is_null(), "the stream must hold a state");
        unsafe { core::ptr::addr_of!((*prefix).tag).read() }
    }

    // -- the tracking allocator, replicating `test/infcover.c` L63-L154 ---------

    /// The zone `test/infcover.c` keeps at `strm.opaque` (its L63-L68).
    #[derive(Debug, Default)]
    struct MemZone {
        /// Live blocks as `(pointer, length)`, most recent first — C's linked list.
        ///
        /// ★ The *pointer* is stored, not its address as an integer. Keeping the
        /// original provenance is what lets `mem_free` hand the very same pointer
        /// back to `dealloc`, which in turn is what makes this harness clean under
        /// `-Zmiri-strict-provenance`; an `as usize` round trip would be rejected
        /// there, and the whole reason for running Miri over this module is the
        /// mode-slot aliasing, which such a diagnostic would mask.
        live: Vec<(*mut u8, usize)>,
        /// Bytes currently outstanding.
        total: usize,
        /// The largest `total` ever reached: C's `mem_high`.
        highwater: usize,
        /// A cap on `total`, or zero for none: C's `mem_limit`.
        limit: usize,
        /// Frees that were not of the most recent block: C's `notlifo`.
        notlifo: usize,
        /// Frees of an address the zone never handed out: C's `rogue`.
        rogue: usize,
    }

    impl MemZone {
        /// C's `mem_done` (its L200-L234): everything back, in order, nothing stray.
        fn assert_clean(&self, what: &str) {
            assert!(
                self.live.is_empty(),
                "{what}: {} blocks leaked",
                self.live.len()
            );
            assert_eq!(self.total, 0, "{what}: bytes not freed");
            assert_eq!(self.notlifo, 0, "{what}: frees not LIFO");
            assert_eq!(self.rogue, 0, "{what}: frees not recognized");
        }
    }

    /// The layout a tracked block of `len` bytes is allocated with.
    ///
    /// 16-byte alignment because that is what C's `malloc` guarantees and what the
    /// state block needs; a zero-length request is rounded up to one byte because
    /// Rust's allocator forbids a zero-sized layout while C's `malloc(0)` does not.
    fn tracked_layout(len: usize) -> Option<Layout> {
        Layout::from_size_align(len.max(1), 16).ok()
    }

    /// C's `mem_alloc` (its L71-L109), including the `0xa5` fill at its L87.
    unsafe extern "C" fn mem_alloc(opaque: voidpf, count: uInt, size: uInt) -> voidpf {
        let zone = opaque.cast::<MemZone>();
        if zone.is_null() {
            return core::ptr::null_mut();
        }
        let zone = unsafe { &mut *zone };
        let len = (count as usize) * (size as usize);
        if zone.limit != 0 && zone.total + len > zone.limit {
            return core::ptr::null_mut();
        }
        let Some(layout) = tracked_layout(len) else {
            return core::ptr::null_mut();
        };
        let ptr = unsafe { std::alloc::alloc(layout) };
        if ptr.is_null() {
            return core::ptr::null_mut();
        }
        // ★ Never zeros. This is the byte that catches code assuming zeroed memory.
        unsafe { core::ptr::write_bytes(ptr, 0xa5, layout.size()) };
        zone.live.insert(0, (ptr, len));
        zone.total += len;
        zone.highwater = zone.highwater.max(zone.total);
        ptr.cast()
    }

    /// C's `mem_free` (its L112-L154), including the `notlifo` and `rogue` counts.
    unsafe extern "C" fn mem_free(opaque: voidpf, address: voidpf) {
        let zone = opaque.cast::<MemZone>();
        if zone.is_null() {
            return;
        }
        let zone = unsafe { &mut *zone };
        // Compared as pointers rather than as integers, so no provenance is lost.
        let found = zone
            .live
            .iter()
            .position(|&(at, _)| core::ptr::eq(at.cast::<c_void>(), address));
        let Some(index) = found else {
            zone.rogue += 1;
            return;
        };
        if index != 0 {
            zone.notlifo += 1;
        }
        let (at, len) = zone.live.remove(index);
        zone.total -= len;
        if let Some(layout) = tracked_layout(len) {
            // `at` is the pointer `mem_alloc` obtained from `std::alloc::alloc` with
            // this very layout, carried through unchanged.
            unsafe { std::alloc::dealloc(at, layout) };
        }
    }

    /// A [`MemZone`] that is only ever reached through one raw pointer.
    ///
    /// ★ The zone is installed at `strm.opaque`, and [`mem_alloc`] and [`mem_free`]
    /// write to it through that raw pointer. The test body must therefore never
    /// touch it through a *separate* place — a `Box` deref or a `&mut` to a local:
    /// under Tree Borrows such a direct read is a **foreign** read that freezes the
    /// installed pointer, and the next callback write through it is then undefined
    /// behaviour. Miri's Tree Borrows caught exactly that here, on a harness that
    /// read `zone.live.len()` between `inflateBackInit_` and `inflateBackEnd`;
    /// Stacked Borrows does not model the transition and reported nothing.
    ///
    /// Keeping a single raw provenance root and reading *through* it — the same
    /// discipline [`ModeWatch`] uses for the mode slot — removes the hazard by
    /// construction rather than by ordering the statements carefully, so a later
    /// edit cannot reintroduce it.
    #[derive(Debug)]
    struct ZoneHandle {
        /// The sole root: every access, from the callbacks and from the test body
        /// alike, is derived from this one pointer and so shares its tag.
        zone: *mut MemZone,
    }

    impl ZoneHandle {
        /// Allocates a zone and keeps only its pointer.
        fn new() -> Self {
            Self {
                zone: Box::into_raw(Box::new(MemZone::default())),
            }
        }

        /// C's `mem_setup` (its L158-L173): installs the tracking triple.
        ///
        /// The pointer is *copied*, which preserves its tag, so the callbacks and
        /// [`Self::with`] operate on the same provenance.
        fn install(&self, strm: &mut z_stream) {
            let alloc: alloc_func = Some(mem_alloc);
            let free: free_func = Some(mem_free);
            strm.opaque = self.zone.cast::<c_void>();
            strm.zalloc = alloc;
            strm.zfree = free;
        }

        /// Reads the zone through the installed pointer.
        fn with<R>(&self, read: impl FnOnce(&MemZone) -> R) -> R {
            // SAFETY: category 3 — the pointer came from `Box::into_raw` in `new`
            // and stays live until `Drop`, so it is non-null, aligned and valid for
            // reads. The shared reference is a *child* of the tag the callbacks
            // use, so taking it neither freezes nor invalidates that tag, and it is
            // dead before any further FFI call can write through the parent.
            read(unsafe { &*self.zone })
        }

        /// C's `mem_limit` (its L177-L181): caps the bytes the zone will hand out.
        fn set_limit(&self, limit: usize) {
            // SAFETY: category 3 — as `with`, but a write, and still through the
            // one root tag rather than through a second place.
            unsafe { (*self.zone).limit = limit };
        }

        /// Blocks currently outstanding.
        fn live(&self) -> usize {
            self.with(|zone| zone.live.len())
        }

        /// Bytes currently outstanding.
        fn total(&self) -> usize {
            self.with(|zone| zone.total)
        }

        /// The largest `total` ever reached: C's `mem_high`.
        fn highwater(&self) -> usize {
            self.with(|zone| zone.highwater)
        }

        /// C's `mem_done` (its L200-L234), through the one root tag.
        fn assert_clean(&self, what: &str) {
            self.with(|zone| zone.assert_clean(what));
        }
    }

    impl Drop for ZoneHandle {
        fn drop(&mut self) {
            // SAFETY: category 3 — reclaims the single `Box` `new` leaked, exactly
            // once, after every derived pointer is out of use.
            drop(unsafe { Box::from_raw(self.zone) });
        }
    }

    /// C's `mem_done` tail (its L231-L233): detaches the zone from the stream.
    fn mem_detach(strm: &mut z_stream) {
        strm.opaque = core::ptr::null_mut();
        strm.zalloc = None;
        strm.zfree = None;
    }

    // -- the callbacks ---------------------------------------------------------

    /// The cookie the replica of `test/infcover.c`'s `pull` receives as `in_desc`.
    ///
    /// C keeps its cursor in a function-`static` and uses `desc` only to reach the
    /// stream; a `static` cannot be shared safely between parallel Rust tests, so
    /// both live here. `strm` non-null reproduces C's `desc != Z_NULL` branch, which
    /// is the branch that writes the mode.
    #[derive(Debug)]
    struct PullDesc {
        /// The stream whose opaque state is written through, or null.
        strm: *mut z_stream,
        /// How far into [`DAT`] the callback has got.
        next: usize,
    }

    /// A replica of `test/infcover.c`'s `pull` (its L447-L460).
    ///
    /// Hands out [`DAT`] one byte at a time, returns zero once exhausted, and —
    /// when it was given a stream — forces `mode = SYNC` first.
    unsafe extern "C" fn pull(desc: *mut c_void, buf: *mut *const Bytef) -> c_uint {
        let desc = desc.cast::<PullDesc>();
        if desc.is_null() {
            // C's L453-L456: no input, because it was already staged at `next_in`.
            return 0;
        }
        let desc = unsafe { &mut *desc };
        if !desc.strm.is_null() {
            // C's L457-L459, verbatim in effect: reach through the opaque pointer
            // and force an otherwise impossible situation.
            let prefix = unsafe { core::ptr::addr_of!((*desc.strm).state).read() };
            let prefix = prefix.cast::<StatePrefix>();
            if !prefix.is_null() {
                unsafe { core::ptr::addr_of_mut!((*prefix).tag).write(SYNC_TAG) };
            }
        }
        match DAT.get(desc.next) {
            Some(byte) => {
                unsafe { buf.write(byte) };
                desc.next += 1;
                1
            }
            None => 0,
        }
    }

    /// A replica of `test/infcover.c`'s `push` (its L463-L468).
    ///
    /// ★ Returns **non-zero** — failure — exactly when `desc` is non-null, which is
    /// how `cover_back` forces an output error.
    unsafe extern "C" fn push(desc: *mut c_void, buf: *mut Bytef, len: c_uint) -> c_int {
        let _ = (buf, len);
        c_int::from(!desc.is_null())
    }

    /// An `in()` that always declines, i.e. always returns zero.
    unsafe extern "C" fn refuse_in(desc: *mut c_void, buf: *mut *const Bytef) -> c_uint {
        let _ = (desc, buf);
        0
    }

    /// An `out()` that always succeeds, i.e. always returns zero.
    unsafe extern "C" fn accept_out(desc: *mut c_void, buf: *mut Bytef, len: c_uint) -> c_int {
        let _ = (desc, buf, len);
        0
    }

    /// An `out()` that appends to the `Vec<u8>` behind `out_desc` and succeeds.
    unsafe extern "C" fn collect_out(desc: *mut c_void, buf: *mut Bytef, len: c_uint) -> c_int {
        let sink = desc.cast::<Vec<u8>>();
        if sink.is_null() || (buf.is_null() && len != 0) {
            return 1;
        }
        let bytes = unsafe { core::slice::from_raw_parts(buf, len as usize) };
        unsafe { &mut *sink }.extend_from_slice(bytes);
        0
    }

    /// The cookie [`feed_in`] walks: a buffer plus a chunk size.
    ///
    /// ★ Holds the buffer as a raw pointer and a length rather than as a slice
    /// reference, and that is not laziness. The cookie reaches the callback as a
    /// bare `void *`, so a lifetime on it would have to be laundered somewhere; a
    /// pointer/length pair is what a real C callback carries anyway, and it lets the
    /// buffer stay an ordinary local in the test instead of being leaked to obtain
    /// `'static`. Miri's leak check notices the difference.
    #[derive(Debug)]
    struct FeedDesc {
        /// The first byte of the buffer being handed out.
        data: *const Bytef,
        /// How many bytes it holds.
        len: usize,
        /// How many have been handed out so far.
        at: usize,
        /// How many to hand out per call. Never zero.
        chunk: usize,
    }

    impl FeedDesc {
        /// Prepares a cookie handing `chunk` bytes of `data` out per call.
        fn new(data: &[u8], chunk: usize) -> Self {
            Self {
                data: data.as_ptr(),
                len: data.len(),
                at: 0,
                chunk,
            }
        }
    }

    /// An `in()` that hands out `chunk` bytes per call until the buffer is empty.
    unsafe extern "C" fn feed_in(desc: *mut c_void, buf: *mut *const Bytef) -> c_uint {
        let desc = desc.cast::<FeedDesc>();
        if desc.is_null() {
            return 0;
        }
        let desc = unsafe { &mut *desc };
        let end = (desc.at + desc.chunk).min(desc.len);
        let run = end.saturating_sub(desc.at);
        if run == 0 {
            return 0;
        }
        // The run lies inside the buffer the cookie was built from, because `end` is
        // clamped to its length and `at` only ever advances to a previous `end`.
        unsafe { buf.write(desc.data.add(desc.at)) };
        desc.at = end;
        c_uint::try_from(run).unwrap_or(0)
    }

    // -- stream fixtures -------------------------------------------------------

    /// Compresses `data` as raw DEFLATE through this library's own exported encoder.
    ///
    /// ★ Going through `crate::deflate`'s exported entry points rather than
    /// hand-writing byte fixtures is deliberate twice over. It is the only way to
    /// obtain streams large enough to make the window wrap, which is what exercises
    /// `ROOM()` (`infback.c` L151-L162) and therefore both the mid-stream `out()`
    /// call and the `whave` assignment that guards reads of the caller's window. And
    /// it makes every decode below a Rust-encoder-to-Rust-`inflateBack` interop
    /// assertion, which is one of the test classes the port owes. The hand-written
    /// fixtures — `test/infcover.c`'s own, which the C implementation is known to
    /// produce these exact answers for — cover the small and malformed cases
    /// separately.
    fn deflate_raw(data: &[u8], level: c_int) -> Vec<u8> {
        let (version, size) = version_args();
        let mut strm = blank_stream();
        let ret = unsafe {
            crate::deflate::deflateInit2_(
                &mut strm,
                level,
                Z_DEFLATED,
                RAW_15,
                8,
                Z_DEFAULT_STRATEGY,
                version,
                size,
            )
        };
        assert_eq!(ret, ReturnCode::OK.as_i32());
        let mut out = vec![0_u8; data.len() * 2 + 4096];
        strm.next_in = data.as_ptr();
        strm.avail_in = uInt::try_from(data.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = uInt::try_from(out.len()).unwrap();
        let ret = unsafe { crate::deflate::deflate(&mut strm, Z_FINISH) };
        assert_eq!(ret, ReturnCode::STREAM_END.as_i32());
        let produced = out.len() - usize::try_from(strm.avail_out).unwrap();
        assert_eq!(
            unsafe { crate::deflate::deflateEnd(&mut strm) },
            ReturnCode::OK.as_i32()
        );
        out.truncate(produced);
        out
    }

    /// Decodes `stream` with [`inflateBack`], returning `(code, output, msg)`.
    ///
    /// Input is staged at `next_in` and `in()` refuses, which is exactly the shape
    /// `test/infcover.c`'s `try()` uses at its L561-L563.
    fn decode_staged(stream: &[u8]) -> (c_int, Vec<u8>, Option<&'static str>) {
        let mut window = vec![0xa5_u8; 32768];
        let zone = ZoneHandle::new();
        let mut strm = blank_stream();
        zone.install(&mut strm);
        assert_eq!(
            back_init(&mut strm, 15, &mut window),
            ReturnCode::OK.as_i32()
        );

        let mut sink: Vec<u8> = Vec::new();
        stage(&mut strm, stream);
        let code = unsafe {
            inflateBack(
                &mut strm,
                Some(refuse_in),
                core::ptr::null_mut(),
                Some(collect_out),
                core::ptr::from_mut(&mut sink).cast::<c_void>(),
            )
        };
        let msg = msg_of(&strm);
        assert_eq!(
            unsafe { inflateBackEnd(&mut strm) },
            ReturnCode::OK.as_i32()
        );
        mem_detach(&mut strm);
        zone.assert_clean("decode_staged");
        (code, sink, msg)
    }

    // -----------------------------------------------------------------------
    // Argument validation, and its order
    // -----------------------------------------------------------------------

    /// `test/infcover.c` L477-L482, the four bad-parameter assertions verbatim.
    #[test]
    fn the_four_bad_parameter_cases_match_the_reference() {
        // `test/infcover.c` L475 uses a 32768-byte stack array; a heap buffer here
        // keeps clippy's stack-array bound happy without changing what is exercised,
        // since nothing in this test reads the window at all.
        let mut win = vec![0xa5_u8; 32768];

        // L477-L478: version = NULL and stream_size = 0 with a NULL stream, and the
        // answer is the VERSION error -- so the version gate fires first.
        let ret = unsafe {
            inflateBackInit_(
                core::ptr::null_mut(),
                0,
                win.as_mut_ptr(),
                core::ptr::null(),
                0,
            )
        };
        assert_eq!(ret, ReturnCode::VERSION_ERROR.as_i32());

        // L479: the macro supplies a correct version pair, so the same call now gets
        // past the gate and fails on the NULL stream instead.
        let ret = back_init(core::ptr::null_mut(), 0, &mut win);
        assert_eq!(ret, ReturnCode::STREAM_ERROR.as_i32());

        // L480-L481: all five arguments NULL.
        let ret = unsafe {
            inflateBack(
                core::ptr::null_mut(),
                None,
                core::ptr::null_mut(),
                None,
                core::ptr::null_mut(),
            )
        };
        assert_eq!(ret, ReturnCode::STREAM_ERROR.as_i32());

        // L482.
        let ret = unsafe { inflateBackEnd(core::ptr::null_mut()) };
        assert_eq!(ret, ReturnCode::STREAM_ERROR.as_i32());
    }

    /// The version gate rejects each of its three failure modes, and always ahead of
    /// the stream test.
    #[test]
    fn the_version_gate_precedes_every_other_check() {
        let mut win = [0_u8; 256];
        let (version, size) = version_args();
        let win_ptr = win.as_mut_ptr();

        // A null `version`, with everything else valid.
        let ret =
            unsafe { inflateBackInit_(core::ptr::null_mut(), 8, win_ptr, core::ptr::null(), size) };
        assert_eq!(ret, ReturnCode::VERSION_ERROR.as_i32());

        // A different major version.
        let ret =
            unsafe { inflateBackInit_(core::ptr::null_mut(), 8, win_ptr, c"2.0.0".as_ptr(), size) };
        assert_eq!(ret, ReturnCode::VERSION_ERROR.as_i32());

        // The same major version but a different `sizeof(z_stream)`.
        let ret = unsafe { inflateBackInit_(core::ptr::null_mut(), 8, win_ptr, version, size - 1) };
        assert_eq!(ret, ReturnCode::VERSION_ERROR.as_i32());

        // A matching major version is enough: only `version[0]` is compared.
        let mut strm = blank_stream();
        let ret = unsafe { inflateBackInit_(&mut strm, 8, win_ptr, c"1.0.0".as_ptr(), size) };
        assert_eq!(ret, ReturnCode::OK.as_i32());
        assert_eq!(
            unsafe { inflateBackEnd(&mut strm) },
            ReturnCode::OK.as_i32()
        );
    }

    /// ★ `windowBits` is `8..=15` and nothing else, and no value overflows the shift.
    #[test]
    fn the_window_bits_boundary_matrix_is_exactly_eight_through_fifteen() {
        let mut win = vec![0xa5_u8; 32768];
        for bits in [
            i32::MIN,
            -32,
            -15,
            -8,
            0,
            1,
            7,
            16,
            17,
            31,
            32,
            47,
            64,
            i32::MAX,
        ] {
            let mut strm = blank_stream();
            let ret = back_init(&mut strm, bits, &mut win);
            assert_eq!(
                ret,
                ReturnCode::STREAM_ERROR.as_i32(),
                "windowBits {bits} must be refused"
            );
            assert!(strm.state.is_null(), "windowBits {bits} installed a state");
        }
        for bits in 8..=15 {
            let mut strm = blank_stream();
            let ret = back_init(&mut strm, bits, &mut win);
            assert_eq!(
                ret,
                ReturnCode::OK.as_i32(),
                "windowBits {bits} must be accepted"
            );
            assert!(!strm.state.is_null());
            assert_eq!(
                unsafe { inflateBackEnd(&mut strm) },
                ReturnCode::OK.as_i32()
            );
            assert!(strm.state.is_null(), "inflateBackEnd must clear state");
        }
    }

    /// A window of exactly `1 << windowBits` bytes is enough, for every exponent.
    ///
    /// ★ There is deliberately no test for a *short* window, and its absence is the
    /// point. `inflateBackInit_` receives a bare `unsigned char *` and `zlib.h`
    /// L1119-L1120 makes the length a promise from the caller -- "window is a caller
    /// supplied buffer of that size". Neither C nor this port can check it, so
    /// passing a short one is a contract violation, recorded in
    /// [`inflateBackInit_`]'s `# Safety` section, and a test that passed one would be
    /// performing the overrun rather than detecting it.
    ///
    /// What *is* checkable is the other half: that no more than the promised extent
    /// is ever claimed. Each iteration hands over a buffer of exactly `1 <<
    /// windowBits` bytes and decodes through it, so an over-claim would be an
    /// out-of-bounds access that ASan and Miri would both report.
    #[test]
    fn a_window_of_exactly_the_requested_size_suffices() {
        for bits in 8..=15_u32 {
            let mut window = vec![0xa5_u8; 1_usize << bits];
            let mut strm = blank_stream();
            let ret = back_init(&mut strm, c_int::try_from(bits).unwrap(), &mut window);
            assert_eq!(ret, ReturnCode::OK.as_i32(), "windowBits {bits}");

            // Decode a real stream through it, so the window is actually written.
            let mut sink: Vec<u8> = Vec::new();
            let stream = deflate_raw(b"exactly sized window", 6);
            stage(&mut strm, &stream);
            let code = unsafe {
                inflateBack(
                    &mut strm,
                    Some(refuse_in),
                    core::ptr::null_mut(),
                    Some(collect_out),
                    core::ptr::from_mut(&mut sink).cast::<c_void>(),
                )
            };
            assert_eq!(code, ReturnCode::STREAM_END.as_i32(), "windowBits {bits}");
            assert_eq!(sink, b"exactly sized window", "windowBits {bits}");
            assert_eq!(
                unsafe { inflateBackEnd(&mut strm) },
                ReturnCode::OK.as_i32()
            );
        }
    }

    /// A null `window` is `Z_STREAM_ERROR`, and nothing is installed or written.
    #[test]
    fn a_null_window_is_refused_without_installing_a_state() {
        let (version, size) = version_args();
        let mut strm = blank_stream();
        strm.msg = c"previous".as_ptr();
        let ret = unsafe { inflateBackInit_(&mut strm, 15, core::ptr::null_mut(), version, size) };
        assert_eq!(ret, ReturnCode::STREAM_ERROR.as_i32());
        assert!(strm.state.is_null());
        // ★ C clears `msg` only *after* the guards (its L33-L36), so a rejected
        // argument list leaves a previous message standing.
        assert_eq!(msg_of(&strm), Some("previous"));
    }

    /// A half-supplied hook pair is refused, in both directions.
    #[test]
    fn a_half_supplied_allocator_pair_is_refused() {
        let mut win = [0_u8; 256];
        let (version, size) = version_args();
        let win_ptr = win.as_mut_ptr();

        let mut strm = blank_stream();
        strm.zalloc = Some(mem_alloc);
        let ret = unsafe { inflateBackInit_(&mut strm, 8, win_ptr, version, size) };
        assert_eq!(ret, ReturnCode::STREAM_ERROR.as_i32());

        let mut strm = blank_stream();
        strm.zfree = Some(mem_free);
        let ret = unsafe { inflateBackInit_(&mut strm, 8, win_ptr, version, size) };
        assert_eq!(ret, ReturnCode::STREAM_ERROR.as_i32());
    }

    /// Every null-argument combination is answered, and none of them crashes.
    #[test]
    fn every_null_argument_combination_is_answered() {
        let mut win = vec![0xa5_u8; 32768];
        let zone = ZoneHandle::new();
        let mut live = blank_stream();
        zone.install(&mut live);
        assert_eq!(back_init(&mut live, 15, &mut win), ReturnCode::OK.as_i32());
        let live_ptr: *mut z_stream = &mut live;

        let stream_error = ReturnCode::STREAM_ERROR.as_i32();
        for strm in [core::ptr::null_mut(), live_ptr] {
            for input in [None, Some(refuse_in as _)] {
                for output in [None, Some(accept_out as _)] {
                    for in_desc in [core::ptr::null_mut(), live_ptr.cast::<c_void>()] {
                        for out_desc in [core::ptr::null_mut(), live_ptr.cast::<c_void>()] {
                            let ret =
                                unsafe { inflateBack(strm, input, in_desc, output, out_desc) };
                            if strm.is_null() || input.is_none() || output.is_none() {
                                assert_eq!(ret, stream_error, "a null argument must be refused");
                            } else {
                                // A live stream with both callbacks present decodes
                                // nothing (no staged input, `in()` refuses) and so
                                // reports the buffer error, never `Z_OK`.
                                assert_eq!(ret, ReturnCode::BUF_ERROR.as_i32());
                            }
                        }
                    }
                }
            }
        }

        assert_eq!(
            unsafe { inflateBackEnd(&mut live) },
            ReturnCode::OK.as_i32()
        );
        assert_eq!(unsafe { inflateBackEnd(&mut live) }, stream_error);
        mem_detach(&mut live);
        zone.assert_clean("null matrix");
    }

    // -----------------------------------------------------------------------
    // Decoding, and the callback polarities
    // -----------------------------------------------------------------------

    /// A complete raw stream decodes byte for byte, at every level, through `out()`.
    #[test]
    fn a_complete_raw_stream_decodes_through_the_callbacks() {
        let payload: Vec<u8> = (0..40_000_u32)
            .map(|index| u8::try_from(index % 251).unwrap_or(0))
            .collect();
        for level in [0, 1, 6, 9] {
            let stream = deflate_raw(&payload, level);
            let (code, output, msg) = decode_staged(&stream);
            assert_eq!(code, ReturnCode::STREAM_END.as_i32(), "level {level}");
            assert_eq!(output, payload, "level {level} round trip");
            assert_eq!(msg, None, "level {level} must publish no message");
        }
    }

    /// The same stream decodes when `in()` supplies it in small chunks instead.
    #[test]
    fn a_chunked_callback_supply_decodes_identically() {
        let payload = b"the quick brown fox jumps over the lazy dog, repeatedly. ".repeat(300);
        let stream = deflate_raw(&payload, 6);
        for chunk in [1_usize, 2, 7, 64, 4096] {
            let mut window = vec![0xa5_u8; 32768];
            let mut strm = blank_stream();
            assert_eq!(
                back_init(&mut strm, 15, &mut window),
                ReturnCode::OK.as_i32()
            );
            let mut feed = Box::new(FeedDesc::new(&stream, chunk));
            let mut sink: Vec<u8> = Vec::new();
            let code = unsafe {
                inflateBack(
                    &mut strm,
                    Some(feed_in),
                    core::ptr::from_mut(feed.as_mut()).cast::<c_void>(),
                    Some(collect_out),
                    core::ptr::from_mut(&mut sink).cast::<c_void>(),
                )
            };
            assert_eq!(code, ReturnCode::STREAM_END.as_i32(), "chunk {chunk}");
            assert_eq!(sink, payload, "chunk {chunk}");
            assert_eq!(
                unsafe { inflateBackEnd(&mut strm) },
                ReturnCode::OK.as_i32()
            );
        }
    }

    /// ★ `in()` fails by returning **zero**, and that leaves `next_in` NULL.
    #[test]
    fn an_input_failure_is_a_buffer_error_with_a_null_next_in() {
        let mut window = vec![0xa5_u8; 32768];
        let mut strm = blank_stream();
        assert_eq!(
            back_init(&mut strm, 15, &mut window),
            ReturnCode::OK.as_i32()
        );

        // Nothing staged and `in()` declines immediately, so not even the three
        // block-header bits can be read.
        let code = unsafe {
            inflateBack(
                &mut strm,
                Some(refuse_in),
                core::ptr::null_mut(),
                Some(accept_out),
                core::ptr::null_mut(),
            )
        };
        assert_eq!(code, ReturnCode::BUF_ERROR.as_i32());
        // `zlib.h` L1199-L1202: NULL means it was `in()` that failed.
        assert!(strm.next_in.is_null());
        assert_eq!(strm.avail_in, 0);
        assert_eq!(msg_of(&strm), None);

        assert_eq!(
            unsafe { inflateBackEnd(&mut strm) },
            ReturnCode::OK.as_i32()
        );
    }

    /// ★ `out()` fails by returning **non-zero**, and that leaves `next_in` non-NULL.
    #[test]
    fn an_output_failure_is_a_buffer_error_with_a_non_null_next_in() {
        let stream = deflate_raw(b"enough bytes to force an out() call", 6);
        let mut window = vec![0xa5_u8; 32768];
        let mut strm = blank_stream();
        assert_eq!(
            back_init(&mut strm, 15, &mut window),
            ReturnCode::OK.as_i32()
        );
        stage(&mut strm, &stream);

        // `push` returns non-zero exactly when `out_desc` is non-null.
        let mut cookie = 0_u8;
        let code = unsafe {
            inflateBack(
                &mut strm,
                Some(refuse_in),
                core::ptr::null_mut(),
                Some(push),
                core::ptr::from_mut(&mut cookie).cast::<c_void>(),
            )
        };
        assert_eq!(code, ReturnCode::BUF_ERROR.as_i32());
        // `zlib.h` L1201-L1202: non-NULL means it was `out()`.
        assert!(!strm.next_in.is_null());

        // The same stream with a null `out_desc` -- so `push` succeeds -- ends cleanly.
        stage(&mut strm, &stream);
        let code = unsafe {
            inflateBack(
                &mut strm,
                Some(refuse_in),
                core::ptr::null_mut(),
                Some(push),
                core::ptr::null_mut(),
            )
        };
        assert_eq!(code, ReturnCode::STREAM_END.as_i32());

        assert_eq!(
            unsafe { inflateBackEnd(&mut strm) },
            ReturnCode::OK.as_i32()
        );
    }

    /// A null `next_in` contributes nothing whatever `avail_in` claims.
    #[test]
    fn a_null_next_in_contributes_nothing_whatever_avail_in_says() {
        let mut window = vec![0xa5_u8; 32768];
        let mut strm = blank_stream();
        assert_eq!(
            back_init(&mut strm, 15, &mut window),
            ReturnCode::OK.as_i32()
        );

        // C's `have = next != Z_NULL ? strm->avail_in : 0` (its L219): a lying
        // `avail_in` must not be believed.
        strm.next_in = core::ptr::null();
        strm.avail_in = 4096;
        let code = unsafe {
            inflateBack(
                &mut strm,
                Some(refuse_in),
                core::ptr::null_mut(),
                Some(accept_out),
                core::ptr::null_mut(),
            )
        };
        assert_eq!(code, ReturnCode::BUF_ERROR.as_i32());

        assert_eq!(
            unsafe { inflateBackEnd(&mut strm) },
            ReturnCode::OK.as_i32()
        );
    }

    /// Each of `test/infcover.c`'s malformed fixtures reports the reference text.
    ///
    /// `try()` asserts `strcmp(id, strm.msg) == 0` after the `inflateBack` pass (its
    /// L565-L568), so every one of these strings is part of the ABI.
    #[test]
    fn malformed_streams_publish_the_reference_messages() {
        let cases: &[(&[u8], &str)] = &[
            (&[0, 0, 0, 0, 0], "invalid stored block lengths"),
            (&[6], "invalid block type"),
            (&[0xfc, 0, 0], "too many length or distance symbols"),
            (&[4, 0, 0xfe, 0xff], "invalid code lengths set"),
            (&[4, 0, 0x24, 0x49, 0], "invalid bit length repeat"),
            (
                &[4, 0, 0x24, 0xe9, 0xff, 0x6d],
                "invalid code -- missing end-of-block",
            ),
            (&[2, 0x7e, 0xff, 0xff], "invalid distance code"),
            (
                &[0x0c, 0xc0, 0x81, 0, 0, 0, 0, 0, 0x90, 0xff, 0x6b, 4, 0],
                "invalid distance too far back",
            ),
        ];
        for (stream, expected) in cases {
            let (code, _, msg) = decode_staged(stream);
            assert_eq!(
                code,
                ReturnCode::DATA_ERROR.as_i32(),
                "{expected} must be a data error"
            );
            assert_eq!(msg, Some(*expected));
        }
    }

    /// The two well-formed fixtures `test/infcover.c` decodes both ways.
    #[test]
    fn the_reference_well_formed_fixtures_decode() {
        // "3 0" -- an empty fixed-code block. Nothing is written, so `out()` is
        // never called at all.
        let (code, output, msg) = decode_staged(&[3, 0]);
        assert_eq!(code, ReturnCode::STREAM_END.as_i32());
        assert!(output.is_empty());
        assert_eq!(msg, None);

        // "1 1 0 fe ff 0" -- a stored block holding one zero byte.
        let (code, output, msg) = decode_staged(&[1, 1, 0, 0xfe, 0xff, 0]);
        assert_eq!(code, ReturnCode::STREAM_END.as_i32());
        assert_eq!(output, vec![0]);
        assert_eq!(msg, None);
    }

    // -----------------------------------------------------------------------
    // The mode written from inside a callback
    // -----------------------------------------------------------------------

    /// ★ `test/infcover.c` L485-L500, the whole `cover_back` body, in order.
    #[test]
    fn the_infcover_back_sequence_passes_in_full() {
        let mut win = vec![0xa5_u8; 32768];
        let zone = ZoneHandle::new();
        let mut strm = blank_stream();

        // L485-L486.
        zone.install(&mut strm);
        assert_eq!(back_init(&mut strm, 15, &mut win), ReturnCode::OK.as_i32());

        // L487-L490: `avail_in = 2; next_in = "\x03";` decodes an empty stream from
        // the staged bytes alone, so `pull` is never reached.
        let mut pull_desc = Box::new(PullDesc {
            strm: core::ptr::null_mut(),
            next: 0,
        });
        stage(&mut strm, &[0x03, 0x00]);
        let code = unsafe {
            inflateBack(
                &mut strm,
                Some(pull),
                core::ptr::null_mut(),
                Some(push),
                core::ptr::null_mut(),
            )
        };
        assert_eq!(code, ReturnCode::STREAM_END.as_i32(), "L490");

        // L492-L495: a non-null `out_desc` forces `push` to fail.
        stage(&mut strm, &[0x63, 0x00, 0x00]);
        let strm_ptr: *mut z_stream = &mut strm;
        let code = unsafe {
            inflateBack(
                strm_ptr,
                Some(pull),
                core::ptr::null_mut(),
                Some(push),
                strm_ptr.cast::<c_void>(),
            )
        };
        assert_eq!(code, ReturnCode::BUF_ERROR.as_i32(), "L495");

        // L497-L498: a non-null `in_desc` makes `pull` write `mode = SYNC`, which is
        // not a mode `inflateBack` implements.
        pull_desc.strm = strm_ptr;
        pull_desc.next = 0;
        let code = unsafe {
            inflateBack(
                strm_ptr,
                Some(pull),
                core::ptr::from_mut(pull_desc.as_mut()).cast::<c_void>(),
                Some(push),
                core::ptr::null_mut(),
            )
        };
        assert_eq!(code, ReturnCode::STREAM_ERROR.as_i32(), "L498");
        // C's `default` arm sets no message, and L214 had cleared it.
        assert_eq!(msg_of(&strm), None);
        // The slot still holds what the caller wrote, as it would in C.
        assert_eq!(mode_tag(&strm), SYNC_TAG);

        // L499-L500.
        assert_eq!(
            unsafe { inflateBackEnd(&mut strm) },
            ReturnCode::OK.as_i32()
        );
        mem_detach(&mut strm);
        zone.assert_clean("inflateBack bad state");
        assert!(
            zone.highwater() > 0,
            "the state must come from the caller's zalloc"
        );

        // L502-L504: the built-in memory routines, with all three hooks cleared.
        assert_eq!(back_init(&mut strm, 15, &mut win), ReturnCode::OK.as_i32());
        assert_eq!(
            unsafe { inflateBackEnd(&mut strm) },
            ReturnCode::OK.as_i32()
        );
    }

    /// A hostile mode write is honoured even when the stream would otherwise finish.
    #[test]
    fn a_mode_written_from_inside_a_callback_is_honoured() {
        let payload = b"a stream long enough that in() is definitely consulted".repeat(200);
        let stream = deflate_raw(&payload, 6);

        // The same stream, decoded with a well-behaved `in()`, ends normally.
        let mut window = vec![0xa5_u8; 32768];
        let mut strm = blank_stream();
        assert_eq!(
            back_init(&mut strm, 15, &mut window),
            ReturnCode::OK.as_i32()
        );
        let mut feed = Box::new(FeedDesc::new(&stream, 16));
        let mut sink: Vec<u8> = Vec::new();
        let code = unsafe {
            inflateBack(
                &mut strm,
                Some(feed_in),
                core::ptr::from_mut(feed.as_mut()).cast::<c_void>(),
                Some(collect_out),
                core::ptr::from_mut(&mut sink).cast::<c_void>(),
            )
        };
        assert_eq!(code, ReturnCode::STREAM_END.as_i32());
        assert_eq!(sink, payload);

        // The same stream again, but `in()` now scribbles `SYNC` into the opaque
        // state. C's `default` arm answers `Z_STREAM_ERROR`; so does this.
        let strm_ptr: *mut z_stream = &mut strm;
        let mut pull_desc = Box::new(PullDesc {
            strm: strm_ptr,
            next: 0,
        });
        let code = unsafe {
            inflateBack(
                strm_ptr,
                Some(pull),
                core::ptr::from_mut(pull_desc.as_mut()).cast::<c_void>(),
                Some(accept_out),
                core::ptr::null_mut(),
            )
        };
        assert_eq!(code, ReturnCode::STREAM_ERROR.as_i32());
        assert_eq!(mode_tag(&strm), SYNC_TAG);

        // And the stream is still usable afterwards, exactly as `cover_back` requires
        // by calling `inflateBackEnd` next.
        assert_eq!(
            unsafe { inflateBackEnd(&mut strm) },
            ReturnCode::OK.as_i32()
        );
    }

    /// A tag that names no state at all is refused the same way `SYNC` is.
    #[test]
    fn a_tag_naming_no_state_is_refused_and_the_block_stays_usable() {
        /// Writes a value that is not an `inflate_mode` at all.
        unsafe extern "C" fn wreck(desc: *mut c_void, buf: *mut *const Bytef) -> c_uint {
            let _ = buf;
            let strm = desc.cast::<z_stream>();
            if strm.is_null() {
                return 0;
            }
            let prefix = unsafe { core::ptr::addr_of!((*strm).state).read() };
            let prefix = prefix.cast::<StatePrefix>();
            if !prefix.is_null() {
                unsafe { core::ptr::addr_of_mut!((*prefix).tag).write(42) };
            }
            0
        }

        let mut window = vec![0xa5_u8; 32768];
        let mut strm = blank_stream();
        assert_eq!(
            back_init(&mut strm, 15, &mut window),
            ReturnCode::OK.as_i32()
        );
        let strm_ptr: *mut z_stream = &mut strm;

        let code = unsafe {
            inflateBack(
                strm_ptr,
                Some(wreck),
                strm_ptr.cast::<c_void>(),
                Some(accept_out),
                core::ptr::null_mut(),
            )
        };
        assert_eq!(code, ReturnCode::STREAM_ERROR.as_i32());
        // ★ 42 names no state, so it is *not* published back: the block keeps a tag
        // its own validator accepts, which is what lets `inflateBackEnd` still work.
        // C's `inflateBackEnd` inspects no mode and gets that for free.
        assert_ne!(mode_tag(&strm), 42);
        assert!(Mode::from_raw(mode_tag(&strm)).is_some());
        assert_eq!(
            unsafe { inflateBackEnd(&mut strm) },
            ReturnCode::OK.as_i32()
        );
    }

    /// The set of modes `inflateBack` drives is exactly the six the reference has
    /// arms for.
    #[test]
    fn the_drivable_set_is_exactly_the_six_reference_arms() {
        let expected = [
            Mode::Type,
            Mode::Stored,
            Mode::Table,
            Mode::Len,
            Mode::Done,
            Mode::Bad,
        ];
        let mut accepted = 0;
        // The whole `inflate_mode` range, plus a margin either side.
        for tag in 16_170..16_225 {
            let named = Mode::from_raw(tag);
            let want = named.is_some_and(|mode| expected.contains(&mode));
            assert_eq!(drivable(tag), want, "tag {tag}");
            accepted += usize::from(drivable(tag));
        }
        assert_eq!(accepted, expected.len());
        // Values nowhere near the range are refused too.
        for tag in [i32::MIN, -1, 0, 1, 42, i32::MAX] {
            assert!(!drivable(tag), "tag {tag}");
        }
        // ★ `SYNC` in particular, which is the one `test/infcover.c` writes.
        assert!(!drivable(SYNC_TAG));
        assert_eq!(Mode::from_raw(SYNC_TAG), Some(Mode::Sync));
    }

    /// [`ModeWatch`] latches the first non-drivable tag and only the first.
    #[test]
    fn the_mode_watch_latches_the_first_hostile_tag_only() {
        // Boxed, and touched only through `slot`, because that is the shape the real
        // thing has: one raw pointer into a block, shared between this side and the
        // caller's callback. Writing the local directly instead would invalidate the
        // pointer it had already been given.
        let mut tag = Box::new(Mode::Type.as_raw());
        let slot: *mut c_int = core::ptr::from_mut(tag.as_mut());
        let watch = ModeWatch::new(slot);

        assert!(watch.observe(), "Type is drivable");
        assert_eq!(watch.latched(), None);

        watch.publish(Mode::Len.as_raw());
        assert_eq!(unsafe { slot.read() }, Mode::Len.as_raw());
        assert!(watch.observe(), "Len is drivable");
        assert_eq!(watch.latched(), None);

        // What a hostile callback does: write through the pointer it reached via
        // `strm->state`, which is the same four bytes.
        unsafe { slot.write(SYNC_TAG) };
        assert!(!watch.observe());
        assert_eq!(watch.latched(), Some(SYNC_TAG));

        // A second, different hostile value does not displace the first: C's dispatch
        // stops at the one it saw.
        unsafe { slot.write(Mode::Head.as_raw()) };
        assert!(!watch.observe());
        assert_eq!(watch.latched(), Some(SYNC_TAG));

        // `publish` still installs whatever it is told, so the epilogue can restore a
        // sane tag over a hostile one.
        watch.publish(Mode::Bad.as_raw());
        assert_eq!(unsafe { slot.read() }, Mode::Bad.as_raw());
        assert!(watch.observe());
    }

    // -----------------------------------------------------------------------
    // The ABI prefix, the write-back, and the allocator
    // -----------------------------------------------------------------------

    /// The installed block exposes the tag where caller-compiled code expects it.
    #[test]
    fn the_state_block_exposes_the_mode_tag_at_offset_eight() {
        assert_eq!(core::mem::offset_of!(StatePrefix, strm), 0);
        assert_eq!(core::mem::offset_of!(StatePrefix, tag), 8);
        assert_eq!(size_of::<c_int>(), 4);

        let mut window = vec![0xa5_u8; 32768];
        let mut strm = blank_stream();
        assert_eq!(
            back_init(&mut strm, 15, &mut window),
            ReturnCode::OK.as_i32()
        );

        // The prefix records the caller's own stream pointer, which is what makes the
        // owner-identity check meaningful.
        let prefix = strm.state.cast::<StatePrefix>();
        let owner = unsafe { core::ptr::addr_of!((*prefix).strm).read() };
        assert!(core::ptr::eq(owner, &raw const strm));
        // And a fresh state's tag names a real mode, so the block passes its own
        // validator from the moment it exists.
        assert!(Mode::from_raw(mode_tag(&strm)).is_some());

        // The block really is `{ prefix, state }` and nothing shifts the prefix.
        assert!(size_of::<StateBlock<BackSlot>>() >= size_of::<StatePrefix>());

        assert_eq!(
            unsafe { inflateBackEnd(&mut strm) },
            ReturnCode::OK.as_i32()
        );
    }

    /// The epilogue writes `next_in`, `avail_in` and `msg` -- and nothing else.
    #[test]
    fn the_epilogue_writes_only_the_three_reference_members() {
        let stream = deflate_raw(b"only three members change", 6);
        let mut window = vec![0xa5_u8; 32768];
        let mut strm = blank_stream();
        assert_eq!(
            back_init(&mut strm, 15, &mut window),
            ReturnCode::OK.as_i32()
        );

        // Distinctive values in every member `inflateBack` must leave alone.
        strm.total_in = 111;
        strm.total_out = 222;
        strm.next_out = core::ptr::without_provenance_mut(0x1000);
        strm.avail_out = 333;
        strm.data_type = 444;
        strm.adler = 555;
        strm.reserved = 666;
        stage(&mut strm, &stream);

        let code = unsafe {
            inflateBack(
                &mut strm,
                Some(refuse_in),
                core::ptr::null_mut(),
                Some(accept_out),
                core::ptr::null_mut(),
            )
        };
        assert_eq!(code, ReturnCode::STREAM_END.as_i32());

        // ★ `infback.c` L560-L569 assigns `next_in` and `avail_in` only, and L214
        // clears `msg`. It keeps no totals, because that is what the callbacks'
        // descriptors are for.
        assert_eq!(strm.total_in, 111);
        assert_eq!(strm.total_out, 222);
        assert_eq!(strm.next_out.addr(), 0x1000);
        assert_eq!(strm.avail_out, 333);
        assert_eq!(strm.data_type, 444);
        assert_eq!(strm.adler, 555);
        assert_eq!(strm.reserved, 666);
        assert_eq!(msg_of(&strm), None);
        // The unused input is published, and the whole stream was consumed.
        assert!(!strm.next_in.is_null());
        assert_eq!(strm.avail_in, 0);

        assert_eq!(
            unsafe { inflateBackEnd(&mut strm) },
            ReturnCode::OK.as_i32()
        );
    }

    /// The state is one block, taken from and returned to the caller's hooks.
    #[test]
    fn the_state_is_a_single_block_from_the_callers_allocator() {
        let mut window = vec![0xa5_u8; 32768];
        let zone = ZoneHandle::new();
        let mut strm = blank_stream();
        zone.install(&mut strm);

        assert_eq!(
            back_init(&mut strm, 15, &mut window),
            ReturnCode::OK.as_i32()
        );
        // ★ Exactly one: `infback.c` L51 allocates the state and no window, which is
        // what makes the LIFO discipline unbreakable for this family.
        assert_eq!(zone.live(), 1);
        let outstanding = zone.total();
        assert!(outstanding >= size_of::<StatePrefix>());

        // Decoding allocates nothing further.
        stage(&mut strm, &[3, 0]);
        let code = unsafe {
            inflateBack(
                &mut strm,
                Some(refuse_in),
                core::ptr::null_mut(),
                Some(accept_out),
                core::ptr::null_mut(),
            )
        };
        assert_eq!(code, ReturnCode::STREAM_END.as_i32());
        assert_eq!(zone.live(), 1);
        assert_eq!(zone.total(), outstanding);

        assert_eq!(
            unsafe { inflateBackEnd(&mut strm) },
            ReturnCode::OK.as_i32()
        );
        mem_detach(&mut strm);
        zone.assert_clean("single block");
        assert_eq!(zone.highwater(), outstanding);
    }

    /// An allocator that refuses the state yields `Z_MEM_ERROR` and installs nothing.
    #[test]
    fn a_refused_allocation_is_a_memory_error() {
        let mut window = vec![0xa5_u8; 32768];
        let zone = ZoneHandle::new();
        zone.set_limit(1);
        let mut strm = blank_stream();
        zone.install(&mut strm);

        assert_eq!(
            back_init(&mut strm, 15, &mut window),
            ReturnCode::MEM_ERROR.as_i32()
        );
        assert!(strm.state.is_null());
        mem_detach(&mut strm);
        zone.assert_clean("refused allocation");
    }

    /// A state this library did not install is refused rather than acted on.
    #[test]
    fn a_foreign_state_pointer_is_refused_by_both_entry_points() {
        let mut foreign = [0_u8; 256];
        let mut strm = blank_stream();
        strm.state = foreign.as_mut_ptr().cast();

        let stream_error = ReturnCode::STREAM_ERROR.as_i32();
        let code = unsafe {
            inflateBack(
                &mut strm,
                Some(refuse_in),
                core::ptr::null_mut(),
                Some(accept_out),
                core::ptr::null_mut(),
            )
        };
        assert_eq!(code, stream_error);
        assert_eq!(unsafe { inflateBackEnd(&mut strm) }, stream_error);
        // Refused, not freed: the caller's array is still the stream's state.
        assert!(core::ptr::eq(strm.state.cast::<u8>(), foreign.as_ptr()));
    }

    /// A state belonging to a different stream fails the owner-identity check.
    #[test]
    fn a_state_owned_by_another_stream_is_refused() {
        let mut window = vec![0xa5_u8; 32768];
        let mut owner = blank_stream();
        assert_eq!(
            back_init(&mut owner, 15, &mut window),
            ReturnCode::OK.as_i32()
        );

        let mut thief = blank_stream();
        thief.state = owner.state;
        let code = unsafe {
            inflateBack(
                &mut thief,
                Some(refuse_in),
                core::ptr::null_mut(),
                Some(accept_out),
                core::ptr::null_mut(),
            )
        };
        assert_eq!(code, ReturnCode::STREAM_ERROR.as_i32());
        assert_eq!(
            unsafe { inflateBackEnd(&mut thief) },
            ReturnCode::STREAM_ERROR.as_i32()
        );
        // Refused, not freed, so the real owner can still end it.
        assert!(core::ptr::eq(thief.state, owner.state));

        assert_eq!(
            unsafe { inflateBackEnd(&mut owner) },
            ReturnCode::OK.as_i32()
        );
    }

    /// A stream is reusable across calls without any reset, per `zlib.h` L1150-L1152.
    #[test]
    fn one_state_decodes_several_streams_in_succession() {
        let first = deflate_raw(b"first payload", 9);
        let second = deflate_raw(b"a rather different second payload", 1);
        let mut window = vec![0xa5_u8; 32768];
        let mut strm = blank_stream();
        assert_eq!(
            back_init(&mut strm, 15, &mut window),
            ReturnCode::OK.as_i32()
        );

        for (stream, expected) in [
            (&first, &b"first payload"[..]),
            (&second, &b"a rather different second payload"[..]),
            (&first, &b"first payload"[..]),
        ] {
            let mut sink: Vec<u8> = Vec::new();
            stage(&mut strm, stream);
            let code = unsafe {
                inflateBack(
                    &mut strm,
                    Some(refuse_in),
                    core::ptr::null_mut(),
                    Some(collect_out),
                    core::ptr::from_mut(&mut sink).cast::<c_void>(),
                )
            };
            assert_eq!(code, ReturnCode::STREAM_END.as_i32());
            assert_eq!(sink, expected);
        }

        assert_eq!(
            unsafe { inflateBackEnd(&mut strm) },
            ReturnCode::OK.as_i32()
        );
    }

    /// A smaller window still decodes a stream deflated for that window.
    #[test]
    fn a_smaller_window_decodes_a_matching_stream() {
        let payload = b"small window, small history, still exact. ".repeat(40);
        let (version, size) = version_args();
        let mut strm = blank_stream();
        let ret = unsafe {
            crate::deflate::deflateInit2_(
                &mut strm,
                6,
                Z_DEFLATED,
                -9,
                8,
                Z_DEFAULT_STRATEGY,
                version,
                size,
            )
        };
        assert_eq!(ret, ReturnCode::OK.as_i32());
        let mut out = vec![0_u8; payload.len() * 2 + 4096];
        strm.next_in = payload.as_ptr();
        strm.avail_in = uInt::try_from(payload.len()).unwrap();
        strm.next_out = out.as_mut_ptr();
        strm.avail_out = uInt::try_from(out.len()).unwrap();
        assert_eq!(
            unsafe { crate::deflate::deflate(&mut strm, Z_FINISH) },
            ReturnCode::STREAM_END.as_i32()
        );
        out.truncate(out.len() - usize::try_from(strm.avail_out).unwrap());
        assert_eq!(
            unsafe { crate::deflate::deflateEnd(&mut strm) },
            ReturnCode::OK.as_i32()
        );

        let mut window = vec![0xa5_u8; 512];
        let mut back = blank_stream();
        assert_eq!(
            back_init(&mut back, 9, &mut window),
            ReturnCode::OK.as_i32()
        );
        let mut sink: Vec<u8> = Vec::new();
        stage(&mut back, &out);
        let code = unsafe {
            inflateBack(
                &mut back,
                Some(refuse_in),
                core::ptr::null_mut(),
                Some(collect_out),
                core::ptr::from_mut(&mut sink).cast::<c_void>(),
            )
        };
        assert_eq!(code, ReturnCode::STREAM_END.as_i32());
        assert_eq!(sink, payload);
        assert_eq!(
            unsafe { inflateBackEnd(&mut back) },
            ReturnCode::OK.as_i32()
        );
    }

    /// `narrow_avail` saturates instead of wrapping.
    #[test]
    fn narrow_avail_saturates() {
        assert_eq!(narrow_avail(0), 0);
        assert_eq!(narrow_avail(7), 7);
        assert_eq!(narrow_avail(uInt::MAX as usize), uInt::MAX);
        assert_eq!(narrow_avail(uInt::MAX as usize + 1), uInt::MAX);
        assert_eq!(narrow_avail(usize::MAX), uInt::MAX);
    }
}
