//! One-shot compression of a whole buffer: the Rust counterpart of `compress.c`.
//!
//! Six entry points sit on top of the streaming encoder, and only one of them
//! contains any logic. `compress2_z` (`compress.c` L24-L66) drives [`deflate`]
//! to completion over a caller-owned source and destination; `compress2`,
//! `compress_z` and `compress` (L67-L85) are one-line adapters, and the two
//! bound functions (L91-L99) are pure arithmetic. This module reproduces all
//! six, because `zlib.h` declares all six -- `compress` at L1271, `compress_z`
//! at L1273, `compress2` at L1288, `compress2_z` at L1291, `compressBound` at
//! L1307 and `compressBound_z` at L1308 -- and the exported symbol set is
//! frozen.
//!
//! # The contract, from `compress.c` L11-L23 and `zlib.h` L1294-L1312
//!
//! `source` is the buffer to compress and `level` has the same meaning as in
//! `deflateInit`. On entry the destination is the total room available, which
//! "must be at least the value returned by `compressBound(sourceLen)`"; on exit
//! the reported count is the actual size of the compressed data. The documented
//! outcomes are [`ReturnCode::OK`], [`ReturnCode::MEM_ERROR`],
//! [`ReturnCode::BUF_ERROR`] when the destination had no room left, and
//! [`ReturnCode::STREAM_ERROR`] when `level` is invalid.
//!
//! Every stream this module produces is a **zlib** container (RFC 1950) with the
//! largest window and the default memory level, because C calls plain
//! `deflateInit` at L42 rather than `deflateInit2`. Raw DEFLATE and gzip members
//! are not reachable from here, exactly as they are not reachable from the
//! reference, and a caller that wants either drives [`deflate`] itself.
//!
//! # ★ `compress_bound_z` is a buffer-overflow hazard, not a hint
//!
//! Callers size their output buffer with [`compress_bound_z`] and then hand that
//! buffer to [`compress2_z`]. A bound that is too small becomes a buffer
//! overflow **in caller code**, which this library cannot detect and cannot
//! contain; a bound that is too large breaks callers and tests that assert exact
//! sizes. The arithmetic at L92-L94 is therefore transcribed operator for
//! operator -- the shifts are 12, 14 and 25, the addend is 13, the additions
//! wrap, and the overflow test is `bound < sourceLen` -- and every expectation
//! in the test module was measured against the reference build rather than
//! derived from the formula.
//!
//! The comment above the C function (L87-L90) ties the constants to the encoder
//! configuration: "If the default `memLevel` or `windowBits` for `deflateInit()`
//! is changed, then this function needs to be updated." Since every function
//! here uses the `deflateInit` defaults, the formula is valid as written. That is
//! also why [`compress2_z`] takes a level rather than a whole
//! [`crate::config::DeflateConfig`]: accepting one would let a
//! caller pick a configuration the bound is not valid for, and the mismatch
//! would surface as a silent overflow in *their* code.
//!
//! # ★ The chunking loop is behavioural, not incidental
//!
//! `z_size_t` is 64 bits wide on LP64 while `avail_in` and `avail_out` are
//! `uInt`, so C feeds the encoder in pieces of at most `MAX_CHUNK` bytes
//! (L28, L52 and L56). Reproducing that on a 64-bit target is not dead weight
//! and not an optimisation: it fixes the exact sequence of `avail_in` and
//! `avail_out` values that [`deflate`] observes, and [`deflate`]'s block-boundary
//! and pending-buffer decisions are defined against that sequence. Collapsing
//! the loop into a single call would change the emitted bytes for inputs above
//! 4 GiB and break the byte-identity criterion, so the loop is kept whatever the
//! buffer sizes are.
//!
//! The flush selector at L60 is the other half of that fidelity: it reads the
//! **residual** source length *after* the top-up, so every chunk but the last is
//! offered [`Flush::NoFlush`] and the last is offered [`Flush::Finish`].
//!
//! # What the safe core does not do
//!
//! Three parts of `compress2_z` exist only because C has raw pointers, and all
//! three belong to `crates/libz-rs-sys` rather than here:
//!
//! * The null guard at L31-L33 returns `Z_STREAM_ERROR` when `destLen` is
//!   `Z_NULL`, when a non-zero `sourceLen` is paired with a null `source`, or
//!   when a non-zero `*destLen` is paired with a null `dest`. A `&[u8]` is never
//!   null and carries its own length, so all three conditions are
//!   unrepresentable here; the facade must apply the guard before it builds the
//!   slices it passes in. Note what the guard does **not** cover: an empty
//!   destination with a non-empty source is perfectly legal input, and it must
//!   reach [`ReturnCode::BUF_ERROR`] through the encoder rather than being
//!   special-cased. See [`compress2_z`] on the zero-length destination.
//! * The `uLong`-versus-`z_size_t` split. `uLong` is 32 bits wide on LLP64
//!   Windows and 64 bits wide on LP64 -- `zlib.h` L1268 says so outright, "Note
//!   that a long is 32 bits on Windows" -- so C needs two spellings of every
//!   function. The safe core works in the `usize` domain only, so the `_z` forms
//!   are **primary** here and the `uLong` forms are their aliases; the width
//!   mapping, the narrowing at L72, and the `(uLong)-1` saturation at L98 all
//!   live in the facade's `types.rs`. [`compress_bound`] spells out exactly which
//!   saturation is the facade's.
//! * `stream.zalloc`, `stream.zfree` and `stream.opaque` are zeroed at L38-L40,
//!   which selects the library's internal allocator. That is
//!   [`GlobalAllocator`], the Rust counterpart of `zcalloc`/`zcfree`, and it is wired in
//!   unconditionally: these six functions are not generic over the allocator
//!   because the C functions they mirror offer callers no way to supply one.
//!
//! # Layering and safety posture
//!
//! `no_std`, and allocation-free in itself -- whatever the encoder needs it takes
//! from [`GlobalAllocator`]. The module names only `core` plus three sibling
//! modules, holds no raw pointer, and needs no escape hatch from the compiler's
//! memory-safety guarantees, so the crate root's blanket prohibition on them
//! costs it nothing.
//!
//! Nothing here can panic. Every window is obtained with a checked accessor,
//! every subtraction saturates, no fallible operation is unwrapped, and the
//! bound arithmetic wraps by request. The loop terminates on every input; the
//! argument is in [`compress2_z`].
//!
//! # The interface this module consumes
//!
//! Four items come from [`crate::deflate`], and they are the whole of this
//! module's coupling to the encoder: [`deflate_init`] (the Rust counterpart of `deflateInit`,
//! `deflate.c` L379-L384), [`DeflateStream`] (the caller-visible half of
//! `z_stream`, the compression-side counterpart of
//! [`crate::inflate::InflateStream`]), [`deflate`] (the driver,
//! `deflate.c` L981-L1292) and [`deflate_end`] (`deflate.c` L1293-L1310). They
//! are used in exactly one function, [`compress2_z`], and in the same order C
//! uses them.
//!
//! The shapes relied on, stated so that the coupling is checkable in one place
//! rather than inferred from the call site:
//!
//! ```text
//! deflate_init(level: i32, allocator: A) -> Result<DeflateState<'_, A>, ReturnCode>
//! DeflateStream::new(input: &[u8], output: &mut [u8]) -> DeflateStream<'_>
//!     .next_in / .next_out : usize     cursors, both writable
//!     .avail_in() / .avail_out()       the two derived counts
//!     .total_in / .total_out : u64     carried across calls
//!     .msg / .adler / .data_type       ditto; `adler` is the RFC 1950 check value
//! deflate(&mut DeflateState<'_, A>, &mut DeflateStream<'_>, flush: i32) -> ReturnCode
//! deflate_end(DeflateState<'_, A>) -> ReturnCode
//! ```
//!
//! These mirror `inflate_init`, [`crate::inflate::InflateStream`],
//! `inflate` and `inflate_end` one for one, which is deliberate: the two one-shot
//! wrappers are read side by side, and the encoder's driver has to group the same
//! nine pieces of caller state the decoder's does -- `clippy.toml` caps a function
//! at eight arguments, so the stream object is a requirement rather than a
//! preference.
//!
//! # Examples
//!
//! ```
//! use zlib_rs::compress::{compress2_z, compress_bound_z};
//! use zlib_rs::error::ReturnCode;
//!
//! let plain = b"hello, hello!";
//!
//! // Size the destination the way `zlib.h` L1298-L1299 instructs.
//! let mut buffer = [0_u8; 64];
//! let room = compress_bound_z(plain.len());
//! assert!(room <= buffer.len());
//! let report = compress2_z(&mut buffer[..room], plain, 9);
//!
//! assert_eq!(report.code, ReturnCode::OK);
//! assert_eq!(report.produced, 18);
//! assert!(report.produced <= room);
//!
//! // A destination one byte short of the whole stream still reports how far it
//! // got, because the count is published before the status is translated.
//! let mut cramped = [0_u8; 17];
//! let report = compress2_z(&mut cramped, plain, 9);
//! assert_eq!(report.code, ReturnCode::BUF_ERROR);
//! assert_eq!(report.produced, 17);
//! ```
//!
//! # Provenance
//!
//! The one-shot compression wrappers.
//!
//! Ported from `compress.c`: `compress`, `compress2`, `compressBound` and their `_z`
//! `size_t`-aware forms, which are the primary implementations.
//!
//! [`deflate_end`]: crate::deflate::deflate_end
//! [`deflate_init`]: crate::deflate::deflate_init

