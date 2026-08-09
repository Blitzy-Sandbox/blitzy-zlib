//! The DEFLATE decompressor: `inflate()` and its eighteen public entry points.
//!
//! This module is the safe-Rust mirror of `inflate.c` (1413 lines), the driver that
//! turns a zlib, gzip or raw DEFLATE byte stream back into the data it was made
//! from. It owns the resumable state machine, the block and trailer states, and
//! every function `zlib.h` declares with an `inflate` prefix.
//!
//! # What lives where
//!
//! The C decompressor is four translation units sharing four private headers:
//! `inflate.c`, `inffast.c`, `inftrees.c` and `infback.c` all include the identical
//! set `zutil.h`, `inftrees.h`, `inflate.h`, `inffast.h`. That shared include set is
//! why `inftrees` and `inffast` are **folded into this module tree** rather than
//! being crate-level siblings (AAP §0.4.1.2): they are not independent units, they
//! are parts of the decoder that happen to have been split across files for
//! compilation reasons that Rust does not have.
//!
//! | Submodule | C source | Role |
//! |---|---|---|
//! | [`mode`] | `inflate.h` L20-L53 | the 32-state `inflate_mode` enum |
//! | [`state`] | `inflate.h` L82-L126 | `struct inflate_state`, the bit accumulator, the window |
//! | `window` | `inflate.c` L252-L296 | `updatewindow` |
//! | [`header`] | `inflate.c` L506-L706 | the zlib, gzip and dictionary header states |
//! | [`inftrees`] | `inftrees.c` | `inflate_table`, `inflate_fixed`, [`inftrees::Code`] |
//! | `inffast` | `inffast.c` | `inflate_fast`, the unrolled decode loop |
//! | [`fixed_tables`] | `inffixed.h` | the RFC 1951 §3.2.6 fixed decode tables |
//! | this file | `inflate.c` | everything else |
//!
//! `inflateBack()` stays at crate level, in `crate::infback`, only because its
//! public entry points are distinct; it reuses [`InflateState`], [`Mode`],
//! `inflate_table`, `inflate_fixed` and `inflate_fast` from here.
//!
//! # The resumable-state-machine contract
//!
//! The substance of `inflate.c` L392-L472, which is the reference's own and best
//! description of how this works, restated here:
//!
//! `inflate()` processes as much input and generates as much output as it can
//! before returning. Its state machine is one `loop` over one exhaustive `match`
//! on [`Mode`], and every arm either returns without changing the mode -- because
//! there is not enough input or output space to make progress -- or makes
//! progress and assigns the next mode. So when `inflate()` is called again the
//! same state is attempted again, and if the
//! appropriate resources are provided the machine proceeds to the next state. The
//! `NEEDBITS(n)` step -- `InflateState::need_bits` here -- is usually how a state
//! decides whether it can proceed or must return. The typical use of the bit macros
//! is
//!
//! ```text
//! NEEDBITS(n);  ... do something with BITS(n) ...  DROPBITS(n);
//! ```
//!
//! where `NEEDBITS(n)` either returns from `inflate()` if there is not enough input
//! left to load `n` bits into the accumulator, or continues; `BITS(n)` gives the low
//! `n` bits in the accumulator; `DROPBITS(n)` drops them; `INITBITS()` clears the
//! accumulator; and `BYTEBITS()` discards just enough bits to put the accumulator on
//! a byte boundary. Decoding a variable-length code uses `PULLBYTE()` directly, in
//! order to pull just enough bytes to decode the next code and no more.
//!
//! Some states loop until they get enough input, keeping enough state to continue
//! the loop where it left off if `NEEDBITS()` returns -- which is why `have`, `nlen`,
//! `ndist`, `length` and `extra` are fields of [`InflateState`] and not locals.
//!
//! A state may also return because there is not enough **output** space to complete
//! it. Those states are copying stored data ([`Mode::Copy`]), writing a literal byte
//! ([`Mode::Lit`]) and copying a matching string ([`Mode::Match`]).
//!
//! When returning, C's `goto inf_leave` updates the total counters, updates the check
//! value, and decides whether any progress was made during the call in order to pick
//! the right return code. Progress is defined as a change in either `avail_in` or
//! `avail_out`. When there is a window, the epilogue updates it with the last output
//! written; if the epilogue is reached mid-stream and there is no window yet, it
//! creates one and copies output into it for the next call.
//!
//! Finally: the `flush` parameter of `inflate()` only affects the **return code**.
//! `inflate()` always writes as much as possible to the output, given the space and
//! the input available -- the effect `zlib.h` documents for `Z_SYNC_FLUSH` -- and it
//! always defers allocating and filling the sliding window until necessary, which is
//! the effect `zlib.h` documents for `Z_FINISH` when the entire input is available.
//! So the only thing `flush` actually does is: when it is `Z_FINISH`, `inflate()`
//! cannot return `Z_OK`; it returns `Z_BUF_ERROR` instead if it has not reached the
//! end of the stream. The exceptions are `Z_BLOCK` and `Z_TREES`, which additionally
//! stop the machine at a block boundary and after a block header respectively.
//!
//! # `LOAD()` and `RESTORE()` are no-ops here
//!
//! C copies six values into registers on entry (`inflate.c` L328-L336) and writes
//! them back on the way out (L339-L347): the output cursor `put`, the output space
//! `left`, the input cursor `next`, the input count `have`, and the accumulator pair
//! `hold`/`bits`. This implementation has neither macro, because it has nowhere to copy from
//! and to:
//!
//! * `hold` and `bits` are fields of [`InflateState`], and every accumulator step is
//!   a method on it, so they are never shadowed by a local.
//! * the two cursors are `usize` fields of the private driver context, mutated in
//!   place, and written back to [`InflateStream`] once, when `inflate()` returns.
//! * `have` and `left` are derived (`avail_in`, `avail_out`) rather than stored, so
//!   they cannot drift out of step with the cursors.
//!
//! The one place the difference could show is C's three **bare** returns, which skip
//! `RESTORE()` as well as the epilogue: `MEM` (L1117), `SYNC`/`default` (L1119-L1122)
//! and the `Z_NEED_DICT` exit, which calls `RESTORE()` explicitly precisely because
//! it must not lose the header bytes it consumed (L702-L703). Reaching the `MEM` or
//! `SYNC` arm consumes nothing at all -- both are the first thing the dispatcher sees
//! and both return immediately -- so writing the cursors back unconditionally is
//! observationally identical to C, and the `Z_NEED_DICT` exit needs exactly that
//! write-back anyway. See `Step::Return`.
//!
//! # Visibility
//!
//! `zlib.map` puts `inflate_table` and `inflate_fast` in the `local:` block of
//! `ZLIB_1.2.0` and `inflate_fixed` in the `local:` block of `ZLIB_1.3.2`; C
//! additionally declares `updatewindow`, `syncsearch` and `inflateStateCheck`
//! `local`, i.e. file-static, so they appear in no header at all. Every one of them
//! is therefore `pub(crate)` in this port, and none may ever be `#[no_mangle]`. The
//! `inffast` and `window` submodules are themselves crate-private for the same
//! reason: they contain nothing the outside world is allowed to name.
//!
//! [`inflate_table`] is the single
//! deliberate exception, and it is an exception to the *Rust* visibility rule only.
//! It is `#[doc(hidden)] pub` because the unmodified `test/infcover.c` calls the C
//! symbol directly and links against `libz.a`, so `crates/libz-rs-sys` has to wrap
//! it -- which it cannot do without being able to call it. The wrapper is the
//! `#[no_mangle]` item; this crate's symbol stays mangled, and the version script
//! keeps the C name out of the `.so`. The
//! [`inftrees`] module documentation carries the full
//! contract.
//!
//! Nothing in this module is feature-gated. In particular the gzip states are not
//! optional: `inflate.h` L11-L17 defines `GUNZIP` unless `NO_GZIP` is defined, and
//! the shipped build does not define it. This crate has no `gz` feature either --
//! that one belongs to the facade, and gates the `gzFile` layer, not gzip decoding.
//!
//! # Safety and robustness
//!
//! This module decodes attacker-controlled bytes, so:
//!
//! * There is no `unsafe` here; the crate root forbids it, so nothing in this module
//!   dereferences a raw pointer or builds a slice from one.
//! * There is no `unwrap()`, `expect()`, panicking index or overflowing arithmetic in
//!   any path reachable from a stream. Malformed input reaches [`Mode::Bad`] and
//!   becomes [`ReturnCode::DATA_ERROR`] with the same message C produces, at the same
//!   decision point.
//! * A handful of checks guard conditions C leaves as invariants -- an out-of-range
//!   decode-table index, a window byte outside the valid history. Each is annotated
//!   with the argument for why no stream can reach it, and each fails closed to a
//!   data error rather than to a panic.
//! * Every state either changes `state.mode`, consumes input, produces output, or
//!   returns, so the dispatch loop cannot spin.
//!
//! [`fixed_tables`]: crate::inflate::fixed_tables
//! [`header`]: crate::inflate::header
//! [`inftrees`]: crate::inflate::inftrees
//! [`inftrees::Code`]: crate::inflate::inftrees::Code
//! [`mode`]: crate::inflate::mode
//! [`state`]: crate::inflate::state

// The definitions closing the module documentation above are link-reference definitions, not
// prose: a module whose `mod` declaration carries an outer doc comment has its `//!` block
// resolved in the scope of the DECLARING module, so an unqualified sibling name does not
// resolve. See "Documentation lints" in `src/lib.rs` for the rule and the gate.

// Items in this module carry the names of the C functions spelled `inflate*`, and the
// they live in is `inflate`, so `clippy::module_name_repetitions` fires on nearly all
// of them. The C names are the API contract -- `crates/libz-rs-sys` exports them
// verbatim -- so they are kept and the lint is relaxed for this file only, exactly as
// `state.rs`, `header.rs` and `config.rs` do.
#![allow(clippy::module_name_repetitions)]

pub mod fixed_tables;
pub mod header;
pub mod inftrees;
pub mod mode;
pub mod state;

// Crate-private, mirroring the `local:` blocks of zlib.map: these two submodules
// export nothing a consumer of the library is permitted to name.
pub(crate) mod inffast;
pub(crate) mod window;

use crate::adler32::{adler32, ADLER32_INITIAL_VALUE};
use crate::allocate::Allocator;
use crate::config::{InflateConfig, DEF_WBITS, Z_BLOCK, Z_FINISH, Z_TREES};
use crate::crc32::crc32;
use crate::error::ReturnCode;
use crate::inflate::header::{HeaderAction, HeaderExit};
use crate::inflate::inffast::{
    inflate_fast, MIN_AVAIL_IN, MIN_AVAIL_OUT, MSG_INVALID_DISTANCE_CODE,
    MSG_INVALID_DISTANCE_TOO_FAR_BACK, MSG_INVALID_LITERAL_LENGTH_CODE,
};
use crate::inflate::inftrees::{
    inflate_fixed, inflate_table, Code, CodeTableSource, CodeType, TABLE_OK,
};
use crate::inflate::state::{is_live_mode_tag, StreamReset, BACK_UNKNOWN, FLAGS_NO_HEADER};
use crate::inflate::window::update_window;
use crate::read_buf::{OutputCursor, OutputRegion};

pub use crate::inflate::header::inflate_get_header;
pub use crate::inflate::mode::Mode;
pub use crate::inflate::state::InflateState;

// The two fixed decode tables keep their C spelling, because those are the names
// `inffixed.h` generates and the names the tests in `fixed_tables` diff against. The
// `#[allow(non_upper_case_globals)]` that makes that legal lives on the definitions
// themselves, in `fixed_tables`.
pub use crate::inflate::fixed_tables::{distfix, lenfix};

/// Permutation of the code-length code lengths, RFC 1951 §3.2.7.
///
/// Transcribed verbatim from `inflate.c` L491-L492. The nineteen code-length codes
/// are transmitted in this order rather than in symbol order, because the middle of
/// the alphabet is the part most likely to be unused and a run of trailing zeros can
/// then be omitted by lowering `HCLEN`.
const ORDER: [u16; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

/// Root index width of the code-length code table (`inflate.c` L812).
const CODES_ROOT_BITS: usize = 7;

/// Root index width of the literal/length code table (`inflate.c` L890).
///
/// ★ Do not change this or [`DISTS_ROOT_BITS`]: `inftrees.h` L38-L48 derives
/// `ENOUGH_LENS` and `ENOUGH_DISTS` -- and therefore the size of the arena
/// `test/infcover.c` L428 budgets for -- from exactly these two values.
const LENS_ROOT_BITS: usize = 9;

/// Root index width of the distance code table (`inflate.c` L899).
const DISTS_ROOT_BITS: usize = 6;

/// Number of code-length codes, RFC 1951 §3.2.7 (`inflate.c` L808 and L813).
const CODE_LENGTH_CODES: usize = 19;

/// Largest `HLIT`-derived literal/length alphabet size (`inflate.c` L791).
const MAX_LITERAL_LENGTH_CODES: u32 = 286;

/// Largest `HDIST`-derived distance alphabet size (`inflate.c` L791).
const MAX_DISTANCE_CODES: u32 = 30;

/// Symbol index of the end-of-block code, RFC 1951 §3.2.3 (`inflate.c` L878).
const END_OF_BLOCK_SYMBOL: usize = 256;

/// `op & 32` -- the end-of-block marker in a decode-table entry (`inflate.c` L950).
const OP_END_OF_BLOCK: u8 = 32;

/// `op & 64` -- the invalid-code marker in a decode-table entry (`inflate.c` L956).
const OP_INVALID_CODE: u8 = 64;

/// `op & 15` -- the extra-bit count in a decode-table entry (`inflate.c` L961).
const OP_EXTRA_BITS: u8 = 15;

/// `op & 0xf0` -- the mask that distinguishes a second-level table link from an
/// operation (`inflate.c` L929 and L981).
const OP_KIND_MASK: u8 = 0xf0;

// Each of these lands in the caller-visible `z_stream.msg` and is printed verbatim
// by `test/example.c` and `test/infcover.c`, so the text, the punctuation and the
// decision point are all part of the contract. The three messages that `inffast.c`
// also produces are imported from `inffast` rather than restated, so the fast and
// slow decode paths cannot drift apart.

/// `inflate.c` L742: the two block-type bits were `11`, which RFC 1951 §3.2.3
/// reserves.
pub(crate) const MSG_INVALID_BLOCK_TYPE: &str = "invalid block type";

/// `inflate.c` L751: a stored block's `LEN` and `NLEN` are not complements.
pub(crate) const MSG_INVALID_STORED_BLOCK_LENGTHS: &str = "invalid stored block lengths";

/// `inflate.c` L792-L793: `HLIT` or `HDIST` named more symbols than RFC 1951 §3.2.5
/// defines. Active in this build because `PKZIP_BUG_WORKAROUND` is not defined.
pub(crate) const MSG_TOO_MANY_SYMBOLS: &str = "too many length or distance symbols";

/// `inflate.c` L816: the code-length code itself is over- or under-subscribed.
pub(crate) const MSG_INVALID_CODE_LENGTHS_SET: &str = "invalid code lengths set";

/// `inflate.c` L840-L841 and L864-L865: a repeat code either had nothing to repeat
/// or ran past the end of the two alphabets. Both sites emit this same text.
pub(crate) const MSG_INVALID_BIT_LENGTH_REPEAT: &str = "invalid bit length repeat";

/// `inflate.c` L879-L880: symbol 256 was given a zero code length, so the block has
/// no way to end. Note the two hyphens; the text is reproduced exactly.
pub(crate) const MSG_MISSING_END_OF_BLOCK: &str = "invalid code -- missing end-of-block";

/// `inflate.c` L894: the literal/length code is over- or under-subscribed.
pub(crate) const MSG_INVALID_LITERAL_LENGTHS_SET: &str = "invalid literal/lengths set";

/// `inflate.c` L903: the distance code is over- or under-subscribed.
pub(crate) const MSG_INVALID_DISTANCES_SET: &str = "invalid distances set";

/// `inflate.c` L1087: the trailing Adler-32 or CRC-32 does not match the data.
pub(crate) const MSG_INCORRECT_DATA_CHECK: &str = "incorrect data check";

/// `inflate.c` L1101: a gzip member's `ISIZE` does not match the bytes produced.
pub(crate) const MSG_INCORRECT_LENGTH_CHECK: &str = "incorrect length check";

/// What `inflateMark` returns for a stream that fails `inflateStateCheck`.
///
/// `inflate.c` L1400-L1401 returns `-(1L << 16)`, i.e. "no bits back, and no match in
/// progress" pushed one whole unit negative so that it cannot be confused with any
/// real answer. Only the facade can detect that condition -- it owns the pointer
/// checks -- so the value it must return lives here as a constant.
pub const INFLATE_MARK_BAD_STATE: i64 = -(1 << 16);

/// What `inflateCodesUsed` returns for a stream that fails `inflateStateCheck`.
///
/// `inflate.c` L1410 returns `(unsigned long)-1`, which is all ones in whatever width
/// `unsigned long` has. Mirrors `inflate.c` L1408-L1412.
pub const INFLATE_CODES_USED_BAD_STATE: u64 = u64::MAX;

// C truncates freely between `unsigned`, `unsigned long` and pointers. Every such
// narrowing in this file goes through one of the four helpers below, so that no cast
// can panic in a debug build and each one has a single place to justify its
// saturating fallback. The fallbacks are unreachable on every 32- or 64-bit target:
// no value in the decoder exceeds 2**32, and `usize` is at least 32 bits wide.

/// A byte count or slice index, from the 64-bit accumulator domain.
#[inline]
fn to_index(value: u64) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// A bit or byte count, from the `usize` domain.
#[inline]
fn to_count(value: usize) -> u32 {
    u32::try_from(value).unwrap_or(u32::MAX)
}

/// A total or length, widened from the `usize` domain.
#[inline]
fn to_wide(value: usize) -> u64 {
    u64::try_from(value).unwrap_or(u64::MAX)
}

/// C's `(unsigned)` narrowing of an accumulator value.
#[inline]
fn low_u32(value: u64) -> u32 {
    u32::try_from(value & u64::from(u32::MAX)).unwrap_or(u32::MAX)
}

/// C's `(unsigned short)` narrowing, used where a code length is stored in `lens`.
#[inline]
fn low_u16(value: u64) -> u16 {
    u16::try_from(value & u64::from(u16::MAX)).unwrap_or(u16::MAX)
}

/// C's `(unsigned char)` narrowing, used where a literal is written to the output.
#[inline]
fn low_u8(value: u64) -> u8 {
    u8::try_from(value & u64::from(u8::MAX)).unwrap_or(u8::MAX)
}

/// `BITS(n)`: the low `count` bits of the accumulator (`inflate.c` L373-L376).
///
/// Unlike [`InflateState::low_bits`], this does **not** require the bits to be
/// present. That difference is load-bearing: a decode table is indexed with
/// `hold & ((1 << root) - 1)` *before* it is known whether `root` bits are available,
/// because the entry found is what says how many bits the code actually needs
/// (`inflate.c` L827 and L925). The accumulator's unfilled high bits are zero, so a
/// short read simply selects a low entry and the loop pulls another byte.
///
/// `count` at or above 64 yields the whole accumulator, which is the limit of C's
/// `(1U << n) - 1` mask as `n` grows; no reachable call site passes more than 15.
#[inline]
fn low_bits(hold: u64, count: u32) -> u64 {
    match 1_u64.checked_shl(count) {
        Some(one) => hold & one.wrapping_sub(1),
        None => hold,
    }
}

/// `DROPBITS(n)`: remove `count` bits from the accumulator (`inflate.c` L378-L382).
///
/// Every call site is preceded by a `NEEDBITS` or by a decode-table entry that named
/// the width, so the accumulator always holds at least `count` bits and the checked
/// form in [`InflateState::drop_bits`] cannot fail. Discarding its result therefore
/// loses no information; were it ever to fail, the accumulator would be left exactly
/// as it was and the stream would go on to report invalid data rather than misdecode.
#[inline]
fn drop_bits<'a, A: Allocator<'a>>(state: &mut InflateState<'a, A>, count: u32) {
    let _dropped = state.drop_bits(count);
}

/// The parts of `z_stream` that [`inflate`] reads and writes, in safe form.
///
/// C reaches all of this through one `z_streamp` (`zlib.h` L90-L110). This crate has
/// no `z_stream`, and it never dereferences a raw pointer or builds a slice from one:
/// the facade crate does that once, rebuilding the two pointer/length pairs as slices,
/// copying the six scalars in, calling an entry point here, and copying the scalars
/// back out.
///
/// # The two buffers
///
/// `input` and `output` are the **whole** buffers, and the cursors say how far into
/// each one the stream has got, so that
///
/// * `avail_in` is `input.len() - next_in`, and
/// * `avail_out` is `output.len() - next_out`.
///
/// This is the convention [`header`] and `inffast` already use, and it is what lets
/// a match copy read bytes this call has itself written. C's `next_in == Z_NULL` with
/// `avail_in == 0` is legal (`inflate.c` L495) and corresponds to an empty `input`
/// slice, which is likewise not an error here; a null `next_in` with a non-zero
/// `avail_in`, and a null `next_out`, are errors the facade rejects before it can
/// build this value at all.
///
/// # The scalars
///
/// `total_in` and `total_out` are `u64` because C declares them `uLong`, which is
/// 64 bits wide on every LP64 target; the facade converts to and from `c_ulong`, and
/// on an LLP64 target that conversion truncates in exactly the places C's own 32-bit
/// `unsigned long` wraps. `msg` is an `Option<&'static str>` rather than a `*mut
/// c_char` because every message the decoder can produce is a string literal, so the
/// facade can publish one as a `const char *` with no allocation.
#[derive(Debug)]
pub struct InflateStream<'a> {
    /// The whole input buffer: `z_stream.next_in` addressed its `next_in`-th byte.
    pub input: &'a [u8],
    /// How far into [`Self::input`] the stream has read. C's `next_in` advance.
    pub next_in: usize,
    /// The whole output buffer: `z_stream.next_out` addressed its `next_out`-th byte.
    ///
    /// An [`OutputRegion`] rather than a `&mut [u8]` because a C caller's `next_out` is only
    /// guaranteed *writable*: `zlib.h` L94-L95 promises `avail_out` bytes of room and says
    /// nothing about their contents, and Rust does not allow a byte slice to address a byte
    /// that holds no value. [`OutputRegion::init`] is the shape a Rust caller has and is what
    /// [`InflateStream::new`] builds; the facade supplies [`OutputRegion::write_only`] through
    /// [`InflateStream::with_region`]. The decoder reads its own output back -- for the check
    /// value (L1080), the window update (L1136) and a match copy (L1055) -- and the region is
    /// what makes those three reads legal over either shape.
    pub output: OutputRegion<'a>,
    /// How far into [`Self::output`] the stream has written. C's `next_out` advance.
    pub next_out: usize,
    /// `z_stream.total_in`: total bytes of input consumed so far (`zlib.h` L93).
    pub total_in: u64,
    /// `z_stream.total_out`: total bytes of output produced so far (`zlib.h` L96).
    pub total_out: u64,
    /// `z_stream.msg`: the last error message, or [`None`] for C's `Z_NULL`
    /// (`zlib.h` L98).
    pub msg: Option<&'static str>,
    /// `z_stream.adler`: the running or final check value (`zlib.h` L106), or
    /// [`None`] when no check value has been assigned.
    ///
    /// ★ **This is the one `z_stream` member the decoder writes only sometimes, so
    /// it is the one member an [`Option`] has to model.** `inflateResetKeep` assigns
    /// `strm->adler` under `if (state->wrap)` (`inflate.c` L108-L109) and the
    /// epilogue under `if ((state->wrap & 4) && out)` (L1144-L1146); a **raw** stream
    /// satisfies neither, so C never touches the member at all and whatever the
    /// caller left there survives. [`None`] is that "left alone" state, and it is why
    /// the facade need not -- and must not -- read the caller's member to build this
    /// view: a freshly initialised raw stream's `adler` is *indeterminate*, and
    /// reading it would be undefined behaviour.
    ///
    /// The decoder never consults the incoming value either way. It keeps the running
    /// check in the state's own `check` field and publishes here, so [`None`] on the way in
    /// costs nothing and [`Some`] on the way out means "C assigned at this point".
    pub adler: Option<u32>,
    /// `z_stream.data_type`: bits left in the accumulator plus the three state flags
    /// (`zlib.h` L105 and `inflate.c` L1147-L1149).
    pub data_type: i32,
}

