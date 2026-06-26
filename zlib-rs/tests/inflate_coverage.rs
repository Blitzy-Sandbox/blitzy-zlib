//! Inflate **coverage** suite — a safe-Rust port of zlib's `test/infcover.c`.
//!
//! `infcover.c` is the zlib decompressor's full-coverage test: it drives
//! malformed and edge-case DEFLATE bitstreams, header-validation paths, the
//! preset-dictionary handshake, and the `inflateBack` callback entry point. It
//! is the most intricate test in the suite because the original deliberately
//! reaches into **private** `struct inflate_state` fields and calls **internal**
//! functions (`inflate_table`, `inflateUndermine`, direct `state->mode`
//! mutation) to manufacture conditions that the public API alone cannot.
//!
//! This crate is built under `#![forbid(unsafe_code)]`. The safe core exposes
//! no way to poke private `struct inflate_state` fields from an external
//! integration test, but it **does** expose a fallible custom [`Allocator`]
//! hook, so allocator-failure (`Z_MEM_ERROR`) *is* reproducible here by
//! installing a memory-limiting allocator that returns `None`. The port
//! therefore follows a strict **reframing policy**:
//!
//! * **PORT** every behavior reachable through the public, safe API — malformed
//!   bodies → [`ZlibError::DataError`] with the exact `msg` string, zlib/gzip
//!   header validation, the `Z_NEED_DICT` → `set_dictionary` handshake,
//!   `inflate_sync` recovery, `copy`, `reset2`, header capture via
//!   `get_header`, the `inflate_back` family, and — via the fallible
//!   [`Allocator`] hook — the `mem_zone`/`mem_limit` allocation-failure paths
//!   that surface `Z_MEM_ERROR` (see the "Allocation-failure coverage"
//!   section).
//! * **OMIT** — with an explicit `// PORT-NOTE:` — every behavior that is
//!   structurally a C/FFI-layer or private-field concern and is therefore
//!   *unrepresentable* in the safe core: null-pointer `Z_STREAM_ERROR`
//!   defenses, wrong-version `Z_VERSION_ERROR` negotiation, direct
//!   internal-table (`inflate_table`) calls, and private `state->mode`
//!   mutation. The one residual allocation case left unmodeled — a failed
//!   `inflateCopy` returning `Z_MEM_ERROR` — is noted at its site, because the
//!   owned `copy()` clones through the global allocator (which aborts on true
//!   OOM) by deliberate safe-ownership design. Each note records where the
//!   behavior *is* validated (the `libz-rs-sys` shim, or a `src/inflate/*` unit
//!   test) so nothing is silently dropped.
//!
//! # Why the API looks different from `infcover.c`
//!
//! The idiomatic Rust decompressor is **method-based** on an owned
//! [`InflateState`] (the C `z_stream`-threaded `inflateXxx(strm, …)` family is
//! re-exposed only by the `libz-rs-sys` FFI shim). The mapping used here is:
//!
//! | C (`infcover.c`)        | Rust (this port)                              |
//! |-------------------------|-----------------------------------------------|
//! | `inflateInit2(&s, win)` | [`InflateState::new(win)`]                    |
//! | `inflate(&s, flush)`    | `state.inflate(input, output, flush)`         |
//! | `inflateEnd(&s)`        | `drop(state)` (RAII)                           |
//! | `inflateReset2(&s, w)`  | `state.reset2(w)`                             |
//! | `inflatePrime(&s, …)`   | `state.prime(bits, value)`                    |
//! | `inflateSetDictionary`  | `state.set_dictionary(dict)`                  |
//! | `inflateCopy(&d, &s)`   | `state.copy()` → owned `Box<InflateState>`    |
//! | `inflateSync(&s)`       | `state.sync(input)`                           |
//! | `inflateGetHeader`      | `state.get_header(head)` (`gzip` feature)     |
//! | `strm.msg`              | `state.msg` (`Option<&'static str>`)          |
//! | `strm.adler`            | `state.check`                                 |
//! | `inflateBackInit(...)`  | [`inflate_back_init(win)`]                     |
//! | `inflateBack(...)`      | [`inflate_back`] with pull/push closures      |
//! | `inflateBackEnd(...)`   | [`inflate_back_end`]                           |
//!
//! The hand-crafted hex fixtures and the exact expected `msg` strings are taken
//! **verbatim** from `test/infcover.c`; they are the wire-format / diagnostic
//! contract and must not be regenerated or loosened.
//!
//! # Features
//!
//! gzip-container cases (`windowBits` 31/47 and `get_header`) are gated behind
//! the `gzip` Cargo feature; the suite still compiles and passes the raw/zlib
//! cases with `--no-default-features`. This file uses only the public
//! `zlib_rs` API (no private/internal paths) and the standard libtest harness.

#![forbid(unsafe_code)]

use core::cell::Cell;

use zlib_rs::constants::Flush;
use zlib_rs::error::{ReturnCode, ZlibError};
use zlib_rs::inflate::{InflateState, inflate_back, inflate_back_end, inflate_back_init};
use zlib_rs::stream::Allocator;

// The custom-allocator coverage cases install a memory-limiting [`Allocator`]
// shared with the owned [`InflateState`]; `Rc` is the integration-test analogue
// of the `mem_zone` cookie `infcover.c` threads through `zalloc`/`zfree`. The
// libtest harness always links `std`, so `std::rc::Rc` is available even when
// the `zlib-rs` crate itself is built `--no-default-features`.
use std::rc::Rc;

// `GzHeader` and the `get_header` capture API only exist with the `gzip`
// feature; importing it unconditionally would be an unused import without it.
#[cfg(feature = "gzip")]
use zlib_rs::gz_header::GzHeader;

// ---------------------------------------------------------------------------
// C status-code aliases (from `zlib.h`)
// ---------------------------------------------------------------------------
//
// The ported `inf()` driver takes the same integer `err` argument the C driver
// does, so these mirror the `zlib.h` macros for a near-verbatim transcription.
// Only the codes actually referenced by the fixtures are defined (an unused
// constant would trip `-D warnings`).

/// `Z_OK` — success.
const Z_OK: i32 = 0;
/// `Z_STREAM_END` — end of the compressed stream reached.
const Z_STREAM_END: i32 = 1;
/// `Z_NEED_DICT` — a preset dictionary is required to continue.
const Z_NEED_DICT: i32 = 2;
/// `Z_STREAM_ERROR` — inconsistent stream state / invalid parameter.
const Z_STREAM_ERROR: i32 = -2;
/// `Z_DATA_ERROR` — the input data was corrupted or invalid.
const Z_DATA_ERROR: i32 = -3;
/// The C driver's "don't care" sentinel (`err == 9`): after the first
/// `inflate()` call the expected code is no longer asserted.
const DONT_CARE: i32 = 9;

// ===========================================================================
// Fixture decoder and status helpers
// ===========================================================================

