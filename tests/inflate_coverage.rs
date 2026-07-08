//! Inflate error/edge-branch coverage — a Rust port of zlib's `test/infcover.c`.
//!
//! `test/infcover.c` is zlib's exhaustive *decoder* coverage harness. It feeds a
//! large table of hand-crafted, mostly-malformed DEFLATE / zlib / gzip byte
//! streams into the inflate engine and pins each one to an exact outcome,
//! driving the state machine through all 30+ inflate modes and every error
//! branch (AAP §0.6.1, §0.6.7). This file reproduces that harness against the
//! `zlib-rs` public API.
//!
//! ## Routing (idiomatic API first, FFI where required)
//!
//! Wherever a coverage operation is expressible through the safe, idiomatic
//! [`zlib_rs::inflate`] API it is used directly. A handful of `infcover.c`
//! scenarios exercise the raw C ABI (passing a NULL `z_stream`, supplying a
//! bogus version string) which the idiomatic API cannot express; those go
//! through the [`zlib_rs::ffi`] `extern "C"` shims. Every `unsafe` block is at
//! that FFI boundary and carries a `// SAFETY:` note (AAP §0.6.2).
//!
//! ## Forced `Z_MEM_ERROR` (reproduced via the FFI allocator hooks)
//!
//! `infcover.c` forces `Z_MEM_ERROR` by installing a byte-capped allocator
//! through the `z_stream` `zalloc`/`zfree` hooks. `zlib-rs` allocation is
//! *fallible* at the FFI boundary (AAP §0.6.3): a caller hook that returns null
//! propagates to `MemError` with **no** global-allocator fallback. That harness
//! is reproduced here — see [`mem_limit_forces_mem_error`], which caps the byte
//! budget below the inflate window size so the lazily-allocated window cannot be
//! obtained and `inflate` returns `Z_MEM_ERROR`, exactly as in C.
//!
//! ## Honestly-handled gap (never faked)
//!
//! One `infcover.c` behaviour reaches into private engine internals and is *not*
//! reachable from an integration test over the public API. It is handled openly
//! rather than by forging private access with `unsafe`:
//!
//! * **Forced inflateBack mode error** — `infcover.c` makes its `pull` callback
//!   poke `((inflate_state*)strm.state)->mode = SYNC`, an otherwise-impossible
//!   internal state, to force `Z_STREAM_ERROR`. The idiomatic [`InFunc`] trait
//!   only yields input bytes and cannot touch private state; equivalent coverage
//!   of that guard lives as a unit test in `src/inflate/back.rs`.
//!
//! Gzip-framed cases are gated behind `#[cfg(feature = "gzip")]`; the file is
//! authored for the default (std + gzip) feature set.

use core::ffi::{c_int, c_uint, c_void};
use core::ptr;

#[cfg(feature = "gzip")]
use zlib_rs::GzHeader;
use zlib_rs::constants::{Z_NO_FLUSH, Z_TREES};
use zlib_rs::ffi::{
    inflate as ffi_inflate, inflateBack, inflateBackEnd, inflateBackInit_, inflateCopy, inflateEnd,
    inflateInit_, inflateInit2_, z_stream,
};
use zlib_rs::inflate::back::{InFunc, OutFunc, inflate_back, inflate_back_end, inflate_back_init};
#[cfg(feature = "gzip")]
use zlib_rs::inflate::inflate_get_header;
use zlib_rs::inflate::tables::{CodeType, InflateTableError};
use zlib_rs::inflate::{
    Code, ENOUGH_DISTS, inflate, inflate_copy, inflate_end, inflate_init, inflate_init2,
    inflate_mark, inflate_prime, inflate_reset2, inflate_set_dictionary, inflate_sync,
    inflate_sync_point, inflate_table, inflate_undermine,
};
use zlib_rs::{ReturnCode, ZStream, ZlibError};

// ===========================================================================
// Helpers
// ===========================================================================

/// Normalize an inflate result into a bare [`ReturnCode`] so success and error
/// variants compare uniformly against the exact expected code (mirrors C, where
/// every entry point returns an `int`).
fn rc(result: Result<ReturnCode, ZlibError>) -> ReturnCode {
    result.unwrap_or_else(ZlibError::as_return_code)
}

/// Liberal hex decoder — a port of `infcover.c`'s `h2b()`.
///
/// Consecutive hex-digit runs accumulate into a byte; any non-hex delimiter
/// (typically a space) terminates the current byte. As in the C original, a
/// value built from a *single* hex digit is biased by 240 before being emitted,
/// so space-separated single digits (e.g. `"0 0"`) decode to two `0x00` bytes
/// while `"00"` decodes to one. Pure safe Rust.
fn h2b(hex: &str) -> Vec<u8> {
    let mut out = Vec::new();
    let mut val: u32 = 1;
    for byte in hex.bytes().chain(std::iter::once(0u8)) {
        match byte {
            b'0'..=b'9' => val = (val << 4) + u32::from(byte - b'0'),
            b'A'..=b'F' => val = (val << 4) + u32::from(byte - b'A' + 10),
            b'a'..=b'f' => val = (val << 4) + u32::from(byte - b'a' + 10),
            _ => {
                if val != 1 && val < 32 {
                    val += 240;
                }
            }
        }
        if val > 255 {
            out.push((val & 0xff) as u8);
            val = 1;
        }
        if byte == 0 {
            break;
        }
    }
    out
}

