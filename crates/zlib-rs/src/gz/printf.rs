//! The bounded-buffer half of `gzprintf`: the Rust counterpart of `gzvprintf` (`gzwrite.c` L403-L485).
//!
//! `gzprintf` is the one entry point in the whole `gzFile` layer that cannot live in one
//! piece. C's body does three things in sequence -- prepare a bounded scratch region inside the
//! input buffer, run `vsnprintf` into it, then account for whatever came back -- and only the
//! middle step needs a raw pointer and a `va_list`. This module owns the first and third steps,
//! entirely in safe Rust; the variadic boundary owns the second.
//!
//! # Why the split is structural rather than stylistic
//!
//! The crate root carries `#![forbid(unsafe_code)]`, and consuming a C variadic argument list is
//! unsafe, as is any read through the `char *format` a caller hands in. Neither can appear anywhere
//! in this crate, so formatting *must* happen one layer up in `libz-rs-sys`, which is the only crate
//! permitted to **use** `unsafe` and therefore the only one that can dereference such a pointer.
//! Keeping the argument list there is not a convenience -- it is the only arrangement the safety
//! posture allows.
//!
//! # SETTLED: how the facade obtains the argument list at the declared MSRV
//!
//! The split above is settled, and so is the mechanism for the middle step. It was an open question
//! when this module was written; option 1 below was chosen, it is built, and it is call-tested.
//! **Read the resolution at the end of this section together with the constraints, because the
//! trade-off it accepts is real and is stated there.**
//!
//! The obstacle is concrete. `core::ffi::VaList` and `VaListImpl` are unstable
//! (`feature(c_variadic)`), so a `#[no_mangle] pub extern "C" fn gzvprintf(..., va: VaList)` cannot
//! be written on stable Rust at all -- and this workspace declares `rust-version = "1.80"` with a
//! stable toolchain, so nightly is not available to it. `gzprintf` is worse: it is genuinely
//! variadic (`zlib.h` L1549), and stable Rust cannot *define* a variadic function in any form.
//! Guessing a `va_list` layout and forwarding it as an opaque pointer is not a way out either:
//! the type is `__va_list_tag[1]` on x86-64 SysV, a by-value struct on AArch64 AAPCS64 and a
//! `char *` on i386, so one Rust declaration cannot be correct on all three.
//!
//! The code snippet below shows the *shape* of the call. The three candidate approaches were:
//!
//! 1. **A small C shim** exporting `gzprintf`/`gzvprintf` and forwarding the formatted result to
//!    this module's two halves. Works on stable, but adds a C translation unit to the library.
//! 2. **Raise the toolchain for these two symbols only** and use `feature(c_variadic)`. Contradicts
//!    the declared MSRV and the stable-channel requirement.
//! 3. **Omit both symbols.** Not an option: they are part of the frozen 95-symbol surface, and
//!    `nm` parity would fail.
//!
//! Note what a symbol-level gate can and cannot establish: `nm` proves a symbol is *present*. It
//! cannot prove the variadic calling convention is right, because that is only exercised by an
//! actual C call with actual varargs. So the acceptance evidence for these two functions has to be
//! a compiled C caller that passes real arguments and checks the bytes written -- not a symbol diff.
//!
//! **Resolution: option 1, with the C translation unit kept out of the CARGO build.**
//! `crates/libz-rs-sys/csrc/gzprintf_shim.c` owns nothing but `va_start`/`va_end` and `vsnprintf`,
//! and calls the two `pub(crate)` adapters in `crates/libz-rs-sys/src/gz.rs` that wrap this module.
//! It is compiled by the PACKAGING layer -- `Makefile.in`'s `rust` target, which already runs a C
//! compiler to relink the archive through `zlib.map` -- and never by `build.rs`, so
//! `cargo build --release` still works on a machine with no C toolchain (AAP §0.6.4.1). That is how
//! option 1's cost is bounded: the shipped *shared library* carries one C translation unit, and the
//! Rust build of the crate carries none.
//!
//! What the shim is, precisely: it DEFINES `gzprintf` and `gzvprintf`, so `va_start`/`va_end` stay
//! the C compiler's business and are correct by construction on every target, and it runs
//! `vsnprintf` into a bounded region this module hands it. It carries no accounting, no allocation,
//! no error policy and no format parsing of its own -- everything that could get zlib's semantics
//! wrong is here, in safe Rust, under test. The two halves meet through [`printf_begin`] and
//! [`printf_commit`], which the facade re-exports as the hidden helpers `_zlib_rs_gzprintf_begin`
//! and `_zlib_rs_gzprintf_commit`.
//!
//! The acceptance evidence this section demanded exists and is the required kind -- an actual C
//! caller with actual varargs, not a symbol diff. `test/example.c` L109 calls
//! `gzprintf(file, ", %s!", "hello")` and requires the return to be exactly 8, and
//! `make rust-test` compiles that UNMODIFIED driver against the staged Rust library and runs it.
//! Measured on both the configured and the unconfigured paths: `ldd` binds the staged library and
//! the driver passes.
//!
//! ONE QUALIFICATION REMAINS, and it is about the artifact rather than the mechanism: a bare
//! `cargo build` produces a library WITHOUT these two symbols, because nothing in that build
//! compiles the shim. A caller of `gzprintf` linked against the cargo output gets an undefined
//! reference. `make rust` is the command that produces the complete library, and
//! `crates/libz-rs-sys/Cargo.toml` states the same distinction where a reader of the manifest will
//! meet it.

