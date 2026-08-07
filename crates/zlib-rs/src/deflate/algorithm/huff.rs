//! The `Z_HUFFMAN_ONLY` compressor: every input byte becomes a Huffman literal.
//!
//! This implements `deflate_huff` (`deflate.c` L2155-L2185), the last function in the
//! reference implementation's encoder and the simplest of its five compressors. The comment
//! above it states the whole of its contract (L2151-L2154).
//!
//! No match finding, no hash-chain insertion, no lazy evaluation: the compressor walks the
//! window one byte at a time and hands each byte to the literal tally, so the only
//! redundancy the output exploits is the entropy coding that
//! [`crate::trees::_tr_flush_block`] performs afterwards. `doc/algorithm.txt` describes the
//! machinery this strategy declines to use; what is left is the LZ77 stage reduced to
//! nothing.
//!
//! # How this compressor is reached
//!
//! Through the strategy, never through the level table.
//! `configuration_table` (`deflate.c` L112-L124) names only `deflate_stored`,
//! `deflate_fast` and `deflate_slow`, so `deflate/config_table.rs` cannot select this
//! function. The dispatch chain in `deflate()` is what does (L1217-L1220).
//!
//! Level 0 is tested first, so `Z_HUFFMAN_ONLY` at level 0 stores rather than Huffman-codes;
//! [`crate::deflate::algorithm::CompressFunc::select`] preserves that precedence.
//!
//! # ⚠ THIS FILE IS PART OF THE OUTPUT CONTRACT ⚠
//!
//! A literal-only encoder looks as though it has no decisions left to get wrong. It has
//! three, and each one is visible in the compressed bytes:
//!
//! * **Where block boundaries fall.** `bflush` is the symbol buffer filling up, and it is
//!   acted on *after* `lookahead` and `strstart` have moved (L2173-L2175). Flushing one
//!   literal earlier or later changes which symbols share a block, which changes the
//!   frequency counts, which changes every Huffman code in it.
//! * **`s->insert = 0`** (L2177), unconditionally. `deflate_fast` (L1940) and `deflate_slow`
//!   (L2068) instead keep `strstart < MIN_MATCH - 1 ? strstart : MIN_MATCH - 1`. Copying
//!   their conditional form here would leave `insert` non-zero, and `fill_window` would then
//!   prime hash chains from data this strategy never indexed (`deflate.c` L317-L343) --
//!   changing which matches a later `deflateParams()` switch away from `Z_HUFFMAN_ONLY`
//!   finds, and therefore the bytes it emits. This is what the reference's "It will be
//!   regenerated if this run of deflate switches away from Huffman" is about.
//! * **`s->match_length = 0`** on every iteration (L2170). Not dead: `deflate()` and
//!   `deflateParams` both observe `match_length`, and `deflate_fast`/`deflate_slow` read it
//!   on entry, so a stale value survives a strategy switch.
//!
//! The `Tracevv` at L2171 is `ZLIB_DEBUG`-only and has no counterpart here -- no logging, no
//! tracing state. `FASTEST` and `LIT_MEM` are not implemented and must never become Cargo
//! features of this crate: both change the compressed output, and a build knob that changes
//! the output is indistinguishable from a bug in a library whose contract is byte-identical
//! compression.
//!
//! # The guard shape is specific to this compressor
//!
//! The five strategies do **not** share one `fill_window` condition, and harmonising them
//! would be a behaviour change:
//!
//! | Compressor | Refill condition | `deflate.c` |
//! |---|---|---|
//! | `deflate_stored` | never calls `fill_window` | L1668 |
//! | `deflate_fast` | `lookahead < MIN_LOOKAHEAD` | L1867 |
//! | `deflate_slow` | `lookahead < MIN_LOOKAHEAD` | L1967 |
//! | `deflate_rle` | `lookahead <= MAX_MATCH` | L2094 |
//! | `deflate_huff` | `lookahead == 0`, with a **nested** re-test | L2160-L2167 |
//!
//! One literal is all this compressor ever needs, so it refills only when it has none, and
//! `MIN_LOOKAHEAD` (`deflate.h` L296) plays no part. The nesting matters too: the
//! `Z_NO_FLUSH` test and the `break` live *inside* the outer `if`, so they are reached only
//! on the iteration that just tried and failed to refill -- there is no separate
//! `if (lookahead == 0) break;` sibling of the kind `deflate_fast` has.

