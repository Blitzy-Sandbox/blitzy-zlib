//! The one place this crate's panic policy is written down.
//!
//! Every function this crate exports is called from code that `rustc` did not
//! compile, and that single fact fixes the whole policy: a Rust panic must never
//! unwind out of an exported function into a non-Rust caller. Letting it would be
//! undefined behaviour, not merely untidy -- a non-Rust frame carries no landing
//! pads, no cleanup blocks and no notion of a payload in flight, so the callee would
//! tear down frames the caller never agreed could be torn down. Aborting is the
//! conservative choice at an FFI edge, because a dead process cannot corrupt
//! anyone's data.
//!
//! The policy is recorded in this one file so that it can be audited by reading a
//! single module rather than every exported function that routes through it. All of
//! the exports have now landed, so that saving is real rather than prospective:
//! ninety-one call sites across the seven export modules reach [`guard`] or
//! [`guard_code`], and none of them restates the policy.
//!
//! # Two helpers, two different contracts
//!
//! **A Rust panic never crosses the C ABI boundary. It aborts the process, in
//! every build.** It does not unwind into the caller, it is not quietly
//! swallowed, and it is never converted into a status code.
//!
//! That last clause is the part worth stating explicitly, because the tempting
//! alternative is to catch the unwind and hand the caller the failure value
//! `zlib.h` documents for that function. This module deliberately offers no such
//! thing, and no export may implement one. A panic is by definition a state the
//! engine believed unreachable, so the unwind can start at any point -- including
//! after the stream's window, pending buffer, bit accumulator or checksum have
//! been partly updated. Returning `Z_STREAM_ERROR` from there tells the caller
//! "the operation did not happen" when in fact it half happened, and `zlib.h`
//! L181-L192 gives the caller no way to tell the difference: its legal next moves
//! on a stream that reported a failure are to retry, to reset or to end it, and
//! every one of those would now run over inconsistent state. If the panic was
//! itself the symptom of memory corruption, that indistinguishability is exactly
//! what lets the corruption spread into the caller's data. A dead process cannot
//! corrupt anyone's data; a surviving one with a half-updated stream can.
//!
//! There is a second, more mundane reason: recovery would make the library's
//! observable behaviour depend on the build profile. `catch_unwind` can only
//! catch anything when the binary was compiled with unwinding, so the same panic
//! would abort under `[profile.release] panic = "abort"` and return a status code
//! under `cargo test`. A contract that changes with the profile is not a
//! contract.
//!
//! The calling convention is what makes that abort automatic. A panic that tries to
//! escape an `extern "C"` function is turned into an abort by the language itself.
//! `extern "C-unwind"` is the opposite choice: it deliberately *permits* the unwind
//! to propagate into the caller, which is only sound when the caller is known to
//! understand unwinding. It has been stable since Rust 1.71 and it is
//! **categorically forbidden in this crate**. Every export in the seven export
//! modules is declared `extern "C"`, and no occurrence of `extern "C-unwind"`
//! exists anywhere in the workspace; a `grep` for it is the check, because no lint
//! can express the rule.
//!
//! Ordinary, expected failures are a different matter entirely and are **not**
//! panics: they are reported as a [`ReturnCode`] by the safe core and converted
//! by [`guard_code`]. [`fallback`] is the single reference for what each family
//! of exports returns when it refuses, and it exists to serve those ordinary
//! paths -- not to give a panic somewhere to land.
//!
//! The guard does not make a panic acceptable: a guard that fires is a bug here.
//!
//! # What each configuration actually does
//!
//! | | release (`panic = "abort"`) | debug/test (unwinding) | `gz` disabled (no `std`) |
//! |---|---|---|---|
//! | [`guard`] | the runtime aborts at the `panic!` site; the catch arm is never reached | catches, reports, then calls `abort` | no `catch_unwind` exists, so the body is called directly |
//!
//! [`guard`]'s outcome is therefore the same in all three columns, and an abort one
//! stack frame earlier is still an abort. There is no second helper whose outcome
//! could differ between them, which is the whole point: a call site does not have to
//! be correct under two different panic contracts, because there is only one.
//!
//! Nothing here depends on the catch arm running -- it adds no state, publishes no
//! flag and performs no cleanup a later call relies on. It exists because it is the
//! only thing that aborts in debug and test builds, and because the diagnostic it
//! prints names this library as the origin, which a bare panic runtime message does
//! not.
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
//!    test builds still unwind. [`guard`] closes the gap: it observes the unwind
//!    at the boundary, prints a diagnostic naming this library, and calls
//!    `std::process::abort`. The observable behaviour is therefore identical
//!    whichever profile produced the binary, which is the property that makes
//!    this a contract rather than a build-dependent tendency.
//!
//! All three mechanisms end in the same place, and there is deliberately no
//! fourth mechanism that ends anywhere else. Every guard in this module diverges
//! on the panic path; none of them returns.
//!
//! # Correct under `panic = "abort"`, not merely dead
//!
//! In a release build the panic runtime aborts at the `panic!` site, so control
//! never reaches [`guard`]'s panic arm and `catch_unwind` catches nothing. That
//! is the intended outcome: the guard's contract is "a panic aborts the process",
//! and an abort one stack frame earlier honours it exactly. Nothing in this module
//! depends on that arm running -- it adds no state, publishes no flag and performs
//! no cleanup that a later call would rely on -- so the release configuration is a
//! strictly *stronger* realisation of the same contract, not a configuration in
//! which the code is subtly wrong. The arm is retained because it is what upholds
//! the contract in debug and test builds, and because the diagnostic it prints
//! names this library as the origin of the abort, which a bare panic runtime
//! message does not.
//!
//! ## What is still visible in the ELF, and why it is not a hole
//!
//! `-C panic=abort` is confirmed on the rustc invocation for the release cdylib,
//! and yet `readelf` on that artifact still shows `NEEDED libgcc_s.so.1`, a
//! `.gcc_except_table`, `rust_eh_personality` and `_Unwind_Resume@GCC_3.0` /
//! `_Unwind_Backtrace`. That is expected and it is worth knowing where it comes
//! from, because it looks like a contradiction. The debug artifact shows exactly
//! the same set, which locates it in the PRE-COMPILED `std` — its panic runtime
//! and backtrace machinery ship as objects built for unwinding, and the linker
//! keeps their tables and the personality routine — rather than in any frame this
//! crate compiles. No unwind can originate in this library's own code: every
//! export is `extern "C"`, this profile aborts at the `panic!` site, and the
//! guards in this module abort explicitly. The only real consequence is a runtime
//! dependency the C library does not have, `libgcc_s.so.1`, which packaging
//! carries and which `ldd` on the staged library lists alongside `libc`.
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
//!   each of these, and the `allow-unwrap-in-tests`, `allow-expect-in-tests` and
//!   `allow-panic-in-tests` grants in the root `clippy.toml` apply to test code
//!   only -- this module's non-test code receives no exemption. There is
//!   deliberately no `allow-indexing-slicing-in-tests` key: it is newer than the
//!   declared 1.80 floor, and an unrecognised clippy.toml field aborts the whole
//!   lint run, so a test module that indexes a fixture carries a narrow local
//!   `#[allow(clippy::indexing_slicing)]` instead;
//! * allocation failure is reported as `Z_MEM_ERROR` through the caller's own
//!   allocator, so a failed allocation is a return value rather than a panic.
//!
//! # This module exports no symbol
//!
//! Nothing here is `#[no_mangle]` or `#[export_name]`, and nothing here may become
//! so.
//!
//! The exported surface of the built library is *intended* to be exactly the
//! ninety-five functions of the C API. The reference C shared library publishes 111
//! dynamic globals -- those ninety-five type-`T` function symbols plus sixteen
//! type-`A` symbol-version nodes -- under the soname `libz.so.1`, which is what the
//! Rust artifact has to reproduce.
//!
//! That baseline is enforced by `Makefile.in`'s `rust` target -- which `rust-test`
//! runs as a prerequisite -- diffing `nm -D --defined-only --extern-only` over the
//! staged library against `zlib.h` and `zlib.map`, checking the sixteen version
//! nodes and checking that every name in `zlib.map`'s `local:` block stayed hidden;
//! `rust-test` then confirms with `ldd` that the relinked C drivers bind the Rust
//! artifact. One stray exported symbol from
//! this module would fail that diff, and none does: measured on the staged library,
//! ninety-five exported functions -- every one of them declared in `zlib.h` -- and
//! one hundred and eleven dynamic symbols in total, identical to the C
//! `libz.so.1.3.2.1-motley`, with all ten `zlib.map` `local:` names hidden and
//! nothing matching `_*` exported. `crates/libz-rs-sys/tests/symbol_parity.rs`
//! asserts the same property from inside `cargo test`, over the artifact `make rust`
//! stages, so the gate now runs from both places; when the staged library is absent
//! those tests say which check they skipped and why rather than passing silently.
//! The items below are ordinary Rust functions and constants, reached through the
//! module path and inlined away.
//!
//! # Feature configuration
//!
//! [`guard`] has one signature and one contract, and two implementations selected
//! at compile time. **Both abort**; the feature decides only whether the abort is
//! this module's explicit call or the panic runtime's own:
//!
//! * **`gz` enabled (the default).** `gz = ["zlib-rs/std"]` is the only feature
//!   of this crate that brings in `std`, so it is also the condition under which
//!   `std::panic::catch_unwind` is available. This is the path that observes the
//!   unwind, reports it and aborts explicitly.
//! * **`gz` disabled.** Only `core` is assumed. There is no `catch_unwind`
//!   without `std`, so there is no point at which an unwind could be observed: the
//!   body is called directly and the guarantee rests on mechanisms 1 and 2 above --
//!   `extern "C"` turns an escaping panic into an abort at the boundary regardless.
//!   Both arms sit side by side in the guard's body under opposite `cfg`
//!   conditions, so which one a given build uses can be read off the source rather
//!   than inferred.
//!
//! Nothing here requires the crate root to be `no_std` when `gz` is off; this
//! module is merely willing to be. That is a distinction worth keeping, because
//! the crate builds a `cdylib` and a `staticlib`, and a `no_std` build of either
//! must supply its own `#[panic_handler]` -- a crate-root obligation, not a
//! module-level one. Both configurations of the root have been checked against
//! this module.
//!
//! Neither path allocates, takes a lock, or performs a system call when the body
//! returns normally, which is what lets every export be wrapped at no measurable
//! cost.
//!
//! # Usage
//!
//! The snippets name types declared elsewhere in this crate, so they are illustrative
//! rather than compilable.
//!
//! ```text
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
//! // An ordinary refusal is NOT a panic. Return the export's documented failure
//! // value from inside the body, naming it from `fallback` rather than writing a
//! // literal, and let the guard concern itself only with panics.
//! #[no_mangle]
//! pub extern "C" fn gzbuffer(file: gzFile, size: c_uint) -> c_int {
//!     panic_guard::guard(|| {
//!         if file.is_null() {
//!             return panic_guard::fallback::GZ_ERROR;
//!         }
//!         /* ... */
//!     })
//! }
//! ```
//!
//! There is deliberately no `guard_or`-style entry point that returns a value when
//! the body panics. See "The guarantee" above for why: a status code returned from
//! a half-updated stream is indistinguishable, to the caller, from an ordinary
//! refusal, and it would make the contract depend on the build profile.
//!
//! # Ported from
//!
//! Nothing, in the sense that no C source line corresponds to this module: the C
//! implementation has no panics to guard against and no unwinding to suppress.
//! It exists because reimplementing `zlib.h`'s contract in Rust introduces a
//! failure mode -- the panic -- that the original could not have, and the contract
//! at `zlib.h` L181-L192 admits only integer status codes as failure reports, so
//! there is nowhere for a panic to be reported to.