/// Decode a hex fixture string into bytes — a faithful port of `infcover.c`'s
/// `h2b()`.
///
/// The decoder accepts pairs of hex digits separated by any non-hex delimiter
/// (conventionally a single space). A **single** hex digit immediately followed
/// by a delimiter is promoted to a full byte (this is how the C fixtures encode
/// values such as `"6"` → `0x06`). The C implementation walks the terminating
/// NUL of the C string so that a trailing single digit is flushed; this port
/// reproduces that by running the accumulator logic one final time once the
/// input is exhausted.
///
/// The accumulator starts at `1` (a sentinel distinguishing "no digits yet"
/// from a real `0`): each hex digit shifts in via `val = (val << 4) + digit`,
/// each delimiter seen while `1 != val < 32` adds `240` to promote a lone
/// digit, and whenever `val > 255` the low byte is emitted and the accumulator
/// resets to `1`.
fn h2b(hex: &str) -> Vec<u8> {
    // Upper bound mirrors C's `(strlen(hex) + 1) >> 1` allocation.
    let mut out = Vec::with_capacity((hex.len() + 1) >> 1);
    let mut bytes = hex.bytes();
    let mut val: u32 = 1;

    loop {
        // `None` represents the C string's terminating NUL: the body still runs
        // once for it (post-tested `do { … } while (*hex++)`), which flushes a
        // pending single digit, after which we stop.
        let next = bytes.next();
        let ch = next.unwrap_or(0);

        if ch.is_ascii_digit() {
            val = (val << 4) + u32::from(ch - b'0');
        } else if (b'A'..=b'F').contains(&ch) {
            val = (val << 4) + u32::from(ch - b'A' + 10);
        } else if (b'a'..=b'f').contains(&ch) {
            val = (val << 4) + u32::from(ch - b'a' + 10);
        } else if val != 1 && val < 32 {
            // Non-hex delimiter following a single digit: promote to a byte.
            val += 240;
        }

        if val > 255 {
            out.push((val & 0xff) as u8);
            val = 1;
        }

        if next.is_none() {
            break;
        }
    }

    out
}

/// Collapse a core `Result<ReturnCode, ZlibError>` into the C integer status so
/// it can be compared against the `err` argument threaded through the drivers.
fn status_i32(status: Result<ReturnCode, ZlibError>) -> i32 {
    match status {
        Ok(code) => code.as_i32(),
        Err(err) => err.as_i32(),
    }
}

// ===========================================================================
// `inf()` — the generic single-stream driver (port of `infcover.c::inf`)
// ===========================================================================

/// Port of `infcover.c`'s `inf()` driver.
///
/// Initializes an [`InflateState`] for `win`, optionally registers a gzip
/// header capture (when `win == 47` and the `gzip` feature is on), then feeds
/// `h2b(hex)` into an output buffer of size `len` in `step`-sized chunks
/// (`step == 0` means "all at once"). The first `inflate()` return code is
/// asserted against `err` (unless `err == DONT_CARE`); a `Z_NEED_DICT` result
/// drives the preset-dictionary handshake. After the loop the state is copied
/// (exercising `inflate_copy` + RAII `inflate_end`) and `reset2(-8)` is
/// asserted, mirroring the C tail.
///
/// # Reframing
///
/// The C `inf()` additionally exercises `Z_MEM_ERROR` by wrapping the stream in
/// a limiting `mem_zone` allocator and then pokes `state->mode = DICT` to
/// recover. The allocation-failure half is now **PORTED** through the fallible
/// [`Allocator`] hook in the dedicated "Allocation-failure coverage" section
/// (`mem_window_allocation_failure_is_mem_error` and
/// `mem_set_dictionary_allocation_failure_is_mem_error`); it is kept out of this
/// shared `inf()` driver only because `inf()` deliberately runs on the default
/// allocator so the success fixtures stay representative. The private
/// `state->mode = DICT` poke remains OMITTED (it mutates a private field the
/// safe API does not expose).
fn inf(hex: &str, what: &str, step: usize, win: i32, len: usize, mut err: i32) {
    let input = h2b(hex);
    let total = input.len();

    // C `inflateInit2`. A bad `windowBits` makes this fail; the C driver simply
    // returns on init failure, but we additionally assert the error matches the
    // caller's expectation so the "bad window size" fixture is meaningful.
    let mut state = match InflateState::new(win) {
        Ok(state) => state,
        Err(init_err) => {
            if err != DONT_CARE {
                assert_eq!(init_err.as_i32(), err, "{what}: unexpected init error");
            }
            return;
        }
    };

    // C: when `win == 47`, register a header capture so the gzip header fields
    // are exercised. Header capture is a `gzip`-feature API; the `win == 47`
    // fixtures are themselves gzip-gated callers, so this block is unreachable
    // (and absent) without the feature.
    #[cfg(feature = "gzip")]
    if win == 47 {
        let head = GzHeader::new()
            .with_extra(Vec::new())
            .with_name(Vec::new())
            .with_comment(Vec::new());
        assert_eq!(
            state.get_header(head),
            Ok(ReturnCode::Ok),
            "{what}: get_header registration"
        );
    }

    let mut out = vec![0u8; len];
    // `step == 0` (or a step larger than the input) means "feed everything at
    // once", matching the C `if (step == 0 || step > have) step = have;`.
    let step = if step == 0 || step > total {
        total.max(1)
    } else {
        step
    };

    let mut pos = 0usize;
    let mut first = true;
    // Hang guard: input always advances or a terminal status breaks the loop;
    // this only fires if the engine spins making no progress.
    let mut guard = total.saturating_mul(64) + 1024;

    loop {
        let chunk_end = core::cmp::min(pos + step, total);
        let result = state.inflate(&input[pos..chunk_end], &mut out, Flush::NoFlush);
        pos += result.consumed;

        if first {
            assert!(
                err == DONT_CARE || status_i32(result.status) == err,
                "{what}: expected status {err}, got {:?}",
                result.status
            );
            first = false;
        }

        let status = result.status;

        // C continues only while the result is one of OK / BUF_ERROR /
        // NEED_DICT; anything else (STREAM_END, DATA_ERROR, …) ends the run.
        let keep_going = matches!(
            status,
            Ok(ReturnCode::Ok) | Ok(ReturnCode::NeedDict) | Err(ZlibError::BufError)
        );
        if !keep_going {
            break;
        }

        if status == Ok(ReturnCode::NeedDict) {
            // --- preset-dictionary handshake (port of inf()'s NEED_DICT arm) ---
            // A wrong dictionary id yields Z_DATA_ERROR ...
            assert_eq!(
                state.set_dictionary(&input[..1]),
                Err(ZlibError::DataError),
                "{what}: wrong dictionary should be DataError"
            );
            // PORT-NOTE: C then calls `mem_limit(&strm, 1)` and expects
            // `inflateSetDictionary(&strm, out, 0) == Z_MEM_ERROR`, then pokes
            // `state->mode = DICT` to recover. The `set_dictionary` →
            // `Z_MEM_ERROR` path is now PORTED by
            // `mem_set_dictionary_allocation_failure_is_mem_error` (a denying
            // `Allocator` makes the window allocation fail); it is exercised
            // there rather than inline because this shared `inf()` driver runs
            // on the default allocator. The private `state->mode = DICT` poke
            // remains OMITTED (private-field mutation). The empty dictionary has
            // id `adler32(1, &[]) == 1`, which matches the "need dictionary"
            // fixture's stored id, so this succeeds.
            assert_eq!(
                state.set_dictionary(&[]),
                Ok(ReturnCode::Ok),
                "{what}: empty dictionary should be Ok"
            );
            // Resuming with no further input or output space is a buffer error.
            assert_eq!(
                state.inflate(&[], &mut out, Flush::NoFlush).status,
                Err(ZlibError::BufError),
                "{what}: resume after dict should be BufError"
            );
        }

        // C `inflateCopy(&copy, &strm)` then `inflateEnd(&copy)`: the copy is an
        // independent owned state and dropping it is the RAII `inflateEnd`.
        drop(state.copy());

        // After the first call the expected code is "don't care".
        err = DONT_CARE;

        // C loop condition `while (strm.avail_in)`: stop once input is drained.
        if pos >= total {
            break;
        }

        guard -= 1;
        assert!(guard > 0, "{what}: inf() exceeded iteration guard");
    }

    // C tail: `inflateReset2(&strm, -8)` must succeed, then `inflateEnd`.
    assert_eq!(state.reset2(-8), Ok(ReturnCode::Ok), "{what}: reset2(-8)");
    // Dropping `state` is the RAII `inflateEnd`.
}

