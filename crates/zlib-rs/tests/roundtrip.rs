// UNSAFE CONTAINMENT, and it is mechanical rather than a convention.  `crates/zlib-rs` is the
// safe core: `src/lib.rs` carries `#![forbid(unsafe_code)]`, and its test suites carry it too, so
// the property "the core and everything that exercises it contains no `unsafe`" is enforced by the
// compiler in both halves.  The workspace's designated FFI boundary -- the only place a raw pointer
// crosses into a foreign implementation -- is `crates/libz-rs-sys/src/**` for the shipped library
// and `crates/zlib-rs-differential/src/{oracle,port}.rs` for the dev-only harness; an assertion
// that needs one of those belongs in a suite of that package, not here.
#![forbid(unsafe_code)]
//! End-to-end round-trip suite for the whole safe core: compress, decompress, assert
//! exact recovery -- across every corpus class, every container format and both feeding
//! styles.
//!
//! This is the broad safety net beneath the targeted per-module suites. A defect that
//! slips past a unit test will almost certainly break a round trip somewhere in this
//! matrix, which is why the matrix is wide rather than deep: it varies the *parameters*
//! exhaustively and leaves the internal invariants to the modules that own them.
//!
//! # Round-tripping alone would be a weak assertion
//!
//! "It came back" is the minimum bar, not the goal. A subtly different encoder -- one
//! that picked a different match, or broke a Huffman tie the other way -- would still
//! round-trip perfectly while emitting bytes the reference implementation never would.
//! So wherever this suite can pin *bytes* instead of mere recoverability, it does:
//!
//! * **Determinism.** The same input and parameters must produce the same bytes twice in
//!   one process, and the same bytes again from a fresh stream.
//! * **Allocator independence.** The bytes must not change when the allocator fills every
//!   block with `0xa5` instead of leaving it at whatever the global allocator produced.
//!   The reference `zcalloc` reaches `malloc`, not `calloc` (`zutil.c` L299-L303), so
//!   nothing may depend on fresh memory being zero, and this is the cheapest possible
//!   detector for a path that does.
//! * **Chunk invariance.** Handing the encoder one byte at a time must produce exactly the
//!   bytes a single call produces. Chunk boundaries interact with flush handling and the
//!   pending buffer, and this is the assertion proving they do not leak into the output.
//! * **Container structure.** The `zlib` header word, the `gzip` magic and its
//!   `CRC-32`/`ISIZE` trailer, and raw deflate's absence of both are checked directly, so
//!   emitting the wrong container cannot hide behind a successful decode.
//! * **Reference bytes.** A set of golden streams transcribed from the in-tree C
//!   implementation is compared byte for byte, which also makes this suite the end-to-end
//!   proof that the optional `simd` feature is output-neutral: the constants do not change
//!   with the feature set, so a vectorised checksum that perturbed the bitstream would
//!   fail here.
//!
//! # What this suite deliberately does not do
//!
//! **Cross-implementation interoperability is not this file's job.** Proving that a stream
//! produced by the C library decodes here, and that a stream produced here decodes there,
//! requires linking the C oracle -- which only `crates/zlib-rs-differential` can do. That
//! comparison, and the full byte-identity matrix beside it, belong to suites under that
//! crate; **neither exists in the tree, so neither property has been demonstrated.** This
//! suite must not attempt them and must not pre-empt them; the golden constants below are a
//! deliberately small, committed sample rather than a second matrix.
//!
//! # Corpus policy
//!
//! Every payload comes from the committed deterministic corpus in
//! `crates/zlib-rs/tests/common/mod.rs`. There is **no** network access, **no** filesystem
//! access, **no** environment-variable dependence and no clock anywhere in this file, so
//! `cargo test` is hermetic and reproducible byte-for-byte across runs and platforms. The
//! opt-in Silesia download used for throughput measurement belongs to
//! `crates/zlib-rs-differential/corpus` and is never reached from a correctness gate.
//!
//! # Constraints this file is written to
//!
//! * **No `unsafe`, no `FFI`, no third-party crate.** The crate under test carries
//!   `#![forbid(unsafe_code)]` and declares an empty `[dependencies]` table; its tests hold
//!   themselves to the same standard, so the harness is the built-in `#[test]` one and the
//!   pseudo-random filler is `common::lcg_fill` rather than a `rand` dependency.
//! * **`std` is available and `no_std` is not.** The library is
//!   `#![cfg_attr(not(feature = "std"), no_std)]`, but an integration test is its own crate
//!   and links `std` unconditionally, so no `#![no_std]` appears here.
//! * **No `#[cfg(feature = ...)]`.** This file compiles and passes identically under
//!   `--no-default-features`, `--features std`, `--features simd` and `--all-features`.
//!   Nothing below names a feature, which is what makes the `simd` comparison meaningful.
//! * **Imports by module path.** `zlib-rs` declares `default = []` and gates every
//!   crate-root re-export behind `rust-api`, so `zlib_rs::ReturnCode` does not exist in a
//!   default build while `zlib_rs::error::ReturnCode` always does.
//!
//! # Miri
//!
//! This is the largest matrix in the folder and therefore the biggest risk to the
//! `cargo +nightly miri test -p zlib-rs` gate. Three things keep it affordable, and all three
//! are deliberate:
//!
//! 1. **A small unconditional core.** Every corpus class except the window-crossing one, all
//!    three containers, both feeding styles, and the levels and shapes each test needs to make
//!    its point -- twenty tests. Everything wider sits behind `#[cfg_attr(miri, ignore)]`,
//!    twenty-two tests, each naming its reason and each running under an ordinary native
//!    `cargo test`.
//! 2. **Shorter payloads under the interpreter.** [`core_classes`] truncates the corpus to a few
//!    hundred bytes when `cfg(miri)` holds. What these tests assert is a property of each
//!    class's *shape*, and a shorter instance of a shape is still that shape.
//! 3. **A smaller buffer shape under the interpreter.** [`Params::core`] asks for
//!    `windowBits = 9` and `memLevel = 1` when `cfg(miri)` holds, because safe Rust must write
//!    every byte of every block it allocates and the reference shape's blocks come to roughly
//!    300 `KiB` per stream. That initialisation, not the compression, is what dominates a
//!    `Miri` run of this file; the reference shape is swept natively by the gated tests.
//!
//! One test per dimension, never a combinatorial explosion inside a single `#[test]`, so a
//! failure names the axis that broke it rather than merely the file.

// The workspace denies the panic-prone lints, which is right for library code and wrong for
// a test: a test asserts, an assertion that fails panics, and reading a fixture by index is
// clearer than defensively matching on it. `clippy.toml` grants the first three inside a
// `#[test]` function, but the file-scope helpers below are not `#[test]` functions and need
// the relaxation stated here. Nothing in this file ships.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic
)]

mod common;

use core::fmt;

use common::{check_err, corpus, TrackingAllocator};

use zlib_rs::adler32::{adler32, ADLER32_INITIAL_VALUE};
use zlib_rs::allocate::{Allocator, GlobalAllocator};
use zlib_rs::compress::{compress2, compress_bound};
use zlib_rs::config::{
    DeflateConfig, InflateConfig, DEF_LEVEL, DEF_MEM_LEVEL, MAX_MATCH, MAX_MEM_LEVEL, MAX_WBITS,
    MIN_MATCH, MIN_MEM_LEVEL, MIN_WBITS, PRESET_DICT, Z_BEST_COMPRESSION, Z_BEST_SPEED, Z_BLOCK,
    Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_DEFLATED, Z_FILTERED, Z_FINISH, Z_FIXED,
    Z_FULL_FLUSH, Z_HUFFMAN_ONLY, Z_NO_COMPRESSION, Z_NO_FLUSH, Z_PARTIAL_FLUSH, Z_RLE,
    Z_SYNC_FLUSH,
};
use zlib_rs::crc32::crc32;
use zlib_rs::deflate::{
    deflate, deflate_bound_z, deflate_end, deflate_init2, deflate_params, deflate_reset,
    deflate_set_dictionary, DeflateReset, DeflateState, DeflateStream,
};
use zlib_rs::error::ReturnCode;
use zlib_rs::inflate::{
    inflate, inflate_end, inflate_init2, inflate_reset, inflate_set_dictionary, inflate_sync,
    InflateState, InflateStream,
};
use zlib_rs::uncompress::uncompress2;

// ---------------------------------------------------------------------------------------
// The three container formats
// ---------------------------------------------------------------------------------------

/// The `zlib` header's `FLEVEL`-independent structural rule: the two header bytes read as a
/// big-endian 16-bit value must be a multiple of 31 (`doc/rfc1950.txt` L212-L215).
const ZLIB_HEADER_MODULUS: u16 = 31;

/// `zlib`'s wrapper overhead: a two-byte header plus a four-byte `Adler-32` trailer.
const ZLIB_OVERHEAD: usize = 2 + 4;

/// `gzip`'s wrapper overhead with no optional fields: a ten-byte header plus a four-byte
/// `CRC-32` and a four-byte `ISIZE` (`doc/rfc1952.txt` L157-L167 and L228-L236).
const GZIP_OVERHEAD: usize = 10 + 4 + 4;

/// The three container formats a `windowBits` value can select.
///
/// The three rows of the mapping are the whole of `deflateInit2_`'s and `inflateInit2_`'s
/// container selection: a negative magnitude asks for raw deflate, a bare magnitude for the
/// `zlib` wrapper, and the magnitude plus 16 for the `gzip` wrapper (`zlib.h` L563-L601 and
/// L879-L904).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Container {
    /// Raw deflate: no header, no trailer, `RFC 1951` only.
    Raw,
    /// The `zlib` wrapper of `RFC 1950`: two header bytes and an `Adler-32` trailer.
    Zlib,
    /// The `gzip` wrapper of `RFC 1952`: ten header bytes, a `CRC-32` and an `ISIZE`.
    Gzip,
}

impl Container {
    /// All three, for the suites that sweep the container axis.
    const ALL: [Self; 3] = [Self::Raw, Self::Zlib, Self::Gzip];

    /// The container's name, for assertion messages.
    const fn name(self) -> &'static str {
        match self {
            Self::Raw => "raw",
            Self::Zlib => "zlib",
            Self::Gzip => "gzip",
        }
    }

    /// The `windowBits` value that selects this container for the compressor.
    ///
    /// `magnitude` is the window exponent, 9 through 15.
    const fn deflate_window_bits(self, magnitude: i32) -> i32 {
        match self {
            Self::Raw => -magnitude,
            Self::Zlib => magnitude,
            Self::Gzip => magnitude + 16,
        }
    }

    /// The `windowBits` value that selects this container for the decompressor.
    ///
    /// Identical to [`Container::deflate_window_bits`] -- the decoder's extra spellings
    /// (`0` for "take it from the header" and `47` for automatic detection) are exercised
    /// separately, because they are decoder-only and have no compressor counterpart.
    const fn inflate_window_bits(self, magnitude: i32) -> i32 {
        self.deflate_window_bits(magnitude)
    }

    /// How many bytes of wrapper this container adds around the raw deflate stream.
    const fn overhead(self) -> usize {
        match self {
            Self::Raw => 0,
            Self::Zlib => ZLIB_OVERHEAD,
            Self::Gzip => GZIP_OVERHEAD,
        }
    }
}

// ---------------------------------------------------------------------------------------
// One point in the compression parameter space
// ---------------------------------------------------------------------------------------

/// The five `deflateInit2_` arguments, as one value.
///
/// Every test in this file reads as a short list of these rather than as repeated
/// initialisation boilerplate, which is what keeps a matrix this size reviewable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Params {
    /// Which wrapper to emit.
    container: Container,
    /// `level`, `0` through `9` or [`Z_DEFAULT_COMPRESSION`].
    level: i32,
    /// `memLevel`, `1` through `9`.
    mem_level: i32,
    /// The window exponent, `9` through `15`. The signed `windowBits` value handed to the
    /// library is derived from this and [`Params::container`].
    magnitude: i32,
    /// One of the five documented strategies.
    strategy: i32,
}

/// The window exponent the unconditional tests use under `Miri`.
///
/// `MIN_WBITS` is 8, but `deflateInit2_` silently promotes 8 to 9 (`deflate.c` L410-L411), so 9
/// is the smallest value that means what it says.
const MIRI_WBITS: i32 = MIN_WBITS + 1;

/// The window exponent the unconditional tests use: the reference maximum natively, and
/// [`MIRI_WBITS`] under `Miri`. See [`Params::core`] for why.
const fn core_magnitude() -> i32 {
    if cfg!(miri) {
        MIRI_WBITS
    } else {
        MAX_WBITS
    }
}

impl Params {
    /// The reference defaults for `container` at `level`: the largest window and
    /// [`DEF_MEM_LEVEL`], with [`Z_DEFAULT_STRATEGY`].
    const fn new(container: Container, level: i32) -> Self {
        Self {
            container,
            level,
            mem_level: DEF_MEM_LEVEL,
            magnitude: MAX_WBITS,
            strategy: Z_DEFAULT_STRATEGY,
        }
    }

    /// The parameters the **unconditional** tests use: [`Params::new`] natively, and the same
    /// thing on a smaller buffer shape under `Miri`.
    ///
    /// # Why the shape changes under the interpreter
    ///
    /// Safe Rust cannot hand out uninitialised memory, so `Buffer::try_global` *writes* every
    /// byte of every block it produces -- where the reference's `zcalloc` reaches `malloc` and
    /// writes none (`zutil.c` L299-L303). At the reference defaults a compressor's four blocks
    /// come to roughly 300 `KiB`: the window is `2 * w_size` bytes, `prev` and `head` are
    /// `w_size` and `hash_size` 16-bit entries, and the pending buffer is
    /// `lit_bufsize * LIT_BUFS` (`deflate.c` L458-L505). Natively that initialisation is a
    /// `memset`; under `Miri` it is three hundred thousand individually interpreted stores, and
    /// it happens once per stream. Measured, it dominates everything else in this file by two
    /// orders of magnitude.
    ///
    /// `windowBits = 9` with `memLevel = 1` asks for the same four blocks at roughly a
    /// seventieth of the size, which is what makes an unconditional matrix affordable. Nothing
    /// the unconditional tests are *about* changes: they vary the corpus class, the container,
    /// the level and the feeding style, and all four are still varied. The reference shape is
    /// swept natively by [`every_mem_level_round_trips_in_every_container`] and
    /// [`every_window_size_round_trips_across_the_window_boundary`], and the byte-exact
    /// comparisons against the reference in [`the_golden_streams_are_reproduced_byte_for_byte`]
    /// are pinned to it -- which is why that test is one of the `Miri`-gated ones.
    const fn core(container: Container, level: i32) -> Self {
        let params = Self::new(container, level).with_magnitude(core_magnitude());
        if cfg!(miri) {
            params.with_mem_level(MIN_MEM_LEVEL)
        } else {
            params
        }
    }

    /// The same parameters with a different `memLevel`.
    const fn with_mem_level(mut self, mem_level: i32) -> Self {
        self.mem_level = mem_level;
        self
    }

    /// The same parameters with a different window exponent.
    const fn with_magnitude(mut self, magnitude: i32) -> Self {
        self.magnitude = magnitude;
        self
    }

    /// The same parameters with a different strategy.
    const fn with_strategy(mut self, strategy: i32) -> Self {
        self.strategy = strategy;
        self
    }

    /// The signed `windowBits` the compressor is initialised with.
    const fn deflate_window_bits(self) -> i32 {
        self.container.deflate_window_bits(self.magnitude)
    }

    /// The signed `windowBits` the matching decompressor is initialised with.
    const fn inflate_window_bits(self) -> i32 {
        self.container.inflate_window_bits(self.magnitude)
    }

    /// The validated configuration these parameters denote.
    ///
    /// # Panics
    ///
    /// If the combination is one `deflateInit2_` rejects, which would be a defect in the
    /// test rather than in the library.
    fn deflate_config(self) -> DeflateConfig {
        DeflateConfig::from_raw(
            self.level,
            Z_DEFLATED,
            self.deflate_window_bits(),
            self.mem_level,
            self.strategy,
        )
        .expect("the parameter matrix must only contain configurations deflateInit2_ accepts")
    }
}

