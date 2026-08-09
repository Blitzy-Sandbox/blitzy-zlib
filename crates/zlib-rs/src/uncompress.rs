//! One-shot decompression of a whole buffer: the Rust counterpart of `uncompr.c`.
//!
//! Four entry points sit on top of the streaming decoder, and only one of them
//! contains any logic. `uncompress2_z` (`uncompr.c` L29-L82) drives
//! [`inflate`] to completion over a caller-owned source and destination; the
//! other three (`uncompr.c` L83-L101) are one-line adapters. This module
//! reproduces all four, because `zlib.h` declares all four -- `uncompress` at
//! L1315, `uncompress_z` at L1317, `uncompress2` at L1335 and `uncompress2_z`
//! at L1337 -- and the exported symbol set is frozen.
//!
//! ★ The C file is named `uncompr.c`, not `uncompress.c`; only the Rust module
//! is named `uncompress`.
//!
//! # The contract, from `uncompr.c` L11-L28
//!
//! `*sourceLen` is the byte length of the source buffer. On entry `*destLen` is
//! the total size of the destination buffer, which must be large enough for the
//! entire uncompressed result -- its size is not recorded in the stream, so the
//! caller has to have learned it by some other means. On exit `*destLen` is the
//! size of the decompressed data **and `*sourceLen` is the number of source
//! bytes consumed**, so that `source + *sourceLen` addresses the first unused
//! input byte.
//!
//! ★ Both length parameters are in/out. An implementation that treated `*sourceLen` as
//! input-only would still pass a naive round-trip test and would then break the
//! first caller that relies on the first-unused-byte guarantee -- which is how a
//! caller finds the start of whatever follows a zlib stream in a container.
//! That is why every entry point here returns a [`Decompressed`] rather than a
//! bare status: the safe core holds no pointer, so the only way to publish an
//! out-parameter is to return it.
//!
//! The documented outcomes are [`ReturnCode::OK`], [`ReturnCode::MEM_ERROR`],
//! [`ReturnCode::BUF_ERROR`] when the destination had no room left, and
//! [`ReturnCode::DATA_ERROR`] when the input was corrupt -- "including if the
//! input data is an incomplete zlib stream", which is a distinction this module
//! draws itself and which [`uncompress2_z`] documents in detail.
//!
//! # What the safe core does not do
//!
//! Three parts of `uncompress2_z` exist only because C has raw pointers, and
//! all three belong to `crates/libz-rs-sys` rather than here:
//!
//! * The null guard at `uncompr.c` L36-L38 returns `Z_STREAM_ERROR` when either
//!   length pointer is `Z_NULL`, or when a non-zero length is paired with a null
//!   buffer. A `&[u8]` is never null and carries its own length, so all four
//!   conditions are unrepresentable here; the facade must apply the guard before
//!   it builds the slices it passes in.
//! * `uncompr.c` L42-L43 redirects a null `dest` at `&stream.reserved` when
//!   `*destLen` is zero, with the comment "`next_out` cannot be NULL", because
//!   [`inflate`] rejects a null output pointer. An empty Rust slice is already
//!   non-null and non-dangling, so the redirection has nothing to fix. The
//!   *behaviour* it enables is preserved exactly, and is load-bearing: see
//!   [`uncompress2_z`] on the zero-length destination.
//! * The `uLong`-versus-`z_size_t` split. `uLong` is 32 bits wide on LLP64
//!   Windows and 64 bits wide on LP64, so C needs two spellings of every
//!   function; the safe core works in the `usize` domain only, and the width
//!   mapping lives in the facade's `types.rs`. The two spellings are therefore
//!   observationally identical here, and each is kept so that the facade has one
//!   core function per exported symbol.
//!
//! # Layering and safety posture
//!
//! `no_std`, and allocation-free in itself -- whatever the decoder needs it
//! takes from [`GlobalAllocator`], which implements `zcalloc`/`zcfree` and
//! therefore exactly what `uncompr.c` L47-L49 selects by zeroing `zalloc`,
//! `zfree` and `opaque`. The module names only `core` plus three sibling
//! modules, holds no raw pointer, and needs no escape hatch from the compiler's
//! memory-safety guarantees, so the crate root's blanket prohibition on them
//! costs it nothing.
//!
//! Nothing here can panic. Every index is produced by `.min()` against a length
//! captured up front, every subtraction saturates, and no fallible operation is
//! unwrapped. That is a hard requirement rather than a nicety: this function is
//! the library's primary untrusted-input surface, it is to be the target of the planned
//! `fuzz/fuzz_targets/fuzz_inflate.rs` -- which has not landed; `fuzz/fuzz_targets/` is still
//! empty -- and it must terminate without panicking
//! on arbitrary bytes. Termination is proved in [`uncompress2_z`].
//!
//! # Examples
//!
//! ```
//! use zlib_rs::compress::compress2_z;
//! use zlib_rs::error::ReturnCode;
//! use zlib_rs::uncompress::uncompress2_z;
//!
//! // A zlib stream for `b"hello, hello!"`, followed by four unrelated bytes.
//! let mut buffer = [0_u8; 64];
//! let made = compress2_z(&mut buffer, b"hello, hello!", 9);
//! assert_eq!(made.code, ReturnCode::OK);
//! let mut stream = buffer[..made.produced].to_vec();
//! stream.extend_from_slice(b"tail");
//!
//! let mut plain = [0_u8; 64];
//! let report = uncompress2_z(&mut plain, &stream);
//!
//! assert_eq!(report.code, ReturnCode::OK);
//! assert_eq!(plain.get(..report.produced), Some(&b"hello, hello!"[..]));
//! // The tail is not part of the stream, so it is not consumed ...
//! assert_eq!(stream.get(report.consumed..), Some(&b"tail"[..]));
//!
//! // ... and a destination too small to hold the result reports how far it got.
//! let mut cramped = [0_u8; 5];
//! let report = uncompress2_z(&mut cramped, &stream);
//! assert_eq!(report.code, ReturnCode::BUF_ERROR);
//! assert_eq!(cramped.get(..report.produced), Some(&b"hello"[..]));
//! ```
//!
//! # Provenance
//!
//! The one-shot decompression wrappers.
//!
//! Ported from `uncompr.c`: `uncompress`, `uncompress2` and their `_z` forms. (The file is
//! `uncompr.c`, not `uncompress.c`; the module is named for the function.)

// Names that repeat their module's name are deliberate here: the C sources this module ports name
// these entry points, and `crates/zlib-rs/src/lib.rs` re-exports several of them under exactly
// these names, so renaming any of them to satisfy `clippy::module_name_repetitions` would cost the
// traceability the port is judged on. The lint sits in `pedantic`, which this workspace denies, and
// it fires on the declared 1.80 floor; upstream has since reclassified it, so the allowance is what
// keeps the same lint gate passing on both toolchains. The same relaxation, for the same reason,
// already appears in `config.rs`, `deflate/**`, `inflate/**` and `gz/**`.
#![allow(clippy::module_name_repetitions)]