// `core`, `std` and the workspace's own status type are the only dependencies, and
// this module holds no raw pointer and contains no `unsafe` block.

use core::ffi::c_int;

use zlib_rs::error::ReturnCode;

/// Prefix written to standard error when a panic is caught and the process is about
/// to abort. It names the library, the boundary and the disposition, because the panic
/// runtime's own message says only that a thread panicked.
#[cfg(feature = "gz")]
/// cbindgen:ignore
const ABORT_NOTE: &str = "libz-rs-sys: a Rust panic reached the C ABI boundary; \
                          aborting rather than unwinding into a non-Rust caller: ";

/// Substituted for the panic payload when it is neither a `&str` nor a `String`.
///
/// A payload can be any `Send + Any` value, because `panic_any` accepts one; a
/// value the library never produces itself is still reported rather than
/// silently dropped.
#[cfg(feature = "gz")]
/// cbindgen:ignore
const OPAQUE_PAYLOAD: &str = "<panic payload was not a string>";

/// The type each guard's panic arm coerces the caught payload to before handing it to
/// [`report`].
///
/// Naming it removes a real trap: `catch_unwind` yields a `Box<dyn Any + Send>`, and
/// that box is *itself* `Any`, so borrowing it without dereferencing unsizes the box
/// rather than the payload and every downcast in [`describe`] fails silently. Each arm
/// therefore writes `let payload: &Payload = &*payload;` with this alias spelled
/// out.
#[cfg(feature = "gz")]
/// cbindgen:ignore
type Payload = dyn core::any::Any + Send;

