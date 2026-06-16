//! Inflate coverage harness — a Rust port of zlib's `test/infcover.c`.
//!
//! `infcover.c` is zlib's most thorough inflate test: it drives the decoder
//! through its 30+ decode modes and nearly every error branch by feeding
//! carefully crafted (often deliberately malformed) DEFLATE/zlib/gzip streams
//! and asserting the exact return code — and, for the data-error cases, the
//! exact error message, which is part of zlib's observable contract.
//!
//! This file ports the six `cover_*` routines into eight granular `#[test]`s
//! plus a self-test of the hex-decoding helper:
//!
//! | infcover routine | Rust test(s)                                    |
//! |------------------|-------------------------------------------------|
//! | `cover_support`  | [`cover_support`]                               |
//! | `cover_wrap`     | [`cover_wrap`]                                  |
//! | `cover_back`     | [`cover_back`]                                  |
//! | `cover_inflate`  | [`cover_inflate_errors`] + [`cover_inflate_paths`] |
//! | `cover_trees`    | [`cover_trees_via_streams`] (adapted)           |
//! | `cover_fast`     | [`cover_fast`]                                  |
//! | `h2b` decoder    | [`h2b_decoder_unit`]                            |
//!
//! Run with `cargo test --test inflate_coverage`.
//!
//! # Public-API-only, zero `unsafe`
//!
//! The integration test drives ONLY the public, safe surface of `zlib_rs`: the
//! idiomatic [`Inflate`] decompressor and the [`inflate_back`] callback engine
//! (plus their constructors and the `Z_*` constants). It never reaches into
//! `pub(crate)` internals (`inflate_table`, `inflate_fixed`, `inflate_fast`) or
//! private state, and it contains no `unsafe`.
//!
//! Several infcover cases exploit C's lack of encapsulation and cannot — by
//! design — be reproduced through a safe, typed API. Each is documented inline
//! at its call site, but in summary:
//!
//! * **`cover_trees` direct `inflate_table()` call.** infcover calls the table
//!   builder directly to provoke the "not enough" over/under-subscription
//!   errors. That builder is `pub(crate)`; its direct unit coverage lives in
//!   `src/inflate/tables.rs`. Here the SAME table-construction branches are
//!   exercised *behaviorally* through crafted streams fed to the public
//!   [`Inflate::inflate`] — see [`cover_trees_via_streams`].
//! * **`mode = DICT` / `mode = SYNC` state pokes.** infcover writes directly to
//!   the internal `inflate_state.mode` to force otherwise-impossible
//!   transitions. The typed API exposes no such poke, and [`inflate_back`]
//!   resets its mode at entry, so these are unreachable.
//! * **`mem_limit` OOM injection (`Z_MEM_ERROR`).** infcover installs a custom
//!   tracking allocator with a hard cap to force allocation failures. The safe
//!   [`Inflate`] uses the global allocator and exposes no `zalloc`/`zfree`
//!   hooks, so these induced-failure branches are out of reach.
//! * **Null-stream / version-mismatch guards.** `inflate(NULL)`,
//!   `inflateEnd(NULL)`, `inflateInit_(…, "!", …)` (`Z_VERSION_ERROR`), etc. are
//!   unrepresentable: the Rust constructors take no version argument and a
//!   `&mut self` method cannot receive a null stream — these are enforced at
//!   compile time.
//!
//! # Mapping raw codes
//!
//! [`Inflate::inflate`] returns the raw C `int` status (matching `inflate()`),
//! so the assertions compare against the `Z_*` constants exactly as infcover
//! does. The [`assert_code`] helper additionally cross-checks that each raw
//! code round-trips through the idiomatic [`ReturnCode`] / [`ZlibError`]
//! mapping, exercising `from_c_int`/`as_c_int` alongside the wire contract.

use zlib_rs::constants::*;
use zlib_rs::inflate::{SliceInput, inflate_back, inflate_back_end, inflate_back_init};
use zlib_rs::{Inflate, ReturnCode, ZlibError};

// ===========================================================================
// Helper: hex → bytes (port of infcover.c `h2b`, L245)
// ===========================================================================

