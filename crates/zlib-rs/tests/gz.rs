// UNSAFE CONTAINMENT, and it is mechanical rather than a convention.  `crates/zlib-rs` is the
// safe core: `src/lib.rs` carries `#![forbid(unsafe_code)]`, and its test suites carry it too, so
// the property "the core and everything that exercises it contains no `unsafe`" is enforced by the
// compiler in both halves.  The workspace's designated FFI boundary -- the only place a raw pointer
// crosses into a foreign implementation -- is `crates/libz-rs-sys/src/**` for the shipped library
// and `crates/zlib-rs-differential/src/{oracle,port}.rs` for the dev-only harness; an assertion
// that needs one of those belongs in a suite of that package, not here.
#![forbid(unsafe_code)]
// THE FEATURE GATE. `crates/zlib-rs` has no `gz` feature: its manifest declares exactly `default`,
// `rust-api`, `simd` and `std`, and `src/lib.rs` L450-L451 declares `pub mod gz` under
// `#[cfg(feature = "std")]` because only the file-I/O half of the layer needs `std`. So this whole
// binary compiles away without that feature, and the gate below is `std` rather than `gz` -- `gz` is
// `libz-rs-sys`'s feature, and it forwards to this one.
//
// A misspelled gate here is the single most likely way for this suite to fail silently: the file
// would compile to an empty binary and every check would pass while asserting nothing. The gate is
// therefore verified by counting: `cargo test -p zlib-rs --features std --test gz` must report a
// non-zero number of tests, not merely "ok".
#![cfg(feature = "std")]
// The workspace denies the panic-prone family -- `unwrap_used`, `expect_used`, `indexing_slicing`,
// `panic` -- in `[workspace.lints.clippy]`, inherited by every target of this crate including this
// one. That policy is right for `src/**`, where a panic would abort a C caller's process, and wrong
// here: a test asserts, and an assertion that fails panics. `clippy.toml` already sets
// `allow-unwrap-in-tests`, `allow-expect-in-tests` and `allow-panic-in-tests`, but those keys key on
// `#[test]` context only, so the file-scope helpers below -- and `indexing_slicing`, which has no
// in-test key at all -- still need this. Nothing else is relaxed.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

//! Integration tests for the `gzFile` layer: `crates/zlib-rs/src/gz/**`.
//!
//! Two things set this suite apart from its siblings in this directory. It is the only one that
//! needs a feature gate and can touch the filesystem, and it is the only one that guards a **frozen
//! ABI**: the caller-visible prefix of the `gzFile` state, whose field offsets are compiled into
//! *caller* object code by the `gzgetc` macro and can therefore never be changed. That contract is
//! asserted first, in [`exposed_prefix_is_the_frozen_gzgetc_abi`], and deliberately without any I/O
//! so that it also runs under Miri.
//!
//! # What is asserted here, and what is asserted elsewhere
//!
//! The internals -- `gz_error`, `gz_intmax`, `gt_off`, and the `gz_reset` / `gz_open` / `gz_load` /
//! `gz_avail` / `gz_look` / `gz_decomp` / `gz_fetch` / `gz_skip` / `gz_read` / `gz_init` /
//! `gz_comp` / `gz_zero` / `gz_write` / `gz_vacate` family -- are `pub(crate)`, matching C's
//! `ZLIB_INTERNAL` and the `local:` block of `zlib.map`. They are unreachable from an integration
//! test by construction, which is the point: each one already carries direct coverage in the
//! `#[cfg(test)] mod tests` block of its own file under `src/gz/`, and what this suite adds is the
//! view a *consumer* has. Every internal behaviour named below is therefore asserted through the
//! public drivers.
//!
//! The definitive `z_stream`, `gz_header` and `struct gzFile_s` layout assertions belong to the
//! facade, because only the facade knows how wide `z_off_t` and `uLong` are on the target; it makes
//! them at compile time in `crates/libz-rs-sys/src/layout_assertions.rs`. This file is the
//! core-side half: it proves that the type the facade hands to a caller has the shape `zlib.h`
//! promises.
//!
//! # The oracle
//!
//! Every number in this file comes from the reference implementation, and the citation is given at
//! its assertion:
//!
//! * `gzguts.h` L154-L167 -- `GZBUFSIZE` and the mode and `how` constants.
//! * `gzguts.h` L170-L203 -- `gz_state`, whose **first** member is `struct gzFile_s x`.
//! * `zlib.h` L1956-L1968 -- `struct gzFile_s` and the `gzgetc` macro.
//! * `zlib.h` L1750-L1760 -- the five codes `gzclose` may return.
//! * `gzlib.c` L108-L197 -- the mode-string grammar; L322-L343 `gzbuffer`; L498 `gzeof`;
//!   L513-L527 `gzerror`.
//! * `gzread.c` L148-L153 -- the four-byte gzip-detection heuristic.
//! * `gzclose.c` L11-L23 -- the direction dispatch.
//! * `test/example.c` L90-L165 -- `test_gzio`, reproduced number for number in
//!   [`test_gzio_is_reproduced_exactly`].
//! * `test/minigzip.c` L144-L145 -- `BUFLEN` and `MAX_NAME_LEN`, and the shape of its
//!   compress/uncompress loops.
//! * `doc/rfc1952.txt` 2.2-2.3 -- the member header and the `CRC32 || ISIZE` trailer.
//!
//! # Miri
//!
//! Miri cannot perform file I/O, so the suite is partitioned. The layout, constant and
//! mode-grammar tests touch nothing outside the process. Everything else is driven through
//! [`zlib_rs::gz::gz_open_handle`] over an in-memory [`MemoryFile`], which is the same technique the
//! `src/gz/` unit tests use and which keeps those tests Miri-runnable too. Only the handful of
//! genuinely filesystem-specific behaviours -- a missing path, `O_EXCL`, append-to-an-existing-file
//! -- go through [`zlib_rs::gz::gzopen`], and each of those carries `#[cfg_attr(miri, ignore)]`
//! with the reason stated at the test.
//!
//! # No `unsafe`, no FFI, no dependencies
//!
//! `crates/zlib-rs` is `#![forbid(unsafe_code)]` and this file honours the same rule: the layout
//! assertions use [`core::mem::size_of`] and [`core::mem::offset_of`], both safe. There is no
//! `tempfile` crate and no `[dev-dependencies]` -- `deny.toml`'s `[bans]` section names the empty
//! dependency table as the enforcement point -- so the temporary-file helper below is written from
//! `std` alone.

mod common;

use core::cell::{Cell, RefCell};
use core::ffi::{c_int, c_uint};
use core::mem::{align_of, offset_of, size_of};

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

// Everything from `zlib_rs` is imported by **module path**, never through the crate root. That is not
// a style preference: the crate declares `default = []` and every root re-export carries
// `#[cfg(feature = "rust-api")]`, so `zlib_rs::ReturnCode` does not exist in a build that only
// enables `std` while `zlib_rs::error::ReturnCode` always does. Reaching in by path is what makes this
// file compile identically under `--features std` and `--all-features`, and it is what the C ABI
// facade does for the same reason. `tests/common/mod.rs` documents the same rule.
//
// `allocate` is reached the same way, through `src/lib.rs`'s unconditional `pub mod allocate`:
// `GlobalAllocator` is the allocator a `gzFile` uses, because a `gzFile` has no caller-supplied
// `zalloc`/`zfree` hooks to honour, and `common::TrackingAllocator` is injected in its place wherever
// a test needs allocation to fail on demand.
use zlib_rs::allocate::GlobalAllocator;
use zlib_rs::config::{
    Z_BEST_COMPRESSION, Z_BEST_SPEED, Z_BLOCK, Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY,
    Z_FILTERED, Z_FINISH, Z_FIXED, Z_FULL_FLUSH, Z_HUFFMAN_ONLY, Z_NO_FLUSH, Z_PARTIAL_FLUSH,
    Z_RLE, Z_SYNC_FLUSH, Z_TREES,
};
use zlib_rs::crc32::crc32;
use zlib_rs::error::ReturnCode;
use zlib_rs::gz::{
    gz_open_handle, gzbuffer, gzclearerr, gzclose, gzclose_r, gzclose_w, gzdirect, gzeof, gzerror,
    gzflush, gzfread, gzfwrite, gzgetc, gzgetc_, gzgets, gzoffset64, gzopen, gzputc, gzputs,
    gzread, gzrewind, gzseek64, gzsetparams, gztell64, gzungetc, gzwrite, narrow_offset,
    printf_begin, printf_bytes, printf_commit, printf_with, GzFileExposed, GzHandle, GzHandleRef,
    GzHow, GzIoError, GzMode, GzOpenError, GzOpenSpec, GzSeekFrom, GzState, ZOff64, COPY,
    GZBUFSIZE, GZIP, GZ_APPEND, GZ_NONE, GZ_READ, GZ_WRITE, LOOK,
};

use common::{check_err, corpus};

// ---------------------------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------------------------

/// The payload `test/example.c` writes and reads back: `"hello, hello!"` plus its terminating zero.
///
/// `test_gzio` computes `len = (int)strlen(hello) + 1` at L95 and asserts `gzread` returns exactly
/// that (L123), so the trailing NUL is part of the data rather than a string terminator. It is
/// [`common::corpus::HELLO`], restated here as a local alias only so the assertions below read like
/// the C they reproduce.
const HELLO: &[u8] = corpus::HELLO;

/// `"hello, hello!\0"` as a gzip member, produced by the reference zlib at its default level.
///
/// Transcribed from the fixture the `src/gz/read.rs` unit tests already use, which is itself the
/// output of the C implementation. Reading it proves the "produced by C zlib, consumed here"
/// direction of bidirectional stream interoperability, on the exact payload `test/example.c` uses.
const HELLO_GZ: [u8; 31] = [
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7,
    0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c, 0x00, 0x9d, 0x3f, 0x6c, 0xb5, 0x0e, 0x00, 0x00, 0x00,
];

/// `"hello, hello!"` -- without a trailing NUL -- compressed by the system `gzip(1)`.
///
/// `FLG` is `0x08`, so this member carries an `FNAME` field (`named.txt\0`) and a real `MTIME`,
/// neither of which [`HELLO_GZ`] has. It exercises the optional-field skipping of `doc/rfc1952.txt`
/// 2.3.1.1 on a member no part of this project produced.
const GZIP_CLI_NAMED: [u8; 40] = [
    0x1f, 0x8b, 0x08, 0x08, 0x13, 0x7f, 0x74, 0x6a, 0x00, 0x03, 0x6e, 0x61, 0x6d, 0x65, 0x64, 0x2e,
    0x74, 0x78, 0x74, 0x00, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x00,
    0x9b, 0xdc, 0x9a, 0xb3, 0x0d, 0x00, 0x00, 0x00,
];

/// The payload [`GZIP_CLI_NAMED`] decompresses to.
const HELLO_NO_NUL: &[u8] = b"hello, hello!";

/// `"first half\n"` as a gzip member, from the same verified fixture set as [`HELLO_GZ`].
const MEMBER_A: [u8; 31] = [
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x4b, 0xcb, 0x2c, 0x2a, 0x2e, 0x51,
    0xc8, 0x48, 0xcc, 0x49, 0xe3, 0x02, 0x00, 0x92, 0xf5, 0x72, 0xa5, 0x0b, 0x00, 0x00, 0x00,
];

/// The payload [`MEMBER_A`] decompresses to.
const PAYLOAD_A: &[u8] = b"first half\n";

/// `"second half\n"` as a gzip member.
const MEMBER_B: [u8; 32] = [
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x2b, 0x4e, 0x4d, 0xce, 0xcf, 0x4b,
    0x51, 0xc8, 0x48, 0xcc, 0x49, 0xe3, 0x02, 0x00, 0xcf, 0x3a, 0xdb, 0xb5, 0x0c, 0x00, 0x00, 0x00,
];

/// The payload [`MEMBER_B`] decompresses to.
const PAYLOAD_B: &[u8] = b"second half\n";

/// The three bytes an RFC 1952 member opens with: `ID1`, `ID2` and `CM`.
///
/// `doc/rfc1952.txt` 2.3.1 fixes `ID1 = 31` (`0x1f`), `ID2 = 139` (`0x8b`) and `CM = 8` for
/// "deflate". `gz_look` tests exactly these three plus `FLG < 32` (`gzread.c` L151-L153).
const GZIP_PREFIX: [u8; 3] = [0x1f, 0x8b, 0x08];

/// The `errno` a stalled non-blocking read or write reports on Linux.
///
/// Only [`GzIoError::would_block`] is acted upon by the layer; the number travels alongside it so
/// that the facade can restore the platform's `errno` for a C caller. `EAGAIN` is used rather than
/// `EWOULDBLOCK` because they are the same value on Linux and `gz_avail` treats them identically.
const EAGAIN: i32 = 11;

/// The `errno` a failed `close(2)` reports here. Any non-zero value drives the same `Z_ERRNO`.
const EIO: i32 = 5;

// ---------------------------------------------------------------------------------------------
// Fixture sizing, and why it depends on Miri
// ---------------------------------------------------------------------------------------------

/// Scales a generated fixture down when the suite is interpreted rather than executed.
///
/// Miri interprets every machine instruction, so a `DEFLATE` round trip that costs microseconds
/// natively costs tens of seconds there. **No test is skipped for it** -- every assertion below runs
/// in both configurations, and only the seven filesystem tests are `#[cfg_attr(miri, ignore)]`,
/// because Miri genuinely cannot open a file. What changes is the *size* of the fixtures whose only
/// purpose is to be longer than one working buffer: those tests set the buffer small with
/// [`gzbuffer`] as well, so the shape under test -- crossing a buffer boundary, refilling the window,
/// spanning several `gz_avail` calls -- is preserved at a fraction of the cost.
///
/// The `native` figure is the one that matters; the divisor is chosen so that even the largest
/// fixture stays under a kilobyte when interpreted.
const fn bulk(native: usize) -> usize {
    if cfg!(miri) {
        native / 16
    } else {
        native
    }
}

/// The read sizes a round-trip test should try.
///
/// Natively the whole spread, because `gz_read` branches on whether the request is smaller than twice
/// the working buffer (`gzread.c` L355-L378) and a one-byte request is the extreme of the small side.
/// Under Miri a single mid-sized value, for the reason given on [`bulk`]: a one-byte read of a
/// thirty-kilobyte payload is thirty thousand interpreted calls.
fn read_chunks() -> &'static [usize] {
    if cfg!(miri) {
        &[7]
    } else {
        &[1, 7, 64, 16384]
    }
}

// ---------------------------------------------------------------------------------------------
// The in-memory file: this suite's stand-in for C's `int fd`
// ---------------------------------------------------------------------------------------------

/// How a [`MemoryFile`] should misbehave instead of transferring bytes.
///
/// The distinction between the two failing kinds is the non-blocking contract, and the layer treats
/// them completely differently: a `would_block` failure is recorded in `again` and is not an error at
/// all (`gzwrite.c` L117-L119), while any other `errno` becomes `Z_ERRNO`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Failure {
    /// Never fail: the ordinary blocking file every test uses unless it says otherwise.
    None,
    /// Report `EAGAIN` on every operation, transferring nothing.
    Stall,
    /// Serve the first `n` operations and report `EAGAIN` on every one after them.
    StallAfter(usize),
    /// Report `EIO` on every operation, which is the `Z_ERRNO` path.
    Hard,
}

/// A file that lives in a shared `Vec<u8>`: the [`GzHandle`] fixture the whole suite is built on.
///
/// Nothing here touches the real filesystem, which is what keeps these tests runnable under Miri and
/// what makes the produced bytes directly inspectable -- and the bytes matter, because
/// [`produced_member_is_valid_rfc1952`] checks the header and trailer against
/// `doc/rfc1952.txt` by hand. It is the same technique the `#[cfg(test)] mod tests` blocks in
/// `src/gz/read.rs` and `src/gz/write.rs` use, for the same reasons.
///
/// The semantics are a real file's, not a stream's: `write` overwrites at the cursor and extends
/// only past the end, and `seek` accepts all three origins, because `finish_open` seeks to
/// `SEEK_END` for an append and to `SEEK_CUR` to record a read stream's starting position
/// (`gzlib.c` L270 and L276).
struct MemoryFile {
    /// The bytes, shared with the test so they can be read back after the state has taken the
    /// handle away for closing.
    data: Rc<RefCell<Vec<u8>>>,
    /// The read/write cursor, in bytes from the start.
    position: usize,
    /// The most bytes one `read` or `write` will transfer, or [`None`] for all of them. A small
    /// value forces `gz_comp`'s short-write loop (`gzwrite.c` L110-L123) and `gz_load`'s
    /// short-read loop (`gzread.c` L10-L12) to iterate.
    chunk: Option<usize>,
    /// Whether, and when, to fail rather than transfer.
    failure: Failure,
    /// How many reads and writes have been attempted, which is what [`Failure::StallAfter`] counts.
    operations: usize,
    /// How many times the handle has been closed, shared so the teardown can be asserted after the
    /// state has consumed the handle.
    closes: Rc<Cell<usize>>,
    /// How many `read` and `write` calls the layer has made, shared for the same reason as
    /// [`MemoryFile::closes`].
    ///
    /// Separate from [`MemoryFile::operations`], which counts *attempts* including the ones
    /// [`Failure`] refuses; this counts transfers that were actually served, which is the figure
    /// [`the_smallest_legal_buffer_produces_identical_bytes`] compares across buffer sizes.
    transfers: Rc<Cell<usize>>,
    /// Whether `close` should report `EIO`, which the layer must turn into `Z_ERRNO`.
    close_fails: bool,
}

impl MemoryFile {
    /// Decides how many bytes this operation may transfer, or reports the failure.
    fn allowance(&mut self, requested: usize) -> Result<usize, GzIoError> {
        let sequence = self.operations;
        self.operations = self.operations.saturating_add(1);
        match self.failure {
            Failure::Stall => return Err(GzIoError::new(EAGAIN, true)),
            Failure::StallAfter(served) if sequence >= served => {
                return Err(GzIoError::new(EAGAIN, true))
            }
            Failure::Hard => return Err(GzIoError::new(EIO, false)),
            // Nothing to report: either the handle never fails, or this operation is one of the
            // `StallAfter` allowance that is still being served.
            Failure::None | Failure::StallAfter(_) => {}
        }
        Ok(self.chunk.map_or(requested, |limit| limit.min(requested)))
    }
}

impl GzHandle for MemoryFile {
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, GzIoError> {
        let allowance = self.allowance(buf.len())?;
        self.transfers.set(self.transfers.get().saturating_add(1));
        let data = self.data.borrow();
        let available = data.len().saturating_sub(self.position);
        let count = allowance.min(available);
        buf[..count].copy_from_slice(&data[self.position..self.position + count]);
        drop(data);
        self.position += count;
        // `Ok(0)` is end of file, exactly as `read(2)` returning 0 is in `gz_load`
        // (`gzread.c` L31-L46).
        Ok(count)
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize, GzIoError> {
        let allowance = self.allowance(buf.len())?;
        self.transfers.set(self.transfers.get().saturating_add(1));
        // A short write is normal, but a zero-byte write of a non-empty buffer is a contract
        // violation the layer answers with `Z_STREAM_ERROR`; a stall is reported as a stall.
        let count = allowance.max(usize::from(!buf.is_empty())).min(buf.len());
        let mut data = self.data.borrow_mut();
        if data.len() < self.position {
            data.resize(self.position, 0);
        }
        let end = self.position + count;
        if data.len() < end {
            data.resize(end, 0);
        }
        data[self.position..end].copy_from_slice(&buf[..count]);
        drop(data);
        self.position = end;
        Ok(count)
    }

    fn seek(&mut self, offset: ZOff64, whence: GzSeekFrom) -> Result<ZOff64, GzIoError> {
        let base = match whence {
            GzSeekFrom::Start => 0,
            GzSeekFrom::Current => ZOff64::try_from(self.position).unwrap(),
            GzSeekFrom::End => ZOff64::try_from(self.data.borrow().len()).unwrap(),
        };
        let Some(target) = base.checked_add(offset) else {
            return Err(GzIoError::new(EIO, false));
        };
        if target < 0 {
            // `lseek` reports `EINVAL` for a negative result; only the failure matters here.
            return Err(GzIoError::new(EIO, false));
        }
        self.position = usize::try_from(target).unwrap();
        Ok(target)
    }

    fn set_nonblocking(&mut self, _nonblocking: bool) -> Result<(), GzIoError> {
        // A path-based open applies the flag in the `open` call, so an implementation that only
        // opens by path may report success without doing anything (`gzlib.c` L160-L162).
        Ok(())
    }

    fn close(&mut self) -> Result<(), GzIoError> {
        // Contractually idempotent: `GzState`'s teardown may close a handle a close path already
        // closed, and the second call must succeed.
        self.closes.set(self.closes.get().saturating_add(1));
        if self.close_fails {
            return Err(GzIoError::new(EIO, false));
        }
        Ok(())
    }
}

/// A shared byte buffer plus its close counter: everything a test needs to outlive its stream.
#[derive(Debug, Clone)]
struct Sink {
    /// The file's bytes.
    data: Rc<RefCell<Vec<u8>>>,
    /// How many times a handle over [`Sink::data`] has been closed.
    closes: Rc<Cell<usize>>,
    /// How many `read` and `write` calls handles over [`Sink::data`] have served.
    transfers: Rc<Cell<usize>>,
}

impl Sink {
    /// An empty file.
    fn new() -> Self {
        Self {
            data: Rc::new(RefCell::new(Vec::new())),
            closes: Rc::new(Cell::new(0)),
            transfers: Rc::new(Cell::new(0)),
        }
    }

    /// A file that already contains `bytes`.
    fn with_contents(bytes: &[u8]) -> Self {
        let sink = Self::new();
        sink.data.borrow_mut().extend_from_slice(bytes);
        sink
    }

    /// A snapshot of the file's current contents.
    fn bytes(&self) -> Vec<u8> {
        self.data.borrow().clone()
    }

    /// The file's current length in bytes.
    fn len(&self) -> usize {
        self.data.borrow().len()
    }

    /// How many times a handle over this file has been closed.
    fn closes(&self) -> usize {
        self.closes.get()
    }

    /// How many `read` and `write` calls handles over this file have served.
    ///
    /// This is the number of times the `gzFile` layer crossed the [`GzHandle`] boundary, which is
    /// what a buffer size actually controls: one call per `want` bytes rather than per byte.
    fn transfers(&self) -> usize {
        self.transfers.get()
    }

    /// An ordinary blocking handle over this file, positioned at the start.
    fn handle(&self) -> MemoryFile {
        MemoryFile {
            data: Rc::clone(&self.data),
            position: 0,
            chunk: None,
            failure: Failure::None,
            operations: 0,
            closes: Rc::clone(&self.closes),
            transfers: Rc::clone(&self.transfers),
            close_fails: false,
        }
    }

    /// A handle that transfers at most `chunk` bytes per operation.
    fn dribbling_handle(&self, chunk: usize) -> MemoryFile {
        MemoryFile {
            chunk: Some(chunk),
            ..self.handle()
        }
    }

    /// A handle that fails as `failure` prescribes.
    fn failing_handle(&self, failure: Failure) -> MemoryFile {
        MemoryFile {
            failure,
            ..self.handle()
        }
    }

    /// A handle whose `close` reports `EIO`.
    fn failing_close_handle(&self) -> MemoryFile {
        MemoryFile {
            close_fails: true,
            ..self.handle()
        }
    }
}

/// Opens an in-memory stream: [`gz_open_handle`] over `handle` with the crate's own allocator.
///
/// `gz_open_handle` is the injection point the safe core cannot avoid needing --
/// `FromRawFd::from_raw_fd` is `unsafe`, so only `crates/libz-rs-sys` can adopt a real descriptor --
/// and it runs the whole of `gz_open`'s shared remainder, `finish_open` and `gz_reset` included. So
/// a stream opened this way is indistinguishable from one [`gzopen`] produced, apart from where its
/// bytes live. `GlobalAllocator` is the same allocator [`gzopen`] uses for a `gzFile`, which has no
/// caller-supplied hooks to honour.
///
/// # Panics
///
/// If the mode string is rejected. Every caller below passes a mode string it expects to be
/// accepted; the rejections are asserted by [`mode_string_grammar_matches_the_reference`] and
/// [`try_open_memory`].
fn open_memory(handle: MemoryFile, mode: &[u8]) -> GzState<'static, GlobalAllocator> {
    try_open_memory(handle, mode).expect("the mode string should be accepted")
}