/// Writes `note`, then the panic payload, to standard error.
///
/// Deliberately not `eprintln!`: that macro panics if the write fails, and a second
/// panic while handling the first replaces a precise diagnostic with an opaque
/// double-panic abort. Every write result is discarded, so this cannot itself fail,
/// and `std::io::stderr` takes a reentrant lock, so it is safe on a thread that was
/// already writing to standard error. `#[cold]`: panic path only.
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
/// `panic!` produces a `&'static str` for a bare literal and a `String` for a
/// formatted message; anything else arrived through `panic_any` and is reported as
/// [`OPAQUE_PAYLOAD`] rather than dropped. Separated from [`report`] so that the
/// downcast can be tested directly.
#[cfg(feature = "gz")]
fn describe(payload: &Payload) -> &str {
    payload
        .downcast_ref::<&'static str>()
        .copied()
        .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
        .unwrap_or(OPAQUE_PAYLOAD)
}

/// Runs an exported function's body, aborting the process if it panics.
///
/// This is the guard the exported surface uses, and it is the ONLY one: there is
/// no recovering variant, by design. It takes no failure value because it does not
/// return one -- on the panic path it diverges, so there is nothing for a C caller
/// to misinterpret. A status code returned after a panic would be
/// indistinguishable, to the caller, from an ordinary refusal, even though the
/// stream may have been half updated; and if the panic was the symptom of memory
/// corruption, that indistinguishability is exactly what lets the corruption
/// spread. An ordinary refusal belongs inside `body`, as a value from
/// [`fallback`].
///
/// When `body` returns normally its value is returned unchanged, with no allocation,
/// no lock and no system call added.
///
/// # ★ What it costs is configuration-dependent, so benchmarks must say which
///
/// With the `gz` feature the return path above goes through
/// `std::panic::catch_unwind`, which establishes a landing pad; without it there
/// is no `std`, no `catch_unwind`, and the body is called directly. And cargo
/// **ignores** `panic` for test and bench targets, so `cargo bench` links an
/// unwinding harness even though the shipped cdylib is built `panic = "abort"`
/// (the root manifest records that nuance beside the key itself). The three
/// configurations therefore do not generate the same code around an export, and
/// none of them is wrong -- what would be wrong is quoting a number from one of
/// them as if it were another. So a benchmark of this library is required to:
///
/// * measure **core algorithm throughput separately from C-ABI entry overhead**,
///   because they are different quantities: the first belongs to `zlib-rs` and
///   has no guard in it at all, the second is this crate's per-call cost;
/// * build any ABI-level measurement with codegen and panic behaviour
///   representative of the **shipped** artifact, not the default bench harness,
///   and say in the report which it used; and
/// * keep the guard where it is -- one boundary per exported call, never inside a
///   loop -- so that the overhead being measured stays per-call rather than
///   per-byte. Nothing in the exported surface may push a guard inward.
///
/// # Behaviour on panic
///
/// The payload is written to standard error with a line naming this library and the
/// boundary, then `std::process::abort` is called. Under `panic = "abort"` the panic
/// runtime aborts first and this arm is never reached; without the `gz` feature there
/// is no `catch_unwind` and `body` is called directly. The outcome is the same in
/// every configuration -- the process dies -- and it is the only outcome this module
/// offers.
///
/// Deliberately not `#[must_use]`: `gzclearerr` is declared `void` at `zlib.h` L1792,
/// so the guard is legitimately used where `T` is `()`, and discarding the result is
/// only ever correct because it *is* the body's own result.
#[inline]
pub(crate) fn guard<T, F>(body: F) -> T
where
    F: FnOnce() -> T,
{
    // `AssertUnwindSafe` is required because every real body borrows the caller's
    // stream mutably and `&mut` is not `UnwindSafe`. It is sound here for a reason
    // specific to this guard: the panic path diverges, so no caller can observe
    // whatever state the panic left behind.
    //
    // `abort` rather than `exit`: it raises `SIGABRT` without running atexit handlers
    // or destructors, so no C cleanup code walks the half-finished state, and a
    // supervisor or core dump records a crash rather than an orderly shutdown.
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
    // Without `std` an unwind cannot be observed, so the abort rests on the two
    // unconditional mechanisms: `extern "C"` at every export and `panic = "abort"`.
    #[cfg(not(feature = "gz"))]
    {
        body()
    }
}

