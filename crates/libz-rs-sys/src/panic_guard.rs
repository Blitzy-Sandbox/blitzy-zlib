//! The one place the port's abort-on-panic guarantee is written down.
//!
//! Every function this crate exports is called from code that `rustc` did not
//! compile. That single fact fixes the whole panic policy, and this module
//! records it so that the policy can be audited by reading one file instead of
//! all ninety-five exported functions.
//!
//! # The guarantee
//!
//! **A Rust panic never crosses the C ABI boundary.** If one reaches the
//! boundary, the process aborts. It does not unwind into the caller, and it is
//! not quietly swallowed.
//!
//! # Why an abort, and not an unwind
//!
//! A program that links `libz` is overwhelmingly likely not to be written in
//! Rust, and therefore not to support stack unwinding at all: its frames carry
//! no landing pads, no cleanup blocks and no notion of a payload in flight.
//! Letting a Rust unwind travel through such a frame is undefined behaviour, not
//! merely untidy -- the callee tears down frames the caller never agreed could be
//! torn down. Aborting is the conservative and correct choice at an FFI edge,
//! because a dead process cannot corrupt anyone's data.
//!
//! The calling convention is what makes that abort automatic. A panic that tries
//! to escape an `extern "C"` function is turned into an abort by the language
//! itself. `extern "C-unwind"` is the opposite choice: it deliberately *permits*
//! the unwind to propagate into the caller, which is only sound when the caller
//! is known to understand unwinding. It has been stable since Rust 1.71 and it
//! is **categorically forbidden in this crate**. Every one of the ninety-five
//! exports is declared `extern "C"`.
//!
//! # Three mechanisms, and why all three are needed
//!
//! 1. **`extern "C"` on every export.** The language-level backstop described
//!    above. It is unconditional and needs no cooperation from the build profile.
//! 2. **`panic = "abort"` in `[profile.release]` of the *root* `Cargo.toml`.**
//!    This removes the unwinding machinery from release builds altogether, so
//!    the abort is not merely a landing-pad decision but the only behaviour the
//!    binary contains. Two facts about that key matter here. First, Cargo honours
//!    `[profile.*]` **only** in the workspace root manifest: it warns about and
//!    then ignores profile tables in member manifests, which is why
//!    `crates/libz-rs-sys/Cargo.toml` deliberately declares none and why moving
//!    the key "closer to the code" would silently disable it. Second, Cargo
//!    ignores `panic` for the `test` and `bench` profiles, so a `cargo test`
//!    build always unwinds no matter what any manifest says.
//! 3. **[`guard`], in this module.** Because of that second fact, a profile key
//!    alone cannot deliver the guarantee in *every* configuration -- debug and
//!    test builds still unwind. [`guard`] closes the gap by catching the unwind
//!    at the boundary and aborting explicitly, so the observable behaviour is the
//!    same whichever profile produced the binary.
//!
//! # Correct under `panic = "abort"`, not merely dead
//!
//! In a release build the panic runtime aborts at the `panic!` site, so control
//! never reaches [`guard`]'s recovery arm and `catch_unwind` catches nothing.
//! That is the intended outcome: the guard's contract is "a panic aborts the
//! process", and an abort one stack frame earlier honours it exactly. Nothing in
//! this module depends on the recovery arm running -- it adds no state, publishes
//! no flag and performs no cleanup that a later call would rely on -- so the
//! release configuration is a strictly *stronger* realisation of the same
//! contract, not a configuration in which the code is subtly wrong. The arm is
//! retained because it is the only thing that upholds the contract in debug and
//! test builds, and because the diagnostic it prints names this library as the
//! origin of the abort, which a bare panic runtime message does not.
//!
//! # Defence in depth, not a licence to panic
//!
//! The engine is written so that a panic is **unreachable**, not merely
//! contained. A guard that fires is a bug in this library, and should be
//! reported as one rather than absorbed as a normal error path. Concretely, in
//! this crate and in the `zlib-rs` core:
//!
//! * every fallible operation reports its outcome as a [`ReturnCode`], never as
//!   a panic;
//! * `unwrap`, `expect`, `panic!`, `todo!`, `unimplemented!`, `unreachable!`,
//!   panicking indexing and slicing, and arithmetic that can overflow in a debug
//!   build must not appear in any library path. The workspace lint table denies
//!   each of these, and the `allow-unwrap-in-tests`, `allow-expect-in-tests`,
//!   `allow-panic-in-tests` and `allow-indexing-slicing-in-tests` grants in the
//!   root `clippy.toml` apply to test code only -- this module's non-test code
//!   receives no exemption;
//! * allocation failure is reported as `Z_MEM_ERROR` through the caller's own
//!   allocator, so a failed allocation is a return value rather than a panic.
//!
//! # This module exports no symbol
//!
//! Nothing here is `#[no_mangle]` or `#[export_name]`, and nothing here may
//! become so. The exported surface of the built library is exactly the
//! ninety-five functions of the C API; the reference C shared library publishes
//! 111 dynamic globals -- those ninety-five type-`T` functions plus sixteen
//! type-`A` symbol-version nodes -- under the soname `libz.so.1`, and
//! `crates/libz-rs-sys/tests/symbol_parity.rs` diffs the built library against
//! that baseline. One stray exported symbol from this module would fail the diff.
//! The items below are ordinary Rust functions and constants, reached through the
//! module path and inlined away.
//!
//! # Feature configuration
//!
//! [`guard`] and [`guard_or`] have one signature and one contract, and two
//! implementations selected at compile time:
//!
//! * **`gz` enabled (the default).** `gz = ["zlib-rs/std"]` is the only feature
//!   of this crate that brings in `std`, so it is also the condition under which
//!   `std::panic::catch_unwind` is available. This is the path that catches the
//!   unwind and aborts explicitly.
//! * **`gz` disabled.** Only `core` is assumed. There is no `catch_unwind`
//!   without `std`, so there is no point at which an unwind could be observed: the
//!   body is called directly and the guarantee rests on mechanisms 1 and 2 above.
//!   The recovery arm is not merely unreachable in this configuration, it is not
//!   compiled at all -- both arms sit side by side in each guard's body, under
//!   opposite `cfg` conditions, so which one a given build uses can be read off
//!   the source rather than inferred.
//!
//! Nothing here requires the crate root to be `no_std` when `gz` is off; this
//! module is merely willing to be. That is a distinction worth keeping, because
//! the crate builds a `cdylib` and a `staticlib`, and a `no_std` build of either
//! must supply its own `#[panic_handler]` -- a crate-root obligation, not a
//! module-level one. Both configurations of the root have been checked against
//! this module.
//!
//! Neither path allocates, takes a lock, or performs a system call when the body
//! returns normally, which is the property that lets every export be wrapped
//! without measurable cost.
//!
//! # Usage
//!
//! ```ignore
//! // The ordinary case: an `int`-returning export whose body yields a ReturnCode.
//! #[no_mangle]
//! pub extern "C" fn deflateReset(strm: z_streamp) -> c_int {
//!     panic_guard::guard_code(|| { /* ... safe core call ... */ })
//! }
//!
//! // A non-`int` return: wrap the body and let a panic abort.
//! #[no_mangle]
//! pub extern "C" fn compressBound(source_len: c_ulong) -> c_ulong {
//!     panic_guard::guard(|| { /* ... */ })
//! }
//!
//! // Where returning the export's documented failure value is provably safer
//! // than aborting, name that value from `fallback` rather than inventing it.
//! #[no_mangle]
//! pub extern "C" fn gzbuffer(file: gzFile, size: c_uint) -> c_int {
//!     panic_guard::guard_or(panic_guard::fallback::GZ_ERROR, || { /* ... */ })
//! }
//! ```
//!
//! # Ported from
//!
//! Nothing, in the sense that no C source line corresponds to this module: the C
//! implementation has no panics to guard against and no unwinding to suppress.
//! It exists because reimplementing `zlib.h`'s contract in Rust introduces a
//! failure mode -- the panic -- that the original could not have, and the contract
//! at `zlib.h` L181-L192 admits only integer status codes as failure reports, so
//! there is nowhere for a panic to be reported to.