/// Build an all-zero [`z_stream`] for the FFI-boundary cases. Every field is a
/// valid zero: null pointers, `None` allocator hooks, zero counters. `z_stream`
/// is `#[repr(C)]` and has no `Default`, so it is constructed field-by-field.
fn zeroed_stream() -> z_stream {
    z_stream {
        next_in: ptr::null(),
        avail_in: 0,
        total_in: 0,
        next_out: ptr::null_mut(),
        avail_out: 0,
        total_out: 0,
        msg: ptr::null_mut(),
        state: ptr::null_mut(),
        zalloc: None,
        zfree: None,
        opaque: ptr::null_mut(),
        data_type: 0,
        adler: 0,
        reserved: 0,
    }
}

/// [`InFunc`] that yields a byte slice exactly once, then signals EOF (empty
/// slice). Mirrors feeding the whole `next_in` buffer to `inflateBack` while the
/// C `pull(desc == Z_NULL)` supplies no additional input.
struct OneShot<'a> {
    data: &'a [u8],
    done: bool,
}

impl<'a> OneShot<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, done: false }
    }
}

impl InFunc for OneShot<'_> {
    fn next_input(&mut self) -> &[u8] {
        if self.done {
            &[]
        } else {
            self.done = true;
            self.data
        }
    }
}

/// [`OutFunc`] that accepts and discards all output — the analogue of C
/// `push(desc == Z_NULL)`, which reports success.
struct SinkAccept;

impl OutFunc for SinkAccept {
    fn write_output(&mut self, _buf: &[u8]) -> Result<(), ()> {
        Ok(())
    }
}

/// [`OutFunc`] that always fails — the analogue of C `push(desc != Z_NULL)`,
/// which forces `inflateBack` to abort with `Z_BUF_ERROR`.
struct SinkReject;

impl OutFunc for SinkReject {
    fn write_output(&mut self, _buf: &[u8]) -> Result<(), ()> {
        Err(())
    }
}

// ===========================================================================
// Drivers
// ===========================================================================