use crate::allocate::GlobalAllocator;
use crate::config::Z_NO_FLUSH;
use crate::error::ReturnCode;
use crate::inflate::{inflate, inflate_end, inflate_init, InflateStream};
use crate::read_buf::OutputRegion;

/// The largest number of bytes handed to the decoder in one call.
///
/// Mirrors `const uInt max = (uInt)-1` (`uncompr.c` L33). `uInt` is C's
/// `unsigned int`, so the value is 32 bits of ones: the widest `avail_in` or
/// `avail_out` a `z_stream` can express. It exists because `z_size_t` is 64 bits
/// wide on LP64 while `avail_in` and `avail_out` are not, so a buffer larger
/// than 4 GiB has to be fed to the decoder in pieces.
///
/// ★ Keeping the cap is not an optimisation and not dead weight on smaller
/// buffers: it fixes the exact sequence of `avail_in`/`avail_out` values that
/// [`inflate`] observes, and [`inflate`]'s resumption behaviour -- how much it
/// consumes before it reports, and therefore the consumed count this module
/// publishes -- is defined against that sequence.
const MAX_CHUNK: usize = u32::MAX as usize;

/// Everything one call of the `uncompress` family reports to its caller.
///
/// C writes two of these three through pointers and returns the third. The safe
/// core has no pointer to write through, so all three come back together. The
/// field names are the reference's own vocabulary, from the comment at
/// `uncompr.c` L69-L71: "Set `*sourceLen` to the amount of input consumed. Set
/// `*destLen` to the amount of data produced."
///
/// ★ The counts report real work on every outcome the decompression loop can
/// produce, not just on success. `zlib.h` L1330-L1332 promises that "in the case
/// where there is not enough room, `uncompress()` will fill the output buffer
/// with the uncompressed data up to that point", so a caller that sees
/// [`ReturnCode::BUF_ERROR`] still learns how much it got. A facade therefore
/// writes both counts back unconditionally.
///
/// ★ **The one exception: a failed stream initialisation.** `uncompr.c` L51-L52
/// is `err = inflateInit(&stream); if (err != Z_OK) return err;` -- it returns
/// *before* `*destLen` and `*sourceLen` are ever written, so on that path the
/// caller's two out-parameters keep the values it passed in. No work was done, so
/// there is no count to report.
///
/// Because this type returns the counts instead of storing them, that behaviour
/// is expressed by echoing the **entry lengths** back: on an initialisation
/// failure -- in practice [`ReturnCode::MEM_ERROR`] -- [`Decompressed::produced`]
/// is the destination's length and [`Decompressed::consumed`] is the source's
/// length. Those two numbers are *not* counts of bytes produced or consumed on
/// that path; they are what lets a facade write both back unconditionally and
/// still leave the caller's variables unchanged.
///
/// So do not read `produced`/`consumed` as work performed without first checking
/// `code`. Every other status -- success, a data error, a buffer error -- reports
/// genuine counts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "the status code reports corrupt input and a short destination buffer"]
pub struct Decompressed {
    /// The status, i.e. C's `int` return value.
    pub code: ReturnCode,
    /// Bytes written to the destination, i.e. `*destLen` on exit.
    ///
    /// Never greater than the destination's length. The bytes themselves are the
    /// destination's leading `produced` bytes; the rest is untouched.
    pub produced: usize,
    /// Source bytes consumed, i.e. `*sourceLen` on exit.
    ///
    /// Never greater than the source's length, so the caller's first unused
    /// input byte is at this offset. For a well-formed stream followed by
    /// unrelated data this is exactly the length of the stream, which is how a
    /// caller walks a sequence of concatenated members.
    pub consumed: usize,
}

/// Decompresses `source` into `dest`, reporting both byte counts.
///
/// The Rust counterpart of `uncompress2_z` (`uncompr.c` L29-L82), declared at `zlib.h`
/// L1337. This is the only entry point in the module with a body; the other
/// three delegate to it, directly or through each other.
///
/// The stream is read as a zlib container with the largest window, because C
/// calls plain `inflateInit` at L51 rather than `inflateInit2`, which fixes
/// `windowBits` at `DEF_WBITS` (15). Raw DEFLATE and gzip members are therefore
/// rejected here, exactly as the reference rejects them, and a caller that wants
/// either drives [`inflate`] itself.
///
/// # The loop
///
/// `uncompr.c` L57-L67, reproduced in shape:
///
/// * Both availabilities start at zero (L46 and L55) and are topped up from the
///   residuals at the head of each iteration -- output first, then input, in
///   that order.
/// * The flush mode is **always** `Z_NO_FLUSH`. There is no `Z_FINISH`
///   selector, unlike `compress2_z`, and that is deliberate: [`inflate`] learns
///   that a stream has ended from the data itself, so declaring the end of input
///   would only forbid the `Z_OK` returns this loop is built around.
/// * The loop continues for as long as [`inflate`] answers
///   [`ReturnCode::OK`].
///
/// Because the availabilities are derived from the slices it is handed, each
/// iteration gives [`inflate`] a **prefix** of `source` and of `dest`, sized to
/// the window C would have described with `next_in`/`avail_in` and
/// `next_out`/`avail_out`. Passing the whole prefix rather than only the
/// untouched tail is what lets a match copy read output that an earlier
/// iteration wrote; it changes nothing else, because [`inflate`] bounds a
/// copy-from-output by what the current call produced and reaches further back
/// only through the window.
///
/// ★ **Termination.** The loop cannot spin. [`inflate`] converts its own
/// [`ReturnCode::OK`] into [`ReturnCode::BUF_ERROR`] whenever a call neither
/// consumed nor produced a byte (`inflate.c` L1150-L1151), so every iteration
/// that continues has advanced a cursor. Both cursors are bounded by their
/// buffers, so the loop runs at most `source.len() + dest.len() + 1` times on
/// any input whatsoever -- which is what makes this function safe to point a
/// fuzzer at.
///
/// # Returns
///
/// * [`ReturnCode::OK`] -- the stream ended and the whole result is in `dest`.
/// * [`ReturnCode::BUF_ERROR`] -- `dest` filled up first. `produced` bytes are
///   valid and `consumed` says how far the input was read.
/// * [`ReturnCode::DATA_ERROR`] -- the input is corrupt, is not a zlib stream,
///   is an **incomplete** zlib stream, or needs a preset dictionary.
/// * [`ReturnCode::MEM_ERROR`] -- the decoder could not allocate.
///
/// ★ The truncated-stream case is the subtle one, and it is why the ladder
/// below tests the residual rather than trusting [`inflate`]'s answer. Running
/// out of input and running out of room both leave [`inflate`] reporting
/// [`ReturnCode::BUF_ERROR`]; the two are told apart by whether any input is
/// left over. None left means the stream simply stopped early, which is
/// corruption, so it becomes [`ReturnCode::DATA_ERROR`]. Some left means the
/// caller's buffer was too small, which is recoverable and stays
/// [`ReturnCode::BUF_ERROR`].
///
/// # The zero-length destination
///
/// An empty `dest` is a supported input, not an error, and it must not be
/// short-circuited. The loop runs, [`inflate`] parses as much as it can without
/// writing anything, and the call reports `produced == 0`. That is how a caller
/// holding no buffer at all probes a stream: an empty stream answers
/// [`ReturnCode::OK`], while a stream with content answers
/// [`ReturnCode::BUF_ERROR`] once input remains unconsumed. C reaches the same
/// place by aiming `next_out` at eight bytes of `z_stream` padding it never
/// reads (L42-L43).
pub fn uncompress2_z(dest: &mut [u8], source: &[u8]) -> Decompressed {
    uncompress2_z_into(&mut OutputRegion::init(dest), source)
}