// -----------------------------------------------------------------------------
//  Dependencies
// -----------------------------------------------------------------------------
//
// `core` and `std` only, plus the workspace's own status type. This module is
// pure safe Rust: it holds no raw pointer, performs no type punning, and
// contains no `unsafe` block, so the crate root's requirement that every unsafe
// operation be individually scoped costs it nothing.

use core::ffi::c_int;

use zlib_rs::error::ReturnCode;

// -----------------------------------------------------------------------------
//  Diagnostics
// -----------------------------------------------------------------------------

/// Prefix of the message written to standard error when a panic is caught at the
/// boundary and the process is about to abort.
///
/// The text names the library, the boundary and the disposition, because the
/// panic runtime's own message says only that a thread panicked -- which, in a
/// process that is mostly C, is not enough to identify where the fault came from.
#[cfg(feature = "gz")]
const ABORT_NOTE: &str = "libz-rs-sys: a Rust panic reached the C ABI boundary; \
                          aborting rather than unwinding into a non-Rust caller: ";

/// Prefix of the message written to standard error when a panic is caught at the
/// boundary and the export's documented failure value is returned instead.
///
/// A recovered panic is still a bug, so it is reported as loudly as an aborted
/// one; the only difference is that the process survives to return a status code.
#[cfg(feature = "gz")]
const RECOVER_NOTE: &str = "libz-rs-sys: a Rust panic reached the C ABI boundary; \
                            returning the documented failure value instead: ";

/// Substituted for the panic payload when it is neither a `&str` nor a `String`.
///
/// A payload can be any `Send + Any` value, because `panic_any` accepts one; a
/// value the library never produces itself is still reported rather than
/// silently dropped.
#[cfg(feature = "gz")]
const OPAQUE_PAYLOAD: &str = "<panic payload was not a string>";

/// The type each guard's panic arm must coerce the caught payload to before
/// handing it to [`report`].
///
/// Naming the type is what makes the coercion unambiguous, and the ambiguity it
/// removes is a real trap: `catch_unwind` yields a `Box<dyn Any + Send>`, and that
/// box is *itself* `Any`. Borrowing it without dereferencing therefore unsizes the
/// box rather than the payload, every downcast in [`describe`] fails, and every
/// panic is reported as unrecognised. The mistake compiles, runs, and loses only
/// the message -- the kind that survives review -- so each arm writes
/// `let payload: &Payload = &*payload;` with this alias spelled out, and the
/// end-to-end assertion in `guard_aborts_the_process_when_the_body_panics` checks
/// the message actually arrives on standard error.
#[cfg(feature = "gz")]
type Payload = dyn core::any::Any + Send;