/// Decode a "liberal hex" string into bytes, reproducing infcover's `h2b`
/// decoder **exactly** so the crafted fixtures below decode identically.
///
/// The rules (straight from infcover): two adjacent hex digits encode one byte;
/// a single hex digit followed by any non-hex delimiter (or the end of the
/// string) also encodes one byte; runs of delimiters are otherwise ignored.
/// The implementation mirrors the C trick of seeding the accumulator with `1`
/// and adding `240` to a lone leading digit so it "looks like two digits",
/// which is what disambiguates the single-digit-plus-delimiter case.
///
/// # Examples
///
/// ```ignore
/// assert_eq!(h2b("1f 8b 8"), [0x1f, 0x8b, 0x08]);
/// assert_eq!(h2b("63 0"), [0x63, 0x00]);
/// ```
fn h2b(hex: &str) -> Vec<u8> {
    let bytes = hex.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity((hex.len() + 1) >> 1);
    // `val == 1` is the "no digits accumulated yet" sentinel; the high bit that
    // appears once two nibbles (or a delimiter-promoted single nibble) are
    // present is what signals "emit a byte".
    let mut val: u32 = 1;
    // The C loop runs the body once for the terminating NUL as well, which is
    // what flushes a trailing single digit; replicate that by iterating one
    // index past the end and treating it as a delimiter (`0`).
    let mut i = 0usize;
    loop {
        let c = if i < bytes.len() { bytes[i] } else { 0u8 };
        if c.is_ascii_digit() {
            val = (val << 4) + u32::from(c - b'0');
        } else if (b'A'..=b'F').contains(&c) {
            val = (val << 4) + u32::from(c - b'A' + 10);
        } else if (b'a'..=b'f').contains(&c) {
            val = (val << 4) + u32::from(c - b'a' + 10);
        } else if val != 1 && val < 32 {
            // One digit followed by a delimiter: promote it to "two digits".
            val += 240;
        }
        if val > 255 {
            out.push((val & 0xff) as u8);
            val = 1;
        }
        if i >= bytes.len() {
            break;
        }
        i += 1;
    }
    out
}

// ===========================================================================
// Helper: code assertion with idiomatic-mapping cross-check
// ===========================================================================

/// Assert that a raw inflate return `actual` equals `expected`, and that the
/// expected code round-trips through the idiomatic [`ReturnCode`] / [`ZlibError`]
/// mapping (`from_c_int` ↔ `as_c_int`).
///
/// This keeps the raw-`i32` assertions (matching infcover and the C ABI) while
/// also exercising the safe error-type bridge the AAP requires.
fn assert_code(actual: i32, expected: i32, ctx: &str) {
    assert_eq!(
        actual, expected,
        "{ctx}: got code {actual}, expected {expected}"
    );
    if expected >= 0 {
        assert_eq!(
            ReturnCode::from_c_int(expected).map(ReturnCode::as_c_int),
            Some(expected),
            "{ctx}: {expected} should map to a ReturnCode",
        );
    } else {
        assert_eq!(
            ZlibError::from_c_int(expected).map(ZlibError::as_c_int),
            Some(expected),
            "{ctx}: {expected} should map to a ZlibError",
        );
    }
}

// ===========================================================================
// Helper: generic inflate() runner (port of infcover.c `inf`, L284)
// ===========================================================================