// ===========================================================================
// `try_stream()` — dual inflate / inflateBack driver (port of `infcover.c::try`)
// ===========================================================================

/// Port of `infcover.c`'s `try()` driver (renamed because `try` is a reserved
/// word in Rust).
///
/// Runs the **same** raw bitstream through both decoder entry points:
///
/// 1. **`inflate`** — `InflateState::new(win)` with `win = 47` (auto-detect
///    wrapper) when `err < 0`, else `-15` (raw); feed the whole fixture with
///    `Flush::Trees`. When `err != 0`, assert the terminal status is
///    `Z_DATA_ERROR` and `state.msg == Some(id)`.
/// 2. **`inflate_back`** — only when `err >= 0` (the C driver skips the back
///    path for the wrapper-only `err < 0` cases). Driven by a pull closure that
///    yields the whole input once then empty, and a push closure that appends
///    to a `Vec`. When `err != 0`, assert `Z_DATA_ERROR` and `msg == Some(id)`.
///
/// `err` is tri-state: `0` = success expected, `1` = data error expected,
/// `-1` = data error expected but exercised through the wrapper window on the
/// `inflate` path only.
fn try_stream(hex: &str, id: &str, err: i32) {
    let input = h2b(hex);
    let total = input.len();

    // ---------------------------- inflate path ----------------------------
    // `err < 0` drives the auto-detecting wrapper window (47); such callers are
    // gzip-gated, so `new(47)` only runs with the gzip feature (where it works).
    let win = if err < 0 { 47 } else { -15 };
    let mut state = InflateState::new(win).expect("inflate init should succeed");
    // Generous output buffer (C uses `1 << (MAX_WBITS)`-ish slack via the loop);
    // a multiple of the input is ample for these tiny fixtures.
    let mut out = vec![0u8; (total << 3).max(1)];
    let mut pos = 0usize;
    // Deferred init: the loop runs at least once and assigns `status` before
    // any `break`, so it is always initialized when read after the loop.
    let mut status;
    let mut guard = total.saturating_mul(64) + 1024;

    loop {
        let result = state.inflate(&input[pos..], &mut out, Flush::Trees);
        pos += result.consumed;
        status = result.status;

        // A stream/mem error here would be a port defect, not a fixture trait.
        assert!(
            !matches!(
                status,
                Err(ZlibError::StreamError) | Err(ZlibError::MemError)
            ),
            "{id}: inflate returned unexpected {status:?}"
        );

        // C breaks the loop on Z_DATA_ERROR or Z_NEED_DICT.
        if matches!(status, Err(ZlibError::DataError) | Ok(ReturnCode::NeedDict)) {
            break;
        }

        // C `while (strm.avail_in || strm.avail_out == 0)`: keep going while
        // input remains or the output filled completely (more may be pending).
        let avail_in = pos < total;
        let avail_out_zero = result.produced == out.len();
        if !(avail_in || avail_out_zero) {
            break;
        }

        guard -= 1;
        assert!(guard > 0, "{id}: try_stream inflate exceeded guard");
    }

    if err != 0 {
        assert_eq!(status, Err(ZlibError::DataError), "{id}: inflate status");
        assert_eq!(state.msg, Some(id), "{id}: inflate msg parity");
    }
    drop(state);

    // -------------------------- inflateBack path ---------------------------
    // The C driver only runs the back path for `err >= 0` (the `err < 0` cases
    // exercise a wrapper the raw `inflateBack` engine does not parse).
    if err < 0 {
        return;
    }

    let mut back = inflate_back_init(15).expect("inflate_back_init should succeed");
    let mut fed = false;
    // Pull closure: hand back the whole fixture once, then signal end-of-input.
    let read = || -> &[u8] {
        if fed {
            &input[total..] // empty tail
        } else {
            fed = true;
            &input[..]
        }
    };
    // Push closure: accumulate output; `false` == success (C `out` returns 0).
    let mut acc: Vec<u8> = Vec::new();
    let write = |buf: &[u8]| -> bool {
        acc.extend_from_slice(buf);
        false
    };

    let result = inflate_back(&mut back, read, write);
    // Copy the status out before touching `back.msg` (keeps the `'a` borrow of
    // `input` carried by `result.unused_input` independent of `back`).
    let back_status = result.status;

    assert!(
        back_status != Err(ZlibError::StreamError),
        "{id}: inflate_back returned StreamError"
    );
    if err != 0 {
        assert_eq!(
            back_status,
            Err(ZlibError::DataError),
            "{id}: inflate_back status"
        );
        assert_eq!(back.msg, Some(id), "{id}: inflate_back msg parity");
    }

    let _ = inflate_back_end(back);
}

// ===========================================================================
// Helper unit tests
// ===========================================================================

#[test]
fn h2b_decodes_pairs_and_single_digits() {
    // Space-separated single digits each become a byte.
    assert_eq!(h2b("1 2 3"), [1, 2, 3]);
    // A bare two-digit pair with no trailing delimiter.
    assert_eq!(h2b("ff"), [0xff]);
    // Mixed case, paired.
    assert_eq!(h2b("De aD bE eF"), [0xde, 0xad, 0xbe, 0xef]);
    // Trailing single digit is flushed by the terminating-NUL pass.
    assert_eq!(h2b("63 0"), [0x63, 0x00]);
    // All-zero single digits.
    assert_eq!(h2b("0 0 0 0 0"), [0, 0, 0, 0, 0]);
    // A lone "6" promotes to 0x06.
    assert_eq!(h2b("6"), [0x06]);
    // Empty input decodes to nothing.
    assert_eq!(h2b(""), []);
}

// ===========================================================================
// cover_support  (port of `infcover.c::cover_support`)
// ===========================================================================