impl<'a> InflateStream<'a> {
    /// Builds a **pre-reset, zeroed** view over `input` and `output`: both cursors at zero and
    /// every scalar at zero.
    ///
    /// # This is not yet an initialised `z_stream` -- a reset must be applied
    ///
    /// Zeroed is deliberately not the same thing as initialised, and one field makes the
    /// difference visible. `inflateReset` sets `strm->adler = state->wrap & 1`
    /// (`inflate.c` L100-L109), so a freshly initialised **wrapped zlib** stream carries
    /// `adler == 1` -- the Adler-32 seed -- and a **raw** stream is left alone entirely.
    /// This constructor sets [`Self::adler`] to [`None`], because it has no configuration
    /// to consult and therefore cannot know the wrap mode: "no check value assigned" is
    /// the only honest starting point.
    ///
    /// So: treat the result as the zeroed storage a reset is then applied to, exactly as C
    /// treats the `z_stream` a caller hands to `inflateInit2_` before `inflateReset2` runs. The
    /// reset is what makes the scalars correct for the configuration; skipping it leaves `adler`
    /// wrong for every zlib and gzip stream.
    ///
    /// A caller that is resuming a stream must likewise restore `total_in`, `total_out`, `adler`
    /// and `data_type` itself; they are public fields for exactly that reason.
    #[must_use]
    pub fn new(input: &'a [u8], output: &'a mut [u8]) -> Self {
        Self::with_region(input, OutputRegion::init(output))
    }

    /// Builds a **pre-reset, zeroed** view over `input` and `region`.
    ///
    /// The entry point `crates/libz-rs-sys` uses, because the region it has is
    /// [`OutputRegion::write_only`]: the `avail_out` bytes at a C caller's `next_out` are
    /// writable but not initialised. Everything [`InflateStream::new`]'s documentation says
    /// about the reset applies here unchanged.
    #[must_use]
    pub fn with_region(input: &'a [u8], region: OutputRegion<'a>) -> Self {
        Self {
            input,
            next_in: 0,
            output: region,
            next_out: 0,
            total_in: 0,
            total_out: 0,
            msg: None,
            adler: None,
            data_type: 0,
        }
    }

    /// `z_stream.avail_in`: input bytes still unread (`zlib.h` L92).
    #[must_use]
    pub fn avail_in(&self) -> usize {
        self.input.len().saturating_sub(self.next_in)
    }

    /// `z_stream.avail_out`: output space still unwritten (`zlib.h` L95).
    #[must_use]
    pub fn avail_out(&self) -> usize {
        self.output.len().saturating_sub(self.next_out)
    }

    /// The output bytes this stream has produced, i.e. `output[..next_out]`.
    #[must_use]
    pub fn written(&self) -> &[u8] {
        self.output.initialized(self.next_out)
    }

    /// Applies the public half of a state reset (`inflate.c` L104-L107).
    ///
    /// `inflateResetKeep` writes five `z_stream` fields; [`StreamReset`] carries them
    /// out of the state module and this method puts them in place. `adler` is written
    /// only when the reset asked for it, which is C's `if (state->wrap)` guard at
    /// L108 -- the comment there records that the assignment exists "to support
    /// ill-conceived Java test suite", so it is preserved for its own sake.
    pub fn apply_reset(&mut self, reset: StreamReset) {
        self.total_in = u64::from(reset.total_in);
        self.total_out = u64::from(reset.total_out);
        self.msg = reset.msg;
        self.data_type = reset.data_type;
        if let Some(adler) = reset.adler {
            self.adler = Some(adler);
        }
    }
}

/// The four ways an arm of C's `switch (state->mode)` can end.
///
/// Naming them is what replaces `goto inf_leave` -- an arm says how it finished and
/// the dispatcher acts on it, so the epilogue has exactly one call site instead of
/// twelve jump targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Step {
    /// C's `break` out of the `switch`, and equally C's `/* fallthrough */`.
    ///
    /// The arm has already put the next state in [`InflateState::mode`], so the
    /// dispatcher simply looks again. The two C spellings differ only in whether the
    /// next arm runs without re-entering the loop, which is unobservable **provided
    /// the fall-through assigned a mode**. `TYPE` -> `TYPEDO` is the one that does
    /// not (`inflate.c` L708-L711), and it is handled by calling the `TYPEDO` body
    /// directly, leaving `mode` at [`Mode::Type`] exactly as C does.
    Continue,

    /// C's `goto inf_leave` with `ret` still `Z_OK`, i.e. out of input or out of
    /// output space. [`InflateState::mode`] is unchanged, so the next call resumes
    /// in the same state.
    Leave,

    /// C's `goto inf_leave` after assigning `ret`: `Z_STREAM_END` from `DONE`
    /// (L1112) or `Z_DATA_ERROR` from `BAD` (L1115).
    LeaveWith(ReturnCode),

    /// C's **bare** `return`, which skips the `inf_leave` epilogue entirely.
    ///
    /// Three sites: `MEM` (L1118), `SYNC` and the unreachable `default` (L1122), and
    /// the `Z_NEED_DICT` exit from `DICT` (L703). Skipping the epilogue is the
    /// contract, not an optimisation -- it is why a memory error is non-recoverable
    /// (the comment at L1129, and the two consecutive `Z_MEM_ERROR` returns
    /// `test/infcover.c` L426-L427 asserts), and why the dictionary id published in
    /// `strm->adler` survives for `inflateSetDictionary` to check against.
    Return(ReturnCode),
}

/// Which of the two live decode tables a lookup walks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DecodeTable {
    /// `state->lencode`, indexed with `state->lenbits`.
    Length,
    /// `state->distcode`, indexed with `state->distbits`.
    Distance,
}

/// The outcome of locating one Huffman code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Lookup {
    /// The entry the code selects. Any `DROPBITS` for a *first-level* entry that
    /// linked to a second-level table has already been applied.
    Found(Code),
    /// C's `PULLBYTE` found no input, so the enclosing state must return.
    NeedInput,
    /// The computed index fell outside the table.
    ///
    /// Unreachable from any stream: [`inflate_table`] always fills all `1 << root`
    /// root entries, a root width never exceeds `MAXBITS`, and a second-level index
    /// is bounded by `val + (1 << op)`, which is the extent that function reserved.
    /// It exists so that the lookup is total, and the caller turns it into the
    /// invalid-code data error that the state it occurred in would have produced.
    OutOfRange,
}

/// Where a match copy reads its bytes from, i.e. the two things C's `from` pointer
/// can address at `inflate.c` L1048, L1051 and L1055.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MatchSource {
    /// An index into the sliding window.
    Window(usize),
    /// An index into output this call, or an earlier one, already wrote.
    Output(usize),
}

/// The locals of C's `inflate()` that outlive a single state arm.
///
/// Holding them in one value is what keeps every state method to two parameters and
/// therefore well inside `clippy.toml`'s argument threshold; it also puts the
/// write-back to [`InflateStream`] in exactly one place. See the module
/// documentation on why C's `LOAD()`/`RESTORE()` have no counterpart.
struct Inflater<'i, 'o> {
    /// C's input buffer, whole. `next` is `input[next_in..]`.
    input: &'i [u8],
    /// C's `next` (L476), as an index.
    next_in: usize,
    /// C's output buffer and C's `put` (L477) as one value: the region plus the index into
    /// it. The cursor owns the region for the duration of the call and gives both back to
    /// [`InflateStream`] in the epilogue.
    output: OutputCursor<'o>,
    /// The `flush` argument, kept as the raw C `int`.
    ///
    /// Deliberately **not** converted to a validated enum: `inflate()` has always
    /// treated an unrecognised `flush` like `Z_NO_FLUSH`, and rejecting one would
    /// refuse calls the reference accepts.
    flush: i32,
    /// C's `in` (L481): the `avail_in` the call started with.
    input_start: usize,
    /// C's `out` (L481): the `avail_out` the call started with -- and then, after the
    /// trailer has been credited, the `avail_out` at that checkpoint (L1081).
    output_start: usize,
    /// `strm->total_in`.
    total_in: u64,
    /// `strm->total_out`.
    total_out: u64,
    /// `strm->msg`.
    msg: Option<&'static str>,
    /// `strm->adler`, or [`None`] while no check value has been assigned.
    ///
    /// Carries [`InflateStream::adler`]'s convention through the drive: the three
    /// sites that assign it are the three places C writes `strm->adler`, and
    /// anything this leaves as [`None`] is a member C never touched.
    adler: Option<u32>,
    /// `strm->data_type`.
    data_type: i32,
}

impl Inflater<'_, '_> {
    /// C's `have` (L478): input bytes still unread.
    #[inline]
    fn avail_in(&self) -> usize {
        self.input.len().saturating_sub(self.next_in)
    }

    /// C's `left` (L478): output space still unwritten.
    #[inline]
    fn avail_out(&self) -> usize {
        self.output.remaining()
    }

    /// C's `put` (L477) as an index: how far the output cursor has advanced.
    #[inline]
    fn next_out(&self) -> usize {
        self.output.written()
    }

    /// `PULLBYTE()`: take one input byte into the accumulator (`inflate.c` L356-L362).
    ///
    /// Returns `false` where C does `goto inf_leave`, leaving the accumulator and the
    /// cursor untouched so that the state resumes cleanly. The second failure --
    /// [`InflateState::pull_byte`] declining a byte -- needs an accumulator holding
    /// more than 56 bits, which no state can produce, and is reported the same way
    /// because "cannot make progress" is the truthful answer either way.
    #[inline]
    fn pull_byte<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> bool {
        let Some(byte) = self.input.get(self.next_in).copied() else {
            return false;
        };
        if !state.pull_byte(byte) {
            return false;
        }
        self.next_in = self.next_in.saturating_add(1);
        true
    }

    /// `NEEDBITS(n)`: fill the accumulator to at least `needed` bits
    /// (`inflate.c` L365-L371).
    ///
    /// Returns `false` for C's implicit `goto inf_leave`. Bits already pulled stay in
    /// the accumulator and the input cursor stays past them, which is what makes a
    /// 32-bit field readable across several calls.
    #[inline]
    fn need_bits<'a, A: Allocator<'a>>(
        &mut self,
        state: &mut InflateState<'a, A>,
        needed: u32,
    ) -> bool {
        state.need_bits(needed, self.input, &mut self.next_in)
    }

    /// `strm->msg = "..."; state->mode = BAD;` -- the shape every rejection in
    /// `inflate()` has.
    ///
    /// Returns [`Step::Continue`] so that the dispatcher lands in the [`Mode::Bad`]
    /// arm, which is precisely what C's `break` out of the `switch` does; the data
    /// error is produced there, at L1115, and never at the rejection site.
    #[inline]
    fn reject<'a, A: Allocator<'a>>(
        &mut self,
        state: &mut InflateState<'a, A>,
        msg: &'static str,
    ) -> Step {
        self.msg = Some(msg);
        state.mode = Mode::Bad;
        Step::Continue
    }

    /// Writes one byte at the output cursor and advances it.
    ///
    /// Returns `false` only when the cursor is already at the end of the buffer; every
    /// caller has checked `avail_out` first, so that is unreachable, and reporting it
    /// as "no space" keeps the state resumable either way.
    #[inline]
    fn write_byte(&mut self, byte: u8) -> bool {
        self.output.push_byte(byte)
    }

    /// `UPDATE_CHECK(check, buf, len)` over the last `count` bytes written
    /// (`inflate.c` L302-L303).
    ///
    /// gzip streams accumulate a CRC-32 and zlib streams an Adler-32; a raw stream
    /// never gets here, because both call sites are guarded by `wrap & 4`. Returns
    /// [`None`] if fewer than `count` bytes have been written, which the arithmetic at
    /// both call sites rules out.
    fn update_check<'a, A: Allocator<'a>>(
        &self,
        state: &InflateState<'a, A>,
        count: usize,
    ) -> Option<u32> {
        let region = self.output.written_tail(count)?;
        Some(if state.flags == 0 {
            adler32(state.check, region)
        } else {
            crc32(state.check, region)
        })
    }

    /// Folds a [`header`] state's outcome into the dispatcher's vocabulary.
    ///
    /// [`header`] owns the eleven header states but not `z_stream`, so it hands back
    /// the two values C writes through the stream pointer: the check value to publish
    /// in `strm->adler` (the four `strm->adler = state->check = ...` sites at L550,
    /// L690, L696 and L705) and the message for `strm->msg`.
    ///
    /// [`HeaderAction::Bad`] becomes [`Step::Continue`], not an error: the handler has
    /// already set [`Mode::Bad`], and C's `break` out of the `switch` is what turns
    /// that into `Z_DATA_ERROR`, one dispatch later.
    fn apply_header(&mut self, exit: HeaderExit) -> Step {
        if let Some(adler) = exit.adler {
            self.adler = Some(adler);
        }
        if let Some(msg) = exit.msg {
            self.msg = Some(msg);
        }
        match exit.action {
            HeaderAction::Advance | HeaderAction::Bad => Step::Continue,
            HeaderAction::NeedInput => Step::Leave,
            HeaderAction::NeedDict => Step::Return(ReturnCode::NEED_DICT),
        }
    }
}

impl Inflater<'_, '_> {
    /// Reads the three-bit block header and selects the block's decoder.
    ///
    /// The Rust counterpart of the `TYPEDO` arm (`inflate.c` L711-L746), which the `TYPE` arm
    /// falls into. Four outcomes, matching RFC 1951 §3.2.3's `BTYPE`: `00` stored,
    /// `01` fixed Huffman codes, `10` dynamic Huffman codes, `11` reserved.
    ///
    /// Two details are easy to lose:
    ///
    /// * `DROPBITS(2)` at L745 runs after the inner `switch` on **every** path
    ///   including the invalid one, which is why it sits after the `match` here. The
    ///   fixed-block `Z_TREES` early exit at L731-L735 does its own `DROPBITS(2)` and
    ///   leaves, so it must not reach the trailing one.
    /// * `if (state->last)` at L712 aligns the accumulator with `BYTEBITS()` before
    ///   moving to the trailer, because the check value that follows is byte-aligned.
    fn type_do<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L712-L716.
        if state.last {
            state.byte_bits();
            state.mode = Mode::Check;
            return Step::Continue;
        }

        // L717.
        if !self.need_bits(state, 3) {
            return Step::Leave;
        }

        // L718-L719: the `BFINAL` bit.
        state.last = low_bits(state.hold, 1) != 0;
        drop_bits(state, 1);

        // L720-L744: `BTYPE`.
        match low_bits(state.hold, 2) {
            // L721-L725: stored block.
            0 => state.mode = Mode::Stored,
            // L726-L735: fixed Huffman codes. The tables are compile-time constants,
            // so selecting them is four assignments and cannot fail.
            1 => {
                inflate_fixed(&mut state.tables);
                state.mode = Mode::LenFirst;
                if self.flush == Z_TREES {
                    drop_bits(state, 2);
                    return Step::Leave;
                }
            }
            // L736-L740: dynamic Huffman codes.
            2 => state.mode = Mode::Table,
            // L741-L743: `11` is reserved by RFC 1951 §3.2.3.
            _ => return self.reject_after_block_type(state),
        }

        // L745.
        drop_bits(state, 2);
        Step::Continue
    }

    /// The reserved-`BTYPE` rejection, which still consumes its two bits.
    ///
    /// C reaches `DROPBITS(2)` at L745 from the `default` arm too, because the arm
    /// only assigns and does not break out of the outer `switch`. Splitting it out
    /// keeps that ordering visible instead of implied.
    fn reject_after_block_type<'a, A: Allocator<'a>>(
        &mut self,
        state: &mut InflateState<'a, A>,
    ) -> Step {
        let step = self.reject(state, MSG_INVALID_BLOCK_TYPE);
        drop_bits(state, 2);
        step
    }

    /// Reads a stored block's byte-aligned `LEN`/`NLEN` pair.
    ///
    /// The Rust counterpart of the `STORED` arm (`inflate.c` L747-L761). RFC 1951 §3.2.4 puts
    /// `NLEN` immediately after `LEN` as its one's complement, so the two must satisfy
    /// `LEN == ~NLEN`; C tests that as `(hold & 0xffff) != ((hold >> 16) ^ 0xffff)`
    /// and the comparison is reproduced on the full 64-bit accumulator rather than on
    /// a masked copy, so that a caller who primed extra high bits gets C's answer.
    ///
    /// `state.mode` becomes [`Mode::CopyBlock`] *before* the `Z_TREES` test at L760,
    /// so a `Z_TREES` caller stops with the block header parsed and the copy pending.
    fn stored<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L748: RFC 1951 §3.2.4 -- "skip any remaining bits in current partially
        // processed byte".
        state.byte_bits();

        // L749.
        if !self.need_bits(state, 32) {
            return Step::Leave;
        }

        // L750-L754.
        let hold = state.hold;
        if (hold & 0xffff) != ((hold >> 16) ^ 0xffff) {
            return self.reject(state, MSG_INVALID_STORED_BLOCK_LENGTHS);
        }

        // L755-L759.
        state.length = low_u32(hold & 0xffff);
        state.init_bits();
        state.mode = Mode::CopyBlock;

        // L760.
        if self.flush == Z_TREES {
            return Step::Leave;
        }
        Step::Continue
    }

    /// Copies a stored block's bytes straight from input to output.
    ///
    /// The Rust counterpart of the `COPY` arm (`inflate.c` L765-L781), which `COPY_` falls into
    /// after the one assignment at L763. The transfer is `min(length, avail_in,
    /// avail_out)` bytes and repeats across calls until `length` reaches zero; a
    /// `copy` of zero with `length` still positive is C's `goto inf_leave` at L770,
    /// and is the only way this state suspends.
    fn copy_stored<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L766-L767.
        let length = to_index(u64::from(state.length));
        if length == 0 {
            // L779-L780: the block is complete.
            state.mode = Mode::Type;
            return Step::Continue;
        }

        // L768-L770.
        let copy = length.min(self.avail_in()).min(self.avail_out());
        if copy == 0 {
            return Step::Leave;
        }

        // L771-L776. `copy` was clamped to both availabilities, so neither range can
        // leave its buffer; a `false` return would mean the clamp itself was wrong,
        // and suspending is then the only non-destructive answer.
        if !self.copy_input_to_output(copy) {
            return Step::Leave;
        }
        state.length = state.length.saturating_sub(to_count(copy));
        Step::Continue
    }

    /// The `zmemcpy(put, next, copy)` of the stored-block state (`inflate.c` L771)
    /// together with the four cursor updates at L772-L775.
    fn copy_input_to_output(&mut self, copy: usize) -> bool {
        let input = self.input;
        let input_end = self.next_in.saturating_add(copy);
        let Some(source) = input.get(self.next_in..input_end) else {
            return false;
        };
        // `push_slice` copies what fits and advances by exactly that much, so the two
        // cursor updates of L772-L775 cannot disagree. A short copy would mean `copy`
        // exceeded `avail_out`, which the `min` at the call site rules out.
        if self.output.push_slice(source) != source.len() {
            return false;
        }
        self.next_in = input_end;
        true
    }

    /// Reads a dynamic block's three alphabet sizes.
    ///
    /// The Rust counterpart of the `TABLE` arm (`inflate.c` L782-L800): `HLIT`, `HDIST` and
    /// `HCLEN` of RFC 1951 §3.2.7, biased by 257, 1 and 4 respectively.
    ///
    /// ★ The `nlen > 286 || ndist > 30` rejection at L791 is inside
    /// `#ifndef PKZIP_BUG_WORKAROUND`, and that macro is **not** defined in the
    /// shipped build, so the check is active and is implemented.
    fn table<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L783.
        if !self.need_bits(state, 14) {
            return Step::Leave;
        }

        // L784-L789. Each bias is small and each field is at most five bits, so no sum
        // approaches the range of `u32`.
        state.nlen = low_u32(low_bits(state.hold, 5)) + 257;
        drop_bits(state, 5);
        state.ndist = low_u32(low_bits(state.hold, 5)) + 1;
        drop_bits(state, 5);
        state.ncode = low_u32(low_bits(state.hold, 4)) + 4;
        drop_bits(state, 4);

        // L790-L797.
        if state.nlen > MAX_LITERAL_LENGTH_CODES || state.ndist > MAX_DISTANCE_CODES {
            return self.reject(state, MSG_TOO_MANY_SYMBOLS);
        }

        // L798-L800.
        state.have = 0;
        state.mode = Mode::LenLens;
        Step::Continue
    }

    /// Reads the code lengths of the code-length code and builds its table.
    ///
    /// The Rust counterpart of the `LENLENS` arm (`inflate.c` L802-L822). `ncode` three-bit
    /// lengths arrive in [`ORDER`]; the remaining slots up to nineteen are zeroed, and
    /// the resulting alphabet is handed to [`inflate_table`] with a root of
    /// [`CODES_ROOT_BITS`].
    ///
    /// `state.have` counts the lengths read so far and is a field, not a local, so the
    /// loop resumes mid-alphabet when `NEEDBITS(3)` runs out of input.
    fn len_lens<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L803-L807. `ncode` is `BITS(4) + 4`, so at most 19; clamping to the length
        // of `ORDER` makes that bound structural rather than argued.
        let ncode = to_index(u64::from(state.ncode)).min(CODE_LENGTH_CODES);
        while to_index(u64::from(state.have)) < ncode {
            if !self.need_bits(state, 3) {
                return Step::Leave;
            }
            let length = low_u16(low_bits(state.hold, 3));
            if !store_code_length(state, length) {
                return Step::Leave;
            }
            drop_bits(state, 3);
        }

        // L808-L809: every code-length code not sent has length zero.
        while to_index(u64::from(state.have)) < CODE_LENGTH_CODES {
            if !store_code_length(state, 0) {
                return Step::Leave;
            }
        }

        // L810-L812: both tables start at the base of this stream's own arena, and the
        // root width is seven. C assigns all three before the build, and passes
        // `&state->lenbits` into it, so the width the builder settles on is written
        // back over the seven whether the build succeeds or fails. `distbits` is
        // deliberately passed through unchanged: C does not touch it here, because the
        // distance table has no meaning until `CODELENS` builds one.
        state.next = 0;
        if !state.use_dynamic_tables(0, CODES_ROOT_BITS, 0, state.tables.distbits) {
            return self.reject(state, MSG_INVALID_CODE_LENGTHS_SET);
        }

        // L813-L814.
        let ret = inflate_table(
            CodeType::Codes,
            &state.lens,
            CODE_LENGTH_CODES,
            &mut state.codes,
            &mut state.next,
            &mut state.tables.lenbits,
            &mut state.work,
        );

        // L815-L819.
        if ret != TABLE_OK {
            return self.reject(state, MSG_INVALID_CODE_LENGTHS_SET);
        }

        // L820-L822.
        state.have = 0;
        state.mode = Mode::CodeLens;
        Step::Continue
    }
}

/// `state->lens[order[state->have++]] = length` (`inflate.c` L805 and L809).
///
/// Returns `false` if the permutation or the length array cannot be indexed. Neither
/// can happen: `have` is below nineteen at every call, [`ORDER`] has nineteen entries,
/// and its values are all below `LENS_LEN`.
fn store_code_length<'a, A: Allocator<'a>>(state: &mut InflateState<'a, A>, length: u16) -> bool {
    let position = to_index(u64::from(state.have));
    let Some(&slot) = ORDER.get(position) else {
        return false;
    };
    let Some(entry) = state.lens.get_mut(to_index(u64::from(slot))) else {
        return false;
    };
    *entry = length;
    state.have = state.have.saturating_add(1);
    true
}

/// The root width of the table `which` names, i.e. `state->lenbits` or
/// `state->distbits` (`inflate.c` L827, L925 and L977).
#[inline]
fn root_bits<'a, A: Allocator<'a>>(state: &InflateState<'a, A>, which: DecodeTable) -> usize {
    match which {
        DecodeTable::Length => state.tables.lenbits,
        DecodeTable::Distance => state.tables.distbits,
    }
}

/// The table `which` names, i.e. `state->lencode` or `state->distcode`.
#[inline]
fn decode_table<'s, 'a, A: Allocator<'a>>(
    state: &'s InflateState<'a, A>,
    which: DecodeTable,
) -> &'s [Code] {
    match which {
        DecodeTable::Length => state.lencode(),
        DecodeTable::Distance => state.distcode(),
    }
}