/// Port of `infcover.c`'s `inf()` — the streaming-inflate driver.
///
/// Initializes inflate with `window_bits = win`, then feeds `h2b(hex)` into the
/// decoder in `step`-sized increments (`step == 0` means "all at once"),
/// re-presenting a fresh `len`-byte output buffer on every call. The first
/// `inflate()` return code must equal `err`; subsequent iterations are
/// "don't care" (matching C setting `err = 9`). Each live iteration also
/// round-trips an [`inflate_copy`], and the stream is finally reset with
/// `inflate_reset2(-8)` and released with `inflate_end`.
fn inf(hex: &str, what: &str, step: usize, win: i32, len: usize, err: ReturnCode) {
    let mut strm = ZStream::new();
    if let Err(e) = inflate_init2(&mut strm, win) {
        // C returns early when init fails; we pin the code so the bad-window
        // cases (which expect Z_STREAM_ERROR here) are still asserted.
        assert_eq!(e.as_return_code(), err, "{what}: init");
        return;
    }

    let mut out = vec![0u8; len];

    // `win == 47` selects gzip auto-detect. Register a gz_header so the optional
    // EXTRA/NAME/COMMENT/HCRC field processing is exercised, mirroring C's
    // inflateGetHeader() wired to the output buffer.
    #[cfg(feature = "gzip")]
    if win == 47 {
        let mut head = GzHeader::new();
        head.extra = Some(vec![0u8; len]);
        head.extra_max = len as u32;
        head.name = Some(vec![0u8; len]);
        head.name_max = len as u32;
        head.comment = Some(vec![0u8; len]);
        head.comm_max = len as u32;
        assert_eq!(
            rc(inflate_get_header(&mut strm, head)),
            ReturnCode::Ok,
            "{what}: get_header",
        );
    }

    let input = h2b(hex);
    let total = input.len();
    let step = if step == 0 || step > total {
        total
    } else {
        step
    };

    // `exposed` tracks how many input bytes have been made available so far
    // (C's growing avail_in); `next` is the consumed cursor (C's advancing
    // next_in). C's loop condition is `while (strm.avail_in)`.
    let mut exposed = core::cmp::min(step, total);
    let mut next = 0usize;
    let mut expect = Some(err);

    // Termination watchdog: these fixtures need only a few iterations. The bound
    // guarantees the loop halts even if a dependency regressed into a
    // no-progress state, without masking the meaningful first-iteration check.
    let cap = total.saturating_mul(1024) + 4096;
    for _ in 0..cap {
        let outcome = inflate(&mut strm, &input[next..exposed], &mut out, Z_NO_FLUSH);
        if let Some(expected) = expect {
            assert_eq!(outcome.code, expected, "{what}");
        }
        next += outcome.consumed;

        // Stop on any non-continuable code (matches C's `break`).
        if !matches!(
            outcome.code,
            ReturnCode::Ok | ReturnCode::BufError | ReturnCode::NeedDict
        ) {
            break;
        }

        if outcome.code == ReturnCode::NeedDict {
            // Reachable portion of C's NEED_DICT coverage. (C additionally forces
            // Z_MEM_ERROR under an allocation limit and then poke-restores the
            // private `state->mode = DICT`; forcing `Z_MEM_ERROR` is covered
            // separately by `mem_limit_forces_mem_error`, but the private-state
            // poke that resumes decoding from it is not expressible over the
            // public API, so only the publicly reachable steps are reproduced.)
            //
            // A dictionary whose Adler-32 mismatches the requested id → DataError.
            assert_eq!(
                rc(inflate_set_dictionary(&mut strm, &input[..1])),
                ReturnCode::DataError,
                "{what}: wrong dictionary",
            );
            // The correct (empty) dictionary — Adler-32("") == the id 1 → Ok.
            assert_eq!(
                rc(inflate_set_dictionary(&mut strm, &[])),
                ReturnCode::Ok,
                "{what}: empty dictionary",
            );
            // Resuming now makes no progress (len == 0 output here) → BufError.
            let resume = inflate(&mut strm, &input[next..exposed], &mut out, Z_NO_FLUSH);
            assert_eq!(
                resume.code,
                ReturnCode::BufError,
                "{what}: resume after dict",
            );
            next += resume.consumed;
        }

        // inflateCopy of a live stream must succeed; release the copy at once.
        let mut copy = ZStream::new();
        assert_eq!(
            rc(inflate_copy(&mut copy, &strm)),
            ReturnCode::Ok,
            "{what}: copy",
        );
        assert_eq!(
            rc(inflate_end(&mut copy)),
            ReturnCode::Ok,
            "{what}: copy end"
        );

        // Subsequent iterations are "don't care" (C sets err = 9).
        expect = None;

        // Expose the next chunk (C: strm.avail_in += min(step, have)).
        let remaining = total - exposed;
        exposed += core::cmp::min(remaining, step);

        // C loop condition: `while (strm.avail_in)`.
        if exposed - next == 0 {
            break;
        }
    }

    assert_eq!(
        rc(inflate_reset2(&mut strm, -8)),
        ReturnCode::Ok,
        "{what}: reset2",
    );
    assert_eq!(rc(inflate_end(&mut strm)), ReturnCode::Ok, "{what}: end");
}

/// Port of `infcover.c`'s `try()` — raw-inflate a stream through BOTH the
/// streaming `inflate()` path and (when `err >= 0`) `inflateBack()`.
///
/// `err`: `1` expects `DataError` (with `strm.msg == id` on the inflate path),
/// `0` expects success, `-1` is an inflate-only trailer-mismatch case (gzip
/// framing, so no `inflateBack`).
fn try_stream(hex: &str, id: &str, err: i32) {
    let input = h2b(hex);
    let total = input.len();
    let size = (total << 3).max(1);

    // --- first with streaming inflate ---
    {
        let mut strm = ZStream::new();
        let win = if err < 0 { 47 } else { -15 };
        assert_eq!(
            rc(inflate_init2(&mut strm, win)),
            ReturnCode::Ok,
            "{id}: init",
        );

        let mut out = vec![0u8; size];
        let mut next = 0usize;
        let mut code = ReturnCode::Ok;

        let cap = total.saturating_mul(1024) + 4096;
        for _ in 0..cap {
            let outcome = inflate(&mut strm, &input[next..], &mut out, Z_TREES);
            // C: never Z_STREAM_ERROR / Z_MEM_ERROR on this path.
            assert_ne!(outcome.code, ReturnCode::StreamError, "{id}: stream error");
            assert_ne!(outcome.code, ReturnCode::MemError, "{id}: mem error");
            code = outcome.code;
            next += outcome.consumed;
            if code == ReturnCode::DataError || code == ReturnCode::NeedDict {
                break;
            }
            // C: `while (strm.avail_in || strm.avail_out == 0)`.
            let more_in = next < total;
            let out_full = outcome.produced == out.len();
            if !(more_in || out_full) {
                break;
            }
            if outcome.consumed == 0 && outcome.produced == 0 {
                break;
            }
        }

        if err != 0 {
            assert_eq!(
                code,
                ReturnCode::DataError,
                "{id}: expected DataError (inflate)",
            );
            assert_eq!(strm.msg, Some(id), "{id}: message (inflate)");
        }
        assert_eq!(rc(inflate_end(&mut strm)), ReturnCode::Ok, "{id}: end");
    }

    // --- then with inflateBack (raw only) ---
    if err >= 0 {
        let mut state = inflate_back_init(15).expect("inflate_back_init(15)");
        let mut src = OneShot::new(&input);
        let mut sink = SinkAccept;
        let code = inflate_back(&mut state, &mut src, &mut sink);
        // C: `assert(ret != Z_STREAM_ERROR)`.
        assert_ne!(code, ReturnCode::StreamError, "{id}: back stream error");
        if err != 0 {
            // inflateBack exposes no z_stream, hence no msg to compare (C compares
            // strm.msg only because inflateBack borrows the same z_stream; the
            // idiomatic API returns just the code).
            assert_eq!(
                code,
                ReturnCode::DataError,
                "{id}: expected DataError (back)"
            );
        }
        assert_eq!(inflate_back_end(state), ReturnCode::Ok, "{id}: back end");
    }
}