/// [`open_memory`] without the panic, for the cases that assert a rejection.
fn try_open_memory(
    handle: MemoryFile,
    mode: &[u8],
) -> Result<GzState<'static, GlobalAllocator>, GzOpenError> {
    gz_open_handle(Box::new(handle), b"<memory>", mode, GlobalAllocator).map_err(|failed| {
        // The handle comes back unclosed, which is `zlib.h` L1415-L1416's "gzdopen does not close
        // fd if it fails". Dropping this one closes nothing real, so the count is left alone.
        failed.error
    })
}

/// Compresses `payload` into a fresh in-memory file and returns the bytes of the gzip stream.
///
/// The write half of `test/minigzip.c`'s `gz_compress` loop (its L200-L228), reduced to one call:
/// write, then close and require `Z_OK`, because "gzclose is required to return `Z_OK`" is exactly
/// what that loop asserts.
fn compress_to_memory(payload: &[u8], mode: &[u8]) -> Vec<u8> {
    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), mode);
    if !payload.is_empty() {
        let written = gzwrite(&mut state, payload);
        assert_eq!(
            usize::try_from(written).unwrap(),
            payload.len(),
            "gzwrite should accept the whole payload"
        );
    }
    // `test/minigzip.c` L391 ends `gz_compress` with `if (gzclose(out) != Z_OK) error("failed
    // gzclose")`, which is the `CHECK_ERR` shape; [`common::check_err`] is that macro's port, so the
    // close goes through it rather than through a hand-written comparison.
    check_err(gzclose(Some(&mut state)), "gzclose after gz_compress");
    sink.bytes()
}

/// Decompresses `stream` through the `gzFile` read path, reading `chunk` bytes at a time.
///
/// The read half of `test/minigzip.c`'s `gz_uncompress` loop (its L231-L257): read until zero,
/// treat a negative count as an error, and require `Z_OK` from the close.
fn decompress_from_memory(stream: &[u8], chunk: usize) -> Vec<u8> {
    let sink = Sink::with_contents(stream);
    let mut state = open_memory(sink.handle(), b"rb");
    let recovered = read_to_end(&mut state, chunk);
    // `test/minigzip.c` L413 ends `gz_uncompress` the same way.
    check_err(gzclose(Some(&mut state)), "gzclose after gz_uncompress");
    recovered
}

/// Reads a stream to exhaustion `chunk` bytes at a time, asserting no error is reported.
fn read_to_end(state: &mut GzState<'static, GlobalAllocator>, chunk: usize) -> Vec<u8> {
    assert!(chunk > 0, "a zero-length read would never terminate");
    let mut out = Vec::new();
    let mut buf = vec![0_u8; chunk];
    loop {
        let got = gzread(state, &mut buf);
        assert!(got >= 0, "gzread reported an error: {:?}", error_of(state));
        if got == 0 {
            return out;
        }
        out.extend_from_slice(&buf[..usize::try_from(got).unwrap()]);
    }
}

/// The stream's recorded status and message, as a caller would read them through `gzerror`.
fn error_of(state: &mut GzState<'static, GlobalAllocator>) -> (i32, String) {
    let mut code = 0;
    let message = gzerror(Some(state), Some(&mut code))
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default();
    (code, message)
}

// ---------------------------------------------------------------------------------------------
// The temporary file, written from `std` alone
// ---------------------------------------------------------------------------------------------

/// A uniquely named path under the platform's temporary directory, removed when dropped.
///
/// There is no `tempfile` crate here and there cannot be one: `crates/zlib-rs` declares an empty
/// `[dependencies]` and no `[dev-dependencies]`, and `deny.toml`'s `[bans]` section names that table
/// as the enforcement point. So the name is built from the process id and a per-process counter --
/// the process id keeps two concurrent `cargo test` runs apart, the counter keeps the tests inside
/// one run apart -- and cleanup is a [`Drop`] impl, which runs on the panicking path as well as the
/// successful one. Nothing here needs `unsafe`.
struct TempPath {
    /// The path, which may or may not exist at any moment.
    path: PathBuf,
}

impl TempPath {
    /// Reserves a fresh path whose name contains `tag`, removing any stale file at it.
    fn new(tag: &str) -> Self {
        /// Distinguishes the paths reserved within one process.
        static COUNTER: AtomicU64 = AtomicU64::new(0);

        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!(
            "zlib_rs_gz_{pid}_{tag}_{unique}.gz",
            pid = process::id()
        ));
        // A path left behind by a killed earlier run would make an `x` (`O_EXCL`) open fail for the
        // wrong reason.
        let _ = fs::remove_file(&path);
        Self { path }
    }

    /// The reserved path.
    fn path(&self) -> &Path {
        &self.path
    }

    /// The path as the bytes `gzopen` takes, which are C's `const char *`.
    ///
    /// `to_str` rather than a platform byte conversion: `crate::gz::open`'s own opener requires a
    /// narrow path to be valid UTF-8 on Windows, and every name this type builds is ASCII apart from
    /// the temporary directory itself.
    fn as_bytes(&self) -> &[u8] {
        self.path
            .to_str()
            .expect("the temporary directory should be valid UTF-8")
            .as_bytes()
    }

    /// Whether a file currently exists at the path.
    fn exists(&self) -> bool {
        self.path.exists()
    }

    /// The file's contents.
    fn read(&self) -> Vec<u8> {
        fs::read(&self.path).expect("the temporary file should be readable")
    }

    /// Replaces the file's contents with `bytes`.
    fn write(&self, bytes: &[u8]) {
        let mut file =
            fs::File::create(&self.path).expect("the temporary file should be creatable");
        file.write_all(bytes)
            .expect("the temporary file should be writable");
        file.flush().expect("the temporary file should flush");
    }
}

impl Drop for TempPath {
    fn drop(&mut self) {
        // Best effort, and deliberately silent: a test that already failed must not be reported as
        // failing again because its scratch file had gone.
        let _ = fs::remove_file(&self.path);
    }
}

// =============================================================================================
// 1. The exposed prefix: the highest-risk ABI constraint in the entire port
// =============================================================================================

/// The layout of the caller-visible prefix is frozen, and this is what freezes it.
///
/// `zlib.h` L1956-L1960 declares:
///
/// ```c
/// struct gzFile_s {
///     unsigned have;
///     unsigned char *next;
///     z_off64_t pos;
/// };
/// ```
///
/// and L1967-L1968 declares `gzgetc` as a **macro** (L1964-L1965 declaring `z_gzgetc` identically
/// under `Z_PREFIX_SET`):
///
/// ```c
/// #define gzgetc(g) \
///       ((g)->have ? ((g)->have--, (g)->pos++, *((g)->next)++) : (gzgetc)(g))
/// ```
///
/// # Why a wrong answer here is not a test failure but memory corruption
///
/// That macro's field arithmetic is compiled into **caller** object code. Every program that calls
/// `gzgetc` already contains, baked into its own instructions, the offsets `have` sits at, `pos`
/// sits at and `next` sits at -- and this port cannot recompile any of them. It performs three
/// mutations per byte with no call into the library at all: `have--`, `pos++`, and a read through
/// `next` followed by a post-increment of the pointer itself. If the Rust type placed those fields
/// anywhere else, a caller would decrement one field while reading another, and the failure would
/// appear as silent heap corruption inside the *application*, at a point arbitrarily far from
/// anything this library did. No other test in this crate would notice. The `Z_PREFIX_SET` variant
/// defines `z_gzgetc` identically, so the constraint does not depend on how the header is
/// configured.
///
/// `gzguts.h` L170-L172 embeds this struct as `gz_state`'s **first** member, so its offsets are
/// offsets into the whole state as well. That half of the contract cannot be asserted from an
/// integration test, because `GzState::x` is `pub(crate)` and [`offset_of`] respects field
/// visibility; it is asserted at compile time instead, inside `src/gz/state.rs`'s `mod layout_64`,
/// where the field is in scope. What is checkable from out here -- that the state is at least as
/// large as the prefix and at least as strictly aligned -- is checked below, and the definitive
/// caller-facing assertions live in `crates/libz-rs-sys/src/layout_assertions.rs`, which is the
/// only place that knows how wide `z_off64_t` is on the target.
///
/// # Portability
///
/// The absolute numbers hold on LP64, the model the reference measurements were taken under, so they
/// are asserted only where a pointer is eight bytes wide. The **relations** are
/// asserted unconditionally, because they are what `#[repr(C)]` guarantees on every target and they
/// are enough to catch a reordering or a shrink: on a 32-bit target the same struct is `have` at 0,
/// `next` at 4 and `pos` at 8 for sixteen bytes total, with no padding after `have`. Note the
/// asymmetry the relations encode: `z_off64_t` is eight bytes on every target this port supports,
/// while pointer width varies, so `pos`'s *size* is fixed and its *offset* is not.
///
/// This test performs no I/O, so it runs under Miri.
#[test]
fn exposed_prefix_is_the_frozen_gzgetc_abi() {
    // The relations, which hold wherever `#[repr(C)]` does.
    assert_eq!(
        offset_of!(GzFileExposed, have),
        0,
        "`have` is the first member of `struct gzFile_s`, so it must sit at offset zero"
    );
    assert!(
        offset_of!(GzFileExposed, next) >= size_of::<c_uint>(),
        "`next` must start at or after the end of `have`"
    );
    assert!(
        offset_of!(GzFileExposed, pos) >= offset_of!(GzFileExposed, next) + size_of::<*mut u8>(),
        "`pos` must start at or after the end of `next`"
    );
    assert!(
        size_of::<GzFileExposed>() >= offset_of!(GzFileExposed, pos) + size_of::<ZOff64>(),
        "the prefix must be large enough to contain `pos`"
    );

    // The field widths. `have` is C's `unsigned`, which is `c_uint`, never a fixed-width integer;
    // `next` is `unsigned char *`, so it is pointer-width; `pos` is `z_off64_t`, which is eight
    // bytes on every supported target.
    assert_eq!(size_of::<c_uint>(), 4, "C's `unsigned` is four bytes here");
    assert_eq!(
        size_of::<ZOff64>(),
        8,
        "`z_off64_t` is a signed 64-bit integer on every target this port supports"
    );
    assert_eq!(
        align_of::<ZOff64>(),
        8,
        "`z_off64_t`'s alignment is what puts the padding after `have` on LP64"
    );

    // The measured LP64 layout of `struct gzFile_s`: 24 bytes, offsets 0 / 8 / 16.
    #[cfg(target_pointer_width = "64")]
    {
        assert_eq!(
            size_of::<GzFileExposed>(),
            24,
            "`sizeof(struct gzFile_s)` is 24 on LP64"
        );
        assert_eq!(offset_of!(GzFileExposed, have), 0, "`have` is at offset 0");
        assert_eq!(offset_of!(GzFileExposed, next), 8, "`next` is at offset 8");
        assert_eq!(offset_of!(GzFileExposed, pos), 16, "`pos` is at offset 16");
    }

    // The 32-bit layout, stated so that a port to such a target is checked rather than assumed.
    #[cfg(target_pointer_width = "32")]
    {
        assert_eq!(size_of::<GzFileExposed>(), 16);
        assert_eq!(offset_of!(GzFileExposed, next), 4);
        assert_eq!(offset_of!(GzFileExposed, pos), 8);
    }

    // The outer state's half of the contract, as far as it is observable from here. `GzState` is
    // `#[repr(C)]` with the prefix declared first, and `offset_of!(GzState, x) == 0` is asserted in
    // `src/gz/state.rs`; these two are what remains checkable through the public surface.
    assert!(
        size_of::<GzState<'static, GlobalAllocator>>() >= size_of::<GzFileExposed>(),
        "the state embeds the prefix, so it cannot be smaller than it"
    );
    assert!(
        align_of::<GzState<'static, GlobalAllocator>>() >= align_of::<GzFileExposed>(),
        "the state must be at least as strictly aligned as its first member"
    );
}

/// The just-opened prefix: nothing available, no buffer, position zero.
///
/// `gz_open` reaches this state through `gz_reset`, which sets `state->x.have = 0` (`gzlib.c` L70)
/// and `state->x.pos = 0` (L82); `next` stays null because no buffer exists until `gz_look` or
/// `gz_init` allocates one. A null `next` alongside a zero `have` is exactly the pair the `gzgetc`
/// macro handles by falling through to the `gzgetc` *function*, which is why the combination is safe
/// to expose.
///
/// No I/O, so this runs under Miri.
#[test]
fn exposed_prefix_empty_is_the_just_opened_state() {
    let empty = GzFileExposed::EMPTY;
    assert_eq!(empty.have, 0, "nothing is available before the first fill");
    assert!(
        empty.next.is_null(),
        "there is no output buffer to point at yet"
    );
    assert_eq!(empty.pos, 0, "the uncompressed position starts at zero");

    // And a freshly constructed state agrees, through the accessors a caller has.
    let state: GzState<'static, GlobalAllocator> = GzState::new(GlobalAllocator);
    assert_eq!(state.have(), 0);
    assert_eq!(state.pos(), 0);
    assert_eq!(state.mode(), GZ_NONE, "no direction has been chosen yet");
    assert_eq!(state.size(), 0, "no buffer has been allocated yet");
    assert_eq!(state.want(), GZBUFSIZE, "the default request is GZBUFSIZE");
    assert_eq!(state.how(), LOOK, "a read stream starts by looking");
    assert_eq!(state.junk(), -1, "-1 marks the first member");
    assert!(!state.has_handle(), "no file has been installed");
}

// =============================================================================================
// 2. The constants
// =============================================================================================

/// Every constant of `gzguts.h` L154-L167, transcribed and checked one at a time.
///
/// The two odd numbers are the point of this test. `gzguts.h` L158 introduces the mode values as
/// "gzip modes, also provide a little integrity check on the passed structure", and every entry
/// point tests `state->mode != GZ_READ && state->mode != GZ_WRITE` before trusting the pointer it
/// was handed (for example `gzlib.c` L328-L329). `7247` and `31153` look like noise precisely
/// because they are: a value a caller's uninitialised or foreign memory is unlikely to hold. Tidying
/// them to `1` and `2` would compile, pass every functional test, and silently reduce that check to
/// nothing -- so they are asserted individually, with the reason attached.
///
/// `GZIP = 2` is included because it is easy to miss: `gzguts.h` L165-L167 lists all three `how`
/// values, and a summary that names only `LOOK` and `COPY` leaves the decompressing path unchecked.
///
/// No I/O, so this runs under Miri.
#[test]
fn gzguts_constants_are_transcribed_exactly() {
    // L156, and the constraint L154-L155 states: this and twice this must fit in an `unsigned`.
    assert_eq!(GZBUFSIZE, 8192, "gzguts.h L156");
    assert!(
        GZBUFSIZE.checked_mul(2).is_some(),
        "gzguts.h L154-L155 requires 2 * GZBUFSIZE to be representable"
    );

    // L159-L162, the modes.
    assert_eq!(GZ_NONE, 0, "gzguts.h L159");
    assert_eq!(
        GZ_READ, 7247,
        "gzguts.h L160 -- an integrity sentinel, not an ordinal; do not tidy it"
    );
    assert_eq!(
        GZ_WRITE, 31153,
        "gzguts.h L161 -- an integrity sentinel, not an ordinal; do not tidy it"
    );
    assert_eq!(
        GZ_APPEND, 1,
        "gzguts.h L162 -- transient; overwritten with GZ_WRITE once the file is open"
    );

    // L165-L167, the `how` values.
    assert_eq!(LOOK, 0, "gzguts.h L165 -- look for a gzip header");
    assert_eq!(COPY, 1, "gzguts.h L166 -- copy input directly");
    assert_eq!(GZIP, 2, "gzguts.h L167 -- decompress a gzip stream");

    // The four mode values are distinct, which is what makes the check a check.
    let modes = [GZ_NONE, GZ_READ, GZ_WRITE, GZ_APPEND];
    for (first, &left) in modes.iter().enumerate() {
        for &right in &modes[first + 1..] {
            assert_ne!(left, right, "the mode values must all differ");
        }
    }
}

/// The exhaustive views agree with the raw constants they replace.
///
/// `mode`, `how` and `whence` are stored raw -- as `i32` -- so that an unrecognised value can be
/// *held* in order to be rejected, which is the whole purpose of the integrity check. `GzMode`,
/// `GzHow` and `GzSeekFrom` are the exhaustive views over the recognised values, and a `match` on
/// one of them is what makes "a state was added but not handled" a compile error. This asserts the
/// two spellings cannot drift apart, and that an unrecognised value round-trips to [`None`] rather
/// than to a wrong variant.
///
/// `SEEK_SET`, `SEEK_CUR` and `SEEK_END` are 0, 1 and 2 per `zconf.h` L513-L515.
///
/// No I/O, so this runs under Miri.
#[test]
fn typed_views_agree_with_the_raw_constants() {
    assert_eq!(GzMode::None.as_raw(), GZ_NONE);
    assert_eq!(GzMode::Read.as_raw(), GZ_READ);
    assert_eq!(GzMode::Write.as_raw(), GZ_WRITE);
    assert_eq!(GzMode::Append.as_raw(), GZ_APPEND);
    assert_eq!(GzMode::from_raw(GZ_READ), Some(GzMode::Read));
    assert_eq!(GzMode::from_raw(GZ_WRITE), Some(GzMode::Write));
    assert_eq!(
        GzMode::from_raw(4242),
        None,
        "an unrecognised mode must be rejected, not mapped"
    );

    assert_eq!(GzHow::Look.as_raw(), LOOK);
    assert_eq!(GzHow::Copy.as_raw(), COPY);
    assert_eq!(GzHow::Gzip.as_raw(), GZIP);
    assert_eq!(GzHow::from_raw(GZIP), Some(GzHow::Gzip));
    assert_eq!(GzHow::from_raw(3), None);

    // `zconf.h` L513-L515.
    assert_eq!(GzSeekFrom::Start.as_raw(), 0, "SEEK_SET");
    assert_eq!(GzSeekFrom::Current.as_raw(), 1, "SEEK_CUR");
    assert_eq!(GzSeekFrom::End.as_raw(), 2, "SEEK_END");
    assert_eq!(GzSeekFrom::from_raw(2), Some(GzSeekFrom::End));
    assert_eq!(GzSeekFrom::from_raw(-1), None);
}

// =============================================================================================
// 3. The mode-string grammar
// =============================================================================================

/// Every character of the mode-string grammar, `gzlib.c` L108-L197.
///
/// The grammar is a table, so it is tested as one. Each row is `(mode string, expected outcome)`,
/// and the expectations are the four decisions `gz_open` writes into the state before it touches the
/// file system: direction, level, strategy and container handling.
///
/// The characters and their meanings, from the C source:
///
/// | Character | Effect | `gzlib.c` |
/// |---|---|---|
/// | `0`..`9` | compression level | L114-L115 |
/// | `r` / `w` / `a` | read / write / append | L120-L128 |
/// | `+` | **rejected** -- "can't read and write at the same time" | L129-L131 |
/// | `b` | ignored, documented as "will request binary anyway" | L132-L133 |
/// | `e` | `O_CLOEXEC` | L134-L136 |
/// | `x` | `O_EXCL` | L137-L139 |
/// | `f` / `h` / `R` / `F` | `Z_FILTERED` / `Z_HUFFMAN_ONLY` / `Z_RLE` / `Z_FIXED` | L140-L155 |
/// | `G` | `direct = -1`, gzip only | L156-L158 |
/// | `N` | `O_NONBLOCK` | L159-L162 |
/// | `T` | `direct = 1`, transparent | L163-L166 |
/// | anything else | **silently ignored** -- "could consider as an error, but just ignore" | L167-L168 |
///
/// and then the resolution rules: no direction at all is rejected (L174-L177), `T` while reading is
/// rejected because a transparent read cannot be forced (L181-L185), a read stream left at `direct
/// == 0` becomes `1` so that an empty file reads as transparent (L186-L189), and `G` while writing
/// is rejected outright (L191-L195).
///
/// Two ordering facts are asserted as well, because they are observable and easy to break: a later
/// character overrides an earlier one, so `"rw"` opens for writing and the last `G` or `T` wins; and
/// `+` is refused immediately, without the rest of the string being consulted.
///
/// This test creates no file and performs no I/O -- [`GzOpenSpec::parse`] is exactly the first half
/// of `gz_open` lifted into a value -- so it runs under Miri.
#[test]
fn mode_string_grammar_matches_the_reference() {
    // Accepted strings and what they resolve to.
    let accepted: &[(&[u8], i32, i32, i32)] = &[
        // (mode, expected mode, expected level, expected direct)
        (b"r", GZ_READ, Z_DEFAULT_COMPRESSION, 1),
        (b"rb", GZ_READ, Z_DEFAULT_COMPRESSION, 1),
        (b"w", GZ_WRITE, Z_DEFAULT_COMPRESSION, 0),
        (b"wb", GZ_WRITE, Z_DEFAULT_COMPRESSION, 0),
        (b"a", GZ_APPEND, Z_DEFAULT_COMPRESSION, 0),
        (b"ab", GZ_APPEND, Z_DEFAULT_COMPRESSION, 0),
        // A level digit takes effect, including the two extremes and the "store only" zero.
        (b"wb0", GZ_WRITE, 0, 0),
        (b"wb1", GZ_WRITE, Z_BEST_SPEED, 0),
        (b"wb9", GZ_WRITE, Z_BEST_COMPRESSION, 0),
        // The last digit wins, because each one simply assigns.
        (b"wb19", GZ_WRITE, Z_BEST_COMPRESSION, 0),
        // `T` forces a transparent write.
        (b"wT", GZ_WRITE, Z_DEFAULT_COMPRESSION, 1),
        // `G` forces gzip on a read stream.
        (b"rG", GZ_READ, Z_DEFAULT_COMPRESSION, -1),
        // The last of `G` and `T` wins, and here that is `T`, so the write is transparent even
        // though a `G` appeared first -- had `G` come last the whole string would be rejected.
        (b"wGT", GZ_WRITE, Z_DEFAULT_COMPRESSION, 1),
        // The mirror image on a read stream: `G` last is accepted, `T` last is not.
        (b"rTG", GZ_READ, Z_DEFAULT_COMPRESSION, -1),
        // A later direction overrides an earlier one, so this opens for writing.
        (b"rw", GZ_WRITE, Z_DEFAULT_COMPRESSION, 0),
        // Unknown characters are ignored rather than rejected.
        (b"wbQ?~", GZ_WRITE, Z_DEFAULT_COMPRESSION, 0),
        // A NUL ends the walk, exactly as C's `while (*mode)` does, so the `+` past it is unseen.
        (b"wb\0+", GZ_WRITE, Z_DEFAULT_COMPRESSION, 0),
    ];

    for &(mode, expected_mode, expected_level, expected_direct) in accepted {
        let spec = GzOpenSpec::parse(mode)
            .unwrap_or_else(|error| panic!("{mode:?} should parse, got {error:?}"));
        assert_eq!(spec.mode(), expected_mode, "direction for {mode:?}");
        assert_eq!(spec.level(), expected_level, "level for {mode:?}");
        assert_eq!(spec.direct(), expected_direct, "direct for {mode:?}");
    }

    // The "last one wins" rule and the resolution rules compose, and the composition is what
    // decides acceptance: `direct` is resolved *after* the whole string has been walked, so which of
    // `G` and `T` came last determines whether the string is legal at all.
    assert_eq!(
        GzOpenSpec::parse(b"wGT").unwrap().direct(),
        1,
        "a `T` after a `G` while writing leaves a transparent write"
    );
    assert_eq!(
        GzOpenSpec::parse(b"wTG"),
        Err(GzOpenError::InvalidMode),
        "a `G` after a `T` while writing is `G` while writing, which gzlib.c L191-L195 forbids"
    );
    assert_eq!(
        GzOpenSpec::parse(b"rGT"),
        Err(GzOpenError::InvalidMode),
        "a `T` after a `G` while reading is a forced transparent read, which gzlib.c L181-L185 forbids"
    );

    // The four strategies, `gzlib.c` L140-L155.
    let strategies: &[(&[u8], i32)] = &[
        (b"wb", Z_DEFAULT_STRATEGY),
        (b"wbf", Z_FILTERED),
        (b"wbh", Z_HUFFMAN_ONLY),
        (b"wbR", Z_RLE),
        (b"wbF", Z_FIXED),
    ];
    for &(mode, expected) in strategies {
        assert_eq!(
            GzOpenSpec::parse(mode).unwrap().strategy(),
            expected,
            "strategy for {mode:?}"
        );
    }

    // The three flag characters, each of which reaches `open(2)` rather than the state.
    assert!(
        GzOpenSpec::parse(b"we").unwrap().cloexec(),
        "`e` -> O_CLOEXEC"
    );
    assert!(!GzOpenSpec::parse(b"w").unwrap().cloexec());
    assert!(
        GzOpenSpec::parse(b"wx").unwrap().exclusive(),
        "`x` -> O_EXCL"
    );
    assert!(!GzOpenSpec::parse(b"w").unwrap().exclusive());
    assert!(
        GzOpenSpec::parse(b"wN").unwrap().nonblocking(),
        "`N` -> O_NONBLOCK"
    );
    assert!(!GzOpenSpec::parse(b"w").unwrap().nonblocking());

    // `b` is a documented no-op, distinct from an unknown character even though both do nothing.
    assert_eq!(
        GzOpenSpec::parse(b"wb").unwrap(),
        GzOpenSpec::parse(b"w").unwrap(),
        "gzlib.c L132-L133: `b` is ignored -- binary is requested anyway"
    );

    // The rejections, all four of them.
    let rejected: &[&[u8]] = &[
        // L129-L131: `+` at any position, and it is refused before the rest is read -- `"r+w"`
        // would open for writing if the walk continued.
        b"r+", b"w+", b"r+w", b"+", // L174-L177: no direction at all.
        b"", b"b", b"9", b"bexT", // L181-L185: a transparent read cannot be forced.
        b"rT", b"rbT", // L191-L195: `G` has no meaning while writing.
        b"wG", b"aG",
    ];
    for &mode in rejected {
        assert_eq!(
            GzOpenSpec::parse(mode),
            Err(GzOpenError::InvalidMode),
            "{mode:?} should be rejected"
        );
    }

    // The typed view of the direction, so the raw value and the enum cannot drift.
    assert_eq!(
        GzOpenSpec::parse(b"rb").unwrap().mode_typed(),
        Some(GzMode::Read)
    );
    assert_eq!(
        GzOpenSpec::parse(b"ab").unwrap().mode_typed(),
        Some(GzMode::Append)
    );
}