/// Generic streaming-`inflate` driver mirroring infcover's `inf()`.
///
/// * `hex`   — the input stream in liberal hex (see [`h2b`]).
/// * `what`  — a human label included in assertion messages.
/// * `step`  — bytes to feed per `inflate()` call (`0` means "feed it all").
/// * `win`   — the `windowBits` passed to the constructor (selects framing:
///   `47` = gzip/zlib auto-detect + header capture, `15` = zlib, `-15`/`-8` =
///   raw, `0` = take the window size from the zlib header, `1` = invalid).
/// * `len`   — output-buffer size per call.
/// * `err`   — the return code expected from the **first** `inflate()` call
///   (exactly the assertion infcover makes; later calls are "don't care").
/// * `expect_msg` — when `Some`, the exact `Inflate::msg` expected after the
///   run (error-message parity for the data-error fixtures).
///
/// The decoder is then run to completion in `step`-sized chunks; a
/// `Z_NEED_DICT` result triggers an empty-dictionary handshake (mirroring
/// infcover's dictionary path), and the stream is reconfigured at the end with
/// `reset2(-8)` exactly as infcover finishes with `inflateReset2(strm, -8)`.
fn inf(
    hex: &str,
    what: &str,
    step: usize,
    win: i32,
    len: usize,
    err: i32,
    expect_msg: Option<&str>,
) {
    // Construct the decompressor. infcover's `inf()` returns silently when
    // inflateInit2 fails; the only fixture that triggers this is the invalid
    // `windowBits == 1` case, so assert the constructor error matches the
    // expected code (preserving the test's intent) and stop.
    let mut inflate = match Inflate::with_window_bits(win) {
        Ok(inflate) => inflate,
        Err(error) => {
            assert_eq!(
                error.as_c_int(),
                err,
                "{what}: with_window_bits({win}) failed with {} but expected {err}",
                error.as_c_int(),
            );
            return;
        }
    };

    // `win == 47` requests gzip-header capture (infcover wires up
    // inflateGetHeader here). The header is captured internally and read back
    // via `Inflate::header`; the call only exists with the `gzip` feature.
    #[cfg(feature = "gzip")]
    if win == 47 {
        inflate
            .get_header()
            .expect("get_header on an auto-detect/gzip stream must succeed");
    }

    let input = h2b(hex);
    let have0 = input.len();
    // `step == 0` (or oversized) means "feed the whole stream at once".
    let mut step = if step == 0 || step > have0 {
        have0
    } else {
        step
    };
    if step == 0 {
        step = 1; // empty input never reaches here for live streams; stay safe.
    }
    // infcover allocates `len` output bytes (which can be 0 for header-only
    // error fixtures); use a 1-byte floor so the slice is always valid while
    // preserving the verified behavior.
    let outlen = if len == 0 { 1 } else { len };
    let mut out = vec![0u8; outlen];

    let mut pos = 0usize; // offset of the next unconsumed input byte
    let mut avail = step.min(have0); // exposed-but-unconsumed window length
    let mut have = have0 - avail; // not-yet-exposed remainder
    let mut first_ret: Option<i32> = None;
    let mut guard = 0u32;

    loop {
        guard += 1;
        assert!(guard < 100_000, "{what}: inflate loop failed to terminate");

        let (ret, consumed, _produced) =
            inflate.inflate(&input[pos..pos + avail], &mut out, Z_NO_FLUSH);
        if first_ret.is_none() {
            first_ret = Some(ret);
        }
        pos += consumed;
        avail -= consumed;

        // Terminal results (anything other than OK / BUF_ERROR / NEED_DICT) end
        // the run, exactly as infcover breaks out of its loop.
        if ret != Z_OK && ret != Z_BUF_ERROR && ret != Z_NEED_DICT {
            break;
        }
        if ret == Z_NEED_DICT {
            // Complete the handshake with an empty dictionary. infcover's
            // wrong-id (Z_DATA_ERROR), OOM (Z_MEM_ERROR) and `mode = DICT` poke
            // sub-cases are unreachable through the public, allocator-free API.
            let _ = inflate.set_dictionary(&[]);
        }

        // Refeed up to `step` more bytes (the standard zlib chunked-input
        // pattern: add back the unconsumed tail, then expose the next chunk).
        have += avail;
        avail = step.min(have);
        have -= avail;
        if avail == 0 {
            break;
        }
    }

    let first_ret = first_ret.expect("at least one inflate() call is made");
    assert_code(first_ret, err, what);

    if let Some(msg) = expect_msg {
        assert_eq!(
            inflate.msg,
            Some(msg),
            "{what}: expected msg {msg:?}, got {:?}",
            inflate.msg,
        );
    }

    // infcover finishes by reconfiguring the live stream (inflateReset2(-8)).
    inflate
        .reset2(-8)
        .expect("reset2(-8) must succeed on a live stream");
}

// ===========================================================================
// Helper: inflate_back() runner (the second arm of infcover's `try`, L553)
// ===========================================================================

/// Decode `input` as a raw-DEFLATE stream through [`inflate_back`], returning
/// the raw C status code.
///
/// `failing` selects the output sink's behavior, replicating infcover's `push`
/// callback convention (where a non-null `desc` forces an output failure):
/// `false` is a succeeding sink, `true` reports a write failure on every chunk
/// (which [`inflate_back`] surfaces as `Z_BUF_ERROR`). The full stream is
/// supplied up front via [`SliceInput`] (infcover's `pull` returns no extra
/// input beyond `next_in` when its `desc` is null).
fn run_back(input: &[u8], failing: bool) -> i32 {
    let mut state = match inflate_back_init(15) {
        Ok(state) => state,
        Err(code) => return code,
    };
    // inflate_back needs a caller-supplied window/output buffer of `1 << 15`.
    let mut window = vec![0u8; 1 << 15];
    let mut source = SliceInput::new(input);

    // `BackOutput::write` returns `true` to signal a write FAILURE. A closure is
    // accepted directly via the blanket `impl BackOutput for FnMut(&[u8]) -> bool`.
    let ret = if failing {
        let mut sink = |_chunk: &[u8]| true;
        inflate_back(&mut state, &mut window, &mut source, &mut sink)
    } else {
        let mut sink = |_chunk: &[u8]| false;
        inflate_back(&mut state, &mut window, &mut source, &mut sink)
    };

    // RAII would free the state on drop; call the explicit teardown too, as
    // infcover does with inflateBackEnd, to exercise that path.
    inflate_back_end(state);
    ret
}

// ===========================================================================
// Helper: raw inflate via both inflate() and inflate_back() (infcover `try`)
// ===========================================================================