impl fmt::Display for Params {
    /// Renders the five arguments the way `deflateInit2_` takes them, so a failure message
    /// can be pasted straight into a reproduction.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} level={} windowBits={} memLevel={} strategy={}",
            self.container.name(),
            self.level,
            self.deflate_window_bits(),
            self.mem_level,
            self.strategy,
        )
    }
}

// ---------------------------------------------------------------------------------------
// Feeding style
// ---------------------------------------------------------------------------------------

/// How much of the input and of the output room a single engine call is offered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Chunk {
    /// Everything at once: the whole input and a buffer large enough to hold the whole
    /// result, in a single call. This is the shape `deflateBound` is documented for
    /// (`zlib.h` L768-L775).
    Whole,
    /// At most this many bytes of input and this many bytes of output room per call. Never
    /// zero, because zero room makes no progress possible.
    Bytes(usize),
}

impl Chunk {
    /// The chunk sizes the sweeps use.
    ///
    /// `1` is `test/example.c`'s "force small buffers" case (L188 and L226). The small
    /// primes catch an off-by-one that a power of two would hide by aligning with every
    /// internal buffer size, and `8192` is large enough to swallow most payloads whole
    /// while still ending on an arbitrary boundary.
    const SWEEP: [usize; 7] = [1, 2, 3, 7, 13, 256, 8192];

    /// The end offset of the window this chunk exposes, given a cursor and a total length.
    const fn limit(self, cursor: usize, len: usize) -> usize {
        match self {
            Self::Whole => len,
            Self::Bytes(bytes) => {
                let want = cursor.saturating_add(bytes);
                if want < len {
                    want
                } else {
                    len
                }
            }
        }
    }

    /// How much room a call must be able to offer at `cursor` for the feeding style to be
    /// honoured exactly.
    ///
    /// Growing the backing buffer to at least this length keeps the engine's view of
    /// `avail_out` a function of the chunk size alone rather than of the buffer's capacity.
    /// That matters: `deflate_stored` decides how much to copy directly from `avail_out`
    /// (`deflate.c` L1668-L1720), so a buffer that happened to be nearly full would change
    /// the emitted bytes at level 0.
    const fn required_len(self, cursor: usize) -> usize {
        match self {
            Self::Whole => cursor.saturating_add(1),
            Self::Bytes(bytes) => cursor.saturating_add(bytes),
        }
    }
}

impl fmt::Display for Chunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Whole => f.write_str("single-shot"),
            Self::Bytes(bytes) => write!(f, "chunked({bytes})"),
        }
    }
}

/// Grows `buffer` so that `chunk` can be offered its full quota at `cursor`.
///
/// Doubling keeps the number of reallocations logarithmic; the `max` keeps the result large
/// enough even when the quota exceeds the current length outright.
fn ensure_room(buffer: &mut Vec<u8>, cursor: usize, chunk: Chunk) {
    let required = chunk.required_len(cursor);
    if buffer.len() < required {
        let grown = buffer.len().saturating_mul(2).max(required);
        buffer.resize(grown, 0);
    }
}

// ---------------------------------------------------------------------------------------
// The scalars a caller carries between calls
// ---------------------------------------------------------------------------------------

/// The five `z_stream` fields that are the caller's to preserve across engine calls.
///
/// [`DeflateStream`] and [`InflateStream`] are transient *views*: the engine itself lives in
/// the state object, and a view holds the buffers plus the public scalars. Driving a stream
/// in chunks therefore means building a fresh view per call -- widening the input and output
/// slices rather than re-slicing from the cursor, because `deflate_stored` reads back through
/// the already-consumed input (`deflate.c` L1766 and L1780) and `flush_pending` writes at
/// `next_out` -- and carrying these five values from one view to the next.
///
/// Note that `total_in`, `total_out` and `adler` are *maintained* by the engine; this type
/// only ferries them. Dropping them on the floor would silently corrupt the check value.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Scalars {
    /// `z_stream.total_in` (`zlib.h` L93).
    total_in: u64,
    /// `z_stream.total_out` (`zlib.h` L96).
    total_out: u64,
    /// `z_stream.adler`: the running check value (`zlib.h` L106).
    adler: u32,
    /// `z_stream.data_type` (`zlib.h` L105).
    data_type: i32,
    /// `z_stream.msg`, or [`None`] for C's `Z_NULL` (`zlib.h` L98).
    msg: Option<&'static str>,
}

impl From<DeflateReset> for Scalars {
    /// The scalars a freshly reset compressor's stream must carry.
    ///
    /// `deflateResetKeep` seeds `adler` unconditionally, choosing `crc32(0, Z_NULL, 0)` for
    /// `gzip` and `adler32(0, Z_NULL, 0)` otherwise (`deflate.c` L667-L671), so a `zlib`
    /// stream reports `adler == 1` before a byte has been compressed.
    fn from(reset: DeflateReset) -> Self {
        Self {
            total_in: reset.total_in,
            total_out: reset.total_out,
            adler: reset.adler,
            data_type: reset.data_type,
            msg: reset.msg,
        }
    }
}

impl Scalars {
    /// Writes these scalars into a compressor view before a call.
    fn load_deflate(self, stream: &mut DeflateStream<'_, '_>) {
        stream.total_in = self.total_in;
        stream.total_out = self.total_out;
        stream.adler = self.adler;
        stream.data_type = self.data_type;
        stream.msg = self.msg;
    }

    /// Reads the scalars back out of a compressor view after a call.
    fn store_deflate(&mut self, stream: &DeflateStream<'_, '_>) {
        self.total_in = stream.total_in;
        self.total_out = stream.total_out;
        self.adler = stream.adler;
        self.data_type = stream.data_type;
        self.msg = stream.msg;
    }

    /// Writes these scalars into a decompressor view before a call.
    fn load_inflate(self, stream: &mut InflateStream<'_>) {
        stream.total_in = self.total_in;
        stream.total_out = self.total_out;
        stream.data_type = self.data_type;
        stream.msg = self.msg;
    }

    /// Reads the scalars back out of a decompressor view after a call.
    fn store_inflate(&mut self, stream: &InflateStream<'_>) {
        self.total_in = stream.total_in;
        self.total_out = stream.total_out;
        if let Some(value) = stream.adler {
            self.adler = value;
        }
        self.data_type = stream.data_type;
        self.msg = stream.msg;
    }
}

// ---------------------------------------------------------------------------------------
// Opening an engine
// ---------------------------------------------------------------------------------------

/// Headroom added to `deflateBound` so that a single-shot call can never be starved by an
/// arithmetic edge case in the bound itself. Purely defensive: a `Z_BUF_ERROR` from the
/// single-shot path is asserted against, not worked around.
const BOUND_SLACK: usize = 64;

/// Creates a compressor for `params`, together with the scalars its stream must start from.
///
/// `deflateInit2_` finishes with `return deflateReset(strm);` (`deflate.c` L532), and the reset
/// has two halves: the state's, which [`deflate_init2`] has already applied, and the caller's
/// `z_stream` fields, which come back as the [`DeflateReset`] this converts.
///
/// # Panics
///
/// If `params` names a combination `deflateInit2_` rejects, or if allocation fails.
fn open_encoder(params: Params) -> (DeflateState<'static, GlobalAllocator>, Scalars) {
    let mut state = deflate_init2(params.deflate_config(), GlobalAllocator)
        .expect("deflate_init2 must accept every configuration in the matrix");
    let scalars = Scalars::from(deflate_reset(&mut state));
    (state, scalars)
}

/// Creates a decompressor for `window_bits`, together with the scalars its stream must start
/// from.
///
/// `inflateReset` writes `total_in`, `total_out`, `msg` and `data_type` unconditionally but
/// assigns `strm->adler` only when the stream is wrapped (`inflate.c` L104-L108), which the
/// reset reports as [`None`]. A raw stream therefore keeps whatever the caller had, and for a
/// freshly zeroed view that is `0` -- which is what `unwrap_or(0)` spells here.
///
/// # Panics
///
/// If `window_bits` is outside the matrix `inflateInit2_` accepts, or if allocation fails.
fn open_decoder(window_bits: i32) -> (InflateState<'static, GlobalAllocator>, Scalars) {
    let mut state = inflate_init2(InflateConfig::new(window_bits), GlobalAllocator)
        .expect("inflate_init2 must accept every windowBits value in the matrix");
    let reset = inflate_reset(&mut state);
    let scalars = Scalars {
        total_in: u64::from(reset.total_in),
        total_out: u64::from(reset.total_out),
        adler: reset.adler.unwrap_or(0),
        data_type: reset.data_type,
        msg: reset.msg,
    };
    (state, scalars)
}

// ---------------------------------------------------------------------------------------
// Compression drivers
// ---------------------------------------------------------------------------------------

/// Compresses `payload` with `params` in one `deflate(Z_FINISH)` call.
///
/// The canonical single-shot shape: the entire input, and an output buffer sized from
/// `deflateBound`, which `zlib.h` L768-L775 documents as valid precisely when `deflate` is
/// called once with `Z_FINISH` and all the input at once. Asserting `Z_STREAM_END` on that
/// one call is therefore also an assertion about the bound.
///
/// # Panics
///
/// If initialisation fails, if the single call does not report `Z_STREAM_END`, or if
/// `deflateEnd` does not report `Z_OK`.
fn compress_single_shot_with<'a, A>(payload: &[u8], params: Params, allocator: A) -> Vec<u8>
where
    A: Allocator<'a> + Copy,
{
    let mut state = deflate_init2(params.deflate_config(), allocator)
        .expect("deflate_init2 must accept every configuration in the matrix");
    let reset = deflate_reset(&mut state);

    let capacity = deflate_bound_z(Some(&state), payload.len()) + BOUND_SLACK;
    let mut out = vec![0_u8; capacity];

    let produced = {
        let mut stream = DeflateStream::new(payload, &mut out);
        Scalars::from(reset).load_deflate(&mut stream);
        let code = deflate(&mut state, &mut stream, Z_FINISH);
        assert_eq!(
            code,
            ReturnCode::STREAM_END,
            "{params}: a single deflate(Z_FINISH) into a deflateBound-sized buffer must \
             finish the stream, got {code:?} after {} of {capacity} bytes",
            stream.next_out,
        );
        assert_eq!(
            stream.next_in,
            payload.len(),
            "{params}: Z_FINISH must consume the whole input",
        );
        stream.next_out
    };

    assert_eq!(
        deflate_end(&mut state),
        ReturnCode::OK,
        "{params}: deflateEnd after Z_STREAM_END must report Z_OK",
    );
    out.truncate(produced);
    out
}

/// Compresses `payload` with `params`, offering the encoder at most `bytes` of input and
/// `bytes` of output room per call.
///
/// A faithful translation of `test_deflate` (`test/example.c` L172-L201): every input byte
/// is handed over under `Z_NO_FLUSH` first, then the stream is finished under `Z_FINISH`,
/// with the buffers kept small throughout. The output buffer is grown so that each call sees
/// exactly `bytes` of room, never less, because `deflate_stored` reads `avail_out` when
/// deciding how much to copy directly (`deflate.c` L1668-L1720) and a nearly-full buffer
/// would therefore change the emitted bytes at level 0.
///
/// # Panics
///
/// If any `Z_NO_FLUSH` call reports anything but `Z_OK`, if any `Z_FINISH` call reports
/// anything but `Z_OK` or `Z_STREAM_END`, or if `deflateEnd` does not report `Z_OK`.
fn compress_chunked_with<'a, A>(
    payload: &[u8],
    params: Params,
    bytes: usize,
    allocator: A,
) -> Vec<u8>
where
    A: Allocator<'a> + Copy,
{
    assert!(bytes > 0, "a zero-byte chunk makes no progress possible");
    let chunk = Chunk::Bytes(bytes);

    let mut state = deflate_init2(params.deflate_config(), allocator)
        .expect("deflate_init2 must accept every configuration in the matrix");
    let reset = deflate_reset(&mut state);

    // The bound is the right *starting* size, not a limit: a small `avail_out` provokes
    // extra stored-block headers at level 0, so a chunked stream can legitimately exceed
    // `deflateBound`. `ensure_room` handles that; the bound just avoids reallocation churn.
    let mut out = vec![0_u8; deflate_bound_z(Some(&state), payload.len()) + BOUND_SLACK];
    let mut scalars = Scalars::from(reset);
    let mut next_in = 0_usize;
    let mut next_out = 0_usize;

    // Phase 1: hand over every input byte, exactly as `test/example.c` L187-L191 does.
    while next_in < payload.len() {
        ensure_room(&mut out, next_out, chunk);
        let in_limit = chunk.limit(next_in, payload.len());
        let out_limit = chunk.limit(next_out, out.len());
        let before = (next_in, next_out);
        let code = {
            let mut stream = DeflateStream::new(&payload[..in_limit], &mut out[..out_limit]);
            stream.next_in = next_in;
            stream.next_out = next_out;
            scalars.load_deflate(&mut stream);
            let code = deflate(&mut state, &mut stream, Z_NO_FLUSH);
            next_in = stream.next_in;
            next_out = stream.next_out;
            scalars.store_deflate(&stream);
            code
        };
        check_err(code, "deflate(Z_NO_FLUSH)");
        assert_ne!(
            before,
            (next_in, next_out),
            "{params} {chunk}: a Z_NO_FLUSH call with both input and room available must \
             make progress",
        );
    }

    // Phase 2: finish, still with small buffers (`test/example.c` L193-L198).
    loop {
        ensure_room(&mut out, next_out, chunk);
        let in_limit = chunk.limit(next_in, payload.len());
        let out_limit = chunk.limit(next_out, out.len());
        let code = {
            let mut stream = DeflateStream::new(&payload[..in_limit], &mut out[..out_limit]);
            stream.next_in = next_in;
            stream.next_out = next_out;
            scalars.load_deflate(&mut stream);
            let code = deflate(&mut state, &mut stream, Z_FINISH);
            next_in = stream.next_in;
            next_out = stream.next_out;
            scalars.store_deflate(&stream);
            code
        };
        if code == ReturnCode::STREAM_END {
            break;
        }
        check_err(code, "deflate(Z_FINISH)");
    }

    assert_eq!(
        deflate_end(&mut state),
        ReturnCode::OK,
        "{params} {chunk}: deflateEnd after Z_STREAM_END must report Z_OK",
    );
    assert_eq!(
        scalars.total_out,
        u64::try_from(next_out).unwrap(),
        "{params} {chunk}: total_out must agree with the output cursor",
    );
    out.truncate(next_out);
    out
}

/// Compresses `payload` with `params` in the feeding style `chunk` names.
fn compress_with<'a, A>(payload: &[u8], params: Params, chunk: Chunk, allocator: A) -> Vec<u8>
where
    A: Allocator<'a> + Copy,
{
    match chunk {
        Chunk::Whole => compress_single_shot_with(payload, params, allocator),
        Chunk::Bytes(bytes) => compress_chunked_with(payload, params, bytes, allocator),
    }
}

/// Compresses `payload` with `params` through the global allocator, the common case.
fn compress(payload: &[u8], params: Params, chunk: Chunk) -> Vec<u8> {
    compress_with(payload, params, chunk, GlobalAllocator)
}

// ---------------------------------------------------------------------------------------
// Decompression driver
// ---------------------------------------------------------------------------------------

/// Everything a completed `inflate` run reports.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Inflated {
    /// The status of the last call: `Z_STREAM_END` for a complete stream, otherwise the
    /// code that stopped it.
    code: ReturnCode,
    /// The bytes recovered.
    output: Vec<u8>,
    /// How many input bytes were consumed, so that trailing data can be located.
    consumed: usize,
    /// The check value the decoder ended with, which for a complete stream is the one the
    /// trailer carried.
    adler: u32,
}

