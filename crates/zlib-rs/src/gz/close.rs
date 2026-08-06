//! The `gzclose` dispatch: the port of `gzclose.c` (23 lines, the smallest translation unit in the
//! whole `gzFile` layer).
//!
//! Exactly one function lives here, and it decides exactly one thing: whether the handle being
//! closed is a read handle or a write handle. The teardown itself -- ending the engine, returning
//! both working buffers to the allocator that produced them, clearing the error, releasing the path
//! and closing the file -- belongs to [`gzclose_r`] and [`gzclose_w`], and **not one step of it is
//! repeated below**. That division of labour is C's, and reproducing it is the entire purpose of
//! this module.
//!
//! # Why a module of its own, for three lines of code
//!
//! `gzclose.c` L8-L10 answers this in its own words: `gzclose` "is in a separate file so that it is
//! linked in only if it is used. That way the other gzclose functions can be used instead to avoid
//! linking in unneeded compression or decompression routines." `zlib.h` L1766-L1772 states the
//! consequence for the application: `gzclose_r` and `gzclose_w` exist so that a program which only
//! reads, or only writes, can avoid pulling in the half it never calls, whereas "if `gzclose()` is
//! used, then both compression and decompression code will be included the application when linking
//! to a static zlib library."
//!
//! The boundary is therefore a **link-granularity optimisation, not an algorithmic one**. In C the
//! unit of linking is the translation unit, so isolating a three-line dispatch in its own object
//! file is the only way to express "pay for this only if you call it". Rust has no equivalent
//! granularity -- a crate is compiled as a whole -- and the same effect arrives from the linker and
//! the optimiser instead: rustc already emits one section per function, so `--gc-sections` and
//! link-time optimisation drop [`gzclose`], and with it whichever half it alone would have reached,
//! out of any final program that never calls it. The workspace release profile asks for fat LTO with
//! a single codegen unit precisely so that this is available.
//!
//! Keeping the dispatch in a module of its own consequently preserves the *intent* and the 1:1
//! mapping to the C source rather than the mechanism. A maintainer who finds three lines of code in
//! a file of its own should read this section before folding it into `read.rs` or `write.rs`: doing
//! so would lose the mapping to `gzclose.c`, and it would bury the dispatch inside one of the two
//! halves it is specifically meant to be independent of.
//!
//! # The `NO_GZCOMPRESS` variant, and what this port reports for it
//!
//! C compiles two different functions out of these 23 lines. `gzclose.c` L12 and L20-L22:
//!
//! ```c
//! #ifndef NO_GZCOMPRESS
//!     ...
//!     return state->mode == GZ_READ ? gzclose_r(file) : gzclose_w(file);
//! #else
//!     return gzclose_r(file);
//! #endif
//! ```
//!
//! With `NO_GZCOMPRESS` defined the function collapses to a bare call to `gzclose_r`, because a
//! build without the compressor has no `gzclose_w` to dispatch to -- and note that the null check of
//! L15-L16 disappears with it, since it sits inside the `#ifndef`.
//!
//! **This port implements the default configuration: both halves are present, and the dispatch
//! below is the real, two-way one.** That determination has a machine-visible consequence one crate
//! up, so it is recorded here rather than left to be re-derived: `zlibCompileFlags` sets bit 16 for
//! `NO_GZCOMPRESS` (`zutil.c` L76-L78, `flags += 1L << 16`), and therefore
//! `crates/libz-rs-sys/src/util.rs` **must report bit 16 CLEAR**. The same goes for the companion
//! decision in the read half: nothing in this port is compiled out, so the capability surface is the
//! full one.
//!
//! # The dispatch, and why it is deliberately asymmetric
//!
//! `gzclose.c` L19 is one ternary:
//!
//! ```c
//! return state->mode == GZ_READ ? gzclose_r(file) : gzclose_w(file);
//! ```
//!
//! The test is *only* "is the mode `GZ_READ`?". C does **not** additionally confirm that the mode is
//! `GZ_WRITE`, so a handle whose mode is `GZ_NONE`, or whose memory was never a `gzFile` at all,
//! goes down the **write** path -- where [`gzclose_w`]'s own `state->mode != GZ_WRITE` guard
//! (`gzwrite.c` L677-L678) is what rejects it. This port reproduces that exactly:
//!
//! | `state->mode` | Value | Branch taken | Outcome |
//! |---|---|---|---|
//! | `GZ_READ` | 7247 | [`gzclose_r`] | the read teardown |
//! | `GZ_WRITE` | 31153 | [`gzclose_w`] | the write teardown |
//! | `GZ_APPEND` | 1 | [`gzclose_w`] | rejected there with `Z_STREAM_ERROR` |
//! | `GZ_NONE` | 0 | [`gzclose_w`] | rejected there with `Z_STREAM_ERROR` |
//! | anything else | -- | [`gzclose_w`] | rejected there with `Z_STREAM_ERROR` |
//!
//! Turning this into a tidy three-way check -- read here, write there, error otherwise -- is
//! rejected on purpose, even though today it would return the same integer. Three reasons, in
//! increasing order of importance:
//!
//! 1. It would duplicate the write path's precondition. `Z_STREAM_ERROR` for a non-write handle is
//!    stated once, in [`gzclose_w`]; a second copy here is a second thing that has to stay in
//!    agreement with it.
//! 2. It would skip [`gzclose_w`]'s other entry work. This port's write half also validates the
//!    exposed `gzFile` prefix on the way in, and that check reports the same code for its own
//!    reasons; short-circuiting the call would silently drop it.
//! 3. Behavioural fidelity to the reference implementation is the governing constraint of this port,
//!    and a divergence that "happens to agree today" is precisely the kind that stops agreeing the
//!    moment one of the two halves changes. The oracle is the specification; tidiness is not.
//!
//! `GZ_APPEND` earns a row in the table even though no caller can observe it: `gz_open` sets it
//! while opening an appended file and overwrites it with `GZ_WRITE` before returning
//! (`gzlib.c` L269-L272). It is listed so that the fall-through is exhaustive on paper as well as in
//! the code.
//!
//! # What the return value means
//!
//! `zlib.h` L1758-L1760 documents five codes for `gzclose`, and every one of them can come out of
//! this dispatch:
//!
//! | Code | Meaning | Produced by |
//! |---|---|---|
//! | `Z_STREAM_ERROR` | the handle is not valid | this function, for an absent stream; otherwise the chosen half's mode guard |
//! | `Z_ERRNO` | a file-operation error | either half, when closing the file fails (`gzread.c` L667, `gzwrite.c` L696-L697) |
//! | `Z_MEM_ERROR` | out of memory | the write half, latched out of `state->err` while finishing the stream |
//! | `Z_BUF_ERROR` | the last read ended in the middle of a gzip stream | the read half, at `gzread.c` L662 |
//! | `Z_OK` | success | either half |
//!
//! ★ **The `Z_BUF_ERROR` case is the one to be careful with, because `gzclose` is the only place a
//! caller ever learns of it.** `zlib.h` L1474-L1478 records the design deliberately: `gzread` does
//! *not* report a truncated member -- it returns the short count it managed to produce and says
//! nothing -- and the report is deferred to close time. [`gzclose_r`] computes it at `gzread.c` L662,
//! `err = state->err == Z_BUF_ERROR ? Z_BUF_ERROR : Z_OK`, and returns it unless closing the file
//! failed as well. This function must therefore hand that status straight back, unaltered. A
//! dispatch that flattened it -- to `Z_OK` because the teardown itself succeeded, say -- would
//! destroy the only signal that the caller has just been handed truncated data.
//!
//! # The caller contract, and where each half of it is enforced
//!
//! `zlib.h` L1752-L1756 imposes two obligations, both about lifetime rather than about arguments:
//!
//! 1. **`gzclose` must not be called more than once on the same file**, "just as `free` must not be
//!    called more than once on the same allocation".
//! 2. **Once the file is closed, `gzerror` must not be called with it**, because its structures have
//!    been deallocated.
//!
//! In C, violating either is undefined behaviour: `gzread.c` L666 and `gzwrite.c` L698 both end in
//! `free(state)`, so every later access reads freed memory.
//!
//! On this side of the boundary neither can happen. [`GzState`] is an owned Rust value; its storage
//! is released by dropping it, its `Drop` runs the same teardown, and the close paths leave nothing
//! behind for that teardown to release a second time. A double close is therefore never *undefined*
//! here: [`gzclose_w`] sets `mode` to `GZ_NONE` before it returns, so a second call takes the write
//! arm again and comes straight back as `Z_STREAM_ERROR`, while a second call on a read handle finds
//! every step of [`gzclose_r`]'s teardown already done and is simply idempotent. Either way no
//! sequence of calls into this function can reach released memory or release anything twice -- which
//! is a stronger guarantee than `zlib.h` asks for, not a licence to rely on it.
//!
//! What Rust's ownership cannot reach is the raw `gzFile` a C caller holds. That pointer may be
//! null, may be stale, and may never have been a `gzFile` at all -- so the guard belongs at the
//! boundary, and `crates/libz-rs-sys/src/gz.rs` owns it (AAP §0.6.1 unsafe-site category 3: the
//! opaque state pointer is tag-validated before it is treated as state). This function's [`Option`]
//! parameter is that guard's safe-core counterpart, and [`None`] *is* `file == NULL`.
//!
//! # Both halves stay public
//!
//! `zlib.h` L1763-L1764 declares `gzclose_r` and `gzclose_w` as exported functions in their own
//! right, not as private helpers of `gzclose`, and the version script exports all three. Nothing
//! here narrows them: [`gzclose_r`] remains a public entry point of `crate::gz::read` and
//! [`gzclose_w`] a public entry point of `crate::gz::write`, so the facade can export the full trio
//! and an application can keep taking `zlib.h`'s advice about linking only the half it uses.
//!
//! # Feature gating
//!
//! The crate root reaches this file through `#[cfg(feature = "std")] mod gz;`, so no per-item gate
//! appears below and `--no-default-features` compiles the whole subtree out. Nothing in this module
//! needs `std` itself -- the dispatch is an integer comparison -- but it lives with the layer it
//! belongs to rather than being hoisted out to save one `cfg`.
//!
//! # Panic and allocation posture
//!
//! Nothing below panics and nothing below allocates. There is no indexing, no slicing, no
//! arithmetic, no `unwrap` and no `expect`; the single fallible conversion,
//! [`ReturnCode::from_i32`], is total, and its [`None`] arm is handled explicitly rather than
//! asserted away.