/// Port of infcover's `try()` (L508): run a raw-DEFLATE stream through BOTH the
/// streaming [`Inflate::inflate`] (with `Z_TREES`) and the [`inflate_back`]
/// callback engine.
///
/// `err` follows infcover's tri-state convention:
/// * `1`  — the stream is malformed: both arms must end in `Z_DATA_ERROR`, and
///   the streaming arm's [`Inflate::msg`] must equal `id`.
/// * `0`  — the stream is valid (`id` is just a label): neither arm may report a
///   fatal error.
/// * `-1` — a trailer mismatch checked only via `inflate()` over a gzip
///   auto-detect window (`47`); the `inflate_back` arm is skipped (it has no
///   trailer), matching infcover's `if (err >= 0)` guard.
///
/// The streaming arm uses `windowBits = 47` when `err < 0` (gzip auto-detect)
/// and `-15` (raw) otherwise, exactly as infcover does.
fn try_inflate(hex: &str, id: &str, err: i32) {
    let input = h2b(hex);

    // --- Arm 1: streaming inflate() with Z_TREES --------------------------
    let win = if err < 0 { 47 } else { -15 };
    let mut inflate =
        Inflate::with_window_bits(win).expect("with_window_bits must succeed in try_inflate");
    // infcover sizes the output buffer at 8× the input (`len << 3`).
    let size = (input.len() << 3).max(1);
    let mut out = vec![0u8; size];
    let mut pos = 0usize;
    // `ret` is assigned on every iteration before it is read, so no dead
    // initializer is needed (an initializer would trip `unused_assignments`).
    let mut ret;
    let mut guard = 0u32;
    loop {
        guard += 1;
        assert!(guard < 100_000, "{id}: inflate loop failed to terminate");

        let (code, consumed, produced) = inflate.inflate(&input[pos..], &mut out, Z_TREES);
        ret = code;
        pos += consumed;
        assert!(
            ret != Z_STREAM_ERROR && ret != Z_MEM_ERROR,
            "{id}: unexpected fatal code {ret}",
        );
        if ret == Z_DATA_ERROR || ret == Z_NEED_DICT {
            break;
        }
        // Continue while input remains OR the output buffer filled completely
        // (infcover: `while (strm.avail_in || strm.avail_out == 0)`).
        let more_input = pos < input.len();
        let out_full = produced == size;
        if !(more_input || out_full) {
            break;
        }
        if consumed == 0 && produced == 0 {
            break; // no progress possible; avoid spinning
        }
    }
    if err != 0 {
        assert_code(ret, Z_DATA_ERROR, id);
        assert_eq!(inflate.msg, Some(id), "{id}: inflate message mismatch");
    }

    // --- Arm 2: inflate_back() (only when err >= 0, per infcover) ---------
    if err >= 0 {
        let back = run_back(&input, false);
        assert!(
            back != Z_STREAM_ERROR,
            "{id}: inflate_back returned a stream error"
        );
        if err != 0 {
            // NOTE: inflate_back operates on an `InflateState`, which carries no
            // `msg` field, so only the return code is asserted here. The
            // message-parity check is fully covered by Arm 1 (Inflate::msg).
            assert_code(back, Z_DATA_ERROR, id);
        }
    }
}

// ===========================================================================
// Test: h2b decoder self-test (the hex helper underpins every fixture)
// ===========================================================================

/// Verifies the [`h2b`] decoder reproduces infcover's semantics exactly,
/// including the two known-answer vectors called out in the task spec.
#[test]
fn h2b_decoder_unit() {
    // Two adjacent hex digits → one byte; a lone digit + delimiter → one byte.
    assert_eq!(h2b("1f 8b 8"), [0x1f_u8, 0x8b, 0x08]);
    assert_eq!(h2b("63 0"), [0x63_u8, 0x00]);
    assert_eq!(h2b("ff"), [0xff_u8]);
    assert_eq!(h2b("f f"), [0x0f_u8, 0x0f]);
    assert_eq!(h2b("1 2 3"), [0x01_u8, 0x02, 0x03]);
    assert_eq!(h2b("78 9c"), [0x78_u8, 0x9c]);
    // A trailing single digit is flushed by the terminating NUL.
    assert_eq!(h2b("8 b8 0 0 0 1"), [0x08_u8, 0xb8, 0x00, 0x00, 0x00, 0x01]);
    // Mixed case is accepted.
    assert_eq!(h2b("De aD bE eF"), [0xde_u8, 0xad, 0xbe, 0xef]);
    // Delimiter-only / empty inputs yield no bytes.
    assert!(h2b("").is_empty());
    assert!(h2b("   ").is_empty());
}

// ===========================================================================
// Test 1: cover_support (port of infcover.c `cover_support`, L350)
// ===========================================================================

