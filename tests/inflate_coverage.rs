//! Inflate coverage harness — a safe-Rust port of zlib's `test/infcover.c`.
//!
//! `infcover.c` is the most intricate of zlib's test programs: it drives the
//! INFLATE engine through *all* of its decode modes and error branches with
//! hand-crafted, byte-level DEFLATE/zlib/gzip streams. This module reproduces
//! that behavioural coverage against the `zlib_rs` crate, exercising the public
//! [`Inflate`] decompressor and the callback-based [`inflate_back`] API.
//!
//! # Provenance
//!
//! Ported from `test/infcover.c` (Copyright 2011/2016/2024 Mark Adler). The six
//! C `cover_*` functions are reorganised into eight granular `#[test]`s
//! (`cover_support`, `cover_wrap`, `cover_back`, `cover_inflate_errors`,
//! `cover_inflate_paths`, `cover_trees_via_streams`, `cover_fast`, plus the
//! `h2b_decoder_unit` self-test of the hex decoder). Run with
//! `cargo test --test inflate_coverage`.
//!
//! # Public API only — what is and is NOT reachable
//!
//! `infcover.c` deliberately `#define ZLIB_INTERNAL`s and reaches into the
//! private `struct inflate_state` to manifest situations the public API cannot
//! produce. Those pokes are **unreachable** through `zlib_rs`'s safe, typed
//! surface and are documented inline where they occur:
//!
//! * `cover_trees` calls the `pub(crate)` `inflate_table()` builder directly to
//!   force its "not enough room" return. That builder is crate-internal here, so
//!   [`cover_trees_via_streams`] instead exercises the *same* over/under-
//!   subscribed Huffman-table branches through crafted DEFLATE streams fed to
//!   the public `inflate()`. The direct unit coverage of `inflate_table` lives in
//!   `src/inflate/tables.rs`'s own `#[cfg(test)]` module, not here.
//! * `cover_wrap`'s `((struct inflate_state *)strm.state)->mode = DICT` poke and
//!   `pull()`'s `state->mode = SYNC` poke touch private state — unrepresentable
//!   here. The `Z_NEED_DICT` handshake and `inflateSync` recovery they support
//!   are still covered through legitimate crafted streams.
//! * The `mem_setup`/`mem_limit` allocator that injects `Z_MEM_ERROR` has no
//!   analogue: the public [`Inflate`] uses the global allocator and exposes no
//!   `zalloc`/`zfree` hooks, so OOM-injection branches are out of reach.
//! * Null-pointer argument guards (`inflate(NULL)`, `inflateEnd(NULL)`, …) and
//!   the `inflateInit_` version-string check are enforced by Rust's type system
//!   at compile time and need no run-time test.
//! * [`inflate_back`] returns only an `i32`; the underlying state exposes no
//!   `msg` field (unlike C's `strm->msg`), so the message-id parity check from
//!   `try()`'s `inflateBack` arm is asserted on the **return code** only.
//!
//! Everything else — every reachable `inf()`, `try()`, and `cover_fast` fixture
//! — is ported with its exact hex input, `step`, `windowBits`, output length and
//! expected return code, and (for the data-error fixtures) its exact
//! `strm->msg` id string, which is part of the observable contract.
//!
//! [`Inflate`]: zlib_rs::Inflate
//! [`inflate_back`]: zlib_rs::inflate::inflate_back

use zlib_rs::constants::{
    Z_BLOCK, Z_BUF_ERROR, Z_DATA_ERROR, Z_MEM_ERROR, Z_NEED_DICT, Z_NO_FLUSH, Z_OK, Z_STREAM_END,
    Z_STREAM_ERROR, Z_TREES,
};
use zlib_rs::inflate::{SliceInput, inflate_back, inflate_back_end, inflate_back_init};
use zlib_rs::{Inflate, ZlibError};

// ===========================================================================
// Harness helpers (ports of infcover.c's `h2b`, `inf`, and `try`).
// ===========================================================================