/// Port of `cover_support`'s first block: `inflateInit` + `inflatePrime`
/// (positive and negative bit counts) + `inflateSetDictionary` on a freshly
/// inited zlib stream (forbidden state → `Z_STREAM_ERROR`) + `inflateEnd`.
#[test]
fn support_prime_and_dict_lifecycle() {
    let mut state = InflateState::new_default().expect("inflateInit should succeed");

    // inflatePrime(5, 31): buffer 5 bits.
    assert_eq!(state.prime(5, 31), Ok(ReturnCode::Ok), "prime(5, 31)");
    // inflatePrime(-1, 0): a negative bit count drains the buffer.
    assert_eq!(state.prime(-1, 0), Ok(ReturnCode::Ok), "prime(-1, 0)");

    // inflateSetDictionary on a wrapped stream that is NOT at DICT is a
    // stream error (the call is wrong *for the current state*, not because of a
    // null pointer — so this is reachable and PORTED, not a BUCKET-2 omission).
    assert_eq!(
        state.set_dictionary(&[]),
        Err(ZlibError::StreamError),
        "set_dictionary on fresh zlib stream"
    );

    // drop(state) is the RAII `inflateEnd`.
}

/// Port of `cover_support`'s "built-in memory routines" tail: a bare
/// `inflateInit` + `inflateEnd` lifecycle must succeed. With ownership the
/// allocation cannot fail, so the only observable is that init/teardown work.
#[test]
fn support_init_end_lifecycle() {
    let state = InflateState::new_default().expect("inflateInit should succeed");
    drop(state); // RAII inflateEnd
}

// PORT-NOTE: `cover_support`'s wrong-version probe
// `inflateInit_(&strm, "!", sizeof(z_stream)) == Z_VERSION_ERROR` is OMITTED.
// Version negotiation lives only at the `_`-suffixed C entry points in the
// `libz-rs-sys` shim; the safe-core `InflateState::new` takes no version string
// and cannot emit `Z_VERSION_ERROR`. It is validated in `libz-rs-sys`.

// The remaining `cover_support` cases are the `inf()`-driven window fixtures.

#[test]
fn support_force_window_allocation() {
    inf("63 0", "force window allocation", 0, -15, 1, Z_OK);
}

#[test]
fn support_force_window_replacement() {
    inf("63 18 5", "force window replacement", 0, -8, 259, Z_OK);
}

#[test]
fn support_force_split_window_update() {
    inf(
        "63 18 68 30 d0 0 0",
        "force split window update",
        4,
        -8,
        259,
        Z_OK,
    );
}

#[test]
fn support_use_fixed_blocks() {
    inf("3 0", "use fixed blocks", 0, -15, 1, Z_STREAM_END);
}

#[test]
fn support_bad_window_size() {
    // windowBits == 1 is invalid; `InflateState::new` fails with Z_STREAM_ERROR,
    // which `inf()` asserts against the expected init error.
    inf("", "bad window size", 0, 1, 0, Z_STREAM_ERROR);
}

// ===========================================================================
// cover_wrap  (port of `infcover.c::cover_wrap`)
// ===========================================================================

// PORT-NOTE: `cover_wrap` opens with the null-pointer defenses
// `inflate(Z_NULL, 0) == Z_STREAM_ERROR`, `inflateEnd(Z_NULL) == Z_STREAM_ERROR`,
// and `inflateCopy(Z_NULL, Z_NULL) == Z_STREAM_ERROR`. These are OMITTED: in the
// safe core a `&mut InflateState` / owned `Box<InflateState>` is provably
// non-null (type-state design, AAP §0.3.2), so a null-pointer `Z_STREAM_ERROR`
// is unrepresentable. The null checks are validated in the `libz-rs-sys` shim,
// which accepts raw `*mut z_stream` pointers.

// --- Header-validation fixtures (zlib / raw need no gzip feature) ---

#[test]
fn wrap_bad_zlib_method() {
    inf("77 85", "bad zlib method", 0, 15, 0, Z_DATA_ERROR);
}

#[test]
fn wrap_set_window_size_from_header() {
    // windowBits == 0 means "take the window size from the zlib header".
    inf("8 99", "set window size from header", 0, 0, 0, Z_OK);
}

#[test]
fn wrap_bad_zlib_window_size() {
    inf("78 9c", "bad zlib window size", 0, 8, 0, Z_DATA_ERROR);
}

#[test]
fn wrap_check_adler32() {
    // A complete zlib stream whose Adler-32 trailer validates -> Z_STREAM_END.
    inf(
        "78 9c 63 0 0 0 1 0 1",
        "check adler32",
        0,
        15,
        1,
        Z_STREAM_END,
    );
}

#[test]
fn wrap_compute_adler32() {
    // A zlib stream with no trailer: the first call produces a byte and reports
    // Z_OK (it has not yet reached the trailer).
    inf("78 9c 63 0", "compute adler32", 0, 15, 1, Z_OK);
}

// --- Header-validation fixtures that require the gzip wrapper (feature `gzip`) ---

#[cfg(feature = "gzip")]
#[test]
fn wrap_bad_gzip_method() {
    inf("1f 8b 0 0", "bad gzip method", 0, 31, 0, Z_DATA_ERROR);
}

#[cfg(feature = "gzip")]
#[test]
fn wrap_bad_gzip_flags() {
    inf("1f 8b 8 80", "bad gzip flags", 0, 31, 0, Z_DATA_ERROR);
}

#[cfg(feature = "gzip")]
#[test]
fn wrap_bad_header_crc() {
    inf(
        "1f 8b 8 1e 0 0 0 0 0 0 1 0 0 0 0 0 0",
        "bad header crc",
        0,
        47,
        1,
        Z_DATA_ERROR,
    );
}

#[cfg(feature = "gzip")]
#[test]
fn wrap_check_gzip_length() {
    inf(
        "1f 8b 8 2 0 0 0 0 0 0 1d 26 3 0 0 0 0 0 0 0 0 0",
        "check gzip length",
        0,
        47,
        0,
        Z_STREAM_END,
    );
}

#[cfg(feature = "gzip")]
#[test]
fn wrap_bad_zlib_header_check() {
    // win 47 is auto-detect, which is gzip-feature-gated.
    inf("78 90", "bad zlib header check", 0, 47, 0, Z_DATA_ERROR);
}

// --- Preset-dictionary handshake ---

#[test]
fn wrap_need_dictionary() {
    // A zlib stream with FDICT set drives Z_NEED_DICT; `inf()`'s NEED_DICT arm
    // then performs the wrong-dict (Z_DATA_ERROR) / empty-dict (Z_OK) handshake.
    inf("8 b8 0 0 0 1", "need dictionary", 0, 8, 0, Z_NEED_DICT);
}