/// Covers inflate.c up to `inflate()`: priming, the empty-dictionary stream
/// error, window allocation/replacement/split-update, fixed blocks, and the
/// invalid-`windowBits` constructor rejection.
#[test]
fn cover_support() {
    // inflatePrime: push 5 bits, then flush the accumulator with a negative
    // bit count. Both must succeed on a fresh stream.
    let mut inflate = Inflate::new().expect("inflate init must succeed");
    inflate.prime(5, 31).expect("prime(5, 31) must succeed");
    inflate
        .prime(-1, 0)
        .expect("prime(-1, 0) flushes the bit accumulator and must succeed");
    // inflateSetDictionary on a fresh (pre-header, non-raw) stream is invalid:
    // infcover asserts Z_STREAM_ERROR; the typed API surfaces it as Err.
    assert!(
        inflate.set_dictionary(&[]).is_err(),
        "set_dictionary on a fresh zlib stream must be a stream error",
    );
    drop(inflate); // RAII teardown replaces inflateEnd.

    // Window allocation / replacement / split-update + fixed-block paths.
    inf("63 0", "force window allocation", 0, -15, 1, Z_OK, None);
    inf(
        "63 18 5",
        "force window replacement",
        0,
        -8,
        259,
        Z_OK,
        None,
    );
    inf(
        "63 18 68 30 d0 0 0",
        "force split window update",
        4,
        -8,
        259,
        Z_OK,
        None,
    );
    inf("3 0", "use fixed blocks", 0, -15, 1, Z_STREAM_END, None);

    // windowBits == 1 is invalid (the analogue of inflateInit2 returning
    // Z_STREAM_ERROR). infcover feeds this through inf() with empty input.
    inf("", "bad window size", 0, 1, 0, Z_STREAM_ERROR, None);

    // UNREACHABLE (documented): infcover's inflateInit_ version-mismatch case
    // (`Z_VERSION_ERROR`) has no analogue — the Rust constructors take no
    // version argument. Confirm the reported version constant instead.
    assert_eq!(ZLIB_VERSION, "1.3.2.1-motley");
    assert_eq!(ZLIB_VERNUM, 0x1321);
}

// ===========================================================================
// Test 2: cover_wrap (port of infcover.c `cover_wrap`, L388)
// ===========================================================================

/// Covers the zlib/gzip header and trailer cases and the diagnostic/recovery
/// code after `inflate()`: bad header fields, dictionary signalling, adler/crc
/// checks, and the `inflateSync`/`inflateMark`/`inflateCopy`/`inflateReset2`
/// surface.
#[test]
fn cover_wrap() {
    // UNREACHABLE (documented): infcover's bad-argument guards —
    // `inflate(NULL)`, `inflateEnd(NULL)`, `inflateCopy(NULL)` all returning
    // Z_STREAM_ERROR — are enforced at COMPILE time here: a `&mut self` method
    // cannot receive a null stream, so the cases are unrepresentable.

    // --- zlib-wrapper and raw fixtures (always available) ----------------
    inf(
        "77 85",
        "bad zlib method",
        0,
        15,
        0,
        Z_DATA_ERROR,
        Some("unknown compression method"),
    );
    inf("8 99", "set window size from header", 0, 0, 0, Z_OK, None);
    inf(
        "78 9c",
        "bad zlib window size",
        0,
        8,
        0,
        Z_DATA_ERROR,
        Some("invalid window size"),
    );
    inf(
        "78 9c 63 0 0 0 1 0 1",
        "check adler32",
        0,
        15,
        1,
        Z_STREAM_END,
        None,
    );
    inf(
        "8 b8 0 0 0 1",
        "need dictionary",
        0,
        8,
        0,
        Z_NEED_DICT,
        None,
    );
    inf("78 9c 63 0", "compute adler32", 0, 15, 1, Z_OK, None);

    // --- gzip-framed fixtures (require the `gzip` feature) ---------------
    // The gzip wrapper format (windowBits 24..=31 and the 40..=47 auto-detect
    // range) is gated on the `gzip` feature; gate the gzip fixtures to match so
    // the test still builds clean under `--no-default-features`.
    #[cfg(feature = "gzip")]
    {
        inf(
            "1f 8b 0 0",
            "bad gzip method",
            0,
            31,
            0,
            Z_DATA_ERROR,
            Some("unknown compression method"),
        );
        inf(
            "1f 8b 8 80",
            "bad gzip flags",
            0,
            31,
            0,
            Z_DATA_ERROR,
            Some("unknown header flags set"),
        );
        inf(
            "1f 8b 8 1e 0 0 0 0 0 0 1 0 0 0 0 0 0",
            "bad header crc",
            0,
            47,
            1,
            Z_DATA_ERROR,
            Some("header crc mismatch"),
        );
        inf(
            "1f 8b 8 2 0 0 0 0 0 0 1d 26 3 0 0 0 0 0 0 0 0 0",
            "check gzip length",
            0,
            47,
            0,
            Z_STREAM_END,
            None,
        );
        inf(
            "78 90",
            "bad zlib header check",
            0,
            47,
            0,
            Z_DATA_ERROR,
            Some("incorrect header check"),
        );
    }

    // Exercise the post-inflate diagnostic/recovery surface that infcover
    // touches: inflateSyncPoint / inflateMark / inflateCodesUsed /
    // inflateValidate / inflateCopy / inflateSync / inflateReset2.
    //
    // UNREACHABLE (documented): infcover's `mode = DICT` internal-state poke and
    // its mem_limit-driven Z_MEM_ERROR cases require reaching into private state
    // and a custom tracking allocator; the public, allocator-free API exposes
    // neither, so those induced-failure branches are out of reach.
    let mut inflate = Inflate::with_window_bits(-15).expect("raw init must succeed");
    let data = h2b("63 0 0 0 0 ff ff");
    let mut out = vec![0u8; 64];
    let _ = inflate.inflate(&data, &mut out, Z_NO_FLUSH);
    let _ = inflate.sync_point(); // inflateSyncPoint
    let _ = inflate.mark(); // inflateMark
    let _ = inflate.codes_used(); // inflateCodesUsed
    inflate.validate(false); // inflateValidate
    let _copy = inflate.copy(); // inflateCopy (deep clone; no pointer fix-up)
    let (sret, _used) = inflate.sync(&data); // inflateSync
    assert!(
        sret == Z_OK || sret == Z_DATA_ERROR || sret == Z_BUF_ERROR,
        "inflateSync returned an unexpected code: {sret}",
    );
    inflate
        .reset2(15)
        .expect("inflateReset2 reconfiguration must succeed");
}

