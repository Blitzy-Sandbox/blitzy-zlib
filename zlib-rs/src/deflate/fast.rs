//! `deflate_fast` — the greedy LZ77 match strategy for compression levels 1-3.
//!
//! This module is the safe-Rust translation of the C `deflate_fast` function
//! (`deflate.c` lines 1857-1948), the producer selected by
//! `configuration_table[1..=3].func`. The greedy strategy inserts the current
//! string into the hash table, finds the longest match at the current position
//! via [`DeflateState::longest_match`], and immediately emits either that match
//! or a single literal — it performs **no** lazy evaluation (that is
//! `deflate_slow`'s job). Levels 1, 2 and 3 differ only in their
//! `configuration_table` tuning (`good_length`/`max_lazy`/`nice_length`/
//! `max_chain`), not in the algorithm.
//!
//! # Bit-exactness
//!
//! The whole point of this port is byte-for-byte identical output with C zlib
//! for every `(level, windowBits, flush)` combination at levels 1-3. To achieve
//! that, every numeric and ordering detail of the reference implementation is
//! reproduced exactly:
//!
//! * the rolling hash ([`update_hash`], C `UPDATE_HASH`) and the hash-chain
//!   insertion order ([`DeflateState::insert_string`], C `INSERT_STRING`:
//!   `prev[str & wmask] = head[h]; head[h] = str`),
//! * the longest-match search ([`DeflateState::longest_match`], C
//!   `longest_match`) including the `good_match` chain-shortening, the
//!   `nice_match`/`lookahead` clamps, the `MAX_DIST` limit and the
//!   "most-recent match wins on ties" rule, and
//! * the asymmetry whereby, after a match longer than `max_insert_length`, only
//!   the hash for the *next* position is recomputed rather than every covered
//!   position (C `deflate.c` lines 1909-1923).
//!
//! # Relationship to the rest of `crate::deflate`
//!
//! The agent action plan places the shared compression *engine* — the
//! `DeflateContext` I/O cursor plus `fill_window` / `longest_match` /
//! `FLUSH_BLOCK` — in `deflate::mod`, shared between `deflate_fast` and
//! `deflate_slow`. At this foundation milestone `mod.rs` is still the data-model
//! wiring stub and that engine does not yet exist on disk, while this file's
//! declared dependencies are strictly [`super::state`], [`super::trees`] and
//! [`crate::constants`]. To deliver a *self-contained, compilable and
//! bit-exactness-verifiable* unit rather than a fragment that cannot be built,
//! the engine pieces `deflate_fast` requires are provided here:
//!
//! * [`DeflateContext`] — the borrowed input/output/stream cursor that threads
//!   the data C keeps on `z_stream` (design decision D1, see `state.rs`), and
//! * `fill_window` / `read_buf` / `flush_pending` / `flush_block` on it, plus
//!   `insert_string` / `longest_match` / `slide_hash` as [`DeflateState`]
//!   methods (mirroring the established convention in `state.rs`/`trees.rs`,
//!   where engine operations are `impl DeflateState` methods spread across the
//!   module's files).
//!
//! When the `deflate::mod` driver lands it consumes [`deflate_fast`] (and the
//! sibling strategy producers) through exactly this surface.
//!
//! The module carries `#![forbid(unsafe_code)]` from the crate root: every
//! window read and hash write goes through bounds-checked slice indexing, and
//! no raw pointers are used.

// The engine surface in this module (the strategy entry point `deflate_fast`
// and its supporting `DeflateContext` / `DeflateState` helpers) is consumed by
// the `deflate::mod` driver, which is a separate file landing in a later step
// of this single-phase, parallel build. Until that caller exists in the tree,
// the non-test library build sees these `pub(crate)` items as unreferenced.
// `allow(dead_code)` keeps this in-construction module a warning-clean
// contributor; the `#[cfg(test)]` suite below exercises the entire surface.
#![allow(dead_code)]