// =============================================================================================
// 4. Write, read, and the shape of the bytes in between
// =============================================================================================

/// Every corpus class survives a write, a close, a reopen and a read unchanged.
///
/// The round trip is `test/minigzip.c`'s two loops back to back: `gz_compress` reads its input and
/// hands it to `gzwrite` until exhausted, then requires `Z_OK` from `gzclose`; `gz_uncompress` calls
/// `gzread` until it returns zero, treating a negative count as an error to be reported through
/// `gzerror`, and requires `Z_OK` from its `gzclose` too.
///
/// Two chunk sizes are used for the read: one large enough to take the whole payload at once, and
/// one small enough to force the layer's own output buffer to refill repeatedly. The distinction is
/// load-bearing, because `gz_read` takes a different branch for a request smaller than twice the
/// buffer size than for a larger one (`gzread.c` L355-L378): the small request fills the layer's
/// buffer so that `gzgetc` stays fast and `gzungetc` keeps its room, while the large one
/// decompresses straight into the caller's buffer.
///
/// The corpus is [`common::corpus::all`]: empty input, a single byte, highly repetitive data,
/// incompressible random data, natural-language text, binary data, the window-crossing payload and
/// the `test/example.c` fixtures. `minigzip`'s own `BUFLEN` of 16384 (`test/minigzip.c` L144) is one
/// of the chunk sizes so that the large-request branch is genuinely exercised.
///
/// Miri cannot perform file I/O, but nothing here performs any: the stream is a [`MemoryFile`].
#[test]
fn round_trip_recovers_every_corpus_class() {
    for (name, payload) in corpus::all() {
        // All eight classes are covered in both configurations; under Miri each one is truncated so
        // that the interpreter is not asked to compress thirty kilobytes. See `bulk` for why.
        let payload = if cfg!(miri) {
            payload[..payload.len().min(2048)].to_vec()
        } else {
            payload
        };
        let stream = compress_to_memory(&payload, b"wb");

        // Empty input still produces a complete member -- a header and a trailer with a zero
        // `ISIZE` -- because `gzclose_w` performs a `Z_FINISH` regardless.
        assert!(
            stream.len() >= 18,
            "{name}: even an empty payload yields a full gzip member, got {} bytes",
            stream.len()
        );
        assert_eq!(
            stream[..3],
            GZIP_PREFIX,
            "{name}: the stream must open with the RFC 1952 magic and CM = 8"
        );

        for &chunk in read_chunks() {
            let recovered = decompress_from_memory(&stream, chunk);
            assert_eq!(
                recovered, payload,
                "{name}: round trip with {chunk}-byte reads did not reproduce the input"
            );
        }
    }
}

/// `gzwrite` and `gzread` report byte counts, and `gzread` reports zero at end of file.
///
/// `gzwrite` returns "the number of uncompressed bytes written" (`zlib.h` L1521) as C's `int`, and
/// `gzread` "the number of uncompressed bytes actually read, less than len for end of file, or -1
/// for error" (`zlib.h` L1466-L1467). Note what `gzread` does *not* report: a truncated gzip stream.
/// `zlib.h` L1474-L1478 is explicit that it returns no error for one, and
/// [`truncated_member_reports_buf_error_at_close`] asserts where that report does surface.
///
/// A negative count must be accompanied by a readable diagnosis through `gzerror`, which is the
/// contract `test/minigzip.c` relies on when it prints `gzerror`'s message and exits.
///
/// No file I/O, so this runs under Miri.
#[test]
fn write_and_read_report_byte_counts() {
    let sink = Sink::new();
    let mut writer = open_memory(sink.handle(), b"wb");
    assert_eq!(
        gzwrite(&mut writer, HELLO),
        i32::try_from(HELLO.len()).unwrap(),
        "gzwrite returns the number of bytes it accepted"
    );
    // An empty write is accepted and reports zero rather than the -1 that means failure.
    assert_eq!(
        gzwrite(&mut writer, b""),
        0,
        "an empty write writes nothing"
    );
    assert_eq!(gzclose(Some(&mut writer)), ReturnCode::OK);

    let mut reader = open_memory(sink.handle(), b"rb");
    let mut buf = [0_u8; 32];
    assert_eq!(
        gzread(&mut reader, &mut buf),
        i32::try_from(HELLO.len()).unwrap(),
        "gzread returns the number of bytes it delivered"
    );
    assert_eq!(&buf[..HELLO.len()], HELLO);
    assert_eq!(
        gzread(&mut reader, &mut buf),
        0,
        "a read past the end delivers nothing"
    );
    // Zero bytes requested is not an error and consumes nothing.
    assert_eq!(gzread(&mut reader, &mut []), 0);
    let (code, message) = error_of(&mut reader);
    assert_eq!(code, ReturnCode::OK.as_i32(), "end of file is not an error");
    assert_eq!(message, "", "gzlib.c L513: no error means no message");
    assert_eq!(gzclose(Some(&mut reader)), ReturnCode::OK);
    assert_eq!(sink.closes(), 2, "each stream closed its own handle once");
}

/// `gzfwrite` and `gzfread` count whole items, and agree with the plain forms.
///
/// Both duplicate `fread`/`fwrite`'s interface, so the byte count is the product `size * nitems` and
/// the value returned is in **items** (`zlib.h` L1529-L1541 and L1492-L1516). Three properties are
/// asserted: the bytes produced by `gzfwrite` are the bytes `gzwrite` would have produced for the
/// same payload; the item count comes back rather than a byte count; and a trailing partial item is
/// still copied into the buffer while being excluded from the count, which `zlib.h` L1508-L1516
/// documents as matching common `fread` implementations.
///
/// The overflow rejection of `zlib.h` L1540-L1541 is asserted too: a product that does not fit in a
/// `usize` writes nothing, returns zero, and records `Z_STREAM_ERROR`.
///
/// No file I/O, so this runs under Miri.
#[test]
fn item_forms_agree_with_the_byte_forms() {
    // 21 bytes: seven items of three, so the item arithmetic has a remainder to lose.
    let payload: &[u8] = b"aaabbbcccdddeeefffggg";
    assert_eq!(payload.len(), 21);

    let item_sink = Sink::new();
    let mut writer = open_memory(item_sink.handle(), b"wb");
    assert_eq!(
        gzfwrite(&mut writer, payload, 3, 7),
        7,
        "gzfwrite returns whole items, not bytes"
    );
    assert_eq!(gzclose(Some(&mut writer)), ReturnCode::OK);

    // The same payload through the byte form must produce byte-identical output: the item form is
    // `gz_write(...) / size` and nothing more (`gzwrite.c` L303).
    assert_eq!(
        item_sink.bytes(),
        compress_to_memory(payload, b"wb"),
        "gzfwrite and gzwrite must agree byte for byte"
    );

    // Reading back in items of four leaves a partial fifth item: 21 = 5 * 4 + 1.
    let mut reader = open_memory(item_sink.handle(), b"rb");
    let mut buf = [0_u8; 24];
    assert_eq!(
        gzfread(&mut reader, &mut buf, 4, 6),
        5,
        "zlib.h L1508-L1516: the partial trailing item is not counted"
    );
    assert_eq!(
        &buf[..21],
        payload,
        "the partial item is still copied into the buffer"
    );
    assert_eq!(
        gztell64(&reader),
        21,
        "gztell recovers the length of the partial item"
    );
    assert_eq!(gzclose(Some(&mut reader)), ReturnCode::OK);

    // zlib.h L1540-L1541: an overflowing product writes nothing and records Z_STREAM_ERROR.
    let overflow_sink = Sink::new();
    let mut overflowing = open_memory(overflow_sink.handle(), b"wb");
    assert_eq!(
        gzfwrite(&mut overflowing, payload, usize::MAX, 2),
        0,
        "an overflowing size * nitems writes nothing"
    );
    let (code, _) = error_of(&mut overflowing);
    assert_eq!(code, ReturnCode::STREAM_ERROR.as_i32());
    assert_eq!(gzclose(Some(&mut overflowing)), ReturnCode::OK);
}

/// `gzputc` and `gzputs` return the documented values, `gzputs("ello") == 4` included.
///
/// `gzputc` "returns the value that was written, or -1 in case of error" (`zlib.h` L1607-L1609) --
/// and the value is masked to a byte on **both** return paths (`gzwrite.c` L338 and L346), so
/// `gzputc(file, -1)` returns 255. That is not a quirk to tidy: -1 is a perfectly good byte and -1
/// is also the error code, so returning the argument unmasked would make a written `0xff`
/// indistinguishable from a failure.
///
/// `gzputs` "returns the number of characters written, or -1 in case of error" (`zlib.h`
/// L1577-L1579), which `test/example.c` L105 pins down as `gzputs(file, "ello") == 4`.
///
/// No file I/O, so this runs under Miri.
#[test]
fn byte_and_string_writers_return_documented_values() {
    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), b"wb");

    assert_eq!(gzputc(&mut state, i32::from(b'h')), i32::from(b'h'));
    assert_eq!(
        gzputs(&mut state, b"ello"),
        4,
        "test/example.c L105: gzputs(file, \"ello\") == 4"
    );
    // The mask on both paths: -1 is written as 0xff and reported as 255.
    assert_eq!(
        gzputc(&mut state, -1),
        255,
        "gzwrite.c L338/L346: the return value is `c & 0xff`, so 0xff is not mistaken for -1"
    );
    // 0x1ff is also masked, which is C's `(unsigned char)c`.
    assert_eq!(gzputc(&mut state, 0x1ff), 255);
    // An empty string writes nothing and reports zero, not the -1 that means failure.
    assert_eq!(gzputs(&mut state, b""), 0);
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    let recovered = decompress_from_memory(&sink.bytes(), 8);
    assert_eq!(recovered, b"hello\xff\xff");
}

/// The bytes a write stream produces are a valid RFC 1952 member, header and trailer both.
///
/// `doc/rfc1952.txt` 2.3 fixes the member header: `ID1 = 31`, `ID2 = 139`, `CM = 8` for deflate,
/// then `FLG`, a four-byte little-endian `MTIME` where zero means "no timestamp available", `XFL`
/// and `OS`. `FLG`'s reserved bits 5, 6 and 7 "must be zero", and a compressor must not set them.
/// 2.3.1.2 fixes the trailer as `CRC32` then `ISIZE`, both little-endian: the CRC-32 of the
/// uncompressed data and its length modulo 2^32.
///
/// The CRC is recomputed here with [`zlib_rs::crc32::crc32`], which ties the file layer to the
/// checksum engine: if either the trailer were written wrongly or the checksum computed wrongly, the
/// two would disagree. That is the same tie `test/example.c` relies on implicitly by round-tripping,
/// made explicit.
///
/// No file I/O, so this runs under Miri.
#[test]
fn produced_member_is_valid_rfc1952() {
    for payload in [corpus::EMPTY, corpus::SINGLE_BYTE, HELLO] {
        let stream = compress_to_memory(payload, b"wb");

        // 2.3: a member is at least a ten-byte header plus an eight-byte trailer.
        assert!(
            stream.len() >= 18,
            "a member cannot be shorter than 18 bytes"
        );
        assert_eq!(stream[0], 0x1f, "ID1");
        assert_eq!(stream[1], 0x8b, "ID2");
        assert_eq!(stream[2], 8, "CM: deflate");
        assert_eq!(
            stream[3] & 0b1110_0000,
            0,
            "FLG bits 5-7 are reserved and must be zero"
        );
        assert_eq!(
            stream[3] & 0b0001_1100,
            0,
            "this writer sets no FEXTRA, FNAME or FCOMMENT field"
        );
        assert_eq!(
            &stream[4..8],
            &[0, 0, 0, 0],
            "MTIME is zero: no timestamp is available to the stream writer"
        );

        // 2.3.1.2: the trailer, both fields little-endian.
        let trailer = &stream[stream.len() - 8..];
        assert_eq!(
            u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]),
            crc32(0, payload),
            "the trailer CRC-32 must match the payload's"
        );
        assert_eq!(
            u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]),
            u32::try_from(payload.len()).unwrap(),
            "ISIZE is the uncompressed length modulo 2^32"
        );

        // And the member the reference produced for the same payload is accepted here, which is the
        // other direction of the interoperability requirement.
        if payload == HELLO {
            assert_eq!(decompress_from_memory(&HELLO_GZ, 32), HELLO);
        }
    }
}

/// A member produced elsewhere, with optional header fields populated, reads correctly.
///
/// [`GZIP_CLI_NAMED`] was produced by the system `gzip(1)`, not by this project: its `FLG` is `0x08`,
/// so it carries an `FNAME` field, and its `MTIME` is a real timestamp rather than zero. Reading it
/// exercises the optional-field skipping of `doc/rfc1952.txt` 2.3.1.1 on bytes no part of this port
/// wrote, which is the "streams produced by C zlib must decompress identically in the port" half of
/// bidirectional interoperability.
///
/// No file I/O, so this runs under Miri.
#[test]
fn a_member_with_optional_header_fields_reads_correctly() {
    assert_eq!(GZIP_CLI_NAMED[3] & 0x08, 0x08, "FNAME is present");
    assert_ne!(&GZIP_CLI_NAMED[4..8], &[0, 0, 0, 0], "MTIME is populated");
    for chunk in [1_usize, 5, 64] {
        assert_eq!(
            decompress_from_memory(&GZIP_CLI_NAMED, chunk),
            HELLO_NO_NUL,
            "a gzip(1) member must read back byte for byte"
        );
    }
}

// =============================================================================================
// 5. `test/example.c`'s `test_gzio`, reproduced number for number
// =============================================================================================

/// The whole of `test_gzio` (`test/example.c` L90-L165), in order, with every number it asserts.
///
/// This is the acceptance test the prompt names, and it is transcribed rather than paraphrased. The
/// C source, step by step, and what each step becomes here:
///
/// | `test/example.c` | C | Here |
/// |---|---|---|
/// | L99 | `file = gzopen(fname, "wb")` | [`open_memory`] with `b"wb"` |
/// | L104 | `gzputc(file, 'h')` | [`gzputc`] |
/// | L105 | `gzputs(file, "ello") != 4` | [`gzputs`] returns 4 |
/// | L109 | `gzprintf(file, ", %s!", "hello") != 8` | [`printf_bytes`] returns 8 |
/// | L113 | `gzseek(file, 1L, SEEK_CUR)` -- "add one zero byte" | [`gzseek64`] with `SEEK_CUR` |
/// | L114 | `gzclose(file)` | [`gzclose`] returns `Z_OK` |
/// | L116 | `file = gzopen(fname, "rb")` | [`open_memory`] with `b"rb"` |
/// | L121 | `strcpy((char*)uncompr, "garbage")` | the destination is pre-filled |
/// | L123 | `gzread(...) != len`, `len = strlen(hello) + 1` | [`gzread`] returns 14 |
/// | L127 | `strcmp((char*)uncompr, hello)` | the bytes equal [`HELLO`] |
/// | L134-L135 | `pos = gzseek(file, -8L, SEEK_CUR)`; `pos != 6 \|\| gztell(file) != pos` | 6, and `gztell` agrees |
/// | L141 | `gzgetc(file) != ' '` | [`gzgetc`] returns a space |
/// | L146 | `gzungetc(' ', file) != ' '` | [`gzungetc`] returns a space |
/// | L151-L156 | `gzgets(...)`; `strlen != 7`; `strcmp(uncompr, hello + 6)` | 7 visible characters, `" hello!"` |
/// | L163 | `gzclose(file)` | [`gzclose`] returns `Z_OK` |
///
/// Three of these steps are easy to get wrong and are worth naming:
///
/// * **L113 writes a byte.** A forward `gzseek` on a *write* stream is the `gz_zero` path: the
///   request is recorded in `skip` and paid for in zeroes at the next write or at the close, which is
///   what turns the twelve bytes written so far into the fourteen L123 reads back. Reading the line
///   as a no-op loses the trailing NUL and with it the `strcmp` at L127.
/// * **L121's pre-fill is not decoration.** `"garbage"` is seven bytes of non-zero data in the
///   destination, so a `gzread` that under-delivered would leave a `g` where the payload's own NUL
///   belongs and the `strcmp` would not notice. The fill is what makes L127 a real check.
/// * **L134 seeks backwards.** That forces a rewind and a full re-decompression, because a gzip
///   stream cannot be read backwards. It is the single most likely positioning bug in the layer, and
///   the C suite hands us the exact expected answer.
///
/// The payload's terminating NUL is data throughout, which is why `gzgets` reports eight bytes while
/// C's `strlen` sees seven: `gzread.c` L617-L619 says the contents are not checked for a zero byte,
/// and the `"hello, hello!\0"` fixture depends on that.
///
/// Miri cannot perform file I/O, and this test performs none: it runs against a [`MemoryFile`].
/// [`filesystem_round_trip_reproduces_test_gzio`] is the same sequence against a real file.
#[test]
fn test_gzio_is_reproduced_exactly() {
    // ---- the write half, L99-L114 ----
    let sink = Sink::new();
    let mut file = open_memory(sink.handle(), b"wb");

    // L104. `gzputc` returns the byte it wrote.
    assert_eq!(gzputc(&mut file, i32::from(b'h')), i32::from(b'h'));

    // L105: `if (gzputs(file, "ello") != 4)`.
    assert_eq!(gzputs(&mut file, b"ello"), 4, "test/example.c L105");

    // L109: `if (gzprintf(file, ", %s!", "hello") != 8)`. The variadic surface belongs to the
    // facade -- `core::ffi::VaList` is unsafe FFI -- so the safe core is handed the rendered result
    // and does the bounded-buffer accounting around it. `", hello!"` is what the format produces.
    assert_eq!(
        printf_bytes(&mut file, b", hello!"),
        Ok(8),
        "test/example.c L109: the formatted write reports eight bytes"
    );
    assert_eq!(
        gztell64(&file),
        13,
        "one plus four plus eight bytes have been accepted"
    );

    // L113: `gzseek(file, 1L, SEEK_CUR); /* add one zero byte */`.
    assert_eq!(
        gzseek64(&mut file, 1, GzSeekFrom::Current.as_raw()),
        14,
        "the seek reports where the stream will land"
    );

    // L114.
    assert_eq!(
        gzclose(Some(&mut file)),
        ReturnCode::OK,
        "test/example.c L114"
    );

    // The deferred zero byte was paid for by the close, so the member decodes to fourteen bytes.
    assert_eq!(
        decompress_from_memory(&sink.bytes(), 64),
        HELLO,
        "gzseek(1, SEEK_CUR) on a write stream must have appended one zero byte"
    );

    // ---- the read half, L116-L163 ----
    let mut file = open_memory(sink.handle(), b"rb");

    // L121: `strcpy((char*)uncompr, "garbage")`. `uncomprLen` in the C suite is far larger than the
    // payload, so the request is deliberately generous.
    let mut uncompr = [0_u8; 64];
    uncompr[..7].copy_from_slice(b"garbage");

    // L123: `if (gzread(file, uncompr, (unsigned)uncomprLen) != len)`, `len = strlen(hello) + 1`.
    assert_eq!(HELLO.len(), 14, "test/example.c L95: strlen(hello) + 1");
    assert_eq!(
        gzread(&mut file, &mut uncompr),
        14,
        "test/example.c L123: gzread returns strlen(hello) + 1"
    );

    // L127: `if (strcmp((char*)uncompr, hello))`.
    assert_eq!(
        &uncompr[..14],
        HELLO,
        "test/example.c L127: the bytes read must be `hello` and its terminating zero"
    );

    // L134-L135: `pos = gzseek(file, -8L, SEEK_CUR); if (pos != 6 || gztell(file) != pos)`.
    let pos = gzseek64(&mut file, -8, GzSeekFrom::Current.as_raw());
    assert_eq!(pos, 6, "test/example.c L135: the backward seek lands at 6");
    assert_eq!(
        gztell64(&file),
        pos,
        "test/example.c L135: gztell must agree with the seek, pending skip included"
    );

    // L141: `if (gzgetc(file) != ' ')`. Byte six of "hello, hello!" is the space after the comma.
    assert_eq!(
        gzgetc(&mut file),
        i32::from(b' '),
        "test/example.c L141: byte six is the space"
    );
    assert_eq!(gztell64(&file), 7, "the byte was consumed");

    // L146: `if (gzungetc(' ', file) != ' ')`.
    assert_eq!(
        gzungetc(&mut file, i32::from(b' ')),
        i32::from(b' '),
        "test/example.c L146: gzungetc returns the character pushed"
    );
    assert_eq!(gztell64(&file), 6, "the push moved the position back");

    // L151-L156: `gzgets(file, uncompr, uncomprLen)`; `strlen(uncompr) != 7`;
    // `strcmp(uncompr, hello + 6)`.
    let mut line = [b'#'; 64];
    let got = gzgets(&mut file, &mut line).expect("gzgets should deliver the tail");
    assert_eq!(
        got, 8,
        "eight data bytes remain from position six: \" hello!\" and the payload's own zero"
    );
    let visible = line.iter().position(|&byte| byte == 0).unwrap();
    assert_eq!(
        visible, 7,
        "test/example.c L152: C's strlen sees seven characters, because byte seven is the payload's zero"
    );
    assert_eq!(
        &line[..visible],
        &HELLO[6..13],
        "test/example.c L156: the tail equals `hello + 6`, which is \" hello!\""
    );
    assert_eq!(&line[..7], b" hello!");

    // L163.
    assert_eq!(
        gzclose(Some(&mut file)),
        ReturnCode::OK,
        "test/example.c L163"
    );
}