// Where each of these comes from, since none of it is a choice:
//
// * `Allocator` is not used directly; it is the bound without which `GzState<'a, A>` cannot be
//   named. `crate::gz::state` declares the type as `GzState<'a, A: Allocator<'a>>` and both close
//   halves require `A: Allocator<'a> + Copy`, so the signature below is unwriteable without it --
//   which is why `gz/state.rs`, `gz/read.rs` and `gz/write.rs` all import it too.
// * `ReturnCode` is the status this returns, and `ReturnCode::from_i32` is what turns `gzclose_w`'s
//   raw C `int` back into one.
// * `GzMode` is the exhaustive view over `state.mode()` that makes the dispatch a wildcard-free
//   `match`; `gz/state.rs` owns it.
// * `gzclose_r` and `gzclose_w` are the two halves being dispatched to, and they are the only
//   functions this module calls at all.
use crate::allocate::Allocator;
use crate::error::ReturnCode;
use crate::gz::read::gzclose_r;
use crate::gz::state::{GzMode, GzState};
use crate::gz::write::gzclose_w;

/// Closes a `gzFile`, dispatching to whichever teardown its direction calls for.
///
/// The port of `gzclose` (`gzclose.c` L11-L23), declared at `zlib.h` L1750 and specified at
/// L1752-L1760: flush all pending output for the file if necessary, close the file, and deallocate
/// the (de)compression state.
///
/// All of that work is [`gzclose_r`]'s or [`gzclose_w`]'s; this function only chooses between them,
/// and returns their answer unchanged. The module documentation covers why the choice is a module of
/// its own, why the test is deliberately not a three-way one, why `Z_BUF_ERROR` must survive the
/// hand-back, and where the double-close and stale-handle contracts of `zlib.h` L1752-L1756 are
/// enforced.
///
/// # The `state` argument
///
/// [`None`] is this port's spelling of C's `file == NULL`, and is rejected with
/// [`ReturnCode::STREAM_ERROR`] exactly as `gzclose.c` L15-L16 rejects a null `gzFile`. The check
/// belongs *here*, rather than being left to the caller, because it is genuinely part of the
/// dispatch: `state->mode` cannot be read to choose a branch until the handle is known to exist.
/// That is what distinguishes this function from [`gzclose_r`] and [`gzclose_w`], where the null
/// test and the mode test are adjacent and the boundary layer can perform both.
///
/// The facade holds a `*mut GzState` and produces this argument in one step with `as_mut`, which
/// yields precisely this [`Option`]. Converting the raw pointer, and refusing one that is stale or
/// foreign, stays the facade's job (AAP §0.6.1 unsafe-site categories 1 and 3).
///
/// # What is *not* done here
///
/// C frees the structure as its very last act (`gzread.c` L666, `gzwrite.c` L698). This port cannot
/// and must not: the structure was allocated by whoever owns it -- `crates/libz-rs-sys` -- and is
/// released by dropping it once this has returned. By that point the chosen half has already ended
/// the engine, returned both buffers, cleared the error and message, released the path and closed
/// the file, so [`GzState`]'s own `Drop` finds nothing left and the two cannot double-release.
///
/// # Returns
///
/// The status of whichever half ran, verbatim:
///
/// * [`ReturnCode::STREAM_ERROR`] if the handle is not valid -- absent here, or refused by the
///   chosen half's own mode guard.
/// * [`ReturnCode::ERRNO`] on a file-operation error.
/// * [`ReturnCode::MEM_ERROR`] if the write half ran out of memory finishing the stream.
/// * [`ReturnCode::BUF_ERROR`] if the last read ended in the middle of a gzip stream: the deferred
///   truncation report `gzread` itself declines to make (`zlib.h` L1474-L1478, `gzread.c` L662).
/// * [`ReturnCode::OK`] on success.
///
/// The status is `#[must_use]` because discarding it is almost always a defect: `gzclose` is where
/// the write path's final flush, the file close and the deferred truncation report all surface, so a
/// caller that ignores it cannot know whether the bytes it wrote actually reached the file.
/// `test/minigzip.c` treats it that way already -- `if (gzclose(out) != Z_OK) error("failed
/// gzclose");` at L360, L391 and L413.
#[must_use]
pub fn gzclose<'a, A: Allocator<'a> + Copy>(state: Option<&mut GzState<'a, A>>) -> ReturnCode {
    // `gzclose.c` L15-L16: `if (file == NULL) return Z_STREAM_ERROR;`. Nothing has been touched, so
    // there is nothing to tidy up on the way out.
    let Some(state) = state else {
        return ReturnCode::STREAM_ERROR;
    };

    // `gzclose.c` L19: `return state->mode == GZ_READ ? gzclose_r(file) : gzclose_w(file);`
    //
    // The match is written without a wildcard so that it is exhaustive over `Option<GzMode>` by
    // naming every inhabitant: adding a fifth direction to `GzMode` becomes a compile error here
    // rather than silently joining the write arm. `GzMode::from_raw` yielding `None` is a mode value
    // that is not a mode at all, which `gzguts.h` L158 describes as the point of the odd constants
    // -- "also provide a little integrity check on the passed structure".
    match GzMode::from_raw(state.mode()) {
        // The only branch C tests for.
        Some(GzMode::Read) => gzclose_r(state),

        // Everything else -- `GZ_WRITE`, the transient `GZ_APPEND`, the direction-less `GZ_NONE`,
        // and any value that is not a mode at all -- goes down the write path, because C's condition
        // is exactly "is it `GZ_READ`?" and nothing more. `gzclose_w`'s own
        // `state->mode != GZ_WRITE` guard (`gzwrite.c` L677-L678) is what produces the
        // `Z_STREAM_ERROR` for the last three; that rejection is deliberately *not* duplicated here.
        // See the module documentation for why this asymmetry is preserved rather than tidied.
        Some(GzMode::Write | GzMode::Append | GzMode::None) | None => {
            // `gzclose_w` reports the raw C `int`, because what it returns is whatever it latched
            // out of `state->err` (`gzwrite.c` L681-L686). Every value it can produce is one of the
            // nine documented codes, so the fallback below is unreachable in practice. It is
            // `STREAM_ERROR` -- the code zlib already uses for a state it does not recognise --
            // rather than an assertion, because aborting a C caller's process over an unrecognised
            // status would be a far worse outcome than reporting one. `unwrap_or` is not the denied
            // `unwrap`: it cannot panic, and it is the form `clippy::manual_unwrap_or` requires.
            ReturnCode::from_i32(gzclose_w(state)).unwrap_or(ReturnCode::STREAM_ERROR)
        }
    }
}