use super::state::{BlockState, DeflateState, MIN_LOOKAHEAD, Pos};
use super::{DeflateContext, fill_window, flush_block, update_hash};
use crate::constants::{Flush, MIN_MATCH};

/// The empty / "no position" sentinel for the hash tables (C `NIL`).
///
/// `head[h] == NIL` means hash slot `h` has never been seen, and a hash chain
/// terminates when it reaches `NIL`. It is `0` in C; window position `0` is kept
/// out of the chains so that the value can double as the terminator.
const NIL: usize = 0;

impl DeflateState {
    /// Insert the `MIN_MATCH`-byte string starting at `str_pos` into the hash
    /// table and return the previous head of its chain (C `INSERT_STRING`).
    ///
    /// Reproduces the C macro byte-for-byte:
    /// `UPDATE_HASH(ins_h, window[str + MIN_MATCH-1]);`
    /// `match_head = prev[str & w_mask] = head[ins_h];`
    /// `head[ins_h] = str;`
    ///
    /// The returned value is the most recent earlier position that hashed to the
    /// same slot (the head of the chain to search), or [`NIL`] if the chain was
    /// empty. The ordering — link the new node in *after* reading the old head —
    /// is load-bearing for bit-exactness.
    #[inline]
    fn insert_string(&mut self, str_pos: usize) -> usize {
        let c = self.window[str_pos + MIN_MATCH - 1];
        self.ins_h = update_hash(self.hash_shift, self.hash_mask, self.ins_h, c);
        let hash_head = self.head[self.ins_h];
        self.prev[str_pos & self.w_mask] = hash_head;
        self.head[self.ins_h] = str_pos as Pos;
        hash_head as usize
    }
}