/// Writes `note`, then the panic payload, to standard error.
///
/// Deliberately not `eprintln!`: that macro panics if the write fails, and a
/// second panic while handling the first would replace a precise diagnostic with
/// an opaque double-panic abort. Every write result is discarded instead, so this
/// function cannot itself fail. `std::io::stderr` takes a reentrant lock, so it
/// is also safe to call from a thread that was already writing to standard error
/// when it panicked.
///
/// Marked `#[cold]` because it is on the panic path only; nothing about it is
/// reachable when a body returns normally.
#[cfg(feature = "gz")]
#[cold]
fn report(note: &str, payload: &Payload) {
    use std::io::Write as _;

    let mut stderr = std::io::stderr();
    let _ = stderr.write_all(note.as_bytes());
    let _ = stderr.write_all(describe(payload).as_bytes());
    let _ = stderr.write_all(b"\n");
    let _ = stderr.flush();
}

/// Renders a panic payload as text, or names it as unrenderable.
///
/// The two cases that occur in practice are the ones the `panic!` macro produces:
/// a `&'static str` for a bare literal, and a `String` for a formatted message.
/// Anything else reached the boundary through `panic_any`, which the library
/// never calls itself, and is reported as [`OPAQUE_PAYLOAD`] rather than dropped
/// -- an unrecognised payload is still evidence of a bug.
///
/// Separated from [`report`] so that the downcast can be tested directly. The
/// deref that the guards perform on the boxed payload before calling in here is
/// easy to omit and impossible to notice from the outside, because the result is
/// a message that merely looks unrecognised.
#[cfg(feature = "gz")]
fn describe(payload: &Payload) -> &str {
    payload
        .downcast_ref::<&'static str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or(OPAQUE_PAYLOAD)
}

// -----------------------------------------------------------------------------
//  The guards
// -----------------------------------------------------------------------------

/// Runs an exported function's body, aborting the process if it panics.
///
/// This is the guard the exported surface uses. It takes no failure value
/// because it does not return one: on the panic path it diverges, so there is
/// nothing for a C caller to misinterpret. Prefer it to [`guard_or`] unless the
/// export can be shown to leave no caller-visible state half-updated, because a
/// status code returned after a panic is indistinguishable, to the caller, from
/// an ordinary refusal -- and if the panic was the symptom of memory corruption,
/// that indistinguishability is exactly what lets the corruption spread.
///
/// When `body` returns normally its value is returned unchanged, with no
/// allocation, no lock and no system call added, so wrapping an export costs
/// nothing measurable.
///
/// # Behaviour on panic
///
/// The panic payload is written to standard error together with a line naming
/// this library and the boundary, and then `std::process::abort` is called.
/// Under `panic = "abort"` -- which the root manifest sets for release builds --
/// the panic runtime aborts first and this arm is never reached, which is the
/// same observable outcome one frame earlier.
///
/// Without the `gz` feature there is no `std`, hence no way to observe an
/// unwind: `body` is called directly and the guarantee rests on `extern "C"` and
/// on the release profile, as the module documentation explains.
///
/// Deliberately not `#[must_use]`. `gzclearerr` is declared `void` at `zlib.h`
/// L1792, so the guard is legitimately used where `T` is `()`; marking it
/// must-use would make that one export the only one obliged to write
/// `let _ = guard(..)`, and an obligation that applies to all but one call site
/// invites a local `#[allow]` that would weaken the audit trail. Discarding the
/// result is only ever correct because it *is* the body's own result.
#[inline]
pub fn guard<T, F>(body: F) -> T
where
    F: FnOnce() -> T,
{
    // `AssertUnwindSafe` is required because every real body borrows the caller's
    // stream mutably, and `&mut` is not `UnwindSafe`. Asserting it is sound here
    // for a reason specific to this guard: the panic path diverges, so no caller
    // can ever observe whatever state the panic left behind.
    //
    // `std::process::abort` is used rather than `std::process::exit`: it raises
    // `SIGABRT` without running atexit handlers or destructors, so no C cleanup
    // code gets to walk the half-finished state, and a supervisor or core dump
    // records a crash rather than an orderly shutdown.
    #[cfg(feature = "gz")]
    {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
            Ok(value) => value,
            Err(payload) => {
                // See `Payload`: the annotation and the deref are load-bearing.
                let payload: &Payload = &*payload;
                report(ABORT_NOTE, payload);
                std::process::abort()
            }
        }
    }
    // Without `std` there is no `catch_unwind`, so there is no point at which an
    // unwind could be observed and converted into an abort. The body is called
    // directly and the guarantee rests on the two unconditional mechanisms --
    // `extern "C"` at every export and `panic = "abort"` in the release profile.
    #[cfg(not(feature = "gz"))]
    {
        body()
    }
}