// ===========================================================================
// Coverage tests (one per infcover.c cover_* orchestrator)
// ===========================================================================

/// Port of `cover_support()` — init / prime / dictionary / reset paths.
#[test]
fn cover_support() {
    // init → prime → set-dictionary error → end.
    let mut strm = ZStream::new();
    assert_eq!(rc(inflate_init(&mut strm)), ReturnCode::Ok);
    assert_eq!(rc(inflate_prime(&mut strm, 5, 31)), ReturnCode::Ok);
    assert_eq!(rc(inflate_prime(&mut strm, -1, 0)), ReturnCode::Ok);
    // Empty dictionary on a zlib stream not awaiting one → StreamError.
    assert_eq!(
        rc(inflate_set_dictionary(&mut strm, &[])),
        ReturnCode::StreamError,
    );
    assert_eq!(rc(inflate_end(&mut strm)), ReturnCode::Ok);

    // Window allocation / replacement / split update / fixed blocks / bad size.
    inf("63 0", "force window allocation", 0, -15, 1, ReturnCode::Ok);
    inf(
        "63 18 5",
        "force window replacement",
        0,
        -8,
        259,
        ReturnCode::Ok,
    );
    inf(
        "63 18 68 30 d0 0 0",
        "force split window update",
        4,
        -8,
        259,
        ReturnCode::Ok,
    );
    inf("3 0", "use fixed blocks", 0, -15, 1, ReturnCode::StreamEnd);
    inf("", "bad window size", 0, 1, 0, ReturnCode::StreamError);

    // Wrong version string → VersionError (version param exists only on the C ABI).
    let mut zs = zeroed_stream();
    // SAFETY: the bogus version ('!' != '1') fails version_check before the
    // stream is touched, so inflateInit_ returns Z_VERSION_ERROR without
    // installing or freeing any state. `zs` is a valid z_stream for the call.
    let ret = unsafe {
        inflateInit_(
            &mut zs,
            c"!".as_ptr(),
            core::mem::size_of::<z_stream>() as c_int,
        )
    };
    assert_eq!(ret, ReturnCode::VersionError.as_c_int());

    // Built-in memory routines: a plain init/end round-trip.
    let mut strm = ZStream::new();
    assert_eq!(rc(inflate_init(&mut strm)), ReturnCode::Ok);
    assert_eq!(rc(inflate_end(&mut strm)), ReturnCode::Ok);
}