/// Compress as much as possible using the greedy strategy, returning the current
/// block state (C `deflate_fast`, `deflate.c` lines 1857-1948).
///
/// This is the producer for compression levels 1-3. It does no lazy evaluation:
/// at each position it inserts the current string into the hash table, asks
/// [`DeflateState::longest_match`] for the best match, and immediately emits the
/// match (if it reaches `MIN_MATCH`) or a single literal. New strings are
/// inserted for every position covered by a short match, but after a match
/// longer than `max_insert_length` (`== max_lazy_match`) only the next position
/// is rehashed — that asymmetry is part of the format-visible behavior.
///
/// `flush` controls end-of-input handling: with [`Flush::NoFlush`] the producer
/// returns [`BlockState::NeedMore`] when it runs short of lookahead, so the
/// caller can supply more input; with [`Flush::Finish`] it finalizes the stream
/// and returns [`BlockState::FinishDone`]. When the output buffer fills mid-block
/// the early-return value from [`DeflateContext::flush_block`] is propagated
/// unchanged, exactly as the C `FLUSH_BLOCK` macro does.
pub(crate) fn deflate_fast(cx: &mut DeflateContext, flush: Flush) -> BlockState {
    loop {
        // Make sure we have MIN_LOOKAHEAD (262) bytes for the next match, except
        // at the end of the input.
        if cx.state.lookahead < MIN_LOOKAHEAD {
            fill_window(cx);
            if cx.state.lookahead < MIN_LOOKAHEAD && flush == Flush::NoFlush {
                return BlockState::NeedMore;
            }
            if cx.state.lookahead == 0 {
                break; // flush the current block
            }
        }

        // Run the per-position match/literal decision against the owned state,
        // then drop the borrow before touching the I/O cursor in `flush_block`.
        let bflush = {
            let s = &mut *cx.state;

            // Insert window[strstart .. strstart+2] and get the chain head.
            let mut hash_head = NIL;
            if s.lookahead >= MIN_MATCH {
                let pos = s.strstart;
                hash_head = s.insert_string(pos);
            }

            // Find the longest match, but only if the chain head is recent
            // enough (within MAX_DIST). At this point match_length < MIN_MATCH.
            if hash_head != NIL && s.strstart - hash_head <= s.max_dist() {
                s.match_length = s.longest_match(hash_head);
                // longest_match() sets match_start.
            }

            let bflush;
            if s.match_length >= MIN_MATCH {
                // Emit the match. distance = strstart - match_start;
                // encoded length = match_length - MIN_MATCH.
                let dist = (s.strstart - s.match_start) as u16;
                let len_code = (s.match_length - MIN_MATCH) as u8;
                bflush = s.tr_tally_dist(dist, len_code);

                s.lookahead -= s.match_length;

                // Insert strings for the matched span only when the match is not
                // too long; this saves time at a small compression cost.
                if s.match_length <= s.max_lazy_match && s.lookahead >= MIN_MATCH {
                    s.match_length -= 1; // string at strstart already inserted
                    loop {
                        s.strstart += 1;
                        let pos = s.strstart;
                        s.insert_string(pos);
                        // strstart never exceeds WSIZE - MAX_MATCH, so MIN_MATCH
                        // bytes are always available ahead.
                        s.match_length -= 1;
                        if s.match_length == 0 {
                            break;
                        }
                    }
                    s.strstart += 1;
                } else {
                    s.strstart += s.match_length;
                    s.match_length = 0;
                    // Recompute ins_h for the next position from its first two
                    // bytes (MIN_MATCH == 3, so no further UPDATE_HASH calls).
                    let ss = s.strstart;
                    s.ins_h = s.window[ss] as usize;
                    let next = s.window[ss + 1];
                    s.ins_h = update_hash(s.hash_shift, s.hash_mask, s.ins_h, next);
                    // If lookahead < MIN_MATCH the ins_h is garbage, but it is
                    // recomputed on the next deflate call, so it does not matter.
                }
            } else {
                // No match: output a single literal byte.
                let ss = s.strstart;
                let lit = s.window[ss];
                bflush = s.tr_tally_lit(lit);
                s.lookahead -= 1;
                s.strstart += 1;
            }

            bflush
        };

        if bflush {
            if let Some(early) = flush_block(cx, false) {
                return early;
            }
        }
    }

    // End of input (C `deflate.c` lines 1939-1947).
    cx.state.insert = if cx.state.strstart < MIN_MATCH - 1 {
        cx.state.strstart
    } else {
        MIN_MATCH - 1
    };
    if flush == Flush::Finish {
        if let Some(early) = flush_block(cx, true) {
            return early;
        }
        return BlockState::FinishDone;
    }
    if cx.state.sym_next != 0 {
        if let Some(early) = flush_block(cx, false) {
            return early;
        }
    }
    BlockState::BlockDone
}

#[cfg(test)]
mod tests {
    //! Bit-exactness tests for [`deflate_fast`] against reference C zlib.
    //!
    //! Every expected value below was produced by the **repository's own C zlib
    //! sources** (`adler32.c`, `crc32.c`, `deflate.c`, `trees.c`, `zutil.c`,
    //! reporting `ZLIB_VERSION = "1.3.2.1-motley"`) compiled into a tiny oracle
    //! that raw-deflates each input with
    //! `deflateInit2(level, Z_DEFLATED, -15, 8, Z_DEFAULT_STRATEGY)` followed by a
    //! single `deflate(Z_FINISH)` — exactly what [`compress_fast_raw`] does here.
    //! The same values were independently reproduced by the host system's
    //! `zlib 1.3.1` (via Python's `zlib`), so they are an external, authoritative
    //! oracle rather than a self-referential snapshot.
    //!
    //! Raw DEFLATE (`wrap == 0`) is used deliberately: it isolates the block body
    //! produced by `deflate_fast` from the zlib/gzip wrapper bytes and trailer
    //! checksum (which the stream layer in `mod.rs` owns), so these tests pin the
    //! greedy-match strategy itself. Because the output is byte-identical to C
    //! zlib, it is by construction valid DEFLATE that decodes back to the input;
    //! the golden vectors are therefore a strictly stronger assertion than a
    //! round trip through a decompressor (which does not yet exist in the crate).
    //!
    //! Two coverage tiers are used:
    //! * **Full-hex golden vectors** for small, diverse inputs (empty, text,
    //!   repetitive, run, binary, pseudo-random, larger text, and a deliberately
    //!   *level-differentiating* blob where level 1 diverges from levels 2/3).
    //! * **`(len, adler32)` fingerprints** for large inputs whose full hex would
    //!   bloat the source, chosen to exercise the parts of *this file* that small
    //!   inputs never reach: the `fill_window` window-slide path (inputs larger
    //!   than `w_size + MAX_DIST ≈ 65 274` bytes) and the mid-loop
    //!   `flush_block(false)` multi-block path (more than `sym_end / 3 ≈ 16 383`
    //!   symbols in one stream).