//!
//! What is left behind, and lives here, is the part that actually matters for correctness: the
//! guard chain, the double-sized input buffer's geometry, the overflow sentinel, and the
//! three-part rejection test that together make up C's entire defence against a formatter writing
//! more than the buffer can hold. None of that is in the shim, so none of it is unverifiable.
//!
//! | C | `gzwrite.c` | Where it lives now |
//! |---|---|---|
//! | guards, `gz_init`, `gz_zero`, `gz_vacate`, sentinel plant | L416-L453 | [`printf_begin`] |
//! | `vsnprintf(next, state->size, format, va)` | L455-L469 | `csrc/gzprintf_shim.c` |
//! | rejection test, accounting, second `gz_vacate` | L471-L483 | [`printf_commit`] |
//! | `gzprintf`'s `va_start`/`va_end` wrapper | L487-L495 | `csrc/gzprintf_shim.c` |
//! | the `NO_vsnprintf` refusal and `ZLIB_INSECURE` opt-in | L404-L409, L457-L462 | `csrc/gzprintf_shim.c` |
//! | the `!STDC && !Z_HAVE_STDARG_H` `snprintf` twin | L499-L596 | not implemented; see below |
//!
//! # The interface contract, as implemented by `crates/libz-rs-sys/src/gz.rs`
//!
//! That file and `csrc/gzprintf_shim.c` now exist and discharge every obligation below; the
//! contract is kept stated here in full because it is what the two of them are checked *against*,
//! and because everything needed to implement the exported `gzprintf` and `gzvprintf` against this
//! module is on this page -- neither file has to read this one's body. The facade side is the pair
//! `_zlib_rs_gzprintf_begin` / `_zlib_rs_gzprintf_commit`, which wrap [`printf_begin`] and
//! [`printf_commit`]; the shim is the only caller of either.
//!
//! The block below is fenced as `text`, not as ignored Rust: it names a `va_list` mechanism that
//! stable Rust at this MSRV cannot spell, so it could never compile. An `ignore` fence would
//! present it as a doctest that merely happens not to run, which is the misleading form this crate
//! does not use anywhere -- there are no ignored doctests in it.
//!
//! ```text
//! // ILLUSTRATIVE SHAPE ONLY -- the real caller is `csrc/gzprintf_shim.c`. `va` stands for whatever
//! // mechanism is chosen; stable Rust at this MSRV cannot name a `va_list` type.
//! // Inside libz-rs-sys, where `unsafe` is permitted. The inner scope is what releases the
//! // loan's borrow of `state` before the accounting needs the stream back.
//! let reported = {
//!     let mut scratch = match printf_begin(state) {
//!         Ok(scratch) => scratch,
//!         Err(code) => return code.as_i32(),   // a negative zlib code, exactly as C returns
//!     };
//!     let region = scratch.as_mut_slice();     // EXACTLY `state->size` bytes, never a pointer
//!     // SAFETY: `region` is a live, uniquely borrowed slice of `region.len()` bytes, and
//!     // `vsnprintf` writes at most that many including its NUL terminator.
//!     unsafe { vsnprintf(region.as_mut_ptr().cast(), region.len(), format, va) }
//! };
//! let written = usize::try_from(reported).unwrap_or(usize::MAX);
//! match printf_commit(state, written) {
//!     Ok(len) => len,                          // the byte count, or 0 if the result did not fit
//!     Err(code) => code.as_i32(),
//! }
//! ```
//!
//! Why the region may safely be handed out as a raw pointer and reclaimed later: the shim calls
//! `vsnprintf` and **nothing else** between the two helpers. `vsnprintf` never re-enters this
//! library, so no second reference to the stream can exist while the region is outstanding, and the
//! loan's borrow has already ended by the time `_zlib_rs_gzprintf_commit` takes the stream back.
//! [`PrintfScratch::into_mut_slice`] exists for exactly this hand-off.
//!
//! The six obligations that go with it:
//!
//! 1. **The region is exactly `state->size` bytes.** Report `region.len()` through the `size`
//!    out-parameter and let the formatter use *that* as `vsnprintf`'s size argument; never a
//!    constant, and never `state->size` read separately. [`PrintfScratch::len`] is the
//!    authoritative width, and it is never zero.
//! 2. **The last byte is pre-set to zero as an overflow sentinel** (C's
//!    `next[state->size - 1] = 0`, L453). It detects a formatter that wrote *past* its bound, and
//!    nothing else. A conforming bounded formatter that truncates writes its terminating NUL at
//!    index `size - 1`, which leaves the sentinel zero — so the sentinel cannot see truncation, and
//!    is not what catches it; obligation 3's reported length is. What leaves a non-zero byte there
//!    is a formatter that wrote `size` characters or more, i.e. one that ignored the size argument
//!    entirely. That is a real configuration in the reference and not a hypothetical: under
//!    `NO_vsnprintf` the formatter is the *unbounded* `vsprintf` (`gzwrite.c` L455-L458), and the
//!    input buffer is deliberately allocated at twice `state->size` so an overrun of that kind
//!    lands in slack space where the sentinel can notice it instead of corrupting the heap.
//!    Accordingly the formatter must **not** unconditionally clear or overwrite that byte, and no
//!    code between [`printf_begin`] and [`printf_commit`] may touch the region other than by
//!    formatting into it.
//! 3. **Report the length the formatter *returned*, not the length it wrote.** C's `vsnprintf`
//!    returns the length it *would* have written, which may exceed the region; that value is what
//!    the rejection test needs. A negative return (an encoding error) has no `usize`, so the shim
//!    passes `(size_t)-1`, which arrives here as [`usize::MAX`]; that reproduces C's
//!    `(unsigned)len >= state->size` comparison on a negative `int`, which is how C folds the same
//!    failure into "did not fit".
//! 4. **NUL termination is the formatter's business, and must be left intact.** This module never
//!    writes a terminator and never counts one: the accepted byte count excludes it, so the NUL
//!    sits just past the newly buffered input where the next call overwrites it harmlessly.
//! 5. **The null checks on `file` and `format` are the shim's** (`gzwrite.c` L418-L419), and it
//!    performs both before calling in. A `&mut GzState` cannot be null, so C's
//!    `if (file == NULL) return Z_STREAM_ERROR` has no analogue below; a structure that is not one
//!    of this library's is still rejected here, by the mode check and by
//!    `GzState::resync_from_exposed`.
//! 6. **Never substitute an unbounded formatter, and never reach for nightly.** The shim's
//!    `vsnprintf` is the only formatting call in the port. There is no `ZLIB_INSECURE` equivalent,
//!    no `VaList`, no hand-written format parser, and no guessed `va_list` layout; the shim's own
//!    header records each of those as a closed door.
//!
//! [`printf_with`] wraps the pair for callers that can express formatting as a closure, and
//! [`printf_bytes`] takes an already-formatted slice. Both go through exactly the same guards and
//! accounting, so nothing is verified twice or skipped once -- which is what makes this module's
//! behaviour testable in safe Rust, without any FFI, and under Miri.
//!
//! ## Who compiles the shim
//!
//! Not cargo. `crates/libz-rs-sys/build.rs` is required to carry no `[build-dependencies]` and to
//! never invoke a C compiler, so that `cargo build --release` for the shipped artifacts works on a
//! machine without one (AAP §0.7.1 (i), and §0.6.4.1's rule that a C toolchain is the differential
//! crate's business alone). The **packaging** layer compiles it, which is the same layer that
//! already owns the `zlib.map` relink -- and it has to, because `--version-script` cannot be handed
//! to rustc's own cdylib link at all, for the measured reasons `build.rs` documents above
//! `reject_version_script_passthrough` (which is why that environment override is refused outright
//! rather than honoured). `Makefile.in`'s `rust` target compiles the shim with
//! `$(CC) $(SFLAGS) $(RUSTSHIMFLAGS)`, adds the object to the staged `libz.a`, and relinks the
//! staged shared library from that archive with `$(RUSTLDSHARED)`. It does **not** depend on
//! `configure` having loaded `LDSHARED` with the soname and version-script arguments: it inspects
//! the link command and supplies whichever of `$(RUSTSHAREDFLAG)`, `$(RUSTSONAMEFLAG)` and
//! `$(RUSTVERSIONSCRIPTFLAG)$(RUSTVERSIONSCRIPT)` is absent, so the relink is correct on a
//! configured tree, on an unconfigured one, and on a target whose linker spells those flags
//! differently. The CMake Rust path and the Rust CI workflow owe the same two steps.
//!
//! ## Symbol visibility -- verified, not assumed
//!
//! Three facts, all read straight out of `zlib.map` and all confirmed by linking the shim against
//! it and reading `nm -D --defined-only --extern-only`:
//!
//! * **`gzvprintf` is a versioned export**, listed in the `ZLIB_1.2.7.1` node alongside
//!   `inflateGetDictionary`. `gzprintf` predates the first node and is not named, so it takes the
//!   base binding. The relinked library shows exactly `gzvprintf@@ZLIB_1.2.7.1` and a bare
//!   `gzprintf`, which is symbol for symbol what the C library exports.
//! * **`zlib.map` has no `local: *;` catch-all.** Its `ZLIB_1.2.0` node lists nine names plus the
//!   pattern `_*`, and a symbol matched by nothing at all is assigned to the base version and stays
//!   *exported*. That is why the two helpers above **must** be named `_zlib_rs_gzprintf_begin` and
//!   `_zlib_rs_gzprintf_commit`: the leading underscore is what `_*` hides, and without it they
//!   would appear in the dynamic table and break the 111-symbol parity target. This is the same
//!   mechanism zlib itself uses for `_tr_init`, `_tr_tally` and the rest of the `_tr_*` family, so
//!   the naming follows the reference implementation's own convention. Do not rename them, and do
//!   not add them to `zlib.map` -- that file is immutable (AAP §0.8.1 directive 2).
//!
//! `make rust-symbols` is what proves both symbols actually reached the staged library.
//! * **`gz_error`, which this module calls, is in the `local:` block of the `ZLIB_1.2.0` node** and
//!   must stay hidden. It is `pub(crate)` in `gz/mod.rs` and is never re-exported from here, so the
//!   version script and the Rust visibility agree by construction rather than by convention.
//!
//! # The return convention
//!
//! `zlib.h` L1553-L1560 documents three outcomes: the count of uncompressed bytes written, a
//! negative zlib code, or -- when the result exceeds 8191 bytes, or one less than the size given to
//! `gzbuffer` -- an "error (0) with nothing written". Two of the three are non-negative, so they are
//! split by type rather than by sign:
//!
//! | This module | C | Meaning |
//! |---|---|---|
//! | `Ok(len)`, `len > 0` | `return len` | `len` uncompressed bytes were buffered |
//! | `Ok(0)` | `return 0` | the result did not fit; **nothing** was written or counted |
//! | `Err(code)` | `return state->err` or `Z_STREAM_ERROR` | the negative code C hands back |
//!
//! `_zlib_rs_gzprintf_commit` collapses them into C's single `int` with `Ok(len) => len` and
//! `Err(code) => code.as_i32()`, and the shim returns that value unchanged.
//!
//! # The double-sized input buffer, and why `gz_vacate` is called twice
//!
//! `gz_init` allocates `in` with `want << 1` and C's comment at `gzwrite.c` L15 says why in four
//! words: "double size for `gzprintf`". This function is the buffer's *only* reason for being
//! twice as large as `size`, and `gz_vacate` (`gzwrite.c` L382-L396) is what keeps the second half
//! free. Hence one call before formatting, to open the room up, and one after, to hand the result
//! to the compressor once more than half the buffer is occupied.
//!
//! The geometry that follows is exact. After a `gz_vacate` that reports the second half free, the
//! buffered input occupies `in[next_in .. next_in + avail_in]` with `next_in + avail_in <= size`,
//! and `in` is `2 * size` bytes long, so the `size` bytes starting immediately after it always fit.
//! That is the invariant C relies on and does not check.
//!
//! # A divergence forced by memory safety
//!
//! C's stalled-write path does not hold that invariant, and reaches an overflow. When the first
//! `gz_vacate` reports the second half *still* occupied and `state->again` is set, C records
//! `Z_BUF_ERROR` and deliberately does not return (L439-L449) -- so the application learns the
//! call is retryable rather than broken. But `gz_vacate` returns true only when
//! `avail_in > state->size` after moving the input back to the start of the buffer, so
//! `next = in + avail_in` is already past the halfway mark: the sentinel plant at
//! `next[state->size - 1]` lands up to `size` bytes past the end of a `2 * size` allocation, and
//! the formatter then writes there.
//!
//! This implementation keeps C's control flow and C's recorded error verbatim -- including the
//! deliberate fall-through -- and replaces the unchecked pointer with a checked slice split. When the
//! split fails, [`printf_begin`] returns `Err(Z_BUF_ERROR)` and nothing is formatted, which is what
//! `zlib.h` L1571-L1572 promises for a stall on a non-blocking write destination.
//!
//! # Fallback variants: deliberately not implemented
//!
//! C carries four preprocessor variants of the formatting step -- `NO_vsnprintf` crossed with
//! `HAS_vsprintf_void`, in both the `stdarg` (L455-L469) and non-`stdarg` (L563-L582) branches --
//! plus `ZLIB_INSECURE`, which re-enables the unbounded `vsprintf`/`sprintf` that the guard at
//! L374-L376 otherwise compiles `gzprintf` away to avoid. **Only the bounded path exists here**,
//! because Rust always has a bounded formatter and an unbounded variant would reintroduce exactly
//! the overflow class this implementation exists to remove. There is no `ZLIB_INSECURE` equivalent
//! and no build switch that could produce one.
//!
//! Two consequences follow, and both are this module's determination to make:
//!
//! * **`zlibCompileFlags` bits 25, 26 and 27 are all CLEAR.** `zutil.c` L88-L102 sets bit 25 for
//!   `NO_vsnprintf` together with `ZLIB_INSECURE`, bit 27 for `NO_vsnprintf` without it, and bit 26
//!   for the void-returning `HAS_vsprintf_void`/`HAS_vsnprintf_void` variants. This port has a
//!   bounded formatter and no void-returning variant, so none of the three conditions holds.
//!   `crates/libz-rs-sys/src/util.rs` **computes** that answer rather than copying a measured
//!   constant, and cites this module as the authority for it. Bit 24, which
//!   `zutil.c` L104 sets only in the non-`stdarg` branch, is likewise clear: the variadic entry
//!   point the facade exports is the `stdarg` one.
//! * **C's `#warning` stubs are unreachable here.** When no bounded formatter exists, C compiles
//!   `gzvprintf` and `gzprintf` down to bodies that ignore their arguments and return
//!   `Z_STREAM_ERROR` (L406-L412 and L505-L515), and `zlib.h` L1566-L1569 documents that a library
//!   built that way reports it through `zlibCompileFlags`. No such build exists for this port, so
//!   [`printf_begin`] never has a "formatting is unavailable" outcome and none is defined. The shim
//!   makes that a build-time guarantee rather than a hope: a compiler that provably does not declare
//!   `vsnprintf` -- a strict pre-C99 mode -- is refused with an `#error` naming the fix, instead of
//!   silently degrading to the unbounded `vsprintf` that `ZLIB_INSECURE` selects or to a stub that
//!   would fail `test/example.c`'s `gzprintf(file, ", %s!", "hello") == 8` assertion.
//!
//! # Feature gating and dependencies
//!
//! The crate root reaches this file through `#[cfg(feature = "std")] mod gz;`, so no per-item gate
//! appears below and `--no-default-features` compiles the whole subtree out. Nothing here needs
//! `std`, `alloc`, or any third-party crate.