// The definitions closing the module documentation above are link-reference definitions, not
// prose: a module whose `mod` declaration carries an outer doc comment has its `//!` block
// resolved in the scope of the DECLARING module, so an unqualified sibling name does not
// resolve. See "Documentation lints" in `src/lib.rs` for the rule and the gate.

// Names that repeat their module's name are deliberate here: the C sources this module ports name
// these entry points, and `crates/zlib-rs/src/lib.rs` re-exports several of them under exactly
// these names, so renaming any of them to satisfy `clippy::module_name_repetitions` would cost the
// traceability the port is judged on. The lint sits in `pedantic`, which this workspace denies, and
// it fires on the declared 1.80 floor; upstream has since reclassified it, so the allowance is what
// keeps the same lint gate passing on both toolchains. The same relaxation, for the same reason,
// already appears in `config.rs`, `deflate/**`, `inflate/**` and `gz/**`.
#![allow(clippy::module_name_repetitions)]

use crate::allocate::GlobalAllocator;
use crate::config::{Flush, Z_DEFAULT_COMPRESSION};
use crate::deflate::{deflate, deflate_end, deflate_init, DeflateStream};
use crate::error::ReturnCode;
use crate::read_buf::OutputRegion;

/// The bound arithmetic shifts a length right by 25 (`compress.c` L93), so
/// `usize` has to be wide enough for that to be defined.
///
/// C reaches the same guarantee differently: `sourceLen >> 25` on a 16-bit
/// `size_t` is undefined behaviour there too, and the reference simply is not
/// built for such a target. Stating it as a compile-time assertion turns a
/// silent miscompilation into a build failure, which is the same trade
/// `crate::deflate::pending` makes for its own width assumption.
const _: () = assert!(
    usize::BITS >= 32,
    "compress.c's bound arithmetic shifts by 25, so usize must be at least 32 bits wide"
);

/// The largest number of bytes handed to the encoder in one call.
///
/// Mirrors `const uInt max = (uInt)-1` (`compress.c` L28). `uInt` is C's
/// `unsigned int`, so the value is 32 bits of ones: the widest `avail_in` or
/// `avail_out` a `z_stream` can express. It exists because `z_size_t` is 64 bits
/// wide on LP64 while `avail_in` and `avail_out` are not, so a buffer larger than
/// 4 GiB has to be fed to the encoder in pieces.
///
/// ★ Keeping the cap is not an optimisation and not dead weight on smaller
/// buffers: it fixes the exact sequence of `avail_in`/`avail_out` values that
/// [`deflate`] observes, and [`deflate`]'s block-boundary and pending-buffer
/// decisions -- and therefore the bytes it emits -- are defined against that
/// sequence. See the module documentation.
const MAX_CHUNK: usize = u32::MAX as usize;

/// Everything one call of the `compress` family reports to its caller.
///
/// C writes one of these two through a pointer and returns the other. The safe
/// core has no pointer to write through, so both come back together.
///
/// ★ The count is meaningful on **every** outcome, not just on success, because
/// `compress.c` L63 assigns `*destLen` unconditionally and before the status is
/// translated at L65. A destination one byte short of the whole stream therefore
/// reports [`ReturnCode::BUF_ERROR`] together with every byte that did fit --
/// measured, not inferred. A facade writes the count back unconditionally.
///
/// There is deliberately no `consumed` field, unlike
/// [`Decompressed`](crate::uncompress::Decompressed): `compress2_z` takes its
/// source length by value and exposes no out-parameter for it, so publishing one
/// would add surface that `zlib.h` does not declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[must_use = "the status reports an invalid level, a short destination and allocation failure"]
pub struct Compressed {
    /// The status, i.e. C's `int` return value.
    pub code: ReturnCode,
    /// Bytes written to the destination, i.e. `*destLen` on exit.
    ///
    /// Never greater than the destination's length. The bytes themselves are the
    /// destination's leading `produced` bytes; the rest is untouched.
    pub produced: usize,
}

/// Compresses `source` into `dest` at `level`, reporting how much was produced.
///
/// The Rust counterpart of `compress2_z` (`compress.c` L24-L66), declared at `zlib.h` L1291.
/// This is the only entry point in the module with a body; the other three
/// delegate to it, directly or through each other.
///
/// `level` is `deflateInit`'s level: `0 ..= 9`, or
/// [`Z_DEFAULT_COMPRESSION`] (-1) for the default of 6. It is **not** validated
/// here, because C does not validate it here either -- `deflateInit` does, and
/// forwarding its status verbatim is what makes an invalid level report
/// [`ReturnCode::STREAM_ERROR`].
///
/// # The encoder configuration
///
/// L42 calls plain `deflateInit`, which supplies `Z_DEFLATED`, `MAX_WBITS` (15),
/// `DEF_MEM_LEVEL` (8) and `Z_DEFAULT_STRATEGY` on the caller's behalf
/// (`deflate.c` L379-L383). Every stream is therefore a zlib container with a
/// 32 KiB window, which is also the only configuration
/// [`compress_bound_z`]'s constants are valid for.
///
/// # The loop
///
/// `compress.c` L50-L61, reproduced in shape:
///
/// * Both availabilities start at zero (L46 and L48) and are topped up from the
///   residuals at the head of each iteration -- output first, then input, in
///   that order, each capped at `MAX_CHUNK` (`compress.c` L28).
/// * The flush mode is [`Flush::Finish`] once the **residual** source length has
///   reached zero and [`Flush::NoFlush`] until then (L60). Reading the residual
///   *after* the top-up is what gives the final chunk -- and, for a source that
///   fits in one chunk, the very first call -- the terminal flush.
/// * The loop continues for as long as [`deflate`] answers [`ReturnCode::OK`].
///
/// Each iteration hands [`deflate`] a **prefix** of `source` and of `dest`, sized
/// so that the cursors leave exactly the `avail_in` and `avail_out` C would have
/// described with its four `z_stream` members. The encoder never reads behind
/// either cursor -- all of its input arrives through `read_buf` and all of its
/// output leaves through `flush_pending` -- so exposing the prefix rather than
/// only the current window changes nothing, and it keeps the produced count
/// available as a plain cursor position.
///
/// ★ **Termination.** The loop cannot spin, for the same reason the reference's
/// cannot. An iteration that continues has [`ReturnCode::OK`], and [`deflate`]
/// answers that only after advancing a cursor: with [`Flush::NoFlush`] the
/// residual is non-zero, so the top-up leaves `avail_in > 0` and the encoder
/// drains it into the window; with [`Flush::Finish`] it either writes output,
/// or reports [`ReturnCode::STREAM_END`] because the stream is complete, or
/// reports [`ReturnCode::BUF_ERROR`] because `avail_out` is zero -- which is the
/// guard at `deflate.c` L1016-L1017, and the reason an exhausted destination
/// ends the loop instead of prolonging it. Both cursors are bounded by their
/// buffers, so the loop runs at most `source.len() + dest.len() + 2` times on any
/// input whatsoever.
///
/// # Returns
///
/// * [`ReturnCode::OK`] -- the whole stream is in `dest`, `produced` bytes of it.
/// * [`ReturnCode::BUF_ERROR`] -- `dest` filled up first. The leading `produced`
///   bytes are a valid *prefix* of the stream, but a prefix is not a stream: it
///   has no trailer and cannot be decompressed.
/// * [`ReturnCode::STREAM_ERROR`] -- `level` is outside `-1 ..= 9`. Reported with
///   `produced == 0`.
/// * [`ReturnCode::MEM_ERROR`] -- the encoder could not allocate. Also reported
///   with `produced == 0`.
///
/// ★ The two zero-count cases are not incidental. L35-L36 sets the residual from
/// the destination's length and then clears the out-parameter **before**
/// `deflateInit` runs, so a failure there leaves the caller's `*destLen` at zero.
/// That is the opposite of [`uncompress2_z`](crate::uncompress::uncompress2_z),
/// which returns its entry values on the same path, and the asymmetry is in the
/// two C functions rather than in this implementation.
///
/// # The zero-length destination
///
/// An empty `dest` is legal input, not an error, and it must not be
/// short-circuited: the loop runs, [`deflate`] finds `avail_out == 0` and reports
/// [`ReturnCode::BUF_ERROR`]. That holds even for an empty `source`, because the
/// shortest zlib stream is still eight bytes long -- measured against the
/// reference, which answers `Z_BUF_ERROR` for an empty source into a zero-length
/// destination. An implementation that special-cased the empty source into
/// [`ReturnCode::OK`] would produce no stream and claim success.
pub fn compress2_z(dest: &mut [u8], source: &[u8], level: i32) -> Compressed {
    compress2_z_into(&mut OutputRegion::init(dest), source, level)
}