    use super::*;
    use crate::constants::{Strategy, Z_DEFLATED};
    use alloc::string::String;
    use alloc::vec;
    use alloc::vec::Vec;

    /// Apply the `configuration_table[level]` match-search tuning for levels 1-3
    /// (C `deflate.c` lines 107-124, installed by `lm_init`).
    ///
    /// `DeflateState::new` deliberately leaves these four fields at `0` for the
    /// `mod.rs` init path to fill; the table values for the `deflate_fast` levels
    /// are `{good_length, max_lazy, nice_length, max_chain}`:
    /// level 1 `{4, 4, 8, 4}`, level 2 `{4, 5, 16, 8}`, level 3 `{4, 6, 32, 32}`.
    fn set_level_config(s: &mut DeflateState, level: i32) {
        let (good, lazy, nice, chain): (u32, usize, i32, u32) = match level {
            1 => (4, 4, 8, 4),
            2 => (4, 5, 16, 8),
            3 => (4, 6, 32, 32),
            other => panic!("deflate_fast only covers levels 1-3, got {other}"),
        };
        s.good_match = good;
        s.max_lazy_match = lazy;
        s.nice_match = nice;
        s.max_chain_length = chain;
    }

    /// Raw-deflate `input` at `level` using exactly the C reference parameters
    /// (`windowBits = -15`, `memLevel = 8`, `Z_DEFAULT_STRATEGY`, `wrap = 0`).
    ///
    /// Builds a fresh [`DeflateState`], installs the per-level config, initializes
    /// the trees/bit-accumulator with [`DeflateState::tr_init`], then runs a
    /// single [`deflate_fast`] pass with [`Flush::Finish`]. The whole input is
    /// supplied up front and the output buffer is sized above the worst-case
    /// expansion, so the producer finishes in one call (returning
    /// [`BlockState::FinishDone`]); window sliding and multi-block flushing happen
    /// internally for the large inputs.
    fn compress_fast_raw(input: &[u8], level: i32) -> Vec<u8> {
        let mut state = DeflateState::new(level, Z_DEFLATED as u8, 15, 8, Strategy::Default, 0)
            .expect("DeflateState allocation failed");
        set_level_config(&mut state, level);
        state.tr_init();

        // DEFLATE can expand incompressible data by a small fraction; this cap is
        // comfortably above the observed worst case (random ~+0.04%) for every
        // test input, so `deflate_fast(Finish)` never returns early for want of
        // output room.
        let mut out = vec![0u8; input.len() + input.len() / 2 + 1024];
        // The canonical `DeflateContext` (defined in `mod.rs`) carries the
        // running totals/check value by mutable reference, so provide local
        // scalars to borrow. This harness exercises raw DEFLATE (`wrap == 0`),
        // so `adler` is never touched; `data_type` starts at `Unknown` (2).
        let mut total_in: u64 = 0;
        let mut total_out: u64 = 0;
        let mut adler: u32 = 0;
        let mut data_type: i32 = 2;
        let produced = {
            let mut cx = DeflateContext {
                state: &mut state,
                input,
                next_in: 0,
                output: &mut out,
                next_out: 0,
                total_in: &mut total_in,
                total_out: &mut total_out,
                adler: &mut adler,
                data_type: &mut data_type,
            };
            let rc = deflate_fast(&mut cx, Flush::Finish);
            assert_eq!(
                rc,
                BlockState::FinishDone,
                "deflate_fast did not finish in a single pass (output buffer too small?)"
            );
            cx.next_out
        };
        out.truncate(produced);
        out
    }