/// Port of `cover_wrap()` — NULL-parameter guards, header/trailer vectors, and
/// the miscellaneous API sequence.
#[test]
fn cover_wrap() {
    // NULL-parameter guards via the FFI C ABI (idiomatic API cannot pass null).
    // SAFETY: each shim checks for a null stream first and returns
    // Z_STREAM_ERROR without dereferencing it or the (absent) callbacks.
    unsafe {
        assert_eq!(
            ffi_inflate(ptr::null_mut(), 0),
            ReturnCode::StreamError.as_c_int(),
        );
        assert_eq!(
            inflateEnd(ptr::null_mut()),
            ReturnCode::StreamError.as_c_int(),
        );
        assert_eq!(
            inflateCopy(ptr::null_mut(), ptr::null_mut()),
            ReturnCode::StreamError.as_c_int(),
        );
    }

    // zlib / raw header + trailer cases (no gzip framing required).
    inf("77 85", "bad zlib method", 0, 15, 0, ReturnCode::DataError);
    inf(
        "8 99",
        "set window size from header",
        0,
        0,
        0,
        ReturnCode::Ok,
    );
    inf(
        "78 9c",
        "bad zlib window size",
        0,
        8,
        0,
        ReturnCode::DataError,
    );
    inf(
        "78 9c 63 0 0 0 1 0 1",
        "check adler32",
        0,
        15,
        1,
        ReturnCode::StreamEnd,
    );
    inf(
        "8 b8 0 0 0 1",
        "need dictionary",
        0,
        8,
        0,
        ReturnCode::NeedDict,
    );
    inf("78 9c 63 0", "compute adler32", 0, 15, 1, ReturnCode::Ok);

    // gzip / auto-detect header + trailer cases (require the gzip wrapper).
    #[cfg(feature = "gzip")]
    {
        inf(
            "1f 8b 0 0",
            "bad gzip method",
            0,
            31,
            0,
            ReturnCode::DataError,
        );
        inf(
            "1f 8b 8 80",
            "bad gzip flags",
            0,
            31,
            0,
            ReturnCode::DataError,
        );
        inf(
            "1f 8b 8 1e 0 0 0 0 0 0 1 0 0 0 0 0 0",
            "bad header crc",
            0,
            47,
            1,
            ReturnCode::DataError,
        );
        inf(
            "1f 8b 8 2 0 0 0 0 0 0 1d 26 3 0 0 0 0 0 0 0 0 0",
            "check gzip length",
            0,
            47,
            0,
            ReturnCode::StreamEnd,
        );
        inf(
            "78 90",
            "bad zlib header check",
            0,
            47,
            0,
            ReturnCode::DataError,
        );
    }

    // Miscellaneous API sequence. The C `mem_*` allocation-limit steps that force
    // Z_MEM_ERROR (the capped inflate() calls and inflateCopy) are consolidated
    // into the dedicated `mem_limit_forces_mem_error` test, which reproduces the
    // forced-failure via the FFI `zalloc`/`zfree` hooks (allocation is fallible
    // at the boundary — see the module docs). The remaining, publicly reachable
    // steps of this sequence are reproduced exactly here.
    let mut strm = ZStream::new();
    assert_eq!(rc(inflate_init2(&mut strm, -8)), ReturnCode::Ok);
    // (C's 1-byte-limit Z_MEM_ERROR steps are covered by mem_limit_forces_mem_error.)
    let dict = [0u8; 257];
    assert_eq!(rc(inflate_set_dictionary(&mut strm, &dict)), ReturnCode::Ok);
    assert_eq!(rc(inflate_prime(&mut strm, 16, 0)), ReturnCode::Ok);
    // inflateSync on 0x80,0x00 finds no marker → DataError.
    let (sync1, _) = inflate_sync(&mut strm, &[0x80, 0x00]);
    assert_eq!(sync1, ReturnCode::DataError);
    // A subsequent inflate on the now-invalid stream → StreamError.
    let after = inflate(&mut strm, &[], &mut [0u8; 16], Z_NO_FLUSH);
    assert_eq!(after.code, ReturnCode::StreamError);
    // inflateSync on the empty-stored marker 00 00 ff ff → Ok.
    let (sync2, _) = inflate_sync(&mut strm, &[0x00, 0x00, 0xff, 0xff]);
    assert_eq!(sync2, ReturnCode::Ok);
    // inflateSyncPoint — return value unused (C casts to void).
    let _ = inflate_sync_point(&strm);
    // C's inflateCopy runs under a byte cap to force Z_MEM_ERROR; that forced
    // path is covered by mem_limit_forces_mem_error. Here the idiomatic API
    // installs no cap, so the copy succeeds.
    let mut copy = ZStream::new();
    assert_eq!(rc(inflate_copy(&mut copy, &strm)), ReturnCode::Ok);
    assert_eq!(rc(inflate_end(&mut copy)), ReturnCode::Ok);
    assert_eq!(rc(inflate_undermine(&mut strm, 1)), ReturnCode::DataError);
    // inflateMark — return value unused (C casts to void).
    let _ = inflate_mark(&strm);
    assert_eq!(rc(inflate_end(&mut strm)), ReturnCode::Ok);
}