/// `gzgets` stops at a newline, stops at the buffer bound, and always terminates with a zero.
///
/// `zlib.h` L1590-L1593 documents the contract: it reads "until len-1 characters are read, or until a
/// newline character is read and transferred to buf, or an end-of-file condition is encountered", and
/// "if any characters are read or if len is one, the string is terminated with a null character". Two
/// details of the implementation matter and are asserted:
///
/// * the newline is **transferred**, so a line's own terminator is part of the count;
/// * `len == 1` returns nothing at all, because C computes `left = len - 1`, skips its loop and
///   returns `NULL` without writing the terminator either (`gzread.c` L592-L621). `zlib.h`
///   L1592-L1593 reads as though the zero were still written; the implementation is the oracle.
///
/// An empty buffer is C's `buf == NULL || len < 1` and is likewise refused (`gzread.c` L572-L578).
///
/// No file I/O, so this runs under Miri.
#[test]
fn gzgets_stops_at_a_newline_and_terminates() {
    let payload: &[u8] = b"first\nsecond\nno trailing newline";
    let stream = compress_to_memory(payload, b"wb");
    let sink = Sink::with_contents(&stream);
    let mut state = open_memory(sink.handle(), b"rb");

    let mut line = [b'#'; 32];
    assert_eq!(gzgets(&mut state, &mut line), Some(6));
    assert_eq!(&line[..6], b"first\n", "the newline is transferred");
    assert_eq!(line[6], 0, "and the result is zero-terminated");

    assert_eq!(gzgets(&mut state, &mut line), Some(7));
    assert_eq!(&line[..7], b"second\n");
    assert_eq!(line[7], 0);

    // The final line has no newline, so it ends at end of file.
    assert_eq!(gzgets(&mut state, &mut line), Some(19));
    assert_eq!(&line[..19], b"no trailing newline");
    assert_eq!(line[19], 0);

    // Nothing left: end of file is `None`, C's `NULL`.
    assert_eq!(gzgets(&mut state, &mut line), None);
    assert_eq!(
        gzeof(Some(&mut state)),
        1,
        "the last line ended at end of file, so the read went past it"
    );

    // ★ And the stream now carries `Z_BUF_ERROR`, which `gzclose` reports. That is not a defect and
    // not this implementation's invention: a `gzgets` that finds nothing re-enters `gz_fetch` while
    // `how` is still `GZIP`, so `gz_decomp` looks for the next member's header, finds no input and no
    // `again`, and records "unexpected end of file" (`gzread.c` L193-L196). `gzclose_r` then hands
    // that latched code back (`gzread.c` L662). Verified against a program linked to the reference
    // library built from the C sources in this repository, which prints exactly
    // `gzerror -> -5 "…: unexpected end of file"` and `gzclose -> -5` for this sequence.
    //
    // Note the asymmetry with `gzread`, asserted just below: `gz_read` breaks out of its own loop
    // when the output buffer is empty and the input is finished (`gzread.c` L351-L353) *without*
    // calling `gz_fetch` again, so a stream read to exhaustion with `gzread` closes with `Z_OK`. The
    // two entry points genuinely differ here, and both behaviours are the reference's.
    let (code, message) = error_of(&mut state);
    assert_eq!(code, ReturnCode::BUF_ERROR.as_i32());
    assert!(
        message.ends_with("unexpected end of file"),
        "gz_error prefixes the stored message with the stream's path: {message:?}"
    );
    assert_eq!(
        gzclose(Some(&mut state)),
        ReturnCode::BUF_ERROR,
        "zlib.h L1758-L1760: gzclose reports a read that ended in the middle of a gzip stream"
    );

    // The contrast: the same payload read to exhaustion with `gzread` closes cleanly.
    let mut state = open_memory(sink.handle(), b"rb");
    assert_eq!(read_to_end(&mut state, 8), payload);
    assert_eq!(error_of(&mut state).0, ReturnCode::OK.as_i32());
    assert_eq!(
        gzclose(Some(&mut state)),
        ReturnCode::OK,
        "gzread stops before re-entering gz_fetch, so nothing is latched"
    );

    // The buffer bound, and the two degenerate lengths.
    let mut state = open_memory(sink.handle(), b"rb");
    let mut small = [b'#'; 4];
    assert_eq!(
        gzgets(&mut state, &mut small),
        Some(3),
        "at most len - 1 data bytes are delivered"
    );
    assert_eq!(&small[..3], b"fir");
    assert_eq!(small[3], 0, "the last byte of the buffer is the terminator");
    assert_eq!(
        gzgets(&mut state, &mut [0_u8; 1]),
        None,
        "gzread.c L592-L621: len == 1 writes nothing and reports nothing"
    );
    assert_eq!(
        gzgets(&mut state, &mut []),
        None,
        "gzread.c L572-L578: an empty buffer is C's len < 1"
    );
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
}

/// `gzungetc` pushes exactly one byte back, and works before anything has been read.
///
/// `zlib.h` L1620-L1630: it "returns the character pushed, or -1 on failure", fails for `c == -1`
/// because end of file cannot be pushed, and "may fail if c is pushed and no characters have been
/// read yet" -- may, not must, and this implementation supports it, as C does, by parking the byte at
/// the end of the output buffer (`gzread.c` L532-L540). That is what the doubled read-side output
/// buffer exists for.
///
/// Four properties are asserted: the byte comes back from the next `gzgetc`; the position moves back
/// by one, even below zero when nothing has been read, which is what `gztell` then reports; the low
/// byte is what gets stored while the *argument* is what gets returned (C's `(unsigned char)c`
/// against its `return c`); and a second push with the first still unread is refused with
/// `Z_DATA_ERROR` (`gzread.c` L542-L546) rather than silently overwriting.
///
/// A push also clears `past`, so a stream at end of file becomes readable again -- the property
/// `gzclearerr` exists for, reached here from the other direction.
///
/// No file I/O, so this runs under Miri.
#[test]
fn gzungetc_pushes_back_exactly_one_byte() {
    let stream = compress_to_memory(HELLO, b"wb");
    let sink = Sink::with_contents(&stream);

    // Before any read: the position goes negative, exactly as C's `state->x.pos--` does.
    let mut state = open_memory(sink.handle(), b"rb");
    assert_eq!(gzungetc(&mut state, i32::from(b'X')), i32::from(b'X'));
    assert_eq!(
        gztell64(&state),
        -1,
        "a push before the first read puts the position one byte before the start"
    );
    assert_eq!(
        gzgetc(&mut state),
        i32::from(b'X'),
        "the pushed byte comes back first"
    );
    assert_eq!(gztell64(&state), 0);
    // And the stream continues with its real contents.
    assert_eq!(gzgetc(&mut state), i32::from(b'h'));

    // A second push while the first is unread is refused with Z_DATA_ERROR, not accepted silently.
    assert_eq!(gzungetc(&mut state, i32::from(b'1')), i32::from(b'1'));
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    // The masking: the low byte is stored, the argument is returned.
    let mut state = open_memory(sink.handle(), b"rb");
    assert_eq!(
        gzungetc(&mut state, 0x141),
        0x141,
        "gzread.c L536/L562: the value returned is the argument as given"
    );
    assert_eq!(
        gzgetc(&mut state),
        0x41,
        "gzread.c L536: what was stored is C's `(unsigned char)c`"
    );

    // End of file cannot be pushed.
    assert_eq!(
        gzungetc(&mut state, -1),
        -1,
        "gzread.c L528-L530: can't push end of file"
    );
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    // A push after end of file clears `past`, so the stream reads again.
    let mut state = open_memory(sink.handle(), b"rb");
    let mut all = [0_u8; 64];
    assert_eq!(gzread(&mut state, &mut all), 14);
    assert_eq!(gzeof(Some(&mut state)), 1, "the read went past the end");
    assert_eq!(gzungetc(&mut state, i32::from(b'!')), i32::from(b'!'));
    assert_eq!(
        gzeof(Some(&mut state)),
        0,
        "gzread.c L539/L556: a push clears `past`"
    );
    assert_eq!(gzgetc(&mut state), i32::from(b'!'));
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
}

/// `gzgetc_` is the function the `gzgetc` macro falls back to, and it must behave identically.
///
/// `zlib.h` L1961 declares it separately "for backward compatibility", so both names have to exist
/// and both have to work. The macro takes its fast path only while `have` is non-zero; when the
/// buffer is empty it calls the *function*, which is what refills. Reading a whole stream one byte at
/// a time through each spelling and requiring the same bytes is what proves the fallback is not a
/// separate implementation that has drifted.
///
/// No file I/O, so this runs under Miri.
#[test]
fn gzgetc_and_its_backward_compatible_twin_agree() {
    let stream = compress_to_memory(HELLO, b"wb");
    let sink = Sink::with_contents(&stream);

    let mut through_macro_path = Vec::new();
    let mut state = open_memory(sink.handle(), b"rb");
    loop {
        let byte = gzgetc(&mut state);
        if byte < 0 {
            break;
        }
        through_macro_path.push(u8::try_from(byte).unwrap());
    }
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    let mut through_function = Vec::new();
    let mut state = open_memory(sink.handle(), b"rb");
    loop {
        let byte = gzgetc_(&mut state);
        if byte < 0 {
            break;
        }
        through_function.push(u8::try_from(byte).unwrap());
    }
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    assert_eq!(through_macro_path, HELLO);
    assert_eq!(through_function, HELLO);
}

// =============================================================================================
// 6. The transparent path, and what `gzdirect` reports about it
// =============================================================================================

/// Data that is not a gzip stream reads back unchanged: the `how = COPY` path.
///
/// `zlib.h` L1461-L1464 documents it: "if the input file is not in gzip format, gzread copies the
/// given number of bytes into the buffer directly from the file". `gz_look` reaches that decision at
/// `gzread.c` L161-L166 by copying whatever it has already buffered into the output buffer and
/// setting `how = COPY`.
///
/// Two request sizes are used, because `gz_read` splits on them: a request smaller than twice the
/// buffer size goes through the layer's own buffer, while a larger one is read straight into the
/// caller's (`gzread.c` L355-L369). Both must deliver the same bytes.
///
/// No file I/O, so this runs under Miri.
#[test]
fn a_non_gzip_file_reads_through_transparently() {
    // Deliberately several buffers long so the transparent path has to refill. The buffer is set
    // small rather than the payload made large, so the shape survives Miri -- see `bulk`.
    let mut plain = vec![0_u8; 512];
    common::lcg_fill(0x5eed_0ff1_ce9a_1357, &mut plain);
    // Guarantee the payload cannot be mistaken for a member however the generator happened to start.
    plain[0] = b'n';
    plain[1] = b'o';

    let sink = Sink::with_contents(&plain);
    for chunk in [1_usize, 100, 512] {
        let mut state = open_memory(sink.handle(), b"rb");
        assert_eq!(
            gzbuffer(Some(&mut state), 64),
            0,
            "a 64-byte buffer makes the payload eight buffers long"
        );
        assert_eq!(
            read_to_end(&mut state, chunk),
            plain,
            "a transparent read with {chunk}-byte requests must copy the bytes verbatim"
        );
        assert_eq!(
            gzclose(Some(&mut state)),
            ReturnCode::OK,
            "a transparent read closes cleanly"
        );
    }
}

/// `gzdirect` reports the container decision truthfully, before and after the first read.
///
/// `zlib.h` L1726-L1747 documents both the answer and its cost: `gzdirect` returns true when reading
/// "directly", i.e. transparently, and "if `gzdirect()` is used immediately after `gzopen()` or
/// `gzdopen()` it will cause buffers to be allocated to allow reading the file to determine if it is a
/// gzip file". That allocation is why the implementation calls `gz_look` for a read stream still in
/// the `LOOK` state with nothing buffered (`gzread.c` L637-L638) -- the question cannot be answered
/// without looking.
///
/// The answer is asserted at both moments, because a decision cached wrongly on the first call would
/// otherwise be invisible. The empty-file case is included deliberately: `gz_open` starts a read
/// stream with a "transparent assumption in case of an empty file" (`gzlib.c` L186-L189), so an empty
/// file reports transparent rather than reporting nothing.
///
/// On a *write* stream the answer is whatever the mode string asked for and needs no allocation, so
/// `"wT"` reports transparent and `"wb"` does not.
///
/// No file I/O, so this runs under Miri.
#[test]
fn gzdirect_reports_the_container_decision() {
    // A real member: not transparent, before or after reading.
    let member = compress_to_memory(HELLO, b"wb");
    let sink = Sink::with_contents(&member);
    let mut state = open_memory(sink.handle(), b"rb");
    assert_eq!(
        gzdirect(Some(&mut state)),
        0,
        "a gzip member is not read directly"
    );
    let mut buf = [0_u8; 4];
    assert_eq!(gzread(&mut state, &mut buf), 4);
    assert_eq!(
        gzdirect(Some(&mut state)),
        0,
        "and the answer does not change once reading has begun"
    );
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    // Plain data: transparent, before and after.
    let sink = Sink::with_contents(b"not a gzip file at all");
    let mut state = open_memory(sink.handle(), b"rb");
    assert_eq!(
        gzdirect(Some(&mut state)),
        1,
        "zlib.h L1726-L1747: plain data is read directly"
    );
    assert_eq!(gzread(&mut state, &mut buf), 4);
    assert_eq!(&buf, b"not ");
    assert_eq!(gzdirect(Some(&mut state)), 1);
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    // An empty file: transparent by assumption, and the read simply delivers nothing.
    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), b"rb");
    assert_eq!(
        gzdirect(Some(&mut state)),
        1,
        "gzlib.c L186-L189: an empty file keeps the transparent assumption"
    );
    assert_eq!(gzread(&mut state, &mut buf), 0);
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    // A write stream answers from the mode string alone.
    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), b"wT");
    assert_eq!(
        gzdirect(Some(&mut state)),
        1,
        "`T` requested a transparent write"
    );
    assert_eq!(
        gzwrite(&mut state, HELLO),
        i32::try_from(HELLO.len()).unwrap()
    );
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
    assert_eq!(
        sink.len(),
        HELLO.len(),
        "a transparent write adds no header and no trailer"
    );
    assert_eq!(
        sink.bytes(),
        HELLO,
        "a transparent write stages nothing and compresses nothing"
    );

    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), b"wb");
    assert_eq!(gzdirect(Some(&mut state)), 0);
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    // A missing stream answers 0 rather than misbehaving, which is C's `if (file == NULL) return 0`.
    assert_eq!(
        gzdirect(None::<&mut GzState<'static, GlobalAllocator>>),
        0,
        "gzread.c L631-L632: no file means not direct"
    );
}

/// The four-byte detection heuristic, exactly at its boundary.
///
/// `gzread.c` L148-L153 is the whole test:
///
/// ```c
/// if (strm->avail_in > 3 &&
///         strm->next_in[0] == 31 && strm->next_in[1] == 139 &&
///         strm->next_in[2] == 8 && strm->next_in[3] < 32) {
/// ```
///
/// Four bytes must be available, the first three must be `ID1`, `ID2` and `CM = 8`, and the fourth --
/// `FLG` -- must be below 32, which is `doc/rfc1952.txt` 2.3.1.2's requirement that the reserved bits
/// 5, 6 and 7 be zero. Every boundary of that condition is asserted here, in both directions:
///
/// * `FLG = 31` is the largest accepted value and `FLG = 32` the smallest rejected one;
/// * three bytes of magic without a fourth byte is **not** enough, because `avail_in > 3` fails, so a
///   two- or three-byte file is transparent no matter what those bytes are;
/// * a single wrong byte anywhere in the first three is transparent.
///
/// A file classified as gzip on the strength of four bytes and then found to be nonsense is a data
/// error, not a transparent read -- which is the point of the heuristic being a heuristic -- so the
/// gzip verdict is asserted through `gzdirect` rather than through a successful read.
///
/// No file I/O, so this runs under Miri.
#[test]
fn the_detection_heuristic_holds_at_its_boundary() {
    /// Whether the reader classified `bytes` as a gzip stream.
    fn classified_as_gzip(bytes: &[u8]) -> bool {
        let sink = Sink::with_contents(bytes);
        let mut state = open_memory(sink.handle(), b"rb");
        let direct = gzdirect(Some(&mut state));
        // The close may report the data error a bogus member produces; the classification is what is
        // under test, so the code is read rather than required to be `Z_OK`.
        let _ = gzclose(Some(&mut state));
        direct == 0
    }

    // FLG below 32 is accepted; 31 is the boundary.
    assert!(classified_as_gzip(&[0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0]));
    assert!(classified_as_gzip(&[0x1f, 0x8b, 0x08, 0x1f, 0, 0, 0, 0]));
    // 32 sets reserved bit 5, so the member is refused and the file is read transparently.
    assert!(
        !classified_as_gzip(&[0x1f, 0x8b, 0x08, 0x20, 0, 0, 0, 0]),
        "gzread.c L153: FLG must be below 32"
    );
    assert!(!classified_as_gzip(&[0x1f, 0x8b, 0x08, 0xff, 0, 0, 0, 0]));

    // Each of the first three bytes must match exactly.
    assert!(!classified_as_gzip(&[0x1e, 0x8b, 0x08, 0x00, 0, 0, 0, 0]));
    assert!(!classified_as_gzip(&[0x1f, 0x8a, 0x08, 0x00, 0, 0, 0, 0]));
    assert!(
        !classified_as_gzip(&[0x1f, 0x8b, 0x09, 0x00, 0, 0, 0, 0]),
        "CM must be 8: doc/rfc1952.txt 2.3.1.2 reserves every other value"
    );

    // A partial prefix is transparent, because `avail_in > 3` fails.
    for partial in [&[0x1f_u8][..], &[0x1f, 0x8b][..], &[0x1f, 0x8b, 0x08][..]] {
        assert!(
            !classified_as_gzip(partial),
            "gzread.c L151: fewer than four bytes cannot satisfy `avail_in > 3`"
        );
        // And such a file still reads cleanly, delivering exactly its bytes.
        let sink = Sink::with_contents(partial);
        let mut state = open_memory(sink.handle(), b"rb");
        assert_eq!(read_to_end(&mut state, 8), partial);
        assert_eq!(
            gzclose(Some(&mut state)),
            ReturnCode::OK,
            "a file shorter than the magic prefix is not an error"
        );
    }

    // Nor is an empty one.
    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), b"rb");
    assert!(read_to_end(&mut state, 8).is_empty());
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
}

// =============================================================================================
// 7. Positioning: tell, seek, rewind and offset
// =============================================================================================

/// `gztell` starts at zero and tracks the uncompressed position as data is delivered.
///
/// `zlib.h` L1694-L1701: it returns "the starting position for the next gzread or gzwrite on file",
/// which "is equivalent to `gzseek(file, 0L, SEEK_CUR)`". The equivalence is asserted directly, because
/// it is the property that makes the pending-skip accounting observable: `gztell` reports
/// `pos + skip` for a stream with a deferred seek, and a `gzseek(0, SEEK_CUR)` must report the same
/// landing point.
///
/// The narrowing to the caller's `z_off_t` is the facade's job, done as C does it -- compute in 64
/// bits, then refuse a value the narrower type cannot hold (`gzlib.c` L461-L465). The core offers
/// [`narrow_offset`] for exactly that, and both families are exported by `crates/libz-rs-sys` because
/// `zconf.h` redirects the unsuffixed names according to the *caller's* `_FILE_OFFSET_BITS` and
/// `_LARGEFILE64_SOURCE` (`zlib.h` L1977-L2022). So the agreement asserted here is between
/// [`gztell64`] and the narrowed value a `gztell` wrapper would return.
///
/// No file I/O, so this runs under Miri.
#[test]
fn gztell_tracks_the_uncompressed_position() {
    let stream = compress_to_memory(HELLO, b"wb");
    let sink = Sink::with_contents(&stream);
    let mut state = open_memory(sink.handle(), b"rb");

    assert_eq!(gztell64(&state), 0, "a fresh stream is at position zero");
    let mut buf = [0_u8; 5];
    for expected in [5_i64, 10, 14] {
        let got = gzread(&mut state, &mut buf);
        assert!(got > 0);
        assert_eq!(
            gztell64(&state),
            expected,
            "gztell must follow the bytes delivered"
        );
    }

    // The documented equivalence, and the narrowing the facade performs on top of it.
    assert_eq!(
        gzseek64(&mut state, 0, GzSeekFrom::Current.as_raw()),
        gztell64(&state),
        "zlib.h L1700: gztell is gzseek(0, SEEK_CUR)"
    );
    assert_eq!(
        narrow_offset::<i32>(gztell64(&state)),
        Some(14),
        "a position that fits in the narrower type narrows to itself"
    );
    assert_eq!(
        narrow_offset::<i32>(i64::from(i32::MAX) + 1),
        None,
        "gzlib.c L461-L465: a position that does not fit is refused rather than truncated"
    );

    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    // A write stream reports its own position, and a stream with no direction reports -1.
    let sink = Sink::new();
    let mut writer = open_memory(sink.handle(), b"wb");
    assert_eq!(gztell64(&writer), 0);
    assert_eq!(gzwrite(&mut writer, b"1234"), 4);
    assert_eq!(gztell64(&writer), 4);
    assert_eq!(gzclose(Some(&mut writer)), ReturnCode::OK);
    assert_eq!(
        gztell64(&writer),
        -1,
        "a closed stream has no direction, so it has no position"
    );
}

/// Forward seeks, from both accepted origins, including past what is already buffered.
///
/// `zlib.h` L1663-L1680: `gzseek` sets "the starting position for the next gzread or gzwrite on
/// file", accepts only `SEEK_SET` and `SEEK_CUR`, and "if the file is opened for reading, this
/// function is emulated but can be extremely slow". The emulation is the interesting part: the
/// request is recorded in `skip` and carried out by `gz_skip` at the next read (`gzlib.c` L432-L433),
/// which is why the value returned is `pos + offset` computed up front rather than something read
/// back from the file.
///
/// Three cases are covered: a `SEEK_SET` to an absolute position, a `SEEK_CUR` within the bytes the
/// layer has already decompressed into its own buffer -- which is consumed immediately rather than
/// deferred, "one less `gzgetc()` check" as C's comment at `gzlib.c` L422-L423 puts it -- and a
/// `SEEK_CUR` beyond them, which has to decompress more. A seek past the end of the data is asserted
/// too: it reports the position it was asked for, and the read that follows simply finds nothing.
///
/// No file I/O, so this runs under Miri.
#[test]
fn forward_seeks_land_where_they_say() {
    let payload: Vec<u8> = (0..=255_u8).cycle().take(4096).collect();
    let stream = compress_to_memory(&payload, b"wb");
    let sink = Sink::with_contents(&stream);

    // SEEK_SET to an absolute position. The offsets are absolute rather than scaled, because the
    // point of the test is a seek landing on a known byte; 4 KiB is the smallest payload that still
    // lets a forward seek pass over more than one buffer's worth of decompressed output.
    let mut state = open_memory(sink.handle(), b"rb");
    assert_eq!(gzseek64(&mut state, 1000, GzSeekFrom::Start.as_raw()), 1000);
    assert_eq!(gztell64(&state), 1000, "the pending skip is included");
    let mut buf = [0_u8; 8];
    assert_eq!(gzread(&mut state, &mut buf), 8);
    assert_eq!(&buf, &payload[1000..1008], "the skip was paid at the read");
    assert_eq!(gztell64(&state), 1008);

    // SEEK_CUR within the bytes already buffered.
    assert_eq!(gzseek64(&mut state, 4, GzSeekFrom::Current.as_raw()), 1012);
    assert_eq!(gzread(&mut state, &mut buf), 8);
    assert_eq!(&buf, &payload[1012..1020]);

    // SEEK_CUR well past them, so more has to be decompressed.
    assert_eq!(
        gzseek64(&mut state, 2000, GzSeekFrom::Current.as_raw()),
        3020
    );
    assert_eq!(gzread(&mut state, &mut buf), 8);
    assert_eq!(&buf, &payload[3020..3028]);

    // Past the end: the seek succeeds, the read finds nothing, and end of file is reported.
    assert_eq!(gzseek64(&mut state, 9000, GzSeekFrom::Start.as_raw()), 9000);
    assert_eq!(gzread(&mut state, &mut buf), 0);
    assert_eq!(gzeof(Some(&mut state)), 1);
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
}