// The four dependencies, and why each is the only way to spell what C spells directly:
//
// * `BlockState`, `Flush`, `StreamCursors` and `flush_block!` are the shared dispatch
//   vocabulary of `deflate/algorithm.rs`. `StreamCursors` stands in for `s->strm`, which
//   this implementation cannot hold as a field, and `flush_block!` is `FLUSH_BLOCK` including the
//   early `return` that C's macro performs on behalf of its caller.
// * `Allocator` and `DeflateState` come as a pair: `DeflateState<'a, A: Allocator<'a>>`
//   cannot be named without the bound.
// * `fill_window` is `deflate.c` L1661's declaration, reached at L2161.
// * `_tr_tally` is the literal tally. `deflate.h` L357-L364's `_tr_tally_lit` macro and
//   this function are the same computation -- L378-L380 defines the macro *as* a call to it
//   -- so a literal is `_tr_tally(state, 0, byte)`. See the tally comment in the body.
use crate::deflate::algorithm::{flush_block, BlockState, Flush, StreamCursors};
use crate::deflate::state::{Allocator, DeflateState};
use crate::deflate::window::fill_window;
use crate::trees::_tr_tally;

/// Compresses the available input as Huffman literals, one per byte.
///
/// Mirrors `deflate_huff` (`deflate.c` L2155-L2185), the `Z_HUFFMAN_ONLY` strategy: no match
/// finding and no hash table, so every byte of the window is emitted as a literal and only
/// the entropy coder compresses.
///
/// # Arguments
///
/// * `state` -- C's `deflate_state *s`. `match_length`, `window.lookahead`,
///   `window.strstart`, `window.insert` and the symbol buffer behind `pending` are all
///   written here.
/// * `cursors` -- the caller's `z_stream` buffers and counters, which C reaches through
///   `s->strm`. `fill_window` consumes from `cursors.input`, and every `FLUSH_BLOCK` writes
///   to `cursors.output`.
/// * `flush` -- the caller's flush mode. Only `Z_NO_FLUSH` and `Z_FINISH` are distinguished:
///   the first is the one mode that asks for more input rather than closing the block, and
///   the second is the one that sets `BFINAL`. `Z_TREES` cannot arrive, because `deflate()`
///   rejects it before dispatching (L985).
///
/// # Returns
///
/// * [`BlockState::NeedMore`] -- no literal was available and `flush` was `Z_NO_FLUSH`, or a
///   `FLUSH_BLOCK` filled the output buffer while `last` was false.
/// * [`BlockState::FinishStarted`] -- the final block was written but did not fit in the
///   output buffer; produced by `flush_block!` with `last` true, not returned literally
///   here.
/// * [`BlockState::FinishDone`] -- `flush` was `Z_FINISH` and the final block is out.
/// * [`BlockState::BlockDone`] -- the block was closed for any other flush mode.
///
/// # Termination
///
/// Every iteration either consumes one byte of `lookahead` or leaves through the guard, and
/// `fill_window` only ever adds bytes the caller supplied, so the loop runs at most once per
/// input byte plus once. Nothing here can spin on an unchanged state.
// `_tr_tally` keeps the underscore-prefixed C spelling of `deflate.h` L312 for oracle
// traceability, which `clippy::pedantic` flags at the call site. The same relaxation, for the
// same reason, appears in `deflate/algorithm.rs` and `trees/mod.rs`.
// MSRV guard: `unknown_lints` comes first because `clippy::used_underscore_items` postdates the
// declared 1.80 floor, where the lint NAME is itself an `unknown_lints` error under `-D warnings`.
// Allowing `unknown_lints` in the same list makes the attribute inert on 1.80 and effective on
// current stable. Do not drop it while the floor is 1.80.
#[allow(unknown_lints, clippy::used_underscore_items)]
pub(crate) fn deflate_huff<'a, A: Allocator<'a>>(
    state: &mut DeflateState<'a, A>,
    cursors: &mut StreamCursors<'_, '_>,
    flush: Flush,
) -> BlockState {
    // `for (;;) {`  (L2158)
    loop {
        // "Make sure that we have a literal to write." (L2159-L2167)
        //
        // `lookahead == 0`, not `< MIN_LOOKAHEAD`: one literal is the whole requirement. See
        // the module documentation for the table of all five refill conditions.
        if state.window.lookahead == 0 {
            // `fill_window(s);`  (L2161)
            //
            // Its IN assertion, `Assert(s->lookahead < MIN_LOOKAHEAD, "already enough
            // lookahead")` (`deflate.c` L257), holds a fortiori: `lookahead` is zero here.
            fill_window(
                state,
                &mut cursors.input,
                &mut cursors.check,
                &mut cursors.total_in,
            );

            // `if (s->lookahead == 0) {`  (L2162) -- nested, so this is reached only on an
            // iteration that just tried to refill and got nothing.
            if state.window.lookahead == 0 {
                // `if (flush == Z_NO_FLUSH) return need_more;`  (L2163-L2164)
                //
                // The caller has more to send, so the partly built block stays open and
                // unflushed; emitting it now would insert a block boundary the reference
                // does not have.
                if flush == Flush::NoFlush {
                    return BlockState::NeedMore;
                }

                // `break;      /* flush the current block */`  (L2165)
                break;
            }
        }

        // `s->match_length = 0;`  (L2170)
        //
        // Every iteration, before the tally, exactly as written. This compressor finds no
        // matches, and saying so is observable: `deflate()` reports `match_length` through
        // `deflateParams`, and `deflate_fast`/`deflate_slow` read it on entry, so a value
        // left over from a previous strategy would survive a switch.
        state.match_length = 0;

        // `Tracevv((stderr,"%c", s->window[s->strstart]));`  (L2171) is `ZLIB_DEBUG`-only
        // and has no counterpart.
        //
        // `s->window[s->strstart]`  (L2172), bounds-checked instead of indexed raw.
        //
        // The index is always valid: the guard above leaves `lookahead >= 1`, and
        // `fill_window` exits having asserted `strstart <= window_size - MIN_LOOKAHEAD`
        // (`deflate.c` L374-L375), so `strstart < window_size`. The impossible [`None`] is
        // answered exactly as the guard one line above answers "no literal to write",
        // because that is what it would mean -- `Z_NO_FLUSH` asks for more input, anything
        // else closes the block. It must not become [`BlockState::BlockDone`] under
        // `Z_NO_FLUSH`: `deflate()` answers `block_done` by emitting a flush terminator
        // (L1238-L1245), which it does only for a real flush.
        let Some(literal) = state.window.byte(state.window.strstart) else {
            if flush == Flush::NoFlush {
                return BlockState::NeedMore;
            }
            break;
        };

        // `_tr_tally_lit(s, s->window[s->strstart], bflush);`  (L2172)
        //
        // `LIT_MEM` is commented out at `deflate.h` L28, so the live macro is the `#else`
        // branch at L357-L364.
        //
        // A distance of zero is how the three-byte symbol record spells "literal", so
        // `_tr_tally(state, 0, literal)` performs precisely those five steps: the two zero
        // bytes and `cc` into `sym_buf`, `dyn_ltree[cc].Freq++`, and
        // `sym_next == sym_end` as the return value. The `LIT_MEM` variant at L339-L345,
        // with its separate `d_buf` and `l_buf`, is not implemented; neither is the `ZLIB_DEBUG`
        // fallback at L378-L380, which routes to the same function anyway.
        let bflush = _tr_tally(state, 0, literal);

        // `s->lookahead--;`  (L2173)
        //
        // `saturating_sub` cannot saturate -- the guard established `lookahead >= 1` and
        // nothing above decrements it -- and keeps this function free of the arithmetic
        // that panics on a debug build.
        state.window.lookahead = state.window.lookahead.saturating_sub(1);

        // `s->strstart++;`  (L2174)
        //
        // Strictly after the tally and after `lookahead`, as written. `saturating_add`
        // likewise cannot saturate: `strstart` stays below `window_size`, at most 65536.
        state.window.strstart = state.window.strstart.saturating_add(1);

        // `if (bflush) FLUSH_BLOCK(s, 0);`  (L2175)
        //
        // After both cursors have moved, so a premature exit here resumes on the next
        // literal rather than re-emitting this one. `flush_block!` performs C's macro in
        // full, including the `return (last) ? finish_started : need_more` that fires when
        // the flush filled the output buffer (L1644).
        if bflush {
            flush_block!(state, cursors, false);
        }
    }

    // `s->insert = 0;`  (L2177)
    //
    // ★ Plain zero, unconditionally. Not the `strstart < MIN_MATCH-1 ? strstart :
    // MIN_MATCH-1` form of `deflate_fast` (L1940) and `deflate_slow` (L2068): this strategy
    // inserted nothing into the hash chains, so there is nothing pending to insert. See the
    // module documentation for what copying their form would break.
    state.window.insert = 0;

    // `if (flush == Z_FINISH) { FLUSH_BLOCK(s, 1); return finish_done; }`  (L2178-L2181)
    //
    // `last` is true, which sets `BFINAL` on the block and makes `_tr_flush_block` align the
    // bit stream to a byte boundary through `bi_windup`.
    if flush == Flush::Finish {
        flush_block!(state, cursors, true);
        return BlockState::FinishDone;
    }

    // `if (s->sym_next) FLUSH_BLOCK(s, 0);`  (L2182-L2183)
    //
    // Only when the block holds symbols. An empty block is not emitted here: the driver
    // decides what a flush mode needs, with `_tr_align` for `Z_PARTIAL_FLUSH`, an empty
    // stored block for `Z_SYNC_FLUSH` and `Z_FULL_FLUSH`, and nothing for `Z_BLOCK`
    // (L1238-L1245). `sym_next` is the symbol buffer's byte cursor, so testing it against
    // zero is C's truth test on the same value.
    if state.pending.sym_next() != 0 {
        flush_block!(state, cursors, false);
    }

    // `return block_done;`  (L2184)
    BlockState::BlockDone
}