/// [`uncompress2_z`] over a destination that may be write-only storage.
///
/// The entry point `crates/libz-rs-sys` uses, because a C caller's `dest` is guaranteed
/// writable and nothing more -- `zlib.h` L1319-L1327 asks for `*destLen` bytes of room and
/// says nothing about their contents. Identical in behaviour to [`uncompress2_z`], which is a
/// one-line forwarder to it over an [`OutputRegion::init`].
pub fn uncompress2_z_into(dest: &mut OutputRegion<'_>, source: &[u8]) -> Decompressed {
    // Captured before the first reborrow of `dest`, and the origin of every
    // bound below. These are C's entry values of `*sourceLen` and `*destLen`.
    let source_len = source.len();
    let dest_len = dest.len();

    // L40-L41. `len` and `left` are the residuals: input not yet handed to the
    // decoder, and destination room not yet handed to it. They are *not* the
    // consumed and produced counts, and the difference matters -- see the
    // accounting after the loop.
    let mut len = source_len;
    let mut left = dest_len;

    // L51-L52. Plain `inflateInit`: a zlib container with a 32 KiB window.
    //
    // C returns here without having written either out-parameter, so the
    // caller's `*destLen` and `*sourceLen` keep their entry values. Reporting
    // those entry values is how that is expressed when the counts are returned
    // instead of stored, and it lets a facade write both back unconditionally.
    let mut state = match inflate_init(GlobalAllocator) {
        Ok(state) => state,
        Err(code) => {
            return Decompressed {
                code,
                produced: dest_len,
                consumed: source_len,
            };
        }
    };

    // L45-L46 and L54-L55: both cursors at the base of their buffer, both
    // availabilities empty so that the first iteration fills them.
    let mut next_in = 0_usize;
    let mut avail_in = 0_usize;
    let mut next_out = 0_usize;
    let mut avail_out = 0_usize;

    // C reuses one `z_stream` for every call, so these five fields carry
    // forward. The safe core rebuilds the view each iteration and therefore has
    // to carry them by hand. This function reads none of them -- the decoder
    // treats all five as write-only, keeping the running check value in its own
    // state rather than in `stream.adler` -- but reproducing C's single-stream
    // accounting costs nothing and keeps the mirror honest.
    let mut total_in = 0_u64;
    let mut total_out = 0_u64;
    let mut msg = None;
    // `None`, not zero: C's `inflateResetKeep` writes `strm->adler` only for a wrapped
    // stream (`inflate.c` L108-L109), and a raw one is left alone. Carrying the
    // reference's "not assigned" state is what keeps a raw decode from inventing a
    // check value it never had.
    let mut adler = None;
    let mut data_type = 0_i32;

    // L57-L67.
    let err = loop {
        // L58-L61: top up the output window, then L62-L65: the input window.
        // The order is C's. Neither `saturating_sub` can saturate, because each
        // chunk is a `min` against the residual it is taken from.
        if avail_out == 0 {
            avail_out = left.min(MAX_CHUNK);
            left = left.saturating_sub(avail_out);
        }
        if avail_in == 0 {
            avail_in = len.min(MAX_CHUNK);
            len = len.saturating_sub(avail_in);
        }

        // The windows themselves. `next_in + avail_in` never exceeds
        // `source_len` and `next_out + avail_out` never exceeds `dest_len`, so
        // the clamps are belt and braces; with them, neither split can panic
        // whatever the arithmetic above produced.
        let in_end = next_in.saturating_add(avail_in).min(source_len);
        let out_end = next_out.saturating_add(avail_out).min(dest_len);
        let (input, _unread) = source.split_at(in_end);

        // ★ The output window starts **at** `next_out` and the stream's own cursor starts at
        // zero, rather than the window starting at the destination's base with the cursor
        // pre-positioned. That is not merely tidier, it is what the decoder needs: the two
        // places `inflate()` reads its own output back -- the check value at `inflate.c` L1080
        // and the window update at L1136 -- are both over *this call's* output, which is
        // exactly what the sub-window spans. It also keeps [`OutputRegion`]'s promise about
        // its write-only variant exact, since every write lands at or after the sub-window's
        // own base.
        let mut window = dest.reborrow(next_out, out_end.saturating_sub(next_out));
        let mut stream = InflateStream::with_region(input, window.reborrow(0, window.len()));
        stream.next_in = next_in;
        stream.total_in = total_in;
        stream.total_out = total_out;
        stream.msg = msg;
        stream.adler = adler;
        stream.data_type = data_type;

        // L66.
        let err = inflate(&mut state, &mut stream, Z_NO_FLUSH);

        next_in = stream.next_in;
        next_out = next_out.saturating_add(stream.next_out);
        avail_in = stream.avail_in();
        avail_out = stream.avail_out();
        total_in = stream.total_in;
        total_out = stream.total_out;
        msg = stream.msg;
        adler = stream.adler;
        data_type = stream.data_type;

        // L67.
        if err != ReturnCode::OK {
            break err;
        }
    };

    // L69-L75, verbatim in arithmetic as well as in shape:
    //
    //     len  += stream.avail_in;
    //     left += stream.avail_out;
    //     *sourceLen -= len;
    //     *destLen   -= left;
    //
    // ★ This is the subtlest passage in the file. `len` and `left` are the
    // residuals never handed to the decoder; adding the decoder's own leftovers
    // turns each into the total that went unused, and subtracting that from the
    // entry total gives the amount actually consumed or produced. It is not the
    // same statement as "accumulate what each call reported", and it must not be
    // rewritten into that form: the two agree only while `avail_in` and
    // `next_in` stay consistent, which the reference guarantees by convention
    // and this implementation guarantees structurally, because the availabilities are
    // derived from the cursors rather than stored beside them.
    //
    // Neither saturation bites. `len + avail_in` is `source_len - consumed` and
    // `left + avail_out` is `dest_len - produced`, so both sums stay within
    // their entry totals and both differences are exact.
    len = len.saturating_add(avail_in);
    left = left.saturating_add(avail_out);
    let consumed = source_len.saturating_sub(len);
    let produced = dest_len.saturating_sub(left);

    // L77. Unconditional, and the result is discarded exactly as C discards it:
    // the only failure C's `inflateEnd` can report is a broken stream handle,
    // which an owned decoder state cannot be.
    let _end = inflate_end(&mut state);

    // L78-L81, arm for arm and in C's order.
    //
    // ★ The third arm reads `len` *after* the accounting above, so it asks "was
    // every input byte consumed?". That is the whole truncated-versus-full
    // discrimination; testing it before the accounting, or testing the
    // pre-accounting residual, silently reports a short output buffer as
    // corruption and an incomplete stream as a short buffer.
    let code = match err {
        ReturnCode::STREAM_END => ReturnCode::OK,
        ReturnCode::NEED_DICT => ReturnCode::DATA_ERROR,
        ReturnCode::BUF_ERROR if len == 0 => ReturnCode::DATA_ERROR,
        other => other,
    };

    Decompressed {
        code,
        produced,
        consumed,
    }
}