impl Inflater<'_, '_> {
    /// Locates one code in the root table, pulling input until the entry it selects
    /// says it has enough bits.
    ///
    /// The Rust counterpart of the bare `for (;;)` loops at `inflate.c` L826-L830, L924-L928 and
    /// L976-L980. Nothing is dropped: the caller decides how many bits the code
    /// actually cost, because a first-level entry may turn out to be a link to a
    /// second-level table.
    ///
    /// The index is `BITS(root)` even when fewer than `root` bits are live -- see
    /// [`low_bits`] -- which is the whole reason the loop terminates: a short read
    /// lands on an entry whose `bits` exceeds what is available, one more byte is
    /// pulled, and the index is recomputed.
    fn root_lookup<'a, A: Allocator<'a>>(
        &mut self,
        state: &mut InflateState<'a, A>,
        which: DecodeTable,
    ) -> Lookup {
        loop {
            let width = to_count(root_bits(state, which));
            let index = to_index(low_bits(state.hold, width));
            let Some(&here) = decode_table(state, which).get(index) else {
                return Lookup::OutOfRange;
            };
            if u32::from(here.bits) <= state.bits {
                return Lookup::Found(here);
            }
            if !self.pull_byte(state) {
                return Lookup::NeedInput;
            }
        }
    }

    /// Locates one code, descending into a second-level table when the root entry is a
    /// link.
    ///
    /// The Rust counterpart of `inflate.c` L924-L939 (literal/length) and L976-L991 (distance),
    /// which differ in exactly one place, at L929 versus L981:
    ///
    /// | Table | C test | Why |
    /// |---|---|---|
    /// | literal/length | `here.op && (here.op & 0xf0) == 0` | `op == 0` is a literal byte, not a link |
    /// | distance | `(here.op & 0xf0) == 0` | the distance alphabet has no literals, so a clear high nibble can only be a link |
    ///
    /// When it does descend, the `DROPBITS(last.bits)` and `state->back += last.bits`
    /// that follow the inner loop (L937-L938 and L989-L990) are applied here, so the
    /// caller is left with exactly C's remaining work: drop `here.bits` and add them
    /// to `back`.
    fn lookup_code<'a, A: Allocator<'a>>(
        &mut self,
        state: &mut InflateState<'a, A>,
        which: DecodeTable,
    ) -> Lookup {
        let last = match self.root_lookup(state, which) {
            Lookup::Found(here) => here,
            other => return other,
        };

        let is_link = match which {
            DecodeTable::Length => last.op != 0 && (last.op & OP_KIND_MASK) == 0,
            DecodeTable::Distance => (last.op & OP_KIND_MASK) == 0,
        };
        if !is_link {
            return Lookup::Found(last);
        }

        // L931-L936 and L983-L988: index the second-level table with the bits above
        // the ones the first level consumed.
        let base = u32::from(last.bits);
        let width = base.saturating_add(u32::from(last.op & OP_EXTRA_BITS));
        let here = loop {
            let offset = low_bits(state.hold, width).checked_shr(base).unwrap_or(0);
            let index = to_index(u64::from(last.val)).saturating_add(to_index(offset));
            let Some(&here) = decode_table(state, which).get(index) else {
                return Lookup::OutOfRange;
            };
            if base.saturating_add(u32::from(here.bits)) <= state.bits {
                break here;
            }
            if !self.pull_byte(state) {
                return Lookup::NeedInput;
            }
        };

        // L937-L938 and L989-L990.
        drop_bits(state, base);
        state.back = state.back.saturating_add(i32::from(last.bits));
        Lookup::Found(here)
    }
}

impl Inflater<'_, '_> {
    /// Decodes the literal/length and distance code lengths, then builds both tables.
    ///
    /// The Rust counterpart of the `CODELENS` arm (`inflate.c` L824-L910), and the longest single
    /// state in the decoder. `nlen + ndist` code lengths are read as code-length codes,
    /// where symbols 0..=15 are literal lengths and 16, 17 and 18 are the three repeat
    /// forms of RFC 1951 §3.2.7:
    ///
    /// | Symbol | Extra bits | Meaning |
    /// |---|---|---|
    /// | 16 | 2 | repeat the previous length 3..=6 times |
    /// | 17 | 3 | repeat a zero length 3..=10 times |
    /// | 18 | 7 | repeat a zero length 11..=138 times |
    ///
    /// ★ Two **distinct** sites both emit [`MSG_INVALID_BIT_LENGTH_REPEAT`]: symbol 16
    /// with no previous length to copy (L840-L841), and any repeat that would run past
    /// the end of the two alphabets (L864-L865). Both are reproduced.
    ///
    /// The end-of-block code is mandatory (L878-L883): a block whose symbol 256 has a
    /// zero code length could never terminate, so it is rejected before the tables are
    /// built rather than discovered mid-decode.
    fn code_lens<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        let total =
            to_index(u64::from(state.nlen)).saturating_add(to_index(u64::from(state.ndist)));

        // L825-L872.
        while to_index(u64::from(state.have)) < total {
            // L826-L830. The code-length code has no second level: its root is seven
            // bits and RFC 1951 §3.2.7 caps its lengths at seven.
            let here = match self.root_lookup(state, DecodeTable::Length) {
                Lookup::Found(here) => here,
                Lookup::NeedInput => return Step::Leave,
                Lookup::OutOfRange => return self.reject(state, MSG_INVALID_CODE_LENGTHS_SET),
            };

            if here.val < 16 {
                // L831-L834: a literal code length.
                drop_bits(state, u32::from(here.bits));
                if !store_length_at_have(state, here.val, total) {
                    return self.reject(state, MSG_INVALID_BIT_LENGTH_REPEAT);
                }
                continue;
            }

            // L835-L871: one of the three repeat forms. Note the ordering C uses in
            // each: `NEEDBITS` covers the code *and* its extra bits, and only then are
            // the code's own bits dropped -- so running out of input rewinds to before
            // the code was consumed and the state resumes cleanly.
            let Some((length, count)) = self.repeat_form(state, here) else {
                // A rejection has already set `Mode::Bad` and the message, and needs
                // one more dispatch to reach the `Bad` arm; running out of input has
                // changed nothing and needs the epilogue.
                return if state.mode == Mode::Bad {
                    Step::Continue
                } else {
                    Step::Leave
                };
            };

            // L863-L868.
            if to_index(u64::from(state.have)).saturating_add(count) > total {
                return self.reject(state, MSG_INVALID_BIT_LENGTH_REPEAT);
            }

            // L869-L870.
            for _ in 0..count {
                if !store_length_at_have(state, length, total) {
                    return self.reject(state, MSG_INVALID_BIT_LENGTH_REPEAT);
                }
            }
        }

        // L874-L875 needs no counterpart: `self.reject` returns `Step::Continue`, so a
        // rejection inside the loop above already leaves through the `Bad` arm.

        // L877-L883: the end-of-block code is not optional.
        if state.lens.get(END_OF_BLOCK_SYMBOL).copied().unwrap_or(0) == 0 {
            return self.reject(state, MSG_MISSING_END_OF_BLOCK);
        }

        self.build_dynamic_tables(state)
    }

    /// Decodes one repeat code and returns `(length, count)`.
    ///
    /// The Rust counterpart of `inflate.c` L836-L862. Returns [`None`] for both of C's exits from
    /// this stretch: out of input, where the state is unchanged and resumable, and the
    /// `have == 0` rejection at L839-L844, which is distinguished by [`Mode::Bad`]
    /// having been set.
    fn repeat_form<'a, A: Allocator<'a>>(
        &mut self,
        state: &mut InflateState<'a, A>,
        here: Code,
    ) -> Option<(u16, usize)> {
        let code_bits = u32::from(here.bits);
        if here.val == 16 {
            // L836-L848: repeat the previous length.
            if !self.need_bits(state, code_bits.saturating_add(2)) {
                return None;
            }
            drop_bits(state, code_bits);
            if state.have == 0 {
                self.reject(state, MSG_INVALID_BIT_LENGTH_REPEAT);
                return None;
            }
            let previous = to_index(u64::from(state.have)).checked_sub(1)?;
            let length = state.lens.get(previous).copied()?;
            let count = 3 + to_index(low_bits(state.hold, 2));
            drop_bits(state, 2);
            Some((length, count))
        } else if here.val == 17 {
            // L849-L855: a short run of zeros.
            if !self.need_bits(state, code_bits.saturating_add(3)) {
                return None;
            }
            drop_bits(state, code_bits);
            let count = 3 + to_index(low_bits(state.hold, 3));
            drop_bits(state, 3);
            Some((0, count))
        } else {
            // L856-L862: a long run of zeros.
            if !self.need_bits(state, code_bits.saturating_add(7)) {
                return None;
            }
            drop_bits(state, code_bits);
            let count = 11 + to_index(low_bits(state.hold, 7));
            drop_bits(state, 7);
            Some((0, count))
        }
    }

    /// Builds the literal/length and distance decode tables from `state.lens`.
    ///
    /// The Rust counterpart of `inflate.c` L885-L910. ★ The two root widths are
    /// [`LENS_ROOT_BITS`] and [`DISTS_ROOT_BITS`]; the reference's own comment at
    /// L885-L887 warns against changing them without reading `inftrees.h`'s notes on
    /// the `ENOUGH` constants, which are derived from exactly these values.
    ///
    /// The distance table starts where the literal/length table ended, which is why
    /// `state.next` is read into `distcode` *before* the second build (L898) and why
    /// both builds share one cursor.
    fn build_dynamic_tables<'a, A: Allocator<'a>>(
        &mut self,
        state: &mut InflateState<'a, A>,
    ) -> Step {
        // L888-L892.
        state.next = 0;
        state.tables.lencode = CodeTableSource::Dynamic { offset: 0 };
        state.tables.lenbits = LENS_ROOT_BITS;
        let nlen = to_index(u64::from(state.nlen));
        let ret = inflate_table(
            CodeType::Lens,
            &state.lens,
            nlen,
            &mut state.codes,
            &mut state.next,
            &mut state.tables.lenbits,
            &mut state.work,
        );

        // L893-L897.
        if ret != TABLE_OK {
            return self.reject(state, MSG_INVALID_LITERAL_LENGTHS_SET);
        }

        // L898-L901. `nlen` is at most 286 and `lens` holds 320 entries, so the tail
        // always exists; the fallible form is used because it is the only way to take
        // it without indexing.
        state.tables.distcode = CodeTableSource::Dynamic { offset: state.next };
        state.tables.distbits = DISTS_ROOT_BITS;
        let ndist = to_index(u64::from(state.ndist));
        let Some(distance_lens) = state.lens.get(nlen..) else {
            return self.reject(state, MSG_INVALID_DISTANCES_SET);
        };
        let ret = inflate_table(
            CodeType::Dists,
            distance_lens,
            ndist,
            &mut state.codes,
            &mut state.next,
            &mut state.tables.distbits,
            &mut state.work,
        );

        // L902-L906.
        if ret != TABLE_OK {
            return self.reject(state, MSG_INVALID_DISTANCES_SET);
        }

        // L907-L909.
        state.mode = Mode::LenFirst;
        if self.flush == Z_TREES {
            return Step::Leave;
        }
        Step::Continue
    }
}

/// `state->lens[state->have++] = length`, bounded by the two alphabets' total size.
///
/// Returns `false` if `have` has reached `total` or the end of `lens`; every caller has
/// already established that it has not, so the guard is what makes the write provably
/// in range rather than argued to be.
fn store_length_at_have<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    length: u16,
    total: usize,
) -> bool {
    let position = to_index(u64::from(state.have));
    if position >= total {
        return false;
    }
    let Some(entry) = state.lens.get_mut(position) else {
        return false;
    };
    *entry = length;
    state.have = state.have.saturating_add(1);
    true
}

impl Inflater<'_, '_> {
    /// Decodes one literal/length code, or hands the whole block to the fast path.
    ///
    /// The Rust counterpart of the `LEN` arm (`inflate.c` L914-L963), which `LEN_` falls into
    /// after the one assignment at L912.
    ///
    /// ★ The dispatch at L915-L922 is the decoder's hot path: when at least six input
    /// bytes and 258 output bytes are available, [`inflate_fast`] decodes as many codes
    /// as it can without re-checking availability per symbol. `state.back = -1` on an
    /// end-of-block return is set **here**, by the caller, not inside the fast path
    /// (L919-L920).
    ///
    /// The slow path resets `back` to zero and then accumulates every bit the code
    /// costs into it, so that `inflateMark` can report how far back in the input the
    /// undecoded remainder begins.
    fn len<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L915-L922.
        if self.avail_in() >= MIN_AVAIL_IN && self.avail_out() >= MIN_AVAIL_OUT {
            let start = self.output_start;
            let input = self.input;
            let exit = inflate_fast(state, input, &mut self.next_in, &mut self.output, start);
            if let Some(msg) = exit.msg {
                self.msg = Some(msg);
            }
            if state.mode == Mode::Type {
                state.back = BACK_UNKNOWN;
            }
            return Step::Continue;
        }

        // L923.
        state.back = 0;

        // L924-L939.
        let here = match self.lookup_code(state, DecodeTable::Length) {
            Lookup::Found(here) => here,
            Lookup::NeedInput => return Step::Leave,
            Lookup::OutOfRange => return self.reject(state, MSG_INVALID_LITERAL_LENGTH_CODE),
        };

        // L940-L942.
        drop_bits(state, u32::from(here.bits));
        state.back = state.back.saturating_add(i32::from(here.bits));
        state.length = u32::from(here.val);

        // L943-L949: a literal byte.
        if here.op == 0 {
            state.mode = Mode::Lit;
            return Step::Continue;
        }

        // L950-L955: end of block. `back` becomes "no pending code".
        if here.op & OP_END_OF_BLOCK != 0 {
            state.back = BACK_UNKNOWN;
            state.mode = Mode::Type;
            return Step::Continue;
        }

        // L956-L960.
        if here.op & OP_INVALID_CODE != 0 {
            return self.reject(state, MSG_INVALID_LITERAL_LENGTH_CODE);
        }

        // L961-L962.
        state.extra = u32::from(here.op & OP_EXTRA_BITS);
        state.mode = Mode::LenExt;
        Step::Continue
    }

    /// Reads a length code's extra bits.
    ///
    /// The Rust counterpart of the `LENEXT` arm (`inflate.c` L964-L974). RFC 1951 §3.2.5 gives
    /// symbols 265..=284 between one and five extra bits, which are added to the base
    /// length the table entry carried.
    ///
    /// ★ `state.was = state.length` at L972 records the match's full length before any
    /// of it is copied, which is what lets `inflateMark` report `was - length` as the
    /// progress through a match in flight.
    fn len_ext<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L965-L970.
        let extra = state.extra;
        if extra != 0 {
            if !self.need_bits(state, extra) {
                return Step::Leave;
            }
            state.length = state
                .length
                .wrapping_add(low_u32(low_bits(state.hold, extra)));
            drop_bits(state, extra);
            state.back = state
                .back
                .saturating_add(i32::try_from(extra).unwrap_or(i32::MAX));
        }

        // L972-L973.
        state.was = state.length;
        state.mode = Mode::Dist;
        Step::Continue
    }

    /// Decodes one distance code.
    ///
    /// The Rust counterpart of the `DIST` arm (`inflate.c` L975-L1001). Structurally identical to
    /// the slow path of [`Inflater::len`], with the one link-test difference documented
    /// on [`Inflater::lookup_code`] and without the literal and end-of-block cases:
    /// the distance alphabet has neither.
    fn dist<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L976-L991.
        let here = match self.lookup_code(state, DecodeTable::Distance) {
            Lookup::Found(here) => here,
            Lookup::NeedInput => return Step::Leave,
            Lookup::OutOfRange => return self.reject(state, MSG_INVALID_DISTANCE_CODE),
        };

        // L992-L993.
        drop_bits(state, u32::from(here.bits));
        state.back = state.back.saturating_add(i32::from(here.bits));

        // L994-L998: symbols 30 and 31 exist in the alphabet but encode nothing, and
        // `inflate_table` marks them invalid.
        if here.op & OP_INVALID_CODE != 0 {
            return self.reject(state, MSG_INVALID_DISTANCE_CODE);
        }

        // L999-L1001.
        state.offset = u32::from(here.val);
        state.extra = u32::from(here.op & OP_EXTRA_BITS);
        state.mode = Mode::DistExt;
        Step::Continue
    }

    /// Reads a distance code's extra bits.
    ///
    /// The Rust counterpart of the `DISTEXT` arm (`inflate.c` L1003-L1019). RFC 1951 §3.2.5 gives
    /// symbols 4..=29 between one and thirteen extra bits.
    ///
    /// The `state->offset > state->dmax` rejection at L1010-L1016 is inside
    /// `#ifdef INFLATE_STRICT`, which the shipped build does not define, so it is
    /// deliberately **not** implemented: a stream whose distances exceed the window size
    /// advertised in its own header is still accepted here, exactly as by C, and the
    /// `MATCH` state's `copy > whave` test is what actually keeps every read in bounds.
    fn dist_ext<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L1004-L1009.
        let extra = state.extra;
        if extra != 0 {
            if !self.need_bits(state, extra) {
                return Step::Leave;
            }
            state.offset = state
                .offset
                .wrapping_add(low_u32(low_bits(state.hold, extra)));
            drop_bits(state, extra);
            state.back = state
                .back
                .saturating_add(i32::try_from(extra).unwrap_or(i32::MAX));
        }

        // L1017-L1018.
        state.mode = Mode::Match;
        Step::Continue
    }

    /// Copies a matching string from the window or from the output already written.
    ///
    /// The Rust counterpart of the `MATCH` arm (`inflate.c` L1020-L1065), and the state where a
    /// malformed distance would become a read outside the buffer.
    ///
    /// C's `copy = out - left` is the number of bytes this call has written. A distance
    /// no larger than that reaches back into the current output buffer; a larger one
    /// reaches into the sliding window, and only that case can be invalid -- hence the
    /// `copy > state->whave` rejection at L1025-L1031, which compares against the bytes
    /// the window is *known* to hold rather than against its allocated size.
    ///
    /// ★ The `INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR` branch at L1032-L1044, which
    /// substitutes zero bytes for unavailable history, is `#ifdef`-gated out of the
    /// shipped build and is not implemented. `state.sane` is therefore always true here, and
    /// `inflateUndermine` cannot change that.
    ///
    /// ★ The copy stays **forward and byte-at-a-time**, exactly as C's
    /// `do { *put++ = *from++; } while (--copy);`. `offset` may be smaller than
    /// `length` -- run-length encoding with `offset == 1` is legal and common -- so the
    /// copy is intentionally self-overlapping and each byte written may be a byte read
    /// later in the same loop. Any bulk copy would produce different output.
    fn match_string<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L1021.
        let left = self.avail_out();
        if left == 0 {
            return Step::Leave;
        }

        // L1022.
        let written = self.output_start.saturating_sub(left);
        let offset = to_index(u64::from(state.offset));
        let length = to_index(u64::from(state.length));

        let (source, mut copy) = if offset > written {
            // L1023-L1052: the match reaches back before this output buffer.
            let mut back = offset.saturating_sub(written);

            // L1025-L1031.
            if back > to_index(u64::from(state.whave)) && state.sane {
                return self.reject(state, MSG_INVALID_DISTANCE_TOO_FAR_BACK);
            }

            // L1046-L1051: `back` bytes before the write cursor, in a circular buffer.
            let wnext = to_index(u64::from(state.wnext));
            let from = if back > wnext {
                back = back.saturating_sub(wnext);
                to_index(u64::from(state.wsize)).saturating_sub(back)
            } else {
                wnext.saturating_sub(back)
            };

            // L1052: never read past the end of the window in one pass; the state
            // repeats with a smaller `back` until `length` is exhausted.
            (MatchSource::Window(from), back.min(length))
        } else {
            // L1054-L1057: the match is entirely within this output buffer.
            (
                MatchSource::Output(self.next_out().saturating_sub(offset)),
                length,
            )
        };

        // L1058-L1060.
        copy = copy.min(left);
        state.length = state.length.saturating_sub(to_count(copy));

        // L1061-L1063.
        if !self.copy_match(state, source, copy) {
            return self.reject(state, MSG_INVALID_DISTANCE_TOO_FAR_BACK);
        }

        // L1064.
        if state.length == 0 {
            state.mode = Mode::Len;
        }
        Step::Continue
    }

    /// `do { *put++ = *from++; } while (--copy);` (`inflate.c` L1061-L1063).
    ///
    /// Reads and writes strictly one byte at a time, in increasing address order, so
    /// that a self-overlapping match reproduces the reference's output byte for byte.
    ///
    /// Returns `false` if a source byte is unavailable. For a window source that means
    /// an index at or past `whave`, which [`Inflater::match_string`]'s rejection has
    /// already excluded: the wrapping branch is reachable only once `whave == wsize`,
    /// because until the window has wrapped `whave == wnext` and `back > wnext` would
    /// contradict `back <= whave`; and the non-wrapping branch reads below `wnext`,
    /// which never exceeds `whave`. For an output source it would mean reading past the
    /// write cursor, which `offset <= written` excludes.
    fn copy_match<'a, A: Allocator<'a>>(
        &mut self,
        state: &InflateState<'a, A>,
        source: MatchSource,
        count: usize,
    ) -> bool {
        match source {
            MatchSource::Window(index) => {
                let mut cursor = index;
                for _ in 0..count {
                    let Some(byte) = state.valid_window_byte(cursor) else {
                        return false;
                    };
                    if !self.write_byte(byte) {
                        return false;
                    }
                    cursor = cursor.saturating_add(1);
                }
                true
            }
            // The source is output this call, or an earlier one, already wrote, so the copy
            // is inside the cursor's own buffer. [`OutputCursor::duplicate_from`] performs it
            // in ascending byte order and reproduces the reference's byte loop for an
            // overlapping match as well as a disjoint one -- which is the one property that
            // matters here, because `offset == 1` run-length encoding is legal and common.
            // Expressing it as one call rather than a per-byte read is also what lets the
            // decoder work over write-only output storage, which cannot be read byte by byte.
            MatchSource::Output(index) => self.output.duplicate_from(index, count),
        }
    }

    /// Writes one decoded literal byte.
    ///
    /// The Rust counterpart of the `LIT` arm (`inflate.c` L1066-L1071). `state.length` holds the
    /// literal, because the literal/length alphabet is merged and the table entry's
    /// `val` was stored there by [`Inflater::len`].
    fn lit<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L1067.
        if self.avail_out() == 0 {
            return Step::Leave;
        }
        // L1068-L1069.
        if !self.write_byte(low_u8(u64::from(state.length))) {
            return Step::Leave;
        }
        // L1070.
        state.mode = Mode::Len;
        Step::Continue
    }
}

impl Inflater<'_, '_> {
    /// Verifies the stream's 32-bit check value.
    ///
    /// The Rust counterpart of the `CHECK` arm (`inflate.c` L1072-L1096). A raw stream has no
    /// trailer and passes straight through to [`Mode::Length`]; a zlib stream carries
    /// the Adler-32 of RFC 1950 §2.2 and a gzip stream the CRC-32 of RFC 1952 §2.3.1.
    ///
    /// The output written since the last checkpoint is credited to the totals and
    /// folded into the check value **before** the comparison (L1075-L1081), because
    /// those bytes are part of what the trailer covers. `out` is then reset to the
    /// current `avail_out`, so the epilogue does not credit them twice.
    ///
    /// ★ The two container formats store the value with **opposite** byte order, and
    /// the asymmetry at L1082-L1086 is the whole point of the branch: gzip's CRC-32 is
    /// little-endian, so the accumulator -- filled from the low end -- already holds it
    /// in the right order and is compared directly; zlib's Adler-32 is big-endian, so
    /// it must be byte-reversed first. Getting this the wrong way round rejects every
    /// stream of one format and accepts corrupt ones of the other.
    ///
    /// The comparisons are made against the full accumulator, as in C, so a caller that
    /// primed bits beyond the trailer sees C's answer rather than a masked one.
    fn check<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L1073.
        if !state.wrap.is_raw() {
            // L1074.
            if !self.need_bits(state, 32) {
                return Step::Leave;
            }

            // L1075-L1077.
            let left = self.avail_out();
            let written = self.output_start.saturating_sub(left);
            self.total_out = self.total_out.wrapping_add(to_wide(written));
            // C accumulates `state->total` in an `unsigned long` and masks it to 32
            // bits at L1100; this implementation stores 32 bits and wraps, which is the same
            // masked value for every stream length.
            state.total = state.total.wrapping_add(low_u32(to_wide(written)));

            // L1078-L1080.
            if state.wrap.verifies_check_value() && written != 0 {
                let Some(value) = self.update_check(state, written) else {
                    return Step::Leave;
                };
                state.check = value;
                self.adler = Some(value);
            }

            // L1081: move the checkpoint forward.
            self.output_start = left;

            // L1082-L1090.
            if state.wrap.verifies_check_value() {
                let trailer = if state.flags == 0 {
                    u64::from(state.hold_low32().swap_bytes())
                } else {
                    state.hold
                };
                if trailer != u64::from(state.check) {
                    return self.reject(state, MSG_INCORRECT_DATA_CHECK);
                }
            }

            // L1091.
            state.init_bits();
        }