/// Drives `inflate` to completion over `bytes`, offering `chunk`'s quota per call.
///
/// The loop stops on `Z_STREAM_END`, on any error, or when a call makes no progress at all
/// while output room was available -- the last being the "no progress possible" condition
/// `zlib.h` L360-L363 documents `Z_BUF_ERROR` for. `capacity_hint` sizes the initial output
/// buffer, which the loop grows as needed; for [`Chunk::Whole`] a hint at least as large as
/// the payload makes the whole stream decode in a single call.
///
/// # Panics
///
/// If initialisation fails. Every other outcome is reported rather than asserted, so that
/// callers can assert on failure codes deliberately.
fn inflate_all(
    bytes: &[u8],
    window_bits: i32,
    chunk: Chunk,
    flush: i32,
    capacity_hint: usize,
) -> Inflated {
    let (mut state, mut scalars) = open_decoder(window_bits);
    let mut out = vec![0_u8; capacity_hint.max(1)];
    let mut next_in = 0_usize;
    let mut next_out = 0_usize;
    let code = loop {
        ensure_room(&mut out, next_out, chunk);
        let in_limit = chunk.limit(next_in, bytes.len());
        let out_limit = chunk.limit(next_out, out.len());
        let before = (next_in, next_out);
        let code = {
            let mut stream = InflateStream::new(&bytes[..in_limit], &mut out[..out_limit]);
            stream.next_in = next_in;
            stream.next_out = next_out;
            scalars.load_inflate(&mut stream);
            let code = inflate(&mut state, &mut stream, flush);
            next_in = stream.next_in;
            next_out = stream.next_out;
            scalars.store_inflate(&stream);
            code
        };
        if code != ReturnCode::OK {
            break code;
        }
        if before == (next_in, next_out) {
            // `Z_OK` with nothing consumed and nothing produced means the decoder is
            // starved of input it will never get, which is the end of a truncated stream.
            break ReturnCode::BUF_ERROR;
        }
    };

    assert_eq!(
        inflate_end(&mut state),
        ReturnCode::OK,
        "inflateEnd must always report Z_OK",
    );
    out.truncate(next_out);
    Inflated {
        code,
        output: out,
        consumed: next_in,
        adler: scalars.adler,
    }
}

/// Decompresses `bytes` and asserts the stream completed, returning what it recovered.
///
/// # Panics
///
/// If the stream does not end with `Z_STREAM_END`.
fn decompress(bytes: &[u8], window_bits: i32, chunk: Chunk, capacity_hint: usize) -> Vec<u8> {
    let result = inflate_all(bytes, window_bits, chunk, Z_NO_FLUSH, capacity_hint);
    assert_eq!(
        result.code,
        ReturnCode::STREAM_END,
        "windowBits={window_bits} {chunk}: inflate stopped with {:?} after {} of {} input \
         bytes and {} output bytes",
        result.code,
        result.consumed,
        bytes.len(),
        result.output.len(),
    );
    assert_eq!(
        result.consumed,
        bytes.len(),
        "windowBits={window_bits} {chunk}: a complete stream must leave no input over",
    );
    result.output
}

// ---------------------------------------------------------------------------------------
// Assertions
// ---------------------------------------------------------------------------------------

/// Asserts that two byte sequences are equal, reporting the first divergence compactly.
///
/// `assert_eq!` on two multi-kilobyte vectors dumps both of them, which buries the one fact
/// that matters. This reports the lengths and the first differing offset instead.
fn assert_bytes_eq(actual: &[u8], expected: &[u8], label: &str) {
    if actual == expected {
        return;
    }
    let first_difference = actual
        .iter()
        .zip(expected)
        .position(|(left, right)| left != right);
    match first_difference {
        Some(at) => panic!(
            "{label}: byte {at} is {:#04x}, expected {:#04x} (lengths {} and {})",
            actual[at],
            expected[at],
            actual.len(),
            expected.len(),
        ),
        None => panic!(
            "{label}: one sequence is a prefix of the other (lengths {} and {})",
            actual.len(),
            expected.len(),
        ),
    }
}

/// Compresses `payload`, decompresses the result and asserts exact recovery.
///
/// The workhorse of this file: every matrix test is a loop that calls this.
fn assert_round_trip(name: &str, payload: &[u8], params: Params, chunk: Chunk) {
    let stream = compress(payload, params, chunk);
    let recovered = decompress(
        &stream,
        params.inflate_window_bits(),
        chunk,
        payload.len() + BOUND_SLACK,
    );
    assert_bytes_eq(
        &recovered,
        payload,
        &format!("{name} / {params} / {chunk}: round trip"),
    );
}

// ---------------------------------------------------------------------------------------
// The axes the matrix sweeps
// ---------------------------------------------------------------------------------------

/// Every level the public API accepts: the ten explicit ones plus the sentinel.
///
/// [`Z_DEFAULT_COMPRESSION`] is `-1` and resolves to `6` (`deflate.c` L423-L424), so it is
/// not a duplicate of level 6 as far as the *entry points* are concerned -- the resolution
/// itself is part of what is under test.
const LEVELS: [i32; 11] = [Z_DEFAULT_COMPRESSION, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9];

/// The two levels the unconditional core uses: the fastest and the default.
const CORE_LEVELS: [i32; 2] = [Z_BEST_SPEED, DEF_LEVEL];

/// All five documented strategies (`zlib.h` L197-L205).
const STRATEGIES: [i32; 5] = [
    Z_DEFAULT_STRATEGY,
    Z_FILTERED,
    Z_HUFFMAN_ONLY,
    Z_RLE,
    Z_FIXED,
];

/// The name [`corpus::all`] gives the one class that exceeds the 32 `KiB` window.
const WINDOW_CROSSING: &str = "window_crossing";

/// Every corpus class that stays a few kilobytes, i.e. all of them but the window-crossing
/// one.
///
/// The gated sweeps use these at full length. The window-crossing class is excluded because it
/// is deliberately larger than the window it has to cross, and the tests that want it ask for it
/// by name.
fn small_classes() -> Vec<(&'static str, Vec<u8>)> {
    corpus::all()
        .into_iter()
        .filter(|(name, _)| *name != WINDOW_CROSSING)
        .collect()
}

/// The longest payload the unconditional tests use under `Miri`.
///
/// Large enough that `binary` still contains all 256 byte values and that `incompressible` is
/// still stored rather than coded, small enough that driving it one byte at a time is a few
/// hundred interpreted engine calls rather than a few thousand.
const MIRI_PAYLOAD_CAP: usize = 256;

/// The corpus the **unconditional** tests iterate: [`small_classes`] natively, and the same
/// classes shortened under `Miri`.
///
/// What these tests assert is a property of each class's *shape* -- maximally compressible,
/// maximally incompressible, natural-language text, all 256 byte values, empty, one byte, and
/// the `test/example.c` fixture -- and a shorter instance of a shape is still that shape. The
/// full-length instances run natively through the gated sweeps, which use [`small_classes`].
fn core_classes() -> Vec<(&'static str, Vec<u8>)> {
    small_classes()
        .into_iter()
        .map(|(name, mut payload)| {
            if cfg!(miri) {
                payload.truncate(MIRI_PAYLOAD_CAP);
            }
            (name, payload)
        })
        .collect()
}

/// Shortens one payload the way [`core_classes`] shortens the corpus, for the unconditional
/// tests that drive a single fixture.
fn core_payload(payload: &[u8]) -> Vec<u8> {
    let limit = if cfg!(miri) {
        payload.len().min(MIRI_PAYLOAD_CAP)
    } else {
        payload.len()
    };
    payload[..limit].to_vec()
}

/// Resolves [`Z_DEFAULT_COMPRESSION`] to the level the compressor actually runs at.
///
/// `if (level == Z_DEFAULT_COMPRESSION) level = 6;` (`deflate.c` L423-L424).
const fn resolved_level(level: i32) -> i32 {
    if level == Z_DEFAULT_COMPRESSION {
        DEF_LEVEL
    } else {
        level
    }
}

/// Whether the reference implementation's output at these parameters is invariant under the
/// feeding style.
///
/// Levels 1 through 9 always are: the match finder and the Huffman coder read only the
/// window and the symbol buffer, so how the input arrived cannot reach them. **Level 0 is
/// the exception.** `deflate_stored` copies straight to `next_out` and sizes each stored
/// block from `avail_out` (`deflate.c` L1668-L1720), so a payload large enough for the
/// segmentation to differ produces different -- equally valid, equally decodable -- bytes.
/// That was measured against the in-tree C implementation, which behaves identically; it is
/// reference behaviour to preserve, not a defect to fix.
///
/// The predicate is therefore deliberately conservative: only the fixtures small enough that
/// the measurement showed invariance at every `memLevel` are claimed.
fn is_chunk_invariant(payload_len: usize, params: Params) -> bool {
    resolved_level(params.level) != Z_NO_COMPRESSION || payload_len <= LEVEL_ZERO_STABLE_LEN
}

/// The payload length up to which level 0 was measured chunk-invariant at every `memLevel`
/// the matrix uses.
///
/// `min_block` in `deflate_stored` is `MIN(pending_buf_size - 5, w_size)` (`deflate.c`
/// L1670), and `pending_buf_size` is `lit_bufsize * LIT_BUFS` with
/// `lit_bufsize = 1 << (memLevel + 6)` (`deflate.c` L502-L505, `deflate.h` L225-L229), so
/// `memLevel == 1` gives `min_block == 507`. Payloads at or below that never reach the
/// segmentation decision at any `memLevel`.
const LEVEL_ZERO_STABLE_LEN: usize = 507;

// ---------------------------------------------------------------------------------------
// Container structure
// ---------------------------------------------------------------------------------------

/// The `XFL` byte `deflate` writes into a `gzip` header (`deflate.c` L873-L877):
///
/// ```c
/// put_byte(s, s->level == 9 ? 2 :
///             (s->strategy >= Z_HUFFMAN_ONLY || s->level < 2 ? 4 : 0));
/// ```
const fn expected_gzip_xfl(level: i32, strategy: i32) -> u8 {
    if level == Z_BEST_COMPRESSION {
        2
    } else if strategy >= Z_HUFFMAN_ONLY || level < 2 {
        4
    } else {
        0
    }
}

/// Asserts that `stream` is an `RFC 1950` container carrying `payload`.
///
/// Two structural facts and one arithmetic one:
///
/// * `CM`, the low nibble of the first byte, is 8 -- the only compression method `zlib.h`
///   L192 defines (`doc/rfc1950.txt` L166-L173).
/// * the two header bytes read as a big-endian 16-bit value are a multiple of 31, which is
///   the check `FCHECK` exists to make true (`doc/rfc1950.txt` L212-L215).
/// * the four trailing bytes are the payload's `Adler-32`, most significant byte first
///   (`doc/rfc1950.txt` L318-L329).
fn assert_zlib_markers(stream: &[u8], payload: &[u8], label: &str) {
    assert!(
        stream.len() >= ZLIB_OVERHEAD,
        "{label}: a zlib stream is at least {ZLIB_OVERHEAD} bytes, got {}",
        stream.len(),
    );
    assert_eq!(
        i32::from(stream[0] & 0x0f),
        Z_DEFLATED,
        "{label}: CM must name the deflate method",
    );
    let header = u16::from_be_bytes([stream[0], stream[1]]);
    assert_eq!(
        header % ZLIB_HEADER_MODULUS,
        0,
        "{label}: the header word {header:#06x} must be a multiple of \
         {ZLIB_HEADER_MODULUS}",
    );
    let trailer = &stream[stream.len() - 4..];
    let carried = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    assert_eq!(
        carried,
        adler32(ADLER32_INITIAL_VALUE, payload),
        "{label}: the trailer must be the payload's Adler-32",
    );
}

/// Asserts that `stream` is an `RFC 1952` container carrying `payload`.
///
/// The ten fixed header bytes are `ID1`, `ID2`, `CM`, `FLG`, four `MTIME` bytes, `XFL` and
/// `OS` (`doc/rfc1952.txt` L157-L167). With no `gz_header` supplied the compressor writes
/// `FLG == 0` and `MTIME == 0` (`deflate.c` L866-L872), so eight of the ten are fixed
/// constants, `XFL` is a function of the level and strategy, and only `OS` is
/// target-dependent -- `OS_CODE` (`zutil.h` L177-L212) -- and therefore left unasserted.
///
/// The eight trailing bytes are the payload's `CRC-32` then its length modulo 2^32, both
/// least significant byte first (`doc/rfc1952.txt` L228-L236).
fn assert_gzip_markers(stream: &[u8], payload: &[u8], level: i32, strategy: i32, label: &str) {
    assert!(
        stream.len() >= GZIP_OVERHEAD,
        "{label}: a gzip stream is at least {GZIP_OVERHEAD} bytes, got {}",
        stream.len(),
    );
    assert_eq!(stream[0], 0x1f, "{label}: ID1 must be 0x1f");
    assert_eq!(stream[1], 0x8b, "{label}: ID2 must be 0x8b");
    assert_eq!(i32::from(stream[2]), Z_DEFLATED, "{label}: CM must be 8");
    assert_eq!(
        stream[3], 0,
        "{label}: FLG must be zero when no gzip header was supplied",
    );
    assert_eq!(
        &stream[4..8],
        &[0, 0, 0, 0],
        "{label}: MTIME must be zero when no gzip header was supplied",
    );
    assert_eq!(
        stream[8],
        expected_gzip_xfl(resolved_level(level), strategy),
        "{label}: XFL must follow the level and strategy",
    );
    let tail = &stream[stream.len() - 8..];
    let carried_crc = u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]);
    let carried_isize = u32::from_le_bytes([tail[4], tail[5], tail[6], tail[7]]);
    assert_eq!(
        carried_crc,
        crc32(0, payload),
        "{label}: the trailer must carry the payload's CRC-32",
    );
    assert_eq!(
        u64::from(carried_isize),
        u64::try_from(payload.len()).unwrap() % (1_u64 << 32),
        "{label}: ISIZE must carry the payload's length modulo 2^32",
    );
}

/// Asserts whichever structural markers `params.container` calls for.
fn assert_container_markers(stream: &[u8], payload: &[u8], params: Params, label: &str) {
    match params.container {
        // Raw deflate has no markers of its own to check. That it carries *neither* header
        // nor trailer is proved positively by `raw_is_exactly_what_the_wrappers_carry`,
        // which shows the raw stream is byte-for-byte the wrapped streams' payload region
        // and exactly the overhead shorter, and semantically by
        // `the_containers_are_not_interchangeable`, where a wrapped decoder rejects it.
        Container::Raw => {}
        Container::Zlib => assert_zlib_markers(stream, payload, label),
        Container::Gzip => {
            assert_gzip_markers(stream, payload, params.level, params.strategy, label);
        }
    }
}

// ---------------------------------------------------------------------------------------
// 3.1  The core matrix
// ---------------------------------------------------------------------------------------

/// Every corpus class round-trips in every container at the two levels that matter most.
///
/// This is the unconditional core: it reaches every container and every corpus shape, in
/// both feeding styles, on payloads small enough for `Miri` to interpret. Everything wider
/// than this is gated.
#[test]
fn the_core_matrix_round_trips_every_class_in_every_container() {
    for (name, payload) in core_classes() {
        for container in Container::ALL {
            for level in CORE_LEVELS {
                let params = Params::core(container, level);
                assert_round_trip(name, &payload, params, Chunk::Whole);
                assert_round_trip(name, &payload, params, Chunk::Bytes(7));
            }
        }
    }
}

/// The whole level axis, including the [`Z_DEFAULT_COMPRESSION`] sentinel.
#[test]
#[cfg_attr(
    miri,
    ignore = "11 levels x 7 classes x 3 containers: too slow to interpret, runs under native `cargo test`"
)]
fn every_level_round_trips_every_class_in_every_container() {
    for (name, payload) in small_classes() {
        for container in Container::ALL {
            for level in LEVELS {
                assert_round_trip(name, &payload, Params::new(container, level), Chunk::Whole);
            }
        }
    }
}