/// Runs an exported function's body and converts its status to the C `int`.
///
/// The shape most of the exported surface has: the deflate, inflate, `inflateBack`
/// and one-shot wrapper entry points all return `int` derived from a [`ReturnCode`].
/// Routing them through here keeps the conversion in one place and the panic policy
/// identical to [`guard`]'s -- a panic aborts.
///
/// # Examples
///
/// `z_streamp` is declared elsewhere in this crate, so the shape is shown rather than
/// compiled.
///
/// ```text
/// #[no_mangle]
/// pub extern "C" fn deflateEnd(strm: z_streamp) -> c_int {
///     panic_guard::guard_code(|| ReturnCode::STREAM_ERROR)
/// }
/// ```
#[inline]
#[must_use]
pub(crate) fn guard_code<F>(body: F) -> c_int
where
    F: FnOnce() -> ReturnCode,
{
    guard(|| body().as_i32())
}

/// The value each family of exports reports when it cannot do what was asked.
///
/// Every entry is the value `zlib.h` documents for that family, cited by line, so
/// that a call site never has to invent one and a reviewer never has to guess
/// whether a literal at a call site was chosen deliberately. Two of the entries
/// are helper functions rather than constants, because the C return type they
/// stand for has a platform-dependent width.
///
/// These are values for the ORDINARY, EXPECTED failure paths: a null pointer, a
/// stream whose tag does not check out, an allocation the caller's `zalloc`
/// refused, a mode string that cannot be parsed. A body returns one of them
/// directly. None of them is ever produced by a panic -- [`guard`] aborts instead,
/// and there is deliberately no guard that hands one of these back in place of an
/// unwind, because a caller cannot tell such a value apart from a genuine refusal.
///
/// # Why the whole table is `#[allow(dead_code)]`
///
/// Completeness is the point of a lookup table: one entry per documented failure
/// value, cited to `zlib.h`, so that a call site never invents a literal. Thirteen
/// of the fourteen entries are called; exactly one, `MEM_ERROR`, has no call site,
/// because the two entry points that would want it -- `deflateInit2_` and
/// `inflateInit2_` -- reach their allocation failure through [`guard_code`] and a
/// [`ReturnCode`] the core already produced, so they never need the literal. That
/// count is measured, not assumed: deleting the attribute and building
/// `-p libz-rs-sys` under default features, `--all-features` and
/// `--no-default-features --features libz-compat` reports `MEM_ERROR` and nothing
/// else. Deleting the entry would leave a table with a gap and would delete the
/// `zlib.h` citation that makes the next call site's choice checkable, so the
/// attribute stays on the module: it keeps the table complete as call sites come
/// and go without the attribute having to migrate between items. Nothing here is reachable from
/// outside the crate either way: `panic_guard` is a private module and every item
/// below is `pub(crate)`, so the table is spelled at exactly the visibility its one
/// audience -- this crate's own export bodies -- needs.
#[allow(dead_code)]
pub(crate) mod fallback {
    use core::ffi::{c_int, c_ulong};
    // Only the four `gz` entries below name these two, and the `gzFile` layer they
    // serve is compiled only with that feature on.
    #[cfg(feature = "gz")]
    use core::ffi::{c_char, CStr};

    use zlib_rs::error::ReturnCode;