        // L1094-L1096.
        state.mode = Mode::Length;
        Step::Continue
    }

    /// Verifies a gzip member's uncompressed length.
    ///
    /// The Rust counterpart of the `LENGTH` arm (`inflate.c` L1097-L1108). RFC 1952 §2.3.1's
    /// `ISIZE` field holds the length of the uncompressed data modulo 2^32, and only a
    /// gzip stream has one -- hence the `state->flags` half of the guard at L1098.
    fn length<'a, A: Allocator<'a>>(&mut self, state: &mut InflateState<'a, A>) -> Step {
        // L1098.
        if !state.wrap.is_raw() && state.flags != 0 {
            // L1099.
            if !self.need_bits(state, 32) {
                return Step::Leave;
            }
            // L1100-L1104. `state.total` is already the low 32 bits, so C's
            // `& 0xffffffff` is implicit in its width.
            if state.wrap.verifies_check_value() && state.hold != u64::from(state.total) {
                return self.reject(state, MSG_INCORRECT_LENGTH_CHECK);
            }
            // L1105.
            state.init_bits();
        }

        // L1109.
        state.mode = Mode::Done;
        Step::Continue
    }
}

impl Inflater<'_, '_> {
    /// Updates the window and the counters, then settles the return code.
    ///
    /// The Rust counterpart of the `inf_leave` block (`inflate.c` L1131-L1152), reproduced
    /// statement for statement. Reached from every [`Step::Leave`] and
    /// [`Step::LeaveWith`], and from none of the [`Step::Return`]s -- which is the
    /// distinction C draws by using `goto inf_leave` for the former and a bare `return`
    /// for the latter.
    ///
    /// Four things happen here, in this order:
    ///
    /// 1. **The window is brought up to date** (L1133-L1138). The condition is subtle
    ///    and is implemented exactly: the window is updated if one already exists, or if
    ///    output was written and the stream neither failed nor finished -- where
    ///    "finished" is qualified by `flush != Z_FINISH`, because a caller who has
    ///    declared there is no more input does not need history kept for a next call.
    ///    ★ `mode < BAD` and `mode < CHECK` are **ordinal** comparisons on
    ///    [`Mode`], which derives `Ord` with declaration order equal to C's enumerator
    ///    order for exactly this purpose. A failed update is C's `state->mode = MEM;
    ///    return Z_MEM_ERROR;`, which skips the rest of the epilogue.
    /// 2. **The counters are credited** (L1139-L1143), from the difference between the
    ///    availabilities at entry and now.
    /// 3. **The check value is extended** over the output written (L1144-L1146), and
    ///    `strm->data_type` is composed from the accumulator occupancy plus the three
    ///    state flags (L1147-L1149).
    /// 4. **`Z_BUF_ERROR` is substituted for `Z_OK`** when no progress was made in
    ///    either direction, or when `flush` was `Z_FINISH` (L1150-L1151). The second
    ///    half is what makes `Z_FINISH` never return `Z_OK`.
    fn leave<'a, A>(&mut self, state: &mut InflateState<'a, A>, ret: ReturnCode) -> ReturnCode
    where
        A: Allocator<'a> + Copy,
    {
        // L1133-L1138.
        let avail_out = self.avail_out();
        let written = self.output_start.saturating_sub(avail_out);
        if state.wsize != 0
            || (self.output_start != avail_out
                && state.mode < Mode::Bad
                && (state.mode < Mode::Check || self.flush != Z_FINISH))
        {
            // C passes `strm->next_out`, the address one past the last byte written;
            // `update_window` takes the last `written` bytes of the region instead, so
            // the whole produced prefix is the natural argument.
            let region = self.output.written_slice();
            if update_window(state, region, written).is_err() {
                return state.latch_memory_error();
            }
        }

        // L1139-L1143. C's counters are `unsigned long` and wrap; so do these.
        let consumed = self.input_start.saturating_sub(self.avail_in());
        self.total_in = self.total_in.wrapping_add(to_wide(consumed));
        self.total_out = self.total_out.wrapping_add(to_wide(written));
        state.total = state.total.wrapping_add(low_u32(to_wide(written)));

        // L1144-L1146.
        if state.wrap.verifies_check_value() && written != 0 {
            if let Some(value) = self.update_check(state, written) {
                state.check = value;
                self.adler = Some(value);
            }
        }

        // L1147-L1149.
        self.data_type = state.data_type();

        // L1150-L1151.
        if ((consumed == 0 && written == 0) || self.flush == Z_FINISH) && ret == ReturnCode::OK {
            return ReturnCode::BUF_ERROR;
        }
        ret
    }
}

impl Inflater<'_, '_> {
    /// Dispatches on [`InflateState::mode`] until a state says to stop.
    ///
    /// The Rust counterpart of C's `for (;;) switch (state->mode)` (`inflate.c` L504-L1123).
    ///
    /// ★ The `match` is **exhaustive over all thirty-two** [`Mode`] variants and has
    /// no `_ =>` arm, which is the point of modelling the mode as an enum at all
    /// (AAP §0.3.3.1): adding a state without handling it is then a compile error rather
    /// than the silent fall-through to `default:` that C would produce. C's `default:`
    /// (L1121) is unreachable there and collapses into the [`Mode::Sync`] arm here,
    /// which returns the same `Z_STREAM_ERROR`.
    ///
    /// Every arm either advances [`InflateState::mode`], consumes input, produces
    /// output, or stops -- so the loop always terminates.
    fn drive<'a, A>(&mut self, state: &mut InflateState<'a, A>) -> ReturnCode
    where
        A: Allocator<'a> + Copy,
    {
        // L503.
        let mut ret = ReturnCode::OK;
        loop {
            let step = match state.mode {
                // L506-L553: the zlib/gzip/raw fork.
                Mode::Head => {
                    let exit = header::head(state, self.input, &mut self.next_in);
                    self.apply_header(exit)
                }
                // L555-L692: the gzip member header, RFC 1952 §2.3.1. Each of these
                // assigns the next mode before C falls through, so re-dispatching is
                // exactly equivalent to the fall-through.
                Mode::Flags => {
                    let exit = header::flags(state, self.input, &mut self.next_in);
                    self.apply_header(exit)
                }
                Mode::Time => {
                    let exit = header::time(state, self.input, &mut self.next_in);
                    self.apply_header(exit)
                }
                Mode::Os => {
                    let exit = header::os(state, self.input, &mut self.next_in);
                    self.apply_header(exit)
                }
                Mode::ExLen => {
                    let exit = header::ex_len(state, self.input, &mut self.next_in);
                    self.apply_header(exit)
                }
                Mode::Extra => {
                    let exit = header::extra(state, self.input, &mut self.next_in);
                    self.apply_header(exit)
                }
                Mode::Name => {
                    let exit = header::name(state, self.input, &mut self.next_in);
                    self.apply_header(exit)
                }
                Mode::Comment => {
                    let exit = header::comment(state, self.input, &mut self.next_in);
                    self.apply_header(exit)
                }
                Mode::HCrc => {
                    let exit = header::hcrc(state, self.input, &mut self.next_in);
                    self.apply_header(exit)
                }
                // L694-L706: the zlib preset-dictionary fork, RFC 1950 §2.2.
                Mode::DictId => {
                    let exit = header::dict_id(state, self.input, &mut self.next_in);
                    self.apply_header(exit)
                }
                Mode::Dict => {
                    let exit = header::dict(state);
                    self.apply_header(exit)
                }
                // L708-L710. ★ C assigns no mode before falling into `TYPEDO`, so the
                // body runs with `mode` still `TYPE`. That is observable -- a suspension
                // inside it leaves `TYPE` behind, which both adds 128 to
                // `strm->data_type` and makes the next call take the L499 shortcut -- so
                // the body is invoked directly instead of re-dispatching.
                Mode::Type => {
                    if self.flush == Z_BLOCK || self.flush == Z_TREES {
                        Step::Leave
                    } else {
                        self.type_do(state)
                    }
                }
                // L711-L746.
                Mode::TypeDo => self.type_do(state),
                // L747-L761.
                Mode::Stored => self.stored(state),
                // L762-L764.
                Mode::CopyBlock => {
                    state.mode = Mode::Copy;
                    Step::Continue
                }
                // L765-L781.
                Mode::Copy => self.copy_stored(state),
                // L782-L800.
                Mode::Table => self.table(state),
                // L802-L822.
                Mode::LenLens => self.len_lens(state),
                // L824-L910.
                Mode::CodeLens => self.code_lens(state),
                // L911-L913.
                Mode::LenFirst => {
                    state.mode = Mode::Len;
                    Step::Continue
                }
                // L914-L963.
                Mode::Len => self.len(state),
                // L964-L974.
                Mode::LenExt => self.len_ext(state),
                // L975-L1001.
                Mode::Dist => self.dist(state),
                // L1003-L1019.
                Mode::DistExt => self.dist_ext(state),
                // L1020-L1065.
                Mode::Match => self.match_string(state),
                // L1066-L1071.
                Mode::Lit => self.lit(state),
                // L1072-L1096.
                Mode::Check => self.check(state),
                // L1097-L1108.
                Mode::Length => self.length(state),
                // L1110-L1113.
                Mode::Done => Step::LeaveWith(ReturnCode::STREAM_END),
                // L1114-L1116. `latched_error` is the state module's name for the two
                // modes that carry an already-decided status; it answers `DATA_ERROR`
                // here and `MEM_ERROR` below, so the fallbacks are unreachable.
                Mode::Bad => {
                    Step::LeaveWith(state.latched_error().unwrap_or(ReturnCode::DATA_ERROR))
                }
                // L1117-L1118: a bare return. The mode is sticky, so every subsequent
                // call reports the same error; `test/infcover.c` L426-L427 asserts
                // exactly that, and L1129 explains why it must be so.
                Mode::Mem => Step::Return(state.latched_error().unwrap_or(ReturnCode::MEM_ERROR)),
                // L1119-L1122: also a bare return. Only `inflateSync` enters this mode,
                // and `inflate()` cannot resume from it -- `inflateSync` must finish the
                // search first. C's unreachable `default:` shares this arm.
                Mode::Sync => Step::Return(ReturnCode::STREAM_ERROR),
            };

            match step {
                Step::Continue => {}
                Step::Leave => break,
                Step::LeaveWith(code) => {
                    ret = code;
                    break;
                }
                Step::Return(code) => return code,
            }
        }

        self.leave(state, ret)
    }
}

/// Decompresses as much of `stream` as the available input and output space allow.
///
/// The Rust counterpart of `inflate` (`inflate.c` L474-L1153), declared at `zlib.h` L405. See the
/// [module documentation](self) for the resumable-state-machine contract this function
/// implements, and for what `flush` does and does not affect.
///
/// # Parameters
///
/// * `state` -- the decoder state, created by [`inflate_init`] or [`inflate_init2`] and
///   carried across calls.
/// * `stream` -- the buffers and the caller-visible counters. Both cursors, both
///   totals, `msg`, `adler` and `data_type` are updated in place.
/// * `flush` -- the raw C `flush` argument. `Z_BLOCK` stops at a deflate block
///   boundary, `Z_TREES` additionally stops immediately after a block header, and
///   `Z_FINISH` forbids a `Z_OK` return. Any other value, documented or not, behaves
///   like `Z_NO_FLUSH`, which is what the reference has always done.
///
/// # Returns
///
/// * [`ReturnCode::OK`] -- progress was made and the stream is not finished.
/// * [`ReturnCode::STREAM_END`] -- the end of the compressed stream was reached.
/// * [`ReturnCode::NEED_DICT`] -- a preset dictionary is required; its Adler-32 is in
///   `stream.adler`, and [`inflate_set_dictionary`] is how to supply it.
/// * [`ReturnCode::DATA_ERROR`] -- the input is corrupt; `stream.msg` says how.
/// * [`ReturnCode::MEM_ERROR`] -- the sliding window could not be allocated. The state
///   is then unusable until it is reset: this is C's non-recoverable memory error.
/// * [`ReturnCode::BUF_ERROR`] -- no progress was possible, or `flush` was `Z_FINISH`
///   and the stream did not end. Not fatal; call again with more room.
///
/// # The guards C applies that this function does not
///
/// C rejects a stream at L494-L496 for three reasons. Two of them are about raw
/// pointers -- a null `next_out`, and a null `next_in` with a non-zero `avail_in` --
/// and belong to `crates/libz-rs-sys`, which is where pointers exist; note that a null
/// `next_in` with `avail_in == 0` is **legal** and corresponds to an empty `input`
/// slice, which is likewise accepted here. The third is `inflateStateCheck`, whose
/// pointer and owner-identity halves are also the facade's; its mode-range half is
/// [`inflate_state_check`], and it cannot fail for a `&mut InflateState`, because such
/// a reference is proof that the state exists and holds a live [`Mode`].
pub fn inflate<'a, A>(
    state: &mut InflateState<'a, A>,
    stream: &mut InflateStream<'_>,
    flush: i32,
) -> ReturnCode
where
    A: Allocator<'a> + Copy,
{
    // L499: skip the `Z_BLOCK`/`Z_TREES` early exit on re-entry, so that a caller who
    // stopped at a block boundary is not stopped again at the same one. Omitting this
    // silently changes `Z_BLOCK` from "stop at each boundary" into "never advance".
    if state.mode == Mode::Type {
        state.mode = Mode::TypeDo;
    }

    // Destructuring gives independent borrows of each field, which is what lets the
    // driver hold the output buffer while the cursors and counters are written back.
    let InflateStream {
        input,
        next_in,
        output,
        next_out,
        total_in,
        total_out,
        msg,
        adler,
        data_type,
    } = stream;

    // L500-L502: `LOAD(); in = have; out = left;`. The output cursor owns its region for
    // the duration of the call -- a complete handle on the caller's buffer that hands out no
    // view of it -- so the region is moved out of the stream here and moved back by the
    // single write-back below.
    let region = core::mem::replace(output, OutputRegion::empty());
    let mut engine = Inflater {
        input,
        next_in: *next_in,
        output: OutputCursor::from_region(region, *next_out),
        flush,
        input_start: 0,
        output_start: 0,
        total_in: *total_in,
        total_out: *total_out,
        msg: *msg,
        adler: *adler,
        data_type: *data_type,
    };
    engine.input_start = engine.avail_in();
    engine.output_start = engine.avail_out();

    let ret = engine.drive(state);

    // C's `RESTORE()` plus the direct writes through `strm`. Unconditional, because the
    // three bare-return paths either consume nothing at all or -- for `Z_NEED_DICT` --
    // call `RESTORE()` themselves; see the module documentation.
    let (produced_region, produced) = engine.output.into_region();
    *output = produced_region;
    *next_in = engine.next_in;
    *next_out = produced;
    *total_in = engine.total_in;
    *total_out = engine.total_out;
    *msg = engine.msg;
    *adler = engine.adler;
    *data_type = engine.data_type;

    ret
}

/// The mode-range half of `inflateStateCheck` (`inflate.c` L88-L98).
///
/// C's function rejects a stream for any of five reasons:
///
/// | C test | Where it lives |
/// |---|---|
/// | `strm == Z_NULL` | the facade |
/// | `strm->zalloc == 0 \|\| strm->zfree == 0` | the facade |
/// | `state == Z_NULL` | the facade |
/// | `state->strm != strm` | the facade |
/// | `state->mode < HEAD \|\| state->mode > SYNC` | **here** |
///
/// The first four require dereferencing caller-supplied pointers and inspecting
/// caller-supplied hooks, which only the facade crate does (AAP §0.6.1 category 3).
/// The fifth is this function.
///
/// ★ The tag range is why [`Mode::Head`] starts at 16180 rather than at zero. The
/// state must already be safe to read before the tag can be looked at, so what the
/// range test adds is a check on the *object*, not on the pointer: a live state that
/// came from `deflateInit`, one that was never initialised, or one whose tag has been
/// overwritten is very unlikely to hold a number in `16180..=16211` where the mode
/// belongs, and the test rejects it. It cannot notice a pointer that has been freed —
/// such a pointer may still hold its old tag, and reading it is already undefined
/// behaviour. It is a heuristic, and C says as much by pairing it with the
/// owner-identity check.
///
/// Returns `true` when the state must be **rejected**, matching the polarity of C's
/// non-zero return so that the two read the same way side by side.
#[must_use]
pub const fn inflate_state_check(mode_tag: i32) -> bool {
    !is_live_mode_tag(mode_tag)
}

/// Creates a decoder state for a `windowBits` request.
///
/// The core half of `inflateInit2_` (`inflate.c` L173-L212), declared at `zlib.h`
/// L1911. C's steps map as follows:
///
/// | C | Here |
/// |---|---|
/// | L178-L180 version and `stream_size` check | the facade; `ZLIB_VERSION` is not this crate's business |
/// | L181-L196 `strm` null check and `zalloc`/`zfree` defaulting | the facade |
/// | L197-L200 `ZALLOC` plus `zmemzero` of the state | the returned value, fully initialised |
/// | L202-L205 install into `strm->state`, `state->strm = strm`, `window = Z_NULL`, `mode = HEAD` | [`InflateState::with_validated_config`] |
/// | L206 `inflateReset2` | [`InflateConfig::validate`], which owns the same decode |
/// | L207-L210 free the state if the reset failed | returning `Err`, which frees nothing because nothing was allocated |
///
/// C pre-seeds `mode = HEAD` at L205 with the comment "to pass state test in
/// `inflateReset2()`", i.e. so the freshly zeroed state survives its own
/// `inflateStateCheck`. Constructing the value here in one step makes that ordering
/// unnecessary, and the state is never observable in an intermediate form.
///
/// # ★ The `z_stream` half of the reset C performs at L206
///
/// `inflateInit2_` reaches `inflateResetKeep` through `inflateReset2` and
/// `inflateReset`, so its side effects on the *caller's* `z_stream` are part of
/// `inflateInit2_`'s observable contract: `total_in`, `total_out` and `data_type` go
/// to zero, `msg` to `Z_NULL`, and -- the easily missed one -- `adler` to `wrap & 1`
/// whenever `wrap` is non-zero (`inflate.c` L105-L109, whose comment reads "to support
/// ill-conceived Java test suite"). A zlib stream therefore reports `adler == 1`
/// *before* a single byte has been decoded.
///
/// This function returns only the state, because a [`StreamReset`] carries nothing a
/// fresh state does not already imply. A facade that owns a `z_stream` obtains that
/// half by calling [`inflate_reset`] on the value returned here, which mirrors C's own
/// call chain and is **idempotent** on the state: every field
/// [`InflateState::reset_keep`] assigns already holds the assigned value, and
/// `wsize`/`whave`/`wnext` are already zero. Skipping it leaves `z_stream.adler` at
/// whatever the caller had, which is a visible divergence.
///
/// # Errors
///
/// [`ReturnCode::STREAM_ERROR`] for a `windowBits` outside the matrix
/// `crate::config::decode_inflate_window_bits` accepts.
pub fn inflate_init2<'a, A: Allocator<'a>>(
    config: InflateConfig,
    allocator: A,
) -> Result<InflateState<'a, A>, ReturnCode> {
    InflateState::new(config, allocator)
}

/// Creates a decoder state for a zlib stream with the largest window.
///
/// The Rust counterpart of `inflateInit_` (`inflate.c` L214-L217), declared at `zlib.h` L237, which
/// is [`inflate_init2`] with `windowBits` = [`DEF_WBITS`].
///
/// # Errors
///
/// None in practice: [`DEF_WBITS`] is always valid. The signature keeps the `Result` so
/// that the facade has one shape for both initialisers.
pub fn inflate_init<'a, A: Allocator<'a>>(allocator: A) -> Result<InflateState<'a, A>, ReturnCode> {
    inflate_init2(InflateConfig::new(DEF_WBITS), allocator)
}

/// Releases everything the decoder state owns.
///
/// The Rust counterpart of `inflateEnd` (`inflate.c` L1155-L1165), declared at `zlib.h` L471. C
/// frees the window and then the state itself, through the same `zfree` they came from,
/// and clears `strm->state`. Here, releasing the window does the first; clearing the
/// facade's `z_stream.state` is the facade's step.
///
/// ★ The state arrives by mutable reference, not by value: a state moved into this
/// function would carry its window's borrow in argument position, where it is protected
/// for the whole call, and freeing memory a protected reference covers is undefined
/// behaviour. The caller keeps the state and drops it where it lives, its window slot
/// already empty. `zlib_rs::allocate::ForeignBlock` records the rule.
///
/// Always [`ReturnCode::OK`]: C's only failure is the `inflateStateCheck` at L1157,
/// which cannot fail for an owned [`InflateState`].
pub fn inflate_end<'a, A: Allocator<'a>>(state: &mut InflateState<'a, A>) -> ReturnCode {
    state.release();
    ReturnCode::OK
}

/// Resets the stream without discarding the window's contents.
///
/// The Rust counterpart of `inflateResetKeep` (`inflate.c` L99-L123), declared at `zlib.h` L490.
/// The window, `wsize`, `whave` and `wnext` all survive, so a caller can restart
/// decoding with the previous stream's history still available as a dictionary.
///
/// The values C assigns are reproduced exactly, and several are not zero: `flags` goes
/// to -1 ("raw or no header yet"), `dmax` to 32768, `back` to -1, `sane` to true, `mode`
/// to [`Mode::Head`], and both decode-table pointers to the base of the arena.
///
/// The returned [`StreamReset`] is the `z_stream` half -- `total_in`, `total_out`, `msg`,
/// `data_type` and the conditional `adler` -- which [`InflateStream::apply_reset`]
/// applies.
pub fn inflate_reset_keep<'a, A: Allocator<'a>>(state: &mut InflateState<'a, A>) -> StreamReset {
    state.reset_keep()
}

/// Resets the stream and forgets the window's contents.
///
/// The Rust counterpart of `inflateReset` (`inflate.c` L125-L134), declared at `zlib.h` L479. The
/// three window cursors are cleared and then [`inflate_reset_keep`] does the rest; the
/// allocation itself is kept, so no reset ever reallocates.
pub fn inflate_reset<'a, A: Allocator<'a>>(state: &mut InflateState<'a, A>) -> StreamReset {
    state.reset()
}

/// Resets the stream and changes the container format or window size.
///
/// The Rust counterpart of `inflateReset2` (`inflate.c` L136-L171), declared at `zlib.h` L500.
///
/// ★ The `windowBits` decode -- the sign convention for raw streams, the `+16` and
/// `+32` gzip requests, the `windowBits == 0` "take it from the header" case and the
/// `8..=15` bounds -- is owned by `crate::config`, which is where the same decode also
/// serves `inflateInit2_` and `inflateBackInit_`. Nothing about it is re-derived here.
///
/// A window already allocated for a *different* exponent is released, because it is the
/// wrong size (L162-L165); one allocated for the same exponent is kept.
///
/// # Errors
///
/// [`ReturnCode::STREAM_ERROR`] for a `windowBits` outside the accepted matrix. The
/// request is validated before anything is changed, so a rejected call leaves the stream
/// exactly as it was.
pub fn inflate_reset2<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    config: InflateConfig,
) -> Result<StreamReset, ReturnCode> {
    state.reset2(config)
}

/// Inserts bits into the accumulator ahead of the stream.
///
/// The Rust counterpart of `inflatePrime` (`inflate.c` L219-L236), declared at `zlib.h` L1011. Used
/// to start decoding at a bit position that is not a byte boundary, which is what a
/// caller resuming from an `inflateMark` position needs.
///
/// `bits == 0` is a no-op; a negative `bits` clears the accumulator; and a request is
/// refused with [`ReturnCode::STREAM_ERROR`] if it asks for more than sixteen bits at
/// once or would push the accumulator above thirty-two bits.
pub fn inflate_prime<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    bits: i32,
    value: i32,
) -> ReturnCode {
    state.prime(bits, value)
}