#[cfg(test)]
// `unwrap`, `expect` and `panic` are already relaxed inside tests by `clippy.toml`;
// `used_underscore_items` is not, and these tests call `_tr_init`, whose name is the C
// spelling of `deflate.h` L311. The same relaxation, for the same reason, appears in
// `deflate/algorithm.rs` and `trees/mod.rs`.
// Fixture indexing: every index below is a literal into a fixture this module just built,
// so each one is provably in range. `clippy::indexing_slicing` is denied workspace-wide and
// is relaxed HERE ONLY, on the test module -- not through a clippy.toml key, which would be a
// field the 1.80 floor does not recognise and would abort the whole lint run.
#[allow(clippy::indexing_slicing)]
// MSRV guard: `unknown_lints` comes first because `clippy::used_underscore_items` postdates the
// declared 1.80 floor, where the lint NAME is itself an `unknown_lints` error under `-D warnings`.
// Allowing `unknown_lints` in the same list makes the attribute inert on 1.80 and effective on
// current stable. Do not drop it while the floor is 1.80.
#[allow(unknown_lints, clippy::used_underscore_items)]
mod tests {
    use super::deflate_huff;
    use crate::config::Z_UNKNOWN;
    use crate::crc32::crc32;
    use crate::deflate::algorithm::{BlockState, Flush, StreamCursors};
    use crate::deflate::state::{
        DeflateConfig, DeflateState, GlobalAllocator, Method, Strategy, DEF_MEM_LEVEL,
    };
    use crate::read_buf::{InputCursor, OutputCursor};
    use crate::trees::_tr_init;