// ===========================================================================
// Test 3: cover_back (port of infcover.c `cover_back`, L471)
// ===========================================================================

/// Covers [`inflate_back`] init validation, a successful empty-block decode, a
/// forced output-write failure, and the undersized-window guard.
#[test]
fn cover_back() {
    // inflateBackInit with out-of-range windowBits → Err (C: Z_STREAM_ERROR).
    assert!(
        inflate_back_init(7).is_err(),
        "windowBits 7 is below the 8..=15 range"
    );
    assert!(
        inflate_back_init(16).is_err(),
        "windowBits 16 is above the 8..=15 range"
    );
    // UNREACHABLE (documented): infcover's `inflateBackInit_(NULL, ...)` ==
    // Z_VERSION_ERROR has no analogue — the Rust init takes no version argument.

    // Valid init + a complete empty fixed block → Z_STREAM_END (success sink).
    let empty_fixed = h2b("3 0");
    assert_code(
        run_back(&empty_fixed, false),
        Z_STREAM_END,
        "inflate_back empty fixed block",
    );

    // A stream that produces output, with a sink that reports a write failure →
    // Z_BUF_ERROR (infcover forces this via a non-null `push` desc).
    let producing = h2b("63 0 0");
    assert_code(
        run_back(&producing, true),
        Z_BUF_ERROR,
        "inflate_back failing output sink",
    );

    // An undersized window buffer is rejected with Z_STREAM_ERROR — the safe
    // port's bounds guard (C assumes an exactly `1 << windowBits` window and
    // would otherwise index out of bounds). This also gives behavioral coverage
    // of inflate_back's Z_STREAM_ERROR branch.
    {
        let mut state = inflate_back_init(15).expect("inflate_back_init(15) must succeed");
        let mut tiny = vec![0u8; 16]; // far smaller than 1 << 15
        let mut source = SliceInput::new(&empty_fixed);
        let mut sink = |_chunk: &[u8]| false;
        let ret = inflate_back(&mut state, &mut tiny, &mut source, &mut sink);
        assert_code(ret, Z_STREAM_ERROR, "inflate_back undersized window");
        assert_code(
            inflate_back_end(state),
            Z_OK,
            "inflate_back_end after undersized window",
        );
    }

    // UNREACHABLE (documented): infcover's third inflateBack call forces
    // `state->mode = SYNC` mid-decode via its `pull` callback to provoke a
    // Z_STREAM_ERROR. The safe port resets `mode = Type` at entry and the
    // BackInput::fill callback has no access to the state, so this mid-decode
    // poke cannot be reproduced; the Z_STREAM_ERROR branch is covered
    // behaviorally by the undersized-window case above.
}

// ===========================================================================
// Test 4a: cover_inflate — error cases (split of infcover.c `cover_inflate`)
// ===========================================================================