// Every public item below is named for the C function it serves, inside a module named after that
// same function. The C names are what a maintainer diffing this file against `gzwrite.c` will
// search for, so they are kept; `gz/write.rs` sets the same allowance for the same reason.
#![allow(clippy::module_name_repetitions)]

use core::ffi::{c_int, c_uint};

use crate::allocate::Allocator;
use crate::error::ReturnCode;
use crate::gz::gz_error;
use crate::gz::state::{GzState, ZOff64, GZ_WRITE};
use crate::gz::write::{gz_init, gz_vacate, gz_zero};

/// `gz_error(state, Z_BUF_ERROR, "stalled write on gzprintf")` (`gzwrite.c` L445).
///
/// Reproduced verbatim, because it is caller-visible: `gzerror` returns it prefixed with the path,
/// and it is how an application distinguishes "retry this `gzprintf`" from a hard failure.
const STALLED_WRITE: &[u8] = b"stalled write on gzprintf";

/// Recorded when the scratch region cannot be addressed at all.
///
/// C has no counterpart, because C does not check: it forms `next` by pointer arithmetic and writes
/// through it. The stalled-write case that this converts into a clean `Z_BUF_ERROR` is described in
/// this module's documentation; every other way of reaching it means the state was not produced by
/// this library.
const NO_SCRATCH_ROOM: &[u8] = b"no room to format into on gzprintf";

/// Widens a C `unsigned` count into a `usize` index.
///
/// Every target this implementation supports has `usize` at least as wide as `c_uint`, so the conversion is
/// exact. Saturating rather than panicking on a hypothetical narrower target is the conservative
/// choice: a saturated value can only make a bounds check stricter, never looser. `gz/write.rs`
/// keeps a private twin of this for the same reason.
fn to_index(value: c_uint) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// Narrows a `usize` count into a C `unsigned`.
///
/// The counterpart of [`to_index`]. C performs the same narrowing with a bare cast --
/// `strm->avail_in += (unsigned)len` (`gzwrite.c` L476) -- guarded by the preceding
/// `(unsigned)len >= state->size` test. Here the value has already been proved smaller than
/// `state->size`, which is itself a `c_uint`, so the saturation is unreachable; it exists so the
/// conversion cannot panic on any input.
fn to_count(value: usize) -> c_uint {
    c_uint::try_from(value).unwrap_or(c_uint::MAX)
}

/// Converts a byte count into the signed file-offset type `x.pos` uses.
///
/// C writes `state->x.pos += len` with `len` an `int` and `pos` a `z_off64_t` (`gzwrite.c` L477),
/// relying on the implicit widening. Saturation stands in for that widening and is unreachable for
/// the same reason [`to_count`]'s is.
fn to_offset(value: usize) -> ZOff64 {
    ZOff64::try_from(value).unwrap_or(ZOff64::MAX)
}

/// The code C would return as `state->err`, with a fallback for a state that latched none.
///
/// Every early exit from `gzvprintf` after the entry guards is spelled `return state->err`
/// (`gzwrite.c` L425, L430 and L448), not `return` of whatever the failing helper reported. The
/// distinction is observable: `gz_error` promotes a failure to allocate the message text into
/// `Z_MEM_ERROR` (`gzlib.c` L577-L580), so the latched code can differ from the one the helper
/// handed back. Reading the state is therefore the faithful choice.
///
/// `fallback` covers the case C cannot express -- a helper reporting failure without latching a
/// code -- and is never reached from any call site below, because each one has already established
/// that a non-`Z_OK` code is latched.
fn latched_or<'a, A: Allocator<'a>>(state: &GzState<'a, A>, fallback: ReturnCode) -> ReturnCode {
    match state.err_code() {
        Some(code) if code != ReturnCode::OK => code,
        _ => fallback,
    }
}

/// An exclusive loan of the region a formatter may write into: the safe form of C's `char *next`.
///
/// C computes `next = (char *)(state->in + (strm->next_in - state->in) + strm->avail_in)`
/// (`gzwrite.c` L452) and then trusts `vsnprintf` to respect the separate `state->size` bound. The
/// two halves of that arrangement -- where the region starts and how long it is -- are carried
/// together here, as one slice, so they cannot disagree and cannot be passed on separately.
///
/// The value borrows the [`GzState`] it came from, so nothing else can touch the stream while the
/// loan is outstanding; that is what makes "no code between `printf_begin` and `printf_commit` may
/// disturb the region" a compiler-checked property rather than a comment.
///
/// # What the region contains on arrival
///
/// * Exactly [`len`](Self::len) bytes, which equals `state->size`.
/// * A zero in the **last** byte: the overflow sentinel C plants at L453. Nothing else about the
///   contents is specified -- the buffer comes from an allocator that does not zero, which
///   `test/infcover.c` L84-L87 checks for by filling every block with `0xa5`.
///
/// See this module's documentation for the obligations that go with holding one.
#[derive(Debug)]
pub struct PrintfScratch<'a> {
    /// The `state->size` bytes immediately after the currently buffered input.
    region: &'a mut [u8],
}