/// [`compress2_z`] over a destination that may be write-only storage.
///
/// The entry point `crates/libz-rs-sys` uses, because a C caller's `dest` is guaranteed
/// writable and nothing more -- `zlib.h` L1281-L1289 asks for `*destLen` bytes of room and
/// says nothing about their contents. Identical in behaviour to [`compress2_z`], which is a
/// one-line forwarder to it over an [`OutputRegion::init`].
///
/// ★ `#[allow(clippy::unnecessary_min_or_max)]` is target-dependent, not a waived defect.
/// `MAX_CHUNK` is `u32::MAX as usize`, so on a 32-bit target it *equals* `usize::MAX` and the
/// two `.min(MAX_CHUNK)` caps below provably have no effect -- which is what the lint reports.
/// On a 64-bit target the same caps are load-bearing: they are what reproduces C's chunking of
/// a `z_size_t` request into `uInt`-sized pieces (`compress.c` L28). Deleting them to satisfy the 32-bit
/// build would therefore break the 64-bit one, so the lint is allowed here, scoped to this
/// function, exactly as `narrow_checksum` in `crates/libz-rs-sys/src/checksum.rs` scopes the
/// three lints its own width conversion trips on 32-bit targets.
#[allow(clippy::unnecessary_min_or_max)]
pub fn compress2_z_into(dest: &mut OutputRegion<'_>, source: &[u8], level: i32) -> Compressed {
    // Captured before the first reborrow of `dest`, and the origin of every bound
    // below. These are C's entry values of `*destLen` and `sourceLen`.
    let dest_len = dest.len();
    let source_len = source.len();

    // L35-L36: `left = *destLen; *destLen = 0;`
    //
    // `left` is the destination room not yet offered to the encoder and `len` is
    // C's `sourceLen` after it becomes the residual input -- neither is a
    // produced or consumed count. Clearing the out-parameter is expressed by
    // every early return below reporting `produced: 0`.
    let mut left = dest_len;
    let mut len = source_len;

    // L38-L43. `deflateInit`, with `zalloc`/`zfree`/`opaque` zeroed, i.e. the
    // library's own allocator.
    //
    // C returns here without having written the out-parameter, which L36 has
    // already set to zero -- so the caller sees a zero count, not its entry
    // value. Both statuses `deflateInit` can produce, `Z_STREAM_ERROR` for an
    // invalid level and `Z_MEM_ERROR` for a failed allocation, are forwarded
    // unchanged.
    let mut state = match deflate_init(level, GlobalAllocator) {
        Ok(state) => state,
        Err(code) => return Compressed { code, produced: 0 },
    };

    // L45-L48: both cursors at the base of their buffer, both availabilities
    // empty so that the first iteration fills them.
    let mut next_in = 0_usize;
    let mut avail_in = 0_usize;
    let mut next_out = 0_usize;
    let mut avail_out = 0_usize;

    // C reuses one `z_stream` for every call, so these five members carry
    // forward. The safe core rebuilds the view each iteration and therefore has
    // to carry them by hand. `adler` is the one that matters: it is the running
    // Adler-32 that `read_buf` folds each input byte into and that [`deflate`]
    // writes verbatim into the RFC 1950 trailer, so dropping it between
    // iterations would corrupt the trailer of every stream longer than one
    // chunk. The other four are write-only from this function's point of view,
    // but reproducing C's single-stream accounting costs nothing and keeps the
    // mirror honest.
    let mut total_in = 0_u64;
    let mut total_out = 0_u64;
    let mut msg = None;
    let mut adler = 0_u32;
    let mut data_type = 0_i32;

    // L50-L61.
    let err = loop {
        // L51-L54: top up the output window, then L55-L59: the input window. The
        // order is C's, and it is observable -- the flush selector below reads
        // the input residual that L58 has just decremented. Neither
        // `saturating_sub` can saturate, because each chunk is a `min` against
        // the residual it is taken from.
        if avail_out == 0 {
            avail_out = left.min(MAX_CHUNK);
            left = left.saturating_sub(avail_out);
        }
        if avail_in == 0 {
            avail_in = len.min(MAX_CHUNK);
            len = len.saturating_sub(avail_in);
        }

        // The windows themselves. `next_in + avail_in` never exceeds `source_len`
        // and `next_out + avail_out` never exceeds `dest_len`: each top-up moves
        // exactly as much as it removes from the residual, so
        // `next_out + avail_out + left == dest_len` holds at every step, and
        // likewise for the input. The clamps are therefore belt and braces; with
        // them, neither accessor can fail whatever the arithmetic above produced.
        let in_end = next_in.saturating_add(avail_in).min(source_len);
        let out_end = next_out.saturating_add(avail_out).min(dest_len);
        let Some(input) = source.get(..in_end) else {
            // Unreachable by the invariant just stated. Reported rather than
            // asserted so that this function stays panic-free on every path;
            // `Z_BUF_ERROR` is the honest status for "no window could be
            // offered", it is one of the documented outcomes, and L65 passes it
            // through unchanged.
            break ReturnCode::BUF_ERROR;
        };

        // ★ The output window starts **at** `next_out` and the stream's own cursor starts at
        // zero, rather than the window starting at the destination's base with the cursor
        // pre-positioned. The two are equivalent for the compressor, which never reads its
        // output back, and this shape is what keeps [`OutputRegion`]'s promise about its
        // write-only variant exact: every write a sub-region performs is at or after its own
        // base, so "everything below the high-water mark has been written" needs no appeal to
        // what an earlier iteration did.
        let mut window = dest.reborrow(next_out, out_end.saturating_sub(next_out));
        let mut stream = DeflateStream::with_region(input, window.reborrow(0, window.len()));
        stream.next_in = next_in;
        stream.total_in = total_in;
        stream.total_out = total_out;
        stream.msg = msg;
        stream.adler = adler;
        stream.data_type = data_type;

        // L60: `err = deflate(&stream, sourceLen ? Z_NO_FLUSH : Z_FINISH);`
        let flush = if len == 0 {
            Flush::Finish
        } else {
            Flush::NoFlush
        };
        // `deflate` takes the C `int`, not the typed mode: it is the driver C
        // callers reach directly, so it owns the `flush > Z_BLOCK` rejection at
        // `deflate.c` L985 and performs the conversion itself through
        // `crate::config::validate_deflate_flush`. `Flush::as_raw` is the one
        // spelling of that `int`, so the selector above stays typed.
        let err = deflate(&mut state, &mut stream, flush.as_raw());

        // Whether this call moved either cursor; only the termination assertion
        // below reads it.
        let advanced = stream.next_in != next_in || stream.next_out != 0;

        next_in = stream.next_in;
        next_out = next_out.saturating_add(stream.next_out);
        avail_in = stream.avail_in();
        avail_out = stream.avail_out();
        total_in = stream.total_in;
        total_out = stream.total_out;
        msg = stream.msg;
        adler = stream.adler;
        data_type = stream.data_type;

        // L61: `} while (err == Z_OK);`
        if err != ReturnCode::OK {
            break err;
        }

        // The termination invariant, in the debug-only form this implementation uses for
        // C's `Assert`. An iteration that continues must have moved a cursor, or
        // else have run the output window dry -- in which case the next iteration
        // either takes a fresh chunk from `left` or meets `deflate`'s
        // `avail_out == 0` guard and ends the loop. A violation would be an
        // encoder that reports progress without making any, which is the only way
        // this loop could fail to terminate.
        debug_assert!(
            advanced || avail_out == 0,
            "deflate returned Z_OK without advancing a cursor or exhausting the output window"
        );
    };

    // L63: `*destLen = (z_size_t)(stream.next_out - dest);`
    //
    // Unconditional, and before the status translation: the count is as
    // meaningful on `Z_BUF_ERROR` as it is on success. `next_out` is that pointer
    // difference, having been carried across every iteration.
    let produced = next_out;

    // L64: `deflateEnd(&stream);`
    //
    // Unconditional, and the result is discarded exactly as C discards it. This
    // is deliberate rather than an oversight to be tidied up: `deflateEnd`
    // legitimately answers `Z_DATA_ERROR` when a stream is freed before it was
    // finished (`deflate.c` L1309), which is precisely the state a `Z_BUF_ERROR`
    // return leaves it in, and surfacing that would replace this function's own
    // documented status with one `zlib.h` L1302-L1304 does not list for it.
    let _end = deflate_end(&mut state);

    // L65: `return err == Z_STREAM_END ? Z_OK : err;`
    //
    // The single translation. Every other status -- `Z_BUF_ERROR`,
    // `Z_STREAM_ERROR`, `Z_MEM_ERROR` -- passes through untouched.
    let code = if err == ReturnCode::STREAM_END {
        ReturnCode::OK
    } else {
        err
    };

    Compressed { code, produced }
}