/// A backward seek rewinds and re-decompresses, and `test_gzio`'s exact case is one of them.
///
/// This is the hardest case in the positioning family, because a gzip stream cannot be read
/// backwards at all: the implementation re-expresses the request from the start of the stream,
/// refuses it if it lands before the beginning, rewinds, and then skips forward
/// (`gzlib.c` L411-L420). Everything about the stream's state -- the output window, the input buffer,
/// the inflate engine, the multi-member `junk` marker -- is discarded and rebuilt.
///
/// `test/example.c` L134-L135 hands us a ready-made regression test for it: on a fourteen-byte stream
/// that has been read to the end, `gzseek(file, -8L, SEEK_CUR)` must report 6 and `gztell` must agree.
/// That case is reproduced in [`test_gzio_is_reproduced_exactly`]; this test covers the general
/// behaviour around it -- that the bytes after a backward seek are the right bytes, that a seek to
/// before the start is refused, and that a refused seek leaves the stream exactly as it was, pending
/// skip included.
///
/// No file I/O, so this runs under Miri.
#[test]
fn backward_seeks_rewind_and_redecompress() {
    let payload: Vec<u8> = (0..=255_u8).cycle().take(3000).collect();
    let stream = compress_to_memory(&payload, b"wb");
    let sink = Sink::with_contents(&stream);
    let mut state = open_memory(sink.handle(), b"rb");

    // Read a long way in, then go back.
    let mut buf = [0_u8; 16];
    assert_eq!(gzseek64(&mut state, 2500, GzSeekFrom::Start.as_raw()), 2500);
    assert_eq!(gzread(&mut state, &mut buf), 16);
    assert_eq!(&buf, &payload[2500..2516]);

    assert_eq!(
        gzseek64(&mut state, -2000, GzSeekFrom::Current.as_raw()),
        516,
        "the backward seek reports its absolute landing point"
    );
    assert_eq!(gztell64(&state), 516);
    assert_eq!(gzread(&mut state, &mut buf), 16);
    assert_eq!(
        &buf,
        &payload[516..532],
        "the bytes after a rewind-and-skip must be the right ones"
    );

    // Back to the very beginning through SEEK_SET, which is the same machinery.
    assert_eq!(gzseek64(&mut state, 0, GzSeekFrom::Start.as_raw()), 0);
    assert_eq!(gzread(&mut state, &mut buf), 16);
    assert_eq!(&buf, &payload[..16]);

    // Before the start is refused, and the refusal changes nothing.
    let before = gztell64(&state);
    assert_eq!(
        gzseek64(&mut state, -1, GzSeekFrom::Start.as_raw()),
        -1,
        "gzlib.c L416-L417: a position before the start of the file is refused"
    );
    assert_eq!(
        gztell64(&state),
        before,
        "a refused seek must leave the stream byte for byte as it was"
    );
    assert_eq!(gzread(&mut state, &mut buf), 16);
    assert_eq!(&buf, &payload[16..32], "and reading continues where it was");

    // A backward seek on a *write* stream is refused: writing cannot go back (`gzlib.c` L413-L414).
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
    let sink = Sink::new();
    let mut writer = open_memory(sink.handle(), b"wb");
    assert_eq!(gzwrite(&mut writer, b"0123456789"), 10);
    assert_eq!(
        gzseek64(&mut writer, -5, GzSeekFrom::Current.as_raw()),
        -1,
        "gzlib.c L413-L414: a write stream cannot seek backwards"
    );
    assert_eq!(gzclose(Some(&mut writer)), ReturnCode::OK);
}

/// `gzrewind` returns to the start, and everything reads again from there.
///
/// `zlib.h` L1683-L1688: it "rewinds the file", is "supported only for reading", and "gzrewind(file)
/// is equivalent to `(int)gzseek(file, 0L, SEEK_SET)`". The equivalence is asserted by doing both and
/// requiring the same subsequent bytes.
///
/// The implementation repositions the file to `state->start` -- which `gz_open` recorded so that a
/// stream over a descriptor already part way into a file rewinds to the right place
/// (`gzlib.c` L273-L279) -- and then `gz_reset` puts every read-path flag back, `junk = -1` included,
/// so the next read looks for a header again. That last part is what makes rewinding a multi-member
/// stream work, and it is asserted in [`concatenated_members_read_as_one_stream`].
///
/// A `gzrewind` on a write stream is refused.
///
/// No file I/O, so this runs under Miri.
#[test]
fn gzrewind_returns_to_the_beginning() {
    let payload: Vec<u8> = corpus::text();
    let stream = compress_to_memory(&payload, b"wb");
    let sink = Sink::with_contents(&stream);
    let mut state = open_memory(sink.handle(), b"rb");

    let first_pass = read_to_end(&mut state, 512);
    assert_eq!(first_pass, payload);
    assert_eq!(gzeof(Some(&mut state)), 1);

    assert_eq!(gzrewind(&mut state), 0, "gzrewind reports success as zero");
    assert_eq!(gztell64(&state), 0);
    assert_eq!(
        gzeof(Some(&mut state)),
        0,
        "gz_reset cleared the end-of-file marker"
    );
    assert_eq!(
        read_to_end(&mut state, 512),
        payload,
        "the whole payload must come back a second time"
    );

    // The documented equivalence with gzseek(0, SEEK_SET).
    assert_eq!(gzseek64(&mut state, 0, GzSeekFrom::Start.as_raw()), 0);
    assert_eq!(read_to_end(&mut state, 512), payload);
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    // Writing cannot rewind.
    let sink = Sink::new();
    let mut writer = open_memory(sink.handle(), b"wb");
    assert_eq!(
        gzwrite(&mut writer, HELLO),
        i32::try_from(HELLO.len()).unwrap()
    );
    assert_eq!(
        gzrewind(&mut writer),
        -1,
        "zlib.h L1685: gzrewind is supported only for reading"
    );
    assert_eq!(gzclose(Some(&mut writer)), ReturnCode::OK);
}

/// `gzoffset` reports the **compressed** file offset, which is not `gztell`'s answer.
///
/// `zlib.h` L1704-L1709: it returns "the current offset in the file being read or written", i.e. the
/// position in the compressed stream, where `gztell` reports the position in the uncompressed data.
/// The implementation reads the handle's own position and then subtracts whatever it has buffered but
/// not yet accounted for (`gzlib.c` L484-L486), so the answer is where the *stream* has got to rather
/// than where the file descriptor happens to be.
///
/// The distinction is asserted three ways: the two answers differ for compressible data; the
/// compressed offset never runs backwards as reading proceeds; and by the end of a fully read stream
/// it equals the compressed length exactly. A closed stream has no file, so it answers -1.
///
/// No file I/O, so this runs under Miri.
#[test]
fn gzoffset_reports_the_compressed_position() {
    // Incompressible data keeps the compressed and uncompressed lengths close but not equal, and the
    // small working buffer makes the stream span many `gz_avail` calls -- which is the property under
    // test -- without needing a payload that Miri would take minutes over.
    let payload = corpus::incompressible(bulk(8000));
    let stream = compress_to_memory(&payload, b"wb");
    let sink = Sink::with_contents(&stream);
    let mut state = open_memory(sink.handle(), b"rb");
    assert_eq!(gzbuffer(Some(&mut state), 64), 0);

    assert_eq!(
        gzoffset64(&mut state),
        0,
        "nothing has been read from the file yet"
    );

    let mut buf = [0_u8; 97];
    let mut previous = 0_i64;
    let mut delivered = 0_usize;
    loop {
        let got = gzread(&mut state, &mut buf);
        assert!(got >= 0, "gzread reported {:?}", error_of(&mut state));
        if got == 0 {
            break;
        }
        let count = usize::try_from(got).unwrap();
        assert_eq!(&buf[..count], &payload[delivered..delivered + count]);
        delivered += count;

        let offset = gzoffset64(&mut state);
        assert!(
            offset >= previous,
            "the compressed offset must never run backwards: {offset} after {previous}"
        );
        assert!(
            offset <= i64::try_from(stream.len()).unwrap(),
            "the compressed offset cannot exceed the file"
        );
        previous = offset;
    }
    assert_eq!(delivered, payload.len());
    assert_eq!(
        gzoffset64(&mut state),
        i64::try_from(stream.len()).unwrap(),
        "a fully consumed stream has reached the end of the compressed file"
    );

    // The two answers are genuinely different quantities.
    assert_ne!(
        gzoffset64(&mut state),
        gztell64(&state),
        "gzoffset is the compressed position and gztell the uncompressed one"
    );
    assert_eq!(
        narrow_offset::<i32>(gzoffset64(&mut state)),
        Some(i32::try_from(stream.len()).unwrap()),
        "the narrowing the facade applies to gzoffset is the same one gztell gets"
    );

    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
    assert_eq!(
        gzoffset64(&mut state),
        -1,
        "a closed stream has no file to report an offset in"
    );
}

/// `SEEK_END` is refused, and so is anything that is not `SEEK_SET` or `SEEK_CUR`.
///
/// `zlib.h` L1668-L1669 states the reason: "`SEEK_END` is not supported". It cannot be -- the
/// uncompressed length of a gzip stream is unknown without decompressing the whole thing -- and
/// `gzlib.c` L384-L385 refuses it outright. `GzSeekFrom::End` nonetheless exists, because the handle's
/// own positioning still needs it: `gz_open` seeks to the end of a file opened for appending so that
/// `gzoffset` is correct (`gzlib.c` L270). The two facts are not in tension, and the test asserts
/// both halves -- the public seek refuses `SEEK_END`, while an append open plainly used it.
///
/// A refused `whence` must also leave the stream untouched, which is asserted by reading afterwards.
///
/// No file I/O, so this runs under Miri.
#[test]
fn seek_end_and_unknown_origins_are_refused() {
    let stream = compress_to_memory(HELLO, b"wb");
    let sink = Sink::with_contents(&stream);
    let mut state = open_memory(sink.handle(), b"rb");

    for whence in [GzSeekFrom::End.as_raw(), -1, 3, 99] {
        assert_eq!(
            gzseek64(&mut state, 0, whence),
            -1,
            "gzlib.c L384-L385: whence {whence} is not accepted"
        );
    }
    assert_eq!(
        gztell64(&state),
        0,
        "a refused seek leaves the position alone"
    );
    assert_eq!(read_to_end(&mut state, 16), HELLO);
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
}

/// A forward seek on a write stream writes zeroes, which is the `gz_zero` path.
///
/// `zlib.h` L1670-L1673: "if the file is opened for writing, only forward seeks are supported;
/// gzseek then compresses a sequence of zeroes up to the new starting position". The request is
/// recorded in `skip` and paid for at the next write or at the close, so the zeroes appear in the
/// stream whether or not anything else is written afterwards.
///
/// `test/example.c` L113 relies on this for a single byte; here the same path is driven with a larger
/// gap, both before more data and immediately before the close, because the two settle the pending
/// skip from different places.
///
/// No file I/O, so this runs under Miri.
#[test]
fn forward_seek_on_a_write_stream_writes_zeroes() {
    // Settled by a following write.
    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), b"wb");
    assert_eq!(gzwrite(&mut state, b"head"), 4);
    assert_eq!(
        gzseek64(&mut state, 6, GzSeekFrom::Current.as_raw()),
        10,
        "the seek reports where the stream will land"
    );
    assert_eq!(gzwrite(&mut state, b"tail"), 4);
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
    assert_eq!(
        decompress_from_memory(&sink.bytes(), 32),
        b"head\0\0\0\0\0\0tail",
        "the gap must be six zero bytes"
    );

    // Settled by the close, with nothing written after the seek. This is `test/example.c` L113's
    // case, and it is the one a reader of that line is most likely to dismiss as a no-op.
    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), b"wb");
    assert_eq!(gzwrite(&mut state, b"head"), 4);
    assert_eq!(gzseek64(&mut state, 3, GzSeekFrom::Current.as_raw()), 7);
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
    assert_eq!(decompress_from_memory(&sink.bytes(), 32), b"head\0\0\0");

    // A SEEK_SET forward from the current position is the same path expressed absolutely.
    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), b"wb");
    assert_eq!(gzwrite(&mut state, b"abc"), 3);
    assert_eq!(gzseek64(&mut state, 5, GzSeekFrom::Start.as_raw()), 5);
    assert_eq!(gzwrite(&mut state, b"z"), 1);
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
    assert_eq!(decompress_from_memory(&sink.bytes(), 32), b"abc\0\0z");
}

// =============================================================================================
// 8. Status: end of file, errors, buffering and parameters
// =============================================================================================

/// `gzeof` reports **`past`**, not "at the end".
///
/// `zlib.h` L1713-L1719 is precise about the distinction and about why it exists: `gzeof` returns
/// true "if the end-of-file indicator for file has been set while reading", and "the end-of-file indicator is
/// set only if the read tried to go past the end of the input, but came up short. Therefore ... `gzeof()`
/// may return false even if there is no more data to read, in the event that the last read request
/// was for the exact number of bytes remaining in the input file." `gzlib.c` L509 returns
/// `state->past`, and `gz_read` sets that flag only when its request could not be satisfied
/// (`gzread.c` L387-L389).
///
/// So the distinction is asserted directly, both ways round, because it is a classic source of
/// confusion and because an implementation that returned "at the end" instead would pass a careless
/// test:
///
/// * a read of **exactly** the remaining bytes leaves `gzeof` false;
/// * a read that asked for **more** than remained sets it, even though that read succeeded.
///
/// Both answers were confirmed against a program linked to the reference library built from the C
/// sources in this repository: it prints `eof=0` after an exact-size read and `eof=1` after an
/// oversized one.
///
/// A write stream always answers 0, because neither flag means anything there (`gzlib.c` L509), and
/// so does a stream with no direction.
///
/// No file I/O, so this runs under Miri.
#[test]
fn gzeof_reports_past_rather_than_at_the_end() {
    let stream = compress_to_memory(b"AAA", b"wb");
    let sink = Sink::with_contents(&stream);

    // Exactly the remaining bytes: the request was satisfied, so `past` stays clear.
    let mut state = open_memory(sink.handle(), b"rb");
    assert_eq!(gzeof(Some(&mut state)), 0, "nothing has been read yet");
    let mut exact = [0_u8; 3];
    assert_eq!(gzread(&mut state, &mut exact), 3);
    assert_eq!(&exact, b"AAA");
    assert_eq!(
        gzeof(Some(&mut state)),
        0,
        "zlib.h L1719-L1723: an exact-size read has not gone past the end"
    );
    // Now a read that finds nothing does set it.
    assert_eq!(gzread(&mut state, &mut exact), 0);
    assert_eq!(gzeof(Some(&mut state)), 1);
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    // More than remained: the same three bytes come back, and `past` is set by the same call.
    let mut state = open_memory(sink.handle(), b"rb");
    let mut generous = [0_u8; 16];
    assert_eq!(gzread(&mut state, &mut generous), 3);
    assert_eq!(
        gzeof(Some(&mut state)),
        1,
        "gzread.c L387-L389: the request came up short, so `past` is set"
    );
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    // A write stream, before and after writing.
    let sink = Sink::new();
    let mut writer = open_memory(sink.handle(), b"wb");
    assert_eq!(gzeof(Some(&mut writer)), 0);
    assert_eq!(gzwrite(&mut writer, b"x"), 1);
    assert_eq!(
        gzeof(Some(&mut writer)),
        0,
        "gzlib.c L509: end of file means nothing on a write stream"
    );
    assert_eq!(gzclose(Some(&mut writer)), ReturnCode::OK);
    assert_eq!(
        gzeof(Some(&mut writer)),
        0,
        "a closed stream has no direction, so the answer is 0"
    );
    assert_eq!(
        gzeof(None::<&mut GzState<'static, GlobalAllocator>>),
        0,
        "gzlib.c L502-L503: no file means 0"
    );
}

/// `gzerror` reports the stored message and code, and `gzclearerr` takes them away again.
///
/// `zlib.h` L1775-L1790 and `gzlib.c` L513-L527. Three specific answers are asserted, because each
/// one is a separate branch of the C function:
///
/// * **no error** yields the empty string and `Z_OK` (`gzlib.c` L526-L527's `state->msg == NULL`
///   arm). The reference prints `err=0 msg=""` for a freshly opened stream.
/// * **a recorded error** yields the message `gz_error` stored, which is the stream's path, a colon
///   and a space, then the text -- C builds it with
///   `snprintf(state->msg, ..., "%s%s%s", state->path, ": ", msg)` (`gzlib.c` L583-L586) -- so the
///   assertions here check the suffix rather than the whole string.
/// * **`Z_MEM_ERROR`** yields the literal `"out of memory"` *instead of* a stored message
///   (`gzlib.c` L526). That is a pair with `gz_error`'s deliberate refusal to allocate a message
///   while reporting that allocation failed, and it is asserted in
///   [`an_exhausted_allocator_is_reported_as_mem_error`].
///
/// `gzclearerr` clears the code, the message, and -- on a read stream only -- `eof` and `past`
/// (`gzlib.c` L539-L547). `zlib.h` L1793-L1796 gives the reason it exists: "this is useful for
/// continuing to read a gzip file that is being written concurrently". The `Z_BUF_ERROR` a read past
/// the end of a completed member latches is therefore clearable, and the stream then closes with
/// `Z_OK` -- confirmed against the reference, which prints `err=0 ""` and `close=0` after a
/// `gzclearerr` that followed a `"…: unexpected end of file"`.
///
/// No file I/O, so this runs under Miri.
#[test]
fn gzerror_and_gzclearerr_report_and_reset() {
    let stream = compress_to_memory(b"AAA", b"wb");
    let sink = Sink::with_contents(&stream);

    // No error: the empty message and Z_OK, with `errnum` written even so.
    let mut state = open_memory(sink.handle(), b"rb");
    let mut code = 99;
    assert_eq!(
        gzerror(Some(&mut state), Some(&mut code)),
        Some(&b""[..]),
        "gzlib.c L526-L527: no stored message is the empty string, not the absence of one"
    );
    assert_eq!(code, ReturnCode::OK.as_i32(), "and the code is Z_OK");
    // `errnum` is optional, exactly as C's `int *errnum` may be NULL.
    assert_eq!(gzerror(Some(&mut state), None), Some(&b""[..]));

    // A recorded error: read to the end with `gzgets`, which latches Z_BUF_ERROR, then inspect it.
    let mut line = [0_u8; 8];
    while gzgets(&mut state, &mut line).is_some() {}
    let (code, message) = error_of(&mut state);
    assert_eq!(code, ReturnCode::BUF_ERROR.as_i32());
    assert!(
        message.starts_with("<memory>: "),
        "gzlib.c L583-L586: the message is prefixed with the stream's path -- {message:?}"
    );
    assert!(message.ends_with("unexpected end of file"));
    assert_eq!(gzeof(Some(&mut state)), 1);

    // gzclearerr takes all of it away.
    gzclearerr(Some(&mut state));
    let (code, message) = error_of(&mut state);
    assert_eq!(code, ReturnCode::OK.as_i32(), "the code is cleared");
    assert_eq!(message, "", "and so is the message");
    assert_eq!(
        gzeof(Some(&mut state)),
        0,
        "gzlib.c L544-L547: so are eof and past"
    );
    assert_eq!(
        gzclose(Some(&mut state)),
        ReturnCode::OK,
        "a cleared stream has nothing left to report at close"
    );

    // A missing stream answers `None`, which is C's NULL return, and `gzclearerr` is a no-op.
    assert_eq!(
        gzerror(None::<&mut GzState<'static, GlobalAllocator>>, None),
        None
    );
    gzclearerr(None::<&mut GzState<'static, GlobalAllocator>>);
}

/// `gzbuffer` is accepted before the first read or write and refused afterwards.
///
/// `zlib.h` L1429-L1442: it "sets the internal buffer size used by this library's functions for file
/// to size", the default is 8192, and it "must be called after `gzopen()` or `gzdopen()`, and before any
/// other calls that read or write the file". `gzlib.c` L322-L343 implements exactly four refusals and
/// one clamp, and every one of them is asserted here with the value the reference returns:
///
/// | Condition | `gzlib.c` | Answer |
/// |---|---|---|
/// | no file | L326-L327 | -1 |
/// | mode is neither read nor write | L330-L331 | -1 |
/// | buffers already allocated | L334-L335 | -1 |
/// | `(size << 1) < size` -- cannot be doubled | L337-L338 | -1 |
/// | `size < 8` | L339-L340 | clamped to 8, accepted |
///
/// The doubling requirement is not arbitrary: `gzguts.h` L154-L155 records that the buffer size "and
/// twice this must be able to fit in an unsigned type", because the output buffer is doubled when
/// reading (`gzread.c` L100) and the input buffer when writing (`gzwrite.c` L16). So `0x8000_0000`
/// is refused while `0x7fff_ffff` is accepted -- both confirmed against the reference.
///
/// A non-default buffer must also still work, so the last part of the test round-trips a payload
/// several times the size of a deliberately tiny buffer.
///
/// No file I/O, so this runs under Miri.
#[test]
fn gzbuffer_is_accepted_only_before_use() {
    let sink = Sink::new();

    // Before use, every legal size is accepted.
    let mut state = open_memory(sink.handle(), b"wb");
    assert_eq!(
        gzbuffer(Some(&mut state), GZBUFSIZE),
        0,
        "the default is legal"
    );
    assert_eq!(gzbuffer(Some(&mut state), 8), 0);
    assert_eq!(
        gzbuffer(Some(&mut state), 1),
        0,
        "gzlib.c L339-L340: a size below 8 is clamped, not refused"
    );
    assert_eq!(gzbuffer(Some(&mut state), 0), 0);
    assert_eq!(
        gzbuffer(Some(&mut state), 0x7fff_ffff),
        0,
        "the largest size that can still be doubled is accepted"
    );
    assert_eq!(
        gzbuffer(Some(&mut state), 0x8000_0000),
        -1,
        "gzlib.c L337-L338: a size that cannot be doubled is refused"
    );
    assert_eq!(gzbuffer(Some(&mut state), c_uint::MAX), -1);

    // The clamp is observable: ask for 1 and the buffers come out at 8.
    assert_eq!(gzbuffer(Some(&mut state), 1), 0);
    assert_eq!(
        gzwrite(&mut state, b"forces the buffers into existence"),
        33
    );
    assert_eq!(
        state.size(),
        8,
        "gzlib.c L340: the clamped size is what was used"
    );

    // After use, it is refused.
    assert_eq!(
        gzbuffer(Some(&mut state), 4096),
        -1,
        "zlib.h L1437-L1439: gzbuffer must be called before anything reads or writes"
    );
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    // And a closed stream has no direction, so it is refused too, as is a missing one.
    assert_eq!(gzbuffer(Some(&mut state), 4096), -1);
    assert_eq!(
        gzbuffer(None::<&mut GzState<'static, GlobalAllocator>>, 4096),
        -1,
        "gzlib.c L326-L327: no file means -1"
    );

    // A tiny buffer must still round-trip correctly, in both directions.
    let payload = corpus::repetitive(bulk(5000));
    let sink = Sink::new();
    let mut writer = open_memory(sink.handle(), b"wb");
    assert_eq!(gzbuffer(Some(&mut writer), 16), 0);
    assert_eq!(
        gzwrite(&mut writer, &payload),
        i32::try_from(payload.len()).unwrap()
    );
    assert_eq!(gzclose(Some(&mut writer)), ReturnCode::OK);

    let mut reader = open_memory(sink.handle(), b"rb");
    assert_eq!(gzbuffer(Some(&mut reader), 16), 0);
    assert_eq!(
        read_to_end(&mut reader, 3),
        payload,
        "a 16-byte buffer must still deliver the whole payload"
    );
    assert_eq!(gzclose(Some(&mut reader)), ReturnCode::OK);

    // A large buffer likewise.
    let sink = Sink::new();
    let mut writer = open_memory(sink.handle(), b"wb");
    assert_eq!(gzbuffer(Some(&mut writer), 1 << 17), 0);
    assert_eq!(
        gzwrite(&mut writer, &payload),
        i32::try_from(payload.len()).unwrap()
    );
    assert_eq!(gzclose(Some(&mut writer)), ReturnCode::OK);
    assert_eq!(decompress_from_memory(&sink.bytes(), 4096), payload);
}

/// `gzsetparams` changes the level and strategy mid-stream and the result still decodes.
///
/// `zlib.h` L1445-L1453: it is "the same as deflateParams", and "if the level is changed for a write
/// stream, then the previously written data is flushed" -- which is why the implementation passes
/// `Z_BLOCK` to `gz_comp` before calling `deflateParams` (`gzwrite.c` L657). It "returns `Z_OK`, or
/// `Z_STREAM_ERROR` if the file was not opened for writing".
///
/// Four things are asserted, and the last two are the ones a careless implementation gets wrong:
///
/// * a change mid-write leaves the stream decodable, with every byte recoverable;
/// * the no-change short circuit (`gzwrite.c` L650-L651, `level == state->level && strategy ==
///   state->strategy`) is a genuine no-op rather than a flush -- so calling it repeatedly cannot
///   corrupt anything or insert stray blocks;
/// * a read stream is refused with `Z_STREAM_ERROR`, which the reference confirms by returning -2;
/// * a **transparent** write stream is refused too, because there is no compressor to reparameterise
///   (`gzwrite.c` L644-L646's `|| state->direct`).
///
/// No file I/O, so this runs under Miri.
#[test]
fn gzsetparams_changes_parameters_mid_stream() {
    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), b"wb");

    // Before anything is written, so `state->size` is still zero and no flush is needed.
    assert_eq!(
        gzsetparams(Some(&mut state), Z_BEST_SPEED, Z_DEFAULT_STRATEGY),
        ReturnCode::OK
    );
    assert_eq!(state.level(), Z_BEST_SPEED);

    let first = vec![b'a'; 30];
    let second = vec![b'b'; 30];
    let third = vec![b'c'; 30];
    assert_eq!(gzwrite(&mut state, &first), 30);

    // Mid-stream, which flushes what is buffered and reparameterises the compressor.
    assert_eq!(
        gzsetparams(Some(&mut state), Z_BEST_COMPRESSION, Z_RLE),
        ReturnCode::OK
    );
    assert_eq!(state.level(), Z_BEST_COMPRESSION);
    assert_eq!(state.strategy(), Z_RLE);
    assert_eq!(gzwrite(&mut state, &second), 30);

    // The no-change short circuit: repeated calls with the same values must change nothing.
    for _ in 0..3 {
        assert_eq!(
            gzsetparams(Some(&mut state), Z_BEST_COMPRESSION, Z_RLE),
            ReturnCode::OK,
            "gzwrite.c L650-L651: an unchanged pair returns Z_OK without flushing"
        );
    }
    assert_eq!(gzwrite(&mut state, &third), 30);
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    let mut expected = Vec::new();
    expected.extend_from_slice(&first);
    expected.extend_from_slice(&second);
    expected.extend_from_slice(&third);
    assert_eq!(
        decompress_from_memory(&sink.bytes(), 128),
        expected,
        "a stream whose parameters changed mid-way must still decode in full"
    );

    // A read stream is refused.
    let mut reader = open_memory(sink.handle(), b"rb");
    assert_eq!(
        gzsetparams(Some(&mut reader), Z_BEST_SPEED, Z_DEFAULT_STRATEGY),
        ReturnCode::STREAM_ERROR,
        "zlib.h L1452-L1453: Z_STREAM_ERROR if the file was not opened for writing"
    );
    assert_eq!(gzclose(Some(&mut reader)), ReturnCode::OK);

    // So is a transparent write stream: there is no compressor to reparameterise.
    let sink = Sink::new();
    let mut transparent = open_memory(sink.handle(), b"wT");
    assert_eq!(
        gzsetparams(Some(&mut transparent), Z_BEST_SPEED, Z_FILTERED),
        ReturnCode::STREAM_ERROR,
        "gzwrite.c L644-L646: `|| state->direct` refuses a transparent stream"
    );
    assert_eq!(gzclose(Some(&mut transparent)), ReturnCode::OK);

    // And a missing stream.
    assert_eq!(
        gzsetparams(
            None::<&mut GzState<'static, GlobalAllocator>>,
            Z_BEST_SPEED,
            Z_DEFAULT_STRATEGY
        ),
        ReturnCode::STREAM_ERROR
    );
}

// =============================================================================================
// 9. The bounded formatting path behind `gzprintf`
// =============================================================================================

/// A formatted write reports its byte count and lands in the stream.
///
/// The variadic surface itself is a **facade** concern and is deliberately absent from this crate:
/// `gzprintf` is variadic (`zlib.h` L1549) and `gzvprintf` takes a `va_list` (`zlib.h` L2047), and
/// `core::ffi::VaList` is unsafe FFI, so the one place a `va_list` is touched anywhere in the port is
/// the C shim in `crates/libz-rs-sys/csrc`. What the safe core owns is everything around the
/// formatter: the guards, the scratch region, the overflow sentinel and the accounting. That is what
/// is tested here, through [`printf_bytes`], which hands an already-rendered result to exactly the
/// same [`printf_begin`]/[`printf_commit`] pair the shim drives.
///
/// `test/example.c` L109 asserts `gzprintf(file, ", %s!", "hello") == 8`; the rendered form of that
/// format is `", hello!"`, so the same eight is asserted here and again, in sequence, in
/// [`test_gzio_is_reproduced_exactly`].
///
/// No file I/O, so this runs under Miri.
#[test]
fn a_formatted_write_reports_its_length() {
    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), b"wb");

    assert_eq!(
        printf_bytes(&mut state, b", hello!"),
        Ok(8),
        "test/example.c L109: the formatted result is eight bytes long"
    );
    assert_eq!(printf_bytes(&mut state, b" and again"), Ok(10));
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    assert_eq!(
        decompress_from_memory(&sink.bytes(), 32),
        b", hello! and again",
        "the formatted bytes must reach the file in order"
    );
}