impl<'a> PrintfScratch<'a> {
    /// The width of the region, in bytes: C's `state->size`.
    ///
    /// This is the value to pass as `vsnprintf`'s `size` argument, and the bound the returned
    /// length is compared against by [`printf_commit`]. It is never zero.
    #[must_use]
    pub fn len(&self) -> usize {
        self.region.len()
    }

    /// Whether the region is empty, which it never is.
    ///
    /// [`printf_begin`] rejects a zero-width region before creating the loan, so this always
    /// reports `false`. It exists because a type that reports a length should also answer the
    /// question, and because it lets a defensive caller assert the invariant cheaply.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.region.is_empty()
    }

    /// The region, for reading -- chiefly to inspect the sentinel.
    #[must_use]
    pub fn as_slice(&self) -> &[u8] {
        self.region
    }

    /// The region, for formatting into.
    ///
    /// The formatter may write anywhere in the returned slice; it may not write outside it, and it
    /// must not clear the final byte except as a consequence of formatting far enough to reach it.
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        self.region
    }

    /// Consumes the loan and yields the region with the borrow's full lifetime.
    ///
    /// The form `_zlib_rs_gzprintf_begin` needs: it reports `region.as_mut_ptr()` and
    /// `region.len()` to `csrc/gzprintf_shim.c` through its two out-parameters, inside its own
    /// `unsafe` block, and the shim then calls `vsnprintf` against exactly that pointer and length.
    /// Consuming the loan ends this module's involvement and releases the borrow of the stream, so
    /// the caller becomes responsible for calling [`printf_commit`] afterwards -- which the shim
    /// does, unconditionally, as its very next statement.
    #[must_use]
    pub fn into_mut_slice(self) -> &'a mut [u8] {
        self.region
    }
}

/// Prepares the stream for formatting and lends out the scratch region.
///
/// The first half of `gzvprintf` (`gzwrite.c` L416-L453), reproduced step for step in C's order --
/// the order matters, because each step depends on the previous one having run.
///
/// | Step | C | `gzwrite.c` |
/// |---|---|---|
/// | 1 | recover the flush cursor from the exposed prefix | -- (this implementation's contract) |
/// | 2 | `if (state->mode != GZ_WRITE \|\| (state->err != Z_OK && !state->again)) return Z_STREAM_ERROR` | L421-L422 |
/// | 3 | `gz_error(state, Z_OK, NULL)` | L423 |
/// | 4 | `if (state->size == 0 && gz_init(state) == -1) return state->err` | L426-L427 |
/// | 5 | `if (state->skip && gz_zero(state) == -1) return state->err` | L430-L431 |
/// | 6 | `ret = gz_vacate(state)` and the stall handling | L438-L449 |
/// | 7 | `if (strm->avail_in == 0) strm->next_in = state->in` | L450-L451 |
/// | 8 | `next = in + (next_in - in) + avail_in` | L452 |
/// | 9 | `next[state->size - 1] = 0` | L453 |
///
/// ★ Step 2's error half is **deliberately narrower than the read side's**. `gzread` and its
/// siblings tolerate a lingering `Z_BUF_ERROR` (`gzread.c` L407); the write side does not, because a
/// `Z_BUF_ERROR` on a write stream means a `gzprintf` stalled and must be retried, so continuing
/// past it would interleave the retried text with whatever came next. `gz/write.rs`'s `enter_write`
/// documents the same asymmetry for the same reason, and steps 1 to 3 here are exactly its body:
/// they are repeated rather than shared because that helper is private to its own module.
///
/// ★ Step 6's control flow is subtle and is reproduced exactly: a stalled write with `state->again`
/// set records `Z_BUF_ERROR` and **does not return**. The application is meant to see a retryable
/// condition, not a hard failure. This module's documentation explains why the fall-through then
/// reaches a checked slice split rather than C's overflow.
///
/// C's `if (file == NULL) return Z_STREAM_ERROR` (L418-L419) has no counterpart: a `&mut GzState`
/// cannot be null. The shim performs that check, together with the `format == NULL` test C omits,
/// and a structure that is not one of this library's is still rejected -- by step 1, which validates the exposed prefix against the output
/// buffer, and by step 2's mode value, which `gzguts.h` L158 introduces as "a little integrity check
/// on the passed structure".
///
/// # Errors
///
/// Each variant is the negative code C returns at the corresponding line:
///
/// * [`ReturnCode::STREAM_ERROR`] -- the exposed prefix is inconsistent with the output buffer, the
///   file is not open for writing, or a serious error is already latched (L421-L422).
/// * whatever `gz_init` latched, normally [`ReturnCode::MEM_ERROR`] -- the working buffers or the
///   compressor could not be created (L426-L427).
/// * whatever `gz_zero` latched, one of [`ReturnCode::MEM_ERROR`], [`ReturnCode::ERRNO`] or
///   [`ReturnCode::STREAM_ERROR`] -- a pending forward seek could not be paid for (L430-L431).
/// * whatever the first `gz_vacate` latched, when the stream is not merely stalled (L447-L448).
/// * [`ReturnCode::BUF_ERROR`] -- the `state->size` bytes of scratch are not addressable. On a
///   stalled non-blocking destination this is the outcome `zlib.h` L1571-L1572 documents; any other
///   route to it means the state was not produced by this library.
///
/// On every error the exposed prefix is refreshed first, unless the failure was step 1 or step 2 --
/// which change nothing, and after which refreshing would publish a pointer derived from an index
/// that was never validated.
pub fn printf_begin<'s, 'a, A: Allocator<'a> + Copy>(
    state: &'s mut GzState<'a, A>,
) -> Result<PrintfScratch<'s>, ReturnCode> {
    // Step 1. The caller's `gzgetc` macro can advance the exposed cursor without telling the
    // library, so the index is stale on arrival. On a write stream `x.have` is always zero and the
    // macro cannot have moved anything, but the contract is unconditional and a state that fails
    // the check is one this library did not produce.
    if state.resync_from_exposed().is_err() {
        return Err(ReturnCode::STREAM_ERROR);
    }

    // Step 2 -- L421-L422.
    if state.mode() != GZ_WRITE {
        return Err(ReturnCode::STREAM_ERROR);
    }
    if state.err() != ReturnCode::OK.as_i32() && !state.again() {
        return Err(ReturnCode::STREAM_ERROR);
    }

    // Step 3 -- L423. Clears the code, releases any previous message, and leaves `x.have` alone.
    gz_error(state, ReturnCode::OK, None);

    // Step 4 -- L426-L427.
    if state.size() == 0 {
        if let Err(reported) = gz_init(state) {
            state.refresh_exposed();
            return Err(latched_or(state, reported));
        }
    }

    // Step 5 -- L430-L431. A forward `gzseek` on a write stream records the distance rather than
    // performing it (`gzlib.c` L432), and this is where it is paid for.
    if state.skip() != 0 {
        if let Err(reported) = gz_zero(state) {
            state.refresh_exposed();
            return Err(latched_or(state, reported));
        }
    }

    // Step 6 -- L438-L449. `gz_vacate` reports occupancy, not status: `true` means the second half
    // of the input buffer is still partly occupied. Its own `gz_comp` result is discarded, exactly
    // as C's `(void)gz_comp(...)` discards it, so the outcome is read off the state instead.
    let occupied = gz_vacate(state);
    if state.err() != ReturnCode::OK.as_i32() {
        if occupied && state.again() {
            // L440-L445: tell the application to retry this `gzprintf`.
            gz_error(state, ReturnCode::BUF_ERROR, Some(STALLED_WRITE));
        }
        // L447-L448. Note what is absent: a stalled stream falls through rather than returning.
        if !state.again() {
            state.refresh_exposed();
            return Err(latched_or(state, ReturnCode::STREAM_ERROR));
        }
    }

    // Step 7 -- L450-L451. With nothing buffered, formatting starts at the front of `in`.
    if state.strm.avail_in == 0 {
        state.strm.next_in = 0;
    }

    // Step 8 -- L452. C's `(strm->next_in - state->in)` is already an index in this implementation, so the
    // whole expression is `next_in + avail_in`.
    let size = to_index(state.size());
    let start = state
        .strm
        .next_in
        .saturating_add(to_index(state.strm.avail_in));

    // The bounds check C leaves to the `2 * size` allocation. Computed through an immutable view so
    // that the failure branch can still record the error, and expressed as a single predicate so
    // that a zero-width region -- which would make the sentinel index underflow -- is rejected here
    // rather than later.
    let in_len = state.in_slice().len();
    let addressable = size != 0 && start.checked_add(size).is_some_and(|end| end <= in_len);
    if !addressable {
        // The stalled-write path has already recorded `Z_BUF_ERROR` with C's own wording; anything
        // else arriving here is a state this library did not produce, and gets its own message.
        if state.err() != ReturnCode::BUF_ERROR.as_i32() {
            gz_error(state, ReturnCode::BUF_ERROR, Some(NO_SCRATCH_ROOM));
        }
        state.refresh_exposed();
        return Err(ReturnCode::BUF_ERROR);
    }
    let end = start.saturating_add(size);

    // Publish the flush cursor before the loan begins. `gz_init` and `gz_zero` may both have moved
    // it, and once the region is borrowed the state cannot be reached again from here -- so a caller
    // that abandons the loan without committing still leaves a coherent structure behind.
    state.refresh_exposed();

    // The loan itself. `get_mut` cannot fail -- `addressable` proved the range lies inside the
    // buffer -- but it is used rather than indexing so that no path here can panic.
    let Some(region) = state.in_slice_mut().get_mut(start..end) else {
        return Err(ReturnCode::BUF_ERROR);
    };

    // Step 9 -- L453: `next[state->size - 1] = 0`. The region is exactly `size` bytes long, so its
    // last element *is* index `size - 1`; asking for the last element rather than computing the
    // index keeps the two definitions from drifting apart.
    let Some(sentinel) = region.last_mut() else {
        return Err(ReturnCode::BUF_ERROR);
    };
    *sentinel = 0;

    Ok(PrintfScratch { region })
}