/// Every compression call reports the code the documentation promises.
///
/// `zlib.h` L355-L363: `Z_OK` while there is more to do, `Z_STREAM_END` once `Z_FINISH` has
/// written everything. And `deflateEnd` reports `Z_OK` unless compression was abandoned
/// mid-stream (`zlib.h` L657-L661) -- which the drivers never do, so every `deflateEnd` and
/// every `inflateEnd` in this file is asserted `Z_OK` at its call site.
#[test]
fn the_documented_status_codes_are_reported() {
    let payload = corpus::HELLO;
    for container in Container::ALL {
        let params = Params::core(container, DEF_LEVEL);
        let (mut state, mut scalars) = open_encoder(params);
        let mut out = vec![0_u8; deflate_bound_z(Some(&state), payload.len()) + BOUND_SLACK];

        // A mid-stream call with nothing to finish reports Z_OK, not Z_STREAM_END.
        let (next_in, next_out) = {
            let mut stream = DeflateStream::new(payload, &mut out);
            scalars.load_deflate(&mut stream);
            assert_eq!(
                deflate(&mut state, &mut stream, Z_NO_FLUSH),
                ReturnCode::OK,
                "{params}: a Z_NO_FLUSH call must report Z_OK",
            );
            scalars.store_deflate(&stream);
            (stream.next_in, stream.next_out)
        };
        assert_eq!(next_in, payload.len(), "{params}: the input must be taken");

        // The final Z_FINISH reports Z_STREAM_END.
        let produced = {
            let mut stream = DeflateStream::new(payload, &mut out);
            stream.next_in = next_in;
            stream.next_out = next_out;
            scalars.load_deflate(&mut stream);
            assert_eq!(
                deflate(&mut state, &mut stream, Z_FINISH),
                ReturnCode::STREAM_END,
                "{params}: the final Z_FINISH must report Z_STREAM_END",
            );
            stream.next_out
        };
        assert_eq!(
            deflate_end(&mut state),
            ReturnCode::OK,
            "{params}: deflateEnd must report Z_OK",
        );
        out.truncate(produced);
        assert_bytes_eq(
            &decompress(&out, params.inflate_window_bits(), Chunk::Whole, 64),
            payload,
            &format!("{params}: two-call round trip"),
        );
    }
}

/// All five strategies work end to end in all three containers.
#[test]
#[cfg_attr(
    miri,
    ignore = "5 strategies x 7 classes x 3 containers: too slow to interpret, runs under native `cargo test`"
)]
fn every_strategy_round_trips_in_every_container() {
    for (name, payload) in small_classes() {
        for container in Container::ALL {
            for strategy in STRATEGIES {
                let params = Params::new(container, DEF_LEVEL).with_strategy(strategy);
                assert_round_trip(name, &payload, params, Chunk::Whole);
            }
        }
    }
}

/// Every `memLevel` works end to end.
///
/// The low values are a genuine behavioural axis rather than a tuning knob: `memLevel`
/// scales `lit_bufsize`, which is both the symbol buffer's capacity and the pending buffer's
/// size (`deflate.c` L502-L505), so `memLevel == 1` makes blocks flush far sooner and
/// shrinks the largest stored block the compressor can emit directly.
#[test]
#[cfg_attr(
    miri,
    ignore = "9 memLevels x 7 classes x 3 containers: too slow to interpret, runs under native `cargo test`"
)]
fn every_mem_level_round_trips_in_every_container() {
    for (name, payload) in small_classes() {
        for container in Container::ALL {
            for mem_level in MIN_MEM_LEVEL..=MAX_MEM_LEVEL {
                let params = Params::new(container, DEF_LEVEL).with_mem_level(mem_level);
                assert_round_trip(name, &payload, params, Chunk::Whole);
            }
        }
    }
}

/// Every window size works end to end on a payload that has to cross the window.
///
/// A small window forces `fill_window` to slide and `slide_hash` to re-base the chains
/// repeatedly (`deflate.c` L187 and L252) rather than once, so this is the axis that catches
/// a defect in the slide itself.
#[test]
#[cfg_attr(
    miri,
    ignore = "7 window sizes over a 33 KiB payload: too slow to interpret, runs under native `cargo test`"
)]
fn every_window_size_round_trips_across_the_window_boundary() {
    let payload = corpus::window_crossing();
    for container in Container::ALL {
        for magnitude in 9..=MAX_WBITS {
            let params = Params::new(container, DEF_LEVEL).with_magnitude(magnitude);
            assert_round_trip(WINDOW_CROSSING, &payload, params, Chunk::Whole);
        }
    }
}

// ---------------------------------------------------------------------------------------
// The container markers
// ---------------------------------------------------------------------------------------

/// The `zlib` wrapper carries its header word and its `Adler-32` trailer.
#[test]
fn the_zlib_container_carries_its_header_word_and_adler_trailer() {
    for (name, payload) in core_classes() {
        for level in CORE_LEVELS {
            let params = Params::core(Container::Zlib, level);
            let stream = compress(&payload, params, Chunk::Whole);
            assert_zlib_markers(&stream, &payload, &format!("{name} / {params}"));
        }
    }
}

/// The `gzip` wrapper carries its magic, its fixed header fields and its
/// `CRC-32`/`ISIZE` trailer.
#[test]
fn the_gzip_container_carries_its_magic_and_crc_isize_trailer() {
    for (name, payload) in core_classes() {
        for level in CORE_LEVELS {
            let params = Params::core(Container::Gzip, level);
            let stream = compress(&payload, params, Chunk::Whole);
            assert_gzip_markers(
                &stream,
                &payload,
                params.level,
                params.strategy,
                &format!("{name} / {params}"),
            );
        }
    }
}

/// The `XFL` byte tracks the level and the strategy across the whole matrix.
#[test]
#[cfg_attr(
    miri,
    ignore = "11 levels x 5 strategies of gzip headers; runs under native `cargo test`"
)]
fn the_gzip_xfl_byte_tracks_the_level_and_strategy() {
    for level in LEVELS {
        for strategy in STRATEGIES {
            let params = Params::new(Container::Gzip, level).with_strategy(strategy);
            let stream = compress(corpus::HELLO, params, Chunk::Whole);
            assert_eq!(
                stream[8],
                expected_gzip_xfl(resolved_level(level), strategy),
                "{params}: XFL",
            );
        }
    }
}

/// A raw stream is exactly what the two wrappers wrap, and is strictly shorter than both.
///
/// This is the positive proof that raw deflate emits neither header nor trailer: the wrapped
/// forms are the raw form with a known-length prefix and suffix bolted on, byte for byte.
/// The two overheads are the *only* difference, which also pins the wrapper lengths
/// themselves -- two plus four for `zlib`, ten plus eight for `gzip`.
#[test]
#[cfg_attr(
    miri,
    ignore = "compresses every class three ways at every level; runs under native `cargo test`"
)]
fn raw_is_exactly_what_the_wrappers_carry() {
    for (name, payload) in small_classes() {
        for level in LEVELS {
            let raw = compress(&payload, Params::new(Container::Raw, level), Chunk::Whole);
            let zlib = compress(&payload, Params::new(Container::Zlib, level), Chunk::Whole);
            let gzip = compress(&payload, Params::new(Container::Gzip, level), Chunk::Whole);

            assert!(
                raw.len() < zlib.len(),
                "{name} level={level}: raw ({}) must be strictly shorter than zlib ({})",
                raw.len(),
                zlib.len(),
            );
            assert!(
                raw.len() < gzip.len(),
                "{name} level={level}: raw ({}) must be strictly shorter than gzip ({})",
                raw.len(),
                gzip.len(),
            );
            assert_eq!(
                zlib.len(),
                raw.len() + Container::Zlib.overhead(),
                "{name} level={level}: the zlib wrapper adds exactly its overhead",
            );
            assert_eq!(
                gzip.len(),
                raw.len() + Container::Gzip.overhead(),
                "{name} level={level}: the gzip wrapper adds exactly its overhead",
            );
            assert_bytes_eq(
                &zlib[2..zlib.len() - 4],
                &raw,
                &format!("{name} level={level}: the zlib payload region"),
            );
            assert_bytes_eq(
                &gzip[10..gzip.len() - 8],
                &raw,
                &format!("{name} level={level}: the gzip payload region"),
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// 3.2  Single-shot versus chunked -- the byte-level assertions
// ---------------------------------------------------------------------------------------

/// Handing the encoder one byte at a time produces exactly the bytes one call produces.
///
/// `avail_in = avail_out = 1` is `test/example.c`'s "force small buffers" case (L188), and
/// this is the assertion that the pending buffer and the flush machinery do not leak the
/// caller's call pattern into the output. Level 0 is excluded and handled by
/// [`level_zero_stored_blocks_follow_the_output_room`], for the reason
/// [`is_chunk_invariant`] documents.
///
/// One level here, because the whole level axis crossed with the whole chunk-size axis is
/// [`no_chunk_size_changes_the_compressed_bytes`]; what this one buys is that the cheapest and
/// strictest case of it -- one byte at a time, in every container, on every corpus shape -- is
/// reached without the gate.
#[test]
fn one_byte_at_a_time_compression_is_byte_identical_to_single_shot() {
    for (name, payload) in core_classes() {
        for container in Container::ALL {
            let params = Params::core(container, DEF_LEVEL);
            assert!(is_chunk_invariant(payload.len(), params));
            let single = compress(&payload, params, Chunk::Whole);
            let chunked = compress(&payload, params, Chunk::Bytes(1));
            assert_bytes_eq(
                &chunked,
                &single,
                &format!("{name} / {params}: one byte at a time versus single-shot"),
            );
        }
    }
}

/// No chunk size changes the compressed bytes, at any compressing level.
///
/// The sweep mixes small primes with powers of two deliberately: a size that happens to
/// divide an internal buffer length would hide an off-by-one that an awkward size exposes.
#[test]
#[cfg_attr(
    miri,
    ignore = "7 chunk sizes x 9 levels x 7 classes x 3 containers, chunk 1 being one call per \
     byte: too slow to interpret, runs under native `cargo test`"
)]
fn no_chunk_size_changes_the_compressed_bytes() {
    for (name, payload) in small_classes() {
        for container in Container::ALL {
            for level in 1..=Z_BEST_COMPRESSION {
                let params = Params::new(container, level);
                let single = compress(&payload, params, Chunk::Whole);
                for bytes in Chunk::SWEEP {
                    let chunked = compress(&payload, params, Chunk::Bytes(bytes));
                    assert_bytes_eq(
                        &chunked,
                        &single,
                        &format!("{name} / {params} / chunk {bytes}: compressed bytes"),
                    );
                }
            }
        }
    }
}

/// Chunk invariance survives the `memLevel` and window-size axes too.
#[test]
#[cfg_attr(
    miri,
    ignore = "sweeps memLevel and windowBits against three chunk sizes; runs under native `cargo test`"
)]
fn no_chunk_size_changes_the_compressed_bytes_at_any_buffer_shape() {
    let payload = corpus::text();
    for container in Container::ALL {
        for mem_level in MIN_MEM_LEVEL..=MAX_MEM_LEVEL {
            for magnitude in 9..=MAX_WBITS {
                let params = Params::new(container, DEF_LEVEL)
                    .with_mem_level(mem_level)
                    .with_magnitude(magnitude);
                let single = compress(&payload, params, Chunk::Whole);
                for bytes in [1_usize, 13, 8192] {
                    let chunked = compress(&payload, params, Chunk::Bytes(bytes));
                    assert_bytes_eq(
                        &chunked,
                        &single,
                        &format!("text / {params} / chunk {bytes}: compressed bytes"),
                    );
                }
            }
        }
    }
}

/// Level 0's stored blocks follow the output room, and that is the reference's behaviour.
///
/// `deflate_stored` copies as many whole stored blocks straight to `next_out` as the
/// *available output space* allows (`deflate.c` L1668-L1720), so the feeding style is visible
/// in the emitted bytes as soon as a payload is long enough to be split. Two halves,
/// measured against the in-tree C implementation and reproduced here:
///
/// * a payload no longer than `min_block` is never split, so its bytes are chunk-invariant;
/// * the window-crossing payload *is* split differently, its bytes differ -- and both forms
///   still decode to exactly the payload, which is what makes the divergence benign.
///
/// The second half is asserted positively rather than merely tolerated: an implementation
/// that produced the same bytes either way would have stopped honouring `avail_out` and would
/// no longer match the reference.
#[test]
#[cfg_attr(
    miri,
    ignore = "the second half drives a 33 KiB payload one byte at a time; runs under native `cargo test`"
)]
fn level_zero_stored_blocks_follow_the_output_room() {
    // Short enough never to be split: identical bytes whatever the feeding style.
    for (name, payload) in small_classes() {
        if payload.len() > LEVEL_ZERO_STABLE_LEN {
            continue;
        }
        for container in Container::ALL {
            for mem_level in MIN_MEM_LEVEL..=MAX_MEM_LEVEL {
                let params = Params::new(container, Z_NO_COMPRESSION).with_mem_level(mem_level);
                let single = compress(&payload, params, Chunk::Whole);
                for bytes in [1_usize, 13, 8192] {
                    let chunked = compress(&payload, params, Chunk::Bytes(bytes));
                    assert_bytes_eq(
                        &chunked,
                        &single,
                        &format!("{name} / {params} / chunk {bytes}: stored bytes"),
                    );
                }
            }
        }
    }

    // Long enough to be split: different bytes, same payload.
    let payload = corpus::window_crossing();
    for container in Container::ALL {
        let params = Params::new(container, Z_NO_COMPRESSION);
        let single = compress(&payload, params, Chunk::Whole);
        let chunked = compress(&payload, params, Chunk::Bytes(1));
        assert_ne!(
            chunked, single,
            "{params}: a 33 KiB stored stream fed one byte at a time must segment its \
             blocks differently, exactly as the C implementation does",
        );
        for stream in [&single, &chunked] {
            assert_bytes_eq(
                &decompress(
                    stream,
                    params.inflate_window_bits(),
                    Chunk::Whole,
                    payload.len() + BOUND_SLACK,
                ),
                &payload,
                &format!("{params}: both stored segmentations decode to the payload"),
            );
        }
    }
}

/// Decompression recovers the payload at every chunk size.
#[test]
#[cfg_attr(
    miri,
    ignore = "7 chunk sizes x 7 classes x 3 containers on the decode side; runs under native `cargo test`"
)]
fn no_chunk_size_changes_what_decompression_recovers() {
    for (name, payload) in small_classes() {
        for container in Container::ALL {
            let params = Params::new(container, DEF_LEVEL);
            let stream = compress(&payload, params, Chunk::Whole);
            for bytes in Chunk::SWEEP {
                let recovered = decompress(
                    &stream,
                    params.inflate_window_bits(),
                    Chunk::Bytes(bytes),
                    payload.len() + BOUND_SLACK,
                );
                assert_bytes_eq(
                    &recovered,
                    &payload,
                    &format!("{name} / {params} / chunk {bytes}: recovered bytes"),
                );
            }
        }
    }
}

/// `test/example.c`'s `test_deflate` and `test_inflate`, reproduced end to end.
///
/// Both C functions force `avail_in = avail_out = 1` for the whole stream (L188 and L226) on
/// the `hello` fixture, which is exactly 14 bytes because every use feeds it as
/// `strlen(hello) + 1` (L69, L95, L175, L341 and L434). The pair is the C suite's smallest
/// complete statement about incremental operation, and it is reproduced verbatim rather than
/// generalised.
#[test]
fn the_example_c_one_byte_pair_round_trips() {
    assert_eq!(
        corpus::HELLO.len(),
        14,
        "test/example.c L35 feeds hello as strlen + 1, so the fixture is 14 bytes",
    );
    let params = Params::core(Container::Zlib, Z_DEFAULT_COMPRESSION);
    let stream = compress(corpus::HELLO, params, Chunk::Bytes(1));
    let recovered = decompress(
        &stream,
        params.inflate_window_bits(),
        Chunk::Bytes(1),
        corpus::HELLO.len() + BOUND_SLACK,
    );
    assert_bytes_eq(
        &recovered,
        corpus::HELLO,
        "test_deflate / test_inflate pair",
    );
}