/// Compresses `source` into `dest` at `level`, reporting how much was produced.
///
/// The Rust counterpart of `compress2` (`compress.c` L67-L74), declared at `zlib.h` L1288.
/// C's body widens the caller's `uLong` destination length to `z_size_t`, calls
/// [`compress2_z`], and narrows the result back.
///
/// In the safe core there is nothing to widen or narrow: lengths are `usize`
/// throughout, so this is [`compress2_z`] under its other name. The width mapping
/// it exists for is the facade's, in `types.rs`, where `uLong` becomes `c_ulong`
/// and the narrowing at L72 truncates on exactly the targets C's own `(uLong)`
/// cast truncates on. Both spellings are kept so that each exported symbol has
/// one core function behind it.
pub fn compress2(dest: &mut [u8], source: &[u8], level: i32) -> Compressed {
    compress2_z(dest, source, level)
}

/// Compresses `source` into `dest` at the default level.
///
/// The Rust counterpart of `compress_z` (`compress.c` L77-L81), declared at `zlib.h` L1273:
/// [`compress2_z`] with [`Z_DEFAULT_COMPRESSION`], which `deflateInit` resolves
/// to level 6.
pub fn compress_z(dest: &mut [u8], source: &[u8]) -> Compressed {
    compress2_z(dest, source, Z_DEFAULT_COMPRESSION)
}

/// Compresses `source` into `dest` at the default level.
///
/// The Rust counterpart of `compress` (`compress.c` L82-L85), declared at `zlib.h` L1271.
/// This is the entry point most callers use -- `test/example.c` L71 among them --
/// and the prose at `zlib.h` L1275-L1286 is written about it. `zlib.h` L1280-L1281
/// states the equivalence this function implements: "`compress()` is equivalent
/// to `compress2()` with a level parameter of `Z_DEFAULT_COMPRESSION`".
///
/// ★ It delegates to [`compress2`], **not** to [`compress_z`]. C's chain is
/// `compress` -> `compress2` -> `compress2_z`: the plain form takes the `uLong`
/// route and only the `_z` form takes the `z_size_t` route. The two routes are
/// observationally identical in the safe core, so the chain is reproduced rather
/// than tidied -- a reader checking where the `uLong` narrowing happens must find
/// the same answer here as in `compress.c`. `uncompress` is asymmetric with this
/// in the reference too, and equally deliberately.
pub fn compress(dest: &mut [u8], source: &[u8]) -> Compressed {
    compress2(dest, source, Z_DEFAULT_COMPRESSION)
}

/// An upper bound on the compressed size of `source_len` bytes.
///
/// The Rust counterpart of `compressBound_z` (`compress.c` L91-L95), declared at `zlib.h`
/// L1308. `zlib.h` L1310-L1312: it "returns an upper bound on the compressed size
/// after `compress()` or `compress2()` on `sourceLen` bytes. It would be used
/// before a `compress()` or `compress2()` call to allocate the destination
/// buffer."
///
/// ```text
/// z_size_t bound = sourceLen + (sourceLen >> 12) + (sourceLen >> 14) +
///                  (sourceLen >> 25) + 13;
/// return bound < sourceLen ? (z_size_t)-1 : bound;
/// ```
///
/// ★ **The four constants are 12, 14, 25 and 13, and the arithmetic is
/// transcribed rather than reformulated.** A single wrong digit is a
/// caller-visible buffer overflow, because the caller allocates from this number
/// and then writes into that allocation. Three properties of the C expression are
/// load-bearing and are preserved exactly:
///
/// * **The additions wrap.** They are unsigned in C, so an overflowing sum
///   truncates rather than trapping, and the overflow test below depends on
///   observing the truncated value. [`usize::wrapping_add`] is that operator;
///   `checked_add` would saturate on a different -- larger -- set of inputs,
///   because it rejects an overflow of any intermediate sum whereas C only ever
///   examines the final one.
/// * **The summation order is C's, left to right.** The four terms are added in
///   the order they are written, so any intermediate wrap happens at the same
///   point. Reassociating is not safe under wrapping arithmetic even though it
///   would be over the integers.
/// * **The overflow test is a comparison, not a flag.** `bound < source_len` is
///   exact here: the three shifted terms plus 13 always sum to less than `2^BITS`
///   for any `source_len`, so a wrap can only ever subtract `2^BITS` once, which
///   necessarily lands the result below `source_len`.
///
/// The sentinel is C's `(z_size_t)-1`, i.e. [`usize::MAX`] -- the value that says
/// "no buffer can be guaranteed large enough". Note that it is also the honest
/// answer for `source_len == usize::MAX` itself, which is why saturating there
/// loses nothing.
///
/// The constants are tied to the encoder configuration by the comment at L87-L90,
/// "If the default `memLevel` or `windowBits` for `deflateInit()` is changed, then
/// this function needs to be updated." Every function in this module uses those
/// defaults, so the bound is valid as written and must not be parameterised. The
/// per-stream equivalent, which does account for a non-default configuration and
/// for a gzip or raw wrapper, is `deflate_bound_z` in [`crate::deflate`] -- a
/// different function with different constants, and not a substitute for this
/// one.
///
/// # Examples
///
/// ```
/// use zlib_rs::compress::compress_bound_z;
///
/// assert_eq!(compress_bound_z(0), 13);
/// assert_eq!(compress_bound_z(4096), 4096 + 1 + 13);
/// assert_eq!(compress_bound_z(usize::MAX), usize::MAX);
/// ```
#[must_use]
pub const fn compress_bound_z(source_len: usize) -> usize {
    // `sourceLen + (sourceLen >> 12) + (sourceLen >> 14) + (sourceLen >> 25) + 13`
    // (`compress.c` L92-L93), term for term and in order.
    let bound = source_len
        .wrapping_add(source_len >> 12)
        .wrapping_add(source_len >> 14)
        .wrapping_add(source_len >> 25)
        .wrapping_add(13);

    // `return bound < sourceLen ? (z_size_t)-1 : bound;` (`compress.c` L94).
    if bound < source_len {
        usize::MAX
    } else {
        bound
    }
}