/// Accounts for a formatted result and hands back the value the caller should report.
///
/// The second half of `gzvprintf` (`gzwrite.c` L471-L483). `written` is the length the formatter
/// *returned*, which for `vsnprintf` is the length it *would* have written and may therefore exceed
/// the region; a negative C return arrives here as [`usize::MAX`], which the range test rejects for
/// the same reason C's `(unsigned)len >= state->size` does.
///
/// The rejection test itself is one predicate, and its three parts are the subject of the next
/// section (`gzwrite.c` L471):
///
/// ```c
/// if (len == 0 || (unsigned)len >= state->size || next[state->size - 1] != 0)
/// ```
///
/// # The three-part rejection test, and why all three parts are needed
///
/// Each condition catches a different shape of failure, and none of them subsumes another:
///
/// | Condition | What it catches |
/// |---|---|
/// | `written == 0` | the formatter failed, or produced nothing; either way there is nothing to buffer |
/// | `written >= size` | the formatter *reported* that the result did not fit, having truncated it |
/// | the sentinel is no longer zero | the formatter wrote `size` characters or more, ignoring its bound |
///
/// The second is what catches truncation, and the third is emphatically **not**: a conforming
/// bounded formatter puts its terminating NUL at index `size - 1` when it truncates, leaving the
/// sentinel zero. What the third catches is a formatter that never respected the bound at all —
/// the reference's `NO_vsnprintf` configuration calls the unbounded `vsprintf` (`gzwrite.c`
/// L455-L458) — and it is what makes the *reported* length untrusted rather than trusted, since a
/// formatter that overran would otherwise have its length believed. Together they are C's complete
/// overflow defence, and all three yield `Ok(0)` -- **not** an error code, and with nothing appended
/// to the buffered input and nothing added to the position. `zlib.h` L1557-L1560 documents that
/// outcome: exceeding the limit "will return an error (0) with nothing written".
///
/// # What is counted, and what is not
///
/// `avail_in` and `x.pos` both advance by exactly `written` bytes, which excludes the formatter's
/// NUL terminator. The terminator therefore sits immediately past the newly buffered input, outside
/// the `avail_in` window, where the next call's sentinel plant or the next formatted result
/// overwrites it. `x.pos` counts the *uncompressed* stream, which is why it moves here even though
/// no compression has happened yet.
///
/// The closing `gz_vacate` is C's "write out buffer if more than half is occupied" (L479-L480). C
/// assigns its report to `ret` and never reads it again; the report is likewise discarded here,
/// because the outcome that matters is the code the call may have latched on the state.
///
/// # Errors
///
/// Whatever the closing `gz_vacate` latched -- normally [`ReturnCode::ERRNO`] for a failed write --
/// when the stream is not merely stalled (L481-L482). A stalled stream reports the byte count
/// instead, so the caller learns how much was accepted and can retry the rest.
///
/// Calling this without a preceding successful [`printf_begin`] is a caller defect. It cannot
/// corrupt anything -- the sentinel is read through a checked lookup, and a region that is not
/// addressable yields [`ReturnCode::BUF_ERROR`] -- but the accounting it performs would be
/// meaningless.
pub fn printf_commit<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
    written: usize,
) -> Result<c_int, ReturnCode> {
    let size = to_index(state.size());
    // Where `printf_begin` placed the region. Recomputed rather than remembered, because C
    // recomputes it too: `next` is still in scope there, derived from the same two fields, and
    // neither has been touched since.
    let start = state
        .strm
        .next_in
        .saturating_add(to_index(state.strm.avail_in));

    // L471, first two conditions, short-circuited in C's order so that the sentinel is only
    // consulted for a length that could plausibly have fitted.
    if written == 0 || written >= size {
        state.refresh_exposed();
        return Ok(0);
    }

    // L471, third condition: `next[state->size - 1] != 0`. Read into a local so the immutable
    // borrow ends before the failure branch needs the state mutably.
    let sentinel_index = start.saturating_add(size.saturating_sub(1));
    let sentinel = state.in_slice().get(sentinel_index).copied();
    let Some(sentinel) = sentinel else {
        // Unreachable after a successful `printf_begin`, which proved the whole region addressable.
        gz_error(state, ReturnCode::BUF_ERROR, Some(NO_SCRATCH_ROOM));
        state.refresh_exposed();
        return Err(ReturnCode::BUF_ERROR);
    };
    if sentinel != 0 {
        state.refresh_exposed();
        return Ok(0);
    }

    // L476-L477. `written < size`, and a successful `gz_vacate` left `next_in + avail_in <= size`,
    // so the sum is below `2 * size` -- which `gzbuffer` guarantees fits in a `c_uint`
    // (`gzlib.c` L337-L338).
    state.strm.avail_in = state.strm.avail_in.saturating_add(to_count(written));
    state.add_pos(to_offset(written));

    // L479-L480. The occupancy report is C's unused `ret`.
    let _occupied = gz_vacate(state);

    // The exit half of the exposed-prefix contract: `x.pos` has just moved, and `gz_vacate` may
    // have moved the flush cursor as well.
    state.refresh_exposed();

    if state.err() != ReturnCode::OK.as_i32() && !state.again() {
        return Err(latched_or(state, ReturnCode::STREAM_ERROR));
    }

    // L483. `written < size`, and `size` is a `c_uint`; the saturation is therefore unreachable for
    // any buffer size a `c_int` could describe, and exists only so this cannot panic.
    Ok(c_int::try_from(written).unwrap_or(c_int::MAX))
}

/// Runs a formatter against the scratch region, doing the whole of `gzvprintf` around it.
///
/// [`printf_begin`] followed by [`printf_commit`], with the loan released in between, so that a
/// caller able to express formatting as a closure cannot get the sequence wrong: the borrow ends
/// before the accounting runs, and the accounting cannot be skipped.
///
/// `format` receives the region -- exactly `state->size` bytes, with a zero already in the last one
/// -- and returns what its formatter reported:
///
/// * `Some(n)`: the length the formatter *would* have written, `vsnprintf`-style. It may exceed the
///   region; [`printf_commit`] is what decides whether it fitted.
/// * `None`: the formatter failed. C's `vsnprintf` signals this with a negative `int`, and the cast
///   in `(unsigned)len >= state->size` turns that into an enormous value, so the result is rejected.
///   [`usize::MAX`] reproduces that comparison, which is why `None` yields `Ok(0)` rather than an
///   error.
///
/// The C variadic boundary cannot use this -- `vsnprintf` needs a raw pointer, and the closure
/// would have to cross the FFI edge -- so `_zlib_rs_gzprintf_begin`/`_zlib_rs_gzprintf_commit` drive
/// the [`printf_begin`] / [`printf_commit`] pair directly instead. This exists for Rust-side callers
/// and for the tests below, which is what lets the guards, the sentinel and the rejection test be
/// exercised without any FFI and under Miri.
///
/// # Errors
///
/// Exactly what [`printf_begin`] and [`printf_commit`] report, unchanged.
pub fn printf_with<'a, A, F>(state: &mut GzState<'a, A>, format: F) -> Result<c_int, ReturnCode>
where
    A: Allocator<'a> + Copy,
    F: FnOnce(&mut [u8]) -> Option<usize>,
{
    // The inner scope is what ends the loan's borrow of `state` before it is needed again.
    let reported = {
        let mut scratch = printf_begin(state)?;
        format(scratch.as_mut_slice())
    };
    printf_commit(state, reported.unwrap_or(usize::MAX))
}