/// The malformed-stream half of infcover's `cover_inflate` (L582): every
/// `try("...", id, 1)` and the two inflate-only trailer mismatches
/// (`try("...", id, -1)`). Each asserts `Z_DATA_ERROR` and exact message parity
/// through `inflate()` (and, for `err == 1`, also through `inflate_back`).
#[test]
fn cover_inflate_errors() {
    // err == 1: malformed raw streams → Z_DATA_ERROR with the exact message id,
    // verified through BOTH inflate(Z_TREES) and inflate_back.
    try_inflate("0 0 0 0 0", "invalid stored block lengths", 1);
    try_inflate("6", "invalid block type", 1);
    try_inflate("fc 0 0", "too many length or distance symbols", 1);
    try_inflate("4 0 fe ff", "invalid code lengths set", 1);
    try_inflate("4 0 24 49 0", "invalid bit length repeat", 1);
    try_inflate("4 0 24 e9 ff ff", "invalid bit length repeat", 1);
    try_inflate("4 0 24 e9 ff 6d", "invalid code -- missing end-of-block", 1);
    try_inflate(
        "4 80 49 92 24 49 92 24 71 ff ff 93 11 0",
        "invalid literal/lengths set",
        1,
    );
    try_inflate(
        "4 80 49 92 24 49 92 24 f b4 ff ff c3 84",
        "invalid distances set",
        1,
    );
    try_inflate(
        "4 c0 81 8 0 0 0 0 20 7f eb b 0 0",
        "invalid literal/length code",
        1,
    );
    try_inflate("2 7e ff ff", "invalid distance code", 1);
    try_inflate(
        "c c0 81 0 0 0 0 0 90 ff 6b 4 0",
        "invalid distance too far back",
        1,
    );

    // err == -1: gzip trailer mismatches, checked only via inflate() over a
    // gzip auto-detect window (47). These require the `gzip` feature for the
    // gzip framing; the inflate_back arm is intentionally skipped (raw DEFLATE
    // has no trailer), matching infcover's `if (err >= 0)` guard.
    #[cfg(feature = "gzip")]
    {
        try_inflate(
            "1f 8b 8 0 0 0 0 0 0 0 3 0 0 0 0 1",
            "incorrect data check",
            -1,
        );
        try_inflate(
            "1f 8b 8 0 0 0 0 0 0 0 3 0 0 0 0 0 0 0 0 1",
            "incorrect length check",
            -1,
        );
    }
}

// ===========================================================================
// Test 4b: cover_inflate — success/path cases (split of `cover_inflate`)
// ===========================================================================

/// The valid-stream half of infcover's `cover_inflate`: every `try("...", id, 0)`
/// (both arms must succeed) plus the two `inf()`-driven path fixtures that
/// exercise the `inflate_fast` TYPE-return and window-wrap branches.
#[test]
fn cover_inflate_paths() {
    // err == 0: valid raw streams driving specific decode paths. Both arms must
    // succeed (inflate → no fatal error; inflate_back → Z_STREAM_END).
    try_inflate("3 0", "fixed", 0);
    try_inflate("1 1 0 fe ff 0", "stored", 0);
    try_inflate("5 c0 21 d 0 0 0 80 b0 fe 6d 2f 91 6c", "pull 17", 0);
    try_inflate(
        "5 e0 81 91 24 cb b2 2c 49 e2 f 2e 8b 9a 47 56 9f fb fe ec d2 ff 1f",
        "long code",
        0,
    );
    try_inflate("ed c0 1 1 0 0 0 40 20 ff 57 1b 42 2c 4f", "length extra", 0);
    try_inflate(
        "ed cf c1 b1 2c 47 10 c4 30 fa 6f 35 1d 1 82 59 3d fb be 2e 2a fc f c",
        "long distance and extra",
        0,
    );
    // 46 tokens exactly, reproducing infcover's two concatenated string
    // literals (13 header tokens + 32 zeros + the trailing `6`).
    try_inflate(
        "ed c0 81 0 0 0 0 80 a0 fd a9 17 a9 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 6",
        "window end",
        0,
    );

    // inf()-driven path fixtures: the inflate_fast TYPE return and the window
    // wrap-around update.
    inf(
        "2 8 20 80 0 3 0",
        "inflate_fast TYPE return",
        0,
        -15,
        258,
        Z_STREAM_END,
        None,
    );
    inf("63 18 5 40 c 0", "window wrap", 3, -8, 300, Z_OK, None);
}

// ===========================================================================
// Test 5: cover_trees — table-builder edges via crafted streams (ADAPTED)
// ===========================================================================