/// An upper bound on the compressed size of `source_len` bytes.
///
/// The Rust counterpart of `compressBound` (`compress.c` L96-L99), declared at `zlib.h`
/// L1307. C's body computes the bound in the `z_size_t` domain and then narrows:
///
/// ```text
/// z_size_t bound = compressBound_z(sourceLen);
/// return (uLong)bound != bound ? (uLong)-1 : (uLong)bound;
/// ```
///
/// ★ **That narrowing is the facade's, not this function's, and the split is
/// exact.** The safe core has one length domain, `usize`, so
/// `(usize)bound != bound` is false for every input and this is
/// [`compress_bound_z`] under its other name. The saturation only has anything to
/// do on a target where `uLong` is narrower than `z_size_t` -- LLP64 Windows,
/// where `unsigned long` is 32 bits and `size_t` is 64 -- and there it is
/// `crates/libz-rs-sys/src/compress.rs`, in its `compressBound` export, that
/// converts the argument from `c_ulong`, calls this function, and answers
/// `c_ulong::MAX` when the result does not fit back. Hard-coding either width
/// here would be wrong on the other one.
///
/// Both spellings are kept so that each exported symbol has one core function
/// behind it, and so that a facade author reading this module can see which of
/// the two saturations they own: this one, and the `*destLen` narrowing in
/// [`compress2`].
#[must_use]
pub const fn compress_bound(source_len: usize) -> usize {
    compress_bound_z(source_len)
}