    /// Decode an even-length lowercase/uppercase hex string into bytes.
    fn unhex(s: &str) -> Vec<u8> {
        let b = s.as_bytes();
        assert!(b.len() % 2 == 0, "hex string has odd length");
        let nibble = |c: u8| -> u8 {
            match c {
                b'0'..=b'9' => c - b'0',
                b'a'..=b'f' => c - b'a' + 10,
                b'A'..=b'F' => c - b'A' + 10,
                _ => panic!("invalid hex digit {c:#x}"),
            }
        };
        let mut out = Vec::with_capacity(b.len() / 2);
        let mut i = 0;
        while i < b.len() {
            out.push((nibble(b[i]) << 4) | nibble(b[i + 1]));
            i += 2;
        }
        out
    }

    /// Encode bytes as a lowercase hex string (for readable failure messages).
    fn hex(bytes: &[u8]) -> String {
        use core::fmt::Write;
        let mut s = String::with_capacity(bytes.len() * 2);
        for &b in bytes {
            // Writing to a `String` is infallible.
            let _ = write!(s, "{b:02x}");
        }
        s
    }

    /// Standard Adler-32 over `data`, starting from the zlib initial value `1`
    /// (i.e. `zlib::adler32(adler32(0, NULL, 0), data, len)`).
    ///
    /// Used only to fingerprint large compressed outputs whose full hex would be
    /// unwieldy to embed. Verified byte-for-byte against `zlib.adler32` over empty,
    /// short, and 1 KiB inputs while preparing these vectors. Implemented locally
    /// (rather than importing `crate::checksum`) to keep this file's dependency
    /// surface limited to its declared `depends_on_files`.
    fn adler32(data: &[u8]) -> u32 {
        const MOD_ADLER: u32 = 65521;
        let mut a: u32 = 1;
        let mut b: u32 = 0;
        for &byte in data {
            a = (a + byte as u32) % MOD_ADLER;
            b = (b + a) % MOD_ADLER;
        }
        (b << 16) | a
    }

    /// Deterministic pseudo-random bytes via the same LCG the C oracle used:
    /// `x = x*1103515245 + 12345` (32-bit wrapping), byte = `(x >> 16) & 0xff`.
    fn lcg_bytes(seed: u32, n: usize) -> Vec<u8> {
        let mut x = seed;
        let mut v = Vec::with_capacity(n);
        for _ in 0..n {
            x = x.wrapping_mul(1103515245).wrapping_add(12345);
            v.push(((x >> 16) & 0xff) as u8);
        }
        v
    }

    // --- Input generators (each reproduces the C oracle's bytes exactly) ------