/// A decompressor fed one byte at a time makes progress or says why it cannot, and never
/// loses a byte.
///
/// `zlib.h` L360-L363 splits the outcomes exactly two ways: `Z_OK` when something moved, and
/// `Z_BUF_ERROR` when nothing could. This walks a whole stream with `avail_in == 1` and
/// `avail_out == 1` and checks that every call falls on the right side of that line -- a
/// `Z_OK` that consumed nothing and produced nothing would be a silent stall, and a
/// `Z_BUF_ERROR` while room remained would be a lost byte.
#[test]
fn one_byte_at_a_time_decompression_either_progresses_or_reports_buf_error() {
    let payload = core_payload(&corpus::text());
    let params = Params::core(Container::Zlib, DEF_LEVEL);
    let stream = compress(&payload, params, Chunk::Whole);

    let (mut state, mut scalars) = open_decoder(params.inflate_window_bits());
    let mut out = vec![0_u8; payload.len() + BOUND_SLACK];
    let mut next_in = 0_usize;
    let mut next_out = 0_usize;
    let mut calls = 0_usize;
    let code = loop {
        calls += 1;
        assert!(
            calls <= 8 * (stream.len() + payload.len() + 16),
            "a one-byte-at-a-time decode must terminate",
        );
        let in_limit = (next_in + 1).min(stream.len());
        let out_limit = (next_out + 1).min(out.len());
        let before = (next_in, next_out);
        let code = {
            let mut view = InflateStream::new(&stream[..in_limit], &mut out[..out_limit]);
            view.next_in = next_in;
            view.next_out = next_out;
            scalars.load_inflate(&mut view);
            let code = inflate(&mut state, &mut view, Z_NO_FLUSH);
            next_in = view.next_in;
            next_out = view.next_out;
            scalars.store_inflate(&view);
            code
        };
        let progressed = before != (next_in, next_out);
        match code {
            ReturnCode::OK => assert!(
                progressed,
                "call {calls} reported Z_OK without consuming or producing anything, \
                 with {} input bytes and {} output bytes still to go",
                stream.len() - next_in,
                out.len() - next_out,
            ),
            ReturnCode::BUF_ERROR => {
                assert!(
                    !progressed,
                    "call {calls} reported Z_BUF_ERROR after making progress",
                );
                break code;
            }
            ReturnCode::STREAM_END => break code,
            other => panic!("call {calls} reported {other:?}"),
        }
    };

    assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    assert_eq!(
        code,
        ReturnCode::STREAM_END,
        "a one-byte-at-a-time decode of a complete stream must reach Z_STREAM_END",
    );
    out.truncate(next_out);
    assert_bytes_eq(&out, &payload, "one byte at a time decode");
    assert_eq!(
        next_in,
        stream.len(),
        "every input byte must have been consumed",
    );
}

// ---------------------------------------------------------------------------------------
// 3.3  Determinism
// ---------------------------------------------------------------------------------------

/// The same input and parameters produce the same bytes, twice in one process.
///
/// The cheapest available guard against a dependence on uninitialised memory or on an
/// allocation's address. It matters because the reference allocator does *not* zero: the
/// generic `zcalloc` reads `sizeof(uInt) > 2 ? malloc(items * size) : calloc(items, size)`
/// (`zutil.c` L299-L303) and `uInt` is four bytes wide on every supported target, so the
/// branch taken is always `malloc`.
#[test]
fn compression_is_deterministic() {
    for (name, payload) in core_classes() {
        assert_deterministic(name, &payload, Params::core(Container::Zlib, DEF_LEVEL));
    }
}

/// Determinism holds in every container at every core level.
#[test]
#[cfg_attr(
    miri,
    ignore = "widens the determinism check to 3 containers x 2 levels; runs under native `cargo test`"
)]
fn compression_is_deterministic_in_every_container() {
    for (name, payload) in small_classes() {
        for container in Container::ALL {
            for level in CORE_LEVELS {
                assert_deterministic(name, &payload, Params::new(container, level));
            }
        }
    }
}

/// Compresses `payload` twice in each feeding style and asserts the pairs agree.
///
/// A fresh stream each time, which is what makes this a statement about the *implementation*
/// rather than about one state object: two independent runs must not be able to tell each
/// other apart, whatever the allocator handed them.
fn assert_deterministic(name: &str, payload: &[u8], params: Params) {
    for chunk in [Chunk::Whole, Chunk::Bytes(3)] {
        let first = compress(payload, params, chunk);
        let second = compress(payload, params, chunk);
        assert_bytes_eq(
            &second,
            &first,
            &format!("{name} / {params} / {chunk}: two runs in one process"),
        );
    }
}

/// The compressed bytes do not change when every allocated block arrives full of `0xa5`.
///
/// [`TrackingAllocator`] is the port of `test/infcover.c`'s instrumented allocator, which
/// fills each block with a non-zero byte precisely so that "the code isn't depending on
/// zeros" (L87). Comparing its output against the global allocator's is a two-line assertion
/// that catches an entire defect class: if the two ever differ, something read memory it had
/// not written. The tracker's books are checked as well, so a leak or an out-of-order release
/// fails here too.
#[test]
fn compression_does_not_depend_on_the_allocator() {
    for (name, payload) in core_classes() {
        assert_allocator_independent(name, &payload, Params::core(Container::Zlib, DEF_LEVEL));
    }
}

/// Allocator independence holds in every container at every core level.
#[test]
#[cfg_attr(
    miri,
    ignore = "widens the allocator check to 3 containers x 2 levels; runs under native `cargo test`"
)]
fn compression_does_not_depend_on_the_allocator_in_any_container() {
    for (name, payload) in small_classes() {
        for container in Container::ALL {
            for level in CORE_LEVELS {
                assert_allocator_independent(name, &payload, Params::new(container, level));
            }
        }
    }
}

/// Compresses `payload` through the global allocator and through the sentinel-filling tracker
/// and asserts the bytes agree, in both feeding styles.
///
/// The chunked leg is not redundant: the state is reused across far more calls there, so a
/// read of memory that was never written has many more chances to show itself.
fn assert_allocator_independent(name: &str, payload: &[u8], params: Params) {
    for chunk in [Chunk::Whole, Chunk::Bytes(3)] {
        let baseline = compress(payload, params, chunk);
        let sentinel = TrackingAllocator::new();
        let under_sentinel = compress_with(payload, params, chunk, &sentinel);
        assert_bytes_eq(
            &under_sentinel,
            &baseline,
            &format!(
                "{name} / {params} / {chunk}: a 0xa5-filling allocator must not change the \
                 compressed bytes"
            ),
        );
        assert!(
            sentinel.high_water() > 0,
            "{name} / {params} / {chunk}: the tracker must actually have served the stream",
        );
        sentinel.assert_clean();
    }
}