/// Runs an exported function's body, returning `fallback` if it panics.
///
/// The recovering counterpart of [`guard`], for the exports where handing the
/// caller its documented failure value is provably safer than killing the
/// process -- typically because the body cannot have mutated anything the caller
/// can see before the panic point. Take the value from the [`fallback`] module
/// rather than writing a literal, so that what a panicking export reports stays
/// tied to what `zlib.h` documents for it.
///
/// A recovered panic is reported to standard error exactly as an aborted one is.
/// It remains a bug in this library: recovery keeps the caller's process alive,
/// it does not make the panic acceptable.
///
/// When `body` returns normally its value is returned unchanged and `fallback` is
/// simply dropped.
///
/// Without the `gz` feature there is no `std` and therefore no way to observe an
/// unwind, so `body` is called directly and `fallback` is discarded: the recovery
/// arm is not compiled in that configuration at all.
///
/// Not `#[must_use]`, for the reason given on [`guard`].
#[inline]
pub fn guard_or<T, F>(fallback: T, body: F) -> T
where
    F: FnOnce() -> T,
{
    // Asserting unwind safety needs a second argument here, because unlike
    // `guard` this path does return to the caller. `zlib.h` L181-L192 lets a
    // caller conclude nothing from a failure code beyond "the operation did not
    // happen": the only legal next moves on a stream that reported one are to
    // retry, to end it, or to abandon it. So a logically inconsistent state left
    // behind by a panic is not observable through any documented operation --
    // which is exactly the property `AssertUnwindSafe` asks the author to
    // establish, and exactly why this guard is the exception rather than the rule.
    #[cfg(feature = "gz")]
    {
        match std::panic::catch_unwind(std::panic::AssertUnwindSafe(body)) {
            Ok(value) => value,
            Err(payload) => {
                // See `Payload`: the annotation and the deref are load-bearing.
                let payload: &Payload = &*payload;
                report(RECOVER_NOTE, payload);
                fallback
            }
        }
    }
    // Without `std` an unwind cannot be observed, so the fallback can never be
    // selected. It is accepted and discarded here rather than omitted from the
    // signature so that both configurations present one contract to call sites.
    #[cfg(not(feature = "gz"))]
    {
        drop(fallback);
        body()
    }
}

/// Runs an exported function's body and converts its status to the C `int`.
///
/// The shape most of the exported surface has: the deflate, inflate,
/// `inflateBack` and one-shot wrapper entry points all return `int`, and all of
/// them get that `int` from a [`ReturnCode`]. Routing them through here keeps the
/// conversion in one place and keeps the panic policy identical to [`guard`]'s --
/// a panic aborts.
///
/// # Examples
///
/// ```ignore
/// #[no_mangle]
/// pub extern "C" fn deflateEnd(strm: z_streamp) -> c_int {
///     panic_guard::guard_code(|| ReturnCode::STREAM_ERROR)
/// }
/// ```
#[inline]
#[must_use]
pub fn guard_code<F>(body: F) -> c_int
where
    F: FnOnce() -> ReturnCode,
{
    guard(|| body().as_i32())
}

// -----------------------------------------------------------------------------
//  Per-family failure values
// -----------------------------------------------------------------------------

/// The value each family of exports reports when it cannot do what was asked.
///
/// Every entry is the value `zlib.h` documents for that family, cited by line, so
/// that a call site never has to invent one and a reviewer never has to guess
/// whether a literal at a call site was chosen deliberately. Two of the entries
/// are helper functions rather than constants, because the C return type they
/// stand for has a platform-dependent width.
///
/// These are the values [`guard_or`] is called with. [`guard`] does not take one,
/// because it aborts; the module is nonetheless the single reference for "what
/// does this export return when it fails", which is also what the ordinary
/// non-panicking error paths need.
pub mod fallback {
    use core::ffi::{c_char, c_int, c_ulong, CStr};

    use zlib_rs::error::ReturnCode;

    /// `Z_STREAM_ERROR`: the stream state is inconsistent, or an argument was
    /// invalid.
    ///
    /// The failure value of every `int`-returning stream entry point --
    /// `deflate`, `deflateEnd`, `deflateReset`, `inflate`, `inflateEnd`,
    /// `inflateBack`, `compress2`, `uncompress2` and their siblings -- when the
    /// operation cannot proceed at all. It is also what `gzsetparams`,
    /// `gzflush`, `gzclose`, `gzclose_r`, `gzclose_w` and `gzprintf` report,
    /// because those are documented to return "the zlib error number"
    /// (`zlib.h` L1650) or "a negative zlib error code" (`zlib.h` L1556) rather
    /// than a bare `-1`.
    ///
    /// Ported from `Z_STREAM_ERROR`, `zlib.h` L185.
    pub const STREAM_ERROR: c_int = ReturnCode::STREAM_ERROR.as_i32();

    /// `Z_MEM_ERROR`: an allocation failed.
    ///
    /// Used in place of [`STREAM_ERROR`] wherever the C implementation would have
    /// reached its own out-of-memory path, so that a caller inspecting the code
    /// -- `test/example.c` and `test/infcover.c` both do -- sees the same
    /// distinction the reference draws.
    ///
    /// Ported from `Z_MEM_ERROR`, `zlib.h` L187.
    pub const MEM_ERROR: c_int = ReturnCode::MEM_ERROR.as_i32();