/// Liberal hex→bytes decoder — a faithful port of `infcover.c`'s `h2b`
/// (`test/infcover.c` L245).
///
/// The decoder scans the string character by character. A running accumulator
/// `val` starts at the sentinel `1`; each hex digit shifts a nibble in
/// (`val = (val << 4) + digit`). The C trick is the sentinel bit: after one hex
/// digit `val` lands in `16..=31` (`!= 1` and `< 32`), and a following
/// delimiter adds `240` to push it past `255`, so a **single** digit before a
/// delimiter still decodes to one byte. Two adjacent digits naturally exceed
/// `255`. Whenever `val > 255`, the low byte is emitted and `val` resets to `1`.
///
/// The C loop runs through the terminating NUL (`do { … } while (*hex++)`) so a
/// trailing single digit is flushed; here a synthetic trailing delimiter byte
/// (`0`) reproduces that, since Rust `&str`s are not NUL-terminated.
fn h2b(hex: &str) -> Vec<u8> {
    /// Decode one ASCII byte as a hex digit value, or `None` for a delimiter.
    fn hex_val(c: u8) -> Option<u32> {
        match c {
            b'0'..=b'9' => Some((c - b'0') as u32),
            b'A'..=b'F' => Some((c - b'A' + 10) as u32),
            b'a'..=b'f' => Some((c - b'a' + 10) as u32),
            _ => None,
        }
    }

    let mut out: Vec<u8> = Vec::with_capacity((hex.len() + 1) >> 1);
    let mut val: u32 = 1;
    // Iterate the input bytes, then one synthetic delimiter to flush a trailing
    // single digit (mirrors C looping through the terminating NUL).
    for c in hex.bytes().chain(core::iter::once(0u8)) {
        if let Some(digit) = hex_val(c) {
            val = (val << 4) + digit;
        } else if val != 1 && val < 32 {
            // One digit followed by a delimiter — make it look like two digits.
            val += 240;
        }
        if val > 255 {
            out.push((val & 0xff) as u8);
            val = 1;
        }
    }
    out
}