/// Formats an already-rendered byte string into the stream.
///
/// The path for callers that have the text in hand: the crate's own tests, and any Rust-side caller
/// that has formatted with `core::fmt` and simply needs the result buffered. It goes through
/// [`printf_with`], so the guards, the sentinel and the rejection test are the same ones the C shim
/// meets -- which is what makes this module testable without any FFI.
///
/// `formatted` is treated exactly as `vsnprintf` treats a rendered result:
///
/// * at most `state->size - 1` of its bytes are copied, the rest truncated away;
/// * a NUL is written immediately after the copied bytes, which lands on the sentinel precisely
///   when the result is long enough to fill the region;
/// * the length *reported* is `formatted.len()`, not the number of bytes copied.
///
/// So a result shorter than `state->size` is accepted whole and a result of `state->size` bytes or
/// more is rejected with nothing written, which is the documented 8191-byte cap at the default
/// buffer size (`zlib.h` L1557-L1560).
///
/// # Errors
///
/// Exactly what [`printf_with`] reports, unchanged.
pub fn printf_bytes<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
    formatted: &[u8],
) -> Result<c_int, ReturnCode> {
    printf_with(state, |region: &mut [u8]| -> Option<usize> {
        // `vsnprintf` writes at most `size` bytes *including* the terminator, so at most `size - 1`
        // characters. The region is never empty, so the subtraction always succeeds.
        let capacity = region.len().checked_sub(1)?;
        let take = formatted.len().min(capacity);
        let head = formatted.get(..take)?;
        region.get_mut(..take)?.copy_from_slice(head);
        // The terminator `vsnprintf` always appends. At `take == capacity` this is the sentinel
        // byte, and setting it to zero is exactly what a bounded formatter does when it truncates.
        *region.get_mut(take)? = 0;
        // The length it would have written, which is the whole point of the return value.
        Some(formatted.len())
    })
}

#[cfg(test)]
mod tests {
    // The crate denies the panic-prone lints in library code, which is the right policy there and
    // the wrong one here: a test asserts, and an assertion that fails panics. clippy.toml already
    // relaxes them inside tests; stating it again keeps the intent local and visible.
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

    use super::{printf_begin, printf_bytes, printf_commit, printf_with, to_index};
    use crate::allocate::GlobalAllocator;
    use crate::error::ReturnCode;
    use crate::gz::state::{
        GzFileSlot, GzHandle, GzIoError, GzSeekFrom, GzState, ZOff64, GZBUFSIZE, GZ_NONE, GZ_READ,
        GZ_WRITE,
    };
    use crate::gz::write::{gz_init, gzclose_w, gzputc, gzputs};
    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::RefCell;
    use core::ffi::c_uint;

    /// Every test below drives a **transparent** stream, `state->direct == 1`.
    ///
    /// `gz_init` then allocates only the doubled input buffer and creates no compressor
    /// (`gzwrite.c` L23-L25), and `gz_comp` writes the buffered bytes straight to the file
    /// (`gzwrite.c` L75-L91). That is deliberate, and it makes these tests sharper rather than
    /// weaker: everything this module does happens in the input buffer -- the guards, the geometry,
    /// the sentinel and the accounting -- and none of it involves compression, so removing the
    /// compressor from the picture turns every assertion into an exact-bytes assertion and keeps the
    /// suite runnable under Miri. `gz/write.rs` owns the tests that exercise the compressing path.
    const TRANSPARENT: i32 = 1;

    /// A file that keeps everything written to it in a shared buffer.
    struct MemoryFile {
        /// Everything written so far, shared with the test so it can be read back after the state
        /// has taken the handle away for closing.
        sink: Rc<RefCell<Vec<u8>>>,
    }

    impl GzHandle for MemoryFile {
        fn read(&mut self, _buf: &mut [u8]) -> Result<usize, GzIoError> {
            Ok(0)
        }

        fn write(&mut self, buf: &[u8]) -> Result<usize, GzIoError> {
            self.sink.borrow_mut().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn seek(&mut self, offset: ZOff64, _whence: GzSeekFrom) -> Result<ZOff64, GzIoError> {
            Ok(offset)
        }

        fn set_nonblocking(&mut self, _nonblocking: bool) -> Result<(), GzIoError> {
            Ok(())
        }

        fn close(&mut self) -> Result<(), GzIoError> {
            Ok(())
        }
    }

    /// A file that refuses every write with the non-blocking stall condition.
    ///
    /// `GzIoError::would_block` is this implementation's spelling of `errno == EAGAIN || errno == EWOULDBLOCK`
    /// (`gzwrite.c` L82 and L117), so this is what drives the `state->again` paths. 11 is Linux's
    /// `EAGAIN`; only the flag is consulted.
    struct StalledFile;

    /// A file that refuses every write with a hard failure rather than a stall.
    ///
    /// The `would_block` flag is clear, so `gz_error` records `Z_ERRNO` and leaves `state->again`
    /// alone -- which is the branch of `gzvprintf`'s stall handling that *does* return early
    /// (`gzwrite.c` L447-L448). 5 is Linux's `EIO`; only the flag is consulted.
    struct BrokenFile;

    impl GzHandle for BrokenFile {
        fn read(&mut self, _buf: &mut [u8]) -> Result<usize, GzIoError> {
            Ok(0)
        }

        fn write(&mut self, _buf: &[u8]) -> Result<usize, GzIoError> {
            Err(GzIoError::new(5, false))
        }

        fn seek(&mut self, offset: ZOff64, _whence: GzSeekFrom) -> Result<ZOff64, GzIoError> {
            Ok(offset)
        }

        fn set_nonblocking(&mut self, _nonblocking: bool) -> Result<(), GzIoError> {
            Ok(())
        }

        fn close(&mut self) -> Result<(), GzIoError> {
            Ok(())
        }
    }

    impl GzHandle for StalledFile {
        fn read(&mut self, _buf: &mut [u8]) -> Result<usize, GzIoError> {
            Ok(0)
        }

        fn write(&mut self, _buf: &[u8]) -> Result<usize, GzIoError> {
            Err(GzIoError::new(11, true))
        }

        fn seek(&mut self, offset: ZOff64, _whence: GzSeekFrom) -> Result<ZOff64, GzIoError> {
            Ok(offset)
        }

        fn set_nonblocking(&mut self, _nonblocking: bool) -> Result<(), GzIoError> {
            Ok(())
        }

        fn close(&mut self) -> Result<(), GzIoError> {
            Ok(())
        }
    }

    /// A transparent write state in exactly the condition `gz_open` leaves one in for `"wbT"`.
    ///
    /// `gz_open` sets the mode, the buffer size and the handle and then leaves `size` at zero so
    /// that `gz_init` runs lazily (`gzlib.c` L103-L112 and L248); assembling it here keeps these
    /// tests independent of `gz/open.rs`.
    fn writer(want: c_uint) -> (GzState<'static, GlobalAllocator>, Rc<RefCell<Vec<u8>>>) {
        let sink = Rc::new(RefCell::new(Vec::new()));
        let mut state = GzState::new(GlobalAllocator);
        state.set_mode(GZ_WRITE);
        state.set_want(want);
        state.set_direct(TRANSPARENT);
        let previous = state.set_handle(GzFileSlot::Boxed(Box::new(MemoryFile {
            sink: Rc::clone(&sink),
        })));
        assert!(!previous.is_installed(), "the slot was empty before");
        (state, sink)
    }