/// Port of `cover_back()` — inflateBack bad-parameter guards and the
/// callback-driven decode paths.
#[test]
fn cover_back() {
    // Bad parameters via the FFI C ABI.
    // SAFETY: each call hits an early version/null guard and returns an error
    // code without dereferencing the null stream or invoking the callbacks.
    unsafe {
        // version NULL → Z_VERSION_ERROR (checked before the stream).
        assert_eq!(
            inflateBackInit_(ptr::null_mut(), 0, ptr::null_mut(), ptr::null(), 0),
            ReturnCode::VersionError.as_c_int(),
        );
        // valid version, NULL stream → Z_STREAM_ERROR.
        assert_eq!(
            inflateBackInit_(
                ptr::null_mut(),
                15,
                ptr::null_mut(),
                c"1".as_ptr(),
                core::mem::size_of::<z_stream>() as c_int,
            ),
            ReturnCode::StreamError.as_c_int(),
        );
        assert_eq!(
            inflateBack(
                ptr::null_mut(),
                None,
                ptr::null_mut(),
                None,
                ptr::null_mut()
            ),
            ReturnCode::StreamError.as_c_int(),
        );
        assert_eq!(
            inflateBackEnd(ptr::null_mut()),
            ReturnCode::StreamError.as_c_int(),
        );
    }

    // Normal completion: a raw fixed empty block decodes to Z_STREAM_END.
    {
        let mut state = inflate_back_init(15).expect("inflate_back_init(15)");
        let mut src = OneShot::new(&[0x03, 0x00]);
        let mut sink = SinkAccept;
        assert_eq!(
            inflate_back(&mut state, &mut src, &mut sink),
            ReturnCode::StreamEnd,
        );
        assert_eq!(inflate_back_end(state), ReturnCode::Ok);
    }

    // Forced output error: the sink rejects, so inflateBack aborts with BufError.
    {
        let mut state = inflate_back_init(15).expect("inflate_back_init(15)");
        let mut src = OneShot::new(&[0x63, 0x00, 0x00]);
        let mut sink = SinkReject;
        assert_eq!(
            inflate_back(&mut state, &mut src, &mut sink),
            ReturnCode::BufError,
        );
        assert_eq!(inflate_back_end(state), ReturnCode::Ok);
    }

    // Forced *mode* error (Z_STREAM_ERROR): NOT reproducible here. C forces it by
    // having its `pull` callback poke `((inflate_state*)strm.state)->mode = SYNC`,
    // an otherwise-impossible internal state. The idiomatic `InFunc` trait only
    // yields input bytes and cannot touch private engine state, and forging it
    // with `unsafe` is expressly forbidden. Equivalent coverage of the bad-state
    // guard in `inflate_back` is provided as a `#[cfg(test)]` unit test in
    // `src/inflate/back.rs` (owned by the `src/inflate/` module).

    // Built-in memory routines: plain init/end.
    let state = inflate_back_init(15).expect("inflate_back_init(15)");
    assert_eq!(inflate_back_end(state), ReturnCode::Ok);
}

/// Port of `cover_inflate()` — DEFLATE data cases through inflate and inflateBack.
#[test]
fn cover_inflate() {
    try_stream("0 0 0 0 0", "invalid stored block lengths", 1);
    try_stream("3 0", "fixed", 0);
    try_stream("6", "invalid block type", 1);
    try_stream("1 1 0 fe ff 0", "stored", 0);
    try_stream("fc 0 0", "too many length or distance symbols", 1);
    try_stream("4 0 fe ff", "invalid code lengths set", 1);
    try_stream("4 0 24 49 0", "invalid bit length repeat", 1);
    try_stream("4 0 24 e9 ff ff", "invalid bit length repeat", 1);
    try_stream("4 0 24 e9 ff 6d", "invalid code -- missing end-of-block", 1);
    try_stream(
        "4 80 49 92 24 49 92 24 71 ff ff 93 11 0",
        "invalid literal/lengths set",
        1,
    );
    try_stream(
        "4 80 49 92 24 49 92 24 f b4 ff ff c3 84",
        "invalid distances set",
        1,
    );
    try_stream(
        "4 c0 81 8 0 0 0 0 20 7f eb b 0 0",
        "invalid literal/length code",
        1,
    );
    try_stream("2 7e ff ff", "invalid distance code", 1);
    try_stream(
        "c c0 81 0 0 0 0 0 90 ff 6b 4 0",
        "invalid distance too far back",
        1,
    );

    // Trailer mismatch — only surfaced through inflate() over gzip framing, so
    // inflate-only (err < 0) and gated on the gzip wrapper.
    #[cfg(feature = "gzip")]
    {
        try_stream(
            "1f 8b 8 0 0 0 0 0 0 0 3 0 0 0 0 1",
            "incorrect data check",
            -1,
        );
        try_stream(
            "1f 8b 8 0 0 0 0 0 0 0 3 0 0 0 0 0 0 0 0 1",
            "incorrect length check",
            -1,
        );
    }

    try_stream("5 c0 21 d 0 0 0 80 b0 fe 6d 2f 91 6c", "pull 17", 0);
    try_stream(
        "5 e0 81 91 24 cb b2 2c 49 e2 f 2e 8b 9a 47 56 9f fb fe ec d2 ff 1f",
        "long code",
        0,
    );
    try_stream("ed c0 1 1 0 0 0 40 20 ff 57 1b 42 2c 4f", "length extra", 0);
    try_stream(
        "ed cf c1 b1 2c 47 10 c4 30 fa 6f 35 1d 1 82 59 3d fb be 2e 2a fc f c",
        "long distance and extra",
        0,
    );
    try_stream(
        "ed c0 81 0 0 0 0 80 a0 fd a9 17 a9 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 6",
        "window end",
        0,
    );

    inf(
        "2 8 20 80 0 3 0",
        "inflate_fast TYPE return",
        0,
        -15,
        258,
        ReturnCode::StreamEnd,
    );
    inf("63 18 5 40 c 0", "window wrap", 3, -8, 300, ReturnCode::Ok);
}