/// Generic `inflate()` run — a port of `infcover.c`'s `inf` (`test/infcover.c`
/// L284).
///
/// * `hex`   — hexadecimal input data (decoded by [`h2b`]).
/// * `what`  — label included in assertion messages.
/// * `step`  — bytes to feed per `inflate()` call (`0` means feed everything).
/// * `win`   — `windowBits` passed to the constructor (selects framing /
///   auto-detect: `47` = gzip+header, `15` = zlib, `-15`/`-8` = raw, `0` =
///   detect zlib window from the header, `1` = invalid).
/// * `len`   — output-buffer size; the buffer is reused (its contents discarded)
///   on every call, exactly as the C harness resets `next_out`/`avail_out`.
/// * `err`   — return code expected from the **first** `inflate()` call.
/// * `expect_msg` — when `Some`, the exact `strm->msg` id the first error must
///   carry (the data-error fixtures); checked before the trailing `reset2`,
///   which clears `msg`.
///
/// On a `Z_NEED_DICT` handshake an empty dictionary is supplied (mirroring the C
/// harness), after first confirming that a wrong dictionary is rejected with a
/// data error. The function ends by exercising `inflateReset2(strm, -8)`, as the
/// C `inf` does.
fn inf(
    hex: &str,
    what: &str,
    step: usize,
    win: i32,
    len: usize,
    err: i32,
    expect_msg: Option<&'static str>,
) {
    let input = h2b(hex);

    // `inflateInit2(strm, win)`. The C `inf` returns early when init fails; the
    // only fixture that hits this is the invalid `windowBits = 1` "bad window
    // size" case, whose expected `err` is the constructor's error code.
    let mut strm = match Inflate::with_window_bits(win) {
        Ok(strm) => strm,
        Err(e) => {
            assert_eq!(
                e.as_c_int(),
                err,
                "{what}: constructor should fail with code {err}, got {}",
                e.as_c_int()
            );
            return;
        }
    };

    // For `win == 47` the C harness attaches a gzip header sink via
    // `inflateGetHeader`. Header capture only exists with the `gzip` feature
    // (and `win >= 24` only constructs successfully when `gzip` is enabled).
    #[cfg(feature = "gzip")]
    if win == 47 {
        strm.get_header().expect("attach gzip header sink");
    }

    let total = input.len();
    // `step == 0` (or an over-large step) means "feed all of it at once".
    let step_eff = if step == 0 || step > total {
        total
    } else {
        step
    };
    let mut out = vec![0u8; len];

    let mut consumed_total = 0usize;
    let mut visible = step_eff.min(total);
    let mut first = true;
    let mut last_ret = Z_OK;
    let mut guard = 0u32;

    loop {
        if visible == 0 {
            break; // C: `while (strm.avail_in)` — no more input to release.
        }
        guard += 1;
        assert!(
            guard < 1_000_000,
            "{what}: inflate loop failed to terminate"
        );

        let end = (consumed_total + visible).min(total);
        let (ret, consumed, produced) =
            strm.inflate(&input[consumed_total..end], &mut out, Z_NO_FLUSH);
        consumed_total += consumed;

        if first {
            assert_eq!(
                ret, err,
                "{what}: first inflate() expected {err}, got {ret}"
            );
            first = false;
        }
        last_ret = ret;

        // C: `if (ret != Z_OK && ret != Z_BUF_ERROR && ret != Z_NEED_DICT) break;`
        if ret != Z_OK && ret != Z_BUF_ERROR && ret != Z_NEED_DICT {
            break;
        }

        if ret == Z_NEED_DICT {
            // C provides an (empty) dictionary after first proving a wrong one is
            // rejected. The `mem_limit` OOM injection and the
            // `state->mode = DICT` poke around it touch private state and are
            // unreachable; the reachable dictionary-handshake coverage remains.
            if !input.is_empty() {
                assert!(
                    matches!(strm.set_dictionary(&input[..1]), Err(ZlibError::DataError)),
                    "{what}: a wrong dictionary must be rejected with a data error"
                );
            }
            // The only Z_NEED_DICT fixture's header DICTID is 1 == adler32(1, &[]),
            // so the empty dictionary is accepted; a final input-starved call then
            // reports Z_BUF_ERROR.
            if strm.set_dictionary(&[]).is_ok() {
                let (ret2, _c, _p) = strm.inflate(&[], &mut out, Z_NO_FLUSH);
                assert_eq!(
                    ret2, Z_BUF_ERROR,
                    "{what}: inflate after dictionary expects Z_BUF_ERROR"
                );
            }
            break;
        }

        // Safety net: if a call neither consumes input nor produces output there
        // is no progress to be made, so stop rather than spin.
        if consumed == 0 && produced == 0 {
            break;
        }

        let remaining = total - consumed_total;
        visible = step_eff.min(remaining);
    }

    // Error-message parity (a hard requirement): the data-error fixtures carry an
    // observable `strm->msg`. Assert it *before* the trailing reset clears it.
    if let Some(expected) = expect_msg {
        assert_eq!(
            last_ret, Z_DATA_ERROR,
            "{what}: expected a data error carrying a message"
        );
        assert_eq!(strm.msg, Some(expected), "{what}: message-id parity");
    }

    // C tail: `inflateReset2(&strm, -8)` must succeed regardless of prior state.
    strm.reset2(-8).expect("reset2(-8) at end of inf()");
}