/// Returns how far into the current block decoding has got.
///
/// The Rust counterpart of `inflateMark` (`inflate.c` L1397-L1406), declared at `zlib.h` L1042. The
/// result packs two numbers: the high bits are the number of unused bits of the last
/// consumed byte, negated -- C's `state->back`, which is -1 when no code is pending --
/// and the low sixteen bits are the progress through a copy:
///
/// | Mode | Low half | Meaning |
/// |---|---|---|
/// | [`Mode::Copy`] | `length` | stored-block bytes still to copy |
/// | [`Mode::Match`] | `was - length` | match bytes already copied |
/// | anything else | 0 | no copy in progress |
///
/// A stream that fails `inflateStateCheck` gets [`INFLATE_MARK_BAD_STATE`] instead;
/// only the facade can detect that.
#[must_use]
pub fn inflate_mark<'a, A: Allocator<'a>>(state: &InflateState<'a, A>) -> i64 {
    // C computes `(long)(((unsigned long)((long)state->back)) << 16)`, whose only
    // purpose is to shift a possibly negative `back` without signed-overflow concerns;
    // shifting an `i64` left by 16 has the identical two's-complement result and cannot
    // overflow, because `back` never exceeds the 48 bits a code can cost.
    let back = i64::from(state.back) << 16;
    let progress = match state.mode {
        Mode::Copy => i64::from(state.length),
        Mode::Match => i64::from(state.was.wrapping_sub(state.length)),
        _ => 0,
    };
    back.wrapping_add(progress)
}

/// Returns whether the stream is stopped at an empty stored block's length field.
///
/// The Rust counterpart of `inflateSyncPoint` (`inflate.c` L1320-L1326), declared at `zlib.h` L1105.
///
/// The reference's own explanation (L1312-L1319): this is true at the end of a block
/// generated by `Z_SYNC_FLUSH` or `Z_FULL_FLUSH`, and one PPP implementation uses it as
/// a safety check -- PPP flushes with `Z_SYNC_FLUSH` but strips the length bytes of the
/// resulting empty stored block, so on decompression it verifies that at the end of an
/// input packet `inflate` is waiting for exactly those bytes.
#[must_use]
pub fn inflate_sync_point<'a, A: Allocator<'a>>(state: &InflateState<'a, A>) -> bool {
    state.mode == Mode::Stored && state.bits == 0
}

/// Returns how many decode-table entries the current block's tables occupy.
///
/// The Rust counterpart of `inflateCodesUsed` (`inflate.c` L1408-L1413), declared at `zlib.h` L1122.
/// C computes `state->next - state->codes`, a pointer difference; this implementation stores that
/// cursor as an index, so it is read directly.
///
/// A stream that fails `inflateStateCheck` gets [`INFLATE_CODES_USED_BAD_STATE`]
/// instead; only the facade can detect that.
#[must_use]
pub fn inflate_codes_used<'a, A: Allocator<'a>>(state: &InflateState<'a, A>) -> u64 {
    // The cursor is bounded by `ENOUGH` (1444), so the conversion is exact on every
    // target; the fallback exists only to keep the function total.
    u64::try_from(state.codes_used()).unwrap_or(INFLATE_CODES_USED_BAD_STATE)
}

/// Turns check-value verification on or off.
///
/// The Rust counterpart of `inflateValidate` (`inflate.c` L1385-L1395), declared at `zlib.h` L1115.
/// Sets or clears bit 2 of `state->wrap`, which is the bit both the trailer comparison
/// and the running check-value update consult. A raw stream is never promoted, because
/// it has no check value to verify -- that is C's `if (check && state->wrap)` guard.
pub fn inflate_validate<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    check: bool,
) -> ReturnCode {
    state.set_validate(check);
    ReturnCode::OK
}

/// Refuses to allow matches that reach further back than the window holds.
///
/// The Rust counterpart of `inflateUndermine` (`inflate.c` L1370-L1383), declared at `zlib.h` L2044.
///
/// ★ In the shipped build this function **fails**: the permissive behaviour lives behind
/// `INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR`, which is not defined, so C forces
/// `state->sane = 1` and returns `Z_DATA_ERROR` rather than `Z_OK`
/// (L1379-L1381). `test/infcover.c` L440 asserts exactly that, and it is why
/// `state.sane` is always true and the `MATCH` state's history check can never be
/// bypassed.
pub fn inflate_undermine<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    subvert: bool,
) -> ReturnCode {
    state.undermine(subvert)
}

/// Duplicates a decoder state, so that decoding can be branched.
///
/// The Rust counterpart of `inflateCopy` (`inflate.c` L1328-L1368), declared at `zlib.h` L970. The
/// use C documents is scanning ahead speculatively while keeping the ability to resume
/// from where the scan began.
///
/// ★ C has to repair pointers after the bulk copy: L1357-L1361 tests whether
/// `state->lencode` points inside `state->codes` and, if so, rebases both table pointers
/// onto the copy's own arena, and L1362 rebases the `next` cursor the same way. This implementation
/// needs none of it, because `crate::inflate::inftrees::CodeTableSource` holds an
/// *offset* into the arena rather than a pointer into it -- and an offset is equally
/// valid in a copy. The fixed tables are shared statics, so they need no fix-up either.
///
/// ★ Only the window's first `whave` bytes are copied (L1363-L1364), not all `wsize` of
/// them: the rest has never been written, and copying it would propagate whatever the
/// allocator left there -- `0xa5` under `test/infcover.c`'s instrumented allocator.
///
/// # Errors
///
/// [`ReturnCode::MEM_ERROR`] if the destination window cannot be allocated. The source
/// is left completely untouched, which is what lets `test/infcover.c` keep using it after
/// forcing this failure with `mem_limit`.
pub fn inflate_copy<'a, 'b, A, B>(
    source: &InflateState<'a, A>,
    allocator: B,
) -> Result<InflateState<'b, B>, ReturnCode>
where
    'a: 'b,
    A: Allocator<'a> + Copy,
    B: Allocator<'b> + Copy,
{
    source.try_clone_in(allocator)
}

/// Reads back the decoder's sliding window as a dictionary.
///
/// The Rust counterpart of `inflateGetDictionary` (`inflate.c` L1167-L1185), declared at `zlib.h`
/// L935. The window is circular, so the history is returned in two pieces: the older
/// half at `window[wnext..whave]` first, then the newer half at `window[..wnext]`,
/// which puts the bytes back in stream order.
///
/// `dictionary` and `dict_length` are independently optional, exactly as C's two
/// `Z_NULL` tests at L1176 and L1182 make them: a caller may ask only how much history
/// there is.
///
/// C's `dictionary` has no length, and `zlib.h` L935-L941 requires the caller to provide
/// at least 32768 bytes. This implementation additionally clamps to the slice it is handed, so a
/// short buffer receives a prefix instead of overflowing. `dict_length` still reports the
/// full `whave`, as C does.
pub fn inflate_get_dictionary<'a, A: Allocator<'a>>(
    state: &InflateState<'a, A>,
    dictionary: Option<&mut OutputRegion<'_>>,
    dict_length: Option<&mut u32>,
) -> ReturnCode {
    // L1176-L1181.
    if state.whave != 0 {
        if let Some(target) = dictionary {
            copy_history(state, target);
        }
    }
    // L1182-L1183.
    if let Some(slot) = dict_length {
        *slot = state.whave;
    }
    ReturnCode::OK
}

/// The two `zmemcpy` calls of `inflateGetDictionary` (`inflate.c` L1177-L1180).
///
/// `whave >= wnext` always holds -- while the window has not wrapped the two are equal,
/// and once it has, `whave == wsize >= wnext` -- so the first piece is never empty when
/// the second is non-empty, and the two together are exactly `whave` bytes.
fn copy_history<'a, A: Allocator<'a>>(state: &InflateState<'a, A>, target: &mut OutputRegion<'_>) {
    let Some(window) = state.window.as_slice() else {
        return;
    };
    let whave = to_index(u64::from(state.whave)).min(window.len());
    let wnext = to_index(u64::from(state.wnext)).min(whave);
    let older = window.get(wnext..whave).unwrap_or(&[]);
    let newer = window.get(..wnext).unwrap_or(&[]);
    let mut written = 0_usize;
    for piece in [older, newer] {
        let room = target.len().saturating_sub(written);
        let take = piece.len().min(room);
        let Some(chunk) = piece.get(..take) else {
            return;
        };
        if !target.write_slice_at(written, chunk) {
            return;
        }
        written = written.saturating_add(take);
    }
}

/// Supplies the preset dictionary a stream asked for.
///
/// The Rust counterpart of `inflateSetDictionary` (`inflate.c` L1187-L1217), declared at `zlib.h`
/// L913. Two quite different callers are supported, which is why the guard at L1196 is
/// shaped the way it is:
///
/// * a **zlib** stream that returned [`ReturnCode::NEED_DICT`] and is therefore in
///   [`Mode::Dict`]. Its dictionary is verified against the Adler-32 the stream
///   published in `strm.adler`, and a mismatch is [`ReturnCode::DATA_ERROR`].
/// * a **raw** stream, at any point before decoding starts, priming the window with
///   history. There is no id to check, because a raw stream carries none.
///
/// Anything else -- a wrapped stream that is not waiting for a dictionary -- is
/// [`ReturnCode::STREAM_ERROR`].
///
/// The dictionary is installed with `update_window`, which appends to whatever history
/// is already there, so a dictionary longer than the window keeps only its tail: the most
/// recent bytes are the ones a match can reach.
pub fn inflate_set_dictionary<'a, A>(
    state: &mut InflateState<'a, A>,
    dictionary: &[u8],
) -> ReturnCode
where
    A: Allocator<'a> + Copy,
{
    // L1196-L1197, through the predicate the facade also applies so that the two
    // cannot answer differently -- see `InflateState::accepts_dictionary`.
    if !state.accepts_dictionary() {
        return ReturnCode::STREAM_ERROR;
    }

    // L1199-L1205. C seeds with `adler32(0L, Z_NULL, 0)`, which is the constant 1: a
    // null buffer makes `adler32` return the initial value rather than compute one, so
    // the constant is the exact translation and an empty slice would not be.
    if state.mode == Mode::Dict {
        let dictid = adler32(ADLER32_INITIAL_VALUE, dictionary);
        if dictid != state.check {
            return ReturnCode::DATA_ERROR;
        }
    }

    // L1207-L1213. C passes `dictionary + dictLength` as the *end* of the region;
    // `update_window` takes the region and a count of trailing bytes instead.
    if update_window(state, dictionary, dictionary.len()).is_err() {
        return state.latch_memory_error();
    }

    // L1214-L1216.
    state.havedict = true;
    ReturnCode::OK
}

/// Searches `buf` for the four-byte pattern `00 00 ff ff`.
///
/// The Rust counterpart of `syncsearch` (`inflate.c` L1244-L1262). Reproducing the reference's own
/// description of the contract (L1233-L1243):
///
/// > Search `buf[0..len-1]` for the pattern: 0, 0, 0xff, 0xff. Return when found or
/// > when out of input. When called, `*have` is the number of pattern bytes found in
/// > order so far, in 0..3. On return `*have` is updated to the new state. If on return
/// > `*have` equals four, then the pattern was found and the return value is how many
/// > bytes were read including the last byte of the pattern. If `*have` is less than
/// > four, then the pattern has not been found yet and the return value is `len`. In the
/// > latter case, `syncsearch()` can be called again with more data and the `*have`
/// > state.  `*have` is initialized to zero for the first call.
///
/// The pattern is the tail of the empty stored block that `Z_SYNC_FLUSH` emits: `LEN`
/// of zero followed by its complement.
///
/// ★ The third branch is easy to mistake for a reset and must not be simplified. On a
/// **zero** byte that did not match the expected position -- i.e. while looking for one
/// of the two `0xff`s -- `got = 4 - got` **re-seeds** the count rather than clearing it,
/// because that zero may itself be the first byte of a fresh pattern. With `got == 2` it
/// yields 2 (the two zeros seen are `00 00`) and with `got == 3` it yields 1. Writing
/// `got = 0` there would miss patterns that overlap a failed one.
pub(crate) fn syncsearch(have: &mut u32, buf: &[u8]) -> usize {
    // L1249-L1250.
    let mut got = *have;
    let mut next = 0_usize;

    // L1251-L1259.
    while next < buf.len() && got < 4 {
        let Some(byte) = buf.get(next).copied() else {
            break;
        };
        let expected = if got < 2 { 0x00 } else { 0xff };
        if byte == expected {
            got = got.saturating_add(1);
        } else if byte != 0 {
            got = 0;
        } else {
            got = 4_u32.saturating_sub(got);
        }
        next = next.saturating_add(1);
    }

    // L1260-L1261.
    *have = got;
    next
}

/// Skips forward to the next possible full-flush point.
///
/// The Rust counterpart of `inflateSync` (`inflate.c` L1264-L1310), declared at `zlib.h` L951. This
/// is the recovery path: after a data error, it discards input up to and including the
/// next `00 00 ff ff` and prepares the state to resume at the block that follows.
///
/// The search covers the accumulator first and then the unread input, because up to four
/// whole bytes of the stream may already have been pulled into the accumulator and would
/// otherwise be skipped. Those bytes are drained low-byte-first, which is the order they
/// were pulled in.
///
/// ★ Two flag updates at L1299-L1302 decide how the resumed stream behaves. If `flags` is
/// still -1 no header has been seen, so there is nothing to un-wrap and the remainder is
/// treated as raw deflate; otherwise only the check-value bit is cleared, because a
/// check value computed over a stream with a hole in it would be meaningless.
///
/// ★ `flags`, `total_in` and `total_out` are saved across the internal
/// [`inflate_reset`] and restored afterwards, because the reset would otherwise zero the
/// totals a caller is entitled to keep reading.
///
/// # Returns
///
/// * [`ReturnCode::OK`] -- the pattern was found; the stream is at [`Mode::Type`] and
///   `inflate` may be called again.
/// * [`ReturnCode::BUF_ERROR`] -- no input at all and fewer than eight bits buffered, so
///   there is nothing to search.
/// * [`ReturnCode::DATA_ERROR`] -- the available input ran out before the pattern was
///   found. Call again with more input; the partial match is remembered.
pub fn inflate_sync<'a, A: Allocator<'a>>(
    state: &mut InflateState<'a, A>,
    stream: &mut InflateStream<'_>,
) -> ReturnCode {
    // L1274.
    if stream.avail_in() == 0 && state.bits < 8 {
        return ReturnCode::BUF_ERROR;
    }

    // L1276-L1289: on the first call, search what the accumulator already holds.
    if state.mode != Mode::Sync {
        state.mode = Mode::Sync;
        // L1279-L1280: `BYTEBITS()` written out, because C inlines it here.
        state.byte_bits();
        // L1281-L1286. `byte_bits` leaves the occupancy a multiple of eight and no
        // state can exceed 39 bits, so at most four whole bytes are drained and the
        // buffer is exactly the right size; the explicit bound makes that structural.
        let mut buf = [0_u8; 4];
        let mut len = 0_usize;
        while state.bits >= 8 && len < buf.len() {
            let Some(byte) = state.take_byte() else {
                break;
            };
            let Some(slot) = buf.get_mut(len) else {
                break;
            };
            *slot = byte;
            len = len.saturating_add(1);
        }
        // L1287-L1288.
        state.have = 0;
        let buffered = buf.get(..len).unwrap_or(&[]);
        let _found = syncsearch(&mut state.have, buffered);
    }

    // L1291-L1295.
    let unread = stream.input.get(stream.next_in..).unwrap_or(&[]);
    let len = syncsearch(&mut state.have, unread);
    stream.next_in = stream.next_in.saturating_add(len);
    stream.total_in = stream.total_in.wrapping_add(to_wide(len));

    // L1297-L1298.
    if state.have != 4 {
        return ReturnCode::DATA_ERROR;
    }

    // L1299-L1302.
    if state.flags == FLAGS_NO_HEADER {
        state.wrap.clear();
    } else {
        state.wrap.disable_check_validation();
    }

    // L1303-L1309.
    let flags = state.flags;
    let total_in = stream.total_in;
    let total_out = stream.total_out;
    let reset = inflate_reset(state);
    stream.apply_reset(reset);
    stream.total_in = total_in;
    stream.total_out = total_out;
    state.flags = flags;
    state.mode = Mode::Type;
    ReturnCode::OK
}

#[cfg(test)]
// The workspace denies the panic family in library code, which is what the decoder
// above is built to honour. A test that cannot assert is useless, so the harness opts
// back in here only; `clippy.toml` allows exactly this with its four
// `allow-*-in-tests` keys.
//
// The two cast lints are opted out of for the same reason: fixture sizes and chunk
// steps here are small literals and `Vec` lengths, so a `try_from` plus an unreachable
// error arm would add noise without adding a check. Library code above uses the
// checked helpers instead and is clean at `pedantic` without any cast allowance.
#[allow(
    clippy::cast_possible_truncation,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used
)]
mod tests {
    use super::{
        inflate, inflate_codes_used, inflate_copy, inflate_end, inflate_get_dictionary,
        inflate_init, inflate_init2, inflate_mark, inflate_prime, inflate_reset, inflate_reset2,
        inflate_reset_keep, inflate_set_dictionary, inflate_state_check, inflate_sync,
        inflate_sync_point, inflate_undermine, inflate_validate, syncsearch, Allocator,
        InflateState, InflateStream, Mode, OutputRegion, ReturnCode, INFLATE_CODES_USED_BAD_STATE,
        INFLATE_MARK_BAD_STATE, MSG_INCORRECT_DATA_CHECK, MSG_INCORRECT_LENGTH_CHECK,
        MSG_INVALID_BIT_LENGTH_REPEAT, MSG_INVALID_BLOCK_TYPE, MSG_INVALID_CODE_LENGTHS_SET,
        MSG_INVALID_DISTANCES_SET, MSG_INVALID_DISTANCE_CODE, MSG_INVALID_DISTANCE_TOO_FAR_BACK,
        MSG_INVALID_LITERAL_LENGTHS_SET, MSG_INVALID_LITERAL_LENGTH_CODE,
        MSG_INVALID_STORED_BLOCK_LENGTHS, MSG_MISSING_END_OF_BLOCK, MSG_TOO_MANY_SYMBOLS, ORDER,
    };
    use crate::adler32::adler32;
    use crate::allocate::{AllocatorId, Buffer, ForeignBlock, GlobalAllocator, Opaque};
    use crate::config::{
        InflateConfig, Z_BLOCK, Z_FINISH, Z_FULL_FLUSH, Z_NO_FLUSH, Z_PARTIAL_FLUSH, Z_SYNC_FLUSH,
        Z_TREES,
    };
    use crate::inflate::fixed_tables::{distfix, lenfix};
    use crate::inflate::inftrees::{Code, ENOUGH};
    // Aliased so that `the_re_exported_fixed_tables_are_the_generated_ones` can name
    // the module root's re-export without writing `super::lenfix`, which resolves to
    // the same item already in scope and so trips `unused_qualifications`. Binding it
    // here is also what makes that assertion load-bearing: deleting the `pub use` at
    // the top of this module breaks this import, and therefore the build.
    use crate::inflate::{distfix as re_exported_distfix, lenfix as re_exported_lenfix};
    use alloc::vec;
    use alloc::vec::Vec;

    // The compressed streams below are the ones `test/infcover.c` already uses, in the
    // same spelling, so that each expectation is one the reference is known to meet.
    // `try(...)` fixtures come from its `cover_inflate` (L580-L613) and `inf(...)`
    // fixtures from `cover_support` (L366-L370), `cover_inflate` and `cover_fast`
    // (L642-L659). The remainder are produced by the reference compressor.

    /// `zlib.compress(b"hello, hello!", 9)` -- the payload `test/example.c` uses.
    const HELLO_ZLIB: &[u8] = &[
        0x78, 0xda, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x00, 0x21,
        0x70, 0x04, 0x96,
    ];

    /// The same payload as raw DEFLATE (`windowBits` -15).
    const HELLO_RAW: &[u8] = &[
        0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x00,
    ];

    /// The same payload as a gzip member (`windowBits` 31).
    const HELLO_GZIP: &[u8] = &[
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0xcb, 0x48, 0xcd, 0xc9, 0xc9,
        0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x00, 0x9b, 0xdc, 0x9a, 0xb3, 0x0d, 0x00, 0x00, 0x00,
    ];

    /// `zlib.compress(b"", 6)`: a zlib wrapper around one empty fixed block.
    const EMPTY_ZLIB: &[u8] = &[0x78, 0x9c, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01];

    /// The payload at level 0, i.e. one stored block inside a zlib wrapper.
    const STORED_ZLIB: &[u8] = &[
        0x78, 0x01, 0x01, 0x0d, 0x00, 0xf2, 0xff, 0x68, 0x65, 0x6c, 0x6c, 0x6f, 0x2c, 0x20, 0x68,
        0x65, 0x6c, 0x6c, 0x6f, 0x21, 0x21, 0x70, 0x04, 0x96,
    ];

    /// The payload compressed against the preset dictionary [`DICTIONARY`].
    const DICT_ZLIB: &[u8] = &[
        0x78, 0xf9, 0x06, 0x2c, 0x02, 0x15, 0xcb, 0x00, 0x11, 0x3a, 0x0a, 0x60, 0x4a, 0x11, 0x00,
        0x21, 0x70, 0x04, 0x96,
    ];

    /// A repetitive and textual mix, level 6: dynamic Huffman codes, long matches, and
    /// a `detect_data_type` verdict of text.
    const REPEAT_ZLIB: &[u8] = &[
        0x78, 0x9c, 0x4b, 0x4c, 0x4a, 0x4e, 0x1c, 0x45, 0xb4, 0x47, 0x25, 0x19, 0xa9, 0x0a, 0x85,
        0xa5, 0x99, 0xc9, 0xd9, 0x0a, 0x49, 0x45, 0xf9, 0xe5, 0x79, 0x0a, 0x69, 0xf9, 0x15, 0x0a,
        0x59, 0xa5, 0xb9, 0x05, 0xc5, 0x0a, 0xf9, 0x65, 0xa9, 0x45, 0x0a, 0x20, 0xe9, 0x9c, 0xc4,
        0xaa, 0x4a, 0x85, 0x94, 0xfc, 0x74, 0x3d, 0x85, 0x61, 0xaf, 0x18, 0x00, 0x8b, 0x1d, 0xeb,
        0x7b,
    ];

    /// ★ A zlib stream whose single block is **dynamic** Huffman (`BTYPE == 0b10`), so
    /// decoding it runs `CODELENS` and writes real entries into the code arena.
    ///
    /// [`REPEAT_ZLIB`] deliberately is not that stream: byte 2 of it is `0x4b`, whose low
    /// three bits are `BFINAL = 1`, `BTYPE = 0b01` -- a *fixed* block, which reuses the
    /// shared static tables and takes **zero** arena entries. Any test that asserts
    /// [`InflateState::codes_used`] is non-zero must use this fixture instead.
    ///
    /// It is [`dynamic_plain`] compressed at level 9.
    const DYNAMIC_ZLIB: &[u8] = &[
        0x78, 0xda, 0xed, 0xcd, 0xc1, 0x0d, 0x03, 0x31, 0x08, 0x44, 0xd1, 0x56, 0xa6, 0x35, 0x62,
        0xd0, 0x0a, 0x09, 0xb0, 0x63, 0xa0, 0xff, 0xb5, 0x52, 0x41, 0x0a, 0xf0, 0x7d, 0xe6, 0x3f,
        0x9b, 0x5b, 0x1c, 0xba, 0xb2, 0x1d, 0x3c, 0x6d, 0x6e, 0xa4, 0x16, 0xc8, 0xa5, 0x30, 0x66,
        0xa4, 0x8c, 0x92, 0xea, 0x0d, 0x62, 0x5d, 0x9a, 0x43, 0xe3, 0x81, 0xd8, 0x19, 0xa4, 0xf0,
        0x99, 0x43, 0xb4, 0xd3, 0x27, 0xa3, 0xc4, 0xd7, 0xb9, 0x6a, 0x0c, 0x65, 0xe5, 0x8e, 0x42,
        0x17, 0x8c, 0x3e, 0x27, 0x8e, 0x53, 0xfa, 0x85, 0x05, 0x4e, 0x4f, 0x10, 0xc8, 0xf4, 0xdb,
        0x04, 0xbb, 0xf0, 0x85, 0x2f, 0x7c, 0xe1, 0x0b, 0xff, 0x0b, 0xbf, 0xb9, 0x6a, 0x1f, 0x1f,
    ];

    /// 200 copies of one byte as raw DEFLATE: a distance-1, self-overlapping match.
    const RLE_RAW: &[u8] = &[0x8b, 0x8a, 0x1a, 0x1e, 0x00, 0x00];