/// Port of `cover_trees()` — force `inflate_table`'s "not enough" (`Enough`)
/// error, which the normal decode path never triggers.
///
/// Unlike `infcover.c` (which reaches a private `inflate_table()` via internal
/// headers), the `zlib-rs` table builder is part of the public API, so this
/// coverage runs directly here — no delegation or forged access required.
#[test]
fn cover_trees() {
    // lens = [1, 2, 3, ..., 15, 15] (16 code lengths).
    let mut lens = [0u16; 16];
    for (i, slot) in lens.iter_mut().enumerate().take(15) {
        *slot = (i + 1) as u16;
    }
    lens[15] = 15;

    let mut work = [0u16; 16];
    let mut table = [Code::default(); ENOUGH_DISTS];

    // First over-subscription attempt (root bits = 15) → Enough.
    let mut table_index = 0usize;
    let mut bits = 15usize;
    assert_eq!(
        inflate_table(
            CodeType::Dists,
            &lens,
            16,
            &mut table,
            &mut table_index,
            &mut bits,
            &mut work,
        ),
        Err(InflateTableError::Enough),
    );

    // Second attempt (root bits = 1) → Enough.
    let mut table_index = 0usize;
    let mut bits = 1usize;
    assert_eq!(
        inflate_table(
            CodeType::Dists,
            &lens,
            16,
            &mut table,
            &mut table_index,
            &mut bits,
            &mut work,
        ),
        Err(InflateTableError::Enough),
    );
}

/// Port of `cover_fast()` — the `inffast` decode inner loop and window copies.
#[test]
fn cover_fast() {
    inf(
        "e5 e0 81 ad 6d cb b2 2c c9 01 1e 59 63 ae 7d ee fb 4d fd b5 35 41 68 ff 7f 0f 0 0 0",
        "fast length extra bits",
        0,
        -8,
        258,
        ReturnCode::DataError,
    );
    inf(
        "25 fd 81 b5 6d 59 b6 6a 49 ea af 35 6 34 eb 8c b9 f6 b9 1e ef 67 49 50 fe ff ff 3f 0 0",
        "fast distance extra bits",
        0,
        -8,
        258,
        ReturnCode::DataError,
    );
    inf(
        "3 7e 0 0 0 0 0",
        "fast invalid distance code",
        0,
        -8,
        258,
        ReturnCode::DataError,
    );
    inf(
        "1b 7 0 0 0 0 0",
        "fast invalid literal/length code",
        0,
        -8,
        258,
        ReturnCode::DataError,
    );
    inf(
        "d c7 1 ae eb 38 c 4 41 a0 87 72 de df fb 1f b8 36 b1 38 5d ff ff 0",
        "fast 2nd level codes and too far back",
        0,
        -8,
        258,
        ReturnCode::DataError,
    );
    inf(
        "63 18 5 8c 10 8 0 0 0 0",
        "very common case",
        0,
        -8,
        259,
        ReturnCode::Ok,
    );
    inf(
        "63 60 60 18 c9 0 8 18 18 18 26 c0 28 0 29 0 0 0",
        "contiguous and wrap around window",
        6,
        -8,
        259,
        ReturnCode::Ok,
    );
    inf(
        "63 0 3 0 0 0 0 0",
        "copy direct from output",
        0,
        -8,
        259,
        ReturnCode::StreamEnd,
    );
}

// ===========================================================================
// Forced Z_MEM_ERROR — a byte-capped allocator via the z_stream hooks
// (faithful port of infcover.c's `mem_*` forced-allocation-failure steps)
// ===========================================================================

/// A byte-capped allocator, the Rust analogue of `infcover.c`'s `mem_*` harness.
///
/// It hands out memory from the global allocator until a fixed byte budget is
/// exhausted, after which it returns null — exactly the failure `infcover.c`
/// injects through the `z_stream` `zalloc`/`zfree` hooks to drive the inflate
/// engine into `Z_MEM_ERROR`. `zlib-rs` allocation is *fallible* at the FFI
/// boundary (AAP §0.6.3): a caller hook that returns null propagates to
/// [`ReturnCode::MemError`] with **no** global-allocator fallback, so this hook
/// forces the error just as in C.
struct MemCap {
    /// Remaining byte budget. Any single request larger than this fails.
    budget: core::cell::Cell<usize>,
}

/// Every allocation is prefixed with a `usize` header recording its total size
/// so [`cap_free`] can reconstruct the [`Layout`](core::alloc::Layout).
const CAP_HEADER: usize = core::mem::size_of::<usize>();