/// The structural markers hold across the whole level and strategy matrix, not just at the
/// two core levels.
#[test]
#[cfg_attr(
    miri,
    ignore = "sweeps every level and strategy in both wrapped containers; runs under native `cargo test`"
)]
fn the_container_markers_hold_across_the_matrix() {
    for (name, payload) in small_classes() {
        for container in [Container::Zlib, Container::Gzip] {
            for level in LEVELS {
                for strategy in STRATEGIES {
                    let params = Params::new(container, level).with_strategy(strategy);
                    let stream = compress(&payload, params, Chunk::Whole);
                    assert_container_markers(
                        &stream,
                        &payload,
                        params,
                        &format!("{name} / {params}"),
                    );
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// 3.4  Flush-mode interaction
// ---------------------------------------------------------------------------------------

/// The canonical empty stored block a byte-aligning flush ends with: `BFINAL == 0`,
/// `BTYPE == 00`, `LEN == 0`, `NLEN == 0xffff` (`doc/rfc1951.txt` L400-L412).
const SYNC_MARKER: [u8; 4] = [0x00, 0x00, 0xff, 0xff];

/// A stream compressed in segments, with the boundaries recorded.
#[derive(Debug)]
struct Segmented {
    /// The complete stream.
    bytes: Vec<u8>,
    /// The output length after each mid-stream flush, in order.
    marks: Vec<usize>,
    /// How much input had been fed by the time of the matching entry in
    /// [`Segmented::marks`].
    fed: Vec<usize>,
}

/// Where segment `index` of `segments` ends, for a payload of `len` bytes.
fn segment_end(len: usize, segments: usize, index: usize) -> usize {
    debug_assert!(index < segments);
    len * (index + 1) / segments
}

/// Compresses `payload` in `segments` pieces separated by `flush`, then finishes.
///
/// A mid-stream flush is driven to completion the way `zlib.h` L326-L328 prescribes: keep
/// calling until `avail_out` comes back non-zero, which is what tells the caller the flush has
/// actually drained rather than merely been requested.
///
/// # Panics
///
/// If any call reports anything but `Z_OK`, or the finishing calls anything but `Z_OK` or
/// `Z_STREAM_END`.
fn compress_segmented(payload: &[u8], params: Params, flush: i32, segments: usize) -> Segmented {
    assert!(segments >= 1, "a stream has at least one segment");
    let (mut state, mut scalars) = open_encoder(params);
    let bound = deflate_bound_z(Some(&state), payload.len());
    let mut out = vec![0_u8; bound + BOUND_SLACK + 128 * segments];
    let mut next_in = 0_usize;
    let mut next_out = 0_usize;
    let mut marks = Vec::with_capacity(segments);
    let mut fed = Vec::with_capacity(segments);

    for index in 0..segments {
        let end = segment_end(payload.len(), segments, index);
        loop {
            ensure_room(&mut out, next_out, Chunk::Whole);
            let room = out.len();
            let code = {
                let mut stream = DeflateStream::new(&payload[..end], &mut out[..room]);
                stream.next_in = next_in;
                stream.next_out = next_out;
                scalars.load_deflate(&mut stream);
                let code = deflate(&mut state, &mut stream, flush);
                next_in = stream.next_in;
                next_out = stream.next_out;
                scalars.store_deflate(&stream);
                code
            };
            check_err(code, "deflate(mid-stream flush)");
            if next_in == end && next_out < out.len() {
                break;
            }
        }
        marks.push(next_out);
        fed.push(end);
    }

    loop {
        ensure_room(&mut out, next_out, Chunk::Whole);
        let room = out.len();
        let code = {
            let mut stream = DeflateStream::new(payload, &mut out[..room]);
            stream.next_in = next_in;
            stream.next_out = next_out;
            scalars.load_deflate(&mut stream);
            let code = deflate(&mut state, &mut stream, Z_FINISH);
            next_in = stream.next_in;
            next_out = stream.next_out;
            scalars.store_deflate(&stream);
            code
        };
        if code == ReturnCode::STREAM_END {
            break;
        }
        check_err(code, "deflate(Z_FINISH)");
    }

    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    out.truncate(next_out);
    Segmented {
        bytes: out,
        marks,
        fed,
    }
}

/// A stream broken up by any of the four mid-stream flushes still recovers completely.
///
/// The four differ in what they emit and in what they reset -- `Z_PARTIAL_FLUSH` aligns with
/// `_tr_align`, `Z_SYNC_FLUSH` and `Z_FULL_FLUSH` emit an empty stored block, `Z_FULL_FLUSH`
/// additionally clears the hash chains, and `Z_BLOCK` closes the block without aligning to a
/// byte (`deflate.c` L1216-L1240) -- but all four must leave a stream that decodes to exactly
/// the input.
#[test]
fn every_mid_stream_flush_leaves_a_recoverable_stream() {
    let payload = core_payload(&corpus::text());
    for flush in [Z_PARTIAL_FLUSH, Z_SYNC_FLUSH, Z_FULL_FLUSH, Z_BLOCK] {
        for container in Container::ALL {
            let params = Params::core(container, DEF_LEVEL);
            let segmented = compress_segmented(&payload, params, flush, 4);
            let recovered = decompress(
                &segmented.bytes,
                params.inflate_window_bits(),
                Chunk::Whole,
                payload.len() + BOUND_SLACK,
            );
            assert_bytes_eq(
                &recovered,
                &payload,
                &format!("{params} / flush={flush}: four segments"),
            );
        }
    }
}

/// A byte-aligning flush ends with the empty stored block, and everything fed so far can be
/// read back from everything emitted so far.
///
/// That second property is the *definition* of a sync flush (`zlib.h` L296-L302): the point of
/// the marker is that a decompressor which has seen only the bytes written up to it can
/// nevertheless produce every input byte fed up to it. Asserting the marker without asserting
/// the recovery would check the shape and miss the meaning.
#[test]
fn a_byte_aligning_flush_ends_with_the_sync_marker_and_hands_over_everything_fed() {
    let payload = core_payload(&corpus::text());
    for flush in [Z_SYNC_FLUSH, Z_FULL_FLUSH] {
        for container in Container::ALL {
            let params = Params::core(container, DEF_LEVEL);
            let segmented = compress_segmented(&payload, params, flush, 3);
            for (&mark, &fed) in segmented.marks.iter().zip(&segmented.fed) {
                assert!(
                    mark >= SYNC_MARKER.len(),
                    "{params} / flush={flush}: a flush emits at least the marker",
                );
                assert_eq!(
                    &segmented.bytes[mark - SYNC_MARKER.len()..mark],
                    &SYNC_MARKER,
                    "{params} / flush={flush}: the flush must end with the empty stored block",
                );

                // Everything emitted so far, decoded: an incomplete stream, so the decoder
                // runs out of input rather than reaching Z_STREAM_END -- but it must have
                // produced every byte that had been fed.
                let partial = inflate_all(
                    &segmented.bytes[..mark],
                    params.inflate_window_bits(),
                    Chunk::Whole,
                    Z_NO_FLUSH,
                    payload.len() + BOUND_SLACK,
                );
                assert!(
                    matches!(partial.code, ReturnCode::OK | ReturnCode::BUF_ERROR),
                    "{params} / flush={flush}: a truncated stream stops for want of input, \
                     got {:?}",
                    partial.code,
                );
                assert_bytes_eq(
                    &partial.output,
                    &payload[..fed],
                    &format!("{params} / flush={flush}: everything fed up to the marker"),
                );
            }
        }
    }
}

/// `test/example.c`'s `test_flush` and `test_sync`, reproduced end to end.
///
/// The richest single scenario in the C suite, because it exercises three properties at once:
/// that a `Z_FULL_FLUSH` makes what follows independent of what came before, that a corrupted
/// first block does not prevent the rest from being read, and that `inflateSync` finds the next
/// usable block. The sequence is `test_flush` (L338-L368) followed by `test_sync` (L373-L409):
///
/// 1. compress the first **three** bytes of `hello` with `Z_FULL_FLUSH`;
/// 2. corrupt the **fourth output byte** -- `compr[3]++` at L357, "force an error in first
///    compressed block";
/// 3. compress the remaining eleven bytes with `Z_FINISH`;
/// 4. decode by handing the decompressor **only the two-byte header** first, then widening the
///    input to the whole stream;
/// 5. `inflateSync` to skip the damaged part, which must report `Z_OK`;
/// 6. `inflate(Z_FINISH)`, which must report `Z_STREAM_END`.
///
/// The tail that comes back is `hello[3..]`, which is why C prints it as `"hel"` followed by the
/// recovered bytes (L408).
#[test]
fn the_example_c_flush_and_sync_sequence_recovers_after_corruption() {
    const HEADER_ONLY: usize = 2;
    const FIRST_SEGMENT: usize = 3;
    const CORRUPTED_BYTE: usize = 3;

    let hello = corpus::HELLO;
    let params = Params::core(Container::Zlib, Z_DEFAULT_COMPRESSION);

    // --- test_flush ---------------------------------------------------------------------
    let (mut state, mut scalars) = open_encoder(params);
    let mut compr = vec![0_u8; 4096];

    let (next_in, next_out) = {
        let mut stream = DeflateStream::new(&hello[..FIRST_SEGMENT], &mut compr);
        scalars.load_deflate(&mut stream);
        check_err(
            deflate(&mut state, &mut stream, Z_FULL_FLUSH),
            "deflate(Z_FULL_FLUSH)",
        );
        scalars.store_deflate(&stream);
        (stream.next_in, stream.next_out)
    };
    assert_eq!(
        next_in, FIRST_SEGMENT,
        "the first three bytes must be taken"
    );
    assert!(
        next_out > CORRUPTED_BYTE,
        "the full flush must have emitted the byte the C suite corrupts",
    );

    // `compr[3]++` (L357). `wrapping_add` is the faithful translation: C's `unsigned char`
    // increment wraps, and Rust's `+` would panic on 0xff in a debug build.
    compr[CORRUPTED_BYTE] = compr[CORRUPTED_BYTE].wrapping_add(1);

    let compressed_len = {
        let mut stream = DeflateStream::new(hello, &mut compr);
        stream.next_in = next_in;
        stream.next_out = next_out;
        scalars.load_deflate(&mut stream);
        assert_eq!(
            deflate(&mut state, &mut stream, Z_FINISH),
            ReturnCode::STREAM_END,
            "the remaining {} bytes must finish the stream",
            hello.len() - FIRST_SEGMENT,
        );
        stream.next_out
    };
    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    compr.truncate(compressed_len);

    // --- test_sync ----------------------------------------------------------------------
    let (mut decoder, mut scalars) = open_decoder(params.inflate_window_bits());
    let mut recovered = vec![0_u8; 256];

    // Step 4a: `avail_in = 2` -- just read the zlib header (L385).
    let (mut next_in, mut next_out) = {
        let mut stream = InflateStream::new(&compr[..HEADER_ONLY], &mut recovered);
        scalars.load_inflate(&mut stream);
        check_err(inflate(&mut decoder, &mut stream, Z_NO_FLUSH), "inflate");
        scalars.store_inflate(&stream);
        (stream.next_in, stream.next_out)
    };
    assert_eq!(next_in, HEADER_ONLY, "the header must have been consumed");
    assert_eq!(next_out, 0, "the header alone produces no output");

    // Step 5: widen the input to the whole stream, then skip the damaged part (L396-L398).
    {
        let mut stream = InflateStream::new(&compr, &mut recovered);
        stream.next_in = next_in;
        stream.next_out = next_out;
        scalars.load_inflate(&mut stream);
        check_err(inflate_sync(&mut decoder, &mut stream), "inflateSync");
        next_in = stream.next_in;
        next_out = stream.next_out;
        scalars.store_inflate(&stream);
    }

    // Step 6: finish (L400-L404).
    let produced = {
        let mut stream = InflateStream::new(&compr, &mut recovered);
        stream.next_in = next_in;
        stream.next_out = next_out;
        scalars.load_inflate(&mut stream);
        assert_eq!(
            inflate(&mut decoder, &mut stream, Z_FINISH),
            ReturnCode::STREAM_END,
            "inflate must report Z_STREAM_END after the sync",
        );
        stream.next_out
    };
    assert_eq!(inflate_end(&mut decoder), ReturnCode::OK);
    recovered.truncate(produced);

    assert_bytes_eq(
        &recovered,
        &hello[FIRST_SEGMENT..],
        "the tail after the corrupted first block",
    );
}

// ---------------------------------------------------------------------------------------
// 3.5  Preset dictionary, both directions
// ---------------------------------------------------------------------------------------

/// A stream compressed against a preset dictionary, plus the dictionary's identity.
#[derive(Debug)]
struct DictStream {
    /// The complete stream.
    bytes: Vec<u8>,
    /// The value `z_stream.adler` carried after `deflateSetDictionary`, which is the
    /// `Adler-32` of the dictionary for a `zlib` stream and is untouched for a raw one.
    dict_id: u32,
}

/// Compresses `payload` against `dictionary` with `params`.
///
/// `deflateSetDictionary` must be called before any compression, and it updates `adler` and
/// `total_in` on the caller's stream (`deflate.c` L565-L600), which is why this port takes
/// both by mutable reference -- the caller owns those fields.
///
/// # Panics
///
/// If `deflateSetDictionary` is refused, if the finishing call does not report
/// `Z_STREAM_END`, or if `deflateEnd` does not report `Z_OK`.
fn compress_with_dictionary(payload: &[u8], dictionary: &[u8], params: Params) -> DictStream {
    let (mut state, mut scalars) = open_encoder(params);
    assert_eq!(
        deflate_set_dictionary(
            &mut state,
            &mut scalars.adler,
            &mut scalars.total_in,
            dictionary,
        ),
        ReturnCode::OK,
        "{params}: deflateSetDictionary before any compression must be accepted",
    );
    let dict_id = scalars.adler;

    let mut out = vec![0_u8; deflate_bound_z(Some(&state), payload.len()) + BOUND_SLACK];
    let produced = {
        let mut stream = DeflateStream::new(payload, &mut out);
        scalars.load_deflate(&mut stream);
        assert_eq!(
            deflate(&mut state, &mut stream, Z_FINISH),
            ReturnCode::STREAM_END,
            "{params}: Z_FINISH must complete the dictionary stream",
        );
        stream.next_out
    };
    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    out.truncate(produced);
    DictStream {
        bytes: out,
        dict_id,
    }
}

/// `test/example.c`'s `test_dict_deflate` and `test_dict_inflate`, reproduced end to end.
///
/// The dictionary is **six** bytes, not five: L426 and L477 pass `(int)sizeof(dictionary)`, and
/// `dictionary[] = "hello"` includes its terminating NUL. Using `strlen` instead would change
/// the `Adler-32` and break the `Z_NEED_DICT` comparison the C suite makes at L472, which is
/// exactly the comparison reproduced here.
#[test]
fn the_example_c_dictionary_pair_round_trips() {
    assert_eq!(
        corpus::DICTIONARY.len(),
        6,
        "test/example.c passes sizeof(dictionary), so the NUL is included",
    );

    let params = Params::core(Container::Zlib, Z_BEST_COMPRESSION);
    let compressed = compress_with_dictionary(corpus::HELLO, corpus::DICTIONARY, params);
    assert_eq!(
        compressed.dict_id,
        adler32(ADLER32_INITIAL_VALUE, corpus::DICTIONARY),
        "deflateSetDictionary must publish the dictionary's Adler-32",
    );

    let (mut decoder, mut scalars) = open_decoder(params.inflate_window_bits());
    let mut recovered = vec![0_u8; corpus::HELLO.len() + BOUND_SLACK];

    let (next_in, next_out) = {
        let mut stream = InflateStream::new(&compressed.bytes, &mut recovered);
        scalars.load_inflate(&mut stream);
        assert_eq!(
            inflate(&mut decoder, &mut stream, Z_NO_FLUSH),
            ReturnCode::NEED_DICT,
            "a preset-dictionary stream must stop and ask for the dictionary",
        );
        assert_eq!(
            stream.adler,
            Some(compressed.dict_id),
            "and must publish the dictionary's Adler-32 while doing so",
        );
        scalars.store_inflate(&stream);
        (stream.next_in, stream.next_out)
    };
    assert_eq!(
        next_out, 0,
        "nothing may be produced before the dictionary is supplied",
    );

    assert_eq!(
        inflate_set_dictionary(&mut decoder, corpus::DICTIONARY),
        ReturnCode::OK,
        "the matching dictionary must be accepted",
    );

    let produced = {
        let mut stream = InflateStream::new(&compressed.bytes, &mut recovered);
        stream.next_in = next_in;
        stream.next_out = next_out;
        scalars.load_inflate(&mut stream);
        assert_eq!(
            inflate(&mut decoder, &mut stream, Z_NO_FLUSH),
            ReturnCode::STREAM_END,
            "the stream must complete once the dictionary is in place",
        );
        stream.next_out
    };
    assert_eq!(inflate_end(&mut decoder), ReturnCode::OK);
    recovered.truncate(produced);
    assert_bytes_eq(&recovered, corpus::HELLO, "dictionary round trip");
}

/// The `zlib` header advertises the dictionary and the four bytes after it identify which one.
///
/// `FDICT`, bit 5 of `FLG`, is set exactly when a dictionary was used, and the dictionary's
/// `Adler-32` then follows the two header bytes, most significant byte first
/// (`doc/rfc1950.txt` L232-L241 and L285-L297). [`PRESET_DICT`] is that bit.
#[test]
fn a_dictionary_stream_advertises_the_dictionary_in_its_header() {
    let params = Params::core(Container::Zlib, Z_BEST_COMPRESSION);

    let with_dictionary = compress_with_dictionary(corpus::HELLO, corpus::DICTIONARY, params);
    assert_ne!(
        u32::from(with_dictionary.bytes[1]) & PRESET_DICT,
        0,
        "FDICT must be set when a dictionary was used",
    );
    let published = u32::from_be_bytes([
        with_dictionary.bytes[2],
        with_dictionary.bytes[3],
        with_dictionary.bytes[4],
        with_dictionary.bytes[5],
    ]);
    assert_eq!(
        published, with_dictionary.dict_id,
        "the four bytes after the header must be the dictionary's Adler-32",
    );
    assert_zlib_markers(&with_dictionary.bytes, corpus::HELLO, "dictionary stream");

    let without = compress(corpus::HELLO, params, Chunk::Whole);
    assert_eq!(
        u32::from(without[1]) & PRESET_DICT,
        0,
        "FDICT must be clear when no dictionary was used",
    );
}

/// Without the dictionary the decoder stops rather than producing something wrong.
#[test]
fn a_missing_dictionary_stops_the_decoder() {
    let params = Params::core(Container::Zlib, Z_BEST_COMPRESSION);
    let compressed = compress_with_dictionary(corpus::HELLO, corpus::DICTIONARY, params);

    let result = inflate_all(
        &compressed.bytes,
        params.inflate_window_bits(),
        Chunk::Whole,
        Z_NO_FLUSH,
        corpus::HELLO.len() + BOUND_SLACK,
    );
    assert_eq!(
        result.code,
        ReturnCode::NEED_DICT,
        "a decoder without the dictionary must stop at Z_NEED_DICT",
    );
    assert!(
        result.output.is_empty(),
        "and must not have produced anything, let alone something wrong",
    );
    assert_eq!(
        result.adler, compressed.dict_id,
        "the reported check value names the dictionary it wants",
    );
}

/// The wrong dictionary is rejected outright, not decoded into rubbish.
///
/// `inflateSetDictionary` verifies the dictionary against the `Adler-32` the stream published
/// and answers `Z_DATA_ERROR` on a mismatch (`inflate.c` L1199-L1205). That check is the only
/// thing standing between a caller with the wrong dictionary and silently corrupt output.
#[test]
fn a_wrong_dictionary_is_rejected() {
    let params = Params::core(Container::Zlib, Z_BEST_COMPRESSION);
    let compressed = compress_with_dictionary(corpus::HELLO, corpus::DICTIONARY, params);

    let (mut decoder, scalars) = open_decoder(params.inflate_window_bits());
    let mut recovered = vec![0_u8; corpus::HELLO.len() + BOUND_SLACK];
    {
        let mut stream = InflateStream::new(&compressed.bytes, &mut recovered);
        scalars.load_inflate(&mut stream);
        assert_eq!(
            inflate(&mut decoder, &mut stream, Z_NO_FLUSH),
            ReturnCode::NEED_DICT,
        );
    }

    // Same length, different bytes, therefore a different Adler-32.
    let wrong = b"world\0";
    assert_eq!(wrong.len(), corpus::DICTIONARY.len());
    assert_ne!(
        adler32(ADLER32_INITIAL_VALUE, wrong),
        compressed.dict_id,
        "the fixture must genuinely be a different dictionary",
    );
    assert_eq!(
        inflate_set_dictionary(&mut decoder, wrong),
        ReturnCode::DATA_ERROR,
        "a dictionary whose Adler-32 does not match must be refused",
    );
    assert_eq!(inflate_end(&mut decoder), ReturnCode::OK);
}

/// A raw stream can be primed with a dictionary too, and must be.
///
/// Raw deflate has no header, so there is no `FDICT` bit and no `Z_NEED_DICT` to signal with:
/// the decoder has to be given the dictionary up front, before decoding starts, and
/// `inflateSetDictionary` accepts it at that point for exactly that reason (`inflate.c`
/// L1196-L1197 admits a raw stream unconditionally). Without it the back-references reach past
/// the start of the data and the stream is rejected.
#[test]
fn a_raw_stream_can_be_primed_with_a_dictionary() {
    let params = Params::core(Container::Raw, DEF_LEVEL);
    let with_dictionary = compress_with_dictionary(corpus::HELLO, corpus::DICTIONARY, params);
    let without = compress(corpus::HELLO, params, Chunk::Whole);
    assert!(
        with_dictionary.bytes.len() < without.len(),
        "the dictionary must actually have been used: {} bytes with it, {} without",
        with_dictionary.bytes.len(),
        without.len(),
    );

    // With the dictionary supplied before decoding starts.
    let (mut decoder, scalars) = open_decoder(params.inflate_window_bits());
    assert_eq!(
        inflate_set_dictionary(&mut decoder, corpus::DICTIONARY),
        ReturnCode::OK,
        "a raw decoder accepts a dictionary before decoding starts",
    );
    let mut recovered = vec![0_u8; corpus::HELLO.len() + BOUND_SLACK];
    let produced = {
        let mut stream = InflateStream::new(&with_dictionary.bytes, &mut recovered);
        scalars.load_inflate(&mut stream);
        assert_eq!(
            inflate(&mut decoder, &mut stream, Z_FINISH),
            ReturnCode::STREAM_END,
            "a primed raw decoder must complete the stream",
        );
        stream.next_out
    };
    assert_eq!(inflate_end(&mut decoder), ReturnCode::OK);
    recovered.truncate(produced);
    assert_bytes_eq(&recovered, corpus::HELLO, "raw dictionary round trip");

    // And without it, the back-references have nowhere to point.
    let unprimed = inflate_all(
        &with_dictionary.bytes,
        params.inflate_window_bits(),
        Chunk::Whole,
        Z_NO_FLUSH,
        corpus::HELLO.len() + BOUND_SLACK,
    );
    assert_eq!(
        unprimed.code,
        ReturnCode::DATA_ERROR,
        "an unprimed raw decoder must reject a distance reaching into the dictionary",
    );
    assert_ne!(
        unprimed.output.as_slice(),
        corpus::HELLO,
        "and must certainly not recover the payload by accident",
    );
}

// ---------------------------------------------------------------------------------------
// 3.6  The one-shot wrappers
// ---------------------------------------------------------------------------------------

/// The levels the one-shot sweep uses: both extremes plus the two that bracket the default.
const ONE_SHOT_LEVELS: [i32; 4] = [
    Z_NO_COMPRESSION,
    Z_BEST_SPEED,
    DEF_LEVEL,
    Z_BEST_COMPRESSION,
];

/// `compress2` then `uncompress2` recovers every class, with the reported counts correct.
///
/// The counts are as much of the contract as the bytes: `compress2` reports what it wrote and
/// `uncompress2` reports both what it wrote and what it read, and a caller walking concatenated
/// members depends on the latter being exactly the stream's length.
#[test]
#[cfg_attr(
    miri,
    ignore = "the one-shot wrappers are fixed to the reference buffer shape, which \
     Params::core cannot shrink; runs under native `cargo test`"
)]
fn the_one_shot_wrappers_round_trip_every_class() {
    for (name, payload) in core_classes() {
        for level in ONE_SHOT_LEVELS {
            let bound = compress_bound(payload.len());
            let mut dest = vec![0_u8; bound];
            let compressed = compress2(&mut dest, &payload, level);
            assert_eq!(
                compressed.code,
                ReturnCode::OK,
                "{name} level={level}: compress2 into a compressBound-sized buffer",
            );
            assert!(
                compressed.produced <= bound,
                "{name} level={level}: compress2 wrote {} bytes into {bound}",
                compressed.produced,
            );
            dest.truncate(compressed.produced);

            // One byte more than the payload needs, so a wrapper that over-produced would be
            // visible in `produced` rather than hidden by an exactly-sized buffer.
            let mut back = vec![0_u8; payload.len() + 1];
            let decompressed = uncompress2(&mut back, &dest);
            assert_eq!(
                decompressed.code,
                ReturnCode::OK,
                "{name} level={level}: uncompress2",
            );
            assert_eq!(
                decompressed.consumed,
                dest.len(),
                "{name} level={level}: uncompress2 must consume the whole stream",
            );
            assert_eq!(
                decompressed.produced,
                payload.len(),
                "{name} level={level}: uncompress2 must produce the whole payload",
            );
            back.truncate(decompressed.produced);
            assert_bytes_eq(&back, &payload, &format!("{name} level={level}: one-shot"));
        }
    }
}

/// The one-shot path emits exactly the bytes the streaming path emits.
///
/// `compress2` is documented as a `deflateInit`/`deflate`/`deflateEnd` cycle (`zlib.h`
/// L1280-L1290), so any divergence would be a divergence in the wrapper's flush selection or in
/// its choice of `windowBits` and `memLevel` -- all of which are meant to be the defaults.
#[test]
#[cfg_attr(
    miri,
    ignore = "compresses every class twice at four levels; runs under native `cargo test`"
)]
fn the_one_shot_path_emits_what_the_streaming_path_emits() {
    for (name, payload) in small_classes() {
        for level in ONE_SHOT_LEVELS {
            let params = Params::new(Container::Zlib, level);
            if !is_chunk_invariant(payload.len(), params) {
                // Level 0 sizes its stored blocks from `avail_out`, and the two paths offer
                // different amounts of room. See `is_chunk_invariant`.
                continue;
            }
            let mut dest = vec![0_u8; compress_bound(payload.len())];
            let compressed = compress2(&mut dest, &payload, level);
            assert_eq!(compressed.code, ReturnCode::OK);
            dest.truncate(compressed.produced);
            assert_bytes_eq(
                &dest,
                &compress(&payload, params, Chunk::Whole),
                &format!("{name} level={level}: one-shot versus streaming"),
            );
        }
    }
}

// ---------------------------------------------------------------------------------------
// 3.7  Cross-container behaviour *within this implementation*
// ---------------------------------------------------------------------------------------
//
// Cross-*implementation* interoperability -- a stream produced by the C library decoded here,
// and one produced here decoded there -- needs the C oracle, so it belongs to a suite under
// `crates/zlib-rs-differential`, and it is verified there by `tests/roundtrip_interop.rs` in
// both directions. This suite must not attempt it and must not duplicate the differential
// matrix; what follows is only about this implementation's own container discrimination.

/// Adding 32 to `windowBits` is the decoder-only spelling that asks for automatic
/// `zlib`-or-`gzip` detection (`zlib.h` L893-L897).
const AUTO_DETECT_OFFSET: i32 = 32;

/// The `windowBits` value that asks for automatic detection at the window size the
/// unconditional tests use. See [`Params::core`].
const fn core_auto_detect_window_bits() -> i32 {
    core_magnitude() + AUTO_DETECT_OFFSET
}

/// `windowBits == 0`: the decoder-only spelling that takes the window size from the `zlib`
/// header (`zlib.h` L884-L886).
const HEADER_WINDOW_BITS: i32 = 0;

/// A decoder configured for one container refuses the other two.
///
/// Container discrimination has to be real rather than incidental: a raw decoder that happened
/// to skip a `zlib` header, or a `zlib` decoder that tolerated a missing one, would silently
/// accept malformed input from every caller.
#[test]
fn the_containers_are_not_interchangeable() {
    let payload = corpus::HELLO;
    let raw = compress(
        payload,
        Params::core(Container::Raw, DEF_LEVEL),
        Chunk::Whole,
    );
    let zlib = compress(
        payload,
        Params::core(Container::Zlib, DEF_LEVEL),
        Chunk::Whole,
    );
    let gzip = compress(
        payload,
        Params::core(Container::Gzip, DEF_LEVEL),
        Chunk::Whole,
    );

    let cases: [(&str, &Vec<u8>, i32); 5] = [
        ("a zlib stream in a raw decoder", &zlib, -MAX_WBITS),
        ("a gzip stream in a raw decoder", &gzip, -MAX_WBITS),
        ("a raw stream in a zlib decoder", &raw, MAX_WBITS),
        ("a gzip stream in a zlib decoder", &gzip, MAX_WBITS),
        ("a zlib stream in a gzip decoder", &zlib, MAX_WBITS + 16),
    ];
    for (label, stream, window_bits) in cases {
        let result = inflate_all(
            stream,
            window_bits,
            Chunk::Whole,
            Z_NO_FLUSH,
            payload.len() + BOUND_SLACK,
        );
        assert_eq!(
            result.code,
            ReturnCode::DATA_ERROR,
            "{label} must be rejected",
        );
        assert_ne!(
            result.output.as_slice(),
            payload,
            "{label} must not recover the payload",
        );
    }

    // A raw stream is not a header the automatic detector can find either.
    let auto = inflate_all(
        &raw,
        core_auto_detect_window_bits(),
        Chunk::Whole,
        Z_NO_FLUSH,
        payload.len() + BOUND_SLACK,
    );
    assert_eq!(
        auto.code,
        ReturnCode::DATA_ERROR,
        "automatic detection covers zlib and gzip, never raw",
    );
}

/// The automatic configuration decodes both wrapped containers.
#[test]
fn the_auto_detect_configuration_decodes_both_wrapped_containers() {
    for (name, payload) in core_classes() {
        for container in [Container::Zlib, Container::Gzip] {
            let params = Params::core(container, DEF_LEVEL);
            let stream = compress(&payload, params, Chunk::Whole);
            let recovered = decompress(
                &stream,
                core_auto_detect_window_bits(),
                Chunk::Whole,
                payload.len() + BOUND_SLACK,
            );
            assert_bytes_eq(
                &recovered,
                &payload,
                &format!("{name} / {params}: automatic detection"),
            );
        }
    }
}

/// `windowBits == 0` takes the window size from the `zlib` header.
///
/// Swept over every window size the compressor can announce, because the point of the spelling
/// is that the decoder allocates whatever the header asks for rather than a fixed 32 `KiB` --
/// so a decoder that quietly used the maximum would pass a single-size test.
#[test]
#[cfg_attr(
    miri,
    ignore = "sweeps seven window sizes; runs under native `cargo test`"
)]
fn zero_window_bits_takes_the_window_size_from_the_zlib_header() {
    let payload = corpus::text();
    for magnitude in 9..=MAX_WBITS {
        let params = Params::new(Container::Zlib, DEF_LEVEL).with_magnitude(magnitude);
        let stream = compress(&payload, params, Chunk::Whole);
        let recovered = decompress(
            &stream,
            HEADER_WINDOW_BITS,
            Chunk::Whole,
            payload.len() + BOUND_SLACK,
        );
        assert_bytes_eq(
            &recovered,
            &payload,
            &format!("{params}: decoded with windowBits = 0"),
        );
    }
}