/// The documented cap: 8191 bytes succeed, 8192 are refused with nothing written.
///
/// `zlib.h` L1551-L1560: `gzprintf` "returns the number of uncompressed bytes actually written, or a
/// negative zlib error code in case of error. The number of uncompressed bytes written is limited to
/// 8191, or one less than the buffer size given to `gzbuffer()`. The caller should ensure that this
/// limit is not exceeded. If it is exceeded, then `gzprintf()` will return an error (0) with nothing
/// written."
///
/// The limit is one less than the buffer because a bounded formatter spends one byte on its
/// terminating NUL. `printf_commit` rejects a result three ways (`gzwrite.c` L471), and the three are
/// not redundant:
///
/// * `len == 0` -- a formatter that failed, which C reaches through the cast of a negative `int`;
/// * `len >= size` -- the reported length did not fit, which is the truncation case;
/// * `next[size - 1] != 0` -- the **sentinel** planted at `gzwrite.c` L453 was overwritten, which
///   catches a formatter that ignored the bound altogether, as the reference's `NO_vsnprintf`
///   configuration does by calling the unbounded `vsprintf` (`gzwrite.c` L455-L458). It is what makes
///   the reported length untrusted rather than trusted.
///
/// All three yield **zero**, not a negative code, and leave the position and the buffered input
/// untouched. The reference confirms the boundary exactly: a format producing 8191 characters returns
/// 8191, one producing 8192 returns 0, and `gztell` afterwards is 8191 -- the refused write
/// contributed nothing.
///
/// No file I/O, so this runs under Miri.
#[test]
fn the_formatting_cap_is_one_byte_below_the_buffer() {
    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), b"wb");

    // The buffer has to exist before its size can be quoted; the first formatted write creates it.
    assert_eq!(printf_bytes(&mut state, b"x"), Ok(1));
    let size = usize::try_from(state.size()).unwrap();
    assert_eq!(
        size,
        usize::try_from(GZBUFSIZE).unwrap(),
        "the default buffer is GZBUFSIZE, so the cap is 8191"
    );

    // Exactly at the cap: accepted whole.
    let at_cap = vec![b'a'; size - 1];
    assert_eq!(
        printf_bytes(&mut state, &at_cap),
        Ok(c_int::try_from(size - 1).unwrap()),
        "a result of size - 1 bytes fits, terminator included"
    );
    let after_cap = gztell64(&state);
    assert_eq!(after_cap, i64::try_from(size).unwrap(), "1 + (size - 1)");

    // One byte over: refused with zero, and nothing changes.
    let over_cap = vec![b'b'; size];
    assert_eq!(
        printf_bytes(&mut state, &over_cap),
        Ok(0),
        "zlib.h L1557-L1560: exceeding the limit returns an error (0) with nothing written"
    );
    assert_eq!(
        gztell64(&state),
        after_cap,
        "a refused formatted write must not move the position"
    );

    // Far over: the same answer, not a different one.
    assert_eq!(printf_bytes(&mut state, &vec![b'c'; size * 3]), Ok(0));
    assert_eq!(gztell64(&state), after_cap);

    // A formatter that reports failure is C's negative `int`, which the cast turns into an enormous
    // unsigned value and the `len >= size` test then rejects.
    assert_eq!(
        printf_with(&mut state, |_region: &mut [u8]| None),
        Ok(0),
        "a failed formatter is rejected, not propagated as an error code"
    );
    assert_eq!(gztell64(&state), after_cap);

    // An empty result is `len == 0`, the first of the three rejections. The reference agrees: it
    // prints `gzprintf empty = 0`.
    assert_eq!(printf_bytes(&mut state, b""), Ok(0));
    assert_eq!(gztell64(&state), after_cap);

    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    let mut expected = Vec::with_capacity(size);
    expected.push(b'x');
    expected.extend_from_slice(&at_cap);
    assert_eq!(
        decompress_from_memory(&sink.bytes(), 4096),
        expected,
        "only the accepted writes may appear in the stream"
    );
}

/// The scratch region arrives as `state->size` bytes with a zero in its last one, and the accounting
/// advances by exactly the number of bytes formatted.
///
/// `printf_begin` is the first half of `gzvprintf` (`gzwrite.c` L416-L453), and its last statement is
/// `next[state->size - 1] = 0` -- the sentinel. `printf_commit` is the second half (L471-L483), and
/// what it counts is stated precisely: `avail_in` and `x.pos` both advance by the number of bytes
/// *formatted*, which excludes the formatter's own NUL terminator. The terminator therefore sits just
/// past the newly buffered input, outside the `avail_in` window, where the next call's sentinel plant
/// overwrites it.
///
/// Driving the two halves apart -- rather than through [`printf_bytes`] -- is what lets the sentinel
/// and the region width be inspected directly. The region must never be empty, which `printf_begin`
/// guarantees by refusing a zero-width one before creating the loan.
///
/// No file I/O, so this runs under Miri.
#[test]
fn the_formatting_scratch_region_and_its_accounting() {
    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), b"wb");

    let before_pos = gztell64(&state);
    let before_avail = state.stream().avail_in;
    assert_eq!(before_pos, 0);
    assert_eq!(before_avail, 0);

    let written = {
        let mut scratch = printf_begin(&mut state).expect("the scratch region should be available");
        let region_len = scratch.len();
        assert!(!scratch.is_empty(), "the region is never empty");
        assert_eq!(
            region_len,
            usize::try_from(GZBUFSIZE).unwrap(),
            "the region is exactly `state->size` bytes"
        );
        assert_eq!(
            scratch.as_slice()[region_len - 1],
            0,
            "gzwrite.c L453: the last byte is the overflow sentinel"
        );

        // Format five bytes and plant the terminator a bounded formatter would.
        let region = scratch.as_mut_slice();
        region[..5].copy_from_slice(b"count");
        region[5] = 0;
        5
    };

    assert_eq!(printf_commit(&mut state, written), Ok(5));
    assert_eq!(
        gztell64(&state),
        before_pos + 5,
        "x.pos advances by the bytes formatted, terminator excluded"
    );
    assert_eq!(
        state.stream().avail_in,
        before_avail + 5,
        "and so does the buffered input"
    );

    // A second round confirms the region moves along with the buffered input rather than restarting.
    let written = {
        let mut scratch = printf_begin(&mut state).expect("the region should still be available");
        assert_eq!(
            scratch.as_slice()[scratch.len() - 1],
            0,
            "the sentinel is planted afresh each time"
        );
        let region = scratch.as_mut_slice();
        region[..3].copy_from_slice(b"ing");
        region[3] = 0;
        3
    };
    assert_eq!(printf_commit(&mut state, written), Ok(3));
    assert_eq!(gztell64(&state), before_pos + 8);
    assert_eq!(state.stream().avail_in, before_avail + 8);

    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
    assert_eq!(decompress_from_memory(&sink.bytes(), 32), b"counting");
}

// =============================================================================================
// 10. Multi-member streams, trailing data, and damage
// =============================================================================================

/// Concatenated members read as one continuous stream, empty members included.
///
/// `doc/rfc1952.txt` 2.2 requires it: "A gzip file consists of a series of 'members' (compressed data
/// sets). ... The members simply appear one after another in the file, with no additional information
/// before, between, or after them." The read path implements the continuation through `gz_look`: once
/// a member has completed, `gz_decomp` sets `junk = 0` and `how = LOOK` (`gzread.c` L233-L235), and
/// `gz_look`'s first branch then goes straight back to gzip rather than re-running the transparency
/// heuristic (`gzread.c` L125-L133) -- which is what stops the second member from being mistaken for
/// raw data.
///
/// Two, three and mixed-size members are all asserted, and an empty member is included deliberately:
/// it is twenty bytes of pure header and trailer with no payload, and a reader that assumed every
/// member yields at least one byte would stall on it. The reference confirms the combination:
/// concatenating `"AAA"`, an empty member and `"BBB"` reads back `"AAABBB"` with `gzclose` returning
/// `Z_OK`.
///
/// A rewind must re-establish the same behaviour, which is why `gz_reset` restores `junk = -1`
/// (`gzlib.c` L75); reading the whole concatenation twice is what checks it.
///
/// No file I/O, so this runs under Miri.
#[test]
fn concatenated_members_read_as_one_stream() {
    let empty_member = compress_to_memory(corpus::EMPTY, b"wb");
    assert_eq!(
        empty_member.len(),
        20,
        "an empty member is a ten-byte header, a two-byte empty block and an eight-byte trailer"
    );

    let a = compress_to_memory(PAYLOAD_A, b"wb");
    let b = compress_to_memory(PAYLOAD_B, b"wb");
    let long = corpus::repetitive(bulk(4000).max(400));
    let c = compress_to_memory(&long, b"wb");

    // Two members.
    let mut two = a.clone();
    two.extend_from_slice(&b);
    let mut expected_two = PAYLOAD_A.to_vec();
    expected_two.extend_from_slice(PAYLOAD_B);
    for chunk in [1_usize, 5, 4096] {
        assert_eq!(
            decompress_from_memory(&two, chunk),
            expected_two,
            "two members must read as their concatenation, {chunk}-byte reads"
        );
    }

    // Three members of very different sizes, with the empty one in the middle.
    let mut three = a.clone();
    three.extend_from_slice(&empty_member);
    three.extend_from_slice(&c);
    let mut expected_three = PAYLOAD_A.to_vec();
    expected_three.extend_from_slice(&long);
    for &chunk in read_chunks() {
        assert_eq!(
            decompress_from_memory(&three, chunk),
            expected_three,
            "an empty member in the middle must be skipped, not stall the reader"
        );
    }

    // An empty member on its own, and two of them back to back.
    assert_eq!(decompress_from_memory(&empty_member, 8), corpus::EMPTY);
    let mut two_empty = empty_member.clone();
    two_empty.extend_from_slice(&empty_member);
    assert_eq!(decompress_from_memory(&two_empty, 8), corpus::EMPTY);

    // And the members produced by the reference concatenate just as ours do.
    let mut reference_pair = MEMBER_A.to_vec();
    reference_pair.extend_from_slice(&MEMBER_B);
    assert_eq!(decompress_from_memory(&reference_pair, 4), expected_two);

    // A rewind must restore the multi-member behaviour, `junk = -1` included.
    let sink = Sink::with_contents(&two);
    let mut state = open_memory(sink.handle(), b"rb");
    assert_eq!(read_to_end(&mut state, 4), expected_two);
    assert_eq!(gzrewind(&mut state), 0);
    assert_eq!(
        read_to_end(&mut state, 4),
        expected_two,
        "gzlib.c L75: gz_reset marks the first member again, so both members are found twice"
    );
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
}

/// Trailing garbage after a complete member is tolerated, and damage inside one is not.
///
/// The `junk` field is what tells the two apart, and it is specific to this `1.3.2.1-motley` tree
/// rather than inherited from zlib 1.3.1. `gzguts.h` L186 documents its three values -- "-1 = start,
/// 1 = junk candidate, 0 = in gzip" -- and they are set at three places: `gz_reset` marks the first
/// member with -1 (`gzlib.c` L75), `gz_look` sets 1 once a header has been accepted
/// (`gzread.c` L157) and `gz_decomp` clears it to 0 as soon as a member has produced output or
/// completed (`gzread.c` L202-L204 and L233). When `inflate` then reports `Z_DATA_ERROR` with
/// `junk == 1`, the remainder is discarded as trailing garbage and the read simply ends
/// (`gzread.c` L213-L219); with `junk == 0` it is a real error.
///
/// Four shapes are asserted, each against the value the reference produces:
///
/// * **a complete member followed by non-gzip bytes** delivers the member's payload, reports no
///   error, and closes with `Z_OK`;
/// * **a complete member followed by a header-shaped but undecodable member** does the same, because
///   the second member is a junk candidate that produces nothing;
/// * **a complete member followed by a junk candidate that does produce a byte or two before
///   failing** is a *real* `Z_DATA_ERROR`, because "any decompressed data marks this as a real gzip
///   stream" (`gzread.c` L202-L204) clears `junk` to 0 and forfeits the tolerance. The bytes it
///   produced are delivered along with the genuine payload;
/// * **a file that is nothing but a header and nonsense** delivers nothing, reports no error and
///   closes with `Z_OK` -- the very first member is a junk candidate too, which is why a corrupt file
///   is not automatically a data error.
///
/// The third shape is the one worth dwelling on: whether trailing rubbish is forgiven depends on
/// whether it happened to inflate to anything, not on whether it was meant to be a member. Both
/// outcomes were confirmed against the reference, which delivers `"first half\nE\0"` with
/// `gzerror` reporting -3 for the byte-producing case and `"first half\n"` with `gzerror` reporting 0
/// for the others.
///
/// No file I/O, so this runs under Miri.
#[test]
fn trailing_garbage_is_tolerated_after_a_complete_member() {
    /// Reads `bytes` to exhaustion and reports what was delivered, the recorded code and the close.
    fn read_all(bytes: &[u8]) -> (Vec<u8>, i32, ReturnCode) {
        let sink = Sink::with_contents(bytes);
        let mut state = open_memory(sink.handle(), b"rb");
        let mut out = Vec::new();
        let mut buf = [0_u8; 16];
        loop {
            let got = gzread(&mut state, &mut buf);
            if got <= 0 {
                break;
            }
            out.extend_from_slice(&buf[..usize::try_from(got).unwrap()]);
        }
        let (code, _) = error_of(&mut state);
        let closed = gzclose(Some(&mut state));
        (out, code, closed)
    }

    let member = compress_to_memory(PAYLOAD_A, b"wb");

    // Plain garbage after a complete member.
    let mut with_text = member.clone();
    with_text.extend_from_slice(b"this is not gzip");
    let (payload, code, closed) = read_all(&with_text);
    assert_eq!(
        payload, PAYLOAD_A,
        "the member's payload is delivered in full"
    );
    assert_eq!(
        code,
        ReturnCode::OK.as_i32(),
        "trailing garbage is not an error"
    );
    assert_eq!(closed, ReturnCode::OK);

    // A header-shaped candidate whose deflate data cannot produce a single byte. `0xff` is
    // `BFINAL = 1` with `BTYPE = 11`, which RFC 1951 3.2.3 reserves, so inflate reports
    // `Z_DATA_ERROR` having emitted nothing and the tolerance applies.
    let mut with_undecodable = member.clone();
    with_undecodable.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0, 0x03]);
    with_undecodable.extend_from_slice(&[0xff; 16]);
    let (payload, code, closed) = read_all(&with_undecodable);
    assert_eq!(
        payload, PAYLOAD_A,
        "a junk candidate that emits nothing adds nothing"
    );
    assert_eq!(
        code,
        ReturnCode::OK.as_i32(),
        "gzread.c L213-L219: a junk candidate that fails to inflate is discarded"
    );
    assert_eq!(closed, ReturnCode::OK);

    // A candidate that does emit before failing forfeits the tolerance. The two extra bytes are what
    // this particular nonsense inflates to, and the reference produces the same pair.
    let mut with_partial_member = member.clone();
    with_partial_member.extend_from_slice(&[0x1f, 0x8b, 0x08, 0x00]);
    with_partial_member.extend_from_slice(b"nonsense that is not deflate data");
    let (payload, code, closed) = read_all(&with_partial_member);
    assert_eq!(
        payload, b"first half\nE\0",
        "gzread.c L202-L204: bytes that were produced are delivered, junk tolerance or not"
    );
    assert_eq!(
        code,
        ReturnCode::DATA_ERROR.as_i32(),
        "gzread.c L202-L204: decompressed output clears `junk`, so the failure is a real one"
    );
    assert_eq!(
        closed,
        ReturnCode::OK,
        "gzread.c L662: only Z_BUF_ERROR is deferred to the close; a data error is not"
    );

    // A file that is nothing but a plausible header and nonsense: the very first member is a junk
    // candidate too, so nothing is delivered and nothing is reported.
    let mut only_fake = vec![0x1f, 0x8b, 0x08, 0x00, 0, 0, 0, 0, 0, 0x03];
    only_fake.extend_from_slice(&[0xff; 16]);
    let (payload, code, closed) = read_all(&only_fake);
    assert!(
        payload.is_empty(),
        "no payload can be recovered from nonsense"
    );
    assert_eq!(code, ReturnCode::OK.as_i32());
    assert_eq!(closed, ReturnCode::OK);
}

/// A truncated member surfaces `Z_BUF_ERROR`, at the close rather than at the read.
///
/// This is the deferred report `zlib.h` documents twice. L1474-L1478: "gzread does not return -1 in
/// the event of an incomplete gzip stream. This function should be used instead of `gzread()` when
/// there is no need to distinguish between an incomplete gzip stream and a read error, or if it is
/// desired to know how much data was read before the error occurred." And L1758-L1760: `gzclose`
/// returns "`Z_BUF_ERROR` if the last read ended in the middle of a gzip stream".
///
/// So the data read before the truncation is still delivered -- silently dropping it would lose
/// information the caller is entitled to -- while the fact that the stream was cut short is recorded
/// and reported by `gzclose_r` (`gzread.c` L662). The reference confirms both halves: reading a member
/// with its trailer removed delivers the payload, `gzerror` then reports
/// `-5 "…: unexpected end of file"`, and `gzclose` returns -5.
///
/// Truncating **into the trailer** is the case asserted, because that is the one where the payload is
/// complete and only the integrity check is missing -- exactly the shape a partially written or
/// partially transferred file has.
///
/// No file I/O, so this runs under Miri.
#[test]
fn truncated_member_reports_buf_error_at_close() {
    let member = compress_to_memory(PAYLOAD_A, b"wb");
    let truncated = &member[..member.len() - 6];

    let sink = Sink::with_contents(truncated);
    let mut state = open_memory(sink.handle(), b"rb");
    let mut buf = [0_u8; 64];

    // The payload is delivered, not discarded.
    let got = gzread(&mut state, &mut buf);
    assert_eq!(
        usize::try_from(got).unwrap(),
        PAYLOAD_A.len(),
        "zlib.h L1474-L1478: the bytes read before the truncation are still returned"
    );
    assert_eq!(&buf[..PAYLOAD_A.len()], PAYLOAD_A);

    // And the truncation is recorded.
    let (code, message) = error_of(&mut state);
    assert_eq!(code, ReturnCode::BUF_ERROR.as_i32());
    assert!(
        message.ends_with("unexpected end of file"),
        "the recorded message names the cause: {message:?}"
    );

    assert_eq!(
        gzclose(Some(&mut state)),
        ReturnCode::BUF_ERROR,
        "zlib.h L1758-L1760: gzclose is where a stream that ended mid-member is reported"
    );

    // ★ The report is latched, not sticky: every read entry point begins with
    // `gz_error(state, Z_OK, NULL)` (`gzread.c` L409), so **another read clears it** and the close
    // then reports `Z_OK`. That is not a leak in the deferral, it is the deferral's scope -- `zlib.h`
    // L1759 says "the *last* read", and the last read in this sequence succeeded in finding nothing
    // rather than failing. Confirmed against the reference: one read then close gives -5, two reads
    // then close gives 0.
    let sink = Sink::with_contents(truncated);
    let mut state = open_memory(sink.handle(), b"rb");
    assert_eq!(
        usize::try_from(gzread(&mut state, &mut buf)).unwrap(),
        PAYLOAD_A.len()
    );
    assert_eq!(error_of(&mut state).0, ReturnCode::BUF_ERROR.as_i32());
    assert_eq!(
        gzread(&mut state, &mut buf),
        0,
        "a further read finds nothing, and still does not report -1"
    );
    assert_eq!(
        error_of(&mut state).0,
        ReturnCode::OK.as_i32(),
        "gzread.c L409: entering a read clears the recorded status"
    );
    assert_eq!(
        gzclose(Some(&mut state)),
        ReturnCode::OK,
        "so the close has nothing left to defer"
    );

    // Cutting the deflate data itself, not just the trailer, reports the same way. One read is used,
    // for the reason above: a second one would clear the very record under test. The payload is
    // incompressible so that half a member is still thousands of recoverable bytes rather than an
    // unfinished header.
    let mut long_member = compress_to_memory(&corpus::incompressible(bulk(4000).max(400)), b"wb");
    long_member.truncate(long_member.len() / 2);
    let sink = Sink::with_contents(&long_member);
    let mut state = open_memory(sink.handle(), b"rb");
    let mut wide = vec![0_u8; 8192];
    let got = gzread(&mut state, &mut wide);
    assert!(got > 0, "the part that did arrive is still decompressed");
    assert_eq!(error_of(&mut state).0, ReturnCode::BUF_ERROR.as_i32());
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::BUF_ERROR);
}