/// Focused assertion that a stream with `FDICT` set reports `Z_NEED_DICT` and
/// exposes the expected dictionary id in `state.check`, then accepts the
/// matching (empty) dictionary — the public-API core of the C dictionary path.
#[test]
fn wrap_need_dictionary_reports_id() {
    let input = h2b("8 b8 0 0 0 1");
    let mut state = InflateState::new(8).expect("init zlib win 8");
    let mut out = [0u8; 1];
    let result = state.inflate(&input, &mut out, Flush::NoFlush);
    assert_eq!(result.status, Ok(ReturnCode::NeedDict), "should need dict");
    // The fixture's stored dictionary id is 1 (== adler32(1, &[])).
    assert_eq!(state.check, 1, "dictionary id surfaced in state.check");
    // A wrong dictionary is rejected ...
    assert_eq!(
        state.set_dictionary(&[0u8]),
        Err(ZlibError::DataError),
        "wrong dict id -> DataError"
    );
    // ... the matching empty dictionary is accepted.
    assert_eq!(
        state.set_dictionary(&[]),
        Ok(ReturnCode::Ok),
        "empty dict -> Ok"
    );
}

/// Reframed port of `cover_wrap`'s "miscellaneous, force memory errors" block:
/// the reachable behaviors (`set_dictionary` on a raw stream, `prime`,
/// `inflate_sync` recovery including the transient SYNC-mode `inflate`,
/// `sync_point`, `undermine`, `mark`, `codes_used`) are PORTED; the
/// allocation-failure and null-pointer steps are OMITTED with notes.
#[test]
fn wrap_miscellaneous_reachable_paths() {
    let mut state = InflateState::new(-8).expect("init raw -8");

    // PORT-NOTE: C primes the raw stream and calls `inflate()` twice under
    // `mem_limit(1)`, asserting `Z_MEM_ERROR` each time — the window allocation
    // is refused. That window-allocation → `Z_MEM_ERROR` path is now PORTED by
    // `mem_window_allocation_failure_is_mem_error` in the "Allocation-failure
    // coverage" section, which installs a denying `Allocator` and asserts
    // `inflate()` returns `ZlibError::MemError`. It lives in its own focused
    // test rather than inline here so this driver keeps exercising the success
    // paths on the default allocator.

    // On a raw stream (wrap == 0) a dictionary always loads -> Z_OK.
    let dict = [0u8; 257];
    assert_eq!(
        state.set_dictionary(&dict),
        Ok(ReturnCode::Ok),
        "raw set_dictionary"
    );

    // inflatePrime with a valid bit count.
    assert_eq!(state.prime(16, 0), Ok(ReturnCode::Ok), "prime(16, 0)");

    // inflateSync: a buffer with no full-flush marker -> Z_DATA_ERROR ...
    let (_, r) = state.sync(&[0x80, 0x00]);
    assert_eq!(r, Err(ZlibError::DataError), "sync without marker");

    // ... calling inflate() from the transient SYNC mode -> Z_STREAM_ERROR ...
    let mut scratch = [0u8; 16];
    assert_eq!(
        state.inflate(&[], &mut scratch, Flush::NoFlush).status,
        Err(ZlibError::StreamError),
        "inflate() from SYNC mode"
    );

    // ... then a buffer containing the 00 00 FF FF marker resyncs -> Z_OK.
    let (_, r) = state.sync(&[0x00, 0x00, 0xff, 0xff]);
    assert_eq!(r, Ok(ReturnCode::Ok), "sync with marker");

    // inflateSyncPoint is a callable observer.
    let _ = state.sync_point();

    // PORT-NOTE: C's `inflateCopy(&copy, &strm) == Z_MEM_ERROR` here is the one
    // residual allocation-failure case left unmodeled. Unlike the window
    // allocation (now covered via the fallible `Allocator`), `copy()` is a
    // `Clone` of the owned `Box<InflateState>`: it duplicates the window/codes
    // through the *global* allocator, which aborts on true OOM rather than
    // returning, by deliberate safe-ownership design. The *successful*
    // `inflate_copy` path is covered by the `inf()` driver; the failing one is
    // structurally non-representable without reintroducing fallible cloning.

    // inflateUndermine -> Z_DATA_ERROR (this build does not enable
    // INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR, exactly like stock zlib).
    assert_eq!(
        state.undermine(true),
        Err(ZlibError::DataError),
        "undermine -> DataError"
    );

    // inflateMark / inflateCodesUsed are callable observers.
    let _ = state.mark();
    let _ = state.codes_used();

    // drop(state) == inflateEnd
}

// ===========================================================================
// Allocation-failure coverage  (port of `infcover.c`'s `mem_zone` / `mem_limit`)
// ===========================================================================
//
// `infcover.c` installs a custom `zalloc`/`zfree` pair (`mem_alloc` /
// `mem_free`) backed by a `mem_zone` cookie. `mem_limit(&strm, limit)` caps the
// live byte total; once a request would overrun the cap, `mem_alloc` returns
// `NULL`, which zlib reports as `Z_MEM_ERROR`. The C `inf()` and `cover_wrap`
// drivers use this to force the sliding-window allocation (during `inflate()`
// and during `inflateSetDictionary`) to fail and assert `Z_MEM_ERROR`.
//
// The safe core exposes the same extension point through the fallible
// [`Allocator`] trait: returning `None` from an `allocate_*` method is the
// safe-Rust spelling of `mem_alloc` returning `NULL`, and the core maps it to
// [`ZlibError::MemError`] (`Z_MEM_ERROR`). The [`LimitedAllocator`] below is the
// `mem_zone` analogue, shared with the owned [`InflateState`] through an [`Rc`]
// (the analogue of the C `opaque` cookie). These tests therefore restore the
// `infcover` allocation-failure coverage that earlier notes recorded as
// omitted, directly against the safe core.

/// A safe-Rust analogue of `infcover.c`'s `mem_zone` limiting allocator.
///
/// Tracks the live byte total and an optional ceiling (`limit`). An allocation
/// request that would push the live total past `limit` is **refused** (returns
/// `None`), exactly like `mem_alloc`'s `if (zone->total + len > zone->limit)
/// return NULL;`. A `limit` of `0` denies every request — the analogue of
/// `mem_limit(&strm, 1)` in the C suite, where any real allocation overruns a
/// one-byte ceiling.
///
/// All counters use [`Cell`] for interior mutability because the [`Allocator`]
/// methods take `&self` (the allocator is shared through an [`Rc`]).
struct LimitedAllocator {
    /// Maximum cumulative *live* bytes the allocator will hand out.
    limit: usize,
    /// Running total of live bytes currently handed out.
    live: Cell<usize>,
    /// Number of allocation requests granted — lets a test assert the window
    /// allocation was genuinely routed through *this* allocator.
    served: Cell<usize>,
    /// Number of allocation requests refused for exceeding `limit`.
    refused: Cell<usize>,
}

impl LimitedAllocator {
    /// Build an allocator that permits up to `limit` cumulative live bytes.
    fn new(limit: usize) -> Self {
        Self {
            limit,
            live: Cell::new(0),
            served: Cell::new(0),
            refused: Cell::new(0),
        }
    }

    /// Account `bytes` against the ceiling. Returns `None` (recording a refusal)
    /// when granting the request would exceed `limit`, mirroring `mem_alloc`'s
    /// `total + len > limit` guard; otherwise records the grant and succeeds.
    fn account(&self, bytes: usize) -> Option<()> {
        let live = self.live.get();
        if live.saturating_add(bytes) > self.limit {
            self.refused.set(self.refused.get() + 1);
            return None;
        }
        self.live.set(live + bytes);
        self.served.set(self.served.get() + 1);
        Some(())
    }