    /// The same, but with a destination that always reports a non-blocking stall.
    fn stalled_writer(want: c_uint) -> GzState<'static, GlobalAllocator> {
        let mut state = GzState::new(GlobalAllocator);
        state.set_mode(GZ_WRITE);
        state.set_want(want);
        state.set_direct(TRANSPARENT);
        let previous = state.set_handle(GzFileSlot::Boxed(Box::new(StalledFile)));
        assert!(!previous.is_installed(), "the slot was empty before");
        state
    }

    /// The bytes currently buffered in `in`, i.e. `in[next_in .. next_in + avail_in]`.
    fn buffered<'s>(state: &'s GzState<'static, GlobalAllocator>) -> &'s [u8] {
        let start = state.stream().next_in;
        let end = start + to_index(state.stream().avail_in);
        &state.in_slice()[start..end]
    }

    #[test]
    fn an_ordinary_result_reports_its_length_and_advances_the_position() {
        let (mut state, sink) = writer(GZBUFSIZE);
        assert_eq!(state.size(), 0, "the buffers are allocated lazily");

        assert_eq!(printf_bytes(&mut state, b"hello"), Ok(5));

        // Step 4 ran: `gz_init` allocated the doubled input buffer and set the marker.
        assert_eq!(state.size(), GZBUFSIZE);
        assert_eq!(state.in_slice().len(), 2 * to_index(GZBUFSIZE));
        // The result is buffered, counted once in `avail_in` and once in the uncompressed position.
        assert_eq!(buffered(&state), b"hello");
        assert_eq!(state.pos(), 5);
        // Well under half the buffer, so the closing `gz_vacate` had nothing to do.
        assert!(sink.borrow().is_empty());
        // A write stream never publishes bytes through the exposed prefix.
        assert_eq!(state.have(), 0);
    }

    #[test]
    fn consecutive_results_accumulate() {
        let (mut state, _sink) = writer(GZBUFSIZE);

        assert_eq!(printf_bytes(&mut state, b"one "), Ok(4));
        assert_eq!(printf_bytes(&mut state, b"two "), Ok(4));
        assert_eq!(printf_bytes(&mut state, b"three"), Ok(5));

        assert_eq!(buffered(&state), b"one two three");
        assert_eq!(state.pos(), 13);
    }

    #[test]
    fn the_terminator_is_written_but_never_counted() {
        let (mut state, _sink) = writer(64);

        assert_eq!(printf_bytes(&mut state, b"abc"), Ok(3));

        // Three characters counted, and the NUL `vsnprintf` appends sitting just past them --
        // outside the `avail_in` window, which is what makes it harmless.
        assert_eq!(state.stream().avail_in, 3);
        assert_eq!(state.in_slice()[3], 0);
    }

    #[test]
    fn a_result_of_exactly_size_minus_one_is_accepted() {
        let (mut state, _sink) = writer(16);
        let payload = vec![b'x'; 15];

        assert_eq!(printf_bytes(&mut state, &payload), Ok(15));

        assert_eq!(state.size(), 16);
        assert_eq!(buffered(&state), &payload[..]);
        assert_eq!(state.pos(), 15);
        // The terminator landed on the sentinel, which is why the sentinel test still passed.
        assert_eq!(state.in_slice()[15], 0);
    }

    #[test]
    fn a_result_of_exactly_size_is_rejected_with_nothing_written() {
        let (mut state, sink) = writer(16);
        let payload = vec![b'x'; 16];

        assert_eq!(printf_bytes(&mut state, &payload), Ok(0));

        assert_eq!(state.stream().avail_in, 0, "nothing was appended");
        assert_eq!(state.pos(), 0, "and nothing was counted");
        assert!(sink.borrow().is_empty());
    }

    #[test]
    fn a_result_one_byte_past_size_is_rejected() {
        let (mut state, _sink) = writer(16);
        let payload = vec![b'x'; 17];

        assert_eq!(printf_bytes(&mut state, &payload), Ok(0));

        assert_eq!(state.stream().avail_in, 0);
        assert_eq!(state.pos(), 0);
    }

    #[test]
    fn a_zero_length_result_is_rejected() {
        let (mut state, _sink) = writer(16);

        assert_eq!(printf_bytes(&mut state, b""), Ok(0));

        assert_eq!(state.stream().avail_in, 0);
        assert_eq!(state.pos(), 0);
    }

    #[test]
    fn a_formatter_that_clobbers_the_sentinel_is_rejected() {
        let (mut state, _sink) = writer(16);

        // A well-behaved short result, except that the last byte of the region has been overwritten
        // -- the signature of a formatter that ran past its bound without reporting it.
        let outcome = printf_with(&mut state, |region| {
            assert_eq!(region.len(), 16);
            region[..2].copy_from_slice(b"ok");
            region[15] = b'!';
            Some(2)
        });

        assert_eq!(outcome, Ok(0));
        assert_eq!(state.stream().avail_in, 0);
        assert_eq!(state.pos(), 0);
    }

    #[test]
    fn a_formatter_that_reports_failure_is_rejected() {
        let (mut state, _sink) = writer(16);

        // `None` stands in for C's negative `vsnprintf` return, which `(unsigned)len >= state->size`
        // folds into the same "did not fit" outcome.
        let outcome = printf_with(&mut state, |_region| None);

        assert_eq!(outcome, Ok(0));
        assert_eq!(state.stream().avail_in, 0);
        assert_eq!(state.pos(), 0);
    }

    #[test]
    fn an_overrunning_report_cannot_buffer_more_than_the_region() {
        let (mut state, _sink) = writer(16);

        // The formatter writes two bytes and then lies about how many it would have written. The
        // claim is at least `size`, so it is rejected: the report is validated, never trusted.
        let outcome = printf_with(&mut state, |region| {
            region[..2].copy_from_slice(b"ok");
            Some(usize::MAX)
        });

        assert_eq!(outcome, Ok(0));
        assert_eq!(state.stream().avail_in, 0);
    }

    #[test]
    fn the_documented_cap_is_one_less_than_the_buffer_size() {
        // `zlib.h` L1557-L1560: "limited to 8191, or one less than the buffer size given to
        // gzbuffer()", with the default `GZBUFSIZE` of 8192.
        let cap = to_index(GZBUFSIZE) - 1;
        assert_eq!(cap, 8191);

        let (mut fits, _fits_sink) = writer(GZBUFSIZE);
        assert_eq!(printf_bytes(&mut fits, &vec![b'a'; cap]), Ok(8191));
        assert_eq!(fits.pos(), 8191);

        let (mut over, over_sink) = writer(GZBUFSIZE);
        assert_eq!(printf_bytes(&mut over, &vec![b'a'; cap + 1]), Ok(0));
        assert_eq!(over.pos(), 0);
        assert!(over_sink.borrow().is_empty());
    }

    #[test]
    fn a_read_stream_is_rejected() {
        let (mut state, _sink) = writer(16);
        state.set_mode(GZ_READ);

        assert_eq!(
            printf_bytes(&mut state, b"nope"),
            Err(ReturnCode::STREAM_ERROR)
        );
        assert_eq!(state.size(), 0, "not even the buffers were allocated");
    }

    #[test]
    fn a_directionless_stream_is_rejected() {
        let (mut state, _sink) = writer(16);
        state.set_mode(GZ_NONE);

        assert_eq!(
            printf_bytes(&mut state, b"nope"),
            Err(ReturnCode::STREAM_ERROR)
        );
    }

    #[test]
    fn a_latched_error_is_rejected() {
        let (mut state, _sink) = writer(16);
        state.set_err(ReturnCode::DATA_ERROR.as_i32());

        assert_eq!(
            printf_bytes(&mut state, b"nope"),
            Err(ReturnCode::STREAM_ERROR)
        );
        assert_eq!(
            state.err(),
            ReturnCode::DATA_ERROR.as_i32(),
            "the guard rejects without disturbing what was latched"
        );
    }

    #[test]
    fn a_latched_buf_error_is_rejected_too() {
        // ★ The asymmetry with the read side: `gzread` tolerates a lingering `Z_BUF_ERROR`
        // (`gzread.c` L407), the write side does not (`gzwrite.c` L421-L422), because on a write
        // stream it means a `gzprintf` stalled and must be retried.
        let (mut state, _sink) = writer(16);
        state.set_err(ReturnCode::BUF_ERROR.as_i32());

        assert_eq!(
            printf_bytes(&mut state, b"nope"),
            Err(ReturnCode::STREAM_ERROR)
        );
    }