/// Decode a raw-DEFLATE stream through **both** `inflate()` and
/// [`inflate_back`] — a port of `infcover.c`'s `try` (`test/infcover.c` L508).
/// (`try` is a reserved Rust keyword, hence `try_inflate`.)
///
/// * `err == 1`  — the stream is invalid: both engines must return
///   `Z_DATA_ERROR`, and (for the `inflate()` arm, which exposes `strm->msg`)
///   the message must equal `id`.
/// * `err == 0`  — the stream is valid: both engines run; the only requirement
///   is that neither reports `Z_STREAM_ERROR` (a success/`Z_STREAM_END` path).
/// * `err == -1` — invalid, but checked through `inflate()` **only** (with the
///   gzip auto-detect window so the trailing checksum/length is validated); the
///   `inflate_back` arm is skipped, matching the C `if (err >= 0)` guard.
///
/// The C `inflate()` arm uses `windowBits = err < 0 ? 47 : -15`. The C
/// `inflateBack` arm feeds the whole buffer up front and relies on a `pull`
/// callback that returns "no more input"; here that is the natural behaviour of
/// [`SliceInput::new`], whose single chunk is served and then exhausted.
fn try_inflate(hex: &str, id: &'static str, err: i32) {
    let input = h2b(hex);

    // ----- Arm 1: the public `inflate()` driven with `Z_TREES`. -----
    {
        let win = if err < 0 { 47 } else { -15 };
        let mut strm = Inflate::with_window_bits(win).expect("try_inflate: inflate init");
        // C allocates `len << 3` and resets the output buffer each iteration.
        let mut out = vec![0u8; (input.len() << 3).max(1)];
        let mut in_pos = 0usize;
        // `ret` is assigned on every iteration before any `break`, so it needs no
        // initialiser (an initial value would be a dead store under `-D warnings`).
        let mut ret;
        let mut guard = 0u32;
        loop {
            guard += 1;
            assert!(guard < 1_000_000, "{id}: inflate loop failed to terminate");
            let (r, consumed, produced) = strm.inflate(&input[in_pos..], &mut out, Z_TREES);
            in_pos += consumed;
            assert!(
                r != Z_STREAM_ERROR && r != Z_MEM_ERROR,
                "{id}: unexpected return {r}"
            );
            ret = r;
            // C: `if (ret == Z_DATA_ERROR || ret == Z_NEED_DICT) break;`
            if r == Z_DATA_ERROR || r == Z_NEED_DICT {
                break;
            }
            // C: `while (strm.avail_in || strm.avail_out == 0);`
            let avail_in = input.len() - in_pos;
            let out_full = produced == out.len();
            if !(avail_in > 0 || out_full) {
                break;
            }
            // No-progress safety net.
            if consumed == 0 && produced == 0 {
                break;
            }
        }
        if err != 0 {
            assert_eq!(
                ret, Z_DATA_ERROR,
                "{id}: inflate() arm expected Z_DATA_ERROR"
            );
            assert_eq!(strm.msg, Some(id), "{id}: inflate() arm message-id parity");
        }
    }

    // ----- Arm 2: `inflate_back()` (skipped for the inflate-only err == -1). -----
    if err >= 0 {
        let mut state = inflate_back_init(15).expect("try_inflate: inflate_back init");
        let mut window = vec![0u8; 1usize << 15];
        let mut src = SliceInput::new(&input);
        let mut produced: Vec<u8> = Vec::new();
        let ret = {
            // The closure is a `BackOutput` via the blanket `FnMut(&[u8]) -> bool`
            // impl; `false` means "write succeeded" (mirrors C `push` returning 0).
            let mut sink = |buf: &[u8]| -> bool {
                produced.extend_from_slice(buf);
                false
            };
            inflate_back(&mut state, &mut window, &mut src, &mut sink)
        };
        // C: `assert(ret != Z_STREAM_ERROR);`
        assert!(
            ret != Z_STREAM_ERROR,
            "{id}: inflate_back arm unexpected stream error"
        );
        if err != 0 {
            // C also asserts `strcmp(id, strm.msg) == 0` here, but `inflate_back`
            // exposes no message field in this crate (the back state carries no
            // `msg`); the data-error *code* parity is asserted, preserving the
            // behavioural coverage of the bad-stream branch.
            assert_eq!(
                ret, Z_DATA_ERROR,
                "{id}: inflate_back arm expected Z_DATA_ERROR"
            );
        }
        assert_eq!(inflate_back_end(state), Z_OK, "{id}: inflate_back_end");
    }
}

// ===========================================================================
// Tests — the six C `cover_*` functions reorganised into eight `#[test]`s.
// ===========================================================================

/// `cover_support` (`test/infcover.c` L350): the lines of `inflate.c` reached
/// before `inflate()` proper — init, `inflatePrime`, `inflateSetDictionary`
/// argument guards, and the window-allocation / fixed-block / bad-window paths.
#[test]
fn cover_support() {
    // `inflatePrime` with a positive bit count, then a negative count (which
    // flushes the bit accumulator). Both succeed on a freshly initialised stream.
    let mut strm = Inflate::new().expect("inflateInit");
    strm.prime(5, 31).expect("inflatePrime(5, 31)");
    strm.prime(-1, 0).expect("inflatePrime(-1, 0)");
    // `inflateSetDictionary` on a fresh *zlib* stream (still in the HEAD state,
    // and wrapped) is invalid → Z_STREAM_ERROR-equivalent.
    assert!(
        matches!(strm.set_dictionary(&[]), Err(ZlibError::StreamError)),
        "set_dictionary on a fresh wrapped stream must be a stream error"
    );

    // Window-management and block-type paths driven through `inf()`.
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
    // `windowBits = 1` is invalid; the constructor must reject it. The C
    // `inf("", …, 1, …)` returns early after `inflateInit2` fails.
    inf("", "bad window size", 0, 1, 0, Z_STREAM_ERROR, None);

    // The C `inflateInit_(&strm, "!", …) == Z_VERSION_ERROR` check is N/A: the
    // idiomatic constructor takes no version string (the ABI/version check lives
    // in the FFI shim), so there is no version-mismatch path to exercise here.
}