    /// [`STREAM_ERROR`] as a [`ReturnCode`], for bodies that yield a status
    /// rather than a raw `int`.
    ///
    /// The same value at the type the safe core speaks, so that a body passed to
    /// [`guard_code`](super::guard_code) and a fallback passed to
    /// [`guard_or`](super::guard_or) cannot disagree.
    pub const STREAM_ERROR_CODE: ReturnCode = ReturnCode::STREAM_ERROR;

    /// [`MEM_ERROR`] as a [`ReturnCode`].
    pub const MEM_ERROR_CODE: ReturnCode = ReturnCode::MEM_ERROR;

    /// The upper bound reported by `deflateBound` and `compressBound` when the
    /// bound cannot be computed: the largest value the return type can hold.
    ///
    /// These two functions have no documented failure value (`zlib.h` L768-L780,
    /// L1307-L1312), and callers size an output buffer with whatever they
    /// return, so the choice is not free: a bound that is too small becomes a
    /// heap overflow *in the caller's code*, while a bound that is too large only
    /// makes the caller's own allocation fail, which it must already handle.
    /// Saturating is therefore the sole safe direction.
    ///
    /// Ported from `deflateBound`, `zlib.h` L768, and `compressBound`,
    /// `zlib.h` L1307.
    pub const BOUND: c_ulong = c_ulong::MAX;

    /// [`BOUND`] at the width of `z_size_t`, for `deflateBound_z` and
    /// `compressBound_z`.
    ///
    /// Ported from `deflateBound_z`, `zlib.h` L769, and `compressBound_z`,
    /// `zlib.h` L1308.
    pub const BOUND_Z: usize = usize::MAX;

    /// The value `adler32` and `adler32_z` return when they are given no data: 1.
    ///
    /// `zlib.h` L1813-L1814 documents that a null buffer yields "the required
    /// initial value for the checksum", and the documented usage example seeds a
    /// running checksum with exactly this call. Returning the seed is therefore
    /// the one answer that is both in range for an Adler-32 and impossible to
    /// mistake for a status code -- which matters, because the return type is
    /// `uLong` and a negative `int` reinterpreted through it would be a plausible
    /// checksum.
    ///
    /// Ported from `adler32`, `zlib.h` L1809-L1814.
    pub const ADLER32_EMPTY: c_ulong = 1;

    /// The value `crc32` and `crc32_z` return when they are given no data: 0.
    ///
    /// The CRC-32 counterpart of [`ADLER32_EMPTY`]; `zlib.h` L1852-L1853 gives
    /// the same rule and the documented usage example seeds with `crc32(0L,
    /// Z_NULL, 0)`.
    ///
    /// Ported from `crc32`, `zlib.h` L1848-L1853.
    pub const CRC32_EMPTY: c_ulong = 0;

    /// The value the combine family returns for an input it cannot use: 0.
    ///
    /// `zlib.h` L1880 and L1887 already define this for a negative `len2` --
    /// "len2 must be non-negative, otherwise zero is returned" -- so
    /// `crc32_combine`, `crc32_combine_gen`, `crc32_combine_op` and their `*64`
    /// forms have a documented failure value and need no invented one.
    /// `adler32_combine` has no such clause (`zlib.h` L1844-L1845 says only that
    /// a negative length leaves the result with "no meaning or utility"), and uses
    /// the same zero for consistency.
    ///
    /// Ported from `crc32_combine`, `zlib.h` L1880, and `crc32_combine_gen`,
    /// `zlib.h` L1887.
    pub const COMBINE_INVALID: c_ulong = 0;

    /// `-1`: the failure value of the `gz` entry points that report one that way.
    ///
    /// Namely `gzread` ("or -1 for error", `zlib.h` L1486), `gzgetc` ("this byte
    /// or -1", `zlib.h` L1615) and `gzgetc_` (`zlib.h` L1961), `gzungetc`
    /// (`zlib.h` L1630), `gzputc` ("or -1 in case of error", `zlib.h` L1610),
    /// `gzputs` ("or -1 in case of error", `zlib.h` L1580), `gzbuffer` ("-1 on
    /// failure", `zlib.h` L1441) and `gzrewind`, which `zlib.h` L1687 defines as
    /// `(int)gzseek(file, 0L, SEEK_SET)`.
    ///
    /// Note that this is **not** the failure value of every `gz` function that
    /// returns `int`: see [`GZ_NOTHING_WRITTEN`] and [`STREAM_ERROR`].
    ///
    /// Ported from `gzread`, `zlib.h` L1486.
    pub const GZ_ERROR: c_int = -1;

    /// `0`: the value `gzwrite` returns on error.
    ///
    /// A deliberate exception to [`GZ_ERROR`]. `zlib.h` L1522-L1523 documents
    /// that `gzwrite` "returns the number of uncompressed bytes written, or 0 in
    /// case of error or if len is 0" -- the count is unsigned in spirit, so `-1`
    /// is not available and zero carries the failure. Reporting `-1` here would
    /// tell a caller that a negative number of bytes was written.
    ///
    /// Ported from `gzwrite`, `zlib.h` L1519-L1523.
    pub const GZ_NOTHING_WRITTEN: c_int = 0;