// -----------------------------------------------------------------------------
//  Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    // The crate denies the panic-prone lints in library code, which is the right policy there and
    // the wrong one here: an assertion that fails panics, and a fixture that cannot index is
    // unreadable. `clippy.toml` already relaxes them inside tests; restating it keeps the intent
    // local and visible. Scoped to this module, which ships in no build.
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

    // The fixtures reach past this module's own dependencies, and deliberately so:
    //
    // * `GlobalAllocator` is what `GzState::new` has to be given, and `crate::config`'s constants
    //   are the level and strategy a `"wb"` open installs -- writing `-1` and `0` as literals here
    //   would hide which knob each one is.
    // * `crate::inflate` decodes the sealed member in [`gunzip`] **instead of** `gzread`, which is
    //   the stronger check: it proves the bytes are a well-formed RFC 1952 member rather than merely
    //   something this port's own reader accepts. `gz/write.rs`'s tests take the same route for the
    //   same reason.
    //
    // Only the library half above is held to the narrow import set; nothing here ships.
    use super::gzclose;
    use crate::allocate::GlobalAllocator;
    use crate::config::{InflateConfig, Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_NO_FLUSH};
    use crate::error::ReturnCode;
    use crate::gz::read::gzread;
    use crate::gz::state::{
        GzHandle, GzIoError, GzSeekFrom, GzState, ZOff64, GZBUFSIZE, GZ_APPEND, GZ_NONE, GZ_READ,
        GZ_WRITE,
    };
    use crate::gz::write::gzwrite;
    use crate::inflate::{inflate, inflate_init2, inflate_reset, InflateStream};
    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::{Cell, RefCell};

    /// `"hello, hello!\0"` as a gzip member, produced by zlib itself at the default level.
    ///
    /// The exact payload `test/example.c`'s `test_gzio` writes -- `strlen(hello) + 1` bytes, the
    /// trailing zero included (L95 and L165) -- so that the close paths are exercised over the same
    /// stream the acceptance suite uses. Byte-for-byte the constant `gz/read.rs`'s own tests use.
    const HELLO_GZ: [u8; 31] = [
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xcb, 0x48, 0xcd, 0xc9, 0xc9,
        0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c, 0x00, 0x9d, 0x3f, 0x6c, 0xb5, 0x0e, 0x00, 0x00,
        0x00,
    ];

    /// The payload [`HELLO_GZ`] decompresses to.
    const HELLO: &[u8] = b"hello, hello!\0";

    /// How many bytes of [`HELLO_GZ`] still yield the whole payload while cutting the trailer short.
    ///
    /// 27 of 31: the deflate data is complete, so `gzread` delivers all 14 bytes, but the four-byte
    /// `ISIZE` of the RFC 1952 trailer is missing, so the member never reaches `Z_STREAM_END`. That
    /// is the exact shape `zlib.h` L1474-L1478 describes -- a read that "ended in the middle of a
    /// gzip stream" -- and the only way to reach the `Z_BUF_ERROR` of `gzread.c` L662.
    const TRUNCATED_LEN: usize = 27;

    /// The `errno` a failing close reports. Any non-zero value does; the number is Linux's `EIO`.
    const EIO: i32 = 5;

    /// An in-memory stand-in for the file behind a `gzFile`: this module's [`GzHandle`] fixture.
    ///
    /// It serves both directions, because the dispatch under test is precisely the thing that
    /// chooses between them: reads are served from `data`, writes are appended to `sink`, and
    /// `closes` counts the closes so the teardown can be asserted from outside the state that owns
    /// the handle. Deliberately free of any real I/O, so every test here runs under Miri.
    struct MemoryFile {
        /// Bytes a read stream consumes.
        data: Vec<u8>,
        /// How far through `data` reading has got.
        position: usize,
        /// Bytes a write stream produced, shared with the test because the state takes the handle
        /// away while closing.
        sink: Rc<RefCell<Vec<u8>>>,
        /// How many times the handle was closed.
        closes: Rc<Cell<usize>>,
        /// Whether `close` fails, which is how `Z_ERRNO` is reached (`gzread.c` L666-L667,
        /// `gzwrite.c` L696-L697).
        close_fails: bool,
    }

    impl GzHandle for MemoryFile {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, GzIoError> {
            let available = self.data.len() - self.position;
            let count = buf.len().min(available);
            buf[..count].copy_from_slice(&self.data[self.position..self.position + count]);
            self.position += count;
            Ok(count)
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
            self.closes.set(self.closes.get() + 1);
            if self.close_fails {
                return Err(GzIoError::new(EIO, false));
            }
            Ok(())
        }
    }

    /// The three pieces a test needs: the state, the bytes written to it, and the close counter.
    struct Fixture {
        state: GzState<'static, GlobalAllocator>,
        sink: Rc<RefCell<Vec<u8>>>,
        closes: Rc<Cell<usize>>,
    }

    impl Fixture {
        /// Runs the dispatch under test over this fixture's state.
        ///
        /// Every assertion goes through here rather than through [`gzclose_r`](super::gzclose_r) or
        /// [`gzclose_w`](super::gzclose_w) directly, which is the point: what is being tested is
        /// that the dispatch picks the right half and hands its status back untouched.
        fn close(&mut self) -> ReturnCode {
            gzclose(Some(&mut self.state))
        }
    }

    /// Builds a state around a [`MemoryFile`], leaving the direction to the caller.
    fn fixture(data: &[u8], close_fails: bool) -> Fixture {
        let sink = Rc::new(RefCell::new(Vec::new()));
        let closes = Rc::new(Cell::new(0));
        let mut state = GzState::new(GlobalAllocator);
        state.try_set_path(b"memory").unwrap();
        let previous = state.set_handle(Some(Box::new(MemoryFile {
            data: data.to_vec(),
            position: 0,
            sink: Rc::clone(&sink),
            closes: Rc::clone(&closes),
            close_fails,
        })));
        assert!(previous.is_none());
        Fixture {
            state,
            sink,
            closes,
        }
    }

    /// A read stream over `data`, in the condition `gz_open(path, "rb")` leaves one in.
    ///
    /// `direct` starts at 1, which is `gz_open`'s "start with a transparent assumption in case of an
    /// empty file" (`gzlib.c` L186-L189); `gz_look` overwrites it once it has seen the header.
    fn reader(data: &[u8]) -> Fixture {
        let mut fixture = fixture(data, false);
        fixture.state.set_mode(GZ_READ);
        fixture.state.set_direct(1);
        fixture.state.set_want(GZBUFSIZE);
        fixture
    }

    /// A write stream, in exactly the condition `gz_open(path, "wb")` leaves one in.
    ///
    /// `size` stays zero so that `gz_init` runs lazily on the first write, which is what
    /// `gzlib.c` L248 arranges.
    fn writer() -> Fixture {
        let mut fixture = fixture(&[], false);
        fixture.state.set_mode(GZ_WRITE);
        fixture.state.set_want(GZBUFSIZE);
        fixture.state.set_level(Z_DEFAULT_COMPRESSION);
        fixture.state.set_strategy(Z_DEFAULT_STRATEGY);
        fixture.state.set_direct(0);
        fixture
    }

    /// Decompresses a concatenation of gzip members by driving the inflate engine directly.
    ///
    /// Deliberately not routed through `gzread`: decoding with the engine proves the closed stream is
    /// a well-formed RFC 1952 member rather than merely something this port's own reader accepts.
    /// Each member ends with `Z_STREAM_END`, after which `inflate_reset` starts the next one.
    fn gunzip(bytes: &[u8]) -> Vec<u8> {
        if bytes.is_empty() {
            return Vec::new();
        }
        // 31 is `MAX_WBITS + 16`: the largest window, with the gzip wrapper selected.
        let mut decoder = inflate_init2(InflateConfig::new(31), GlobalAllocator).unwrap();
        let mut out = vec![0_u8; 1 << 16];
        let mut produced = 0_usize;
        let mut consumed = 0_usize;
        loop {
            if produced == out.len() {
                out.resize(out.len() * 2, 0);
            }
            let mut stream = InflateStream::new(bytes, &mut out);
            stream.next_in = consumed;
            stream.next_out = produced;
            let code = inflate(&mut decoder, &mut stream, Z_NO_FLUSH);
            consumed = stream.next_in;
            produced = stream.next_out;
            assert!(
                code == ReturnCode::OK || code == ReturnCode::STREAM_END,
                "inflate reported {code:?} after {produced} bytes"
            );
            if code == ReturnCode::STREAM_END {
                if consumed == bytes.len() {
                    break;
                }
                let _reset = inflate_reset(&mut decoder);
                continue;
            }
            assert!(
                consumed < bytes.len(),
                "inflate wants more input than the stream contains"
            );
        }
        out.truncate(produced);
        out
    }

    // -------------------------------------------------------------------------
    //  The absent stream: `gzclose.c` L15-L16
    // -------------------------------------------------------------------------

    #[test]
    fn a_missing_stream_is_rejected() {
        // `gzclose.c` L15-L16: `if (file == NULL) return Z_STREAM_ERROR;`. `None` is this port's
        // spelling of that null, and it must be answered before any mode is read.
        let absent: Option<&mut GzState<'_, GlobalAllocator>> = None;
        assert_eq!(gzclose(absent), ReturnCode::STREAM_ERROR);
    }

    // -------------------------------------------------------------------------
    //  The read arm: `state->mode == GZ_READ`
    // -------------------------------------------------------------------------

    #[test]
    fn closing_a_read_stream_returns_ok() {
        let mut fixture = reader(&HELLO_GZ);
        let mut buf = [0_u8; 64];
        assert_eq!(gzread(&mut fixture.state, &mut buf), 14);
        assert_eq!(&buf[..14], HELLO);

        assert_eq!(fixture.close(), ReturnCode::OK);
        assert_eq!(fixture.closes.get(), 1, "the file was closed exactly once");

        // The read half did the teardown, not this module: nothing is left to release.
        assert!(fixture.state.in_slice().is_empty());
        assert!(fixture.state.out_slice().is_empty());
        assert!(fixture.state.stream().engine.is_none());
        assert!(fixture.state.path().is_empty());
        assert!(!fixture.state.has_handle());
        assert_eq!(fixture.state.err(), ReturnCode::OK.as_i32());

        // ...and `size` is deliberately *not* cleared, which is the read half's faithful
        // reproduction of C: `gzclose_r` frees the structure immediately afterwards
        // (`gzread.c` L666), so the field has no reader left and C never bothers. That is the one
        // place the two halves differ -- `gzclose_w` does clear it, because it also has to leave
        // `mode` at `GZ_NONE` to make a second close refusable -- and it is pinned here so the
        // asymmetry cannot be "tidied" into a divergence from the oracle.
        assert_ne!(fixture.state.size(), 0);
        assert_eq!(fixture.state.mode(), GZ_READ);
    }

    #[test]
    fn closing_a_read_stream_that_read_nothing_returns_ok() {
        // `size == 0` means no buffer and no engine were ever created, which is the arm
        // `gzread.c` L656 guards. Closing straight after `gzopen` must still succeed.
        let mut fixture = reader(&HELLO_GZ);
        assert_eq!(fixture.state.size(), 0);
        assert_eq!(fixture.close(), ReturnCode::OK);
        assert_eq!(fixture.closes.get(), 1);
    }

    #[test]
    fn closing_a_read_stream_after_a_truncated_member_returns_buf_error() {
        // `zlib.h` L1474-L1478 and L1758-L1760: `gzread` does not report an incomplete stream, and
        // `gzclose` is the only place the caller ever learns of it. `gzclose_r` computes it at
        // `gzread.c` L662, and this dispatch must hand it back unaltered.
        //
        // This is also the proof that the read arm is genuinely taken: `Z_BUF_ERROR` is a code only
        // `gzclose_r` can produce, so seeing it here means the dispatch did not fall through to the
        // write half.
        let mut fixture = reader(&HELLO_GZ[..TRUNCATED_LEN]);
        let mut buf = [0_u8; 64];
        assert_eq!(
            gzread(&mut fixture.state, &mut buf),
            14,
            "the payload itself is complete; the trailer is not"
        );
        assert_eq!(fixture.state.err(), ReturnCode::BUF_ERROR.as_i32());

        assert_eq!(fixture.close(), ReturnCode::BUF_ERROR);
        assert_eq!(fixture.closes.get(), 1);
    }

    #[test]
    fn a_read_stream_whose_file_close_fails_reports_errno() {
        // `gzread.c` L667: `return ret ? Z_ERRNO : err;` -- the failed `close` outranks the status
        // the teardown computed, even when that status was the truncation report.
        let mut fixture = fixture(&HELLO_GZ[..TRUNCATED_LEN], true);
        fixture.state.set_mode(GZ_READ);
        fixture.state.set_direct(1);
        fixture.state.set_want(GZBUFSIZE);
        let mut buf = [0_u8; 64];
        assert_eq!(gzread(&mut fixture.state, &mut buf), 14);
        assert_eq!(fixture.state.err(), ReturnCode::BUF_ERROR.as_i32());

        assert_eq!(fixture.close(), ReturnCode::ERRNO);
        assert_eq!(fixture.closes.get(), 1);
    }

    // -------------------------------------------------------------------------
    //  The write arm: everything that is not `GZ_READ`
    // -------------------------------------------------------------------------

    #[test]
    fn closing_a_write_stream_returns_ok_and_finalises_the_member() {
        let mut fixture = writer();
        assert_eq!(gzwrite(&mut fixture.state, HELLO), 14);
        // Nothing has reached the file yet: the compressor is still holding the member open.
        assert!(fixture.sink.borrow().is_empty());

        assert_eq!(fixture.close(), ReturnCode::OK);
        assert_eq!(fixture.closes.get(), 1, "the file was closed exactly once");

        let produced = fixture.sink.borrow().clone();
        assert_eq!(
            &produced[..3],
            &[0x1f, 0x8b, 0x08],
            "an RFC 1952 member opens with the magic and the deflate method"
        );
        assert_eq!(
            gunzip(&produced),
            HELLO,
            "the member is complete: `Z_FINISH`, the CRC-32 and the ISIZE were all written"
        );
        // The write half did the teardown, and left the markers that refuse further work.
        assert_eq!(fixture.state.size(), 0);
        assert_eq!(fixture.state.mode(), GZ_NONE);
        assert!(fixture.state.path().is_empty());
        assert!(!fixture.state.has_handle());
    }

    #[test]
    fn closing_a_write_stream_pays_for_a_pending_forward_seek_first() {
        // `gzwrite.c` L680-L682: a `gzseek` past the end of a write stream only records `skip`, and
        // the zeros are written when something forces them out. `gzclose_w`'s `gz_zero` is the last
        // chance, so the zeros must land inside the member -- before the `Z_FINISH` that seals it.
        let mut fixture = writer();
        assert_eq!(gzwrite(&mut fixture.state, b"tail"), 4);
        fixture.state.set_skip(3);

        assert_eq!(fixture.close(), ReturnCode::OK);
        assert_eq!(gunzip(&fixture.sink.borrow()), b"tail\0\0\0");
        assert_eq!(fixture.closes.get(), 1);
    }

    #[test]
    fn closing_a_write_stream_that_wrote_nothing_still_produces_a_member() {
        // `gzclose_w` runs `gz_comp(state, Z_FINISH)` unconditionally (`gzwrite.c` L684-L685), so an
        // opened-and-immediately-closed write handle leaves a valid empty member behind -- which is
        // what `gzip` itself produces for empty input.
        let mut fixture = writer();
        assert_eq!(fixture.close(), ReturnCode::OK);
        let produced = fixture.sink.borrow().clone();
        assert!(!produced.is_empty());
        assert_eq!(&produced[..3], &[0x1f, 0x8b, 0x08]);
        assert!(gunzip(&produced).is_empty());
    }

    #[test]
    fn a_write_stream_whose_file_close_fails_reports_errno() {
        // `gzwrite.c` L696-L697: `if (close(state->fd) == -1) ret = Z_ERRNO;`
        let mut fixture = fixture(&[], true);
        fixture.state.set_mode(GZ_WRITE);
        fixture.state.set_want(GZBUFSIZE);
        fixture.state.set_level(Z_DEFAULT_COMPRESSION);
        fixture.state.set_strategy(Z_DEFAULT_STRATEGY);
        fixture.state.set_direct(0);
        assert_eq!(gzwrite(&mut fixture.state, HELLO), 14);

        assert_eq!(fixture.close(), ReturnCode::ERRNO);
        // The member was still finished and the buffers still released: a failed `close` does not
        // abandon the teardown (`gzwrite.c` L680-L697 latches `ret` and carries on).
        assert_eq!(gunzip(&fixture.sink.borrow()), HELLO);
        assert_eq!(fixture.state.size(), 0);
    }

    #[test]
    fn a_second_close_of_a_write_stream_is_refused() {
        // `zlib.h` L1755-L1756 forbids a second close outright; C would free the structure twice.
        // This port cannot, so the second call is merely rejected -- via the write arm again, because
        // `gzclose_w` left `mode` at `GZ_NONE`.
        let mut fixture = writer();
        assert_eq!(gzwrite(&mut fixture.state, HELLO), 14);
        assert_eq!(fixture.close(), ReturnCode::OK);

        assert_eq!(fixture.close(), ReturnCode::STREAM_ERROR);
        assert_eq!(
            fixture.closes.get(),
            1,
            "the file was not closed a second time"
        );
    }

    // -------------------------------------------------------------------------
    //  The fall-through: C tests only for `GZ_READ`
    // -------------------------------------------------------------------------

    #[test]
    fn a_direction_less_state_takes_the_write_path_and_is_rejected_there() {
        // The heart of the asymmetry. `gzclose.c` L19 asks only "is it `GZ_READ`?", so a `GZ_NONE`
        // state goes to `gzclose_w`, whose own `state->mode != GZ_WRITE` guard (`gzwrite.c`
        // L677-L678) is what answers `Z_STREAM_ERROR`. Nothing was written and nothing was closed,
        // which is how the rejection is distinguished from a successful teardown.
        let mut fixture = writer();
        fixture.state.set_mode(GZ_NONE);

        assert_eq!(fixture.close(), ReturnCode::STREAM_ERROR);
        assert!(fixture.sink.borrow().is_empty());
        assert_eq!(fixture.closes.get(), 0, "the write guard refused first");
        assert!(
            fixture.state.has_handle(),
            "a refused close tears nothing down"
        );
    }

    #[test]
    fn the_transient_append_mode_takes_the_write_path() {
        // `GZ_APPEND` is overwritten with `GZ_WRITE` before `gz_open` returns
        // (`gzlib.c` L269-L272), so no caller can present it; the row exists so the fall-through is
        // exhaustive in fact and not only on paper.
        let mut fixture = writer();
        fixture.state.set_mode(GZ_APPEND);
        assert_eq!(fixture.close(), ReturnCode::STREAM_ERROR);
        assert_eq!(fixture.closes.get(), 0);
    }

    #[test]
    fn a_mode_that_is_not_a_mode_at_all_takes_the_write_path() {
        // `GzMode::from_raw` yields `None`, which shares the write arm. This is the case
        // `gzguts.h` L158 has in mind when it calls the odd mode constants "a little integrity check
        // on the passed structure": the value cannot have come from this library.
        for bogus in [1_i32, -1, 7246, 7248, 31152, 31154, i32::MIN, i32::MAX] {
            let mut fixture = writer();
            fixture.state.set_mode(bogus);
            assert_eq!(
                fixture.close(),
                ReturnCode::STREAM_ERROR,
                "mode {bogus} must be refused by the write guard"
            );
            assert_eq!(fixture.closes.get(), 0);
        }
    }

    #[test]
    fn only_gz_read_reaches_the_read_half() {
        // A truncated member leaves `err` at `Z_BUF_ERROR`, and only `gzclose_r` translates that
        // into a return value (`gzread.c` L662). Presenting the very same state under a
        // non-`GZ_READ` mode must therefore *not* produce `Z_BUF_ERROR`, which pins the dispatch
        // from the other side.
        let mut fixture = reader(&HELLO_GZ[..TRUNCATED_LEN]);
        let mut buf = [0_u8; 64];
        assert_eq!(gzread(&mut fixture.state, &mut buf), 14);
        assert_eq!(fixture.state.err(), ReturnCode::BUF_ERROR.as_i32());

        fixture.state.set_mode(GZ_NONE);
        assert_eq!(fixture.close(), ReturnCode::STREAM_ERROR);
    }
}