    /// A non-zero `insert` planted before each call, so that "the call set `insert` to zero"
    /// is distinguishable from "`insert` was zero all along".
    ///
    /// `MIN_MATCH - 1`, which is the largest value `deflate_fast` and `deflate_slow` ever
    /// leave behind (`deflate.c` L1940 and L2068) and therefore the value this compressor
    /// would plausibly inherit from a `deflateParams()` switch.
    const INSERT_SENTINEL: usize = 2;

    /// A stream configured as `deflateInit2(&strm, 6, Z_DEFLATED, -15, mem_level,
    /// Z_HUFFMAN_ONLY)` and wired up by `_tr_init`, which is the state a compressor is first
    /// entered with.
    ///
    /// Raw DEFLATE (negative `windowBits`) so that nothing but the block itself reaches the
    /// output: `deflate()` writes no zlib or gzip header for `wrap == 0` and no trailer
    /// either, which is what lets a single call's output be compared with the C oracle's
    /// byte for byte. In a debug build every block the allocator hands back is filled with
    /// `0xa5`, the byte `test/infcover.c` L87 uses, so nothing below can pass by accident on
    /// zeroed memory.
    fn huffman_only_state(mem_level: i32) -> DeflateState<'static, GlobalAllocator> {
        let config = DeflateConfig {
            level: 6,
            method: Method::Deflated,
            window_bits: -15,
            mem_level,
            strategy: Strategy::HuffmanOnly,
        };
        let mut state = DeflateState::new(config, GlobalAllocator).unwrap();
        _tr_init(&mut state);
        state.window.insert = INSERT_SENTINEL;
        state
    }

    /// Cursors over `input` and `output`, with the scalars a freshly reset raw stream carries
    /// (`deflate.c` L663-L668).
    fn cursors<'i, 'o>(input: &'i [u8], output: &'o mut [u8]) -> StreamCursors<'i, 'o> {
        StreamCursors {
            input: InputCursor::new(input),
            output: OutputCursor::new(output),
            check: 0,
            total_in: 0,
            total_out: 0,
            data_type: Z_UNKNOWN,
        }
    }

    /// One `Z_FINISH` call over `input`, writing into `output`; returns the block state and
    /// how many bytes were produced.
    ///
    /// This is deliberately a single call with an output buffer large enough for the whole
    /// stream, because that is exactly what the oracle generator does — see
    /// [`tests::HELLO_FINISH`].
    fn finish(input: &[u8], mem_level: i32, output: &mut [u8]) -> (BlockState, usize) {
        let mut state = huffman_only_state(mem_level);
        let mut cursors = cursors(input, output);
        let bstate = deflate_huff(&mut state, &mut cursors, Flush::Finish);
        assert_eq!(state.window.insert, 0, "deflate.c L2177");
        (bstate, cursors.output.written())
    }

    // The four vectors below were produced by the reference implementation in this tree,
    // built by the environment's oracle build (`-O3 -fPIC -D_LARGEFILE64_SOURCE=1
    // -DHAVE_HIDDEN`), driven with exactly the call this test makes.
    //
    // A single call with ample output space makes `deflate()` a transparent wrapper around
    // `deflate_huff`: raw `windowBits` means no header and no trailer, and `finish_done`
    // sends the driver straight to `return Z_STREAM_END`. So `s.next_out[0..total_out]` is
    // precisely this function's output.
    //
    // These are *byte identity* assertions, which is the acceptance criterion. A round-trip
    // test that merely decompresses would prove nothing about it: infinitely many valid
    // DEFLATE streams decode to the same input.

    /// `"hello, hello!"` — the payload `test/example.c` compresses (L44 and L64).
    const HELLO_FINISH: [u8; 15] = [
        0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0x04, 0x00,
    ];

    /// No input at all: one empty final block, `BFINAL` set.
    const EMPTY_FINISH: [u8; 2] = [0x03, 0x00];

    /// A single literal, the smallest stream this compressor can emit.
    const SINGLE_FINISH: [u8; 3] = [0x4b, 0x04, 0x00];

    #[test]
    fn finish_over_the_example_payload_is_byte_identical_to_the_c_oracle() {
        let mut output = [0u8; 64];
        let (bstate, written) = finish(b"hello, hello!", DEF_MEM_LEVEL, &mut output);

        assert_eq!(bstate, BlockState::FinishDone, "deflate.c L2180");
        assert_eq!(
            &output[..written],
            &HELLO_FINISH,
            "output diverged from C zlib"
        );
    }

    #[test]
    fn finish_over_no_input_is_byte_identical_to_the_c_oracle() {
        let mut output = [0u8; 64];
        let (bstate, written) = finish(b"", DEF_MEM_LEVEL, &mut output);

        // The loop breaks on its first iteration -- `lookahead` is zero and `flush` is not
        // `Z_NO_FLUSH` -- and the tail emits the empty final block.
        assert_eq!(bstate, BlockState::FinishDone, "deflate.c L2165 then L2180");
        assert_eq!(
            &output[..written],
            &EMPTY_FINISH,
            "output diverged from C zlib"
        );
    }

    #[test]
    fn finish_over_a_single_byte_is_byte_identical_to_the_c_oracle() {
        let mut output = [0u8; 64];
        let (bstate, written) = finish(b"a", DEF_MEM_LEVEL, &mut output);

        assert_eq!(bstate, BlockState::FinishDone);
        assert_eq!(
            &output[..written],
            &SINGLE_FINISH,
            "output diverged from C zlib"
        );
    }

    /// Input long enough to fill the symbol buffer repeatedly, so that the `bflush` branch at
    /// `deflate.c` L2175 is what decides where the block boundaries fall.
    ///
    /// `memLevel` 1 makes `lit_bufsize` `1 << (1 + 6)` = 128 and therefore
    /// `sym_end` = `(128 - 1) * 3` = 381 bytes = 127 symbols (`deflate.c` L520-L521), so 600
    /// literals cross the boundary four times. Compared by length and CRC-32 rather than by
    /// an inline array purely for legibility; both are the oracle's.
    #[test]
    fn the_bflush_boundary_matches_the_c_oracle() {
        let mut input = [0u8; 600];
        for (index, slot) in input.iter_mut().enumerate() {
            // `(unsigned char)('A' + (i % 7))`
            *slot = b'A' + u8::try_from(index % 7).unwrap();
        }

        let mut output = [0u8; 1024];
        let (bstate, written) = finish(&input, 1, &mut output);

        assert_eq!(bstate, BlockState::FinishDone);
        assert_eq!(written, 296, "block boundaries diverged from C zlib");
        assert_eq!(
            crc32(0, &output[..written]),
            0x4d4e_f2c6,
            "output diverged from C zlib"
        );
    }

    /// A wide alphabet, so the dynamic literal tree is non-trivial and the emitted codes
    /// depend on the exact frequency counts this function accumulates.
    #[test]
    fn a_wide_literal_alphabet_matches_the_c_oracle() {
        let mut input = [0u8; 1000];
        for (index, slot) in input.iter_mut().enumerate() {
            // `(unsigned char)((i * 31u + (i >> 3)) & 0xff)`
            let index = u32::try_from(index).unwrap();
            let value = index.wrapping_mul(31).wrapping_add(index >> 3) & 0xff;
            *slot = u8::try_from(value).unwrap();
        }

        let mut output = [0u8; 2048];
        let (bstate, written) = finish(&input, DEF_MEM_LEVEL, &mut output);

        assert_eq!(bstate, BlockState::FinishDone);
        assert_eq!(written, 1005, "output length diverged from C zlib");
        assert_eq!(
            crc32(0, &output[..written]),
            0x4269_7345,
            "output diverged from C zlib"
        );
    }

    /// `if (flush == Z_NO_FLUSH) return need_more;` (`deflate.c` L2163-L2164).
    ///
    /// With no input and nothing in the window there is no literal to write, so the call must
    /// ask for more rather than close the block — and it must return *before* `insert = 0`
    /// (L2177), leaving the sentinel in place.
    #[test]
    fn no_literal_under_no_flush_returns_need_more_and_touches_nothing() {
        let mut state = huffman_only_state(DEF_MEM_LEVEL);
        let mut output = [0u8; 64];
        let mut cursors = cursors(b"", &mut output);

        let bstate = deflate_huff(&mut state, &mut cursors, Flush::NoFlush);

        assert_eq!(bstate, BlockState::NeedMore);
        assert_eq!(state.window.insert, INSERT_SENTINEL, "L2164 precedes L2177");
        assert_eq!(state.pending.sym_next(), 0, "nothing was tallied");
        assert_eq!(cursors.output.written(), 0, "no block was flushed");
    }

    /// Every input byte becomes exactly one literal, and nothing else is recorded.
    ///
    /// `Z_NO_FLUSH` is what makes the tally observable: 13 literals are far short of
    /// `sym_end`, so no `FLUSH_BLOCK` runs and `init_block` never resets the counts. This is
    /// the whole of `_tr_tally_lit`'s effect (`deflate.h` L357-L364), checked directly.
    #[test]
    fn every_input_byte_is_tallied_as_one_literal() {
        const INPUT: &[u8] = b"hello, hello!";

        let mut state = huffman_only_state(DEF_MEM_LEVEL);
        let mut output = [0u8; 64];
        let mut cursors = cursors(INPUT, &mut output);

        assert_eq!(
            deflate_huff(&mut state, &mut cursors, Flush::NoFlush),
            BlockState::NeedMore
        );

        // Three bytes of `sym_buf` per literal (L2172, through `push_symbol`).
        assert_eq!(state.pending.sym_next(), INPUT.len() * 3);

        // `s->dyn_ltree[cc].Freq++` -- a plain histogram of the input.
        for byte in 0u16..=255 {
            let expected = INPUT.iter().filter(|&&b| u16::from(b) == byte).count();
            assert_eq!(
                usize::from(state.dyn_ltree[usize::from(byte)].freq()),
                expected,
                "literal frequency for byte {byte}"
            );
        }

        // No distance was ever tallied: this strategy finds no matches at all.
        assert!(state.dyn_dtree.iter().all(|entry| entry.freq() == 0));
        assert_eq!(state.matches, 0);

        // `s->lookahead--` and `s->strstart++`, once per literal (L2173-L2174).
        assert_eq!(state.window.strstart, INPUT.len());
        assert_eq!(state.window.lookahead, 0);
        assert_eq!(cursors.total_in, INPUT.len() as u64);

        // `s->match_length = 0` on every iteration (L2170).
        assert_eq!(state.match_length, 0);

        // Still no block boundary.
        //
        // `insert` is deliberately *not* asserted here. `fill_window` drains it while priming
        // the hash chains, as soon as `lookahead + insert >= MIN_MATCH`
        // (`deflate.c` L315-L332), so the sentinel is already gone by the time L2177 could
        // clear it -- in C exactly as here. That L2164 returns *before* L2177 is instead
        // shown by `no_literal_under_no_flush_returns_need_more_and_touches_nothing`, where
        // `lookahead + insert` is 2 and the priming block is therefore skipped.
        assert_eq!(cursors.output.written(), 0);
    }

    /// A flush mode other than `Z_FINISH` closes the block and reports `block_done`
    /// (`deflate.c` L2182-L2184), clearing `insert` on the way through (L2177).
    #[test]
    fn a_non_finish_flush_closes_the_block_and_clears_insert() {
        let mut state = huffman_only_state(DEF_MEM_LEVEL);
        let mut output = [0u8; 64];
        let mut cursors = cursors(b"hello", &mut output);

        let bstate = deflate_huff(&mut state, &mut cursors, Flush::Block);

        assert_eq!(bstate, BlockState::BlockDone);
        assert_eq!(
            state.window.insert, 0,
            "deflate.c L2177 is an unconditional 0"
        );
        assert!(cursors.output.written() > 0, "the block was flushed");
        // `_tr_flush_block` ends with `init_block`, so the symbol buffer is empty again.
        assert_eq!(state.pending.sym_next(), 0);
    }

    /// `Z_BLOCK` with an already-empty block emits nothing: `if (s->sym_next)` is false, and
    /// the driver -- not this function -- decides what an empty flush needs (L1238-L1245).
    #[test]
    fn a_non_finish_flush_with_no_symbols_emits_nothing() {
        let mut state = huffman_only_state(DEF_MEM_LEVEL);
        let mut output = [0u8; 64];
        let mut cursors = cursors(b"", &mut output);

        let bstate = deflate_huff(&mut state, &mut cursors, Flush::Block);

        assert_eq!(bstate, BlockState::BlockDone);
        assert_eq!(state.pending.sym_next(), 0);
        assert_eq!(
            cursors.output.written(),
            0,
            "deflate.c L2182 guards the flush"
        );
        assert_eq!(state.pending.pending(), 0, "not even buffered");
    }

    /// `FLUSH_BLOCK`'s premature exit: `return (last) ? finish_started : need_more` when the
    /// flush filled the output buffer (`deflate.c` L1644).
    ///
    /// The final block for `"hello, hello!"` is 15 bytes, so a 4-byte output buffer cannot
    /// take it and the call must report [`BlockState::FinishStarted`] rather than
    /// [`BlockState::FinishDone`] — which is what tells `deflate()` to enter `FINISH_STATE`
    /// and still return `Z_OK` (L1222-L1237).
    #[test]
    fn a_full_output_buffer_during_the_final_flush_reports_finish_started() {
        let mut state = huffman_only_state(DEF_MEM_LEVEL);
        let mut output = [0u8; 4];
        let mut cursors = cursors(b"hello, hello!", &mut output);

        let bstate = deflate_huff(&mut state, &mut cursors, Flush::Finish);

        assert_eq!(bstate, BlockState::FinishStarted);
        assert_eq!(cursors.avail_out(), 0);
        // The bytes that did fit are still the stream's first bytes.
        assert_eq!(cursors.output.written_slice(), &HELLO_FINISH[..4]);
        // And `insert` was cleared before the flush, as L2177 precedes L2179.
        assert_eq!(state.window.insert, 0);
    }

    /// Feeding the input one byte per call reaches the same final stream as feeding it whole.
    ///
    /// This is the resumability contract: `deflate()` may be called repeatedly with
    /// `Z_NO_FLUSH` and a byte at a time, and the compressed result must not depend on how
    /// the caller chunked its input. The block boundaries are decided by `sym_next`, which
    /// survives across calls, not by call boundaries.
    #[test]
    fn chunked_input_produces_the_same_stream_as_a_single_call() {
        const INPUT: &[u8] = b"hello, hello!";

        let mut state = huffman_only_state(DEF_MEM_LEVEL);
        let mut output = [0u8; 64];
        let mut cursors = cursors(&[], &mut output);

        // One `Z_NO_FLUSH` call per byte. `StreamCursors::input` is rebuilt each time, which
        // is what the facade does when the caller advances `next_in` itself.
        for index in 0..INPUT.len() {
            cursors.input = InputCursor::new(&INPUT[index..=index]);
            assert_eq!(
                deflate_huff(&mut state, &mut cursors, Flush::NoFlush),
                BlockState::NeedMore,
                "byte {index} should have asked for more input"
            );
        }

        cursors.input = InputCursor::new(&[]);
        assert_eq!(
            deflate_huff(&mut state, &mut cursors, Flush::Finish),
            BlockState::FinishDone
        );
        assert_eq!(cursors.output.written_slice(), &HELLO_FINISH);
    }
}