    /// `0`: the item count `gzfread` and `gzfwrite` return on error.
    ///
    /// Both duplicate `fread`/`fwrite`, returning a `z_size_t` count of complete
    /// items, so zero is the only failure value the type admits. `zlib.h` L1501
    /// and L1537 both say so, and L1503 adds that `gzerror` must be consulted to
    /// distinguish a zero count from end of file.
    ///
    /// Ported from `gzfread`, `zlib.h` L1492-L1503, and `gzfwrite`,
    /// `zlib.h` L1529-L1538.
    pub const GZ_NO_ITEMS: usize = 0;

    /// `1`: the answer the `gz` predicates give when the truth is unknown.
    ///
    /// `gzeof` (`zlib.h` L1711) and `gzdirect` (`zlib.h` L1726) return a boolean
    /// and have no failure value at all. Neither answer can corrupt memory, so
    /// the choice is about behaviour: reporting end of file stops a read loop
    /// rather than spinning it (`zlib.h` L1721-L1722), and reporting direct
    /// copying is what `gzdirect` already answers before four bytes of input have
    /// been seen (`zlib.h` L1740).
    ///
    /// Ported from `gzeof`, `zlib.h` L1711, and `gzdirect`, `zlib.h` L1726.
    pub const GZ_TRUE: c_int = 1;

    /// A valid, empty, NUL-terminated message for `gzerror`.
    ///
    /// `gzerror` returns a `const char *` that callers pass straight to `printf`,
    /// so the failure value must be a readable string. The C implementation does
    /// return `NULL` for a handle it cannot recognise, but a caller that has just
    /// tripped a panic is precisely the caller least likely to check, and handing
    /// it `NULL` would turn one fault into two. The empty string is the value the
    /// C implementation returns for a valid handle with no message pending, so it
    /// is both safe to print and already part of the documented behaviour.
    ///
    /// Take the pointer with [`CStr::as_ptr`], which needs no `unsafe`; the
    /// string is `'static`, so the pointer outlives any call.
    ///
    /// Ported from `gzerror`, `zlib.h` L1775.
    pub const GZ_NO_MESSAGE: &CStr = c"";

    /// `Z_NULL`: the handle the `gzopen` family returns when it cannot open.
    ///
    /// Serves `gzopen` (`zlib.h` L1357), `gzdopen` (L1404), `gzopen64` (L1978),
    /// `gzopen_w` (L2042) and also `gzgets` (L1597-L1598), which returns a null
    /// `char *` "for end-of-file or in case of error". `Z_NULL` is spelled `0` at
    /// `zlib.h` L216 and is a null pointer in every one of those positions, so one
    /// generic helper covers them: the pointee is inferred from the export's
    /// return type.
    ///
    /// A function rather than a constant because the pointee differs per export.
    /// Constructing a null pointer requires no `unsafe`; only dereferencing one
    /// would, and nothing here dereferences it.
    ///
    /// Ported from `Z_NULL`, `zlib.h` L216.
    #[inline]
    #[must_use]
    pub const fn null_handle<T>() -> *mut T {
        core::ptr::null_mut()
    }

    /// A null `char *`, the value `gzgets` returns on error or end of file.
    ///
    /// A named spelling of [`null_handle`] for the one export whose null is a
    /// string rather than an opaque handle, so that a call site reads as the
    /// header does.
    ///
    /// Ported from `gzgets`, `zlib.h` L1588-L1598.
    #[inline]
    #[must_use]
    pub const fn null_string() -> *mut c_char {
        null_handle()
    }

    /// `-1`: the offset the seek family reports on error.
    ///
    /// Serves `gzseek` ("or -1 in case of error", `zlib.h` L1678), `gztell`
    /// (`zlib.h` L1691, defined at L1698 as `gzseek(file, 0L, SEEK_CUR)`),
    /// `gzoffset` ("On error, `gzoffset()` returns -1", `zlib.h` L1708) and their
    /// `*64` forms.
    ///
    /// A generic function rather than a constant because `z_off_t` and
    /// `z_off64_t` resolve to `off_t`, `off64_t`, `__int64`, `offset_t` or
    /// `long long` depending on the target (`zconf.h` L494-L531). All are signed
    /// and at least 32 bits wide, so `-1` is representable in every one of them
    /// and the conversion from `i8` is exact -- which is why this is written as a
    /// widening conversion rather than a truncating cast from a fixed width.
    #[inline]
    #[must_use]
    pub fn offset_error<T: From<i8>>() -> T {
        T::from(-1)
    }
}

// -----------------------------------------------------------------------------
//  Tests
// -----------------------------------------------------------------------------
//
// Test code is the one place in this crate where panicking is permitted: the
// root `clippy.toml` grants `allow-panic-in-tests`, `allow-unwrap-in-tests` and
// `allow-expect-in-tests`, and nothing below this banner is compiled into the
// shipped library.
//
// Which semantics are asserted, and how:
//
//   * `guard_or` recovers, so its panic path is asserted in process.
//   * `guard` aborts, which cannot be asserted in process without killing the
//     test runner. It is asserted out of process instead: the test re-executes
//     the test binary, filtered to itself and with a marker variable set, and
//     requires the child to die of SIGABRT. A child that returned normally, or
//     that failed for any other reason, fails the assertion -- including the case
//     where the filter matched nothing, because a child that ran no tests exits
//     successfully and so carries no signal.