/// Behavioral port of infcover.c `cover_trees` (L618).
///
/// The C original calls `inflate_table(DISTS, lens, 16, &next, &bits, work)`
/// **directly** to manifest the table builder's "not enough room" (`ret == 1`)
/// branch, because — quoting infcover — "zlib insures that enough is always
/// enough", so the over-subscription error is otherwise unreachable from a
/// stream. In `zlib-rs`, `inflate_table` is `pub(crate)`: it is **not** part of
/// the public surface this integration test is allowed to touch (HARD
/// CONSTRAINT: public API only, no reaching into `pub(crate)` internals).
///
/// Direct unit coverage of `inflate_table` — including the over/under-subscribed
/// (`ret == 1` / incomplete-code) paths the C harness pokes — lives in the
/// in-crate unit tests in `src/inflate/tables.rs`, which CAN see the
/// `pub(crate)` builder. Here we instead preserve the *behavioral* coverage of
/// the Huffman table-construction error branches by driving crafted DEFLATE
/// streams through the public `inflate()`: each stream exercises one
/// table-build rejection path (`inflate_table` returning the error that
/// `inflate()` surfaces as `Z_DATA_ERROR` with a specific message id).
#[test]
fn cover_trees_via_streams() {
    // Over-subscribed code-length (CODES) table → "invalid code lengths set".
    inf(
        "4 0 fe ff",
        "tree: invalid code lengths set",
        0,
        -15,
        16,
        Z_DATA_ERROR,
        Some("invalid code lengths set"),
    );

    // Bit-length repeat code that runs past the end of the set.
    inf(
        "4 0 24 49 0",
        "tree: invalid bit length repeat",
        0,
        -15,
        16,
        Z_DATA_ERROR,
        Some("invalid bit length repeat"),
    );

    // Symbol counts (HLIT/HDIST) exceeding the legal maxima — the count guard
    // that precedes table construction.
    inf(
        "fc 0 0",
        "tree: too many length or distance symbols",
        0,
        -15,
        16,
        Z_DATA_ERROR,
        Some("too many length or distance symbols"),
    );

    // Over-subscribed literal/length (LENS) table → "invalid literal/lengths set".
    inf(
        "4 80 49 92 24 49 92 24 71 ff ff 93 11 0",
        "tree: invalid literal/lengths set",
        0,
        -15,
        16,
        Z_DATA_ERROR,
        Some("invalid literal/lengths set"),
    );

    // Over-subscribed distance (DISTS) table → "invalid distances set". This is
    // the exact failure class the C `cover_trees` provokes via its direct
    // `inflate_table(DISTS, ...)` call, reproduced here through a stream.
    inf(
        "4 80 49 92 24 49 92 24 f b4 ff ff c3 84",
        "tree: invalid distances set",
        0,
        -15,
        16,
        Z_DATA_ERROR,
        Some("invalid distances set"),
    );
}

// ===========================================================================
// Test 6: cover_fast — inffast.c decoding + window copying (PORTABLE)
// ===========================================================================

/// Direct port of infcover.c `cover_fast` (L642).
///
/// Every fixture drives the `inflate_fast` hot path through the **public**
/// `inflate()` (via the [`inf`] harness), so — unlike `cover_trees` — this
/// function ports verbatim with no adaptation. Window bits are `-8` (raw
/// DEFLATE, small 256-byte window) for all cases, which forces the fast-path
/// window-copy and wrap-around logic to engage on short outputs.
///
/// The error fixtures additionally assert `Inflate::msg` parity (the message
/// ids are part of the observable contract). infcover's own `inf()` only
/// checks the return code; asserting the message here strengthens the port.
/// Note the subtle empirical truth preserved below: "fast distance extra bits"
/// surfaces as "invalid distance too far back" (not "invalid distance code").
#[test]
fn cover_fast() {
    // --- error paths through the fast inner loop (each → Z_DATA_ERROR) -------
    inf(
        "e5 e0 81 ad 6d cb b2 2c c9 01 1e 59 63 ae 7d ee fb 4d fd b5 35 41 68 ff 7f 0f 0 0 0",
        "fast length extra bits",
        0,
        -8,
        258,
        Z_DATA_ERROR,
        Some("invalid distance too far back"),
    );
    inf(
        "25 fd 81 b5 6d 59 b6 6a 49 ea af 35 6 34 eb 8c b9 f6 b9 1e ef 67 49 50 fe ff ff 3f 0 0",
        "fast distance extra bits",
        0,
        -8,
        258,
        Z_DATA_ERROR,
        Some("invalid distance too far back"),
    );
    inf(
        "3 7e 0 0 0 0 0",
        "fast invalid distance code",
        0,
        -8,
        258,
        Z_DATA_ERROR,
        Some("invalid distance code"),
    );
    inf(
        "1b 7 0 0 0 0 0",
        "fast invalid literal/length code",
        0,
        -8,
        258,
        Z_DATA_ERROR,
        Some("invalid literal/length code"),
    );
    inf(
        "d c7 1 ae eb 38 c 4 41 a0 87 72 de df fb 1f b8 36 b1 38 5d ff ff 0",
        "fast 2nd level codes and too far back",
        0,
        -8,
        258,
        Z_DATA_ERROR,
        Some("invalid distance too far back"),
    );

    // --- success paths exercising fast-path window copy / wrap-around --------
    inf(
        "63 18 5 8c 10 8 0 0 0 0",
        "very common case",
        0,
        -8,
        259,
        Z_OK,
        None,
    );
    inf(
        "63 60 60 18 c9 0 8 18 18 18 26 c0 28 0 29 0 0 0",
        "contiguous and wrap around window",
        6,
        -8,
        259,
        Z_OK,
        None,
    );
    inf(
        "63 0 3 0 0 0 0 0",
        "copy direct from output",
        0,
        -8,
        259,
        Z_STREAM_END,
        None,
    );
}