    /// Raw DEFLATE holding `b"first"`, a `Z_SYNC_FLUSH` point at offset 11, then
    /// `b"second"`.
    const SYNC_RAW: &[u8] = &[
        0x4a, 0xcb, 0x2c, 0x2a, 0x2e, 0x01, 0x00, 0x00, 0x00, 0xff, 0xff, 0x2b, 0x4e, 0x4d, 0xce,
        0xcf, 0x4b, 0x01, 0x00,
    ];

    /// The uncompressed form of [`HELLO_ZLIB`], [`HELLO_RAW`], [`HELLO_GZIP`],
    /// [`STORED_ZLIB`] and [`DICT_ZLIB`].
    const HELLO: &[u8] = b"hello, hello!";

    /// The preset dictionary [`DICT_ZLIB`] was compressed against
    /// (`test/example.c` L47).
    const DICTIONARY: &[u8] = b"hello";

    /// The uncompressed form of [`REPEAT_ZLIB`].
    fn repeat_plain() -> Vec<u8> {
        let mut plain = Vec::new();
        for _ in 0..40 {
            plain.extend_from_slice(b"abcabcabc");
        }
        for _ in 0..6 {
            plain.extend_from_slice(b"the quick brown fox jumps over the lazy dog. ");
        }
        plain
    }

    /// The 1452 plaintext bytes [`DYNAMIC_ZLIB`] expands to.
    fn dynamic_plain() -> Vec<u8> {
        let mut plain = Vec::new();
        for _ in 0..12 {
            plain.extend_from_slice(b"lorem ipsum dolor sit amet consectetur ");
            plain.extend_from_slice(b"adipiscing elit sed do eiusmod tempor ");
            plain.extend_from_slice(b"incididunt ut labore et dolore magna aliqua ");
        }
        plain
    }

    /// `test/infcover.c` L36-L59 `h2b`: space-separated hex byte values to bytes.
    ///
    /// Keeping the reference's own spelling for every fixture is what makes each
    /// expectation below traceable to the C test that already asserts it.
    fn h2b(hex: &str) -> Vec<u8> {
        hex.split_whitespace()
            .map(|token| u8::from_str_radix(token, 16).expect("fixture is not hex"))
            .collect()
    }

    /// Everything one decode run produced.
    #[derive(Debug)]
    struct Outcome {
        ret: ReturnCode,
        output: Vec<u8>,
        msg: Option<&'static str>,
        total_in: u64,
        total_out: u64,
        adler: u32,
        data_type: i32,
        calls: u32,
    }

    /// Drives a decode to completion, feeding `in_step` bytes and offering `out_step`
    /// bytes of room per call.
    ///
    /// The safe-core equivalent of `test/infcover.c`'s `inf()` (its L282-L346): a step of
    /// zero means "all of it", and the loop stops on a terminal status or when three
    /// consecutive calls make no progress. A hard iteration cap turns a hypothetical
    /// non-terminating state machine into a test failure instead of a hung suite.
    fn decode(compressed: &[u8], window_bits: i32, in_step: usize, out_step: usize) -> Outcome {
        decode_with_flush(compressed, window_bits, in_step, out_step, Z_NO_FLUSH)
    }

    /// [`decode`] with an explicit `flush`.
    fn decode_with_flush(
        compressed: &[u8],
        window_bits: i32,
        in_step: usize,
        out_step: usize,
        flush: i32,
    ) -> Outcome {
        let mut state = inflate_init2(InflateConfig::new(window_bits), GlobalAllocator)
            .expect("windowBits should be accepted");
        let outcome = drive_state(&mut state, compressed, in_step, out_step, flush);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
        outcome
    }

    /// The loop of [`decode`], over a state the caller owns.
    fn drive_state<'a, A: Allocator<'a> + Copy>(
        state: &mut InflateState<'a, A>,
        compressed: &[u8],
        in_step: usize,
        out_step: usize,
        flush: i32,
    ) -> Outcome {
        let in_step = if in_step == 0 {
            compressed.len().max(1)
        } else {
            in_step
        };
        let out_step = out_step.max(1);
        let cap = compressed.len().saturating_mul(64).saturating_add(4096);

        let mut output = Vec::new();
        let mut consumed = 0_usize;
        let mut ret = ReturnCode::OK;
        let mut msg = None;
        let mut total_in = 0_u64;
        let mut total_out = 0_u64;
        // ★ `adler` does not start at zero for every stream. C's `inflateInit2_` reaches
        // `inflateResetKeep`, which sets `strm->adler = wrap & 1` (`inflate.c` L108-L109)
        // *under `if (state->wrap)`*, so a zlib or gzip stream reports 1 before any byte
        // is decoded and a raw stream keeps whatever the caller's `z_stream` held -- zero,
        // for the zeroed struct this models. Reproducing the facade's init-time
        // `apply_reset` here, guard included, is what makes this harness agree with the
        // reference on `adler` for streams that fail in the header and never produce output.
        let mut adler = if state.wrap.is_raw() {
            0
        } else {
            state.wrap.adler_seed()
        };
        let mut data_type = 0_i32;
        let mut idle = 0_u32;
        let mut calls = 0_u32;

        for iteration in 0..=cap {
            assert!(iteration < cap, "inflate did not terminate");
            let end = consumed.saturating_add(in_step).min(compressed.len());
            let mut buffer = vec![0_u8; out_step];
            let mut stream = InflateStream::new(&compressed[consumed..end], &mut buffer);
            stream.total_in = total_in;
            stream.total_out = total_out;
            // `adler` is deliberately *not* seeded: the facade hands the core `None` and
            // writes the caller's member back only when the core assigned one, so the
            // harness carries the value itself and adopts whatever came back.
            stream.data_type = data_type;

            ret = inflate(state, &mut stream, flush);
            calls = calls.saturating_add(1);

            let progress = stream.next_in != 0 || stream.next_out != 0;
            output.extend_from_slice(stream.written());
            consumed = consumed.saturating_add(stream.next_in);
            total_in = stream.total_in;
            total_out = stream.total_out;
            if let Some(value) = stream.adler {
                adler = value;
            }
            data_type = stream.data_type;
            msg = stream.msg;

            if matches!(
                ret,
                ReturnCode::STREAM_END
                    | ReturnCode::DATA_ERROR
                    | ReturnCode::MEM_ERROR
                    | ReturnCode::STREAM_ERROR
                    | ReturnCode::NEED_DICT
            ) {
                break;
            }
            idle = if progress { 0 } else { idle.saturating_add(1) };
            if idle >= 3 {
                break;
            }
        }

        Outcome {
            ret,
            output,
            msg,
            total_in,
            total_out,
            adler,
            data_type,
            calls,
        }
    }

    /// What one `inflate` call did.
    #[derive(Debug)]
    struct Call {
        ret: ReturnCode,
        output: Vec<u8>,
        data_type: i32,
        msg: Option<&'static str>,
        consumed: usize,
    }