#[cfg(test)]
mod tests {
    use core::ffi::c_ulong;

    use super::{fallback, guard, guard_code, guard_or, ReturnCode};

    #[test]
    fn guard_returns_the_body_value_untouched() {
        assert_eq!(guard(|| 42_i32), 42);
        assert_eq!(guard(|| c_ulong::MAX), c_ulong::MAX);
        assert_eq!(guard(|| ReturnCode::OK), ReturnCode::OK);
        assert_eq!(guard(|| "a borrowed string"), "a borrowed string");
    }

    #[test]
    fn guard_accepts_a_unit_returning_body() {
        // `gzclearerr` is declared `void` at `zlib.h` L1792, so the guard has to
        // be usable where there is no value to return.
        let mut ran = false;
        guard(|| ran = true);
        assert!(ran);
    }

    #[test]
    fn the_guards_accept_a_body_that_borrows_mutably() {
        // Every real export body borrows the caller's stream mutably, and `&mut`
        // is not `UnwindSafe`. This is what `AssertUnwindSafe` inside `catch`
        // buys: no call site has to wrap anything itself.
        let mut total = 0_u32;
        let seen = guard(|| {
            total += 5;
            total
        });
        assert_eq!((seen, total), (5, 5));

        let mut other = 0_i32;
        let seen = guard_or(fallback::GZ_ERROR, || {
            other += 3;
            other
        });
        assert_eq!((seen, other), (3, 3));
    }

    #[test]
    fn guard_or_returns_the_body_value_when_nothing_panics() {
        assert_eq!(guard_or(fallback::STREAM_ERROR, || 7_i32), 7);
        assert_eq!(guard_or(fallback::BOUND, || 12), 12);
    }

    #[cfg(feature = "gz")]
    #[test]
    fn guard_or_returns_the_fallback_when_the_body_panics() {
        assert_eq!(
            guard_or(fallback::STREAM_ERROR, || panic!("deliberate test panic")),
            fallback::STREAM_ERROR
        );
        assert_eq!(
            guard_or(fallback::GZ_NOTHING_WRITTEN, || panic!(
                "{}",
                "formatted payload"
            )),
            fallback::GZ_NOTHING_WRITTEN
        );
        // A payload that is neither `&str` nor `String` still selects the
        // fallback; `report` substitutes a description for it.
        assert_eq!(
            guard_or(fallback::MEM_ERROR, || std::panic::panic_any(7_u8)),
            fallback::MEM_ERROR
        );
    }

    #[cfg(feature = "gz")]
    #[test]
    fn the_panic_payload_survives_the_box_it_arrives_in() {
        // Regression guard. `catch_unwind` yields a `Box<dyn Any + Send>`, and
        // that box is itself `Any`, so borrowing it without dereferencing unsizes
        // the box rather than the payload and every downcast below silently
        // fails. The symptom is not a compile error or a crash -- it is a
        // diagnostic that reports every panic as unrecognised, which is exactly
        // the information an operator needs and the easiest thing to lose.
        let literal = std::panic::catch_unwind(|| panic!("payload from a literal")).unwrap_err();
        let literal: &super::Payload = &*literal;
        assert_eq!(super::describe(literal), "payload from a literal");

        let formatted = std::panic::catch_unwind(|| panic!("{}", "payload from a format"));
        let formatted = formatted.unwrap_err();
        let formatted: &super::Payload = &*formatted;
        assert_eq!(super::describe(formatted), "payload from a format");

        let opaque = std::panic::catch_unwind(|| std::panic::panic_any(9_u16)).unwrap_err();
        let opaque: &super::Payload = &*opaque;
        assert_eq!(super::describe(opaque), super::OPAQUE_PAYLOAD);
    }

    #[test]
    fn guard_code_converts_the_status_to_the_c_int_the_abi_expects() {
        assert_eq!(guard_code(|| ReturnCode::OK), 0);
        assert_eq!(guard_code(|| ReturnCode::STREAM_END), 1);
        assert_eq!(guard_code(|| ReturnCode::NEED_DICT), 2);
        assert_eq!(
            guard_code(|| ReturnCode::STREAM_ERROR),
            fallback::STREAM_ERROR
        );
        assert_eq!(guard_code(|| ReturnCode::MEM_ERROR), fallback::MEM_ERROR);
    }

    #[test]
    fn the_int_returning_fallbacks_carry_the_documented_values() {
        assert_eq!(fallback::STREAM_ERROR, -2);
        assert_eq!(fallback::MEM_ERROR, -4);
        assert_eq!(fallback::GZ_ERROR, -1);
        assert_eq!(fallback::GZ_NOTHING_WRITTEN, 0);
        assert_eq!(fallback::GZ_NO_ITEMS, 0);
        assert_eq!(fallback::GZ_TRUE, 1);
    }

    #[test]
    fn the_status_fallbacks_agree_with_their_int_forms() {
        assert_eq!(fallback::STREAM_ERROR_CODE.as_i32(), fallback::STREAM_ERROR);
        assert_eq!(fallback::MEM_ERROR_CODE.as_i32(), fallback::MEM_ERROR);
        assert!(fallback::STREAM_ERROR_CODE.is_error());
        assert!(fallback::MEM_ERROR_CODE.is_error());
    }