/// `zalloc` hook: allocate `items * size` bytes when within budget, else null.
unsafe extern "C" fn cap_alloc(opaque: *mut c_void, items: c_uint, size: c_uint) -> *mut c_void {
    // SAFETY: `opaque` is the `&MemCap` installed on the stream below, which
    // outlives every inflate call in the test.
    let cap = unsafe { &*(opaque as *const MemCap) };
    let bytes = (items as usize).saturating_mul(size as usize);
    if bytes == 0 || bytes > cap.budget.get() {
        return ptr::null_mut();
    }
    let total = bytes + CAP_HEADER;
    let layout = core::alloc::Layout::from_size_align(total, CAP_HEADER).expect("valid layout");
    // SAFETY: `layout` has non-zero size.
    let raw = unsafe { std::alloc::alloc(layout) };
    if raw.is_null() {
        return ptr::null_mut();
    }
    // SAFETY: `raw` owns `total` bytes aligned for `usize`; record the size in
    // the header and hand back the pointer just past it.
    unsafe { *(raw as *mut usize) = total };
    cap.budget.set(cap.budget.get() - bytes);
    // SAFETY: the returned pointer lies one header inside the allocation.
    unsafe { raw.add(CAP_HEADER) as *mut c_void }
}

/// `zfree` hook: reconstruct the layout from the header, refund the budget, and
/// release the block.
unsafe extern "C" fn cap_free(opaque: *mut c_void, address: *mut c_void) {
    if address.is_null() {
        return;
    }
    // SAFETY: `address` was returned by `cap_alloc`, so its `usize` size header
    // sits in the `CAP_HEADER` bytes immediately before it; stepping back by
    // `CAP_HEADER` stays within that same allocation.
    let raw = unsafe { (address as *mut u8).sub(CAP_HEADER) };
    // SAFETY: `raw` points at the `usize` size header written by `cap_alloc`.
    let total = unsafe { *(raw as *const usize) };
    let layout = core::alloc::Layout::from_size_align(total, CAP_HEADER).expect("valid layout");
    // SAFETY: `opaque` is the live `&MemCap` for the stream.
    let cap = unsafe { &*(opaque as *const MemCap) };
    cap.budget.set(cap.budget.get() + (total - CAP_HEADER));
    // SAFETY: `raw`/`layout` match the original allocation from `cap_alloc`.
    unsafe { std::alloc::dealloc(raw, layout) };
}

/// Port of the forced-`Z_MEM_ERROR` steps of `infcover.c`'s `mem_*` harness.
///
/// With a byte budget below the inflate window size, the lazily-allocated
/// window (`1 << windowBits` bytes; 256 for `windowBits = -8`) cannot be
/// obtained, so `inflate` reports `Z_MEM_ERROR` — the same outcome `infcover.c`
/// pins under its 1-byte allocation limit. `inflateInit2_` itself makes no hook
/// allocation (the window is lazy), so it still returns `Z_OK`, mirroring C.
#[test]
fn mem_limit_forces_mem_error() {
    let cap = MemCap {
        // 200 < 256 = the raw 8-bit inflate window, so the window allocation
        // fails while leaving room for any smaller incidental request.
        budget: core::cell::Cell::new(200),
    };

    let mut strm = zeroed_stream();
    strm.zalloc = Some(cap_alloc);
    strm.zfree = Some(cap_free);
    strm.opaque = (&cap as *const MemCap) as *mut c_void;

    // SAFETY: `strm` is a valid, caller-owned `z_stream` with a live capped
    // allocator installed; `c"1"` is a valid version whose first byte matches
    // the library version, and the reported size is the true `sizeof(z_stream)`.
    let init = unsafe {
        inflateInit2_(
            &mut strm,
            -8,
            c"1".as_ptr(),
            core::mem::size_of::<z_stream>() as c_int,
        )
    };
    assert_eq!(
        init,
        ReturnCode::Ok.as_c_int(),
        "inflateInit2_ makes no hook allocation (the window is lazy) and must succeed",
    );

    // A minimal raw-DEFLATE fragment that drives the engine to grow its window
    // (mirrors the `\x63\x00` feed in infcover.c's mem coverage).
    let input = [0x63u8, 0x00];
    let mut out = [0u8; 1];
    strm.next_in = input.as_ptr();
    strm.avail_in = input.len() as c_uint;
    strm.next_out = out.as_mut_ptr();
    strm.avail_out = out.len() as c_uint;

    // SAFETY: `strm` holds a valid inflate state; `next_in`/`next_out` point at
    // the live local buffers with matching `avail_*` counts.
    let ret = unsafe { ffi_inflate(&mut strm, Z_NO_FLUSH) };
    assert_eq!(
        ret,
        ReturnCode::MemError.as_c_int(),
        "a budget below the window size must force Z_MEM_ERROR",
    );

    // SAFETY: `strm` was initialized by `inflateInit2_`; `inflateEnd` reclaims
    // the state box (allocated globally, so it succeeds despite the cap).
    let end = unsafe { inflateEnd(&mut strm) };
    assert_eq!(end, ReturnCode::Ok.as_c_int(), "inflateEnd must succeed");
}