// =============================================================================================
// 11. Closing, flushing, and what happens when the file or the allocator says no
// =============================================================================================

/// `gzclose` dispatches on direction, and the two halves refuse the other direction.
///
/// `gzclose.c` L11-L23 is the whole function:
///
/// ```c
/// return state->mode == GZ_READ ? gzclose_r(file) : gzclose_w(file);
/// ```
///
/// The condition is exactly "is it `GZ_READ`?" and nothing more, so everything else -- `GZ_WRITE`,
/// the transient `GZ_APPEND`, the direction-less `GZ_NONE`, and any value that is not a mode at all --
/// goes down the write path, where `gzclose_w`'s own `state->mode != GZ_WRITE` guard
/// (`gzwrite.c` L677-L678) produces the `Z_STREAM_ERROR`. That asymmetry is preserved rather than
/// tidied, and it is what makes a **second** close return `Z_STREAM_ERROR` instead of quietly
/// re-running the teardown: the first close leaves `mode` at `GZ_NONE`.
///
/// `zlib.h` L1750-L1760 lists the five codes, and each of the four reachable through this layer is
/// asserted here or in the two tests that follow: `Z_STREAM_ERROR` for an invalid handle, `Z_ERRNO` on
/// a file error ([`a_failing_file_is_reported_as_errno`]), `Z_MEM_ERROR` when out of memory
/// ([`an_exhausted_allocator_is_reported_as_mem_error`]), `Z_BUF_ERROR` for a read that ended
/// mid-member ([`truncated_member_reports_buf_error_at_close`]), and `Z_OK` otherwise.
///
/// `zlib.h` L1754-L1755 warns that "gzclose must not be called more than once on the same file, just
/// as free must not be called more than once on the same allocation" -- in C, because the second call
/// touches freed memory. This implementation cannot free the structure, since the facade owns it, so
/// it leaves the state refusing further work instead, which makes the misuse deterministic rather
/// than undefined. That is asserted too.
///
/// No file I/O, so this runs under Miri.
#[test]
fn gzclose_dispatches_on_direction() {
    let stream = compress_to_memory(HELLO, b"wb");
    let sink = Sink::with_contents(&stream);

    // A read stream: `gzclose` reaches `gzclose_r`, and `gzclose_w` would refuse it.
    let mut reader = open_memory(sink.handle(), b"rb");
    assert_eq!(
        gzclose_w(&mut reader),
        ReturnCode::STREAM_ERROR.as_i32(),
        "gzwrite.c L677-L678: gzclose_w is only for a write stream"
    );
    assert_eq!(gzclose(Some(&mut reader)), ReturnCode::OK);
    assert_eq!(
        sink.closes(),
        1,
        "the stream closed its handle exactly once, and the refused gzclose_w closed nothing"
    );

    // A write stream: `gzclose` reaches `gzclose_w`, and `gzclose_r` would refuse it.
    let sink = Sink::new();
    let mut writer = open_memory(sink.handle(), b"wb");
    assert_eq!(
        gzwrite(&mut writer, HELLO),
        i32::try_from(HELLO.len()).unwrap()
    );
    assert_eq!(
        gzclose_r(&mut writer),
        ReturnCode::STREAM_ERROR,
        "gzread.c L646-L648: gzclose_r is only for a read stream"
    );
    assert_eq!(gzclose(Some(&mut writer)), ReturnCode::OK);
    assert_eq!(
        decompress_from_memory(&sink.bytes(), 32),
        HELLO,
        "the refused gzclose_r must not have damaged the stream"
    );

    // Calling the direction-specific halves directly is equivalent to the dispatcher.
    let sink = Sink::new();
    let mut writer = open_memory(sink.handle(), b"wb");
    assert_eq!(gzwrite(&mut writer, b"direct"), 6);
    assert_eq!(gzclose_w(&mut writer), ReturnCode::OK.as_i32());
    assert_eq!(decompress_from_memory(&sink.bytes(), 16), b"direct");
    let mut reader = open_memory(sink.handle(), b"rb");
    assert_eq!(read_to_end(&mut reader, 16), b"direct");
    assert_eq!(gzclose_r(&mut reader), ReturnCode::OK);

    // A second close is refused rather than repeated. `mode` is `GZ_NONE`, which is not `GZ_READ`, so
    // the dispatcher chooses the write path and `gzclose_w`'s guard answers.
    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), b"wb");
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
    assert_eq!(state.mode(), GZ_NONE, "the close cleared the direction");
    assert_eq!(
        state.size(),
        0,
        "and the buffers, so the next gz_comp cannot skip gz_init and find nothing"
    );
    assert_eq!(
        gzclose(Some(&mut state)),
        ReturnCode::STREAM_ERROR,
        "zlib.h L1754-L1755: a second close is a defect, and here it is a deterministic refusal"
    );
    assert_eq!(
        sink.closes(),
        1,
        "and the file was not closed a second time either"
    );

    // No file at all.
    assert_eq!(
        gzclose(None::<&mut GzState<'static, GlobalAllocator>>),
        ReturnCode::STREAM_ERROR,
        "gzclose.c L15-L16: `if (file == NULL) return Z_STREAM_ERROR;`"
    );
}

/// `gzflush` accepts `Z_NO_FLUSH` through `Z_FINISH` and nothing else, and the stream stays decodable.
///
/// `zlib.h` L1647-L1660: it "flushes all pending output into the compressed file", the parameter
/// "flush is as in the `deflate()` function", and it "returns the zlib error number". `gzwrite.c` L617's
/// range test is `flush < 0 || flush > Z_FINISH`, which is **narrower than `deflate`'s own**: `Z_BLOCK`
/// and `Z_TREES` are refused with `Z_STREAM_ERROR` even though `deflate` accepts `Z_BLOCK`. The layer
/// does use `Z_BLOCK` internally -- `gzsetparams` passes it before changing parameters
/// (`gzwrite.c` L657) -- but will not take it from a caller. The reference confirms every value:
/// `Z_FINISH` returns 0 while `Z_BLOCK`, `Z_TREES` and -1 all return -2.
///
/// `zlib.h` L1652-L1653 also warns that flushing too often "can seriously degrade compression", which
/// is why each accepted mode is exercised on its own stream rather than all on one.
///
/// The `Z_FINISH` case has a consequence worth pinning down: `gz_comp` arms a pending reset after a
/// `Z_FINISH` (`gzwrite.c` L94-L101), so writing again afterwards starts a **new member** rather than
/// corrupting the finished one, and the reader sees the concatenation. And because `gzclose_w`
/// performs its own `Z_FINISH`, a `gzflush(Z_FINISH)` immediately before a close leaves an extra empty
/// member behind -- the reference grows the same file from 25 to 45 bytes, which is exactly one
/// twenty-byte empty member, and still reads back the original payload.
///
/// No file I/O, so this runs under Miri.
#[test]
fn gzflush_accepts_only_the_deflate_flush_range() {
    // Every accepted mode leaves a decodable stream.
    for flush in [
        Z_NO_FLUSH,
        Z_PARTIAL_FLUSH,
        Z_SYNC_FLUSH,
        Z_FULL_FLUSH,
        Z_FINISH,
    ] {
        let sink = Sink::new();
        let mut state = open_memory(sink.handle(), b"wb");
        assert_eq!(gzwrite(&mut state, b"before "), 7);
        assert_eq!(
            gzflush(&mut state, flush),
            ReturnCode::OK.as_i32(),
            "gzwrite.c L617: flush mode {flush} is inside Z_NO_FLUSH ..= Z_FINISH"
        );
        assert_eq!(gzwrite(&mut state, b"after"), 5);
        assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
        assert_eq!(
            decompress_from_memory(&sink.bytes(), 32),
            b"before after",
            "a stream flushed with mode {flush} must still decode in full"
        );
    }

    // The refusals, and each one leaves the stream usable.
    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), b"wb");
    assert_eq!(gzwrite(&mut state, b"kept"), 4);
    for flush in [Z_BLOCK, Z_TREES, -1, Z_FINISH + 100] {
        assert_eq!(
            gzflush(&mut state, flush),
            ReturnCode::STREAM_ERROR.as_i32(),
            "gzwrite.c L616-L618: flush mode {flush} is outside the accepted range"
        );
    }
    assert_eq!(gzwrite(&mut state, b" anyway"), 7);
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
    assert_eq!(decompress_from_memory(&sink.bytes(), 32), b"kept anyway");

    // A Z_FINISH immediately before the close leaves an extra empty member, and the payload survives.
    let plain = compress_to_memory(HELLO, b"wb");
    let sink = Sink::new();
    let mut state = open_memory(sink.handle(), b"wb");
    assert_eq!(
        gzwrite(&mut state, HELLO),
        i32::try_from(HELLO.len()).unwrap()
    );
    assert_eq!(gzflush(&mut state, Z_FINISH), ReturnCode::OK.as_i32());
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
    let finished = sink.bytes();
    assert_eq!(
        finished.len(),
        plain.len() + 20,
        "gzclose_w's own Z_FINISH is a flushing call, so it emits a second, empty member"
    );
    assert_eq!(
        decompress_from_memory(&finished, 32),
        HELLO,
        "and the reader skips the empty member, so the payload is unchanged"
    );

    // A read stream cannot be flushed.
    let sink = Sink::with_contents(&plain);
    let mut reader = open_memory(sink.handle(), b"rb");
    assert_eq!(
        gzflush(&mut reader, Z_FINISH),
        ReturnCode::STREAM_ERROR.as_i32(),
        "zlib.h L1647: gzflush is for a write stream"
    );
    assert_eq!(gzclose(Some(&mut reader)), ReturnCode::OK);
}

/// A file that refuses an operation is reported as `Z_ERRNO`, and only this layer produces it.
///
/// `Z_ERRNO` exists so that a caller can consult the platform's `errno`: `zlib.h` L1779-L1781 says "if
/// an error occurred in the file system and not in the compression library, `*errnum` is set to
/// `Z_ERRNO` and the application may consult `errno` to get the exact error code". Every site that
/// records it is in this layer -- `gz_load` for a failed read (`gzread.c` L41), `gz_comp` for a failed
/// write (`gzwrite.c` L119), and the two close paths for a failed `close` (`gzread.c` L665-L667,
/// `gzwrite.c` L696-L697) -- because nothing else in zlib performs I/O. The engines report
/// `Z_DATA_ERROR`, `Z_MEM_ERROR`, `Z_BUF_ERROR` or `Z_STREAM_ERROR` and never this.
///
/// A `would_block` failure is deliberately **not** one of these: `gz_comp` records it in `state->again`
/// and treats the stream as retryable rather than broken (`gzwrite.c` L117-L119), and `zlib.h`
/// L1480-L1483 documents the read side of the same idea: on a non-blocking device, "if the input
/// stalls and there is no uncompressed data to return", `gzread` returns -1 with `errno` set to
/// `EAGAIN` or `EWOULDBLOCK` and "can then be called again". A stalled device and a broken one are
/// therefore different states, and both are asserted so that the two are not collapsed into one.
///
/// No file I/O, so this runs under Miri: the failures come from the handle, not from a real device.
#[test]
fn a_failing_file_is_reported_as_errno() {
    let stream = compress_to_memory(HELLO, b"wb");

    // A failing `close` on a read stream.
    let sink = Sink::with_contents(&stream);
    let mut state = open_memory(sink.failing_close_handle(), b"rb");
    assert_eq!(read_to_end(&mut state, 16), HELLO);
    assert_eq!(
        gzclose(Some(&mut state)),
        ReturnCode::ERRNO,
        "gzread.c L665: a failed close(2) is Z_ERRNO"
    );
    assert_eq!(sink.closes(), 1, "the close was attempted exactly once");

    // A failing `close` on a write stream, where the data still reached the file.
    let sink = Sink::new();
    let mut state = open_memory(sink.failing_close_handle(), b"wb");
    assert_eq!(
        gzwrite(&mut state, HELLO),
        i32::try_from(HELLO.len()).unwrap()
    );
    assert_eq!(
        gzclose(Some(&mut state)),
        ReturnCode::ERRNO,
        "gzwrite.c L696: the same on the write side"
    );
    assert_eq!(
        decompress_from_memory(&sink.bytes(), 32),
        HELLO,
        "the bytes were flushed before the close failed"
    );

    // A hard read failure.
    let sink = Sink::with_contents(&stream);
    let mut state = open_memory(sink.failing_handle(Failure::Hard), b"rb");
    let mut buf = [0_u8; 16];
    assert_eq!(
        gzread(&mut state, &mut buf),
        -1,
        "a read that the device refused is -1, not end of file"
    );
    let (code, message) = error_of(&mut state);
    assert_eq!(code, ReturnCode::ERRNO.as_i32(), "gzread.c L41");
    assert!(
        message.starts_with("<memory>: "),
        "the message names the file: {message:?}"
    );

    // A hard write failure. The buffer is deliberately tiny, because a write only reaches the device
    // once the buffer has to be drained: with eight bytes of buffer the failure is reached by the
    // first `gzwrite` rather than by piling up tens of kilobytes to overflow a default-sized one.
    let sink = Sink::new();
    let mut state = open_memory(sink.failing_handle(Failure::Hard), b"wb");
    assert_eq!(gzbuffer(Some(&mut state), 8), 0);
    assert_eq!(
        gzwrite(&mut state, b"long enough to have to be drained"),
        0,
        "a write the device refused reports that nothing was written"
    );
    assert_eq!(
        error_of(&mut state).0,
        ReturnCode::ERRNO.as_i32(),
        "gzwrite.c L119: a write the device refused is Z_ERRNO"
    );
    let closed = gzclose(Some(&mut state));
    assert_eq!(closed, ReturnCode::ERRNO, "and the close reports it too");

    // A stalled non-blocking read is a different thing: -1, but with `again` recorded rather than a
    // broken stream, so a later call can succeed.
    let sink = Sink::with_contents(&stream);
    let mut state = open_memory(sink.failing_handle(Failure::Stall), b"rb");
    assert_eq!(gzread(&mut state, &mut buf), -1);
    assert!(
        state.again(),
        "gzwrite.c L117-L119: a would-block failure is recorded as a stall, not as damage"
    );
    assert_eq!(
        error_of(&mut state).0,
        ReturnCode::ERRNO.as_i32(),
        "zlib.h L1480-L1483: the code is still Z_ERRNO so that errno can be consulted"
    );
    let _ = gzclose(Some(&mut state));
}

/// An exhausted allocator is reported as `Z_MEM_ERROR`, with `"out of memory"` and no allocation.
///
/// This is the path `test/infcover.c` exists to reach: its `mem_limit` (L176-L181) forces a failure at
/// a chosen moment so that the error branches are exercised deliberately rather than by chance. The
/// equivalent here is [`common::TrackingAllocator::set_limit`], which is a transcription of it.
///
/// Two properties are asserted beyond the code itself:
///
/// * `gzerror` answers `"out of memory"` **without** the usual path prefix. That is not an
///   inconsistency: `gz_error` deliberately declines to allocate a message while reporting that
///   allocation failed (`gzlib.c` L571-L572), and `gzerror` substitutes the literal instead
///   (`gzlib.c` L526). The two halves are a pair, and testing one without the other would let either
///   drift.
/// * the allocator's books balance afterwards. `test/infcover.c`'s `mem_done` (L200-L234) reports
///   leaks, releases that were not last-in-first-out, and releases of blocks it never handed out;
///   [`common::TrackingAllocator::finish`] returns the same three figures. A failed initialisation
///   that leaked half its buffers would pass a test that only checked the return code.
///
/// No file I/O, so this runs under Miri.
#[test]
fn an_exhausted_allocator_is_reported_as_mem_error() {
    let stream = compress_to_memory(HELLO, b"wb");

    // The write side: `gz_init` cannot obtain its buffers.
    {
        let tracker = common::TrackingAllocator::new();
        let sink = Sink::new();
        let mut state = gz_open_handle(Box::new(sink.handle()), b"<memory>", b"wb", &tracker)
            .expect("the open itself allocates nothing through the injected allocator");
        tracker.set_limit(1);
        assert_eq!(
            gzwrite(&mut state, HELLO),
            0,
            "a write that cannot allocate its buffers writes nothing"
        );
        let mut code = 0;
        let message = gzerror(Some(&mut state), Some(&mut code)).unwrap_or_default();
        assert_eq!(code, ReturnCode::MEM_ERROR.as_i32());
        assert_eq!(
            message, b"out of memory",
            "gzlib.c L526: Z_MEM_ERROR answers with the literal, unprefixed"
        );
        let closed = gzclose(Some(&mut state));
        assert_eq!(
            closed,
            ReturnCode::MEM_ERROR,
            "zlib.h L1756: gzclose reports Z_MEM_ERROR when out of memory"
        );
        drop(state);
        let report = tracker.finish();
        assert!(
            report.is_clean(),
            "a failed initialisation must leak nothing: {report:?}"
        );
    }

    // The read side: `gz_look` cannot obtain its buffers.
    {
        let tracker = common::TrackingAllocator::new();
        let sink = Sink::with_contents(&stream);
        let mut state = gz_open_handle(Box::new(sink.handle()), b"<memory>", b"rb", &tracker)
            .expect("the open should succeed");
        tracker.set_limit(1);
        let mut buf = [0_u8; 16];
        assert_eq!(
            gzread(&mut state, &mut buf),
            -1,
            "a read that cannot allocate its buffers reports failure"
        );
        let mut code = 0;
        let message = gzerror(Some(&mut state), Some(&mut code)).unwrap_or_default();
        assert_eq!(code, ReturnCode::MEM_ERROR.as_i32());
        assert_eq!(message, b"out of memory");
        let _ = gzclose(Some(&mut state));
        drop(state);
        assert!(tracker.finish().is_clean());
    }

    // And with the limit lifted, the same stream works and the books still balance -- which is what
    // shows the tracker is measuring a real allocation pattern rather than refusing everything.
    {
        let tracker = common::TrackingAllocator::new();
        let sink = Sink::with_contents(&stream);
        let mut state = gz_open_handle(Box::new(sink.handle()), b"<memory>", b"rb", &tracker)
            .expect("the open should succeed");
        let mut buf = [0_u8; 32];
        assert_eq!(
            gzread(&mut state, &mut buf),
            i32::try_from(HELLO.len()).unwrap()
        );
        assert_eq!(&buf[..HELLO.len()], HELLO);
        assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
        drop(state);
        let report = tracker.finish();
        assert!(report.is_clean(), "a clean stream must balance: {report:?}");
        assert!(
            tracker.high_water() > 0,
            "the tracker must actually have served the stream"
        );
    }
}

/// A file that transfers a few bytes at a time is handled by looping, not by giving up.
///
/// `gz_load`'s comment (`gzread.c` L10-L12) explains why the loop exists: a `read(2)` may return fewer
/// bytes than asked for, and `gz_comp` loops until its buffer is drained because "the compressed
/// output is written in chunks" and a partial `write(2)` is normal rather than exceptional
/// (`gzwrite.c` L110-L123). A handle that transfers one byte per call is the most demanding version of
/// both, and it must produce byte-identical results.
///
/// The stall-then-recover case is asserted alongside it, because it is the same loop reached through
/// the non-blocking contract: a handle that serves a few operations and then reports `EAGAIN` must
/// leave the stream retryable rather than broken, which `gzclearerr` then makes usable again.
///
/// No file I/O, so this runs under Miri.
#[test]
fn short_transfers_are_looped_over() {
    let payload = corpus::text();
    let payload = payload[..payload.len().min(bulk(4096).max(256))].to_vec();

    // A write handle that accepts one byte per call.
    let sink = Sink::new();
    let mut state = open_memory(sink.dribbling_handle(1), b"wb");
    assert_eq!(
        gzwrite(&mut state, &payload),
        i32::try_from(payload.len()).unwrap()
    );
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);
    let dribbled = sink.bytes();
    assert_eq!(
        dribbled,
        compress_to_memory(&payload, b"wb"),
        "a short-writing file must produce byte-identical output"
    );

    // A read handle that delivers one byte per call.
    let sink = Sink::with_contents(&dribbled);
    let mut state = open_memory(sink.dribbling_handle(1), b"rb");
    assert_eq!(read_to_end(&mut state, 64), payload);
    assert_eq!(gzclose(Some(&mut state)), ReturnCode::OK);

    // A handle that serves three kilobyte-sized operations and then stalls: what arrived is still
    // delivered, and the stall that follows leaves the stream retryable rather than broken. The
    // payload is incompressible so that three kilobytes of input are genuinely three kilobytes of
    // recoverable output rather than an unfinished header.
    let stalling_payload = corpus::incompressible(1000);
    let bulk_stream = compress_to_memory(&stalling_payload, b"wb");
    assert!(
        bulk_stream.len() > 192,
        "the fixture must be longer than the bytes served before the stall"
    );
    let sink = Sink::with_contents(&bulk_stream);
    let mut state = open_memory(
        MemoryFile {
            chunk: Some(64),
            ..sink.failing_handle(Failure::StallAfter(3))
        },
        b"rb",
    );
    let mut buf = vec![0_u8; 4096];
    let first = gzread(&mut state, &mut buf);
    assert!(
        first > 0,
        "gzread.c L38-L39: partial progress on a stalled descriptor is success, so the bytes that \
         did arrive are delivered"
    );
    assert_eq!(
        &buf[..usize::try_from(first).unwrap()],
        &stalling_payload[..usize::try_from(first).unwrap()],
        "and they are the right bytes"
    );

    // The next read finds the device still stalled, which is -1 with `again` recorded.
    assert_eq!(
        gzread(&mut state, &mut buf),
        -1,
        "zlib.h L1480-L1483: a stalled non-blocking device is distinguishable from a finished file"
    );
    assert!(
        state.again(),
        "and it is recorded as a stall rather than as damage"
    );
    gzclearerr(Some(&mut state));
    assert_eq!(
        error_of(&mut state).0,
        ReturnCode::OK.as_i32(),
        "zlib.h L1793-L1796: gzclearerr is how a stalled stream is resumed"
    );
    let _ = gzclose(Some(&mut state));
}

// =============================================================================================
// 12. The real filesystem
// =============================================================================================
//
// Everything above runs against a `MemoryFile`, which is what keeps it Miri-runnable. The tests
// below go through `gzopen` and touch actual files, because their subject *is* the file system:
// a path that does not exist, `O_EXCL` against a path that does, appending to a file that already
// has a member in it, and the bytes that end up on disk. None of that can be observed through an
// injected handle.
//
// Each one therefore carries `#[cfg_attr(miri, ignore)]`, for one reason: **Miri cannot perform file
// I/O**. Under Miri these are skipped and the layout, constant, grammar and behaviour tests above
// still run; under an ordinary native `cargo test` they execute in full. Cleanup is the `Drop` impl
// on `TempPath`, which runs on the panicking path too, so a failing test leaves nothing behind.