    /// Return `bytes` to the live pool on deallocation, like the C `mem_free`
    /// subtracting from the zone total.
    fn release(&self, bytes: usize) {
        self.live.set(self.live.get().saturating_sub(bytes));
    }
}

impl Allocator for LimitedAllocator {
    fn allocate_bytes(&self, len: usize) -> Option<Vec<u8>> {
        self.account(len)?;
        Some(vec![0u8; len])
    }

    fn allocate_u16(&self, len: usize) -> Option<Vec<u16>> {
        // Each `u16` element costs two bytes against the ceiling.
        self.account(len.saturating_mul(2))?;
        Some(vec![0u16; len])
    }

    fn deallocate_bytes(&self, buffer: Vec<u8>) {
        self.release(buffer.len());
    }

    fn deallocate_u16(&self, buffer: Vec<u16>) {
        self.release(buffer.len().saturating_mul(2));
    }
}

/// Port of the `mem_zone`/`mem_limit` allocation-failure behavior exercised by
/// `cover_support`'s `inf("63 0", …)` fixture and `cover_wrap`'s primed-stream
/// block: when the sliding window cannot be allocated, `inflate()` reports
/// `Z_MEM_ERROR`.
///
/// `"63 0"` is the fixture `support_force_window_allocation` uses to *force* a
/// window allocation. Here the shared [`LimitedAllocator`] denies every request
/// (`limit == 0`, the analogue of `mem_limit(&strm, 1)`), so the lazy window
/// allocation in `update_window` fails and surfaces as [`ZlibError::MemError`].
/// Init itself succeeds because the window is allocated lazily — exactly as in
/// stock zlib, where `inflateInit2` precedes the limited allocation.
#[test]
fn mem_window_allocation_failure_is_mem_error() {
    let alloc = Rc::new(LimitedAllocator::new(0));
    let shared: Rc<dyn Allocator> = alloc.clone();
    let mut state = InflateState::new_in(-15, Some(shared)).expect("init raw -15 (window is lazy)");

    let input = h2b("63 0");
    let mut out = [0u8; 1];
    let result = state.inflate(&input, &mut out, Flush::NoFlush);

    assert_eq!(
        result.status,
        Err(ZlibError::MemError),
        "a refused window allocation must surface as Z_MEM_ERROR"
    );
    assert!(
        alloc.refused.get() >= 1,
        "the custom allocator must have been consulted and refused the window"
    );
}

/// Port of `cover_wrap`'s `inflateSetDictionary(...) == Z_MEM_ERROR` under
/// `mem_limit`: loading a dictionary on a raw stream populates the sliding
/// window, so a refused window allocation makes `set_dictionary` report
/// `Z_MEM_ERROR`.
///
/// On a raw (`wrap == 0`) stream `set_dictionary` always proceeds (there is no
/// dictionary-id handshake), and `wrap_miscellaneous_reachable_paths` shows the
/// success path returns `Z_OK`. With the denying [`LimitedAllocator`] installed,
/// the window allocation fails instead and is mapped to [`ZlibError::MemError`].
#[test]
fn mem_set_dictionary_allocation_failure_is_mem_error() {
    let alloc = Rc::new(LimitedAllocator::new(0));
    let shared: Rc<dyn Allocator> = alloc.clone();
    let mut state = InflateState::new_in(-8, Some(shared)).expect("init raw -8 (window is lazy)");

    // A 257-byte dictionary (the size `wrap_miscellaneous_reachable_paths` loads
    // successfully under the default allocator) needs a window; the denying
    // allocator refuses it.
    let dict = [0u8; 257];
    assert_eq!(
        state.set_dictionary(&dict),
        Err(ZlibError::MemError),
        "a refused window allocation in set_dictionary must surface as Z_MEM_ERROR"
    );
    assert!(
        alloc.refused.get() >= 1,
        "the custom allocator must have been consulted and refused the window"
    );
}

/// Positive control proving the window allocation is genuinely routed through
/// the installed custom [`Allocator`] (not the global one): the same `"63 0"`
/// fixture that fails under a denying allocator *succeeds* under a generous one,
/// and the allocator records that it served at least one request and refused
/// none.
///
/// Together with `mem_window_allocation_failure_is_mem_error` this shows the
/// custom allocator fully governs the outcome — the core consults it for the
/// window, honors a refusal as `Z_MEM_ERROR`, and uses the buffer it returns on
/// success — the end-to-end behavior `mem_zone` verifies in C.
#[test]
fn mem_generous_allocator_permits_and_routes_window() {
    let alloc = Rc::new(LimitedAllocator::new(usize::MAX));
    let shared: Rc<dyn Allocator> = alloc.clone();
    let mut state = InflateState::new_in(-15, Some(shared)).expect("init raw -15");

    let input = h2b("63 0");
    let mut out = [0u8; 1];
    let result = state.inflate(&input, &mut out, Flush::NoFlush);

    assert_eq!(
        result.status,
        Ok(ReturnCode::Ok),
        "the fixture succeeds when the custom allocator permits the window"
    );
    assert!(
        alloc.served.get() >= 1,
        "the window allocation must have been served by the custom allocator"
    );
    assert_eq!(
        alloc.refused.get(),
        0,
        "no request should have been refused"
    );
}

// ===========================================================================
// cover_back  (port of `infcover.c::cover_back`)
// ===========================================================================

// PORT-NOTE: `cover_back` opens with FFI-layer defenses that are OMITTED here:
//   * `inflateBackInit_(Z_NULL, 0, win, 0, 0) == Z_VERSION_ERROR` — version
//     negotiation is an FFI concern (the `_`-suffixed entry lives in
//     `libz-rs-sys`); the safe `inflate_back_init` takes no version string.
//   * `inflateBack(Z_NULL, …) == Z_STREAM_ERROR` and
//     `inflateBackEnd(Z_NULL) == Z_STREAM_ERROR` — null-pointer defenses,
//     unrepresentable against the owned `Box<InflateState>`; validated in
//     `libz-rs-sys`.
// The reachable `inflateBackInit(&strm, badbits, win) == Z_STREAM_ERROR`
// behavior IS ported below (it is a parameter check, not a null check).

/// `inflate_back_init` rejects out-of-range `windowBits` with `Z_STREAM_ERROR`
/// (the only [8, 15] window is valid for raw back-inflate), and a valid init
/// followed immediately by `inflate_back_end` succeeds.
#[test]
fn back_param_validation_and_lifecycle() {
    // windowBits below the floor and above the ceiling are both rejected.
    assert!(
        matches!(inflate_back_init(7), Err(ZlibError::StreamError)),
        "windowBits 7 -> StreamError"
    );
    assert!(
        matches!(inflate_back_init(16), Err(ZlibError::StreamError)),
        "windowBits 16 -> StreamError"
    );

    // A valid init + end lifecycle (C "inflateBack built-in memory routines").
    let state = inflate_back_init(15).expect("inflate_back_init(15)");
    assert_eq!(
        inflate_back_end(state),
        Ok(ReturnCode::Ok),
        "inflate_back_end"
    );
}