    #[test]
    fn a_stalled_stream_may_still_be_written_to() {
        // `state->again` is the escape hatch in the same guard: a stream waiting on a non-blocking
        // destination is not in error, and `gz_error(state, Z_OK, NULL)` at L423 then clears the
        // latched code so the closing test at L481-L482 sees a clean state.
        let (mut state, _sink) = writer(16);
        state.set_err(ReturnCode::BUF_ERROR.as_i32());
        state.set_again(true);

        assert_eq!(printf_bytes(&mut state, b"ab"), Ok(2));
        assert_eq!(buffered(&state), b"ab");
    }

    #[test]
    fn a_pending_skip_is_paid_for_before_formatting() {
        let (mut state, sink) = writer(16);
        // What `gzseek(file, 3, SEEK_CUR)` records on a write stream (`gzlib.c` L432).
        state.set_skip(3);

        assert_eq!(printf_bytes(&mut state, b"xy"), Ok(2));

        // `gz_zero` compressed three zero bytes through the transparent path first ...
        assert_eq!(&sink.borrow()[..], &[0, 0, 0]);
        assert_eq!(state.skip(), 0);
        // ... and the position counts the zeros as well as the formatted result.
        assert_eq!(state.pos(), 5);
        assert_eq!(buffered(&state), b"xy");
    }

    #[test]
    fn crossing_the_halfway_mark_flushes_and_rewinds_the_cursor() {
        let (mut state, sink) = writer(16);

        // First call: 10 of the 16 bytes, so the second half stays free and nothing is flushed.
        assert_eq!(printf_bytes(&mut state, b"0123456789"), Ok(10));
        assert!(sink.borrow().is_empty());
        assert_eq!(state.stream().next_in, 0);

        // Second call: the region starts at index 10, and the result takes the total past `size`,
        // so the closing `gz_vacate` writes the buffer out and puts the cursor back to the front.
        assert_eq!(printf_bytes(&mut state, b"abcdefghij"), Ok(10));
        assert_eq!(&sink.borrow()[..], b"0123456789abcdefghij");
        assert_eq!(state.stream().avail_in, 0);
        assert_eq!(state.stream().next_in, 0);
        assert_eq!(state.pos(), 20);

        // Third call: back at the front of the buffer, and still correct.
        assert_eq!(printf_bytes(&mut state, b"klm"), Ok(3));
        assert_eq!(buffered(&state), b"klm");
        assert_eq!(state.pos(), 23);
    }

    #[test]
    fn many_short_results_survive_repeated_flushing() {
        let (mut state, sink) = writer(16);
        let mut expected = Vec::new();

        for index in 0..64_u8 {
            let chunk = [b'a' + (index % 26), b'0' + (index % 10)];
            assert_eq!(printf_bytes(&mut state, &chunk), Ok(2));
            expected.extend_from_slice(&chunk);
        }

        assert_eq!(state.pos(), 128);
        // Whatever has not been flushed yet is still buffered, and the two together are the input.
        let mut produced = sink.borrow().clone();
        produced.extend_from_slice(buffered(&state));
        assert_eq!(produced, expected);
    }

    #[test]
    fn a_stalled_write_records_buf_error_and_reports_it() {
        // The path this module diverges from C on. `gz_vacate` reports the second half still
        // occupied, `state->again` is set, so C records `Z_BUF_ERROR` and falls through -- into an
        // overflow. Here the checked split turns that into the `Z_BUF_ERROR` return that
        // `zlib.h` L1571-L1572 documents.
        let mut state = stalled_writer(16);
        gz_init(&mut state).unwrap();
        assert_eq!(state.size(), 16);

        // More than `size` bytes buffered, which is the only way `gz_vacate` can report `true`.
        state.in_slice_mut()[..20].fill(b'z');
        state.stream_mut().next_in = 0;
        state.stream_mut().avail_in = 20;

        assert_eq!(
            printf_bytes(&mut state, b"retry me"),
            Err(ReturnCode::BUF_ERROR)
        );

        assert_eq!(state.err(), ReturnCode::BUF_ERROR.as_i32());
        assert!(state.again(), "the stall itself is still recorded");
        assert!(
            state.msg().unwrap().ends_with(b"stalled write on gzprintf"),
            "C's own wording reaches the application through gzerror"
        );
        // Nothing was formatted and nothing was counted.
        assert_eq!(state.stream().avail_in, 20);
        assert_eq!(state.pos(), 0);
    }

    #[test]
    fn a_hard_write_failure_is_propagated() {
        // The same shape, but without the stall flag: `gz_vacate`'s `gz_comp` fails, `state->again`
        // is clear, so C returns `state->err` at L447-L448.
        let (mut state, _sink) = writer(16);
        gz_init(&mut state).unwrap();
        let _previous = state.set_handle(GzFileSlot::Boxed(Box::new(BrokenFile)));

        state.in_slice_mut()[..20].fill(b'z');
        state.stream_mut().next_in = 0;
        state.stream_mut().avail_in = 20;

        assert_eq!(printf_bytes(&mut state, b"nope"), Err(ReturnCode::ERRNO));
        assert!(!state.again());
        assert_eq!(state.pos(), 0);
    }

    #[test]
    fn the_loan_is_exactly_size_bytes_and_starts_after_the_buffered_input() {
        let (mut state, _sink) = writer(16);
        assert_eq!(gzputs(&mut state, b"hello"), 5);
        assert_eq!(state.stream().avail_in, 5);

        {
            let scratch = printf_begin(&mut state).unwrap();
            assert_eq!(scratch.len(), 16, "exactly state->size");
            assert!(!scratch.is_empty());
            assert_eq!(
                scratch.as_slice().last(),
                Some(&0),
                "the overflow sentinel is planted"
            );
        }

        // The region began at index 5, immediately after "hello", which is C's
        // `next = in + (next_in - in) + avail_in`.
        assert_eq!(state.in_slice()[20], 0, "in[5 + 16 - 1] is the sentinel");
    }

    #[test]
    fn the_shim_sequence_begin_format_commit_works() {
        let (mut state, _sink) = writer(16);
        assert_eq!(gzputs(&mut state, b"hello"), 5);

        let reported = {
            let mut scratch = printf_begin(&mut state).unwrap();
            let region = scratch.as_mut_slice();
            region[..3].copy_from_slice(b"abc");
            region[3] = 0;
            3_usize
        };
        assert_eq!(printf_commit(&mut state, reported), Ok(3));

        assert_eq!(buffered(&state), b"helloabc");
        assert_eq!(state.pos(), 8);
    }

    #[test]
    fn the_loan_can_be_consumed_for_its_region() {
        let (mut state, _sink) = writer(16);

        let reported = {
            let region = printf_begin(&mut state).unwrap().into_mut_slice();
            assert_eq!(region.len(), 16);
            region[..2].copy_from_slice(b"hi");
            region[2] = 0;
            2_usize
        };
        assert_eq!(printf_commit(&mut state, reported), Ok(2));

        assert_eq!(buffered(&state), b"hi");
    }

    #[test]
    fn begin_refuses_a_read_stream_without_allocating() {
        let (mut state, _sink) = writer(16);
        state.set_mode(GZ_READ);

        assert!(printf_begin(&mut state).is_err());
        assert_eq!(state.size(), 0);
    }

    #[test]
    fn the_example_c_sequence_reports_eight_and_produces_hello_hello() {
        // `test/example.c` L104-L114, verbatim:
        //     gzputc(file, 'h');
        //     if (gzputs(file, "ello") != 4) ... exit(1);
        //     if (gzprintf(file, ", %s!", "hello") != 8) ... exit(1);
        //     gzseek(file, 1L, SEEK_CUR);   /* add one zero byte */
        //     gzclose(file);
        // and then L123-L131 reads back `strlen(hello) + 1 == 14` bytes equal to "hello, hello!".
        let (mut state, sink) = writer(GZBUFSIZE);

        assert_eq!(gzputc(&mut state, i32::from(b'h')), i32::from(b'h'));
        assert_eq!(gzputs(&mut state, b"ello"), 4);
        // ", %s!" with "hello" renders as ", hello!" -- eight bytes, which is the assertion.
        assert_eq!(printf_bytes(&mut state, b", hello!"), Ok(8));
        assert_eq!(state.pos(), 13);

        // `gzseek(file, 1L, SEEK_CUR)` on a write stream just records the distance.
        state.set_skip(1);
        assert_eq!(gzclose_w(&mut state), ReturnCode::OK.as_i32());

        // 14 bytes: the string plus the zero the seek added, which is exactly what `gzread` returns
        // and `strcmp` then matches against `hello`.
        assert_eq!(&sink.borrow()[..], b"hello, hello!\0");
        assert_eq!(sink.borrow().len(), b"hello, hello!".len() + 1);
    }
}