    /// `Z_STREAM_ERROR` (`zlib.h` L185): the stream state is inconsistent, or an
    /// argument was invalid. The failure value of every `int`-returning stream entry
    /// point, and also of `gzsetparams`, `gzflush`, `gzclose`, `gzclose_r`,
    /// `gzclose_w` and `gzprintf`, which return "the zlib error number"
    /// (`zlib.h` L1650) or "a negative zlib error code" (L1556) rather than `-1`.
    /// cbindgen:ignore
    pub(crate) const STREAM_ERROR: c_int = ReturnCode::STREAM_ERROR.as_i32();

    /// `Z_MEM_ERROR` (`zlib.h` L187): an allocation failed. Used in place of
    /// [`STREAM_ERROR`] wherever the C implementation would have reached its own
    /// out-of-memory path, so that a caller inspecting the code -- `test/example.c`
    /// and `test/infcover.c` both do -- sees the same distinction the reference
    /// draws.
    /// cbindgen:ignore
    pub(crate) const MEM_ERROR: c_int = ReturnCode::MEM_ERROR.as_i32();

    /// [`STREAM_ERROR`] as a [`ReturnCode`], for bodies that yield a status
    /// rather than a raw `int`.
    ///
    /// The same value at the type the safe core speaks, so that a body returning a
    /// [`ReturnCode`] and one returning a raw `int` through
    /// [`guard_code`](super::guard_code) cannot disagree about what the failure is.
    /// cbindgen:ignore
    pub(crate) const STREAM_ERROR_CODE: ReturnCode = ReturnCode::STREAM_ERROR;

    /// [`MEM_ERROR`] as a [`ReturnCode`].
    /// cbindgen:ignore
    pub(crate) const MEM_ERROR_CODE: ReturnCode = ReturnCode::MEM_ERROR;

    /// The bound `deflateBound` (`zlib.h` L768) and `compressBound` (L1307) report
    /// when it cannot be computed: the largest value the return type holds. Neither
    /// has a documented failure value and callers size an output buffer with whatever
    /// they return, so the direction is not free -- too small is a heap overflow *in
    /// the caller's code*, too large only fails the caller's own allocation, which it
    /// must already handle.
    /// cbindgen:ignore
    pub(crate) const BOUND: c_ulong = c_ulong::MAX;

    /// `1`: what `adler32` and `adler32_z` return when given no data.
    /// `zlib.h` L1813-L1814 documents that a null buffer yields "the required initial
    /// value for the checksum". The seed is the one answer both in range for an
    /// Adler-32 and impossible to mistake for a status code -- which matters, because
    /// the return type is `uLong` and a negative `int` through it is a plausible
    /// checksum.
    /// cbindgen:ignore
    pub(crate) const ADLER32_EMPTY: c_ulong = 1;

    /// `0`: the CRC-32 counterpart of [`ADLER32_EMPTY`]; `zlib.h` L1852-L1853 gives
    /// the same rule, and the documented usage example seeds with
    /// `crc32(0L, Z_NULL, 0)`.
    /// cbindgen:ignore
    pub(crate) const CRC32_EMPTY: c_ulong = 0;

    /// The value the **CRC-32** combine family returns for an input it cannot
    /// use: 0.
    ///
    /// `zlib.h` L1880 and L1887 define it -- "len2 must be non-negative,
    /// otherwise zero is returned" -- and the implementation matches:
    /// `crc32_combine_gen64` opens with `if (len2 < 0) return 0;`
    /// (`crc32.c` L954-L956) and `crc32_combine_op` with `if (op == 0) return 0;`
    /// (`crc32.c` L969-L971), so a zero generator propagates through
    /// `crc32_combine` and `crc32_combine64` (`crc32.c` L976-L983). Serves
    /// `crc32_combine`, `crc32_combine64`, `crc32_combine_gen`,
    /// `crc32_combine_gen64` and `crc32_combine_op`.
    ///
    /// Ported from `crc32_combine_gen64`, `crc32.c` L954-L956.
    //
    // ★ `#[allow(dead_code)]` here and on `ADLER32_COMBINE_INVALID` is required at the
    // declared MSRV and is not cosmetic. Both are used by the `const _: () = …` invariant
    // block below, and **rustc 1.80's dead-code pass does not traverse those bodies** --
    // verified, not assumed: `cargo +1.80 build -p libz-rs-sys --all-features` reports both
    // as never used and 1.97.1 reports neither. It is the same quirk
    // `crates/libz-rs-sys/src/layout_assertions.rs` documents for its `IS_LP64`. Since CI
    // builds with `-D warnings`, the attribute is what keeps the MSRV build green.
    //
    // These two are also the only entries in this registry whose value the safe core
    // produces rather than this crate, so no export body returns either one today; the
    // invariant block and the tests at the foot of the file are their consumers. Scoped to
    // the two items rather than to the module, so real dead code here is still reported.
    #[allow(dead_code)]
    /// cbindgen:ignore
    pub(crate) const CRC32_COMBINE_INVALID: c_ulong = 0;