/// Basic successful `inflate_back` of a small valid raw stream: an empty fixed
/// block (`03 00`) decodes to no output and reports `Z_STREAM_END`.
#[test]
fn back_basic_success() {
    let input = h2b("3 0");
    let mut state = inflate_back_init(15).expect("inflate_back_init(15)");

    let mut fed = false;
    let read = || -> &[u8] {
        if fed {
            &input[input.len()..]
        } else {
            fed = true;
            &input[..]
        }
    };
    let mut acc: Vec<u8> = Vec::new();
    let write = |buf: &[u8]| -> bool {
        acc.extend_from_slice(buf);
        false
    };

    let result = inflate_back(&mut state, read, write);
    assert_eq!(
        result.status,
        Ok(ReturnCode::StreamEnd),
        "empty fixed block"
    );
    assert!(acc.is_empty(), "empty block produces no output");

    let _ = inflate_back_end(state);
}

/// Forcing an **output** error: a push closure that reports failure (`true`)
/// makes `inflate_back` fail with `Z_BUF_ERROR` (the C `out()`-returns-nonzero
/// path).
#[test]
fn back_output_error() {
    // `63 0 0` is a fixed block that produces output, so the push closure is
    // invoked and its failure is observed.
    let input = h2b("63 0 0");
    let mut state = inflate_back_init(15).expect("inflate_back_init(15)");

    let mut fed = false;
    let read = || -> &[u8] {
        if fed {
            &input[input.len()..]
        } else {
            fed = true;
            &input[..]
        }
    };
    // Always report failure from the push callback.
    let write = |_buf: &[u8]| -> bool { true };

    let result = inflate_back(&mut state, read, write);
    assert_eq!(
        result.status,
        Err(ZlibError::BufError),
        "push failure -> BufError"
    );

    let _ = inflate_back_end(state);
}

/// Forcing an **input** error: a pull closure that immediately signals
/// end-of-input (empty slice) makes `inflate_back` fail with `Z_BUF_ERROR`
/// rather than panicking (the C `in()`-returns-0 path).
#[test]
fn back_input_exhausted() {
    let mut state = inflate_back_init(15).expect("inflate_back_init(15)");

    // Read always yields empty: no input is ever available.
    let read = || -> &[u8] { &[] };
    let mut acc: Vec<u8> = Vec::new();
    let write = |buf: &[u8]| -> bool {
        acc.extend_from_slice(buf);
        false
    };

    let result = inflate_back(&mut state, read, write);
    assert_eq!(
        result.status,
        Err(ZlibError::BufError),
        "no input -> BufError"
    );

    let _ = inflate_back_end(state);
}

/// A dedicated `cover_back` malformed-stream case (overlapping the back path of
/// `try_stream`, kept distinct per the porting checklist): a bad stored-block
/// length header is reported as `Z_DATA_ERROR` with the exact message.
#[test]
fn back_malformed_data_error() {
    let input = h2b("0 0 0 0 0");
    let mut state = inflate_back_init(15).expect("inflate_back_init(15)");

    let mut fed = false;
    let read = || -> &[u8] {
        if fed {
            &input[input.len()..]
        } else {
            fed = true;
            &input[..]
        }
    };
    let mut acc: Vec<u8> = Vec::new();
    let write = |buf: &[u8]| -> bool {
        acc.extend_from_slice(buf);
        false
    };

    let result = inflate_back(&mut state, read, write);
    let status = result.status;
    assert_eq!(status, Err(ZlibError::DataError), "malformed -> DataError");
    assert_eq!(
        state.msg,
        Some("invalid stored block lengths"),
        "msg parity"
    );

    let _ = inflate_back_end(state);
}

// PORT-NOTE: `cover_back`'s third `inflateBack` call drives the pull callback
// that sets `state->mode = SYNC` to "force an otherwise impossible situation"
// and expects `Z_STREAM_ERROR`. OMITTED: it depends on mutating a private
// `mode` field, which `#![forbid(unsafe_code)]` and the encapsulated
// `InflateState` make unreachable from an external test. The SYNC-mode
// `Z_STREAM_ERROR` transition is itself covered through the public
// `inflate_sync` path in `wrap_miscellaneous_reachable_paths`.

// ===========================================================================
// cover_inflate  (port of `infcover.c::cover_inflate`)
// ===========================================================================
//
// The malformed-DEFLATE catalog, run through BOTH `inflate` and `inflate_back`
// by `try_stream`. Every fixture's hex, expected `msg`, and `err` are copied
// verbatim from `test/infcover.c`; the `msg` strings are the engine's exact
// contract (a mismatch is a parity defect to report, never to loosen).

#[test]
fn inflate_invalid_stored_block_lengths() {
    try_stream("0 0 0 0 0", "invalid stored block lengths", 1);
}

#[test]
fn inflate_fixed() {
    try_stream("3 0", "fixed", 0);
}

#[test]
fn inflate_invalid_block_type() {
    try_stream("6", "invalid block type", 1);
}

#[test]
fn inflate_stored() {
    try_stream("1 1 0 fe ff 0", "stored", 0);
}

#[test]
fn inflate_too_many_length_or_distance_symbols() {
    try_stream("fc 0 0", "too many length or distance symbols", 1);
}

#[test]
fn inflate_invalid_code_lengths_set() {
    try_stream("4 0 fe ff", "invalid code lengths set", 1);
}

#[test]
fn inflate_invalid_bit_length_repeat_a() {
    try_stream("4 0 24 49 0", "invalid bit length repeat", 1);
}

#[test]
fn inflate_invalid_bit_length_repeat_b() {
    try_stream("4 0 24 e9 ff ff", "invalid bit length repeat", 1);
}

#[test]
fn inflate_invalid_code_missing_end_of_block() {
    // NOTE: the C source labels this fixture's family "bit length repeat" but
    // its expected message is the distinct "invalid code -- missing
    // end-of-block"; ported literally per the C contract.
    try_stream("4 0 24 e9 ff 6d", "invalid code -- missing end-of-block", 1);
}

#[test]
fn inflate_invalid_literal_lengths_set() {
    try_stream(
        "4 80 49 92 24 49 92 24 71 ff ff 93 11 0",
        "invalid literal/lengths set",
        1,
    );
}

#[test]
fn inflate_invalid_distances_set() {
    try_stream(
        "4 80 49 92 24 49 92 24 f b4 ff ff c3 84",
        "invalid distances set",
        1,
    );
}

#[test]
fn inflate_invalid_literal_length_code() {
    try_stream(
        "4 c0 81 8 0 0 0 0 20 7f eb b 0 0",
        "invalid literal/length code",
        1,
    );
}

#[test]
fn inflate_invalid_distance_code() {
    try_stream("2 7e ff ff", "invalid distance code", 1);
}

#[test]
fn inflate_invalid_distance_too_far_back() {
    try_stream(
        "c c0 81 0 0 0 0 0 90 ff 6b 4 0",
        "invalid distance too far back",
        1,
    );
}