// ---------------------------------------------------------------------------------------
// 3.8  Window-boundary behaviour
// ---------------------------------------------------------------------------------------

/// `MIN_LOOKAHEAD`, `MAX_MATCH + MIN_MATCH + 1` (`deflate.h` L296).
const MIN_LOOKAHEAD: usize = MAX_MATCH + MIN_MATCH + 1;

/// `MAX_DIST(s)`, `w_size - MIN_LOOKAHEAD` (`deflate.h` L301), at the largest window.
///
/// The furthest back a match may reach: `32768 - (258 + 3 + 1) == 32506`. The comment at
/// `deflate.h` L297-L300 explains the margin -- "we need `MAX_MATCH` bytes for the next match,
/// plus `MIN_MATCH` bytes to insert the string following the next match".
const MAX_DIST: usize = (1_usize << MAX_WBITS) - MIN_LOOKAHEAD;

/// Builds a payload whose only long match reaches back `distance` bytes.
///
/// `marker`, then filler, then either `marker` again or a different block of the same size.
/// Both regions are pseudo-random, so the *only* match worth finding is the intended one and
/// the compressed size becomes a direct measurement of whether it was found.
fn long_distance_payload(marker_len: usize, distance: usize, repeat: bool) -> Vec<u8> {
    assert!(distance > marker_len);
    let mut marker = vec![0_u8; marker_len];
    common::lcg_fill(0x5EED_0001, &mut marker);
    let mut filler = vec![0_u8; distance - marker_len];
    common::lcg_fill(0x5EED_0002, &mut filler);

    let mut tail = vec![0_u8; marker_len];
    if repeat {
        tail.copy_from_slice(&marker);
    } else {
        common::lcg_fill(0x5EED_0003, &mut tail);
    }

    let mut out = Vec::with_capacity(2 * marker_len + filler.len());
    out.extend_from_slice(&marker);
    out.extend_from_slice(&filler);
    out.extend_from_slice(&tail);
    out
}

/// The window-crossing class round-trips in every container at every level.
#[test]
#[cfg_attr(
    miri,
    ignore = "a 33 KiB payload at 11 levels in 3 containers; runs under native `cargo test`"
)]
fn the_window_crossing_class_round_trips_everywhere() {
    let payload = corpus::window_crossing();
    assert!(
        payload.len() > (1_usize << MAX_WBITS),
        "the window-crossing class must exceed the 32 KiB window",
    );
    for container in Container::ALL {
        for level in LEVELS {
            assert_round_trip(
                WINDOW_CROSSING,
                &payload,
                Params::new(container, level),
                Chunk::Whole,
            );
        }
    }
}

/// A small window slides repeatedly and still recovers the payload exactly.
#[test]
#[cfg_attr(
    miri,
    ignore = "drives a 33 KiB payload through 512-byte to 2 KiB windows; runs under native `cargo test`"
)]
fn a_small_window_slides_repeatedly_without_losing_a_byte() {
    let payload = corpus::window_crossing();
    for container in Container::ALL {
        for magnitude in 9..=11 {
            let params = Params::new(container, DEF_LEVEL).with_magnitude(magnitude);
            assert_round_trip(WINDOW_CROSSING, &payload, params, Chunk::Whole);
            assert_round_trip(WINDOW_CROSSING, &payload, params, Chunk::Bytes(8192));
        }
    }
}

/// A match reaching back nearly the whole window is found and decoded.
///
/// This is where an off-by-one in `MAX_DIST` or in the decoder's `whave` accounting shows up:
/// too strict and the match is never emitted, too lax and the decoder is handed a distance it
/// cannot satisfy. The assertion is a measurement rather than a claim -- the same payload with
/// the far block replaced by unrelated bytes must compress materially worse, which it can only
/// do if the repeat was actually exploited.
#[test]
#[cfg_attr(
    miri,
    ignore = "builds a 32 KiB payload to reach the maximum match distance; runs under native `cargo test`"
)]
fn a_match_at_nearly_the_full_window_distance_survives() {
    const MARKER: usize = 4096;
    let distance = MAX_DIST - 1;

    let with_repeat = long_distance_payload(MARKER, distance, true);
    let without_repeat = long_distance_payload(MARKER, distance, false);
    assert_eq!(with_repeat.len(), without_repeat.len());

    for container in Container::ALL {
        let params = Params::new(container, DEF_LEVEL);
        let repeated = compress(&with_repeat, params, Chunk::Whole);
        let unrepeated = compress(&without_repeat, params, Chunk::Whole);
        assert!(
            repeated.len() + MARKER / 2 < unrepeated.len(),
            "{params}: a repeat at distance {distance} must be exploited -- {} bytes with \
             it, {} without",
            repeated.len(),
            unrepeated.len(),
        );
        assert_bytes_eq(
            &decompress(
                &repeated,
                params.inflate_window_bits(),
                Chunk::Whole,
                with_repeat.len() + BOUND_SLACK,
            ),
            &with_repeat,
            &format!("{params}: maximum-distance match"),
        );
    }
}

/// Matches whose source spans the window's wrap point decode correctly.
///
/// The decoder's window is a ring: `updatewindow` writes into it modulo its size and a copy
/// whose source starts before the wrap and ends after it has to be split in two
/// (`inflate.c` L252-L290 and the `from`/`op` handling in `inffast.c`). Forcing that is a
/// matter of using a window far smaller than the payload, so a 512-byte window is asked to
/// serve a 5 `KiB` payload whose matches sit at distance 100. Because 100 is less than
/// `MAX_MATCH`, the copies also *overlap* their own source, which is the other half of the
/// same code path.
#[test]
#[cfg_attr(
    miri,
    ignore = "5 KiB through a 512-byte window; runs under native `cargo test`"
)]
fn a_match_spanning_the_window_wrap_point_survives() {
    const PATTERN: usize = 100;
    const REPEATS: usize = 50;

    let mut pattern = vec![0_u8; PATTERN];
    common::lcg_fill(0x5EED_0004, &mut pattern);
    let mut payload = Vec::with_capacity(PATTERN * REPEATS);
    for _ in 0..REPEATS {
        payload.extend_from_slice(&pattern);
    }

    for container in Container::ALL {
        for magnitude in 9..=10 {
            let params = Params::new(container, DEF_LEVEL).with_magnitude(magnitude);
            let window = 1_usize << magnitude;
            assert!(
                payload.len() > 4 * window,
                "the payload must wrap the window several times over",
            );
            assert!(
                PATTERN < window - MIN_LOOKAHEAD,
                "and the match distance must stay inside MAX_DIST for this window",
            );
            let stream = compress(&payload, params, Chunk::Whole);
            assert!(
                stream.len() < payload.len() / 4,
                "{params}: a 50-fold repeat must compress well, got {} of {} bytes",
                stream.len(),
                payload.len(),
            );
            for bytes in [1_usize, 7, 4096] {
                assert_bytes_eq(
                    &decompress(
                        &stream,
                        params.inflate_window_bits(),
                        Chunk::Bytes(bytes),
                        payload.len() + BOUND_SLACK,
                    ),
                    &payload,
                    &format!("{params} / chunk {bytes}: wrapped window copy"),
                );
            }
        }
    }
}

// ---------------------------------------------------------------------------------------
// 3.9  Large buffers and mid-stream parameter changes
// ---------------------------------------------------------------------------------------

/// `test/example.c`'s `test_large_deflate` and `test_large_inflate`, reproduced end to end.
///
/// The sizes are C's: `uncomprLen` is 20000 and `comprLen` is three times that (L499-L500). The
/// shape is C's too (L246-L332):
///
/// 1. compress 20000 mostly-zero bytes at `Z_BEST_SPEED` in one call, and check that
///    `avail_in` came back zero -- C's *"deflate not greedy"* assertion (L268-L271);
/// 2. switch to `Z_NO_COMPRESSION` with `deflateParams` and feed 10000 bytes of
///    already-compressed, therefore incompressible, data;
/// 3. switch to `Z_BEST_COMPRESSION` with `Z_FILTERED` and feed the 20000 zeros again;
/// 4. finish, which must report `Z_STREAM_END`;
/// 5. decompress in a loop that discards the output, and check the total is
///    `2 * 20000 + 10000` (L327).
///
/// **One deviation, and why.** C aims `next_in` at `compr` in step 2 -- the very buffer it is
/// writing into -- so the input and the output alias, and the exact bytes fed depend on how far
/// `next_out` has advanced into the region being read. Safe Rust cannot express that aliasing,
/// and the C test asserts nothing about the resulting bytes (only the status codes and the total
/// length), so a snapshot of the compressed prefix is fed instead. Every property the C test
/// actually checks is preserved: incompressible input, no compression, half the length, and the
/// 50000-byte total.
#[test]
#[cfg_attr(
    miri,
    ignore = "50 KB through the compressor and back: too slow to interpret, runs under native `cargo test`"
)]
fn the_example_c_large_pair_round_trips() {
    const UNCOMPR_LEN: usize = 20_000;
    const COMPR_LEN: usize = 3 * UNCOMPR_LEN;
    /// `2 * uncomprLen + uncomprLen / 2` (`test/example.c` L327), spelled out because a
    /// `usize` constant cannot widen to `u64` in a const expression without a cast.
    const EXPECTED_TOTAL: u64 = 50_000;

    // C's `uncompr` is `calloc`ed and "still mostly zeroes, so it should compress very well"
    // (L261-L263).
    let zeros = vec![0_u8; UNCOMPR_LEN];

    let params = Params::new(Container::Zlib, Z_BEST_SPEED);
    let (mut state, mut scalars) = open_encoder(params);
    let mut compr = vec![0_u8; COMPR_LEN];

    // Step 1.
    let mut next_out = {
        let mut stream = DeflateStream::new(&zeros, &mut compr);
        scalars.load_deflate(&mut stream);
        check_err(deflate(&mut state, &mut stream, Z_NO_FLUSH), "deflate");
        assert_eq!(
            stream.avail_in(),
            0,
            "deflate not greedy: one call with 20000 bytes of room to spare must take all \
             of the input",
        );
        scalars.store_deflate(&stream);
        stream.next_out
    };

    // Step 2. `deflateParams` is called with no input pending, exactly as C does at L274.
    next_out = {
        let mut stream = DeflateStream::new(&[], &mut compr);
        stream.next_out = next_out;
        scalars.load_deflate(&mut stream);
        check_err(
            deflate_params(
                &mut state,
                &mut stream,
                Z_NO_COMPRESSION,
                Z_DEFAULT_STRATEGY,
            ),
            "deflateParams(Z_NO_COMPRESSION)",
        );
        scalars.store_deflate(&stream);
        stream.next_out
    };
    let already_compressed = compr[..UNCOMPR_LEN / 2].to_vec();
    next_out = {
        let mut stream = DeflateStream::new(&already_compressed, &mut compr);
        stream.next_out = next_out;
        scalars.load_deflate(&mut stream);
        check_err(deflate(&mut state, &mut stream, Z_NO_FLUSH), "deflate");
        assert_eq!(
            stream.avail_in(),
            0,
            "the incompressible half must be taken in one call",
        );
        scalars.store_deflate(&stream);
        stream.next_out
    };

    // Step 3.
    next_out = {
        let mut stream = DeflateStream::new(&[], &mut compr);
        stream.next_out = next_out;
        scalars.load_deflate(&mut stream);
        check_err(
            deflate_params(&mut state, &mut stream, Z_BEST_COMPRESSION, Z_FILTERED),
            "deflateParams(Z_BEST_COMPRESSION, Z_FILTERED)",
        );
        scalars.store_deflate(&stream);
        stream.next_out
    };
    next_out = {
        let mut stream = DeflateStream::new(&zeros, &mut compr);
        stream.next_out = next_out;
        scalars.load_deflate(&mut stream);
        check_err(deflate(&mut state, &mut stream, Z_NO_FLUSH), "deflate");
        assert_eq!(stream.avail_in(), 0, "the zeros must be taken in one call");
        scalars.store_deflate(&stream);
        stream.next_out
    };

    // Step 4.
    let compressed_len = {
        let mut stream = DeflateStream::new(&[], &mut compr);
        stream.next_out = next_out;
        scalars.load_deflate(&mut stream);
        assert_eq!(
            deflate(&mut state, &mut stream, Z_FINISH),
            ReturnCode::STREAM_END,
            "deflate should report Z_STREAM_END",
        );
        scalars.store_deflate(&stream);
        stream.next_out
    };
    assert_eq!(deflate_end(&mut state), ReturnCode::OK);
    assert_eq!(
        scalars.total_in, EXPECTED_TOTAL,
        "three feeds of 20000, 10000 and 20000 bytes",
    );
    compr.truncate(compressed_len);

    // Step 5: `test_large_inflate`, which discards the output and checks only the total.
    let (mut decoder, mut scalars) = open_decoder(params.inflate_window_bits());
    let mut sink = vec![0_u8; UNCOMPR_LEN];
    let mut next_in = 0_usize;
    loop {
        let code = {
            // `next_out` back to the start of the sink on every call, which is C's
            // `d_stream.next_out = uncompr` at L317: the output is thrown away and the
            // decoder's own window supplies the history.
            let mut stream = InflateStream::new(&compr, &mut sink);
            stream.next_in = next_in;
            scalars.load_inflate(&mut stream);
            let code = inflate(&mut decoder, &mut stream, Z_NO_FLUSH);
            next_in = stream.next_in;
            scalars.store_inflate(&stream);
            code
        };
        if code == ReturnCode::STREAM_END {
            break;
        }
        check_err(code, "large inflate");
    }
    assert_eq!(inflate_end(&mut decoder), ReturnCode::OK);
    assert_eq!(
        scalars.total_out, EXPECTED_TOTAL,
        "bad large inflate: the total must match everything that was fed",
    );
}