/// Decompresses `source` into `dest`, reporting both byte counts.
///
/// The Rust counterpart of `uncompress2` (`uncompr.c` L83-L91), declared at `zlib.h` L1335.
/// C's body widens the caller's two `uLong` lengths to `z_size_t`, calls
/// [`uncompress2_z`], and narrows both results back.
///
/// In the safe core there is nothing to widen or narrow: lengths are `usize`
/// throughout, so this is [`uncompress2_z`] under its other name. The width
/// mapping it exists for is the facade's, in `types.rs`, where `uLong` becomes
/// `c_ulong` and the narrowing at `uncompr.c` L88-L89 truncates on exactly the
/// targets C's own `(uLong)` cast truncates on. Both spellings are kept so that
/// each exported symbol has one core function behind it.
pub fn uncompress2(dest: &mut [u8], source: &[u8]) -> Decompressed {
    uncompress2_z(dest, source)
}

/// Decompresses `source` into `dest`, reporting how much was produced.
///
/// The Rust counterpart of `uncompress_z` (`uncompr.c` L92-L96), declared at `zlib.h` L1317:
/// [`uncompress2_z`] with the source length passed by value instead of by
/// pointer, so the consumed count has nowhere to go and C drops it.
///
/// [`Decompressed::consumed`] is still filled in, because the safe core returns
/// one shape from all four entry points and discarding a field would cost a
/// second shape. A facade exporting this symbol has no out-parameter to write it
/// to and must ignore it; [`Decompressed::produced`] and
/// [`Decompressed::code`] are the whole of this function's C-visible result.
pub fn uncompress_z(dest: &mut [u8], source: &[u8]) -> Decompressed {
    uncompress2_z(dest, source)
}

/// Decompresses `source` into `dest`, reporting how much was produced.
///
/// The Rust counterpart of `uncompress` (`uncompr.c` L97-L101), declared at `zlib.h` L1315.
/// This is the entry point most callers use, and the prose at `zlib.h`
/// L1319-L1332 is written about it.
///
/// ★ It delegates to [`uncompress2`], **not** to [`uncompress_z`]. C's chain is
/// `uncompress` -> `uncompress2` -> `uncompress2_z`: the plain form takes the
/// `uLong` route and only the `_z` form takes the `z_size_t` route. That is
/// asymmetric with `compress`, so the chain is reproduced rather than tidied --
/// a reader who assumes symmetry would otherwise conclude the widening happens
/// somewhere it does not.
///
/// As with [`uncompress_z`], C exposes no out-parameter for the consumed count,
/// so a facade exporting this symbol reports only
/// [`Decompressed::produced`] and [`Decompressed::code`].
pub fn uncompress(dest: &mut [u8], source: &[u8]) -> Decompressed {
    uncompress2(dest, source)
}

// Every expectation below was MEASURED against the reference implementation --
// the in-tree C sources built as `libz.a` -- by calling `uncompress2_z` on the
// same fixture and recording the resulting `(int, *destLen, *sourceLen)` triple.
// None of them is inferred from the algorithm, because the point of the suite is
// to catch a divergence that looks reasonable.
//
// The fixtures are compressed streams rather than plaintext because the encoder
// is not this module's dependency: a stream produced by the C compressor and
// decoded here is a direct test of the C-to-Rust interoperability that
// the planned `crates/zlib-rs-differential/tests/roundtrip_interop.rs` will assert at workspace
// scope. Their spellings match the ones the inflate suite already uses, so a
// reviewer can compare fixture for fixture.
//
// `clippy.toml` sets `allow-unwrap-in-tests`, `allow-expect-in-tests` and
// `allow-panic-in-tests`, so assertions here may panic; nothing above this line
// may.
// Fixture indexing: every index below is a literal into a fixture this module just built,
// so each one is provably in range. `clippy::indexing_slicing` is denied workspace-wide and
// is relaxed HERE ONLY, on the test module -- not through a clippy.toml key, which would be a
// field the 1.80 floor does not recognise and would abort the whole lint run.
#[allow(clippy::indexing_slicing)]
#[cfg(test)]
mod tests {
    use super::{uncompress, uncompress2, uncompress2_z, uncompress_z, Decompressed, MAX_CHUNK};
    use crate::error::ReturnCode;
    use alloc::vec;
    use alloc::vec::Vec;

    /// `compress2(b"hello, hello!", 9)`: the payload `test/example.c` uses, in a
    /// zlib container. 18 bytes in, 13 bytes out.
    const HELLO_ZLIB: &[u8] = &[
        0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x00, 0x21,
        0x70, 0x04, 0x96,
    ];

    /// The plaintext [`HELLO_ZLIB`], [`STORED_ZLIB`] and [`DICT_ZLIB`] expand to
    /// (`test/example.c` L44).
    const HELLO: &[u8] = b"hello, hello!";