// The two trailer-mismatch fixtures use the gzip wrapper window (err == -1 →
// win 47), so they run only with the `gzip` feature and only on the `inflate`
// path (`try_stream` skips the back path when `err < 0`).

#[cfg(feature = "gzip")]
#[test]
fn inflate_incorrect_data_check() {
    try_stream(
        "1f 8b 8 0 0 0 0 0 0 0 3 0 0 0 0 1",
        "incorrect data check",
        -1,
    );
}

#[cfg(feature = "gzip")]
#[test]
fn inflate_incorrect_length_check() {
    try_stream(
        "1f 8b 8 0 0 0 0 0 0 0 3 0 0 0 0 0 0 0 0 1",
        "incorrect length check",
        -1,
    );
}

// Window / long-code edge fixtures (all successful raw decodes).

#[test]
fn inflate_pull_17() {
    try_stream("5 c0 21 d 0 0 0 80 b0 fe 6d 2f 91 6c", "pull 17", 0);
}

#[test]
fn inflate_long_code() {
    try_stream(
        "5 e0 81 91 24 cb b2 2c 49 e2 f 2e 8b 9a 47 56 9f fb fe ec d2 ff 1f",
        "long code",
        0,
    );
}

#[test]
fn inflate_length_extra() {
    try_stream("ed c0 1 1 0 0 0 40 20 ff 57 1b 42 2c 4f", "length extra", 0);
}

#[test]
fn inflate_long_distance_and_extra() {
    try_stream(
        "ed cf c1 b1 2c 47 10 c4 30 fa 6f 35 1d 1 82 59 3d fb be 2e 2a fc f c",
        "long distance and extra",
        0,
    );
}

#[test]
fn inflate_window_end() {
    try_stream(
        "ed c0 81 0 0 0 0 80 a0 fd a9 17 a9 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 \
         0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 6",
        "window end",
        0,
    );
}

#[test]
fn inflate_fast_type_return() {
    inf(
        "2 8 20 80 0 3 0",
        "inflate_fast TYPE return",
        0,
        -15,
        258,
        Z_STREAM_END,
    );
}

#[test]
fn inflate_window_wrap() {
    inf("63 18 5 40 c 0", "window wrap", 3, -8, 300, Z_OK);
}

// ===========================================================================
// cover_trees  (port of `infcover.c::cover_trees`)
// ===========================================================================

// PORT-NOTE: `cover_trees` calls the internal `inflate_table(DISTS, …)`
// directly to manifest the "not enough" (`ret == 1`) table-overflow path,
// because, in its own words, "zlib insures that enough is always enough" — the
// condition is unreachable through the public API. OMITTED here: `inflate_table`
// is an internal symbol, and its `ENOUGH`-overflow unit test belongs in
// `src/inflate/tables.rs` (where the function is defined). The table-building
// error handling is still exercised *indirectly* by the `cover_inflate`
// "invalid … set" / "too many length or distance symbols" fixtures above, which
// drive the same code-length machinery through the public `inflate`.

// ===========================================================================
// cover_fast  (port of `infcover.c::cover_fast`)
// ===========================================================================
//
// The `inflate_fast` decoder edge cases. With `#![forbid(unsafe_code)]` the
// core's fast loop uses bounds-checked indexing (AAP §0.2.2): behavior (not
// performance) must match, so these public-`inflate` fixtures still pass.

#[test]
fn fast_length_extra_bits() {
    inf(
        "e5 e0 81 ad 6d cb b2 2c c9 01 1e 59 63 ae 7d ee fb 4d fd b5 35 41 68 \
         ff 7f 0f 0 0 0",
        "fast length extra bits",
        0,
        -8,
        258,
        Z_DATA_ERROR,
    );
}

#[test]
fn fast_distance_extra_bits() {
    inf(
        "25 fd 81 b5 6d 59 b6 6a 49 ea af 35 6 34 eb 8c b9 f6 b9 1e ef 67 49 \
         50 fe ff ff 3f 0 0",
        "fast distance extra bits",
        0,
        -8,
        258,
        Z_DATA_ERROR,
    );
}

#[test]
fn fast_invalid_distance_code() {
    inf(
        "3 7e 0 0 0 0 0",
        "fast invalid distance code",
        0,
        -8,
        258,
        Z_DATA_ERROR,
    );
}

#[test]
fn fast_invalid_literal_length_code() {
    inf(
        "1b 7 0 0 0 0 0",
        "fast invalid literal/length code",
        0,
        -8,
        258,
        Z_DATA_ERROR,
    );
}

#[test]
fn fast_second_level_codes_and_too_far_back() {
    inf(
        "d c7 1 ae eb 38 c 4 41 a0 87 72 de df fb 1f b8 36 b1 38 5d ff ff 0",
        "fast 2nd level codes and too far back",
        0,
        -8,
        258,
        Z_DATA_ERROR,
    );
}

#[test]
fn fast_very_common_case() {
    inf(
        "63 18 5 8c 10 8 0 0 0 0",
        "very common case",
        0,
        -8,
        259,
        Z_OK,
    );
}

#[test]
fn fast_contiguous_and_wrap_around_window() {
    inf(
        "63 60 60 18 c9 0 8 18 18 18 26 c0 28 0 29 0 0 0",
        "contiguous and wrap around window",
        6,
        -8,
        259,
        Z_OK,
    );
}

#[test]
fn fast_copy_direct_from_output() {
    inf(
        "63 0 3 0 0 0 0 0",
        "copy direct from output",
        0,
        -8,
        259,
        Z_STREAM_END,
    );
}

// ===========================================================================
// gzip header capture (dedicated, feature-gated)
// ===========================================================================

/// A dedicated capture test for the `gzip` header path: decoding a minimal
/// gzip stream with a header capture registered populates the `GzHeader` and
/// marks it `done`. This exercises `inflate_get_header` + `header()` beyond the
/// `inf()` driver's blanket registration.
#[cfg(feature = "gzip")]
#[test]
fn gzip_get_header_capture() {
    // The "check gzip length" fixture: a complete, well-formed gzip stream with
    // an empty payload.
    let input = h2b("1f 8b 8 2 0 0 0 0 0 0 1d 26 3 0 0 0 0 0 0 0 0 0");
    let mut state = InflateState::new(47).expect("init gzip auto-detect");

    let head = GzHeader::new()
        .with_extra(Vec::new())
        .with_name(Vec::new())
        .with_comment(Vec::new());
    assert_eq!(
        state.get_header(head),
        Ok(ReturnCode::Ok),
        "get_header registration"
    );

    // Drive the stream to completion (empty payload -> Z_STREAM_END).
    let mut out = [0u8; 16];
    let result = state.inflate(&input, &mut out, Flush::NoFlush);
    assert_eq!(
        result.status,
        Ok(ReturnCode::StreamEnd),
        "well-formed gzip stream decodes to StreamEnd"
    );

    // The captured header must be present and fully parsed.
    let captured = state.header().expect("header should be captured");
    assert!(captured.done, "header parsing should be marked done");
}