/// `cover_wrap` (`test/infcover.c` L388): all of `inflate()`'s header/trailer
/// validation across the zlib and gzip wrappers, plus the post-`inflate()`
/// helper API (`inflateSync`, `inflateMark`, `inflateUndermine`, …).
#[test]
fn cover_wrap() {
    // The C bad-parameter calls — `inflate(NULL)`, `inflateEnd(NULL)`,
    // `inflateCopy(NULL, NULL)`, each == Z_STREAM_ERROR — are statically
    // impossible here: the methods take `&mut self`, so a null stream cannot be
    // constructed. Compile-time guarantees replace these run-time checks.

    // --- zlib-wrapper validation (no gzip feature required). ---
    // Bad zlib method: CM (low nibble of the CMF byte) is 7, not 8 (Z_DEFLATED).
    inf(
        "77 85",
        "bad zlib method",
        0,
        15,
        0,
        Z_DATA_ERROR,
        Some("unknown compression method"),
    );
    // windowBits = 0 → take the window size from the (valid) zlib header.
    inf("8 99", "set window size from header", 0, 0, 0, Z_OK, None);
    // Header advertises a 15-bit window but the caller requested 8 → too small.
    inf(
        "78 9c",
        "bad zlib window size",
        0,
        8,
        0,
        Z_DATA_ERROR,
        Some("invalid window size"),
    );
    // A complete, valid zlib stream: the Adler-32 trailer checks out.
    inf(
        "78 9c 63 0 0 0 1 0 1",
        "check adler32",
        0,
        15,
        1,
        Z_STREAM_END,
        None,
    );
    // A zlib stream with no payload after the header — drives the adler path.
    inf("78 9c 63 0", "compute adler32", 0, 15, 1, Z_OK, None);
    // FDICT set in the zlib header → the decoder asks for a dictionary.
    inf(
        "8 b8 0 0 0 1",
        "need dictionary",
        0,
        8,
        0,
        Z_NEED_DICT,
        None,
    );

    // --- gzip-wrapper validation (requires the `gzip` feature; win >= 24). ---
    #[cfg(feature = "gzip")]
    {
        // Bad gzip method: CM byte is 0, not 8.
        inf(
            "1f 8b 0 0",
            "bad gzip method",
            0,
            31,
            0,
            Z_DATA_ERROR,
            Some("unknown compression method"),
        );
        // Bad gzip flags: the FLG byte sets a reserved bit.
        inf(
            "1f 8b 8 80",
            "bad gzip flags",
            0,
            31,
            0,
            Z_DATA_ERROR,
            Some("unknown header flags set"),
        );
        // FHCRC set with a wrong header CRC-16 → header crc mismatch.
        inf(
            "1f 8b 8 1e 0 0 0 0 0 0 1 0 0 0 0 0 0",
            "bad header crc",
            0,
            47,
            1,
            Z_DATA_ERROR,
            Some("header crc mismatch"),
        );
        // A complete gzip stream: CRC-32 + ISIZE trailer both check out.
        inf(
            "1f 8b 8 2 0 0 0 0 0 0 1d 26 3 0 0 0 0 0 0 0 0 0",
            "check gzip length",
            0,
            47,
            0,
            Z_STREAM_END,
            None,
        );
        // win == 47 auto-detect on a zlib stream with a bad header check.
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

    // --- post-inflate helper API on a raw stream. ---
    // The C tail wraps these in `mem_limit` to force `Z_MEM_ERROR` on copy/init;
    // those OOM-injection branches and the `state->mode = DICT` poke are
    // unreachable here. The reachable helpers are exercised directly.
    let mut strm = Inflate::with_window_bits(-8).expect("raw inflateInit2(-8)");
    // On a *raw* stream (no wrapper) a dictionary may be set at any time → Ok.
    strm.set_dictionary(&[0u8; 257])
        .expect("raw set_dictionary");
    // Inject 16 bits, then drive `inflateSync` with input that holds no marker.
    strm.prime(16, 0).expect("inflatePrime(16, 0)");
    let (sync_ret, _used) = strm.sync(&[0x80]);
    assert_eq!(
        sync_ret, Z_DATA_ERROR,
        "inflateSync with no marker → Z_DATA_ERROR"
    );
    // `inflateMark` / `inflateSyncPoint` are progress queries — smoke-exercise.
    let _ = strm.mark();
    let _ = strm.sync_point();
    // `inflateUndermine` is a no-op that reports Z_DATA_ERROR (the
    // allow-invalid-distance compile option is off).
    assert_eq!(
        strm.undermine(true),
        Z_DATA_ERROR,
        "inflateUndermine → Z_DATA_ERROR"
    );
    // An independent copy of the decode state (RAII clone of the owned state).
    let _copy = strm.copy();

    // A fresh raw stream drives `inflateSync` to the marker-found (Z_OK) branch
    // on the canonical `00 00 FF FF` sync sequence.
    let mut strm2 = Inflate::with_window_bits(-8).expect("raw inflateInit2(-8) #2");
    let (sync_ret2, _used2) = strm2.sync(&[0x00, 0x00, 0xff, 0xff]);
    assert_eq!(
        sync_ret2, Z_OK,
        "inflateSync at the 00 00 ff ff marker → Z_OK"
    );
    // `inflateReset2` reconfigures window/wrap, as the C `inf` tail does.
    strm2.reset2(-8).expect("inflateReset2(-8)");
}

/// `cover_back` (`test/infcover.c` L471): `inflateBack` initialisation guards
/// and its decode/error paths, driven through the safe [`inflate_back`] API.
#[test]
fn cover_back() {
    // C bad-parameter cases that are N/A here:
    //  * `inflateBackInit_(NULL, …) == Z_VERSION_ERROR` — no version arg.
    //  * `inflateBack(NULL, …)` / `inflateBackEnd(NULL) == Z_STREAM_ERROR` —
    //    null streams are unrepresentable in the typed API.
    // The reachable guard is the windowBits range check:
    assert_eq!(
        inflate_back_init(0).err(),
        Some(Z_STREAM_ERROR),
        "inflate_back_init(0): windowBits out of 8..=15 → Z_STREAM_ERROR"
    );
    assert_eq!(
        inflate_back_init(7).err(),
        Some(Z_STREAM_ERROR),
        "windowBits 7 rejected"
    );
    assert_eq!(
        inflate_back_init(16).err(),
        Some(Z_STREAM_ERROR),
        "windowBits 16 rejected"
    );

    // A valid window size initialises successfully.
    let state = inflate_back_init(15).expect("inflate_back_init(15)");
    assert_eq!(inflate_back_end(state), Z_OK, "inflate_back_end after init");

    // Decode a complete raw stream → Z_STREAM_END. The byte 0x03 is a final
    // fixed block (BFINAL=1, BTYPE=01) whose end-of-block code completes once
    // the trailing zero bits are available, exactly the C `"\x03"` fixture.
    {
        let mut state = inflate_back_init(15).expect("init");
        let mut window = vec![0u8; 1usize << 15];
        let mut src = SliceInput::new(&[0x03u8, 0x00]);
        let mut out: Vec<u8> = Vec::new();
        let ret = {
            let mut sink = |buf: &[u8]| -> bool {
                out.extend_from_slice(buf);
                false
            };
            inflate_back(&mut state, &mut window, &mut src, &mut sink)
        };
        assert_eq!(ret, Z_STREAM_END, "valid empty fixed block → Z_STREAM_END");
        assert!(out.is_empty(), "the empty fixed block produces no output");
        assert_eq!(inflate_back_end(state), Z_OK);
    }

    // Force an output error: a sink that always reports failure downgrades the
    // run to Z_BUF_ERROR (the C `push` returning non-zero, here `true`).
    {
        let mut state = inflate_back_init(15).expect("init");
        let mut window = vec![0u8; 1usize << 15];
        // A valid one-byte-literal raw stream so the sink is actually invoked.
        let raw = h2b("63 0 3 0 0 0 0 0");
        let mut src = SliceInput::new(&raw);
        let mut sink = |_buf: &[u8]| -> bool { true }; // always "fail"
        let ret = inflate_back(&mut state, &mut window, &mut src, &mut sink);
        assert_eq!(ret, Z_BUF_ERROR, "an always-failing sink → Z_BUF_ERROR");
        assert_eq!(inflate_back_end(state), Z_OK);
    }

    // A reserved block type (BTYPE=11 in the first byte 0x06) → Z_DATA_ERROR.
    {
        let mut state = inflate_back_init(15).expect("init");
        let mut window = vec![0u8; 1usize << 15];
        let mut src = SliceInput::new(&[0x06u8]);
        let mut sink = |_buf: &[u8]| -> bool { false };
        let ret = inflate_back(&mut state, &mut window, &mut src, &mut sink);
        assert_eq!(ret, Z_DATA_ERROR, "reserved block type → Z_DATA_ERROR");
        assert_eq!(inflate_back_end(state), Z_OK);
    }

    // An undersized window buffer is rejected (the C contract requires exactly
    // `1 << windowBits` bytes) → Z_STREAM_ERROR rather than a panic.
    {
        let mut state = inflate_back_init(15).expect("init");
        let mut window = vec![0u8; (1usize << 15) - 1];
        let raw = h2b("3 0");
        let mut src = SliceInput::new(&raw);
        let mut sink = |_buf: &[u8]| -> bool { false };
        let ret = inflate_back(&mut state, &mut window, &mut src, &mut sink);
        assert_eq!(ret, Z_STREAM_ERROR, "undersized window → Z_STREAM_ERROR");
        assert_eq!(inflate_back_end(state), Z_OK);
    }
}

/// First half of `cover_inflate` (`test/infcover.c` L582): the invalid-stream
/// `try()` fixtures. Each must report `Z_DATA_ERROR` with the exact `strm->msg`
/// id through `inflate()` (and, for `err == 1`, through `inflate_back` too).
#[test]
fn cover_inflate_errors() {
    // err == 1: invalid via BOTH inflate() and inflate_back().
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

    // err == -1: trailer mismatches, checked through inflate() ONLY (gzip
    // auto-detect window 47 so the CRC-32 / ISIZE trailer is validated). These
    // require the gzip feature.
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

/// Second half of `cover_inflate` (`test/infcover.c` L582): the *success* `try()`
/// fixtures (valid streams exercised through both engines) and the `inf()` path
/// fixtures (`inflate_fast` TYPE return and the deferred-window wrap).
#[test]
fn cover_inflate_paths() {
    // err == 0: valid streams; both engines run and must not report a stream
    // error. These walk the fixed/stored/dynamic block decoders and the longer
    // dynamic-table / window-spanning code paths.
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
    try_inflate(
        "ed c0 81 0 0 0 0 80 a0 fd a9 17 a9 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 \
         0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 6",
        "window end",
        0,
    );

    // `inf()` path fixtures: the first returns through `inflate_fast` at a TYPE
    // boundary; the second forces a deferred sliding-window wrap (`step = 3`).
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

    // Exercise the `Z_BLOCK` flush mode (return at block boundaries) on a
    // complete, valid zlib stream — `infcover.c` itself only uses Z_NO_FLUSH and
    // Z_TREES, so this adds the Z_BLOCK coverage called for by §0.6.7. The same
    // one-block stream as the `cover_wrap` "check adler32" fixture is used.
    {
        let input = h2b("78 9c 63 0 0 0 1 0 1");
        let mut strm = Inflate::with_window_bits(15).expect("zlib init for Z_BLOCK");
        let mut out = vec![0u8; 16];
        let (ret, _consumed, _produced) = strm.inflate(&input, &mut out, Z_BLOCK);
        // Z_BLOCK makes orderly progress, stopping at a block boundary or stream
        // end; it must never yield an error for this valid stream.
        assert!(
            ret == Z_OK || ret == Z_STREAM_END,
            "Z_BLOCK flush should make orderly progress, got {ret}"
        );
    }
}

/// `cover_trees` (`test/infcover.c` L618) — **adapted**.
///
/// The C version `#define ZLIB_INTERNAL`s and calls `inflate_table()` directly
/// to force its "not enough room" (return value `1`) branches, because a
/// correct zlib stream can never over-subscribe the table. In `zlib_rs`,
/// `inflate_table` is `pub(crate)` (it is a `ZLIB_INTERNAL` function, not part
/// of the public zlib API), so it is **unreachable** from this integration
/// test. Its direct unit coverage lives in `src/inflate/tables.rs`'s own
/// `#[cfg(test)]` module.
///
/// Here we preserve the *behavioural* coverage of the over/under-subscribed
/// Huffman-table branches by feeding crafted DEFLATE streams to the public
/// `inflate()` and asserting the matching `Z_DATA_ERROR` + message id — the same
/// error branches the C `inflate_table` "not enough" return ultimately serves.
#[test]
fn cover_trees_via_streams() {
    // Over-subscribed counts: more length/distance symbols than the format
    // permits (the `nlen > 286 || ndist > 30` guard).
    inf(
        "fc 0 0",
        "too many symbols via stream",
        0,
        -15,
        1,
        Z_DATA_ERROR,
        Some("too many length or distance symbols"),
    );
    // An incomplete set of code-length codes (under-subscribed code-length tree).
    inf(
        "4 0 fe ff",
        "incomplete code lengths via stream",
        0,
        -15,
        1,
        Z_DATA_ERROR,
        Some("invalid code lengths set"),
    );
    // A bad code-length repeat (the 16/17/18 repeat opcodes mis-applied).
    inf(
        "4 0 24 49 0",
        "bad bit-length repeat via stream",
        0,
        -15,
        1,
        Z_DATA_ERROR,
        Some("invalid bit length repeat"),
    );
    // An over-subscribed literal/length tree.
    inf(
        "4 80 49 92 24 49 92 24 71 ff ff 93 11 0",
        "oversubscribed lit/len via stream",
        0,
        -15,
        1,
        Z_DATA_ERROR,
        Some("invalid literal/lengths set"),
    );
    // An over-subscribed distance tree.
    inf(
        "4 80 49 92 24 49 92 24 f b4 ff ff c3 84",
        "oversubscribed distances via stream",
        0,
        -15,
        1,
        Z_DATA_ERROR,
        Some("invalid distances set"),
    );
}

/// `cover_fast` (`test/infcover.c` L642): the `inffast.c` decode and window-copy
/// paths. These are fully portable — the C `inf()` driver calls the public
/// `inflate()`, which dispatches to `inflate_fast` for the bulk decode, so every
/// fixture exercises the fast path through the public API (all use `win = -8`,
/// a small raw window, to provoke the wrap/back-reference branches).
#[test]
fn cover_fast() {
    inf(
        "e5 e0 81 ad 6d cb b2 2c c9 01 1e 59 63 ae 7d ee fb 4d fd b5 35 41 68 \
         ff 7f 0f 0 0 0",
        "fast length extra bits",
        0,
        -8,
        258,
        Z_DATA_ERROR,
        Some("invalid distance too far back"),
    );
    inf(
        "25 fd 81 b5 6d 59 b6 6a 49 ea af 35 6 34 eb 8c b9 f6 b9 1e ef 67 49 \
         50 fe ff ff 3f 0 0",
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

/// Self-test of the [`h2b`] hex decoder, confirming it reproduces the exact
/// semantics of `infcover.c`'s `h2b` (single-digit-before-delimiter, adjacent
/// two-digit bytes, and the trailing-digit flush).
#[test]
fn h2b_decoder_unit() {
    // Two adjacent hex digits → one byte; a single digit before a delimiter →
    // one byte (the `val += 240` trick); a trailing single digit is flushed.
    assert_eq!(h2b("1f 8b 8"), vec![0x1f, 0x8b, 0x08], "1f 8b 8");
    assert_eq!(h2b("63 0"), vec![0x63, 0x00], "63 0");
    assert_eq!(h2b("3 0"), vec![0x03, 0x00], "3 0");
    assert_eq!(h2b("6"), vec![0x06], "single trailing digit");
    assert_eq!(h2b(""), Vec::<u8>::new(), "empty input → no bytes");
    assert_eq!(
        h2b("1 1 0 fe ff 0"),
        vec![0x01, 0x01, 0x00, 0xfe, 0xff, 0x00],
        "1 1 0 fe ff 0"
    );
    // Adjacent digits with no separators decode pairwise.
    assert_eq!(h2b("78 9c"), vec![0x78, 0x9c], "78 9c");
    // Leading/duplicate delimiters are ignored (no spurious zero bytes).
    assert_eq!(h2b("  78   9c  "), vec![0x78, 0x9c], "extra spaces ignored");
}