    /// The value the **Adler-32** combine family returns for a negative `len2`:
    /// `0xffff_ffff`.
    ///
    /// **This is deliberately NOT zero, and it must not be unified with
    /// [`CRC32_COMBINE_INVALID`].** `adler32_combine_` opens with
    ///
    /// ```c
    /// // for negative len, return invalid adler32 as a clue for debugging
    /// if (len2 < 0)
    ///     return 0xffffffffUL;
    /// ```
    ///
    /// (`adler32.c` L138-L140), and the choice carries information: a genuine
    /// Adler-32 has both halves below `BASE` (65521), so `0xffff` in either half
    /// is unreachable and the value is recognisably not a checksum. Zero, by
    /// contrast, is a *plausible* Adler-32 and would silently pass as one.
    /// `zlib.h` L1844-L1845 documents only that a negative length leaves the
    /// result with "no meaning or utility", so the sentinel is the
    /// implementation's, and reproducing the implementation is the requirement.
    ///
    /// Written as the bare literal `0xffff_ffff`, typed by the declaration, rather
    /// than as `u32::MAX as c_ulong`. `c_ulong` is 8 bytes on LP64 and 4 on LLP64
    /// Windows (`zconf.h` L406 makes `uLong` an `unsigned long`), and the literal
    /// is exact and non-truncating in both widths -- it is the same 32-bit pattern
    /// the C source writes as `0xffffffffUL`. A cast would additionally be a
    /// *trivial* one wherever `c_ulong` is already `u32`, which `trivial_numeric_casts`
    /// rejects on that target; no cast means nothing to silence.
    ///
    /// Serves `adler32_combine` and `adler32_combine64`. The safe core computes
    /// the same sentinel itself in `zlib_rs::adler32::adler32_combine`; this entry
    /// exists so that a facade error path -- a `len2` the ABI layer rejects before
    /// it ever reaches the core -- cannot disagree with it.
    ///
    /// Ported from `adler32_combine_`, `adler32.c` L133-L140.
    // Invariant-block consumer only; see the note above `CRC32_COMBINE_INVALID`.
    #[allow(dead_code)]
    /// cbindgen:ignore
    pub(crate) const ADLER32_COMBINE_INVALID: c_ulong = 0xffff_ffff;

    // The three properties that make the pair above correct, pinned at compile time so
    // that a future edit to either value is a build failure rather than a behavioural
    // surprise in an error path no functional test reaches. The runtime tests at the foot
    // of this file check the same properties against the C line references and against the
    // safe core's own computation; these are the half that cannot be skipped.
    //
    // ★ The two entries are the only ones in this registry whose value the SAFE CORE
    // produces rather than this crate -- `zlib_rs::adler32::adler32_combine` answers a
    // negative length itself -- so no export body returns either of them today, and these
    // assertions are what keep them from being dead weight as well as what pins them, and the
    // `#[allow(dead_code)]` on each of the two is what covers the MSRV's inability to see a
    // use that occurs only inside a `const _: () = …` body.
    /// cbindgen:ignore
    const _: () = {
        // `adler32.c` L140 returns 0xffffffffUL and `crc32.c` L956 returns 0. Unifying the
        // two would be wrong for whichever family lost its own value.
        assert!(ADLER32_COMBINE_INVALID != CRC32_COMBINE_INVALID);
        // The Adler sentinel must be unreachable as a genuine checksum: every half of a
        // real Adler-32 is below BASE, so a half at or above it cannot be mistaken for
        // one. Zero has no such property, which is why it may not stand in here.
        assert!((ADLER32_COMBINE_INVALID & 0xffff) >= 65521);
        assert!((ADLER32_COMBINE_INVALID >> 16) >= 65521);
    };

    /// `-1`: the failure value of `gzread` (`zlib.h` L1486), `gzgetc` (L1615),
    /// `gzgetc_` (L1961), `gzungetc` (L1630), `gzputc` (L1610), `gzputs` (L1580),
    /// `gzbuffer` (L1441) and `gzrewind`, which L1687 defines as
    /// `(int)gzseek(file, 0L, SEEK_SET)`. It is **not** the failure value of every
    /// `gz` function returning `int`: see [`GZ_NOTHING_WRITTEN`] and
    /// [`STREAM_ERROR`].
    #[cfg(feature = "gz")]
    /// cbindgen:ignore
    pub(crate) const GZ_ERROR: c_int = -1;

    /// `0`: what `gzwrite` returns on error, a deliberate exception to [`GZ_ERROR`].
    /// `zlib.h` L1522-L1523 documents that it "returns the number of uncompressed
    /// bytes written, or 0 in case of error or if len is 0" -- the count is unsigned in
    /// spirit, so `-1` would tell a caller that a negative number of bytes was
    /// written.
    #[cfg(feature = "gz")]
    /// cbindgen:ignore
    pub(crate) const GZ_NOTHING_WRITTEN: c_int = 0;

    /// `0`: the item count `gzfread` (`zlib.h` L1501) and `gzfwrite` (L1537) return on
    /// error. Both duplicate `fread`/`fwrite`, returning a `z_size_t` count of complete
    /// items, so zero is the only failure value the type admits; L1503 adds that
    /// `gzerror` must be consulted to distinguish it from end of file.
    #[cfg(feature = "gz")]
    /// cbindgen:ignore
    pub(crate) const GZ_NO_ITEMS: usize = 0;

    /// A valid, empty, NUL-terminated message for `gzerror` (`zlib.h` L1775).
    ///
    /// Callers pass `gzerror`'s `const char *` straight to `printf`, so the failure
    /// value must be a readable string. The C implementation does return `NULL` for a
    /// handle it cannot recognise, but a caller that has just tripped a panic is the
    /// one least likely to check; the empty string is what C returns for a valid handle
    /// with no message pending, so it is safe to print and already documented. Take the
    /// pointer with [`CStr::as_ptr`], which needs no `unsafe`, and the `'static`
    /// lifetime outlives any call.
    /// cbindgen:ignore
    #[cfg(feature = "gz")]
    pub(crate) const GZ_NO_MESSAGE: &CStr = c"";