    #[test]
    fn the_checksum_fallbacks_are_the_documented_seeds() {
        // `zlib.h` L1812 and L1851: a null buffer yields the initial value.
        assert_eq!(fallback::ADLER32_EMPTY, 1);
        assert_eq!(fallback::CRC32_EMPTY, 0);
        // `zlib.h` L1877 and L1884: an unusable length yields zero.
        assert_eq!(fallback::COMBINE_INVALID, 0);
        // None of them is one of the nine status codes reinterpreted as a
        // checksum, which is the mistake this module exists to prevent.
        for seed in [fallback::ADLER32_EMPTY, fallback::CRC32_EMPTY] {
            assert!(seed <= c_ulong::from(u32::MAX));
        }
    }

    #[test]
    fn the_bound_fallback_saturates_rather_than_truncating() {
        assert_eq!(fallback::BOUND, c_ulong::MAX);
        assert_eq!(fallback::BOUND_Z, usize::MAX);
        // The dangerous direction is a bound that is too small: the caller sizes
        // its output buffer with it.
        assert_ne!(fallback::BOUND, 0);
        assert_ne!(fallback::BOUND_Z, 0);
    }

    #[test]
    fn the_pointer_fallbacks_are_null() {
        assert!(fallback::null_handle::<u8>().is_null());
        assert!(fallback::null_handle::<c_ulong>().is_null());
        assert!(fallback::null_string().is_null());
    }

    #[test]
    fn the_gzerror_fallback_is_a_printable_non_null_empty_string() {
        // Non-nullness needs no assertion: `CStr::as_ptr` cannot return null,
        // and `useless_ptr_null_checks` rejects a test that pretends otherwise.
        // That static guarantee is the whole reason this is the right fallback
        // for an export whose result is handed to `printf`.
        assert_eq!(fallback::GZ_NO_MESSAGE.to_bytes(), b"");
        assert_eq!(fallback::GZ_NO_MESSAGE.to_bytes_with_nul(), b"\0");
    }

    #[test]
    fn the_offset_fallback_is_minus_one_at_every_signed_width() {
        assert_eq!(fallback::offset_error::<i32>(), -1);
        assert_eq!(fallback::offset_error::<i64>(), -1);
        assert_eq!(fallback::offset_error::<core::ffi::c_long>(), -1);
        assert_eq!(fallback::offset_error::<core::ffi::c_longlong>(), -1);
    }

    /// Marker variable that switches the abort probe from its parent role to its
    /// child role.
    #[cfg(all(unix, feature = "gz"))]
    const ABORT_PROBE: &str = "ZLIB_RS_PANIC_GUARD_ABORT_PROBE";

    /// The POSIX signal number of `SIGABRT`, which `abort` raises.
    #[cfg(all(unix, feature = "gz"))]
    const SIGABRT: i32 = 6;

    /// The panic message the child half of the abort probe raises.
    #[cfg(all(unix, feature = "gz"))]
    const PROBE_MESSAGE: &str = "deliberate abort-probe panic";

    #[cfg(all(unix, feature = "gz"))]
    #[test]
    #[cfg_attr(
        miri,
        ignore = "Miri does not support posix_spawn, so the child process cannot be launched"
    )]
    fn guard_aborts_the_process_when_the_body_panics() {
        use std::os::unix::process::ExitStatusExt as _;
        use std::process::{Command, Stdio};

        if std::env::var_os(ABORT_PROBE).is_some() {
            // Child role. This must not come back; if it does, the panic below
            // makes the child exit through libtest instead of through a signal,
            // and the parent's assertion fails.
            let _unreached: i32 = guard(|| panic!("{PROBE_MESSAGE}"));
            panic!("guard returned instead of aborting the process");
        }

        // Parent role. Re-run exactly this test in a child process. The name is
        // derived rather than written out, so renaming the test or moving the
        // module cannot leave a filter that silently matches nothing -- a child
        // that ran no tests exits successfully and so carries no signal, failing
        // the assertion below rather than passing vacuously.
        let module = module_path!();
        let relative = module.split_once("::").map_or(module, |(_, rest)| rest);
        let filter = format!("{relative}::guard_aborts_the_process_when_the_body_panics");

        let binary = std::env::current_exe().expect("the test binary has a path");
        let child = Command::new(binary)
            .args(["--exact", &filter, "--nocapture"])
            .env(ABORT_PROBE, "1")
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .output()
            .expect("the test binary can be run as a child process");

        assert_eq!(
            child.status.signal(),
            Some(SIGABRT),
            "the child should have died of SIGABRT, but reported {:?}",
            child.status
        );

        // The whole diagnostic chain, end to end: the guard caught the unwind,
        // named this library as the origin, and recovered the panic message from
        // the boxed payload. The last of those is the part that fails silently if
        // the payload is borrowed without being dereferenced, so it is asserted
        // here rather than inferred from the unit test on `describe`.
        let stderr = String::from_utf8_lossy(&child.stderr);
        assert!(
            stderr.contains(super::ABORT_NOTE),
            "the abort note should name the boundary; stderr was: {stderr}"
        );
        assert!(
            stderr.contains(PROBE_MESSAGE),
            "the panic message should survive the boxed payload; stderr was: {stderr}"
        );
    }
}