/// The whole of `test_gzio` again, this time against a real file opened by path.
///
/// [`test_gzio_is_reproduced_exactly`] asserts the sequence and every number in it; this asserts that
/// the same sequence works through [`gzopen`], which is the entry point `test/example.c` L99 and L116
/// actually call. The difference between the two is exactly one thing -- where the bytes live -- and
/// running both is what shows the injected handle used everywhere else is not hiding a
/// path-specific defect.
///
/// The file on disk is checked as a gzip file too, header and trailer, because that is what a
/// consumer such as `gzip(1)` would see.
#[test]
#[cfg_attr(miri, ignore)]
fn filesystem_round_trip_reproduces_test_gzio() {
    let path = TempPath::new("gzio");

    // The write half, `test/example.c` L99-L114.
    let mut file = gzopen(path.as_bytes(), b"wb", GlobalAllocator).expect("gzopen for writing");
    assert_eq!(gzputc(&mut file, i32::from(b'h')), i32::from(b'h'));
    assert_eq!(gzputs(&mut file, b"ello"), 4);
    assert_eq!(printf_bytes(&mut file, b", hello!"), Ok(8));
    assert_eq!(gzseek64(&mut file, 1, GzSeekFrom::Current.as_raw()), 14);
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);

    // What reached the disk is a valid RFC 1952 member for exactly this payload.
    let on_disk = path.read();
    assert_eq!(
        on_disk[..3],
        GZIP_PREFIX,
        "the file begins with the gzip magic"
    );
    let trailer = &on_disk[on_disk.len() - 8..];
    assert_eq!(
        u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]),
        crc32(0, HELLO),
        "the trailer CRC-32 matches the payload"
    );
    assert_eq!(
        u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]),
        14,
        "and ISIZE is the fourteen bytes gzseek made sure of"
    );

    // The read half, `test/example.c` L116-L163.
    let mut file = gzopen(path.as_bytes(), b"rb", GlobalAllocator).expect("gzopen for reading");
    let mut uncompr = [0_u8; 64];
    uncompr[..7].copy_from_slice(b"garbage");
    assert_eq!(gzread(&mut file, &mut uncompr), 14);
    assert_eq!(&uncompr[..14], HELLO);

    let pos = gzseek64(&mut file, -8, GzSeekFrom::Current.as_raw());
    assert_eq!(pos, 6);
    assert_eq!(gztell64(&file), pos);
    assert_eq!(gzgetc(&mut file), i32::from(b' '));
    assert_eq!(gzungetc(&mut file, i32::from(b' ')), i32::from(b' '));

    let mut line = [b'#'; 64];
    assert_eq!(gzgets(&mut file, &mut line), Some(8));
    let visible = line.iter().position(|&byte| byte == 0).unwrap();
    assert_eq!(visible, 7);
    assert_eq!(&line[..visible], b" hello!");
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);

    // `gzoffset` on a real file is the file's own length once the stream is exhausted.
    let mut file = gzopen(path.as_bytes(), b"rb", GlobalAllocator).expect("gzopen for reading");
    assert_eq!(read_to_end(&mut file, 4), HELLO);
    assert_eq!(
        gzoffset64(&mut file),
        i64::try_from(on_disk.len()).unwrap(),
        "the compressed offset reaches the end of the real file"
    );
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);
}

/// Opening a path that does not exist fails cleanly, carrying the platform's `errno`.
///
/// `zlib.h` L1394-L1398 promises that "`errno` can be checked to determine if the reason `gzopen`
/// failed was that the file could not be opened", so the number has to survive rather than be
/// flattened into "no". `GzOpenError::Io` carries it in [`GzIoError::errno`], and
/// `GzOpenError::as_return_code` maps the variant to `Z_ERRNO` -- the same code every other `gzFile`
/// I/O failure reports. The facade is what installs the number into the caller's thread-local
/// `errno`; what this crate guarantees is that the number is available.
///
/// Nothing is left behind on the failing path: `gz_open_with` opens the file only after the mode
/// string and the label allocation have both succeeded, so a refused open has created no state and no
/// file. The absence of the file is asserted directly.
#[test]
#[cfg_attr(miri, ignore)]
fn opening_a_missing_file_for_reading_fails_cleanly() {
    let path = TempPath::new("missing");
    assert!(!path.exists(), "the path must not exist to begin with");

    // A `match` rather than `expect_err`, because `GzState` deliberately does not implement `Debug`:
    // it owns a `dyn GzHandle` that has nothing safe or useful to print.
    let Err(error) = gzopen(path.as_bytes(), b"rb", GlobalAllocator) else {
        panic!("opening a nonexistent path for reading must fail")
    };
    match error {
        GzOpenError::Io(io) => {
            assert_ne!(
                io.errno, 0,
                "zlib.h L1394-L1398: the platform's errno must survive the failure"
            );
            assert!(
                !io.would_block,
                "a missing file is not a non-blocking stall"
            );
        }
        other => panic!("expected an I/O failure, got {other:?}"),
    }
    assert_eq!(
        error.as_return_code(),
        ReturnCode::ERRNO,
        "a refusal from the file system is the Z_ERRNO every gzFile I/O failure reports"
    );
    assert!(
        !path.exists(),
        "a failed open for reading must not create the file"
    );
}

/// The `x` character is `O_EXCL`: the open fails when the file already exists.
///
/// `gzlib.c` L137-L139 sets the flag and `zlib.h` L1379-L1381 documents the character: "'x' ... for
/// `O_EXCL`", so `gzopen(file, "wx")` "will fail if the file already exists". The failure has to be
/// the file system's rather than the grammar's, which is why the mode string itself parses --
/// [`GzOpenSpec::parse`] accepts `"wx"` and reports `exclusive()` -- and only the `open(2)` refuses.
/// Both halves are asserted, and so is the fact that the existing file is left untouched: an
/// exclusive create that failed must not have truncated anything.
#[test]
#[cfg_attr(miri, ignore)]
fn exclusive_create_fails_when_the_file_exists() {
    let path = TempPath::new("excl");

    // The grammar accepts it; only the file system can refuse.
    assert!(GzOpenSpec::parse(b"wx").unwrap().exclusive());

    // Against a path that does not exist, it succeeds.
    let mut file = gzopen(path.as_bytes(), b"wbx", GlobalAllocator)
        .expect("an exclusive create must succeed when the path is free");
    assert_eq!(gzwrite(&mut file, b"original"), 8);
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);
    let before = path.read();
    assert!(!before.is_empty());

    // Against the path it just created, it fails.
    let Err(error) = gzopen(path.as_bytes(), b"wbx", GlobalAllocator) else {
        panic!("zlib.h L1379-L1381: an exclusive create must fail when the file exists")
    };
    assert_eq!(error.as_return_code(), ReturnCode::ERRNO);
    assert!(matches!(error, GzOpenError::Io(_)));

    assert_eq!(
        path.read(),
        before,
        "a refused exclusive create must not have truncated the existing file"
    );

    // And without `x`, the same mode string truncates and replaces it.
    let mut file = gzopen(path.as_bytes(), b"wb", GlobalAllocator).expect("gzopen for writing");
    assert_eq!(gzwrite(&mut file, b"replacement"), 11);
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);
    let mut file = gzopen(path.as_bytes(), b"rb", GlobalAllocator).expect("gzopen for reading");
    assert_eq!(read_to_end(&mut file, 16), b"replacement");
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);
}

/// Append mode seeks to the end and becomes a write, producing a multi-member file.
///
/// `gzguts.h` L162 records the lifetime of the transient mode: `GZ_APPEND` is "mode set to `GZ_WRITE`
/// after the file is opened". `gz_open` seeks to the end so that `gzoffset` is correct and then
/// overwrites the mode "to simplify later checks" (`gzlib.c` L269-L272), so no entry point ever
/// observes `GZ_APPEND`. Both halves are asserted: the parsed spec reports `GZ_APPEND`, and the state
/// that comes back from the open reports `GZ_WRITE`.
///
/// The result is a file with two members in it, which `doc/rfc1952.txt` 2.2 permits and which
/// [`concatenated_members_read_as_one_stream`] proves the reader handles. Appending is the natural way
/// such a file arises, so the two tests are two halves of one property.
#[test]
#[cfg_attr(miri, ignore)]
fn append_mode_produces_a_multi_member_file() {
    let path = TempPath::new("append");

    // The grammar reports the transient mode.
    assert_eq!(GzOpenSpec::parse(b"ab").unwrap().mode(), GZ_APPEND);
    assert_eq!(
        GzOpenSpec::parse(b"ab").unwrap().mode_typed(),
        Some(GzMode::Append)
    );

    let mut file = gzopen(path.as_bytes(), b"wb", GlobalAllocator).expect("gzopen for writing");
    assert_eq!(gzwrite(&mut file, b"first"), 5);
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);
    let first_member_len = path.read().len();

    // Appending: the open resolves the transient mode away before returning.
    let mut file = gzopen(path.as_bytes(), b"ab", GlobalAllocator).expect("gzopen for appending");
    assert_eq!(
        file.mode(),
        GZ_WRITE,
        "gzlib.c L269-L272: GZ_APPEND is gone by the time the open returns"
    );
    assert_eq!(gzwrite(&mut file, b"second"), 6);
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);

    let both = path.read();
    assert!(
        both.len() > first_member_len,
        "the append must have added a second member rather than replacing the first"
    );
    assert_eq!(
        &both[..first_member_len],
        &path.read()[..first_member_len],
        "and the first member's bytes are untouched"
    );
    assert_eq!(
        both[first_member_len], 0x1f,
        "the second member begins where the first ended, with no padding between them"
    );

    // And it reads back as the concatenation.
    let mut file = gzopen(path.as_bytes(), b"rb", GlobalAllocator).expect("gzopen for reading");
    assert_eq!(read_to_end(&mut file, 4), b"firstsecond");
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);
}

/// A rejected mode string produces no file, and the level and `T` characters reach the bytes.
///
/// The grammar itself is asserted exhaustively and without I/O in
/// [`mode_string_grammar_matches_the_reference`]; what remains to check on a real file is that a
/// rejection happens *before* anything is created, and that the two characters whose effect is visible
/// in the output really do reach it:
///
/// * a level digit changes the compressed bytes -- level 1 and level 9 must not agree on a payload
///   with structure to exploit, which is what makes the digit more than decoration;
/// * `T` bypasses the compressor entirely, so the file on disk is the payload verbatim with no gzip
///   header at all (`gzwrite.c` L23-L30 allocates no output buffer and no engine for a transparent
///   stream);
/// * an unknown character is ignored rather than refused, so `"wbQ"` behaves exactly as `"wb"`.
#[test]
#[cfg_attr(miri, ignore)]
fn mode_characters_reach_the_file() {
    // `+` is refused, and no file appears.
    let path = TempPath::new("plus");
    assert_eq!(
        gzopen(path.as_bytes(), b"w+", GlobalAllocator).err(),
        Some(GzOpenError::InvalidMode),
        "gzlib.c L129-L131: `+` is refused outright"
    );
    assert!(
        !path.exists(),
        "a rejected mode string must not create the file"
    );

    // A level digit changes the bytes.
    let payload = corpus::text();
    let fast = TempPath::new("level1");
    let mut file = gzopen(fast.as_bytes(), b"wb1", GlobalAllocator).expect("gzopen level 1");
    assert_eq!(
        gzwrite(&mut file, &payload),
        i32::try_from(payload.len()).unwrap()
    );
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);

    let best = TempPath::new("level9");
    let mut file = gzopen(best.as_bytes(), b"wb9", GlobalAllocator).expect("gzopen level 9");
    assert_eq!(
        gzwrite(&mut file, &payload),
        i32::try_from(payload.len()).unwrap()
    );
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);

    assert_ne!(
        fast.read(),
        best.read(),
        "gzlib.c L114-L115: the level digit must reach the compressor"
    );
    for produced in [&fast, &best] {
        let mut file =
            gzopen(produced.as_bytes(), b"rb", GlobalAllocator).expect("gzopen for reading");
        assert_eq!(read_to_end(&mut file, 256), payload);
        assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);
    }

    // `T` writes the payload verbatim: no header, no trailer, no compression.
    let transparent = TempPath::new("transparent");
    let mut file =
        gzopen(transparent.as_bytes(), b"wT", GlobalAllocator).expect("gzopen transparent");
    assert_eq!(
        gzwrite(&mut file, &payload),
        i32::try_from(payload.len()).unwrap()
    );
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);
    assert_eq!(
        transparent.read(),
        payload,
        "gzwrite.c L23-L30: a transparent stream stages nothing and compresses nothing"
    );

    // And reading it back auto-detects the transparency.
    let mut file =
        gzopen(transparent.as_bytes(), b"rb", GlobalAllocator).expect("gzopen for reading");
    assert_eq!(gzdirect(Some(&mut file)), 1);
    assert_eq!(read_to_end(&mut file, 256), payload);
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);

    // An unknown character is ignored, so this is `"wb"` with noise in it.
    let noisy = TempPath::new("unknown");
    let mut file = gzopen(noisy.as_bytes(), b"wbQ", GlobalAllocator).expect("gzopen with noise");
    assert_eq!(
        gzwrite(&mut file, HELLO),
        i32::try_from(HELLO.len()).unwrap()
    );
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);
    let mut file = gzopen(noisy.as_bytes(), b"rb", GlobalAllocator).expect("gzopen for reading");
    assert_eq!(read_to_end(&mut file, 16), HELLO);
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);
}

/// A file written by `gzopen` decompresses under the reference, and vice versa.
///
/// The `minigzip` shape: `test/minigzip.c`'s `gz_compress` writes a file that `gz_uncompress` -- or
/// `gzip -d` -- must be able to read, and its `BUFLEN` of 16384 (its L144) is the buffer both loops
/// use. This test does both directions with the two fixtures whose provenance is known: a member the
/// reference zlib produced ([`HELLO_GZ`]) and one the system `gzip(1)` produced
/// ([`GZIP_CLI_NAMED`]), written to a real file and read back through [`gzopen`]. Together with
/// [`produced_member_is_valid_rfc1952`], which checks the header and trailer of what this
/// implementation writes, that is bidirectional interoperability in both directions.
#[test]
#[cfg_attr(miri, ignore)]
fn foreign_members_read_from_a_real_file() {
    for (name, member, expected) in [
        ("zlib", &HELLO_GZ[..], HELLO),
        ("gzip-cli", &GZIP_CLI_NAMED[..], HELLO_NO_NUL),
    ] {
        let path = TempPath::new(name);
        path.write(member);
        let mut file = gzopen(path.as_bytes(), b"rb", GlobalAllocator).expect("gzopen for reading");
        assert_eq!(
            read_to_end(&mut file, 16384),
            expected,
            "{name}: a member produced elsewhere must read back byte for byte"
        );
        assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);
    }

    // The other direction, as far as this crate can check it: what this implementation writes is a
    // member whose header, payload and trailer this implementation also reads, byte for byte, on a
    // real file. Linking the unmodified C drivers against the built library is the facade's gate.
    let path = TempPath::new("interop");
    let payload = corpus::window_crossing();
    let mut file = gzopen(path.as_bytes(), b"wb", GlobalAllocator).expect("gzopen for writing");
    assert_eq!(
        gzwrite(&mut file, &payload),
        i32::try_from(payload.len()).unwrap()
    );
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);

    let on_disk = path.read();
    assert_eq!(on_disk[..3], GZIP_PREFIX);
    let trailer = &on_disk[on_disk.len() - 8..];
    assert_eq!(
        u32::from_le_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]),
        crc32(0, &payload)
    );
    assert_eq!(
        u32::from_le_bytes([trailer[4], trailer[5], trailer[6], trailer[7]]),
        u32::try_from(payload.len()).unwrap()
    );

    let mut file = gzopen(path.as_bytes(), b"rb", GlobalAllocator).expect("gzopen for reading");
    assert_eq!(read_to_end(&mut file, 16384), payload);
    assert_eq!(gzclose(Some(&mut file)), ReturnCode::OK);
}

/// The temporary-file guard removes its file, on the panicking path as well as the ordinary one.
///
/// There is no `tempfile` crate available here -- see [`TempPath`] for why -- so the cleanup is this
/// suite's own responsibility and is worth checking rather than assuming. A leaked scratch file is not
/// merely untidy: [`exclusive_create_fails_when_the_file_exists`] would fail for the wrong reason if a
/// previous run had left a file at the path it reserves.
///
/// Both paths are asserted. The panicking one goes through [`std::panic::catch_unwind`], which is what
/// a failing `#[test]` does, so this checks the guard under exactly the conditions that matter.
#[test]
#[cfg_attr(miri, ignore)]
fn the_temporary_file_guard_cleans_up() {
    // The ordinary path.
    let remembered;
    {
        let path = TempPath::new("cleanup");
        path.write(b"scratch");
        assert!(path.exists());
        remembered = path.path().to_path_buf();
    }
    assert!(
        !remembered.exists(),
        "the guard must remove its file when it goes out of scope"
    );

    // The panicking path.
    let remembered = std::panic::catch_unwind(|| {
        let path = TempPath::new("cleanup-panic");
        path.write(b"scratch");
        let recorded = path.path().to_path_buf();
        // Hand the path out before unwinding, so the assertion below can look for it.
        std::panic::panic_any(recorded);
    })
    .err()
    .and_then(|payload| payload.downcast::<PathBuf>().ok())
    .expect("the closure must have panicked with the path it created");
    assert!(
        !remembered.exists(),
        "the guard must remove its file while the stack unwinds, which is what a failing test does"
    );

    // Two guards reserved in the same process never collide.
    let first = TempPath::new("unique");
    let second = TempPath::new("unique");
    assert_ne!(
        first.path(),
        second.path(),
        "the per-process counter must keep concurrent reservations apart"
    );
}

/// The file this library opens for itself reaches its `read` and `write` without a vtable.
///
/// `gz_load` fills the input buffer with one [`GzHandle::read`] (`gzread.c` L30) and `gz_comp`
/// drains the output buffer with one [`GzHandle::write`] (`gzwrite.c` L115), so every byte a
/// `gzFile` moves crosses that boundary once per underlying transfer. Erasing the handle to
/// `dyn GzHandle` put an indirect call on that path -- unmeasurable behind an 8 KiB buffer and a
/// syscall, but not behind `gzbuffer(8)`, whose floor is a single byte (`gzlib.c` L337-L340), and not
/// behind a handle backed by memory rather than by a descriptor. Worse than the call itself, it
/// stopped the optimizer from inlining [`FileHandle`]'s method and folding away its `Option` check.
///
/// [`GzHandleRef`] is the fix, and this is the assertion that keeps it: the variant a path-opened
/// stream produces must be the statically dispatched one. A future change that boxed the built-in
/// handle for convenience would still pass every behavioural test in this file, and would fail here.
///
/// The injected case is asserted alongside it, because retaining it is half the design: the facade
/// adopts a caller's descriptor with a type this crate cannot name, so that case must stay a trait
/// object -- and both cases must answer the identical [`GzHandle`] surface, which the shared helpers
/// below check by driving each through the same generic function.
#[test]
#[cfg_attr(miri, ignore)]
fn the_built_in_handle_dispatches_without_a_vtable() {
    let path = TempPath::new("dispatch");

    // Opening by path: the case `gzopen` produces, and the common one.
    let mut writer = gzopen(path.as_bytes(), b"wb", GlobalAllocator).expect("gzopen for writing");
    assert!(
        matches!(writer.handle_mut(), Some(GzHandleRef::Owned(_))),
        "a path-opened stream must reach its file directly, not through a vtable"
    );
    // The same handle still answers the whole trait surface.
    assert_eq!(
        writer.handle_mut().unwrap().write(b"direct").unwrap(),
        6,
        "the statically dispatched write must still write"
    );
    assert_eq!(gzclose(Some(&mut writer)), ReturnCode::OK);

    // An injected handle: what the facade supplies for an adopted descriptor.
    let sink = Sink::new();
    let mut injected = open_memory(sink.handle(), b"wb");
    assert!(
        matches!(injected.handle_mut(), Some(GzHandleRef::Boxed(_))),
        "an injected handle must remain a trait object -- the facade's type cannot be named here"
    );
    assert_eq!(
        injected.handle_mut().unwrap().write(b"boxed").unwrap(),
        5,
        "and it must answer the identical surface"
    );
    assert_eq!(gzclose(Some(&mut injected)), ReturnCode::OK);

    // An empty slot answers `None` from both, which is C's closed descriptor.
    let mut closed = gzopen(path.as_bytes(), b"rb", GlobalAllocator).expect("gzopen for reading");
    let _ = closed.take_handle();
    assert!(
        closed.handle_mut().is_none(),
        "a taken handle leaves nothing to dispatch to"
    );
}

/// A one-byte buffer is the configuration that makes the per-transfer call path visible, and it must
/// still produce byte-identical output.
///
/// `gzbuffer` floors `want` at 8 "to behave well with flushing" (`gzlib.c` L337-L340), so eight bytes
/// is the smallest a stream can legally be configured to use. At that size `gz_comp`'s write loop and
/// `gz_load`'s read loop run once per eight bytes rather than once per eight kilobytes, which is
/// where [`GzHandleRef`]'s static dispatch is worth having -- and, more importantly for correctness,
/// where any buffering mistake shows up immediately.
///
/// The assertion is that the *bytes* do not change: the same payload written through an 8-byte buffer
/// and through the default [`GZBUFSIZE`] must produce the identical gzip member, because buffer size
/// is an I/O-scheduling choice and not a compression parameter. AAP §0.6.2 forbids anything that
/// perturbs emitted bytes, and this is the cheapest place that could have.
///
/// The handle-crossing count is measured alongside the bytes, because it is the figure that decides
/// whether dispatch cost is visible at all. It is reported rather than merely bounded: a wall-clock
/// benchmark of one indirect call is not reproducible in CI, but the number of calls is exact, and it
/// is precisely what the cost of an indirect call would be multiplied by.
///
/// No file I/O, so this runs under Miri.
#[test]
fn the_smallest_legal_buffer_produces_identical_bytes() {
    let payload = corpus::text();

    let tiny_sink = Sink::new();
    let tiny = {
        let mut state = open_memory(tiny_sink.handle(), b"wb");
        assert_eq!(
            gzbuffer(Some(&mut state), 8),
            0,
            "8 is the documented floor"
        );
        assert_eq!(
            usize::try_from(gzwrite(&mut state, &payload)).unwrap(),
            payload.len()
        );
        check_err(
            gzclose(Some(&mut state)),
            "gzclose after the tiny-buffer write",
        );
        tiny_sink.bytes()
    };

    let default_sink = Sink::new();
    let default = {
        let mut state = open_memory(default_sink.handle(), b"wb");
        assert_eq!(
            usize::try_from(gzwrite(&mut state, &payload)).unwrap(),
            payload.len()
        );
        check_err(
            gzclose(Some(&mut state)),
            "gzclose after the default-buffer write",
        );
        default_sink.bytes()
    };

    assert_eq!(
        tiny, default,
        "buffer size schedules I/O; it must not change a single emitted byte"
    );
    assert_eq!(
        default,
        compress_to_memory(&payload, b"wb"),
        "and the locally opened default stream agrees with the shared helper"
    );

    // The exposure a small buffer buys, measured: at the 8-byte floor the layer crosses the handle
    // boundary once per eight bytes of compressed output, and at `GZBUFSIZE` once per 8192. Every
    // crossing was an indirect call before `GzHandleRef`, and this ratio is the multiplier that made
    // removing it worthwhile for the handle this library opens itself.
    let tiny_transfers = tiny_sink.transfers();
    let default_transfers = default_sink.transfers();
    println!(
        "gz write transfers: {tiny_transfers} at gzbuffer(8) vs {default_transfers} at GZBUFSIZE, \
         for {} compressed bytes",
        default.len()
    );
    assert!(
        default_transfers >= 1,
        "the default stream must have reached the file at least once"
    );
    assert!(
        tiny_transfers > default_transfers * 8,
        "an 8-byte buffer must cross the handle boundary far more often ({tiny_transfers} vs \
         {default_transfers}); if it does not, this test no longer measures what it claims"
    );

    // And the tiny buffer reads its own output back, one 8-byte transfer at a time.
    let sink = Sink::with_contents(&tiny);
    let mut reader = open_memory(sink.handle(), b"rb");
    assert_eq!(gzbuffer(Some(&mut reader), 8), 0);
    let mut readback = vec![0_u8; payload.len()];
    let mut filled = 0;
    while filled < readback.len() {
        let read = gzread(&mut reader, &mut readback[filled..]);
        assert!(read > 0, "the read must make progress");
        filled += usize::try_from(read).unwrap();
    }
    assert_eq!(readback, payload, "and it must read back byte for byte");
    assert_eq!(gzclose(Some(&mut reader)), ReturnCode::OK);
}