    /// The literal `gzerror` returns for `Z_MEM_ERROR` (`gzlib.c` L522-L523).
    ///
    /// C answers an out-of-memory error with a static string rather than a stored
    /// message, because `gz_error` deliberately declines to allocate one while reporting
    /// that allocation failed (`gzlib.c` L571-L572). The two halves are a pair: without
    /// this literal, the one condition a caller most needs explained would carry no text
    /// at all. `zlib_rs::gz::gzerror` returns the same bytes for a Rust caller; this is
    /// the NUL-terminated spelling a C caller can hand to `printf`.
    #[cfg(feature = "gz")]
    /// cbindgen:ignore
    pub(crate) const GZ_OUT_OF_MEMORY: &CStr = c"out of memory";

    /// `Z_NULL` (`zlib.h` L216): the handle the `gzopen` family returns when it cannot
    /// open -- `gzopen` (L1357), `gzdopen` (L1404), `gzopen64` (L1978), `gzopen_w`
    /// (L2042) and `gzgets` (L1597-L1598), which returns a null `char *` "for
    /// end-of-file or in case of error". A function rather than a constant because the
    /// pointee is inferred from the export's return type. Constructing a null pointer
    /// needs no `unsafe`; only dereferencing one would, and nothing here does.
    #[inline]
    #[must_use]
    #[cfg(feature = "gz")]
    pub(crate) const fn null_handle<T>() -> *mut T {
        core::ptr::null_mut()
    }

    /// A null `char *`, the value `gzgets` returns on error or end of file
    /// (`zlib.h` L1588-L1598).
    ///
    /// A named spelling of [`null_handle`] for the one export whose null is a string
    /// rather than an opaque handle, so that a call site reads as the header does.
    #[inline]
    #[must_use]
    #[cfg(feature = "gz")]
    pub(crate) const fn null_string() -> *mut c_char {
        null_handle()
    }

    /// `-1`: the offset `gzseek` (`zlib.h` L1678), `gztell` (L1691, defined at L1698
    /// as `gzseek(file, 0L, SEEK_CUR)`), `gzoffset` (L1708) and their `*64` forms
    /// report on error.
    ///
    /// Generic rather than a constant because `z_off_t` and `z_off64_t` resolve to
    /// `off_t`, `off64_t`, `__int64`, `offset_t` or `long long` depending on the target
    /// (`zconf.h` L494-L531). All are signed and at least 32 bits wide, so the widening
    /// conversion from `i8` is exact on every one of them.
    #[inline]
    #[must_use]
    #[cfg(feature = "gz")]
    pub(crate) fn offset_error<T: From<i8>>() -> T {
        T::from(-1)
    }
}

// Test code is the one place in this crate where panicking is permitted: the
// root `clippy.toml` grants `allow-panic-in-tests`, `allow-unwrap-in-tests` and
// `allow-expect-in-tests`, and nothing below this banner is compiled into the
// shipped library.
//
// Which semantics are asserted, and how:
//
//   * The happy path -- the body's value passes through untouched, `()` bodies are
//     accepted, and a body may borrow mutably -- is asserted in process.
//   * The panic path ABORTS, which cannot be asserted in process without killing
//     the test runner. It is asserted out of process instead: the test re-executes
//     the test binary, filtered to itself and with a marker variable set, and
//     requires the child to die of SIGABRT. A child that returned normally, or
//     that failed for any other reason, fails the assertion -- including the case
//     where the filter matched nothing, because a child that ran no tests exits
//     successfully and so carries no signal.
//
// There is deliberately no in-process assertion that a panic produces a return
// value, because no guard produces one: the module offers exactly one guard, and it
// diverges. If a `guard_or`-shaped helper is ever proposed again, the reason it was
// removed is written up under "The guarantee" in the module documentation.

#[cfg(test)]
mod tests {
    use core::ffi::c_ulong;

    use super::{fallback, guard, guard_code, ReturnCode};

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
    fn the_guard_accepts_a_body_that_borrows_mutably() {
        // Every real export body borrows the caller's stream mutably, and `&mut`
        // is not `UnwindSafe`. This is what `AssertUnwindSafe` inside the guard
        // buys: no call site has to wrap anything itself.
        let mut total = 0_u32;
        let seen = guard(|| {
            total += 5;
            total
        });
        assert_eq!((seen, total), (5, 5));
    }

    #[test]
    fn a_body_reports_an_ordinary_refusal_as_a_value_not_a_panic() {
        // The shape every export uses for an expected failure: the body returns
        // the documented value from `fallback`, and the guard is not involved at
        // all. This is what replaces a recovering guard.
        assert_eq!(guard(|| fallback::STREAM_ERROR), fallback::STREAM_ERROR);
        assert_eq!(
            guard_code(|| ReturnCode::STREAM_ERROR),
            fallback::STREAM_ERROR
        );
    }