// ---------------------------------------------------------------------------------------
// Golden streams from the reference implementation
// ---------------------------------------------------------------------------------------
//
// Transcribed from the in-tree C sources -- `adler32.c`, `crc32.c`, `deflate.c`, `inflate.c`,
// `inffast.c`, `inftrees.c`, `trees.c`, `zutil.c`, `compress.c` and `uncompr.c` compiled
// together, version 1.3.2.1-motley -- by compressing each fixture with `deflateInit2` and one
// `deflate(Z_FINISH)` call.
//
// These are a deliberately small committed sample, not a second differential matrix: the full
// level x windowBits x memLevel x strategy x flush sweep against the C oracle belongs to a suite
// under `crates/zlib-rs-differential`, which can link the oracle and compare on the fly, and no
// such suite is in the tree. What they buy here is twofold. They pin the emitted bytes without a
// C compiler,
// so a change in match selection or Huffman tie-breaking fails in this crate's own test run;
// and because nothing in this file names a feature, they are the end-to-end proof that the
// optional `simd` feature is output-neutral -- the same constants must match under
// `--no-default-features`, `--features simd` and `--all-features` alike, so a vectorised
// checksum that perturbed the bitstream could not pass.

/// `hello` in a `zlib` container at level 6.
const GOLDEN_HELLO_ZLIB_6: &[u8] = &[
    0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c, 0x00, 0x26,
    0x06, 0x04, 0x96,
];

/// `hello` in a `zlib` container at level 1. Only `FLEVEL` differs from level 6.
const GOLDEN_HELLO_ZLIB_1: &[u8] = &[
    0x78, 0x01, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c, 0x00, 0x26,
    0x06, 0x04, 0x96,
];

/// `hello` in a `zlib` container at level 9.
const GOLDEN_HELLO_ZLIB_9: &[u8] = &[
    0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c, 0x00, 0x26,
    0x06, 0x04, 0x96,
];

/// `hello` in a `zlib` container at level 0: one stored block, so the payload appears verbatim.
const GOLDEN_HELLO_ZLIB_0: &[u8] = &[
    0x78, 0x01, 0x01, 0x0e, 0x00, 0xf1, 0xff, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x2c, 0x20, 0x68, 0x65,
    0x6c, 0x6c, 0x6f, 0x21, 0x00, 0x26, 0x06, 0x04, 0x96,
];

/// `hello` as raw deflate at level 6: the payload region of [`GOLDEN_HELLO_ZLIB_6`], alone.
const GOLDEN_HELLO_RAW_6: &[u8] = &[
    0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c, 0x00,
];

/// `hello` in a `gzip` container at level 6. Byte 9 is `OS_CODE` and is target-dependent.
const GOLDEN_HELLO_GZIP_6: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7,
    0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c, 0x00, 0x9d, 0x3f, 0x6c, 0xb5, 0x0e, 0x00, 0x00, 0x00,
];

/// `hello` at level 6 with `Z_FILTERED`, which declines the distant match.
const GOLDEN_HELLO_ZLIB_6_FILTERED: &[u8] = &[
    0x78, 0x9c, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0x64,
    0x00, 0x00, 0x26, 0x06, 0x04, 0x96,
];

/// `hello` at level 6 with `Z_HUFFMAN_ONLY`, which emits no matches at all.
const GOLDEN_HELLO_ZLIB_6_HUFFMAN: &[u8] = &[
    0x78, 0x01, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0x64,
    0x00, 0x00, 0x26, 0x06, 0x04, 0x96,
];

/// `hello` at level 6 with `Z_RLE`, which allows only distance-1 matches.
const GOLDEN_HELLO_ZLIB_6_RLE: &[u8] = &[
    0x78, 0x01, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x48, 0xcd, 0xc9, 0xc9, 0x57, 0x64,
    0x00, 0x00, 0x26, 0x06, 0x04, 0x96,
];

/// `hello` at level 6 with `Z_FIXED`, which forbids dynamic Huffman trees.
const GOLDEN_HELLO_ZLIB_6_FIXED: &[u8] = &[
    0x78, 0x01, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c, 0x00, 0x26,
    0x06, 0x04, 0x96,
];

/// The empty payload in a `zlib` container: the shortest complete stream, eight bytes.
const GOLDEN_EMPTY_ZLIB_6: &[u8] = &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];

/// The empty payload as raw deflate: a single empty fixed block, two bytes.
const GOLDEN_EMPTY_RAW_6: &[u8] = &[0x03, 0x00];

/// The empty payload in a `gzip` container: header, empty block, and a zero `CRC-32`/`ISIZE`.
const GOLDEN_EMPTY_GZIP_6: &[u8] = &[
    0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00,
    0x00, 0x00, 0x00, 0x00,
];

/// A single `a` in a `zlib` container: the degenerate one-symbol tree.
const GOLDEN_SINGLE_BYTE_ZLIB_6: &[u8] = &[0x78, 0x9c, 0x4b, 0x04, 0x00, 0x00, 0x62, 0x00, 0x62];

/// `hello` compressed against the `hello` dictionary at level 9.
///
/// Twenty bytes: the header word `0x78f9` with `FDICT` set, the dictionary's `Adler-32`
/// `0x08410215`, the deflate data, and the payload's `Adler-32`.
const GOLDEN_HELLO_DICT_ZLIB_9: &[u8] = &[
    0x78, 0xf9, 0x08, 0x41, 0x02, 0x15, 0xcb, 0x00, 0x91, 0x3a, 0x0a, 0x60, 0x4a, 0x91, 0x01, 0x00,
    0x26, 0x06, 0x04, 0x96,
];

/// The `Adler-32` of the six-byte `hello` dictionary, as the reference reports it.
const GOLDEN_DICT_ID: u32 = 0x0841_0215;

/// The index of the `OS` byte in a `gzip` header (`doc/rfc1952.txt` L166).
const GZIP_OS_INDEX: usize = 9;

/// Compares a `gzip` stream against a golden one, skipping the target-dependent `OS` byte.
///
/// `OS_CODE` is chosen by the platform (`zutil.h` L177-L212) -- 3 for Unix, 0 for the DOS
/// family, 11 for Windows -- so it is the one header byte a portable golden cannot pin. Every
/// other byte, header and trailer alike, is compared.
fn assert_gzip_golden(actual: &[u8], golden: &[u8], label: &str) {
    assert_eq!(
        actual.len(),
        golden.len(),
        "{label}: stream length ({} versus {})",
        actual.len(),
        golden.len(),
    );
    assert_bytes_eq(
        &actual[..GZIP_OS_INDEX],
        &golden[..GZIP_OS_INDEX],
        &format!("{label}: header before OS"),
    );
    assert_bytes_eq(
        &actual[GZIP_OS_INDEX + 1..],
        &golden[GZIP_OS_INDEX + 1..],
        &format!("{label}: everything after OS"),
    );
}

/// Every golden stream is reproduced byte for byte.
///
/// A failure here means the encoder made a different decision from the reference: a different
/// match, a different lazy-match rejection, a different Huffman tie-break or a different block
/// type. None of those would show up in a round trip, which is the whole reason this test
/// exists.
#[test]
#[cfg_attr(
    miri,
    ignore = "pinned to the reference buffer shape, ~300 KiB of interpreted stores per \
     stream; the bytes are checked natively under every feature combination"
)]
fn the_golden_streams_are_reproduced_byte_for_byte() {
    let zlib = |level: i32| Params::new(Container::Zlib, level);
    let cases: [(&str, Params, &[u8], &[u8]); 12] = [
        ("hello zlib 6", zlib(6), corpus::HELLO, GOLDEN_HELLO_ZLIB_6),
        ("hello zlib 1", zlib(1), corpus::HELLO, GOLDEN_HELLO_ZLIB_1),
        ("hello zlib 9", zlib(9), corpus::HELLO, GOLDEN_HELLO_ZLIB_9),
        ("hello zlib 0", zlib(0), corpus::HELLO, GOLDEN_HELLO_ZLIB_0),
        (
            "hello raw 6",
            Params::new(Container::Raw, 6),
            corpus::HELLO,
            GOLDEN_HELLO_RAW_6,
        ),
        (
            "hello zlib 6 filtered",
            zlib(6).with_strategy(Z_FILTERED),
            corpus::HELLO,
            GOLDEN_HELLO_ZLIB_6_FILTERED,
        ),
        (
            "hello zlib 6 huffman-only",
            zlib(6).with_strategy(Z_HUFFMAN_ONLY),
            corpus::HELLO,
            GOLDEN_HELLO_ZLIB_6_HUFFMAN,
        ),
        (
            "hello zlib 6 rle",
            zlib(6).with_strategy(Z_RLE),
            corpus::HELLO,
            GOLDEN_HELLO_ZLIB_6_RLE,
        ),
        (
            "hello zlib 6 fixed",
            zlib(6).with_strategy(Z_FIXED),
            corpus::HELLO,
            GOLDEN_HELLO_ZLIB_6_FIXED,
        ),
        ("empty zlib 6", zlib(6), corpus::EMPTY, GOLDEN_EMPTY_ZLIB_6),
        (
            "empty raw 6",
            Params::new(Container::Raw, 6),
            corpus::EMPTY,
            GOLDEN_EMPTY_RAW_6,
        ),
        (
            "single byte zlib 6",
            zlib(6),
            corpus::SINGLE_BYTE,
            GOLDEN_SINGLE_BYTE_ZLIB_6,
        ),
    ];

    for (label, params, payload, golden) in cases {
        // Both feeding styles, because the golden bytes are the reference's whatever the call
        // pattern -- which is the point of `no_chunk_size_changes_the_compressed_bytes`,
        // restated here against an external witness rather than against ourselves.
        assert_bytes_eq(&compress(payload, params, Chunk::Whole), golden, label);
        if is_chunk_invariant(payload.len(), params) {
            assert_bytes_eq(
                &compress(payload, params, Chunk::Bytes(1)),
                golden,
                &format!("{label} (one byte at a time)"),
            );
        }
        // And the golden bytes really are this payload.
        assert_bytes_eq(
            &decompress(
                golden,
                params.inflate_window_bits(),
                Chunk::Whole,
                payload.len() + BOUND_SLACK,
            ),
            payload,
            &format!("{label}: the golden stream decodes to the fixture"),
        );
    }

    // The `gzip` goldens, whose `OS` byte is the platform's.
    let gzip = Params::new(Container::Gzip, 6);
    assert_gzip_golden(
        &compress(corpus::HELLO, gzip, Chunk::Whole),
        GOLDEN_HELLO_GZIP_6,
        "hello gzip 6",
    );
    assert_gzip_golden(
        &compress(corpus::EMPTY, gzip, Chunk::Whole),
        GOLDEN_EMPTY_GZIP_6,
        "empty gzip 6",
    );

    // The dictionary golden, including the identity the reference publishes.
    let dictionary = compress_with_dictionary(
        corpus::HELLO,
        corpus::DICTIONARY,
        Params::new(Container::Zlib, Z_BEST_COMPRESSION),
    );
    assert_eq!(
        dictionary.dict_id, GOLDEN_DICT_ID,
        "the dictionary's Adler-32 must match the reference",
    );
    assert_bytes_eq(
        &dictionary.bytes,
        GOLDEN_HELLO_DICT_ZLIB_9,
        "hello with dictionary, zlib 9",
    );
}

/// The two checksums the containers carry agree with the streams they are attached to.
///
/// A container-level cross-check that costs almost nothing: the `zlib` trailer must be the
/// payload's `Adler-32` and the `gzip` trailer its `CRC-32`, so computing both directly and
/// comparing them against what the compressor emitted ties the checksum modules to the
/// container modules. The `zlib` values are additionally pinned by the golden streams above.
#[test]
fn the_container_checksums_agree_with_the_payload() {
    for (name, payload) in core_classes() {
        let expected_adler = adler32(ADLER32_INITIAL_VALUE, &payload);
        let expected_crc = crc32(0, &payload);

        let zlib = compress(
            &payload,
            Params::core(Container::Zlib, DEF_LEVEL),
            Chunk::Whole,
        );
        let tail = &zlib[zlib.len() - 4..];
        assert_eq!(
            u32::from_be_bytes([tail[0], tail[1], tail[2], tail[3]]),
            expected_adler,
            "{name}: the zlib trailer",
        );

        let gzip = compress(
            &payload,
            Params::core(Container::Gzip, DEF_LEVEL),
            Chunk::Whole,
        );
        let tail = &gzip[gzip.len() - 8..];
        assert_eq!(
            u32::from_le_bytes([tail[0], tail[1], tail[2], tail[3]]),
            expected_crc,
            "{name}: the gzip trailer",
        );

        // The decoder ends with the same value it verified against.
        let params = Params::core(Container::Zlib, DEF_LEVEL);
        let result = inflate_all(
            &zlib,
            params.inflate_window_bits(),
            Chunk::Whole,
            Z_NO_FLUSH,
            payload.len() + BOUND_SLACK,
        );
        assert_eq!(result.code, ReturnCode::STREAM_END, "{name}");
        assert_eq!(
            result.adler, expected_adler,
            "{name}: the decoder must end with the check value it verified",
        );
    }
}