// Every expectation below was MEASURED against the reference implementation --
// the in-tree C sources built as `libz.a` -- by calling `compress2_z` or
// `compressBound_z` on the same fixture and recording the resulting
// `(int, *destLen)` pair or the returned bound. None of them is derived from the
// formula or from the algorithm, because the point of the suite is to catch a
// divergence that looks reasonable. The bound table in particular is the one
// place where a plausible-looking transcription error becomes a buffer overflow
// in caller code, so its expectations are transcribed from the reference's own
// output rather than recomputed here.
//
// Byte-for-byte equality against the C encoder across the whole
// level x windowBits x memLevel x strategy x flush matrix is
// the planned `crates/zlib-rs-differential/tests/byte_identical.rs`'s job. What this module
// owns is the wrapper: the bound arithmetic, the chunking loop's shape, the
// accounting, and the four status outcomes. The fixtures here are exact output
// bytes even so, because for the `deflateInit` defaults they cost nothing and
// they catch a wrapper bug -- a dropped `adler`, a mis-ordered top-up, a wrong
// flush selector -- immediately and locally.
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
    use super::{
        compress, compress2, compress2_z, compress_bound, compress_bound_z, compress_z, Compressed,
        MAX_CHUNK,
    };
    use crate::error::ReturnCode;
    // Test-only, and the only place this module reaches outside its own
    // dependencies: the round trips below prove that what the encoder emits the
    // decoder accepts, which is the property a length assertion alone cannot
    // establish.
    use crate::uncompress::uncompress2_z;
    use alloc::vec;
    use alloc::vec::Vec;

    /// The payload `test/example.c` L35 uses. "'hello world' would be more
    /// standard, but the repeated 'hello' exercises the compression code better",
    /// says the comment at L36-L38.
    const HELLO: &[u8] = b"hello, hello!";

    /// [`HELLO`] as `test/example.c` L69 actually passes it: `strlen(hello) + 1`,
    /// so the terminating NUL is part of the payload.
    const HELLO_NUL: &[u8] = b"hello, hello!\0";

    /// `compress2_z(HELLO, 0)`: one **stored** block in a zlib container, which is
    /// the path level 0 always takes. Measured: 24 bytes.
    const HELLO_LEVEL_0: &[u8] = &[
        0x78, 0x01, 0x01, 0x0d, 0x00, 0xf2, 0xff, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x2c, 0x20, 0x68,
        0x65, 0x6c, 0x6c, 0x6f, 0x21, 0x21, 0x70, 0x04, 0x96,
    ];

    /// `compress2_z(HELLO, 1)`. Measured: 18 bytes.
    const HELLO_LEVEL_1: &[u8] = &[
        0x78, 0x01, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x00, 0x21,
        0x70, 0x04, 0x96,
    ];

    /// `compress2_z(HELLO, 6)`, i.e. also `compress_z(HELLO)`. Measured: 18 bytes.
    const HELLO_LEVEL_6: &[u8] = &[
        0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x00, 0x21,
        0x70, 0x04, 0x96,
    ];

    /// `compress2_z(HELLO, 9)`. Measured: 18 bytes, differing from
    /// [`HELLO_LEVEL_6`] in the header's second byte alone -- at this size the
    /// three levels find the same matches and only the advertised compression
    /// class changes.
    const HELLO_LEVEL_9: &[u8] = &[
        0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x00, 0x21,
        0x70, 0x04, 0x96,
    ];

    /// `compress(HELLO_NUL)`: the exact call `test/example.c` L71 makes. Measured:
    /// 19 bytes.
    const HELLO_NUL_DEFAULT: &[u8] = &[
        0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c, 0x00,
        0x26, 0x06, 0x04, 0x96,
    ];

    /// `compress_z(b"")`: a zlib wrapper around one empty fixed block, and the
    /// shortest stream that decodes to nothing at all. Measured: 8 bytes.
    const EMPTY_DEFAULT: &[u8] = &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];

    /// `compress2_z(b"", 0)`: the same payload as one empty **stored** block, so
    /// three bytes longer. Measured: 11 bytes.
    const EMPTY_LEVEL_0: &[u8] = &[
        0x78, 0x01, 0x01, 0x00, 0x00, 0xff, 0xff, 0x00, 0x00, 0x00, 0x01,
    ];

    /// `compress2_z(&[b'a'; 1000], 1)`. Measured: 19 bytes. Levels 1 to 3 share
    /// this output; levels 4 and up find the longer match and reach 17.
    const THOUSAND_A_LEVEL_1: &[u8] = &[
        0x78, 0x01, 0x4b, 0x4c, 0x1c, 0x05, 0xa3, 0x21, 0x30, 0x1a, 0x02, 0xc3, 0x3d, 0x04, 0x00,
        0xf9, 0xd8, 0x7a, 0xf8,
    ];

    /// `compress2_z(&[b'a'; 1000], 6)`. Measured: 17 bytes.
    const THOUSAND_A_LEVEL_6: &[u8] = &[
        0x78, 0x9c, 0x4b, 0x4c, 0x1c, 0x05, 0xa3, 0x60, 0x14, 0x0c, 0x77, 0x00, 0x00, 0xf9, 0xd8,
        0x7a, 0xf8,
    ];

    /// `compress2_z(&[b'a'; 1000], 9)`. Measured: 17 bytes.
    const THOUSAND_A_LEVEL_9: &[u8] = &[
        0x78, 0xda, 0x4b, 0x4c, 0x1c, 0x05, 0xa3, 0x60, 0x14, 0x0c, 0x77, 0x00, 0x00, 0xf9, 0xd8,
        0x7a, 0xf8,
    ];

    /// The header's second byte per level, measured for levels 0 to 9.
    ///
    /// `deflate()` packs `level_flags` into bits 6 and 7 of it -- 0 below level 2,
    /// 1 below level 6, 2 at level 6, 3 above -- and then adjusts the whole word
    /// so that it is a multiple of 31. Asserting it per level is the cheapest
    /// available check that the level actually reached the encoder.
    const FLG_PER_LEVEL: [u8; 10] = [0x01, 0x01, 0x5e, 0x5e, 0x5e, 0x5e, 0x9c, 0xda, 0xda, 0xda];

    /// `compress2_z` output lengths for [`seven_cycle`], measured for levels 0
    /// to 9. The input is 70 000 bytes, so it spans more than two 32 KiB windows
    /// and the encoder slides its window several times before finishing.
    const SEVEN_CYCLE_LENGTHS: [usize; 10] = [70_016, 408, 408, 408, 132, 132, 132, 132, 132, 132];

    /// 1000 copies of `b'a'`, the payload behind the three `THOUSAND_A_*`
    /// fixtures.
    fn thousand_a() -> Vec<u8> {
        vec![b'a'; 1000]
    }

    /// 70 000 bytes cycling through seven letters, the payload behind
    /// [`SEVEN_CYCLE_LENGTHS`]. Highly compressible, and long enough to exercise
    /// the window slide and several block boundaries.
    fn seven_cycle() -> Vec<u8> {
        (0..70_000).map(|i| b"abcdefg"[i % 7]).collect()
    }

    /// 512 bytes from the linear congruential generator the incompressible
    /// fixture was produced from, so the expectation is reproducible without
    /// embedding the plaintext. `state = state * 1103515245 + 12345`, taking bits
    /// 16 to 23 -- the classic ANSI C `rand()` recurrence.
    fn incompressible_512() -> Vec<u8> {
        let mut state = 0x1234_5678_u32;
        (0..512)
            .map(|_| {
                state = state.wrapping_mul(1_103_515_245).wrapping_add(12_345);
                // Bits 16 to 23, i.e. `(state >> 16) & 0xff`, spelled without a
                // narrowing cast.
                state.to_le_bytes()[2]
            })
            .collect()
    }

    /// Runs [`compress2_z`] with a destination of exactly `capacity` bytes and
    /// returns the report together with the bytes it produced.
    fn run(source: &[u8], capacity: usize, level: i32) -> (Compressed, Vec<u8>) {
        let mut dest = vec![0_u8; capacity];
        let report = compress2_z(&mut dest, source, level);
        assert!(
            report.produced <= capacity,
            "produced {} exceeds the destination capacity {capacity}",
            report.produced
        );
        dest.truncate(report.produced);
        (report, dest)
    }

    /// Runs [`compress2_z`] into a destination sized exactly as `zlib.h`
    /// L1298-L1299 instructs, which is the way every real caller uses it.
    fn run_at_bound(source: &[u8], level: i32) -> (Compressed, Vec<u8>) {
        run(source, compress_bound_z(source.len()), level)
    }

    /// Asserts the whole measured pair at once.
    fn assert_report(report: Compressed, code: ReturnCode, produced: usize) {
        assert_eq!(report.code, code, "status: {report:?}");
        assert_eq!(report.produced, produced, "produced: {report:?}");
    }

    /// Compresses at `level` and asserts that the decoder gives the input back.
    fn assert_round_trip(source: &[u8], level: i32) {
        let (report, stream) = run_at_bound(source, level);
        assert_report(report, ReturnCode::OK, stream.len());

        let mut plain = vec![0_u8; source.len() + 16];
        let back = uncompress2_z(&mut plain, &stream);
        assert_eq!(back.code, ReturnCode::OK, "level {level}: {back:?}");
        assert_eq!(back.consumed, stream.len(), "level {level}: {back:?}");
        assert_eq!(&plain[..back.produced], source, "level {level}");
    }

    /// [`MAX_CHUNK`] is `(uInt)-1`, and it must stay representable as a `usize`.
    #[test]
    fn chunk_cap_matches_the_reference() {
        assert_eq!(MAX_CHUNK, 4_294_967_295);
        assert_eq!(u64::try_from(MAX_CHUNK).unwrap(), u64::from(u32::MAX));
    }

    /// The reference's own answers for nineteen lengths spanning every shift
    /// threshold. `13`, `4096`, `16384` and `1 << 25` are the points at which the
    /// constant and the three shifted terms first contribute.
    ///
    /// Every row here is representable at any pointer width. The one measurement that
    /// is not -- 2^32, whose bound exceeds `usize::MAX` on a 32-bit target -- is
    /// asserted separately below under `cfg`, because a literal wider than `usize` is
    /// rejected by `overflowing_literals` at compile time and so cannot be skipped at
    /// run time.
    #[test]
    fn bound_matches_the_reference_table() {
        const TABLE: [(usize, usize); 19] = [
            (0, 13),
            (1, 14),
            (2, 15),
            (12, 25),
            (13, 26),
            (4_095, 4_108),
            (4_096, 4_110),
            (4_097, 4_111),
            (8_191, 8_205),
            (8_192, 8_207),
            (16_383, 16_399),
            (16_384, 16_402),
            (16_385, 16_403),
            (65_535, 65_566),
            (65_536, 65_569),
            (1_000_000, 1_000_318),
            (1_048_576, 1_048_909),
            (33_554_431, 33_564_682),
            (33_554_432, 33_564_686),
        ];

        for (source_len, expected) in TABLE {
            assert_eq!(
                compress_bound_z(source_len),
                expected,
                "compress_bound_z({source_len})"
            );
        }

        // The twentieth measurement, 2^32, exercises a length above the `>> 25` term's
        // own range. It exists only where `usize` can hold it.
        #[cfg(target_pointer_width = "64")]
        assert_eq!(
            compress_bound_z(4_294_967_296),
            4_296_278_157,
            "compress_bound_z(4294967296)"
        );
    }

    /// The four terms, spelled out at the thresholds the table above pins down.
    /// This is the same arithmetic read the other way round: from the shift
    /// constants rather than from the reference's output, so a transposed digit
    /// cannot satisfy both.
    #[test]
    fn bound_is_the_sum_of_the_four_documented_terms() {
        assert_eq!(compress_bound_z(0), 13);
        assert_eq!(compress_bound_z(1), 1 + 13);
        assert_eq!(compress_bound_z(4_095), 4_095 + 13);
        assert_eq!(compress_bound_z(4_096), 4_096 + 1 + 13);
        assert_eq!(compress_bound_z(16_384), 16_384 + 4 + 1 + 13);
        assert_eq!(
            compress_bound_z(1 << 25),
            (1 << 25) + (1 << 13) + (1 << 11) + 1 + 13
        );
        // 2^54 is far enough up that all three shifted terms are large, which is
        // where a swapped pair of shift constants would finally show. It needs a
        // 64-bit `usize`; the 2^25 case above is the widest that fits a 32-bit one,
        // and it already has all four terms contributing.
        #[cfg(target_pointer_width = "64")]
        assert_eq!(
            compress_bound_z(1 << 54),
            (1 << 54) + (1 << 42) + (1 << 40) + (1 << 29) + 13
        );
    }

    /// ★ The saturation sentinel, at and near the top of the range. Measured:
    /// both `usize::MAX` and `usize::MAX - 1` come back as `usize::MAX`, and so
    /// does every length whose bound would not fit.
    #[test]
    fn bound_saturates_on_overflow() {
        assert_eq!(compress_bound_z(usize::MAX), usize::MAX);
        assert_eq!(compress_bound_z(usize::MAX - 1), usize::MAX);
        assert_eq!(compress_bound_z(usize::MAX - 12), usize::MAX);

        // The largest length whose bound still fits is far below `usize::MAX`,
        // because the bound grows faster than the length. Either side of the
        // crossing must answer consistently: below it a real bound, above it the
        // sentinel, and never a value smaller than the length itself.
        //
        // `usize::BITS - 1` rather than a literal 63, so the top-bit case is the top
        // bit at whatever width the target has; every entry is then representable and
        // no shift exceeds the word.
        let top_bit = 1_usize << (usize::BITS - 1);
        for &source_len in &[usize::MAX / 2, top_bit, top_bit + 1, usize::MAX - (1 << 20)] {
            let bound = compress_bound_z(source_len);
            assert!(
                bound == usize::MAX || bound > source_len,
                "compress_bound_z({source_len}) = {bound}: neither sentinel nor upper bound"
            );
        }

        // Measured values immediately below the crossing. These are readings from the
        // reference LP64 build, so they are asserted only where `usize` is 64 bits --
        // both the arguments and the answers exceed a 32-bit `usize`, and no 32-bit
        // reference measurement exists in this repository to put in their place. The
        // property assertions above hold at every width and are what covers the others.
        #[cfg(target_pointer_width = "64")]
        {
            assert_eq!(compress_bound_z(usize::MAX / 2), 9_226_187_061_499_789_321);
            assert_eq!(compress_bound_z(1_usize << 63), 9_226_187_061_499_789_325);
            assert_eq!(compress_bound_z(1_usize << 54), 18_019_896_604_491_789);
        }
    }

    /// The bound is never below its input plus the 13-byte constant, and it never
    /// decreases as the input grows. Both properties are what makes it usable as
    /// an allocation size, and neither holds if a term is dropped or a shift is
    /// applied to the wrong operand.
    #[test]
    fn bound_is_monotonic_and_never_short() {
        let mut previous = 0;
        for source_len in (0..70_000).step_by(97) {
            let bound = compress_bound_z(source_len);
            assert!(
                bound >= source_len + 13,
                "compress_bound_z({source_len}) = {bound} is below the incompressible floor"
            );
            assert!(
                bound >= previous,
                "compress_bound_z({source_len}) = {bound} is below the previous {previous}"
            );
            previous = bound;
        }
    }

    /// The narrow spelling agrees with the wide one everywhere in the `usize`
    /// domain, which is what "the saturation is the facade's" means concretely.
    /// Measured against the reference on this LP64 target, where `uLong` and
    /// `z_size_t` are both 64 bits: `compressBound(0)` is 13,
    /// `compressBound(13)` is 26, and `compressBound(ULONG_MAX)` is `ULONG_MAX`.
    #[test]
    fn bound_narrow_form_agrees_in_the_usize_domain() {
        assert_eq!(compress_bound(0), 13);
        assert_eq!(compress_bound(13), 26);
        assert_eq!(compress_bound(usize::MAX), usize::MAX);
        for source_len in (0..40_000).step_by(311) {
            assert_eq!(compress_bound(source_len), compress_bound_z(source_len));
        }
    }

    /// The four levels the agent brief names, byte for byte. Measured:
    /// `0 / 24`, `0 / 18`, `0 / 18`, `0 / 18`.
    #[test]
    fn hello_matches_the_reference_bytes_at_the_four_levels() {
        for (level, expected) in [
            (0, HELLO_LEVEL_0),
            (1, HELLO_LEVEL_1),
            (6, HELLO_LEVEL_6),
            (9, HELLO_LEVEL_9),
        ] {
            let (report, stream) = run_at_bound(HELLO, level);
            assert_report(report, ReturnCode::OK, expected.len());
            assert_eq!(stream, expected, "level {level}");
            assert!(
                stream.len() <= compress_bound_z(HELLO.len()),
                "level {level} exceeded the bound"
            );
        }
    }

    /// Every level round trips, and the produced length never exceeds the bound.
    #[test]
    fn hello_round_trips_at_every_level() {
        for level in 0..=9 {
            assert_round_trip(HELLO, level);
        }
        assert_round_trip(HELLO, -1);
    }

    /// The zlib header's second byte encodes the level class, so it is the
    /// cheapest proof that `level` reached the encoder rather than being dropped
    /// on the way. Measured for all ten levels.
    #[test]
    fn header_flag_byte_matches_the_reference_per_level() {
        for (index, &expected_flg) in FLG_PER_LEVEL.iter().enumerate() {
            let level = i32::try_from(index).unwrap();
            let (report, stream) = run_at_bound(HELLO, level);
            assert_eq!(report.code, ReturnCode::OK, "level {level}");
            assert_eq!(stream[0], 0x78, "level {level}: CMF");
            assert_eq!(stream[1], expected_flg, "level {level}: FLG");
            // RFC 1950: the two header bytes, read as a big-endian word, must be
            // a multiple of 31.
            let header = (u32::from(stream[0]) << 8) | u32::from(stream[1]);
            assert_eq!(header % 31, 0, "level {level}: header check");
        }
    }

    /// The exact call `test/example.c` L71 makes, through the same entry point:
    /// `compress(compr, &comprLen, hello, strlen(hello) + 1)`. Measured:
    /// `0 / 19`. The NUL is one more byte of payload, and it changes the output,
    /// which is why the fixture differs from [`HELLO_LEVEL_6`].
    #[test]
    fn example_c_payload_matches_the_reference() {
        let mut dest = vec![0_u8; compress_bound_z(HELLO_NUL.len())];
        let report = compress(&mut dest, HELLO_NUL);
        assert_report(report, ReturnCode::OK, HELLO_NUL_DEFAULT.len());
        assert_eq!(&dest[..report.produced], HELLO_NUL_DEFAULT);

        let mut plain = vec![0_u8; 64];
        let back = uncompress2_z(&mut plain, &dest[..report.produced]);
        assert_eq!(back.code, ReturnCode::OK);
        assert_eq!(&plain[..back.produced], HELLO_NUL);
    }

    /// All four spellings of "compress at the default level" agree, which is the
    /// delegation chain `compress` -> `compress2` -> `compress2_z` and
    /// `compress_z` -> `compress2_z` doing what `compress.c` L77-L85 says.
    #[test]
    fn default_level_spellings_agree() {
        let capacity = compress_bound_z(HELLO.len());

        let mut a = vec![0_u8; capacity];
        let mut b = vec![0_u8; capacity];
        let mut c = vec![0_u8; capacity];
        let mut d = vec![0_u8; capacity];

        let ra = compress(&mut a, HELLO);
        let rb = compress_z(&mut b, HELLO);
        let rc = compress2(&mut c, HELLO, -1);
        let rd = compress2_z(&mut d, HELLO, -1);

        assert_report(ra, ReturnCode::OK, HELLO_LEVEL_6.len());
        assert_eq!(rb, ra);
        assert_eq!(rc, ra);
        assert_eq!(rd, ra);
        assert_eq!(&a[..ra.produced], HELLO_LEVEL_6);
        assert_eq!(a, b);
        assert_eq!(a, c);
        assert_eq!(a, d);
    }

    /// A destination with room to spare leaves the surplus untouched, and the
    /// report says exactly how far the stream reaches into it.
    #[test]
    fn roomy_destination_leaves_the_surplus_untouched() {
        let mut dest = vec![0xcc_u8; 512];
        let report = compress2_z(&mut dest, HELLO, 6);
        assert_report(report, ReturnCode::OK, HELLO_LEVEL_6.len());
        assert_eq!(&dest[..report.produced], HELLO_LEVEL_6);
        assert!(dest[report.produced..].iter().all(|&byte| byte == 0xcc));
    }

    /// A destination of exactly `compress_bound_z(len)` always succeeds -- that is
    /// the promise `zlib.h` L1298-L1299 makes to every caller, across the whole
    /// small corpus and every level.
    #[test]
    fn exact_bound_destination_always_succeeds() {
        let thousand = thousand_a();
        let random = incompressible_512();
        let corpus: [&[u8]; 6] = [
            b"",
            b"x",
            HELLO,
            HELLO_NUL,
            thousand.as_slice(),
            random.as_slice(),
        ];

        for source in corpus {
            for level in 0..=9 {
                let (report, stream) = run_at_bound(source, level);
                assert_eq!(
                    report.code,
                    ReturnCode::OK,
                    "level {level}, {} source bytes: {report:?}",
                    source.len()
                );
                assert!(
                    stream.len() <= compress_bound_z(source.len()),
                    "level {level}: {} bytes exceeds the bound {}",
                    stream.len(),
                    compress_bound_z(source.len())
                );
            }
        }
    }

    /// An empty source is a real corpus case, and it produces a real stream:
    /// eight bytes at the default level, eleven at level 0. Measured.
    #[test]
    fn empty_source_produces_the_reference_stream() {
        let (report, stream) = run(b"", 64, -1);
        assert_report(report, ReturnCode::OK, EMPTY_DEFAULT.len());
        assert_eq!(stream, EMPTY_DEFAULT);

        let (report, stream) = run(b"", 64, 0);
        assert_report(report, ReturnCode::OK, EMPTY_LEVEL_0.len());
        assert_eq!(stream, EMPTY_LEVEL_0);
    }

    /// And the stream it produces decodes back to nothing at all, at every level.
    #[test]
    fn empty_source_round_trips() {
        for level in 0..=9 {
            let (report, stream) = run_at_bound(b"", level);
            assert_eq!(report.code, ReturnCode::OK, "level {level}");

            let mut plain = [0_u8; 16];
            let back = uncompress2_z(&mut plain, &stream);
            assert_eq!(back.code, ReturnCode::OK, "level {level}: {back:?}");
            assert_eq!(back.produced, 0, "level {level}: {back:?}");
            assert_eq!(back.consumed, stream.len(), "level {level}: {back:?}");
        }
    }

    /// ★ A destination one byte short of the whole stream reports
    /// [`ReturnCode::BUF_ERROR`] together with **every byte that did fit** --
    /// measured: `-5 / 17` for an 18-byte stream, not `-5 / 0`. `compress.c` L63
    /// assigns the count before L65 translates the status, so the count is
    /// meaningful on failure; an implementation that reported zero here would look tidier and
    /// would be wrong.
    #[test]
    fn destination_one_byte_short_reports_buf_error_with_the_partial_count() {
        let full = HELLO_LEVEL_9.len();
        let (report, stream) = run(HELLO, full - 1, 9);
        assert_report(report, ReturnCode::BUF_ERROR, full - 1);
        assert_eq!(stream.len(), 17);
        // What did fit is a prefix of the real stream -- but only a prefix: it has
        // no trailer, so it is not decodable.
        assert_eq!(stream.as_slice(), &HELLO_LEVEL_9[..full - 1]);
    }

    /// Every destination size from empty up to one byte short fails, and every
    /// size from the exact length upwards succeeds. The transition happens at
    /// exactly one place, which is what proves the loop neither stops early nor
    /// writes past its window.
    #[test]
    fn the_buf_error_boundary_is_exactly_the_stream_length() {
        let full = HELLO_LEVEL_6.len();
        for capacity in 0..=(full + 4) {
            let (report, stream) = run(HELLO, capacity, 6);
            if capacity < full {
                assert_report(report, ReturnCode::BUF_ERROR, capacity);
                assert_eq!(stream.as_slice(), &HELLO_LEVEL_6[..capacity]);
            } else {
                assert_report(report, ReturnCode::OK, full);
                assert_eq!(stream.as_slice(), HELLO_LEVEL_6);
            }
        }
    }

    /// ★ A zero-length destination is legal input and reaches
    /// [`ReturnCode::BUF_ERROR`] through the encoder rather than by a special
    /// case -- **including for an empty source**, because the shortest zlib stream
    /// is still eight bytes long. Measured: `-5 / 0` for both.
    #[test]
    fn zero_length_destination_reports_buf_error() {
        let mut empty: [u8; 0] = [];

        let report = compress2_z(&mut empty, HELLO, 6);
        assert_report(report, ReturnCode::BUF_ERROR, 0);

        let report = compress2_z(&mut empty, b"", 6);
        assert_report(report, ReturnCode::BUF_ERROR, 0);

        let report = compress2_z(&mut empty, b"", 0);
        assert_report(report, ReturnCode::BUF_ERROR, 0);
    }

    /// An invalid level is rejected by `deflateInit` and forwarded verbatim, with
    /// a zero count because `compress.c` L36 clears the out-parameter first.
    /// Measured for -3, -2 and 10; extended here to the extremes of `i32`, which
    /// the same range test rejects.
    #[test]
    fn invalid_levels_report_stream_error_and_produce_nothing() {
        for level in [-3, -2, 10, 11, 255, i32::MIN, i32::MAX] {
            let (report, stream) = run(HELLO, 512, level);
            assert_report(report, ReturnCode::STREAM_ERROR, 0);
            assert!(stream.is_empty(), "level {level} produced bytes");
        }
    }

    /// And the ten valid levels plus `Z_DEFAULT_COMPRESSION` are all accepted, so
    /// the rejection above is not simply rejecting everything.
    #[test]
    fn every_documented_level_is_accepted() {
        for level in -1..=9 {
            let (report, _) = run_at_bound(HELLO, level);
            assert_eq!(report.code, ReturnCode::OK, "level {level}: {report:?}");
        }
    }

    /// 1000 identical bytes, byte for byte at the three levels whose output was
    /// measured. A long run is the shortest input that forces a match longer than
    /// one emitted length code, so it catches a wrapper that mis-sizes a window.
    #[test]
    fn thousand_a_matches_the_reference_bytes() {
        let source = thousand_a();
        for (level, expected) in [
            (1, THOUSAND_A_LEVEL_1),
            (6, THOUSAND_A_LEVEL_6),
            (9, THOUSAND_A_LEVEL_9),
        ] {
            let (report, stream) = run_at_bound(&source, level);
            assert_report(report, ReturnCode::OK, expected.len());
            assert_eq!(stream, expected, "level {level}");
        }
        // Level 0 stores the run verbatim, so the output is longer than the
        // input -- which is exactly what the bound has to cover. Measured: 1011.
        let (report, _) = run_at_bound(&source, 0);
        assert_report(report, ReturnCode::OK, 1011);
    }

    /// 70 000 bytes, more than two whole windows, at every level. The measured
    /// lengths are asserted rather than the bytes, because the bytes are the
    /// differential suite's business and the lengths already pin down every block
    /// decision the encoder made.
    ///
    /// This is also the termination and check-value test: the loop has to run many
    /// iterations, carry `adler` across all of them, and stop -- and a dropped
    /// check value would show as a trailer the decoder rejects.
    #[test]
    fn large_pattern_lengths_match_the_reference_and_round_trip() {
        let source = seven_cycle();
        assert_eq!(source.len(), 70_000);

        for (index, &expected_len) in SEVEN_CYCLE_LENGTHS.iter().enumerate() {
            let level = i32::try_from(index).unwrap();
            let (report, stream) = run_at_bound(&source, level);
            assert_report(report, ReturnCode::OK, expected_len);

            let mut plain = vec![0_u8; source.len() + 16];
            let back = uncompress2_z(&mut plain, &stream);
            assert_eq!(back.code, ReturnCode::OK, "level {level}: {back:?}");
            assert_eq!(&plain[..back.produced], source.as_slice(), "level {level}");
        }
    }

    /// Incompressible data is where the bound earns its keep: the output is longer
    /// than the input at every level, and the bound still covers it with two bytes
    /// to spare. Measured: 523 bytes out of 512 in, against a bound of 525.
    #[test]
    fn incompressible_data_stays_within_the_bound_at_every_level() {
        let source = incompressible_512();
        // The generator is part of the expectation, so pin its first bytes.
        assert_eq!(
            &source[..8],
            &[0x71, 0x47, 0x1d, 0x94, 0xec, 0x89, 0x93, 0xc7]
        );
        assert_eq!(compress_bound_z(source.len()), 525);

        for level in 0..=9 {
            let (report, stream) = run_at_bound(&source, level);
            assert_report(report, ReturnCode::OK, 523);
            assert!(
                stream.len() > source.len(),
                "level {level} shrank random data"
            );

            let mut plain = vec![0_u8; source.len() + 16];
            let back = uncompress2_z(&mut plain, &stream);
            assert_eq!(back.code, ReturnCode::OK, "level {level}: {back:?}");
            assert_eq!(&plain[..back.produced], source.as_slice(), "level {level}");
        }
    }
}