    /// `compress2(b"", 6)`: a zlib wrapper around one empty fixed block. The
    /// shortest stream that decodes to nothing at all.
    const EMPTY_ZLIB: &[u8] = &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];

    /// `compress2(b"x", 6)`: the single-byte payload.
    const SINGLE_BYTE_ZLIB: &[u8] = &[0x78, 0x9c, 0xab, 0x00, 0x00, 0x00, 0x79, 0x00, 0x79];

    /// [`HELLO`] at level 0, i.e. one **stored** block in a zlib container. This
    /// is the path incompressible data takes, and it is the only path whose
    /// output length is decided by the block header rather than by a code.
    const STORED_ZLIB: &[u8] = &[
        0x78, 0x01, 0x01, 0x0d, 0x00, 0xf2, 0xff, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x2c, 0x20, 0x68,
        0x65, 0x6c, 0x6c, 0x6f, 0x21, 0x21, 0x70, 0x04, 0x96,
    ];

    /// [`HELLO`] compressed against the preset dictionary `b"hello"`
    /// (`test/example.c` L47), so its header sets `FDICT` and decoding it without
    /// the dictionary reaches `Z_NEED_DICT`.
    const DICT_ZLIB: &[u8] = &[
        0x78, 0xf9, 0x06, 0x2c, 0x02, 0x15, 0xcb, 0x00, 0x11, 0x3a, 0x0a, 0x60, 0x4a, 0x11, 0x00,
        0x21, 0x70, 0x04, 0x96,
    ];

    /// 200 copies of one byte as **raw** DEFLATE, i.e. with no zlib header. Fed
    /// to this module it must be rejected, because `inflateInit` asks for a
    /// container.
    const RLE_RAW: &[u8] = &[0x8b, 0x8a, 0x1a, 0x1e, 0x00, 0x00];

    /// `compress2(repeat_large(), 6)`: 174 bytes that expand to 100 000, so the
    /// decode crosses the 32 KiB window boundary many times over and every match
    /// after the first window is served from the window rather than from output.
    const REPEAT_LARGE_ZLIB: &str = "\
        78 9c ed c4 39 01 00 00 08 04 a0 ac e7 d3 bf 82 35 1c 60 20 d5 b3 91 24 49 92 24 49 92 24 \
        49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 \
        49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 \
        49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 \
        49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 \
        49 92 24 49 92 24 49 92 24 49 92 24 49 92 24 49 92 f4 b1 03 c5 69 18 ba";

    /// `compress2(random_256(), 9)`: 267 bytes for 256 bytes of input. Even at
    /// the highest level incompressible data becomes a stored block, which is
    /// visible in the fixture -- `01 00 01 ff fe` is `BFINAL | BTYPE=stored`,
    /// `LEN = 0x0100`, `NLEN = 0xfffe`.
    const RANDOM_ZLIB: &str = "\
        78 da 01 00 01 ff fe a0 44 29 f4 46 51 8d 6c 6b 2c b7 87 0b c2 60 64 71 e4 ae ff aa 30 96 \
        89 04 80 4f d3 b6 6d 8a 91 2a d7 2a 18 c9 37 e9 d7 37 a6 ae ec 82 02 c6 b1 67 bc 5c c3 22 \
        b9 4b 41 03 df f7 55 47 02 e1 33 aa 37 78 4e 95 dd 7e 79 7f 0f 14 67 88 af 78 d6 68 93 96 \
        24 4b b5 0d c1 89 be 8d 0d 6b 1e b5 44 aa a5 8d 77 d2 db a8 da 13 47 e6 a2 cb 93 36 66 7c \
        6a 11 b1 df 68 e1 dd b0 94 ae af ac ff 35 40 68 75 10 bb 77 97 4a 35 b5 3a 86 63 24 b8 91 \
        b5 42 30 37 c6 42 48 b2 d9 cc ac 29 eb 20 28 dd 8a f7 78 13 0f fb c3 aa b7 19 ba d6 3c 69 \
        cc 1b ac 57 33 b5 1d 22 e2 de 4f 81 cf 5c 41 05 be 54 69 0b 54 1e 8a 74 58 b3 46 c9 9f 15 \
        33 ff 37 6f 9a 97 49 79 8d af bf a8 9e 05 b6 85 41 79 19 1e 25 7f 46 91 f9 98 26 2f ff cb \
        5d a6 c2 e6 b9 4a ee 22 f1 93 30 3b 61 47 e5 eb 47 ac bd 64 0f b8 70 7c 29 7e 6a";

    /// `test/infcover.c` L36-L59 `h2b`: whitespace-separated hex bytes to bytes.
    /// Keeping the reference's own spelling is what lets a long fixture stay
    /// readable at the file's 100-column width.
    fn h2b(hex: &str) -> Vec<u8> {
        hex.split_whitespace()
            .map(|token| u8::from_str_radix(token, 16).expect("fixture is not hex"))
            .collect()
    }

    /// The 100 000 bytes [`REPEAT_LARGE_ZLIB`] expands to.
    fn repeat_large() -> Vec<u8> {
        (0..100_000).map(|i| b"abcde"[i % 5]).collect()
    }

    /// The 256 bytes [`RANDOM_ZLIB`] expands to: a 32-bit xorshift, which is the
    /// generator the fixture was produced from, so the expectation is reproducible
    /// without embedding the plaintext as well.
    fn random_256() -> Vec<u8> {
        let mut state = 0x0123_4567_u32;
        (0..256)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                (state & 0xff) as u8
            })
            .collect()
    }

    /// Runs [`uncompress2_z`] with a destination of exactly `capacity` bytes and
    /// returns the report together with the bytes it produced.
    fn run(source: &[u8], capacity: usize) -> (Decompressed, Vec<u8>) {
        let mut dest = vec![0_u8; capacity];
        let report = uncompress2_z(&mut dest, source);
        assert!(
            report.produced <= capacity,
            "produced {} exceeds the destination capacity {capacity}",
            report.produced
        );
        assert!(
            report.consumed <= source.len(),
            "consumed {} exceeds the source length {}",
            report.consumed,
            source.len()
        );
        dest.truncate(report.produced);
        (report, dest)
    }

    /// Asserts the whole measured triple at once.
    fn assert_report(report: Decompressed, code: ReturnCode, produced: usize, consumed: usize) {
        assert_eq!(report.code, code, "status: {report:?}");
        assert_eq!(report.produced, produced, "produced: {report:?}");
        assert_eq!(report.consumed, consumed, "consumed: {report:?}");
    }

    /// [`MAX_CHUNK`] is `(uInt)-1`, and it must stay representable as a `usize`.
    #[test]
    fn chunk_cap_matches_the_reference() {
        assert_eq!(MAX_CHUNK, 4_294_967_295);
        assert_eq!(u64::try_from(MAX_CHUNK).unwrap(), u64::from(u32::MAX));
    }

    /// A destination sized exactly to the result. Measured: `0 / 13 / 18`.
    #[test]
    fn hello_into_an_exact_destination() {
        let (report, out) = run(HELLO_ZLIB, HELLO.len());
        assert_report(report, ReturnCode::OK, 13, 18);
        assert_eq!(out, HELLO);
    }

    /// A destination with room to spare leaves the surplus untouched. Measured:
    /// `0 / 13 / 18`.
    #[test]
    fn hello_into_a_roomy_destination() {
        let mut dest = vec![0xcc_u8; 64];
        let report = uncompress2_z(&mut dest, HELLO_ZLIB);
        assert_report(report, ReturnCode::OK, 13, 18);
        assert_eq!(&dest[..13], HELLO);
        assert!(dest[13..].iter().all(|&byte| byte == 0xcc));
    }

    /// The empty payload. Measured: `0 / 0 / 8`.
    #[test]
    fn empty_payload() {
        let (report, out) = run(EMPTY_ZLIB, 64);
        assert_report(report, ReturnCode::OK, 0, 8);
        assert!(out.is_empty());
    }

    /// ★ An empty payload decodes into a **zero-length** destination and still
    /// succeeds. Measured: `0 / 0 / 8`. This is the case C's L42-L43 redirection
    /// exists to keep reachable, and it must not be short-circuited: the whole
    /// stream, check value included, has to be read to know the answer is
    /// `Z_OK`.
    #[test]
    fn empty_payload_into_a_zero_length_destination() {
        let report = uncompress2_z(&mut [], EMPTY_ZLIB);
        assert_report(report, ReturnCode::OK, 0, 8);
    }

    /// A single byte of payload. Measured: `0 / 1 / 9`.
    #[test]
    fn single_byte_payload() {
        let (report, out) = run(SINGLE_BYTE_ZLIB, 16);
        assert_report(report, ReturnCode::OK, 1, 9);
        assert_eq!(out, b"x");
    }

    /// A stored block, which is how incompressible input is carried. Measured:
    /// `0 / 13 / 24`.
    #[test]
    fn stored_block_payload() {
        let (report, out) = run(STORED_ZLIB, 32);
        assert_report(report, ReturnCode::OK, 13, 24);
        assert_eq!(out, HELLO);
    }

    /// Highly repetitive input: 174 compressed bytes for 100 000 plaintext, so
    /// the decode runs far past the 32 KiB window and every long match after the
    /// first window is served from the window. Measured: `0 / 100000 / 174`.
    #[test]
    fn highly_repetitive_payload_across_the_window() {
        let source = h2b(REPEAT_LARGE_ZLIB);
        let plain = repeat_large();
        assert_eq!(source.len(), 174);
        let (report, out) = run(&source, plain.len());
        assert_report(report, ReturnCode::OK, 100_000, 174);
        assert_eq!(out, plain);
    }

    /// Incompressible pseudo-random input, which even level 9 stores verbatim.
    /// Measured: `0 / 256 / 267`.
    #[test]
    fn incompressible_random_payload() {
        let source = h2b(RANDOM_ZLIB);
        let plain = random_256();
        assert_eq!(source.len(), 267);
        assert_eq!(
            &plain[..8],
            &[0xa0, 0x44, 0x29, 0xf4, 0x46, 0x51, 0x8d, 0x6c]
        );
        let (report, out) = run(&source, plain.len());
        assert_report(report, ReturnCode::OK, 256, 267);
        assert_eq!(out, plain);
    }

    /// ★ Trailing bytes after a complete stream are **not** consumed: the count
    /// stops at the stream's own last byte, which is what makes
    /// `source + consumed` the first unused input byte. Measured: `0 / 13 / 18`
    /// for a 25-byte source.
    #[test]
    fn trailing_data_is_left_unconsumed() {
        let mut source = HELLO_ZLIB.to_vec();
        source.extend_from_slice(&[0xab; 7]);
        let (report, out) = run(&source, 64);
        assert_report(report, ReturnCode::OK, 13, 18);
        assert_eq!(out, HELLO);
        assert_eq!(&source[report.consumed..], &[0xab; 7]);
    }

    /// The same, with a destination sized exactly to the result, so the decode
    /// finishes with no output room to spare. Measured: `0 / 13 / 18`.
    #[test]
    fn trailing_data_with_an_exact_destination() {
        let mut source = HELLO_ZLIB.to_vec();
        source.extend_from_slice(&[0xab; 7]);
        let (report, out) = run(&source, HELLO.len());
        assert_report(report, ReturnCode::OK, 13, 18);
        assert_eq!(out, HELLO);
    }

    /// Two streams back to back: the first call stops at the boundary, and
    /// restarting from `consumed` decodes the second. This is the guarantee the
    /// unconsumed-tail contract is for.
    #[test]
    fn a_second_stream_starts_where_the_first_stopped() {
        let mut source = HELLO_ZLIB.to_vec();
        source.extend_from_slice(SINGLE_BYTE_ZLIB);
        let (first, out) = run(&source, 64);
        assert_report(first, ReturnCode::OK, 13, 18);
        assert_eq!(out, HELLO);

        let (second, out) = run(&source[first.consumed..], 64);
        assert_report(second, ReturnCode::OK, 1, 9);
        assert_eq!(out, b"x");
    }

    /// ★ One byte short. The result is `Z_BUF_ERROR`, the destination holds every
    /// byte that fitted, and input is left over -- which is precisely what keeps
    /// the status out of the truncated-stream arm. Measured: `-5 / 12 / 13`.
    #[test]
    fn destination_one_byte_too_small() {
        let (report, out) = run(HELLO_ZLIB, HELLO.len() - 1);
        assert_report(report, ReturnCode::BUF_ERROR, 12, 13);
        assert_eq!(out, b"hello, hello");
        assert!(
            report.consumed < HELLO_ZLIB.len(),
            "input must be left over"
        );
    }

    /// The same for a stored block, whose copy loop reaches the limit by a
    /// different route. Measured: `-5 / 12 / 19`.
    #[test]
    fn stored_block_destination_one_byte_too_small() {
        let (report, out) = run(STORED_ZLIB, HELLO.len() - 1);
        assert_report(report, ReturnCode::BUF_ERROR, 12, 19);
        assert_eq!(out, b"hello, hello");
    }

    /// A destination that holds nothing at all, against a stream that produces
    /// something. Measured: `-5 / 0 / 4` -- four bytes are consumed because the
    /// decoder fills its bit accumulator before it discovers it has nowhere to
    /// write, and the leftover input is what makes this `Z_BUF_ERROR` rather than
    /// `Z_DATA_ERROR`.
    #[test]
    fn zero_length_destination_probes_for_buf_error() {
        let report = uncompress2_z(&mut [], HELLO_ZLIB);
        assert_report(report, ReturnCode::BUF_ERROR, 0, 4);
    }

    /// Halving the destination repeatedly never produces more than it can hold,
    /// never consumes more than the source, and never panics.
    #[test]
    fn every_destination_size_is_consistent() {
        for capacity in 0..=HELLO.len() + 2 {
            let (report, out) = run(HELLO_ZLIB, capacity);
            let expected = capacity.min(HELLO.len());
            assert_eq!(report.produced, expected, "capacity {capacity}: {report:?}");
            assert_eq!(&out[..], &HELLO[..expected], "capacity {capacity}");
            let expected_code = if capacity >= HELLO.len() {
                ReturnCode::OK
            } else {
                ReturnCode::BUF_ERROR
            };
            assert_eq!(
                report.code, expected_code,
                "capacity {capacity}: {report:?}"
            );
        }
    }

    /// ★ A stream one byte short of its check value. Every input byte is
    /// consumed, so the residual is zero and `inflate`'s `Z_BUF_ERROR` becomes
    /// `Z_DATA_ERROR` -- the "incomplete zlib stream" of `uncompr.c` L24-L25.
    /// Note the payload is nonetheless fully decoded. Measured: `-3 / 13 / 17`.
    #[test]
    fn truncated_before_the_check_value() {
        let source = &HELLO_ZLIB[..HELLO_ZLIB.len() - 1];
        let (report, out) = run(source, 64);
        assert_report(report, ReturnCode::DATA_ERROR, 13, 17);
        assert_eq!(out, HELLO);
        assert_eq!(report.consumed, source.len(), "all input must be consumed");
    }

    /// Truncated with the whole four-byte check value missing. Measured:
    /// `-3 / 13 / 13`.
    #[test]
    fn truncated_without_the_check_value() {
        let source = &HELLO_ZLIB[..HELLO_ZLIB.len() - 5];
        let (report, out) = run(source, 64);
        assert_report(report, ReturnCode::DATA_ERROR, 13, 13);
        assert_eq!(out, HELLO);
    }

    /// Truncated mid-payload, so nothing at all is produced. Measured:
    /// `-3 / 0 / 2`.
    #[test]
    fn truncated_to_the_header() {
        let (report, out) = run(&HELLO_ZLIB[..2], 64);
        assert_report(report, ReturnCode::DATA_ERROR, 0, 2);
        assert!(out.is_empty());
    }

    /// Truncated to a single byte, i.e. half a zlib header. Measured:
    /// `-3 / 0 / 1`.
    #[test]
    fn truncated_to_one_byte() {
        assert_report(run(&HELLO_ZLIB[..1], 64).0, ReturnCode::DATA_ERROR, 0, 1);
    }

    /// ★ An empty source. Nothing is consumed and nothing is produced, so the
    /// residual is zero and the answer is `Z_DATA_ERROR`, not `Z_BUF_ERROR`: an
    /// empty buffer is an incomplete stream. Measured: `-3 / 0 / 0`, and
    /// independently of the destination size.
    #[test]
    fn empty_source_is_an_incomplete_stream() {
        assert_report(run(&[], 64).0, ReturnCode::DATA_ERROR, 0, 0);
        assert_report(uncompress2_z(&mut [], &[]), ReturnCode::DATA_ERROR, 0, 0);
    }

    /// Every prefix of a valid stream either decodes it or reports
    /// `Z_DATA_ERROR`; none reports `Z_BUF_ERROR`, because a roomy destination
    /// cannot fill up, and none panics.
    #[test]
    fn every_truncation_reports_data_error() {
        for length in 0..HELLO_ZLIB.len() {
            let (report, _out) = run(&HELLO_ZLIB[..length], 64);
            assert_eq!(
                report.code,
                ReturnCode::DATA_ERROR,
                "prefix of {length} bytes: {report:?}"
            );
            assert_eq!(report.consumed, length, "prefix of {length} bytes");
        }
        assert_eq!(run(HELLO_ZLIB, 64).0.code, ReturnCode::OK);
    }

    /// A flipped byte inside the compressed data. The decoder still writes
    /// thirteen bytes -- the wrong ones -- and the check value catches it.
    /// Measured: `-3 / 13 / 18` with output `helk7, helk7!`.
    #[test]
    fn a_corrupt_payload_byte_is_caught_by_the_check_value() {
        let mut source = HELLO_ZLIB.to_vec();
        source[6] ^= 0xff;
        let (report, out) = run(&source, 64);
        assert_report(report, ReturnCode::DATA_ERROR, 13, 18);
        assert_eq!(out, b"helk7, helk7!");
    }

    /// A flipped byte in the check value alone: the payload decodes correctly and
    /// is still rejected. Measured: `-3 / 13 / 18`.
    #[test]
    fn a_corrupt_check_value_is_rejected() {
        let mut source = HELLO_ZLIB.to_vec();
        let last = source.len() - 1;
        source[last] ^= 0xff;
        let (report, out) = run(&source, 64);
        assert_report(report, ReturnCode::DATA_ERROR, 13, 18);
        assert_eq!(out, HELLO, "the payload itself was intact");
    }

    /// A header whose check bits do not divide by 31. Measured: `-3 / 0 / 2`.
    #[test]
    fn an_invalid_header_is_rejected() {
        let mut source = HELLO_ZLIB.to_vec();
        source[0] = 0x00;
        assert_report(run(&source, 64).0, ReturnCode::DATA_ERROR, 0, 2);
    }

    /// Raw DEFLATE has no container, and this entry point insists on one, because
    /// C reaches it through plain `inflateInit`. Measured: `-3 / 0 / 2`.
    #[test]
    fn raw_deflate_is_rejected() {
        assert_report(run(RLE_RAW, 256).0, ReturnCode::DATA_ERROR, 0, 2);
    }

    /// ★ A stream compressed against a preset dictionary. `inflate` answers
    /// `Z_NEED_DICT`, but this entry point has no way to supply one, so the
    /// second arm of the ladder converts it to `Z_DATA_ERROR`. Measured:
    /// `-3 / 0 / 6` -- the two header bytes plus the four-byte dictionary
    /// identifier.
    #[test]
    fn a_preset_dictionary_stream_becomes_a_data_error() {
        let (report, out) = run(DICT_ZLIB, 64);
        assert_report(report, ReturnCode::DATA_ERROR, 0, 6);
        assert!(out.is_empty());
    }

    /// The conversion is independent of the destination size: with no room at
    /// all, `Z_NEED_DICT` still arrives before anything could be written, so the
    /// second arm still wins over the third. Measured: `-3 / 0 / 6`.
    #[test]
    fn a_preset_dictionary_stream_with_a_zero_length_destination() {
        let report = uncompress2_z(&mut [], DICT_ZLIB);
        assert_report(report, ReturnCode::DATA_ERROR, 0, 6);
    }

    /// All four entry points agree, on success and on both kinds of failure,
    /// because all four reach the same body. Measured for each of
    /// `uncompress`, `uncompress_z`, `uncompress2` and `uncompress2_z`.
    #[test]
    fn the_wrappers_agree_with_the_primary() {
        for (source, capacity, code, produced, consumed) in [
            (HELLO_ZLIB, 64, ReturnCode::OK, 13, 18),
            (HELLO_ZLIB, 12, ReturnCode::BUF_ERROR, 12, 13),
            (HELLO_ZLIB, 0, ReturnCode::BUF_ERROR, 0, 4),
            (EMPTY_ZLIB, 0, ReturnCode::OK, 0, 8),
            (DICT_ZLIB, 64, ReturnCode::DATA_ERROR, 0, 6),
            (RLE_RAW, 64, ReturnCode::DATA_ERROR, 0, 2),
        ] {
            let expected = Decompressed {
                code,
                produced,
                consumed,
            };
            let mut dest = vec![0_u8; capacity];
            assert_eq!(uncompress2_z(&mut dest, source), expected, "uncompress2_z");
            let mut dest = vec![0_u8; capacity];
            assert_eq!(uncompress2(&mut dest, source), expected, "uncompress2");
            let mut dest = vec![0_u8; capacity];
            assert_eq!(uncompress_z(&mut dest, source), expected, "uncompress_z");
            let mut dest = vec![0_u8; capacity];
            assert_eq!(uncompress(&mut dest, source), expected, "uncompress");
        }
    }

    /// The four entry points write identical bytes, not merely identical counts.
    #[test]
    fn the_wrappers_produce_identical_bytes() {
        let source = h2b(REPEAT_LARGE_ZLIB);
        let expected = repeat_large();
        for decode in [uncompress, uncompress_z, uncompress2, uncompress2_z] {
            let mut dest = vec![0_u8; expected.len()];
            let report = decode(&mut dest, &source);
            assert_report(report, ReturnCode::OK, expected.len(), source.len());
            assert_eq!(dest, expected);
        }
    }

    /// The set of statuses `uncompr.c` L22-L25 documents. `Z_STREAM_ERROR` is
    /// absent on purpose: the guard that produces it cannot be expressed over
    /// slices, and the initialiser cannot fail for the fixed `DEF_WBITS`.
    fn assert_documented(report: Decompressed, source_len: usize, capacity: usize) {
        assert!(
            matches!(
                report.code,
                ReturnCode::OK
                    | ReturnCode::MEM_ERROR
                    | ReturnCode::BUF_ERROR
                    | ReturnCode::DATA_ERROR
            ),
            "undocumented status: {report:?}"
        );
        assert!(report.produced <= capacity, "over-wrote: {report:?}");
        assert!(report.consumed <= source_len, "over-read: {report:?}");
    }

    /// The second bytes [`every_two_byte_input_terminates`] pairs with `first`.
    ///
    /// Natively all 256 of them, so the sweep is exhaustive over the 65 536
    /// two-byte inputs.
    ///
    /// Under Miri only two: the smallest `FLG` that makes `CMF << 8 | FLG` a
    /// multiple of 31 -- a well-formed header -- and its successor, which is not.
    /// Miri interprets rather than executes, so an exhaustive sweep there would
    /// run for days, and what the reduction drops is repetition rather than
    /// coverage: paired with every `first`, these two still reach all sixteen
    /// method nibbles, all sixteen window nibbles and both verdicts of the
    /// header's check-value test, which is every branch two bytes can select.
    /// `FDICT` is reached under Miri by the two preset-dictionary tests above.
    fn second_bytes(first: u8) -> Vec<u8> {
        if cfg!(miri) {
            // `zlib.h`-conformant headers satisfy `(CMF * 256 + FLG) % 31 == 0`
            // (RFC 1950 section 2.2), so this is the residue that completes one.
            let residue = (31 - (usize::from(first) * 256) % 31) % 31;
            let valid = u8::try_from(residue).expect("a residue modulo 31 fits in a byte");
            vec![valid, valid.wrapping_add(1)]
        } else {
            (0..=u8::MAX).collect()
        }
    }

    /// ★ Every two-byte input. A zlib header is two bytes, so this covers every
    /// reachable header -- valid, invalid, dictionary-requesting and
    /// reserved-method alike -- and asserts only that the call returns a
    /// documented status without panicking. This is the cheap in-crate precursor
    /// to the planned `fuzz/fuzz_targets/fuzz_inflate.rs`.
    #[test]
    fn every_two_byte_input_terminates() {
        let mut dest = [0_u8; 32];
        for first in 0..=u8::MAX {
            for second in second_bytes(first) {
                let source = [first, second];
                assert_documented(uncompress2_z(&mut dest, &source), 2, dest.len());
            }
        }
    }

    /// Pseudo-random inputs of many lengths, against destinations of many sizes,
    /// including zero in both dimensions. Deterministic, so a failure is
    /// reproducible.
    #[test]
    fn arbitrary_input_terminates() {
        let mut state = 0x9e37_79b9_u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for length in [0_usize, 1, 2, 3, 5, 8, 13, 21, 34, 55, 89, 144, 233] {
            for capacity in [0_usize, 1, 7, 64, 1024] {
                for _ in 0..24 {
                    let source: Vec<u8> = (0..length).map(|_| (next() & 0xff) as u8).collect();
                    let mut dest = vec![0_u8; capacity];
                    let report = uncompress2_z(&mut dest, &source);
                    assert_documented(report, length, capacity);
                }
            }
        }
    }

    /// A valid header followed by random bytes: the decoder is entered for real
    /// and then fed nonsense, which is the shape that reaches the deepest states.
    #[test]
    fn a_valid_header_followed_by_noise_terminates() {
        let mut state = 0x2545_f491_u32;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        for length in [0_usize, 1, 2, 4, 9, 33, 128] {
            for _ in 0..64 {
                let mut source = vec![0x78, 0x9c];
                source.extend((0..length).map(|_| (next() & 0xff) as u8));
                let mut dest = vec![0_u8; 512];
                let report = uncompress2_z(&mut dest, &source);
                assert_documented(report, source.len(), 512);
            }
        }
    }

    /// Every single-byte truncation of every fixture, against every small
    /// destination size. A cheap sweep over the interaction between the two exit
    /// conditions the ladder has to tell apart.
    #[test]
    fn every_prefix_of_every_fixture_terminates() {
        let repeat = h2b(REPEAT_LARGE_ZLIB);
        let random = h2b(RANDOM_ZLIB);
        let fixtures: [&[u8]; 7] = [
            HELLO_ZLIB,
            EMPTY_ZLIB,
            SINGLE_BYTE_ZLIB,
            STORED_ZLIB,
            DICT_ZLIB,
            &repeat,
            &random,
        ];
        for fixture in fixtures {
            for length in 0..=fixture.len() {
                for capacity in [0_usize, 1, 13, 300] {
                    let mut dest = vec![0_u8; capacity];
                    let report = uncompress2_z(&mut dest, &fixture[..length]);
                    assert_documented(report, length, capacity);
                }
            }
        }
    }
}