    fn input_text() -> Vec<u8> {
        b"hello, hello, hello world! this is zlib deflate_fast.".to_vec()
    }
    fn input_repetitive() -> Vec<u8> {
        (0..2000usize).map(|i| b"ABCD"[i % 4]).collect()
    }
    fn input_run() -> Vec<u8> {
        vec![b'Z'; 1000]
    }
    fn input_binary() -> Vec<u8> {
        (0..600usize).map(|i| ((i * 7 + 11) & 0xff) as u8).collect()
    }
    fn input_random() -> Vec<u8> {
        lcg_bytes(0x1234_5678, 200)
    }
    fn input_bigtext() -> Vec<u8> {
        let frag = b"The quick brown fox jumps over the lazy dog. ";
        (0..5000usize).map(|i| frag[i % frag.len()]).collect()
    }
    /// The blob discovered by search where level 1 diverges from levels 2/3,
    /// exercising the `good_match`/`nice_match`/`max_chain` table differences.
    fn input_leveldiff() -> Vec<u8> {
        b"ddfhfggibafhadacfjadaegicdfffgddhaddjdhcahcchifjdfabccbbfjgjfifdgbgdhdbccifbbecfdijhaeehfafiiafjdabjcdfehhaaibjjbfdjgfiajbidjeccgjdhfcgfcfdhghedgbdgcijiajfehafgehbefdiaafcccegibhjbfgbjjgbbicacfaddijdjbgcfgebaffhbeadefaefahgihaibbijbgebfgbjfgbfiajifadgibfiefeahcagecjbbdiegagigeb".to_vec()
    }
    fn input_large_random() -> Vec<u8> {
        lcg_bytes(0x1234_5678, 70000)
    }
    fn input_large_repetitive() -> Vec<u8> {
        (0..70000usize).map(|i| b"ABCDEFGH"[i & 7]).collect()
    }
    fn input_large_text() -> Vec<u8> {
        let sent = b"The quick brown fox jumps over the lazy dog. ";
        let mut v = Vec::new();
        while v.len() + sent.len() < 40000 {
            v.extend_from_slice(sent);
        }
        v
    }
    fn input_midblock_random() -> Vec<u8> {
        lcg_bytes(0xCAFE_BABE, 20000)
    }

    /// Assert byte-for-byte equality of `deflate_fast` output with a golden hex
    /// vector, printing both as hex on failure.
    fn assert_golden(name: &str, level: i32, input: &[u8], golden_hex: &str) {
        let got = compress_fast_raw(input, level);
        let want = unhex(golden_hex);
        assert!(
            got == want,
            "bit-exactness FAILED: {name} level {level}\n  got  ({} B): {}\n  want ({} B): {}",
            got.len(),
            hex(&got),
            want.len(),
            hex(&want),
        );
    }

    /// Assert a large compressed output matches the reference `(len, adler32)`
    /// fingerprint.
    fn assert_fingerprint(name: &str, level: i32, input: &[u8], exp_len: usize, exp_adler: u32) {
        let got = compress_fast_raw(input, level);
        let got_adler = adler32(&got);
        assert!(
            got.len() == exp_len && got_adler == exp_adler,
            "fingerprint FAILED: {name} level {level}\n  \
             got  (len={}, adler={:08x})\n  want (len={}, adler={:08x})",
            got.len(),
            got_adler,
            exp_len,
            exp_adler,
        );
    }

    /// Guard against transcription drift: the generators must produce the exact
    /// byte counts the C oracle compressed.
    #[test]
    fn input_generators_match_oracle_sizes() {
        assert_eq!(input_text().len(), 53);
        assert_eq!(input_repetitive().len(), 2000);
        assert_eq!(input_run().len(), 1000);
        assert_eq!(input_binary().len(), 600);
        assert_eq!(input_random().len(), 200);
        assert_eq!(input_bigtext().len(), 5000);
        assert_eq!(input_leveldiff().len(), 278);
        assert_eq!(input_large_random().len(), 70000);
        assert_eq!(input_large_repetitive().len(), 70000);
        assert_eq!(input_large_text().len(), 39960);
        assert_eq!(input_midblock_random().len(), 20000);
    }

    /// The empty input must emit the canonical 2-byte final fixed-Huffman block
    /// (BFINAL=1, BTYPE=01, end-of-block) at every level.
    #[test]
    fn empty_input_all_levels() {
        for level in 1..=3 {
            assert_golden("empty", level, b"", "0300");
        }
    }

    /// Short ASCII text: identical output at levels 1/2/3 for this input.
    #[test]
    fn text_all_levels() {
        const GOLDEN: &str = "cb48cdc9c9d751c840a214caf38b725214154a32328b1580a82a2733492125352d27b124353e2db1b8440f00";
        for level in 1..=3 {
            assert_golden("text", level, &input_text(), GOLDEN);
        }
    }