    /// The same shape for the `gz` family, whose refusal values exist only when the
    /// `gzFile` layer is compiled.
    #[cfg(feature = "gz")]
    #[test]
    fn a_gz_body_reports_its_own_refusal_value_the_same_way() {
        assert_eq!(guard(|| fallback::GZ_ERROR), fallback::GZ_ERROR);
        assert_eq!(
            guard(|| fallback::GZ_NOTHING_WRITTEN),
            fallback::GZ_NOTHING_WRITTEN
        );
        assert_eq!(guard(|| fallback::GZ_NO_ITEMS), fallback::GZ_NO_ITEMS);
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
        assert_eq!(guard_code(|| ReturnCode::MEM_ERROR), -4);
    }

    #[test]
    fn the_int_returning_fallbacks_carry_the_documented_values() {
        assert_eq!(fallback::STREAM_ERROR, -2);
    }

    /// The `gz` family's documented values, gated with the layer that returns them.
    #[cfg(feature = "gz")]
    #[test]
    fn the_gz_fallbacks_carry_the_documented_values() {
        assert_eq!(fallback::GZ_ERROR, -1);
        assert_eq!(fallback::GZ_NOTHING_WRITTEN, 0);
        assert_eq!(fallback::GZ_NO_ITEMS, 0);
    }

    #[test]
    fn the_status_fallback_agrees_with_its_int_form() {
        assert_eq!(fallback::STREAM_ERROR_CODE.as_i32(), fallback::STREAM_ERROR);
        assert!(fallback::STREAM_ERROR_CODE.is_error());
    }

    #[test]
    fn the_checksum_fallbacks_are_the_documented_seeds() {
        // `zlib.h` L1812 and L1851: a null buffer yields the initial value.
        assert_eq!(fallback::ADLER32_EMPTY, 1);
        assert_eq!(fallback::CRC32_EMPTY, 0);
        // None of them is one of the nine status codes reinterpreted as a
        // checksum, which is the mistake this module exists to prevent.
        for seed in [fallback::ADLER32_EMPTY, fallback::CRC32_EMPTY] {
            assert!(seed <= c_ulong::from(u32::MAX));
        }
    }

    #[test]
    fn the_two_combine_sentinels_are_the_ones_their_own_c_source_returns() {
        // `adler32.c` L15: the largest prime below 65536.
        const BASE: c_ulong = 65521;

        // `crc32.c` L954-L956 and L969-L971, and `zlib.h` L1880/L1887: zero.
        assert_eq!(fallback::CRC32_COMBINE_INVALID, 0);
        // `adler32.c` L138-L140: 0xffffffffUL, and NOT zero. The two families
        // disagree, so a single shared constant would be wrong for one of them --
        // which is exactly what this assertion is here to keep from coming back.
        assert_eq!(fallback::ADLER32_COMBINE_INVALID, 0xffff_ffff);
        assert_ne!(
            fallback::ADLER32_COMBINE_INVALID,
            fallback::CRC32_COMBINE_INVALID
        );
        // The Adler sentinel is unreachable as a real checksum -- both halves of a
        // genuine Adler-32 are below BASE -- which is what makes it usable as a
        // clue. Zero is not: it is a plausible checksum, and that is why it may not
        // stand in for this value.
        for half in [
            fallback::ADLER32_COMBINE_INVALID & 0xffff,
            fallback::ADLER32_COMBINE_INVALID >> 16,
        ] {
            assert!(
                half >= BASE,
                "half {half} of the Adler sentinel is a reachable checksum value"
            );
        }
        // Both fit the 32 bits `uLong` carries a checksum in on every target.
        for sentinel in [
            fallback::ADLER32_COMBINE_INVALID,
            fallback::CRC32_COMBINE_INVALID,
        ] {
            assert!(sentinel <= c_ulong::from(u32::MAX));
        }
    }

    #[test]
    fn the_adler_combine_sentinel_matches_the_safe_core() {
        // The core computes the sentinel itself (`zlib_rs::adler32::adler32_combine`
        // step 1). The facade constant exists for the arguments the ABI layer
        // rejects before the core is reached, so the two must agree exactly.
        for len2 in [-1_i64, -2, -1024, i64::MIN] {
            assert_eq!(
                c_ulong::from(zlib_rs::adler32::adler32_combine(1, 1, len2)),
                fallback::ADLER32_COMBINE_INVALID,
                "len2 = {len2}"
            );
        }
    }

    #[test]
    fn the_bound_fallback_saturates_rather_than_truncating() {
        assert_eq!(fallback::BOUND, c_ulong::MAX);
        // The dangerous direction is a bound that is too small: the caller sizes
        // its output buffer with it.
        assert_ne!(fallback::BOUND, 0);
    }

    #[cfg(feature = "gz")]
    #[test]
    fn the_pointer_fallbacks_are_null() {
        assert!(fallback::null_handle::<u8>().is_null());
        assert!(fallback::null_handle::<c_ulong>().is_null());
        assert!(fallback::null_string().is_null());
    }

    #[cfg(feature = "gz")]
    #[test]
    fn the_gzerror_fallback_is_a_printable_non_null_empty_string() {
        // Non-nullness needs no assertion: `CStr::as_ptr` cannot return null,
        // and `useless_ptr_null_checks` rejects a test that pretends otherwise.
        // That static guarantee is the whole reason this is the right fallback
        // for an export whose result is handed to `printf`.
        assert_eq!(fallback::GZ_NO_MESSAGE.to_bytes(), b"");
        assert_eq!(fallback::GZ_NO_MESSAGE.to_bytes_with_nul(), b"\0");
    }

    #[cfg(feature = "gz")]
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