    /// A decode in progress, so a test can assert on the state *between* calls.
    ///
    /// Keeping the input cursor is the whole point: `inflate` consumes a prefix of the
    /// slice it is handed, so a caller that re-offered the same slice would replay bytes
    /// the decoder had already absorbed.
    struct Session<'a> {
        data: &'a [u8],
        consumed: usize,
    }

    impl<'a> Session<'a> {
        fn new(data: &'a [u8]) -> Self {
            Self { data, consumed: 0 }
        }

        /// One `inflate` call offering all remaining input and `out_len` bytes of room.
        fn step<'s, A: Allocator<'s> + Copy>(
            &mut self,
            state: &mut InflateState<'s, A>,
            out_len: usize,
            flush: i32,
        ) -> Call {
            let mut buffer = vec![0_u8; out_len];
            let mut stream = InflateStream::new(&self.data[self.consumed..], &mut buffer);
            let ret = inflate(state, &mut stream, flush);
            self.consumed = self.consumed.saturating_add(stream.next_in);
            Call {
                ret,
                output: stream.written().to_vec(),
                data_type: stream.data_type,
                msg: stream.msg,
                consumed: stream.next_in,
            }
        }
    }

    /// The status of the very first `inflate` call, which is what `test/infcover.c`'s
    /// `inf()` asserts against its `err` argument (its L319).
    fn first_call(compressed: &[u8], window_bits: i32, in_step: usize, out_len: usize) -> Call {
        let mut state = inflate_init2(InflateConfig::new(window_bits), GlobalAllocator)
            .expect("windowBits should be accepted");
        let step = if in_step == 0 {
            compressed.len()
        } else {
            in_step.min(compressed.len())
        };
        let mut session = Session::new(&compressed[..step]);
        let call = session.step(&mut state, out_len, Z_NO_FLUSH);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
        call
    }

    /// An allocator that refuses every request, mirroring `mem_limit(&strm, 1)` at
    /// `test/infcover.c` L421.
    #[derive(Debug, Clone, Copy)]
    struct NoMemory;

    impl<'a> Allocator<'a> for NoMemory {
        fn id(&self) -> AllocatorId {
            // Any non-global identity will do; the three words stand in for the
            // `zalloc`/`zfree`/`opaque` triple a C caller would supply.
            AllocatorId::foreign(1, 2, 3)
        }

        fn opaque(&self) -> Opaque {
            Opaque::NULL
        }

        fn allocate_bytes(&self, _items: usize, _size: usize) -> Option<Buffer<'a, u8>> {
            None
        }

        fn allocate_u16s(&self, _items: usize) -> Option<Buffer<'a, u16>> {
            None
        }

        // Unreachable: nothing is ever handed out, so nothing comes back.
        fn release_foreign_bytes(&self, _block: ForeignBlock<'a, u8>) {}

        fn release_foreign_u16s(&self, _block: ForeignBlock<'a, u16>) {}
    }

    #[test]
    fn the_re_exported_fixed_tables_are_the_generated_ones() {
        // `inffixed.h` L10-L12 and its distance block: the first entries of each table,
        // transcribed from the generated header. A transcription error anywhere in the
        // 544 entries would show up as a decode failure much later and much less
        // legibly, which is why this test runs before any algorithmic one.
        assert_eq!(lenfix.len(), 512);
        assert_eq!(distfix.len(), 32);
        assert_eq!(lenfix[0], Code::new(96, 7, 0));
        assert_eq!(lenfix[1], Code::new(0, 8, 80));
        assert_eq!(lenfix[2], Code::new(0, 8, 16));
        assert_eq!(lenfix[3], Code::new(20, 8, 115));
        assert_eq!(distfix[0], Code::new(16, 5, 1));
        assert_eq!(distfix[1], Code::new(23, 5, 257));
        assert_eq!(distfix[2], Code::new(19, 5, 17));
        assert_eq!(distfix[3], Code::new(27, 5, 4097));
        // The re-export names the same constant, not a copy of it. Writing `super::lenfix`
        // here instead would trip `unused_qualifications`, precisely because rustc resolves
        // that path and the `fixed_tables` import to one and the same item -- which is the
        // fact these two lines exist to state. The aliased imports above are what keep the
        // statement checkable without suppressing the lint.
        assert_eq!(re_exported_lenfix, lenfix);
        assert_eq!(re_exported_distfix, distfix);
    }

    #[test]
    fn order_is_the_rfc_1951_code_length_permutation() {
        // `inflate.c` L491-L492, transcribed. RFC 1951 §3.2.7 lists the same sequence.
        assert_eq!(
            ORDER,
            [16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15]
        );
        assert_eq!(ORDER.len(), 19);
    }

    #[test]
    fn the_state_check_accepts_exactly_the_live_tag_range() {
        // `inflate.c` L95: `state->mode < HEAD || state->mode > SYNC`.
        assert!(!inflate_state_check(Mode::Head.as_raw()));
        assert!(!inflate_state_check(Mode::Sync.as_raw()));
        assert!(inflate_state_check(Mode::Head.as_raw() - 1));
        assert!(inflate_state_check(Mode::Sync.as_raw() + 1));
        assert!(inflate_state_check(0));
        assert!(inflate_state_check(i32::MIN));
        assert!(inflate_state_check(i32::MAX));
        for mode in Mode::ALL {
            assert!(!inflate_state_check(mode.as_raw()), "{mode:?}");
        }
    }

    #[test]
    fn zlib_streams_decode() {
        let outcome = decode(HELLO_ZLIB, 15, 0, 64);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, HELLO);
        assert_eq!(outcome.total_in, HELLO_ZLIB.len() as u64);
        assert_eq!(outcome.total_out, HELLO.len() as u64);
        // RFC 1950 §2.2: the trailer is the Adler-32 of the uncompressed data.
        assert_eq!(outcome.adler, adler32(1, HELLO));
    }

    #[test]
    fn raw_streams_decode() {
        let outcome = decode(HELLO_RAW, -15, 0, 64);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, HELLO);
        // A raw stream has no check value, so `adler` is left at its initial 0.
        assert_eq!(outcome.adler, 0);
    }

    #[test]
    fn gzip_streams_decode() {
        let outcome = decode(HELLO_GZIP, 31, 0, 64);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, HELLO);
        // RFC 1952 §2.3.1: the trailer is the CRC-32, then ISIZE.
        assert_eq!(outcome.adler, crate::crc32::crc32(0, HELLO));
    }

    #[test]
    fn window_bits_thirty_two_detects_either_container() {
        for stream in [HELLO_ZLIB, HELLO_GZIP] {
            let outcome = decode(stream, 47, 0, 64);
            assert_eq!(outcome.ret, ReturnCode::STREAM_END);
            assert_eq!(outcome.output, HELLO);
        }
        // A raw stream has no magic to detect, so auto-detection must reject it rather
        // than guess (`inflate.c` L524-L532).
        let outcome = decode(HELLO_RAW, 47, 0, 64);
        assert_eq!(outcome.ret, ReturnCode::DATA_ERROR);
    }

    #[test]
    fn window_bits_zero_takes_the_size_from_the_header() {
        // `test/infcover.c` L392: inf("8 99", "set window size from header", 0, 0, 0, Z_OK).
        let outcome = decode(HELLO_ZLIB, 0, 0, 64);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, HELLO);
    }

    #[test]
    fn every_window_exponent_in_range_is_accepted() {
        for bits in 8..=15 {
            assert!(inflate_init2(InflateConfig::new(bits), GlobalAllocator).is_ok());
            assert!(inflate_init2(InflateConfig::new(-bits), GlobalAllocator).is_ok());
            assert!(inflate_init2(InflateConfig::new(bits + 16), GlobalAllocator).is_ok());
            assert!(inflate_init2(InflateConfig::new(bits + 32), GlobalAllocator).is_ok());
        }
        // `inflate.c` L146-L147 and L160-L161.
        assert!(inflate_init2(InflateConfig::new(-16), GlobalAllocator).is_err());
        assert!(inflate_init2(InflateConfig::new(7), GlobalAllocator).is_err());
        assert!(inflate_init2(InflateConfig::new(16), GlobalAllocator).is_ok());
        // `test/infcover.c` L370: inf("", "bad window size", 0, 1, 0, Z_STREAM_ERROR).
        assert!(inflate_init2(InflateConfig::new(1), GlobalAllocator).is_err());
    }

    #[test]
    fn stored_blocks_decode() {
        let outcome = decode(STORED_ZLIB, 15, 0, 64);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, HELLO);
    }

    #[test]
    fn dynamic_huffman_blocks_decode() {
        let plain = repeat_plain();
        let outcome = decode(REPEAT_ZLIB, 15, 0, 4096);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, plain);
        assert_eq!(outcome.adler, adler32(1, &plain));
        // A dynamic block builds both tables in the state's own arena.
        assert!(outcome.output.len() > 512);
    }

    #[test]
    fn an_empty_stream_decodes_to_nothing() {
        let outcome = decode(EMPTY_ZLIB, 15, 0, 16);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert!(outcome.output.is_empty());
        assert_eq!(outcome.total_out, 0);
        assert_eq!(outcome.adler, 1);
    }

    #[test]
    fn a_distance_one_match_self_overlaps_forward() {
        // The RLE case: `offset == 1` with `length == 200`, so every byte written is the
        // source of the next. A bulk copy would produce a different result.
        let outcome = decode(RLE_RAW, -15, 0, 512);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, vec![b'Z'; 200]);
    }

    #[test]
    fn one_byte_at_a_time_matches_a_single_shot_decode() {
        let cases: [(&[u8], i32); 8] = [
            (HELLO_ZLIB, 15),
            (HELLO_RAW, -15),
            (HELLO_GZIP, 31),
            (HELLO_GZIP, 47),
            (EMPTY_ZLIB, 15),
            (STORED_ZLIB, 15),
            (REPEAT_ZLIB, 15),
            (RLE_RAW, -15),
        ];
        for (stream, bits) in cases {
            let bulk = decode(stream, bits, 0, 65_536);
            // One input byte and one output byte per call forces every state to suspend
            // and resume at least once, which is what proves `NEEDBITS` keeps enough
            // state behind and that the window is updated on every exit.
            let dribble = decode(stream, bits, 1, 1);
            assert_eq!(dribble.ret, bulk.ret, "ret for {bits}");
            assert_eq!(dribble.output, bulk.output, "output for {bits}");
            assert_eq!(dribble.total_in, bulk.total_in, "total_in for {bits}");
            assert_eq!(dribble.total_out, bulk.total_out, "total_out for {bits}");
            assert_eq!(dribble.adler, bulk.adler, "adler for {bits}");
            // The dribbled run really did take many more calls.
            assert!(dribble.calls > bulk.calls, "call count for {bits}");
        }
    }

    #[test]
    fn every_input_and_output_chunking_agrees() {
        let expected = repeat_plain();
        for in_step in [1_usize, 2, 3, 5, 7, 13, 61] {
            for out_step in [1_usize, 2, 6, 258, 259, 4096] {
                let outcome = decode(REPEAT_ZLIB, 15, in_step, out_step);
                assert_eq!(
                    outcome.ret,
                    ReturnCode::STREAM_END,
                    "in {in_step} out {out_step}"
                );
                assert_eq!(outcome.output, expected, "in {in_step} out {out_step}");
            }
        }
    }

    #[test]
    fn the_fast_path_and_the_slow_path_agree() {
        // `inflate.c` L915 takes the fast path only with at least 6 input and 258 output
        // bytes available, so these two runs exercise different decoders over the same
        // stream and must produce the same bytes.
        let expected = repeat_plain();
        let fast = decode(REPEAT_ZLIB, 15, 0, 4096);
        let slow = decode(REPEAT_ZLIB, 15, 5, 257);
        assert_eq!(fast.output, expected);
        assert_eq!(slow.output, expected);
        assert_eq!(fast.adler, slow.adler);
    }

    #[test]
    fn the_three_shapes_of_a_window_update_all_work() {
        // `test/infcover.c` L366-L369 and L613: four fixtures written to force each shape
        // of `updatewindow` -- first allocation, whole-window replacement, the split
        // update, and the wrap. The expected status is the one C's `inf()` asserts on the
        // first call.
        for (hex, what, step, bits, out) in [
            ("63 0", "force window allocation", 0_usize, -15_i32, 1_usize),
            ("63 18 5", "force window replacement", 0, -8, 259),
            (
                "63 18 68 30 d0 0 0",
                "force split window update",
                4,
                -8,
                259,
            ),
            ("63 18 5 40 c 0", "window wrap", 3, -8, 300),
        ] {
            let bytes = h2b(hex);
            let call = first_call(&bytes, bits, step, out);
            assert_eq!(call.ret, ReturnCode::OK, "{what}");
        }
    }

    #[test]
    fn the_fast_path_window_and_output_cases_all_work() {
        // `test/infcover.c` `cover_fast` (its L642-L659), verbatim. These fixtures were
        // written to reach each of `inflate_fast`'s six copy shapes and each of its three
        // rejections, and the fast path is entered only because `avail_out` is 258 or 259.
        for (hex, what, step, bits, out, expected) in [
            (
                "e5 e0 81 ad 6d cb b2 2c c9 01 1e 59 63 ae 7d ee fb 4d fd b5 35 41 68 ff 7f 0f 0 0 0",
                "fast length extra bits",
                0_usize,
                -8_i32,
                258_usize,
                ReturnCode::DATA_ERROR,
            ),
            (
                "25 fd 81 b5 6d 59 b6 6a 49 ea af 35 6 34 eb 8c b9 f6 b9 1e ef 67 49 50 fe ff ff 3f 0 0",
                "fast distance extra bits",
                0,
                -8,
                258,
                ReturnCode::DATA_ERROR,
            ),
            ("3 7e 0 0 0 0 0", "fast invalid distance code", 0, -8, 258, ReturnCode::DATA_ERROR),
            (
                "1b 7 0 0 0 0 0",
                "fast invalid literal/length code",
                0,
                -8,
                258,
                ReturnCode::DATA_ERROR,
            ),
            (
                "d c7 1 ae eb 38 c 4 41 a0 87 72 de df fb 1f b8 36 b1 38 5d ff ff 0",
                "fast 2nd level codes and too far back",
                0,
                -8,
                258,
                ReturnCode::DATA_ERROR,
            ),
            ("63 18 5 8c 10 8 0 0 0 0", "very common case", 0, -8, 259, ReturnCode::OK),
            (
                "63 60 60 18 c9 0 8 18 18 18 26 c0 28 0 29 0 0 0",
                "contiguous and wrap around window",
                6,
                -8,
                259,
                ReturnCode::OK,
            ),
            ("63 0 3 0 0 0 0 0", "copy direct from output", 0, -8, 259, ReturnCode::STREAM_END),
        ] {
            let bytes = h2b(hex);
            let call = first_call(&bytes, bits, step, out);
            assert_eq!(call.ret, expected, "{what}");
        }
    }

    #[test]
    fn the_awkward_but_valid_streams_all_decode() {
        // The `err == 0` fixtures of `test/infcover.c` `cover_inflate` (its L581, L584 and
        // L599-L608), which are the cases written to reach a specific decoder path rather
        // than a specific error. `try()` drives them raw with `Z_TREES`.
        for hex in [
            "3 0",
            "1 1 0 fe ff 0",
            "5 c0 21 d 0 0 0 80 b0 fe 6d 2f 91 6c",
            "5 e0 81 91 24 cb b2 2c 49 e2 f 2e 8b 9a 47 56 9f fb fe ec d2 ff 1f",
            "ed c0 1 1 0 0 0 40 20 ff 57 1b 42 2c 4f",
            "ed cf c1 b1 2c 47 10 c4 30 fa 6f 35 1d 1 82 59 3d fb be 2e 2a fc f c",
            "ed c0 81 0 0 0 0 80 a0 fd a9 17 a9 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 6",
        ] {
            let bytes = h2b(hex);
            let outcome = decode_with_flush(&bytes, -15, 0, bytes.len() << 3, Z_TREES);
            assert!(
                !matches!(
                    outcome.ret,
                    ReturnCode::DATA_ERROR | ReturnCode::MEM_ERROR | ReturnCode::STREAM_ERROR
                ),
                "{hex}: {:?} {:?}",
                outcome.ret,
                outcome.msg
            );
        }
        // `test/infcover.c` L612: the fast path returning straight to `TYPE`.
        let bytes = h2b("2 8 20 80 0 3 0");
        let call = first_call(&bytes, -15, 0, 258);
        assert_eq!(call.ret, ReturnCode::STREAM_END);
    }

    #[test]
    fn every_flush_mode_decodes_the_same_bytes() {
        // `zlib.h` L463-L472: on the decode side `flush` affects only the return code,
        // except that `Z_BLOCK` and `Z_TREES` additionally stop early.
        for flush in [Z_NO_FLUSH, Z_PARTIAL_FLUSH, Z_SYNC_FLUSH, Z_FULL_FLUSH] {
            let outcome = decode_with_flush(HELLO_ZLIB, 15, 0, 64, flush);
            assert_eq!(outcome.ret, ReturnCode::STREAM_END, "flush {flush}");
            assert_eq!(outcome.output, HELLO, "flush {flush}");
        }
        // An undocumented `flush` must behave like `Z_NO_FLUSH`, not be rejected.
        for flush in [-7_i32, 7, 99, i32::MIN, i32::MAX] {
            let outcome = decode_with_flush(HELLO_ZLIB, 15, 0, 64, flush);
            assert_eq!(outcome.ret, ReturnCode::STREAM_END, "flush {flush}");
            assert_eq!(outcome.output, HELLO, "flush {flush}");
        }
    }

    #[test]
    fn z_block_stops_at_a_block_boundary() {
        let mut state = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        let mut session = Session::new(HELLO_ZLIB);
        // First call: the header, then a stop at the start of the first block.
        let call = session.step(&mut state, 64, Z_BLOCK);
        assert_eq!(call.ret, ReturnCode::OK);
        assert!(call.output.is_empty());
        assert_eq!(call.consumed, 2);
        // `inflate.c` L1148 adds 128 when the stream suspended in `TYPE`.
        assert_eq!(state.mode, Mode::Type);
        assert_eq!(call.data_type & 128, 128);
        // The second call takes the L499 shortcut and actually decodes; without it the
        // stream would stop at the same boundary for ever.
        let call = session.step(&mut state, 64, Z_BLOCK);
        assert_eq!(call.output, HELLO);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);

        // Driven to completion, `Z_BLOCK` still yields the whole payload.
        let outcome = decode_with_flush(HELLO_ZLIB, 15, 0, 64, Z_BLOCK);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, HELLO);
    }

    #[test]
    fn z_trees_stops_after_a_fixed_block_header() {
        // The fixed-block early exit, `inflate.c` L731-L735.
        let mut state = inflate_init2(InflateConfig::new(-15), GlobalAllocator).unwrap();
        let mut session = Session::new(HELLO_RAW);
        let call = session.step(&mut state, 64, Z_TREES);
        assert_eq!(call.ret, ReturnCode::OK);
        assert!(call.output.is_empty());
        // `inflate.c` L1149 adds 256 when the stream suspended in `LEN_` or `COPY_`.
        assert_eq!(state.mode, Mode::LenFirst);
        assert_eq!(call.data_type & 256, 256);

        // The second call decodes the block and then stops again, at the block boundary
        // this time, because `Z_TREES` implies `Z_BLOCK` (`inflate.c` L709). It is
        // `Z_OK`, not `Z_STREAM_END`: the trailer has not been looked at yet.
        let call = session.step(&mut state, 64, Z_TREES);
        assert_eq!(call.ret, ReturnCode::OK);
        assert_eq!(call.output, HELLO);
        assert_eq!(state.mode, Mode::Type);
        assert_eq!(call.data_type & 128, 128);
        assert_eq!(call.data_type & 64, 64, "the block was the last one");

        // The third call takes the L499 shortcut, sees `last`, and finishes.
        let call = session.step(&mut state, 64, Z_TREES);
        assert_eq!(call.ret, ReturnCode::STREAM_END);
        assert!(call.output.is_empty());
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn z_trees_stops_after_a_stored_block_header() {
        // The stored-block early exit, `inflate.c` L760-L761.
        let mut state = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        let mut session = Session::new(STORED_ZLIB);
        let mut stopped = false;
        for _ in 0..8 {
            let call = session.step(&mut state, 64, Z_TREES);
            assert!(
                !matches!(call.ret, ReturnCode::DATA_ERROR | ReturnCode::STREAM_ERROR),
                "{:?} {:?}",
                call.ret,
                call.msg
            );
            if state.mode == Mode::CopyBlock {
                assert_eq!(call.data_type & 256, 256);
                assert!(call.output.is_empty(), "no data before the copy starts");
                stopped = true;
                break;
            }
            if call.ret == ReturnCode::STREAM_END {
                break;
            }
        }
        assert!(stopped, "Z_TREES did not stop at COPY_");
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn z_trees_stops_after_a_dynamic_block_header() {
        // The dynamic-block early exit, `inflate.c` L909-L910.
        let mut state = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        let mut session = Session::new(REPEAT_ZLIB);
        let mut stopped = false;
        for _ in 0..8 {
            let call = session.step(&mut state, 4096, Z_TREES);
            assert!(
                !matches!(call.ret, ReturnCode::DATA_ERROR | ReturnCode::STREAM_ERROR),
                "{:?} {:?}",
                call.ret,
                call.msg
            );
            if state.mode == Mode::LenFirst {
                assert_eq!(call.data_type & 256, 256);
                assert!(call.output.is_empty(), "no data before the codes are used");
                stopped = true;
                break;
            }
            if call.ret == ReturnCode::STREAM_END {
                break;
            }
        }
        assert!(stopped, "Z_TREES did not stop at LEN_");
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);

        // And driven to completion it still produces every byte.
        let outcome = decode_with_flush(REPEAT_ZLIB, 15, 0, 4096, Z_TREES);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, repeat_plain());
        assert!(outcome.calls > 3, "Z_TREES should have stopped repeatedly");
    }

    #[test]
    fn z_finish_never_returns_ok() {
        // `inflate.c` L1150-L1151 and the comment at L465-L472.
        let mut state = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        let mut session = Session::new(HELLO_ZLIB);
        // Too little room to finish: `Z_OK` would be the natural answer, and is forbidden.
        let call = session.step(&mut state, 4, Z_FINISH);
        assert_eq!(call.ret, ReturnCode::BUF_ERROR);
        assert_eq!(call.output.len(), 4);
        // The stream is still perfectly usable; `Z_BUF_ERROR` is not fatal.
        let call = session.step(&mut state, 64, Z_FINISH);
        assert_eq!(call.ret, ReturnCode::STREAM_END);
        assert_eq!(call.output, &HELLO[4..]);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);

        // With room from the start, `Z_FINISH` reaches the end in one call.
        let mut state = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        let mut session = Session::new(HELLO_ZLIB);
        let call = session.step(&mut state, 64, Z_FINISH);
        assert_eq!(call.ret, ReturnCode::STREAM_END);
        assert_eq!(call.output, HELLO);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn no_progress_is_a_buffer_error() {
        let mut state = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        // Neither input nor output: nothing can happen, so `Z_BUF_ERROR`.
        let mut nothing = Session::new(&[]);
        assert_eq!(
            nothing.step(&mut state, 0, Z_NO_FLUSH).ret,
            ReturnCode::BUF_ERROR
        );
        // Input but no room: the header is consumed, which is progress.
        let mut session = Session::new(HELLO_ZLIB);
        let call = session.step(&mut state, 0, Z_NO_FLUSH);
        assert_eq!(call.ret, ReturnCode::OK);
        assert!(call.consumed > 0);
        // Offering no room again makes no progress at all.
        let call = session.step(&mut state, 0, Z_NO_FLUSH);
        assert_eq!(call.ret, ReturnCode::BUF_ERROR);
        assert_eq!(call.consumed, 0);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn data_type_reports_the_accumulator_and_the_last_block_flag() {
        // `inflate.c` L1147-L1149: bits + 64 (last block) + 128 (TYPE) + 256 (LEN_/COPY_).
        let mut state = inflate_init2(InflateConfig::new(-15), GlobalAllocator).unwrap();
        let mut session = Session::new(HELLO_RAW);
        let call = session.step(&mut state, 64, Z_NO_FLUSH);
        assert_eq!(call.ret, ReturnCode::STREAM_END);
        assert_eq!(call.output, HELLO);
        // The stream's single block is final, so bit 6 is set; it ended in `DONE`, which
        // contributes neither 128 nor 256.
        assert_eq!(call.data_type & 64, 64);
        assert_eq!(call.data_type & 128, 0);
        assert_eq!(call.data_type & 256, 0);
        // The low six bits are the accumulator occupancy, which cannot exceed 32.
        assert!((call.data_type & 63) <= 32, "{}", call.data_type);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);

        // A non-final block leaves bit 6 clear: this stream's first block is not the last.
        let mut state = inflate_init2(InflateConfig::new(-15), GlobalAllocator).unwrap();
        let mut session = Session::new(SYNC_RAW);
        let call = session.step(&mut state, 5, Z_BLOCK);
        assert_eq!(call.output, b"first");
        assert_eq!(call.data_type & 64, 0);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);

        // The same three flags survive a chunked drive, because the epilogue recomposes
        // `data_type` from scratch on every call rather than accumulating into it.
        for (fixture, bits) in [(HELLO_RAW, -15), (HELLO_ZLIB, 15), (HELLO_GZIP, 31)] {
            let outcome = decode(fixture, bits, 1, 1);
            assert_eq!(outcome.ret, ReturnCode::STREAM_END);
            assert_eq!(outcome.data_type & 64, 64, "the final block was seen");
            assert_eq!(outcome.data_type & 128, 0, "the stream ended in DONE");
            assert_eq!(outcome.data_type & 256, 0);
            assert!((outcome.data_type & 63) <= 32);
        }
    }

    #[test]
    fn every_rejection_reports_the_reference_message() {
        // The `err != 0` fixtures of `test/infcover.c` `cover_inflate` (its L580-L602),
        // verbatim, together with the message C asserts for each. `try()` drives the
        // first fourteen raw and the last two with `windowBits` 47.
        let raw_cases: [(&str, &str); 12] = [
            ("0 0 0 0 0", MSG_INVALID_STORED_BLOCK_LENGTHS),
            ("6", MSG_INVALID_BLOCK_TYPE),
            ("fc 0 0", MSG_TOO_MANY_SYMBOLS),
            ("4 0 fe ff", MSG_INVALID_CODE_LENGTHS_SET),
            // ★ The two distinct sites that emit the same text: `have == 0` with a
            // symbol-16 repeat (`inflate.c` L840-L841), and a repeat that overruns the
            // two alphabets (L864-L865).
            ("4 0 24 49 0", MSG_INVALID_BIT_LENGTH_REPEAT),
            ("4 0 24 e9 ff ff", MSG_INVALID_BIT_LENGTH_REPEAT),
            ("4 0 24 e9 ff 6d", MSG_MISSING_END_OF_BLOCK),
            (
                "4 80 49 92 24 49 92 24 71 ff ff 93 11 0",
                MSG_INVALID_LITERAL_LENGTHS_SET,
            ),
            (
                "4 80 49 92 24 49 92 24 f b4 ff ff c3 84",
                MSG_INVALID_DISTANCES_SET,
            ),
            (
                "4 c0 81 8 0 0 0 0 20 7f eb b 0 0",
                MSG_INVALID_LITERAL_LENGTH_CODE,
            ),
            ("2 7e ff ff", MSG_INVALID_DISTANCE_CODE),
            (
                "c c0 81 0 0 0 0 0 90 ff 6b 4 0",
                MSG_INVALID_DISTANCE_TOO_FAR_BACK,
            ),
        ];
        for (hex, expected) in raw_cases {
            let bytes = h2b(hex);
            let outcome = decode_with_flush(&bytes, -15, 0, bytes.len() << 3, Z_TREES);
            assert_eq!(outcome.ret, ReturnCode::DATA_ERROR, "{hex}");
            assert_eq!(outcome.msg, Some(expected), "{hex}");
        }

        // The two trailer mismatches, which only `inflate()` can reach (`try(..., -1)`
        // uses `windowBits` 47).
        let wrapped_cases: [(&str, &str); 2] = [
            (
                "1f 8b 8 0 0 0 0 0 0 0 3 0 0 0 0 1",
                MSG_INCORRECT_DATA_CHECK,
            ),
            (
                "1f 8b 8 0 0 0 0 0 0 0 3 0 0 0 0 0 0 0 0 1",
                MSG_INCORRECT_LENGTH_CHECK,
            ),
        ];
        for (hex, expected) in wrapped_cases {
            let bytes = h2b(hex);
            let outcome = decode_with_flush(&bytes, 47, 0, bytes.len() << 3, Z_TREES);
            assert_eq!(outcome.ret, ReturnCode::DATA_ERROR, "{hex}");
            assert_eq!(outcome.msg, Some(expected), "{hex}");
        }
    }

    #[test]
    fn every_message_is_spelled_the_way_the_reference_spells_it() {
        // Transcribed from `inflate.c`, not from the constants, so that a typo in either
        // one shows up here. The two-hyphen form of the end-of-block message is
        // deliberate (`inflate.c` L880).
        assert_eq!(MSG_INVALID_BLOCK_TYPE, "invalid block type");
        assert_eq!(
            MSG_INVALID_STORED_BLOCK_LENGTHS,
            "invalid stored block lengths"
        );
        assert_eq!(MSG_TOO_MANY_SYMBOLS, "too many length or distance symbols");
        assert_eq!(MSG_INVALID_CODE_LENGTHS_SET, "invalid code lengths set");
        assert_eq!(MSG_INVALID_BIT_LENGTH_REPEAT, "invalid bit length repeat");
        assert_eq!(
            MSG_MISSING_END_OF_BLOCK,
            "invalid code -- missing end-of-block"
        );
        assert_eq!(
            MSG_INVALID_LITERAL_LENGTHS_SET,
            "invalid literal/lengths set"
        );
        assert_eq!(MSG_INVALID_DISTANCES_SET, "invalid distances set");
        assert_eq!(MSG_INCORRECT_DATA_CHECK, "incorrect data check");
        assert_eq!(MSG_INCORRECT_LENGTH_CHECK, "incorrect length check");
        // The three shared with `inffast.c` are imported, so these assertions also prove
        // the two decode paths cannot report different text for the same fault.
        assert_eq!(
            MSG_INVALID_LITERAL_LENGTH_CODE,
            "invalid literal/length code"
        );
        assert_eq!(MSG_INVALID_DISTANCE_CODE, "invalid distance code");
        assert_eq!(
            MSG_INVALID_DISTANCE_TOO_FAR_BACK,
            "invalid distance too far back"
        );
    }

    #[test]
    fn a_reserved_block_type_still_consumes_its_bits() {
        // `inflate.c` L741-L745: the `default` arm falls out of the inner switch into the
        // shared `DROPBITS(2)`, so three bits are consumed even though the block is
        // rejected. An implementation that returned early would leave two bits behind.
        let bytes = h2b("6");
        let mut state = inflate_init2(InflateConfig::new(-15), GlobalAllocator).unwrap();
        let mut session = Session::new(&bytes);
        let call = session.step(&mut state, 16, Z_NO_FLUSH);
        assert_eq!(call.ret, ReturnCode::DATA_ERROR);
        assert_eq!(call.msg, Some(MSG_INVALID_BLOCK_TYPE));
        assert_eq!(state.bits, 5, "8 pulled, 3 dropped");
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn a_data_error_is_reported_again_on_every_later_call() {
        // `inflate.c` L1114-L1116: `BAD` is sticky until the stream is reset.
        let bytes = h2b("6");
        let mut state = inflate_init2(InflateConfig::new(-15), GlobalAllocator).unwrap();
        let mut session = Session::new(&bytes);
        for _ in 0..3 {
            let call = session.step(&mut state, 16, Z_NO_FLUSH);
            assert_eq!(call.ret, ReturnCode::DATA_ERROR);
            assert_eq!(state.mode, Mode::Bad);
        }
        // A reset clears it.
        let reset = inflate_reset(&mut state);
        assert_eq!(reset.total_in, 0);
        assert_eq!(state.mode, Mode::Head);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn a_zlib_trailer_is_big_endian() {
        // RFC 1950 §2.2 stores the Adler-32 most significant byte first, so reversing the
        // four trailer bytes must be rejected -- and if the implementation had the branch the wrong
        // way round, the reversed stream would be the one it accepted.
        let mut swapped = HELLO_ZLIB.to_vec();
        let len = swapped.len();
        swapped[len - 4..].reverse();
        let outcome = decode(&swapped, 15, 0, 64);
        assert_eq!(outcome.ret, ReturnCode::DATA_ERROR);
        assert_eq!(outcome.msg, Some(MSG_INCORRECT_DATA_CHECK));
        // The unswapped original is accepted, so the fixture differs only in byte order.
        assert_eq!(decode(HELLO_ZLIB, 15, 0, 64).ret, ReturnCode::STREAM_END);
    }

    #[test]
    fn a_gzip_trailer_is_little_endian() {
        // RFC 1952 §2.3.1 stores the CRC-32 least significant byte first, which is the
        // order the accumulator already holds -- so here it is the *reversal* that must
        // fail, for the opposite reason to the zlib case above.
        let mut swapped = HELLO_GZIP.to_vec();
        let len = swapped.len();
        swapped[len - 8..len - 4].reverse();
        let outcome = decode(&swapped, 31, 0, 64);
        assert_eq!(outcome.ret, ReturnCode::DATA_ERROR);
        assert_eq!(outcome.msg, Some(MSG_INCORRECT_DATA_CHECK));
        assert_eq!(decode(HELLO_GZIP, 31, 0, 64).ret, ReturnCode::STREAM_END);
    }

    #[test]
    fn a_gzip_isize_mismatch_is_rejected() {
        // `inflate.c` L1098-L1104. The last four bytes are ISIZE; bumping them makes the
        // trailer describe a different length than was produced.
        let mut broken = HELLO_GZIP.to_vec();
        let len = broken.len();
        broken[len - 4] = broken[len - 4].wrapping_add(1);
        let outcome = decode(&broken, 31, 0, 64);
        assert_eq!(outcome.ret, ReturnCode::DATA_ERROR);
        assert_eq!(outcome.msg, Some(MSG_INCORRECT_LENGTH_CHECK));
        // The payload itself was produced before the trailer was examined.
        assert_eq!(outcome.output, HELLO);
    }

    #[test]
    fn validation_can_be_turned_off_and_on() {
        // `inflate.c` L1385-L1394. With validation off, a corrupt trailer is ignored.
        let mut swapped = HELLO_ZLIB.to_vec();
        let len = swapped.len();
        swapped[len - 4..].reverse();

        let mut state = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        assert_eq!(inflate_validate(&mut state, false), ReturnCode::OK);
        let outcome = drive_state(&mut state, &swapped, 0, 64, Z_NO_FLUSH);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, HELLO);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);

        // Turned back on, the same stream is rejected again.
        let mut state = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        assert_eq!(inflate_validate(&mut state, false), ReturnCode::OK);
        assert_eq!(inflate_validate(&mut state, true), ReturnCode::OK);
        let outcome = drive_state(&mut state, &swapped, 0, 64, Z_NO_FLUSH);
        assert_eq!(outcome.ret, ReturnCode::DATA_ERROR);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);

        // A raw stream is never promoted, because it has no check value to verify.
        let mut state = inflate_init2(InflateConfig::new(-15), GlobalAllocator).unwrap();
        assert_eq!(inflate_validate(&mut state, true), ReturnCode::OK);
        assert!(!state.wrap.verifies_check_value());
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn a_dictionary_stream_asks_for_its_dictionary_and_then_decodes() {
        let mut state = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        let mut buffer = vec![0_u8; 64];
        let mut stream = InflateStream::new(DICT_ZLIB, &mut buffer);

        // `inflate.c` L700-L704: the stream stops and asks, without running the epilogue.
        let ret = inflate(&mut state, &mut stream, Z_NO_FLUSH);
        assert_eq!(ret, ReturnCode::NEED_DICT);
        assert_eq!(state.mode, Mode::Dict);
        // L696 published the dictionary id, and that is how the caller knows which one to
        // supply (`zlib.h` L903-L906).
        assert_eq!(stream.adler, Some(adler32(1, DICTIONARY)));
        // The bare return skipped the epilogue, so the totals were not credited.
        assert_eq!(stream.total_in, 0);
        assert_eq!(stream.total_out, 0);
        let consumed = stream.next_in;
        assert_eq!(consumed, 6, "two header bytes and the four-byte DICTID");

        // A wrong dictionary is rejected by its Adler-32 (L1203-L1204).
        assert_eq!(
            inflate_set_dictionary(&mut state, b"not the dictionary"),
            ReturnCode::DATA_ERROR
        );
        assert!(!state.havedict);

        // The right one is accepted and decoding resumes.
        assert_eq!(
            inflate_set_dictionary(&mut state, DICTIONARY),
            ReturnCode::OK
        );
        assert!(state.havedict);
        let outcome = drive_state(&mut state, &DICT_ZLIB[consumed..], 0, 64, Z_NO_FLUSH);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, HELLO);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn a_dictionary_may_only_be_set_when_one_is_wanted() {
        // `inflate.c` L1196-L1197 and `test/infcover.c` L362-L363.
        let mut state = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        assert_eq!(
            inflate_set_dictionary(&mut state, &[]),
            ReturnCode::STREAM_ERROR
        );
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);

        // A raw stream accepts one at any time, with no id to check, and it becomes
        // history the first match can reach back into.
        let mut state = inflate_init2(InflateConfig::new(-15), GlobalAllocator).unwrap();
        assert_eq!(
            inflate_set_dictionary(&mut state, DICTIONARY),
            ReturnCode::OK
        );
        assert_eq!(state.whave, DICTIONARY.len() as u32);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn get_dictionary_returns_the_history_in_stream_order() {
        // `inflate.c` L1167-L1185.
        let mut state = inflate_init2(InflateConfig::new(-15), GlobalAllocator).unwrap();
        let mut length = 0_u32;
        // Before anything has been decoded there is no history at all.
        assert_eq!(
            inflate_get_dictionary(&state, None, Some(&mut length)),
            ReturnCode::OK
        );
        assert_eq!(length, 0);

        // After a decode the window holds everything produced.
        let outcome = drive_state(&mut state, HELLO_RAW, 1, 1, Z_NO_FLUSH);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        let mut history = vec![0_u8; 64];
        assert_eq!(
            inflate_get_dictionary(
                &state,
                Some(&mut OutputRegion::init(&mut history)),
                Some(&mut length)
            ),
            ReturnCode::OK
        );
        assert_eq!(length as usize, HELLO.len());
        assert_eq!(&history[..HELLO.len()], HELLO);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn get_dictionary_reassembles_a_wrapped_window() {
        // A 256-byte window under 200 bytes of output does not wrap; the RLE stream is
        // decoded twice into the same window so that `wnext` passes the end and the
        // read-back has to splice the two halves (`inflate.c` L1177-L1180).
        let mut state = inflate_init2(InflateConfig::new(-8), GlobalAllocator).unwrap();
        let first = drive_state(&mut state, RLE_RAW, 0, 64, Z_NO_FLUSH);
        assert_eq!(first.ret, ReturnCode::STREAM_END);
        let reset = inflate_reset_keep(&mut state);
        assert!(reset.adler.is_none(), "a raw stream leaves adler alone");
        let second = drive_state(&mut state, RLE_RAW, 0, 64, Z_NO_FLUSH);
        assert_eq!(second.ret, ReturnCode::STREAM_END);

        // 400 bytes have passed through a 256-byte window, so it has wrapped and is full.
        assert_eq!(state.wsize, 256);
        assert_eq!(state.whave, 256);
        assert!(state.wnext > 0 && state.wnext < 256, "{}", state.wnext);

        let mut history = vec![0_u8; 512];
        let mut length = 0_u32;
        assert_eq!(
            inflate_get_dictionary(
                &state,
                Some(&mut OutputRegion::init(&mut history)),
                Some(&mut length)
            ),
            ReturnCode::OK
        );
        assert_eq!(length, 256);
        // Every byte of both runs was the same, so the spliced history must be uniform;
        // a splice in the wrong order would still be uniform, so also check the length.
        assert!(history[..256].iter().all(|&byte| byte == b'Z'));
        assert!(history[256..].iter().all(|&byte| byte == 0));
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn get_dictionary_clamps_to_a_short_buffer() {
        // C has no length for `dictionary` and requires 32768 bytes; this implementation truncates
        // rather than overflowing, and still reports the full `whave`.
        let mut state = inflate_init2(InflateConfig::new(-15), GlobalAllocator).unwrap();
        assert_eq!(
            drive_state(&mut state, HELLO_RAW, 0, 64, Z_NO_FLUSH).ret,
            ReturnCode::STREAM_END
        );
        let mut history = vec![0xff_u8; 4];
        let mut length = 0_u32;
        assert_eq!(
            inflate_get_dictionary(
                &state,
                Some(&mut OutputRegion::init(&mut history)),
                Some(&mut length)
            ),
            ReturnCode::OK
        );
        assert_eq!(length as usize, HELLO.len());
        assert_eq!(history, &HELLO[..4]);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn syncsearch_finds_the_pattern_and_remembers_partial_matches() {
        // `inflate.c` L1244-L1262.
        let mut have = 0_u32;
        assert_eq!(syncsearch(&mut have, &[0x00, 0x00, 0xff, 0xff]), 4);
        assert_eq!(have, 4);

        // Split across two calls, the partial match is carried in `have`.
        let mut have = 0_u32;
        assert_eq!(syncsearch(&mut have, &[0x00, 0x00]), 2);
        assert_eq!(have, 2);
        assert_eq!(syncsearch(&mut have, &[0xff, 0xff, 0x11]), 2);
        assert_eq!(have, 4);

        // A non-zero byte in the wrong place clears the count outright.
        let mut have = 0_u32;
        assert_eq!(syncsearch(&mut have, &[0x00, 0x01]), 2);
        assert_eq!(have, 0);

        // ★ A *zero* byte where an `0xff` was expected re-seeds rather than clearing:
        // `got = 4 - got`. With two zeros seen, a third zero leaves the count at 2,
        // because the last two zeros are themselves a fresh `00 00`.
        let mut have = 0_u32;
        assert_eq!(syncsearch(&mut have, &[0x00, 0x00, 0x00]), 3);
        assert_eq!(have, 2, "`got = 4 - got`, not `got = 0`");
        // And with three matched, a zero leaves one.
        let mut have = 3_u32;
        assert_eq!(syncsearch(&mut have, &[0x00]), 1);
        assert_eq!(have, 1);

        // The pattern found late in a buffer returns the bytes read, not the length.
        let mut have = 0_u32;
        let haystack = [0x11, 0x22, 0x00, 0x00, 0xff, 0xff, 0x33, 0x44];
        assert_eq!(syncsearch(&mut have, &haystack), 6);
        assert_eq!(have, 4);

        // Absent, it reports the whole buffer and stops looking only when it runs out.
        let mut have = 0_u32;
        assert_eq!(syncsearch(&mut have, &[0x11; 9]), 9);
        assert_eq!(have, 0);
        // An empty buffer is a no-op.
        let mut have = 2_u32;
        assert_eq!(syncsearch(&mut have, &[]), 0);
        assert_eq!(have, 2);
    }

    #[test]
    fn sync_restarts_at_the_next_flush_point() {
        // [`SYNC_RAW`] carries a `Z_SYNC_FLUSH` point whose `00 00 ff ff` marker ends at
        // offset 11, so synchronising from the start of the stream must skip the whole
        // first block and resume at the second (`inflate.c` L1264-L1309).
        let mut state = inflate_init2(InflateConfig::new(-15), GlobalAllocator).unwrap();
        let mut buffer = vec![0_u8; 64];
        let mut stream = InflateStream::new(SYNC_RAW, &mut buffer);

        // Non-zero totals prove the save/restore around the internal reset: without it,
        // `inflateReset` would zero both.
        stream.total_in = 17;
        stream.total_out = 23;
        assert_eq!(inflate_sync(&mut state, &mut stream), ReturnCode::OK);
        assert_eq!(state.mode, Mode::Type);
        assert_eq!(
            stream.next_in, 11,
            "consumed up to and including the marker"
        );
        assert_eq!(stream.total_in, 17 + 11, "credited, not reset");
        assert_eq!(stream.total_out, 23, "preserved across the reset");
        // ★ L1299-L1300: no header had been seen, so the remainder is treated as raw.
        assert!(state.wrap.is_raw());

        let rest = SYNC_RAW[stream.next_in..].to_vec();
        let outcome = drive_state(&mut state, &rest, 0, 64, Z_NO_FLUSH);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, b"second");
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn sync_clears_only_the_check_bit_once_a_header_has_been_seen() {
        // ★ The other half of `inflate.c` L1299-L1302: a stream that has already read a
        // zlib header keeps its wrap bits and loses only the validation bit, because a
        // check value computed across a hole would be meaningless.
        let mut state = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        let mut session = Session::new(HELLO_ZLIB);
        let call = session.step(&mut state, 64, Z_NO_FLUSH);
        assert_eq!(call.ret, ReturnCode::STREAM_END);
        assert_eq!(state.flags, 0, "a zlib header was seen");
        assert!(state.wrap.verifies_check_value());

        let marker = [0x00_u8, 0x00, 0xff, 0xff];
        let mut buffer = vec![0_u8; 1];
        let mut stream = InflateStream::new(&marker, &mut buffer);
        assert_eq!(inflate_sync(&mut state, &mut stream), ReturnCode::OK);
        assert!(!state.wrap.is_raw(), "the zlib wrap bit survives");
        assert!(!state.wrap.verifies_check_value(), "validation is dropped");
        assert_eq!(state.flags, 0, "flags are restored after the reset");
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn sync_reports_the_reference_statuses() {
        // `inflate.c` L1274: nothing to search at all.
        let mut state = inflate_init2(InflateConfig::new(-15), GlobalAllocator).unwrap();
        let mut buffer = vec![0_u8; 1];
        let mut empty = InflateStream::new(&[], &mut buffer);
        assert_eq!(inflate_sync(&mut state, &mut empty), ReturnCode::BUF_ERROR);

        // `inflate.c` L1298: the pattern is not there.
        let absent = [0x80_u8, 0x11];
        let mut buffer = vec![0_u8; 1];
        let mut stream = InflateStream::new(&absent, &mut buffer);
        assert_eq!(
            inflate_sync(&mut state, &mut stream),
            ReturnCode::DATA_ERROR
        );
        assert_eq!(state.mode, Mode::Sync);
        assert_eq!(stream.next_in, absent.len(), "all of it was searched");

        // ★ `inflate()` cannot resume from `SYNC`: `inflate.c` L1119-L1122 is a bare
        // return, and `test/infcover.c` L434 asserts exactly this.
        let mut buffer = vec![0_u8; 1];
        let mut stream = InflateStream::new(&absent, &mut buffer);
        assert_eq!(
            inflate(&mut state, &mut stream, Z_NO_FLUSH),
            ReturnCode::STREAM_ERROR
        );

        // `test/infcover.c` L437: the pattern found on a second attempt.
        let pattern = [0x00_u8, 0x00, 0xff, 0xff];
        let mut buffer = vec![0_u8; 1];
        let mut stream = InflateStream::new(&pattern, &mut buffer);
        assert_eq!(inflate_sync(&mut state, &mut stream), ReturnCode::OK);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn sync_point_is_an_empty_stored_block_boundary() {
        // `inflate.c` L1320-L1326: true only in `STORED` with a byte-aligned accumulator,
        // and the reference's comment at L1312-L1318 explains what it is for -- PPP emits
        // `Z_SYNC_FLUSH` but strips the empty stored block's length bytes, so on the
        // decoding side it checks that the input packet ended waiting for exactly those.
        let mut state = inflate_init2(InflateConfig::new(-15), GlobalAllocator).unwrap();
        assert!(!inflate_sync_point(&state));

        // Bytes 0..7 of [`SYNC_RAW`] are the first block plus the empty stored block's
        // three header bits; the `00 00 ff ff` length pair starts at offset 7. Truncating
        // there reproduces PPP's stripped packet exactly.
        let stripped = &SYNC_RAW[..7];
        let mut session = Session::new(stripped);
        let call = session.step(&mut state, 64, Z_NO_FLUSH);
        assert_eq!(call.output, b"first");
        assert_eq!(state.mode, Mode::Stored);
        assert_eq!(state.bits, 0, "BYTEBITS aligned the accumulator");
        assert!(inflate_sync_point(&state));

        // Supplying the stripped bytes moves the stream on, and it is no longer at a
        // sync point.
        let rest = &SYNC_RAW[7..];
        let outcome = drive_state(&mut state, rest, 0, 64, Z_NO_FLUSH);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, b"second");
        assert!(!inflate_sync_point(&state));
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn a_copied_state_decodes_the_rest_independently() {
        // `inflate.c` L1328-L1367, and the use `zlib.h` L970-L976 documents: scan ahead
        // speculatively while keeping the ability to resume from the split point.
        // ★ `DYNAMIC_ZLIB`, not `REPEAT_ZLIB`: only a dynamic block puts entries in the
        // arena, which is what makes the "no pointer rebasing" assertion below meaningful.
        let expected = dynamic_plain();
        let mut state = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        let mut session = Session::new(DYNAMIC_ZLIB);

        // Decode part of the stream, far enough that dynamic tables have been built and
        // the window holds history.
        let first = session.step(&mut state, 300, Z_NO_FLUSH);
        assert_eq!(first.ret, ReturnCode::OK);
        assert_eq!(first.output.len(), 300);
        assert!(state.codes_used() > 0, "dynamic tables were built");
        assert!(state.whave > 0, "the window has history");

        let split = session.consumed;
        let mut copy = inflate_copy(&state, GlobalAllocator).unwrap();
        // ★ No pointer rebasing was needed: the copy's table selections are offsets into
        // its own arena, so they already point at the right entries.
        assert_eq!(copy.codes_used(), state.codes_used());
        assert_eq!(copy.whave, state.whave);
        assert_eq!(copy.mode, state.mode);

        // Both continue from the split point and produce the same bytes.
        let from_original = drive_state(&mut state, &DYNAMIC_ZLIB[split..], 0, 4096, Z_NO_FLUSH);
        let from_copy = drive_state(&mut copy, &DYNAMIC_ZLIB[split..], 7, 61, Z_NO_FLUSH);
        assert_eq!(from_original.ret, ReturnCode::STREAM_END);
        assert_eq!(from_copy.ret, ReturnCode::STREAM_END);
        assert_eq!(from_original.output, from_copy.output);
        assert_eq!(
            [first.output.as_slice(), from_copy.output.as_slice()].concat(),
            expected
        );
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
        assert_eq!(inflate_end(&mut copy), ReturnCode::OK);
    }

    #[test]
    fn a_copy_that_cannot_allocate_leaves_the_source_usable() {
        // `test/infcover.c` L438: `inflateCopy` under a budget that cannot hold a second
        // window must fail with `Z_MEM_ERROR` and change nothing.
        let mut state = inflate_init2(InflateConfig::new(-15), GlobalAllocator).unwrap();
        let call = drive_state(&mut state, HELLO_RAW, 0, 64, Z_NO_FLUSH);
        assert_eq!(call.ret, ReturnCode::STREAM_END);
        assert!(state.whave > 0);

        assert_eq!(
            inflate_copy(&state, NoMemory).err(),
            Some(ReturnCode::MEM_ERROR)
        );
        // The source still works: reset it and decode the same stream again.
        let _ = inflate_reset(&mut state);
        let again = drive_state(&mut state, HELLO_RAW, 0, 64, Z_NO_FLUSH);
        assert_eq!(again.ret, ReturnCode::STREAM_END);
        assert_eq!(again.output, HELLO);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn a_copy_of_a_windowless_state_needs_no_window() {
        // `inflate.c` L1343-L1351: the window is allocated only if the source has one, so
        // copying a freshly initialised stream succeeds even under a refusing allocator.
        let mut state = inflate_init(GlobalAllocator).unwrap();
        let mut copy = inflate_copy(&state, NoMemory).unwrap();
        assert_eq!(copy.mode, Mode::Head);
        assert_eq!(copy.whave, 0);
        assert_eq!(inflate_end(&mut copy), ReturnCode::OK);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn prime_accepts_and_refuses_exactly_what_the_reference_does() {
        // `inflate.c` L219-L235, plus `test/infcover.c` L359-L360.
        let mut state = inflate_init(GlobalAllocator).unwrap();
        // Zero bits is a documented no-op.
        assert_eq!(inflate_prime(&mut state, 0, 0xff), ReturnCode::OK);
        assert_eq!(state.bits, 0);
        assert_eq!(state.hold, 0);

        // Sixteen is the largest single request.
        assert_eq!(inflate_prime(&mut state, 16, 0x1234), ReturnCode::OK);
        assert_eq!(state.bits, 16);
        assert_eq!(state.hold, 0x1234);
        // A second sixteen reaches the 32-bit ceiling exactly.
        assert_eq!(inflate_prime(&mut state, 16, 0x5678), ReturnCode::OK);
        assert_eq!(state.bits, 32);
        assert_eq!(state.hold, 0x5678_1234);
        // One more bit would exceed it.
        assert_eq!(
            inflate_prime(&mut state, 1, 1),
            ReturnCode::STREAM_ERROR,
            "state.bits + bits > 32"
        );
        assert_eq!(state.bits, 32, "a refused request changes nothing");

        // A negative count clears the accumulator.
        assert_eq!(inflate_prime(&mut state, -1, 0), ReturnCode::OK);
        assert_eq!(state.bits, 0);
        assert_eq!(state.hold, 0);

        // More than sixteen bits at once is refused.
        assert_eq!(inflate_prime(&mut state, 17, 0), ReturnCode::STREAM_ERROR);
        assert_eq!(inflate_prime(&mut state, 31, 0), ReturnCode::STREAM_ERROR);
        // The value is masked to the requested width (L232).
        assert_eq!(inflate_prime(&mut state, 5, -1), ReturnCode::OK);
        assert_eq!(state.hold, 0x1f);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn mark_reports_the_position_within_a_block() {
        // `inflate.c` L1397-L1406.
        let mut state = inflate_init2(InflateConfig::new(-15), GlobalAllocator).unwrap();
        // A fresh stream has no pending code, so `back` is -1.
        assert_eq!(inflate_mark(&state), INFLATE_MARK_BAD_STATE);
        assert_eq!(INFLATE_MARK_BAD_STATE, -65_536);

        // Mid-copy in `MATCH`, the low half is `was - length`.
        state.mode = Mode::Match;
        state.back = 3;
        state.was = 100;
        state.length = 40;
        assert_eq!(inflate_mark(&state), (3 << 16) + 60);

        // Mid-copy in `COPY`, it is `length` itself.
        state.mode = Mode::Copy;
        state.length = 7;
        assert_eq!(inflate_mark(&state), (3 << 16) + 7);

        // Anywhere else the low half is zero.
        state.mode = Mode::Type;
        assert_eq!(inflate_mark(&state), 3 << 16);
        // And a negative `back` shifts as two's complement, exactly as C's cast pair does.
        state.back = -1;
        assert_eq!(inflate_mark(&state), -65_536);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn codes_used_counts_the_arena_entries_the_tables_took() {
        // `inflate.c` L1408-L1412.
        let mut state = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        assert_eq!(inflate_codes_used(&state), 0);
        let outcome = drive_state(&mut state, DYNAMIC_ZLIB, 0, 4096, Z_NO_FLUSH);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, dynamic_plain());
        let used = inflate_codes_used(&state);
        assert!(used > 0, "a dynamic block builds tables");
        assert!(used <= ENOUGH as u64, "{used} exceeds the arena");
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);

        // ★ A fixed-code block uses the shared static tables and takes no arena entries.
        // `REPEAT_ZLIB` is one such stream even though it is 61 bytes of matches: byte 2 is
        // `0x4b`, so `BTYPE == 0b01`.
        for fixture in [HELLO_RAW, REPEAT_ZLIB] {
            let window_bits = if core::ptr::eq(fixture, HELLO_RAW) {
                -15
            } else {
                15
            };
            let mut state =
                inflate_init2(InflateConfig::new(window_bits), GlobalAllocator).unwrap();
            assert_eq!(
                drive_state(&mut state, fixture, 0, 4096, Z_NO_FLUSH).ret,
                ReturnCode::STREAM_END
            );
            assert_eq!(inflate_codes_used(&state), 0);
            assert_eq!(inflate_end(&mut state), ReturnCode::OK);
        }

        // The sentinel the facade reports for a stream that fails its own state check.
        assert_eq!(INFLATE_CODES_USED_BAD_STATE, u64::MAX);
    }

    #[test]
    fn undermine_refuses_in_the_shipped_configuration() {
        // ★ `inflate.c` L1379-L1381: without `INFLATE_ALLOW_INVALID_DISTANCE_TOOFAR_ARRR`
        // this forces `sane` true and reports `Z_DATA_ERROR`, and `test/infcover.c` L440
        // asserts exactly that.
        let mut state = inflate_init(GlobalAllocator).unwrap();
        assert!(state.sane);
        assert_eq!(inflate_undermine(&mut state, true), ReturnCode::DATA_ERROR);
        assert!(state.sane, "sane cannot be cleared");
        assert_eq!(inflate_undermine(&mut state, false), ReturnCode::DATA_ERROR);
        assert!(state.sane);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);

        // Which is why a too-far-back distance is always rejected.
        let bytes = h2b("c c0 81 0 0 0 0 0 90 ff 6b 4 0");
        let outcome = decode(&bytes, -15, 0, 128);
        assert_eq!(outcome.ret, ReturnCode::DATA_ERROR);
        assert_eq!(outcome.msg, Some(MSG_INVALID_DISTANCE_TOO_FAR_BACK));
    }

    #[test]
    fn reset_restores_the_documented_values() {
        // `inflate.c` L99-L134.
        let mut state = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        // ★ Four output bytes per call, not the whole stream at once. A single-call decode
        // never populates the window: `CHECK` resets the epilogue's output snapshot to the
        // remaining `avail_out` (L1078-L1084), so `out != strm->avail_out` is false there
        // and `state->wsize` is still zero -- the whole `updatewindow` condition at L1132
        // collapses. Only a call that suspends mid-stream with bytes written creates it.
        assert_eq!(
            drive_state(&mut state, HELLO_ZLIB, 0, 4, Z_NO_FLUSH).ret,
            ReturnCode::STREAM_END
        );
        assert!(state.whave > 0);

        // `inflateResetKeep` leaves the window contents alone (L125-L133 is the variant
        // that clears them).
        let reset = inflate_reset_keep(&mut state);
        assert_eq!(reset.total_in, 0);
        assert_eq!(reset.total_out, 0);
        assert_eq!(reset.msg, None);
        assert_eq!(reset.data_type, 0);
        assert_eq!(reset.adler, Some(1), "wrap & 1 for a zlib stream");
        assert_eq!(state.mode, Mode::Head);
        assert!(!state.last);
        assert!(!state.havedict);
        assert_eq!(state.flags, -1);
        assert_eq!(state.dmax, 32_768);
        assert_eq!(state.hold, 0);
        assert_eq!(state.bits, 0);
        assert_eq!(state.back, -1);
        assert!(state.sane);
        assert_eq!(state.codes_used(), 0);
        assert!(state.whave > 0, "resetKeep preserves the history");

        // `inflateReset` clears the cursors too.
        let reset = inflate_reset(&mut state);
        assert_eq!(reset.adler, Some(1));
        assert_eq!(state.whave, 0);
        assert_eq!(state.wsize, 0);
        assert_eq!(state.wnext, 0);

        // And the stream decodes again from the top.
        let outcome = drive_state(&mut state, HELLO_ZLIB, 0, 64, Z_NO_FLUSH);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, HELLO);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn reset_two_changes_the_container_format() {
        // `inflate.c` L136-L170, and `test/infcover.c` L343: `inflateReset2(&strm, -8)`.
        let mut state = inflate_init2(InflateConfig::new(15), GlobalAllocator).unwrap();
        assert_eq!(
            drive_state(&mut state, HELLO_ZLIB, 0, 64, Z_NO_FLUSH).ret,
            ReturnCode::STREAM_END
        );

        // Switching to raw at a different exponent releases the window, because it is now
        // the wrong size (L162-L165).
        let reset = inflate_reset2(&mut state, InflateConfig::new(-8)).unwrap();
        assert_eq!(reset.adler, None, "a raw stream leaves adler alone");
        assert!(state.wrap.is_raw());
        assert_eq!(state.wbits, 8);
        assert_eq!(state.wsize, 0);
        let outcome = drive_state(&mut state, HELLO_RAW, 0, 64, Z_NO_FLUSH);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, HELLO);

        // And back to gzip.
        assert!(inflate_reset2(&mut state, InflateConfig::new(31)).is_ok());
        let outcome = drive_state(&mut state, HELLO_GZIP, 0, 64, Z_NO_FLUSH);
        assert_eq!(outcome.ret, ReturnCode::STREAM_END);
        assert_eq!(outcome.output, HELLO);

        // An invalid request changes nothing and reports a stream error.
        assert_eq!(
            inflate_reset2(&mut state, InflateConfig::new(1)).err(),
            Some(ReturnCode::STREAM_ERROR)
        );
        assert!(state.wrap.allows_gzip_header(), "the old request survives");
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn a_memory_error_is_sticky_and_skips_the_epilogue() {
        // ★ `test/infcover.c` L419-L423: with an allocator that cannot provide a window,
        // two consecutive `inflate` calls must both report `Z_MEM_ERROR`. The second one
        // comes from the `MEM` arm, which is a bare return (`inflate.c` L1117-L1118) --
        // so it must not touch the totals either, as `inflate.c` L1129 explains.
        let mut state = inflate_init2(InflateConfig::new(-8), NoMemory).unwrap();
        let input = [0x63_u8, 0x00];
        let mut buffer = vec![0_u8; 1];
        let mut stream = InflateStream::new(&input, &mut buffer);
        stream.total_in = 5;
        stream.total_out = 9;

        let ret = inflate(&mut state, &mut stream, Z_NO_FLUSH);
        assert_eq!(ret, ReturnCode::MEM_ERROR);
        assert_eq!(state.mode, Mode::Mem);
        // The epilogue latched the error before crediting anything.
        assert_eq!(stream.total_in, 5);
        assert_eq!(stream.total_out, 9);

        // Again, and again: the mode is sticky.
        for _ in 0..3 {
            let mut buffer = vec![0_u8; 1];
            let mut stream = InflateStream::new(&input, &mut buffer);
            stream.total_in = 5;
            stream.total_out = 9;
            assert_eq!(
                inflate(&mut state, &mut stream, Z_NO_FLUSH),
                ReturnCode::MEM_ERROR
            );
            // A bare return consumes nothing and credits nothing.
            assert_eq!(stream.next_in, 0);
            assert_eq!(stream.next_out, 0);
            assert_eq!(stream.total_in, 5);
            assert_eq!(stream.total_out, 9);
            assert_eq!(stream.data_type, 0);
        }

        // Only a reset clears it.
        let _ = inflate_reset(&mut state);
        assert_eq!(state.mode, Mode::Head);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn a_dictionary_that_cannot_be_stored_latches_a_memory_error() {
        // `inflate.c` L1210-L1213: the other `state->mode = MEM` site.
        let mut state = inflate_init2(InflateConfig::new(-15), NoMemory).unwrap();
        assert_eq!(
            inflate_set_dictionary(&mut state, DICTIONARY),
            ReturnCode::MEM_ERROR
        );
        assert_eq!(state.mode, Mode::Mem);
        assert!(!state.havedict);
        assert_eq!(inflate_end(&mut state), ReturnCode::OK);
    }

    #[test]
    fn arbitrary_bytes_never_panic_or_hang() {
        // A fast local proxy for `cargo fuzz run fuzz_inflate`, which is the binding gate.
        // The generator is a deterministic xorshift so a failure is reproducible, and no
        // external crate is involved.
        let mut seed = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };

        let window_bits = [-15_i32, -8, 0, 8, 15, 16, 31, 47];
        for round in 0..2_000_u32 {
            let length = 1 + (next() % 96) as usize;
            let mut noise: Vec<u8> = (0..length).map(|_| (next() & 0xff) as u8).collect();
            // Half the rounds start from a real header, so the noise reaches deeper into
            // the state machine instead of being rejected on the first two bytes.
            if round % 2 == 0 {
                let prefix: &[u8] = match round % 6 {
                    0 => HELLO_ZLIB,
                    2 => HELLO_GZIP,
                    _ => REPEAT_ZLIB,
                };
                let keep = (next() % (prefix.len() as u64 + 1)) as usize;
                let mut seeded = prefix[..keep].to_vec();
                seeded.append(&mut noise);
                noise = seeded;
            }
            let bits = window_bits[(next() % window_bits.len() as u64) as usize];
            let out_step = 1 + (next() % 300) as usize;
            let in_step = 1 + (next() % 17) as usize;

            // The assertion is simply that this returns: `decode` caps its own iteration
            // count and panics if the machine fails to terminate, and any panic inside the
            // decoder itself would surface here.
            let outcome = decode(&noise, bits, in_step, out_step);
            assert!(
                matches!(
                    outcome.ret,
                    ReturnCode::OK
                        | ReturnCode::STREAM_END
                        | ReturnCode::NEED_DICT
                        | ReturnCode::DATA_ERROR
                        | ReturnCode::BUF_ERROR
                ),
                "round {round}: unexpected {:?}",
                outcome.ret
            );
            // Whatever happened, the decoder never claims more output than it was given
            // room for.
            assert!(
                outcome.total_out as usize >= outcome.output.len() || outcome.output.is_empty()
            );
        }
    }

    #[test]
    fn a_truncated_stream_suspends_rather_than_failing() {
        // Every prefix of a valid stream must be a legal "need more input" state, never a
        // data error and never a panic.
        for (stream, bits) in [
            (HELLO_ZLIB, 15_i32),
            (HELLO_GZIP, 31),
            (REPEAT_ZLIB, 15),
            (STORED_ZLIB, 15),
        ] {
            for cut in 0..stream.len() {
                let outcome = decode(&stream[..cut], bits, 0, 4096);
                assert!(
                    matches!(outcome.ret, ReturnCode::OK | ReturnCode::BUF_ERROR),
                    "cut {cut} of {bits}: {:?} {:?}",
                    outcome.ret,
                    outcome.msg
                );
                // The prefix of the output is always a prefix of the whole output.
                let full = decode(stream, bits, 0, 4096);
                assert!(full.output.starts_with(&outcome.output), "cut {cut}");
            }
        }
    }
}