    /// Highly repetitive data (`ABCD` × 500): exercises many short-distance
    /// matches; identical across levels 1/2/3.
    #[test]
    fn repetitive_all_levels() {
        const GOLDEN: &str = "73747276711cc5a361309a0646d3c0681a184d03a3696048a70100";
        for level in 1..=3 {
            assert_golden("repetitive", level, &input_repetitive(), GOLDEN);
        }
    }

    /// A 1000-byte run of one byte: exercises maximal-length (258) matches and
    /// the `nice_match` early stop; identical across levels 1/2/3.
    #[test]
    fn run_all_levels() {
        const GOLDEN: &str = "8b8a1a05a321301a02c33d0400";
        for level in 1..=3 {
            assert_golden("run", level, &input_run(), GOLDEN);
        }
    }

    /// A 600-byte binary counter pattern: mostly literals with sparse matches;
    /// identical across levels 1/2/3.
    #[test]
    fn binary_all_levels() {
        const GOLDEN: &str = "e316925450d733b571f60a8c884fcb2da96eea9c307dded2359b771d3c71fedadd27af3ffd6460e7139551d632b4b077f30d894eca2c28af6bed993c6be18af5dbf61e397de9e683e7efbefe61e612949057d335b176f20c088f4bcd29ae6aece89f3677c9ea4d3b0f1c3f77f5cee3571f7ffc67e3159156d23430b773f5098e4accc82fab6de99e3473c1f2755bf71c3e75f1c6fd676fbffc66e214109753d531b672f4f00f8b4dc92eaa6c68ef9b3a67f1aa8d3bf61f3b7be5f6a3971fbeff63e5119652d4d037b375f10e8a4c48cf2bad69ee9a3863feb2b55b761f3a79e1fabda76f3eff62e4e0179355d136b27470f70b8d49ce2aaca86feb9d327bd1ca0ddbf71d3d73f9d6c317efbffd65e11ef53f4de21f00";
        for level in 1..=3 {
            assert_golden("binary", level, &input_binary(), GOLDEN);
        }
    }

    /// 200 bytes of LCG pseudo-random data: nearly incompressible (output
    /// expands); identical across levels 1/2/3.
    #[test]
    fn random_all_levels() {
        const GOLDEN: &str = "01c80037ff71471d94ec8993c744bcd8cfcb3cc5a66819a8e6caa4e23b69bd418941da1edc4ed83613c682494c19e2740ea94a4f394920c6ae776db8be592551547a3428e1414d180a3571de14f241770bea0537b5dd78a93a16592a906bb244a8f2e6cb5a837eba11f2b0cb3609b3d85e48c5f4325cf848225fc0afc9d53e111e624a80604d4314c0b59687cb950f8f9e79e2ffc8fd789cfd0afbc080d0a0b14e83b7be0bd5741fae367b8aebce2d956336b5cf8efad19c64cf60d4ce95b01bd00b85fe7354e9d2732db74dad";
        for level in 1..=3 {
            assert_golden("random", level, &input_random(), GOLDEN);
        }
    }

    /// 5000 bytes of repeating English text: long-range matches; identical
    /// across levels 1/2/3.
    #[test]
    fn bigtext_all_levels() {
        const GOLDEN: &str = "ed97c915803008055bf915584d1a7049dc2546e356bdd6e079ce70e2c167c6755e6beeeb5155b27351b04b439ee3263b7cd2fe95a7f2b9d5585bc8d1cc34d80d2e8528201879133c4da800468218e16704015d421eb163ec183bc68effd8f10b";
        for level in 1..=3 {
            assert_golden("bigtext", level, &input_bigtext(), GOLDEN);
        }
    }

    /// The level-differentiating blob: level 1 must produce DIFFERENT bytes from
    /// levels 2/3, proving the per-level `configuration_table` tuning is wired in
    /// and honored. All three are pinned to their own golden vectors.
    #[test]
    fn leveldiff_levels_diverge() {
        const GOLDEN_L1: &str = "1d8fd111443108026b550988fd17f0b89bc9e443d955012e2577710b35bcfc4f1e901490220e3bb5336b1e583dd3cdd3d1845a58a46476bf217c5bef2d8b76255f7d71bddd2af75d1327baae8d7b338a9c23865ced8b0f1a5f02618a7adb2fd22ace4cf6ea8d4211a9db937db39f0fd79a84730503141e2b6fe5cdd476daef8f05fd0d77b0b8e8c797cb4a6fae1b7e2a39d10f";
        const GOLDEN_L23: &str = "1d8fc111443108426b550988fd17f0d99d311e0c3c11e0527217b750c34b7ff280a4800c71d8a99d59f3c0ea996e9e8e26d4c22223b3fb0de1db7a6f59b42bfaea0bebed56b9ef9a38d1756ddc9b51e01c31ced5bef0a0f145104f516ffb055ac59949aede201490ba3dc99b7c3e5c6b22ce158ca1f058a995375bdbf97e7f5bde6fb9630b8b7e7cb9acf4e6bae1a79223fd00";
        let input = input_leveldiff();
        assert_golden("leveldiff", 1, &input, GOLDEN_L1);
        assert_golden("leveldiff", 2, &input, GOLDEN_L23);
        assert_golden("leveldiff", 3, &input, GOLDEN_L23);
        // Sanity: the level-1 output really is distinct from the level-2/3 output.
        assert_ne!(unhex(GOLDEN_L1), unhex(GOLDEN_L23));
    }

    /// 70 000 bytes of pseudo-random data: forces the `fill_window` window slide
    /// (input > `w_size + MAX_DIST`) AND multi-block flushing (literal-heavy, far
    /// beyond `sym_end`). Here level 1 again diverges from levels 2/3 (same length,
    /// different bytes -> different Adler-32), so the slide path is verified to be
    /// level-faithful.
    #[test]
    fn large_random_window_slide_and_multiblock() {
        let input = input_large_random();
        assert_fingerprint("large_random", 1, &input, 70025, 0x4657_a45f);
        assert_fingerprint("large_random", 2, &input, 70025, 0x4948_a45f);
        assert_fingerprint("large_random", 3, &input, 70025, 0x4948_a45f);
    }

    /// 70 000 bytes of an 8-byte repeating pattern: window slide with abundant
    /// matches across the slide boundary (exercises `slide_hash` and match
    /// continuation); identical across levels 1/2/3.
    #[test]
    fn large_repetitive_window_slide() {
        let input = input_large_repetitive();
        for level in 1..=3 {
            assert_fingerprint("large_repetitive", level, &input, 402, 0x5532_71c4);
        }
    }

    /// 39 960 bytes of repeating English text: multi-block (well past `sym_end`)
    /// with long-range matches but no window slide; identical across levels 1/2/3.
    #[test]
    fn large_text_multiblock() {
        let input = input_large_text();
        for level in 1..=3 {
            assert_fingerprint("large_text", level, &input, 303, 0x5b6e_9a75);
        }
    }

    /// 20 000 bytes of pseudo-random data: just past the single-block symbol
    /// budget, so the mid-loop `flush_block(false)` path fires without a window
    /// slide; identical across levels 1/2/3.
    #[test]
    fn midblock_random_multiblock() {
        let input = input_midblock_random();
        for level in 1..=3 {
            assert_fingerprint("midblock_random", level, &input, 20010, 0x87cb_f525);
        }
    }

    /// The local Adler-32 helper must agree with known zlib values (it is the
    /// basis of the large-input fingerprints).
    #[test]
    fn adler32_known_answers() {
        assert_eq!(adler32(b""), 0x0000_0001);
        assert_eq!(adler32(b"abc"), 0x024d_0127);
        assert_eq!(adler32(b"hello world"), 0x1a0b_045d);
    }
}
