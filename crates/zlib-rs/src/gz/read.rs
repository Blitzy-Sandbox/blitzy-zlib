//! The read side of the `gzFile` layer: the Rust counterpart of `gzread.c` (668 lines) together with the
//! positioning entry points of `gzlib.c` (L346-L495).
//!
//! This module drives the inflate engine on behalf of `gzread`, `gzfread`, `gzgetc`, `gzungetc`,
//! `gzgets`, `gzseek`, `gztell`, `gzoffset`, `gzrewind` and `gzclose_r`. It owns three things that
//! live nowhere else in the implementation:
//!
//! 1. **The transparent-read decision.** `gz_look` inspects the first four bytes of a stream and
//!    decides whether the file is a gzip member to be decompressed or arbitrary data to be copied
//!    through unchanged. The answer is what `gzdirect` reports (`gzread.c` L93-L170).
//! 2. **`Z_ERRNO`.** No other part of this library can produce it: the deflate and inflate engines
//!    report only their own status codes, so every `Z_ERRNO` a caller ever sees was recorded here or
//!    in the write half. Wherever C writes `zstrerror()` (`gzguts.h` L131-L133, i.e.
//!    `strerror(errno)`) this module renders a [`GzIoError`] instead; see `errno_message`.
//! 3. **The multi-member and trailing-garbage rules.** `zlib.h` L1462-L1466 promises that any number
//!    of concatenated gzip members are decompressed as one continuous stream, and that anything
//!    other than a member found *after* a member is silently ignored. Both behaviours come out of
//!    the three-state `junk` field described below.
//!
//! # What this module does not do
//!
//! It contains no decompression logic and no gzip-header decoding of its own. The engine is
//! `crate::inflate`, initialised in gunzip mode exactly as `gz_look` initialises it with
//! `inflateInit2(&(state->strm), 15 + 16)` (`gzread.c` L115), and the four-byte detection heuristic
//! is `looks_like_gzip`, which `crate::gz::header` owns. Header fields, the trailer, the CRC-32
//! and the length check are the engine's business (`inflate.c` L513-L671), so nothing about
//! RFC 1952 is re-derived here.
//!
//! # The three-state `junk` field
//!
//! `gzguts.h` L186 documents it as "-1 = start, 1 = junk candidate, 0 = in gzip", and getting its
//! transitions wrong breaks either multi-member reads or `gzip` compatibility:
//!
//! | Value | Meaning | Set by |
//! |---|---|---|
//! | -1 | nothing has been read yet; this is the first member | `gz_reset` (`gzlib.c` L75) |
//! | 1 | a gzip header was seen but no byte has been decompressed from it yet | `gz_look` (`gzread.c` L156), and `junk != -1` at L130 |
//! | 0 | this really is a gzip stream: either output was produced or a member completed | `gz_decomp` (`gzread.c` L203 and L233) |
//!
//! The value is what distinguishes "trailing garbage after a real member, which `zlib.h` L1465-L1466
//! says to ignore" from "this was never gzip, which is a data error": `gz_decomp` accepts a
//! `Z_DATA_ERROR` silently when `junk == 1` and reports it when `junk == 0`
//! (`gzread.c` L213-L224).
//!
//! # The `again` non-blocking protocol
//!
//! `state->again` is set whenever a read fails with `EAGAIN` or `EWOULDBLOCK`
//! (`gzread.c` L36-L40), and it is a first-class part of this layer's contract rather than an edge
//! case. It threads through `gz_load`, `gz_avail`, `gz_look`, `gz_decomp`, [`gzread`] and
//! `gz_error` -- which declines to clear `x.have` for a merely stalled stream
//! (`gzlib.c` L564-L565). The observable consequences are the ones `zlib.h` documents: `gzread`
//! returns -1 rather than 0 so a stall is not mistaken for end of file (L1480-L1483), and `gzdirect`
//! answers 1 until four bytes have accumulated (L1734-L1743).
//!
//! Because this crate takes no `libc` dependency, `EAGAIN`/`EWOULDBLOCK` are recognised through
//! [`GzIoError::would_block`], which the handle implementation derives from
//! `std::io::ErrorKind::WouldBlock`.
//!
//! # Buffer sizes: `out` is deliberately twice `in`
//!
//! `gz_look` allocates `want` input bytes and `want << 1` output bytes (`gzread.c` L99-L100), and
//! `gzguts.h` L154-L155 explains why: "default i/o buffer size -- double this for output when
//! reading". Two behaviours depend on the asymmetry, and neither survives making the buffers equal:
//!
//! * the transparent path copies the whole input buffer into the output buffer in one go, which is
//!   only sound because the destination is larger (`gzread.c` L161-L166);
//! * `gzungetc` parks a pushed byte at index `2 * size - 1` and can therefore always accept at least
//!   one push, which is what `zlib.h` L1633 guarantees (`gzread.c` L533-L536).
//!
//! # The exposed-prefix contract
//!
//! `crate::gz::state` explains at length why the first 24 bytes of [`GzState`] are frozen: the
//! caller's `gzgetc` macro decrements `have`, increments `pos` and post-increments `next` in the
//! caller's own object code (`zlib.h` L1967-L1968). Consequently **every public entry point in this
//! module calls [`GzState::resync_from_exposed`] on entry and [`GzState::refresh_exposed`] before it
//! returns**, and the internal helpers work in terms of [`GzState::out_pos`] rather than the
//! pointer. The two places where C's pointer transiently leaves the output buffer are handled as
//! that module prescribes: `gz_decomp` reports its production count as a value instead of
//! publishing a window into someone else's buffer, and `gzseek64`'s raw-area fast path clears the
//! pair together with [`GzState::clear_have`].
//!
//! # Freshly allocated buffers are never assumed to be zeroed
//!
//! Both working buffers come from the injected [`Allocator`], which for a `gzFile` is `malloc`-backed
//! in C (`zutil.c` L299-L303 takes the `malloc` branch whenever `sizeof(uInt) > 2`), and
//! `test/infcover.c` L84-L87 fills every block it hands out with `0xa5` precisely to catch code that
//! assumes otherwise. Nothing below reads a byte of either buffer that a read or a decompression has
//! not first written.
//!
//! # What this module expects from `crate::gz`
//!
//! Two crate-internal helpers are used from the module root, which is where `gzlib.c`'s shared
//! plumbing lives and where `crate::gz::open` already takes `gz_error` from:
//!
//! * `gz_error(state, code, message)` -- the Rust counterpart of `gz_error` (`gzlib.c` L555-L590). Recording a
//!   [`ReturnCode`] with an optional byte-string message, which is prefixed with the path exactly as
//!   [`GzState::try_set_prefixed_msg`] builds it, and which clears `x.have` for a fatal error unless
//!   the stream is merely stalled.
//! * `gt_off(value)` -- the Rust counterpart of the `GT_OFF` macro (`gzguts.h` L212-L216), true when an
//!   `unsigned` cannot be compared against a `z_off64_t` without loss. It is false on every target
//!   where `z_off64_t` is wider than `int`, which is every target this implementation supports, but the guard
//!   is preserved because the comparisons it protects are C's.
//!
//! # Panic and allocation posture
//!
//! No path here can panic: there is no indexing, no slicing syntax, no `unwrap`, no `expect` and no
//! arithmetic that can overflow in a debug build. Every buffer access goes through a checked
//! accessor, and the one place a message must be formatted uses a fixed-size stack buffer
//! (`ErrnoMessage`) rather than a heap `String`, so that reporting an I/O error cannot itself fail.

use core::ffi::{c_int, c_uint};

use crate::allocate::{Allocator, Buffer};
use crate::config::{InflateConfig, Z_NO_FLUSH};
use crate::error::ReturnCode;
use crate::gz::header::looks_like_gzip;
use crate::gz::open::gz_reset;
use crate::gz::state::{
    split_buffers, GzEngine, GzHandle, GzHow, GzIoError, GzSeekFrom, GzState, GzStream, ZOff64,
    COPY, GZIP, GZ_READ, GZ_WRITE, LOOK,
};
use crate::gz::{errno_message, gt_off, gz_error};
use crate::inflate::{inflate, inflate_end, inflate_init2, inflate_reset, InflateStream};
use crate::read_buf::OutputRegion;

/// The largest number of bytes one call to the handle may be asked for.
///
/// Implements `gz_load`'s `unsigned get, max = ((unsigned)-1 >> 2) + 1` (`gzread.c` L21). The
/// expression is reproduced rather than replaced by a round number because it is the value the
/// reference implementation uses: a quarter of the `unsigned` range plus one, which on a 32-bit
/// `unsigned` is `0x4000_0000`. C needs the clamp because `read` returns an `int`, so a request larger
/// than `INT_MAX` could not report its own result; this implementation keeps it so that the number of handle
/// calls, and therefore the number of underlying `read` syscalls, matches.
const MAX_READ_CHUNK: c_uint = (c_uint::MAX >> 2) + 1;

/// The gzip-only `windowBits` request the read path initialises its engine with.
///
/// `gz_look` calls `inflateInit2(&(state->strm), 15 + 16)` and labels it "gunzip"
/// (`gzread.c` L115): 15 is the maximum window exponent and `+ 16` asks `crate::inflate` for the
/// gzip container rather than the zlib one. The sum is written the way C writes it so that the two
/// halves of the request stay visible.
const GUNZIP_WINDOW_BITS: i32 = 15 + 16;

/// Widens a C `unsigned` to a `usize`, saturating rather than wrapping.
///
/// Every `unsigned` this module widens is a buffer length or a count already bounded by one, so the
/// saturation is unreachable; it exists because a wrapping conversion would be a silent bug and a
/// panicking one is forbidden.
fn widen(value: c_uint) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// Narrows a `usize` to a C `unsigned`, reporting values that do not fit.
///
/// C performs this narrowing with an unchecked cast -- `(unsigned)len` at `gzread.c` L337, for
/// instance -- always after a range test. Making the test part of the conversion is what keeps the
/// two from drifting apart.
fn narrow(value: usize) -> Option<c_uint> {
    c_uint::try_from(value).ok()
}

/// Widens a `usize` to a file offset, saturating rather than wrapping.
///
/// Used for `state->x.pos += n` (`gzread.c` L384 and L612), where `n` is a count of bytes just
/// delivered and therefore far below [`ZOff64::MAX`].
fn to_off(value: usize) -> ZOff64 {
    ZOff64::try_from(value).unwrap_or(ZOff64::MAX)
}

/// Whether the recorded error still permits reading, i.e. C's
/// `state->err == Z_OK || state->err == Z_BUF_ERROR`.
///
/// The test appears at the head of `gz_avail` (`gzread.c` L60), of every public read entry point
/// (L407, L452, L485, L520, L581) and of `gzrewind` and `gzseek64` (`gzlib.c` L356, L380).
/// `Z_BUF_ERROR` is deliberately not disqualifying: `zlib.h` L1470-L1478 documents it as the
/// recoverable "the input file ended in the middle of a gzip stream" condition, which `gzclearerr`
/// can clear so that reading resumes on a file that is still growing.
fn err_permits_reading(state_err: i32) -> bool {
    state_err == ReturnCode::OK.as_i32() || state_err == ReturnCode::BUF_ERROR.as_i32()
}

/// The clamp C writes as
/// `GT_OFF(have) || (z_off64_t)have > amount ? (unsigned)amount : have`.
///
/// It appears twice, in `gz_skip` (`gzread.c` L288-L290) and in `gzseek64`
/// (`gzlib.c` L424-L425), and both callers pass a non-negative `amount`. On every target where
/// `gt_off` is false -- every target with a `z_off64_t` wider than `int` -- the expression is exactly
/// `min(have, amount)`; the `gt_off` term covers the platform where the signed comparison would
/// itself lose information, and is preserved for that reason.
///
/// The `(unsigned)amount` cast is reached only when `amount < have`, so the value always fits; the
/// fallback returns `have`, which is the other arm of C's conditional and cannot over-consume.
fn clamp_to_have(have: c_uint, amount: ZOff64) -> c_uint {
    if gt_off(have) || ZOff64::from(have) > amount {
        c_uint::try_from(amount).unwrap_or(have)
    } else {
        have
    }
}

/// The message C reports for a structure whose `how` field holds no recognised value.
///
/// `gz_fetch`'s `default:` arm (`gzread.c` L271-L273). This module reuses it for the handful of
/// internal inconsistencies C cannot express -- a missing file handle, or a cursor outside its
/// buffer -- because they are the same class of problem: the state is not one this library produced.
const STATE_CORRUPT: &[u8] = b"state corrupt";

/// The message every allocation failure in this layer reports.
///
/// `gz_look` uses it for both the working buffers and the inflate state (`gzread.c` L104 and L119),
/// and `gz_decomp` for a `Z_MEM_ERROR` out of the engine (L210). `gz_error` deliberately does not
/// allocate for `Z_MEM_ERROR`, so this text is what `gzerror` synthesises (`gzlib.c` L526-L527).
const OUT_OF_MEMORY: &[u8] = b"out of memory";

/// `gz_decomp`'s report for input that ran out mid-member (`gzread.c` L195).
///
/// Paired with `Z_BUF_ERROR`, which `zlib.h` L1473-L1478 documents as recoverable and defers to
/// `gzclose`.
const UNEXPECTED_EOF: &[u8] = b"unexpected end of file";

/// `gz_decomp`'s report for a status the engine should not be able to produce here
/// (`gzread.c` L205-L206): `Z_STREAM_ERROR`, or `Z_NEED_DICT` for a stream that cannot have one.
const INFLATE_CORRUPT: &[u8] = b"internal error: inflate stream corrupt";

/// `gz_decomp`'s fallback when the engine reports corrupt data without a message
/// (`gzread.c` L222).
const COMPRESSED_DATA_ERROR: &[u8] = b"compressed data error";

/// `gzungetc`'s report when the output buffer is already completely full (`gzread.c` L544).
const NO_ROOM_TO_PUSH: &[u8] = b"out of room to push characters";

/// `gzread`'s report for a length that cannot be returned in an `int` (`gzread.c` L414).
const REQUEST_EXCEEDS_INT: &[u8] = b"request does not fit in an int";

/// `gzfread`'s report for a `size * nitems` product that overflows (`gzread.c` L459).
const REQUEST_EXCEEDS_SIZE_T: &[u8] = b"request does not fit in a size_t";

/// The guard that replaces C's unchecked write past a caller's buffer in `gzfread`.
///
/// C computes `len = nitems * size` and hands `len` bytes at `buf` to `gz_read` with no way to know
/// how long the caller's buffer really is. This port receives a slice, so a request longer than the
/// slice is detectable -- and it is refused rather than silently shortened, because shortening it
/// would answer a *different* question from the one the caller asked and the caller has no way to
/// tell which happened: `gzfread` returns whole items, so a short answer is indistinguishable from
/// end of file. `crate::gz::write` refuses the mirror-image request in `gzfwrite` with the same
/// wording, so the two directions agree.
const REQUEST_PAST_BUFFER: &[u8] = b"request does not fit in the supplied buffer";

/// The message for a [`GzHandle`] that claims to have read more than it was offered.
///
/// C cannot detect this: `read(2)` is trusted to honour its `count` argument and `gz_load` adds the
/// return value to `*have` unchecked (`gzread.c` L33). This port hands the handle a *slice*, so a
/// count larger than that slice is a broken implementation of the trait -- the bytes it claims to
/// have delivered cannot exist. Clamping the number was the alternative and it is unsafe in the
/// meaningful sense: the layer would go on to treat uninitialised buffer bytes as data the handle
/// supplied. Refusing turns a broken handle into a reported `Z_STREAM_ERROR` instead.
const HANDLE_OVER_REPORTED: &[u8] = b"file handle reported more bytes than were requested";

/// What one input or decompression step produced, and whether it failed.
///
/// C's internal read-path helpers report two things at once: a count, through an out-parameter
/// (`gz_load`'s `unsigned *have`, `gzread.c` L19) or through `state->x.have` (`gz_decomp`'s
/// L228), and a success flag, as a `0`/`-1` return. Both halves are needed even together: on a
/// failing read that had already obtained some bytes, `gz_read` consumes the partial count *and*
/// stops the loop (`gzread.c` L369 and L381-L385). Collapsing the pair into a `Result` would throw
/// the count away on exactly that path, so it is kept as a struct.
#[derive(Debug)]
pub(crate) struct Progress {
    /// Bytes obtained: C's `*have` for [`gz_load`], C's `had - strm->avail_out` for `gz_decomp`.
    pub(crate) count: c_uint,
    /// `Ok(())` for C's `0` return, `Err(code)` for C's `-1`.
    ///
    /// The code is the one `gz_error` has just recorded in `state->err`, which is where C's callers
    /// read it from (`gzread.c` L190).
    pub(crate) result: Result<(), ReturnCode>,
}

/// Where [`gz_load`] puts the bytes it reads: the Rust counterpart of the three `buf` arguments C passes it.
///
/// The destination cannot simply be a `&mut [u8]`, because two of the three live inside the very
/// [`GzState`] whose handle performs the read, and a single `&mut GzState` cannot yield both at once.
/// Naming the three cases lets the borrows be taken apart field by field, which is safe precisely
/// because `handle`, `input` and `output` are distinct fields.
#[derive(Debug)]
pub(crate) enum LoadTarget<'buf> {
    /// `state->in + strm->avail_in` for `state->size - strm->avail_in` bytes: `gz_avail`'s call
    /// (`gzread.c` L75-L76), which refills the free tail of the input buffer.
    Input {
        /// Index of the first free byte of the input buffer.
        offset: usize,
        /// How many bytes of the tail to offer to the handle.
        len: usize,
    },
    /// `state->out` for `state->size << 1` bytes: `gz_fetch`'s transparent call
    /// (`gzread.c` L260-L261), which fills the whole output buffer.
    Output {
        /// How many bytes of the output buffer to offer to the handle.
        len: usize,
    },
    /// The caller's own buffer: `gz_read`'s large-request transparent call (`gzread.c` L369).
    ///
    /// An [`OutputRegion`] rather than a `&mut [u8]`, because a C caller's buffer is
    /// guaranteed writable and nothing more. This is the one destination in the module that
    /// the operating system writes into rather than this crate, so it is also the one that
    /// goes through [`OutputRegion::writable_bytes`]; that method documents the single extra
    /// pass write-only storage costs here and why no other path pays it.
    User(OutputRegion<'buf>),
}

/// Where `gz_decomp` sends the bytes the engine produces.
///
/// The two ways C points `strm->next_out` before calling it: at the layer's own output buffer, so
/// that `gzgetc` can be served from it (`gzread.c` L266-L267), or straight at the caller's buffer for
/// a request too large to be worth double-buffering (L373-L374).
#[derive(Debug)]
pub(crate) enum DecompTarget<'buf> {
    /// The layer's own output buffer, whose delivered window `gz_decomp` publishes on return.
    Internal,
    /// The caller's buffer. Nothing is published: the count comes back in [`Progress::count`],
    /// which is what C's caller reads out of `x.have` before immediately zeroing it
    /// (`gzread.c` L376-L377).
    ///
    /// An [`OutputRegion`] rather than a `&mut [u8]`, because a C caller's buffer is
    /// guaranteed writable and nothing more; the decoder writes through it and reads back
    /// only what it has written itself.
    User(OutputRegion<'buf>),
}

/// What one call to the handle's `read` sequence observed.
///
/// The four things `gz_load`'s loop can learn, kept together so that the loop itself needs no access
/// to the [`GzState`] and can therefore run while the handle and the destination buffer are borrowed
/// out of it.
#[derive(Debug)]
struct ReadOutcome {
    /// C's `*have`: bytes actually read into the destination.
    have: usize,
    /// C's `state->again`: the read failed with `EAGAIN` or `EWOULDBLOCK`.
    have_stalled: bool,
    /// C's `ret == 0` at `gzread.c` L44: the handle reported end of file.
    at_eof: bool,
    /// The failure to report as `Z_ERRNO`, if C would have reported one at L41.
    failure: Option<GzIoError>,
    /// The handle reported a count larger than the window it was given.
    ///
    /// Has no C counterpart -- see [`HANDLE_OVER_REPORTED`] -- and is fatal rather than clamped,
    /// which is why it travels separately from [`ReadOutcome::failure`]: it is a `Z_STREAM_ERROR`,
    /// not a `Z_ERRNO`, because no operating-system error occurred.
    over_reported: bool,
}

/// Resolves a [`LoadTarget`] into the byte window the handle should read into.
///
/// Split out of [`gz_load`] so that the field borrows it needs are taken in one place. [`None`]
/// means the requested window does not lie inside the buffer it names, which C cannot express and
/// this implementation reports as a corrupt state rather than trusting.
fn resolve_load_window<'buf, 'alloc>(
    target: &'buf mut LoadTarget<'_>,
    input: Option<&'buf mut Buffer<'alloc, u8>>,
    output: Option<&'buf mut Buffer<'alloc, u8>>,
) -> Option<&'buf mut [u8]> {
    match target {
        LoadTarget::Input { offset, len } => {
            let (offset, len) = (*offset, *len);
            let end = offset.checked_add(len)?;
            input
                .map_or(&mut [][..], Buffer::as_mut_slice)
                .get_mut(offset..end)
        }
        LoadTarget::Output { len } => output
            .map_or(&mut [][..], Buffer::as_mut_slice)
            .get_mut(..*len),
        LoadTarget::User(region) => {
            // The window *is* the sub-region `gz_read` carved out, so the whole of it is
            // offered. For write-only storage this is where the range is initialised before
            // the handle writes into it; see `OutputRegion::writable_bytes`.
            let len = region.len();
            region.writable_bytes(0, len)
        }
    }
}

/// Runs `gz_load`'s read loop over an already-borrowed handle and destination.
///
/// The loop is C's, statement for statement (`gzread.c` L26-L46):
///
/// * it **must** loop, because a single `read` is not obliged to return everything asked for, which
///   depends on the kind of descriptor (L10-L12);
/// * each request is clamped to [`MAX_READ_CHUNK`], C's `((unsigned)-1 >> 2) + 1` (L21);
/// * a read of zero bytes is end of file (L44-L45);
/// * a `would_block` failure that has already collected bytes is a **success** with the stall
///   recorded, and only a stall with nothing to show for it is reported as an error
///   (L36-L40). Preserving that order is what lets a non-blocking reader make partial progress.
///
/// A handle that returns more bytes than the window it was given cannot move the cursor past the
/// window: the count is clamped. C has no equivalent because `read` is trusted, but here the handle
/// may be an injected trait object and the clamp costs nothing.
///
/// Generic over the handle, and `?Sized` so that both a
/// [`GzHandleRef`](crate::gz::state::GzHandleRef) -- which dispatches by variant -- and a bare
/// `dyn GzHandle` are accepted. Taking `&mut dyn GzHandle` instead would force every caller back
/// through a vtable, which is precisely what [`GzHandleRef`](crate::gz::state::GzHandleRef) exists
/// to avoid.
fn read_into<H: GzHandle + ?Sized>(handle: &mut H, buffer: &mut [u8]) -> ReadOutcome {
    let max = widen(MAX_READ_CHUNK);
    let len = buffer.len();
    let mut have = 0_usize;
    let mut have_stalled = false;
    let mut at_eof = false;
    let mut failure = None;
    let mut over_reported = false;

    loop {
        let get = len.saturating_sub(have).min(max);
        let end = have.saturating_add(get);
        let Some(window) = buffer.get_mut(have..end) else {
            break;
        };

        match handle.read(window) {
            // L31-L32 with `ret == 0`, resolved at L44-L45.
            Ok(0) => {
                at_eof = true;
                break;
            }
            // A count larger than the window is impossible from a correct handle, so it is
            // refused rather than clamped: the extra bytes do not exist, and adding them to `have`
            // would publish uninitialised buffer as data. Nothing is added, so no cursor moves.
            Ok(count) if count > get => {
                over_reported = true;
                break;
            }
            Ok(count) => {
                // L33: `*have += (unsigned)ret;`
                have = have.saturating_add(count);
                // L34: `} while (*have < len);`
                if have >= len {
                    break;
                }
            }
            Err(error) => {
                if error.would_block {
                    have_stalled = true;
                    // L38-L39: partial progress on a stalled descriptor is success.
                    if have != 0 {
                        break;
                    }
                }
                failure = Some(error);
                break;
            }
        }
    }

    ReadOutcome {
        have,
        have_stalled,
        at_eof,
        failure,
        over_reported,
    }
}

/// Reads into `target`, updating `eof`, `again` and the error state as C does.
///
/// Implements `gz_load` (`gzread.c` L18-L47). C reports the count through `unsigned *have` and
/// success through its return value; both come back in [`Progress`], and the count is meaningful even
/// when the result is an error, because a read can fail after delivering bytes.
///
/// `errno = 0` at L24 has no counterpart: this implementation never consults the thread's `errno` to decide
/// anything, only to render a message, and it takes the value from the failure the handle reported.
pub(crate) fn gz_load<'a, A: Allocator<'a>>(
    state: &mut GzState<'a, A>,
    mut target: LoadTarget<'_>,
) -> Progress {
    let outcome = {
        // `handle`, `input` and `output` are distinct fields, so these borrows are disjoint.
        let GzState {
            handle,
            buffer_slot,
            ..
        } = state;
        let (input, output) = split_buffers(buffer_slot);
        match handle.handle_mut() {
            Some(mut handle) => resolve_load_window(&mut target, input, output)
                .map(|window| read_into(&mut handle, window)),
            None => None,
        }
    };

    let Some(outcome) = outcome else {
        // Unreachable through the public API: an open `gzFile` always has a handle, and every
        // window this module asks for is inside the buffer it names.
        gz_error(state, ReturnCode::STREAM_ERROR, Some(STATE_CORRUPT));
        return Progress {
            count: 0,
            result: Err(ReturnCode::STREAM_ERROR),
        };
    };

    // L23: `state->again = 0`, then L37's `state->again = 1`. Recorded before `gz_error` runs,
    // because `gz_error` consults it before deciding whether to clear `x.have`
    // (`gzlib.c` L564-L565).
    state.set_again(outcome.have_stalled);
    let count = narrow(outcome.have).unwrap_or(c_uint::MAX);

    // A handle that over-reported has told the layer nothing it can trust, so the whole call is
    // discarded: zero bytes delivered, no cursor moved, and a `Z_STREAM_ERROR` recorded. Checked
    // before the `Z_ERRNO` arm because it is the more serious of the two -- an `errno` describes a
    // failed syscall, this describes a handle that cannot be relied on at all.
    if outcome.over_reported {
        gz_error(state, ReturnCode::STREAM_ERROR, Some(HANDLE_OVER_REPORTED));
        return Progress {
            count: 0,
            result: Err(ReturnCode::STREAM_ERROR),
        };
    }

    if let Some(error) = outcome.failure {
        let message = errno_message(Some(error));
        gz_error(state, ReturnCode::ERRNO, Some(message.as_bytes()));
        return Progress {
            count,
            result: Err(ReturnCode::ERRNO),
        };
    }

    if outcome.at_eof {
        state.set_eof(true);
    }

    Progress {
        count,
        result: Ok(()),
    }
}

/// Refills the input buffer, and records end of file when the handle runs out.
///
/// Implements `gz_avail` (`gzread.c` L49-L82). Any input still unconsumed is first moved to the
/// front of the buffer and the free tail is then filled, so that the decoder always sees one
/// contiguous run of bytes starting at index zero.
///
/// The subtlety C's own comment draws attention to (L50-L52) is worth restating, because it is what
/// makes `gzeof` behave like `feof`: `eof` records that *the file* ended, and it is set while unused
/// data may still be sitting in the buffer. Once that data has been consumed no further read is
/// attempted, which is why every loop in this module tests `!eof || avail_in != 0` rather than `eof`
/// alone.
///
/// # Errors
///
/// * The recorded error, when one that forbids reading is already latched (L60-L61).
/// * [`ReturnCode::ERRNO`] from [`gz_load`] when the handle fails.
/// * [`ReturnCode::STREAM_ERROR`] if the unconsumed input does not lie inside the input buffer,
///   which C cannot express and no correct caller can produce.
pub(crate) fn gz_avail<'a, A: Allocator<'a>>(state: &mut GzState<'a, A>) -> Result<(), ReturnCode> {
    if !err_permits_reading(state.err()) {
        return Err(state.err_code().unwrap_or(ReturnCode::STREAM_ERROR));
    }

    // L62: with the file exhausted there is nothing left to read, and whatever is still buffered
    // stays where it is.
    if state.eof() {
        return Ok(());
    }

    let avail_in = state.stream().avail_in;
    let filled = widen(avail_in);

    // L63-L74: `if (strm->avail_in) { ... if (q != p) { copy } }`. The guard matters: with the
    // cursor already at the front there is nothing to move, and `copy_within` would be asked for a
    // zero-distance copy.
    if avail_in != 0 {
        let start = state.stream().next_in;
        if start != 0 {
            let end = start.saturating_add(filled);
            let buffer = state.in_slice_mut();
            if end > buffer.len() {
                gz_error(state, ReturnCode::STREAM_ERROR, Some(STATE_CORRUPT));
                return Err(ReturnCode::STREAM_ERROR);
            }
            buffer.copy_within(start..end, 0);
        }
    }

    // L75-L77: fill the tail. The window starts at `filled` because the bytes that were there are
    // now at the front.
    let capacity = widen(state.size());
    let len = capacity.saturating_sub(filled);
    let progress = gz_load(
        state,
        LoadTarget::Input {
            offset: filled,
            len,
        },
    );
    // C returns before touching `avail_in` on a failure, so a partially filled tail is simply not
    // accounted for -- the bytes are in the buffer but no one will look at them.
    progress.result?;

    let stream = state.stream_mut();
    stream.avail_in = avail_in.saturating_add(progress.count);
    stream.next_in = 0;
    Ok(())
}

/// Allocates the working buffers and the engine on first use.
///
/// Implements `gz_look`'s opening block (`gzread.c` L97-L122), kept separate so that the decision
/// logic below reads as one piece. C's two `malloc` calls become
/// [`GzState::allocate_read_buffers`], which requests `want` input bytes and `want << 1` output
/// bytes from the **injected** allocator and unwinds a partial success in C's order (output first,
/// then input).
///
/// The sequencing around `size` is C's and is not incidental: it is set as soon as the buffers exist
/// (L107) and put back to zero if engine initialisation then fails (L118), so that a later call
/// retries the whole block rather than half of it.
///
/// C also zeroes `strm->zalloc`, `strm->zfree` and `strm->opaque` here (L110-L112). Those fields do
/// not exist in this implementation -- `crate::gz::state` records why -- but the two cursor assignments beside
/// them (L113-L114) do, and are reproduced.
///
/// # Errors
///
/// [`ReturnCode::MEM_ERROR`], with the message recorded, for either allocation. Both C sites report
/// exactly `"out of memory"` (L104 and L119).
fn gz_look_allocate<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
) -> Result<(), ReturnCode> {
    if state.allocate_read_buffers().is_err() {
        gz_error(state, ReturnCode::MEM_ERROR, Some(OUT_OF_MEMORY));
        return Err(ReturnCode::MEM_ERROR);
    }
    state.set_size(state.want());

    // L113-L114: no input has been examined yet.
    let stream = state.stream_mut();
    stream.avail_in = 0;
    stream.next_in = 0;

    // L115: `inflateInit2(&(state->strm), 15 + 16)` -- "gunzip".
    let allocator = *state.allocator();
    let engine = inflate_init2(InflateConfig::new(GUNZIP_WINDOW_BITS), allocator)
        .and_then(|engine| state.install_inflate(engine));
    if engine.is_err() {
        state.release_buffers();
        state.set_size(0);
        gz_error(state, ReturnCode::MEM_ERROR, Some(OUT_OF_MEMORY));
        return Err(ReturnCode::MEM_ERROR);
    }
    Ok(())
}

/// Resets the decompressor so that it starts on a fresh gzip member.
///
/// Implements the three `inflateReset(strm)` calls in `gz_look` (`gzread.c` L128 and L154). C
/// ignores the return value, because the only way `inflateReset` can fail is a state the stream
/// cannot be in here; this implementation instead reports a missing engine, which is the same condition and is
/// equally unreachable.
///
/// `inflateReset` also writes the `z_stream` half of the reset. Of those five fields this layer keeps
/// only `msg` (`crate::gz::state` records why the others are absent), so that is the one applied.
///
/// # Errors
///
/// [`ReturnCode::STREAM_ERROR`] if no decompressor is installed.
fn reset_engine<'a, A: Allocator<'a>>(state: &mut GzState<'a, A>) -> Result<(), ReturnCode> {
    let Some(engine) = state.inflate_state_mut() else {
        return Err(ReturnCode::STREAM_ERROR);
    };
    let reset = inflate_reset(engine);
    state.stream_mut().msg = reset.msg;
    Ok(())
}

/// Decides whether the stream is a gzip member to decompress or data to copy through.
///
/// Implements `gz_look` (`gzread.c` L84-L170), and the single most caller-visible function in this
/// module: what it decides is what `gzdirect` reports (`zlib.h` L1727-L1730), and whether a file is
/// decompressed at all.
///
/// `state->x.have` must be zero on entry, as C's comment at L84 requires. On return `how` is one of:
///
/// | `how` | Meaning |
/// |---|---|
/// | [`LOOK`] | no decision yet -- there was not enough input; try again later |
/// | [`COPY`] | not a gzip member; the leftover input has been copied to the output buffer |
/// | [`GZIP`] | a member is starting and the engine has been reset for it |
///
/// # The four decisions, in C's order
///
/// 1. **Skip detection entirely** (L127-L133) when `direct == -1`, which is the `G` mode letter
///    asking for gzip only (`gzlib.c` L157), or when `junk == 0`, which means a member has already
///    been decompressed and this is the search for the next one. `junk` then becomes
///    `state->junk != -1` -- reproduced literally, because the two reasons for taking this branch
///    want different answers: the `G` case has never seen a member and must stay a candidate for
///    zero, while the continuation case is already known to be gzip.
/// 2. **Wait for more input** (L143-L146) when nothing is buffered, or when a non-blocking read
///    stalled with fewer than four bytes. Returning without deciding is what makes `gzdirect` answer
///    1 on a slow device until four bytes have accumulated (`zlib.h` L1734-L1743).
/// 3. **A gzip member** (L151-L158) when the first four bytes satisfy [`looks_like_gzip`].
/// 4. **Transparent copy** (L161-L169) otherwise. C's own justification is worth keeping: the copy
///    "assumes that the output buffer is larger than the input buffer, which also assures space for
///    `gzungetc()`" -- see this module's note on the buffer asymmetry.
///
/// # Errors
///
/// * [`ReturnCode::MEM_ERROR`] from the first-use allocation.
/// * Whatever [`gz_avail`] reports while filling the input buffer.
/// * [`ReturnCode::STREAM_ERROR`] for an inconsistent state, which C cannot express.
pub(crate) fn gz_look<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
) -> Result<(), ReturnCode> {
    if state.size() == 0 {
        gz_look_allocate(state)?;
    }

    if state.direct() == -1 || state.junk() == 0 {
        reset_engine(state)?;
        state.set_how(GZIP);
        state.set_junk(i32::from(state.junk() != -1));
        state.set_direct(0);
        return Ok(());
    }

    // L139-L142: an empty input is not an error -- it is a transparent read of zero bytes.
    gz_avail(state)?;

    let avail_in = state.stream().avail_in;
    if avail_in == 0 || (state.again() && avail_in < 4) {
        return Ok(());
    }

    // L148-L153: the four-byte heuristic, which `crate::gz::header` owns.
    let detected = {
        let start = state.stream().next_in;
        let end = start.saturating_add(widen(avail_in));
        let buffer = state.in_slice();
        looks_like_gzip(buffer.get(start..end).unwrap_or(&[]))
    };
    if detected {
        reset_engine(state)?;
        state.set_how(GZIP);
        state.set_junk(1);
        state.set_direct(0);
        return Ok(());
    }

    // L161-L166: copy the leftover input into the output buffer, which is the larger of the two.
    {
        let GzState {
            buffer_slot, strm, ..
        } = state;
        let (input, output) = split_buffers(buffer_slot);
        let len = widen(strm.avail_in);
        let start = strm.next_in;
        let end = start.saturating_add(len);
        let source = input.map_or(&[][..], |buffer| buffer.as_slice());
        let destination = output.map_or(&mut [][..], Buffer::as_mut_slice);
        let (Some(source), Some(destination)) =
            (source.get(start..end), destination.get_mut(..len))
        else {
            // The output buffer is `2 * size` bytes and `avail_in` never exceeds `size`, so this is
            // unreachable; reporting beats trusting.
            gz_error(state, ReturnCode::STREAM_ERROR, Some(STATE_CORRUPT));
            return Err(ReturnCode::STREAM_ERROR);
        };
        destination.copy_from_slice(source);
    }
    // L164 and L166 together: the delivered window is the whole copy, starting at the front.
    state.set_output_window(0, avail_in)?;
    state.stream_mut().avail_in = 0;
    state.set_how(COPY);
    Ok(())
}

/// Runs the engine once over the buffered input, writing into `target`.
///
/// Implements the single `inflate(strm, Z_NO_FLUSH)` call at `gzread.c` L200 together with the
/// cursor bookkeeping around it. C keeps the four cursors in the embedded `z_stream` and hands the
/// decoder their addresses; this implementation keeps the same four values in [`GzStream`] and rebuilds the
/// slice pair for each call, because `crate::inflate` takes whole buffers plus cursors.
///
/// The windows are sliced to exactly `next + avail`, which is what makes
/// [`InflateStream::avail_in`] and [`InflateStream::avail_out`] -- both defined as
/// `len - cursor` -- agree with C's `avail_in` and `avail_out` on return.
///
/// `total_in`, `total_out`, `adler` and `data_type` start at zero on every call and are discarded,
/// and the copies [`GzStream`] carries are left untouched. That is faithful, not lossy: this layer
/// never reads any of them (none of the four names occurs anywhere in `gzlib.c`, `gzread.c`,
/// `gzwrite.c` or `gzclose.c`), and the decoder only ever writes them -- its own running check
/// lives in its state, not in the stream. The compressor is the opposite case, which is why
/// `crate::gz::write` threads all four through the same fields: `deflate` keeps the running CRC-32
/// of the member there and writes it verbatim into the gzip trailer.
///
/// # Errors
///
/// [`ReturnCode::STREAM_ERROR`] if no decompressor is installed, or if a cursor lies outside the
/// buffer it indexes. Neither is reachable through the public API.
fn inflate_step<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
    target: &mut DecompTarget<'_>,
) -> Result<ReturnCode, ReturnCode> {
    let GzState {
        buffer_slot, strm, ..
    } = state;
    let (input, output) = split_buffers(buffer_slot);
    let GzStream {
        avail_in,
        next_in,
        avail_out,
        next_out,
        msg,
        engine,
        ..
    } = strm;

    let engine = match engine {
        GzEngine::Inflate(boxed) => boxed.get_mut(),
        GzEngine::None | GzEngine::Deflate(_) => None,
    };
    let Some(engine) = engine else {
        return Err(ReturnCode::STREAM_ERROR);
    };

    let in_end = next_in.saturating_add(widen(*avail_in));
    let Some(source) = input
        .map_or(&[][..], |buffer| buffer.as_slice())
        .get(..in_end)
    else {
        return Err(ReturnCode::STREAM_ERROR);
    };

    // ★ The window starts **at** `next_out` and the stream's own cursor starts at zero, which
    // is what C's advancing `strm->next_out` pointer amounts to. Two things follow, and both
    // matter: the decoder's two read-backs of its own output -- the check value at
    // `inflate.c` L1080 and the window update at L1136 -- span exactly this call's output,
    // and every write a write-only sub-region receives lands at or after its own base, which
    // is what keeps [`OutputRegion`]'s high-water promise exact.
    let room = widen(*avail_out);
    let destination = match target {
        DecompTarget::Internal => output
            .map_or(&mut [][..], Buffer::as_mut_slice)
            .get_mut(*next_out..next_out.saturating_add(room))
            .map(OutputRegion::init),
        DecompTarget::User(region) => Some(region.reborrow(*next_out, room)),
    };
    let Some(destination) = destination else {
        return Err(ReturnCode::STREAM_ERROR);
    };

    let mut stream = InflateStream::with_region(source, destination);
    stream.next_in = *next_in;
    stream.msg = *msg;

    let ret = inflate(engine, &mut stream, Z_NO_FLUSH);

    *next_in = stream.next_in;
    *next_out = next_out.saturating_add(stream.next_out);
    // Both counts shrank from values that already fitted in a `c_uint`, so neither narrowing can
    // fail; reporting beats silently substituting a different count.
    *avail_in = narrow(stream.avail_in()).ok_or(ReturnCode::STREAM_ERROR)?;
    *avail_out = narrow(stream.avail_out()).ok_or(ReturnCode::STREAM_ERROR)?;
    *msg = stream.msg;
    Ok(ret)
}

/// Decompresses into `target` until it is full or the member ends.
///
/// Implements `gz_decomp` (`gzread.c` L172-L240). The caller sets `avail_out` and `next_out` first,
/// exactly as `gz_fetch` (L266-L267) and `gz_read` (L373-L374) do, and passes the matching
/// destination.
///
/// # The three things this function decides
///
/// * **When a stall is not an error.** Running out of input raises
///   `Z_BUF_ERROR "unexpected end of file"` only when `again` is clear (L193-L197). On a
///   non-blocking device the same condition is a stall the caller may retry, and `zlib.h`
///   L1468-L1478 turns that distinction into the documented contract that `Z_BUF_ERROR` from
///   `gzerror` means "the input file ended in the middle of a gzip stream" and can be cleared with
///   `gzclearerr`.
/// * **When trailing garbage is acceptable.** A `Z_DATA_ERROR` with `junk == 1` means a gzip header
///   was seen but nothing was ever decompressed from it, so what follows a completed member is not a
///   member at all. C then declares end of file, goes back to [`LOOK`] and reports success
///   (L213-L220), which is exactly the promise at `zlib.h` L1465-L1466. With `junk == 0` the same
///   status is a genuine corruption and the engine's own message is forwarded, falling back to
///   `"compressed data error"` when it has none (L221-L222).
/// * **When a member has ended.** `Z_STREAM_END` sets `junk = 0` and returns to [`LOOK`]
///   (L231-L236), which is the multi-member continuation: the next [`gz_fetch`] looks for another
///   member and, finding none, falls through to the garbage rule above.
///
/// # What it reports
///
/// [`Progress::count`] is C's `had - strm->avail_out` (L228): the bytes written this call, valid
/// whether or not the call succeeded. For [`DecompTarget::Internal`] the delivered window is also
/// published, which is C's `x.next = strm->next_out - x.have` (L229). For
/// [`DecompTarget::User`] nothing is published: C's pointer would then denote a location inside the
/// *caller's* buffer, and `gz_read` throws it away one line later with `x.have = 0`
/// (L376-L377), so the count travels as a value instead. The end state is identical and no pointer
/// ever leaves the output buffer -- see `crate::gz::state`'s fact 2.
pub(crate) fn gz_decomp<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
    mut target: DecompTarget<'_>,
) -> Progress {
    let mut ret = ReturnCode::OK;
    let had = state.stream().avail_out;

    loop {
        // L188-L192: get more input for `inflate`.
        if state.stream().avail_in == 0 && gz_avail(state).is_err() {
            ret = state.err_code().unwrap_or(ReturnCode::STREAM_ERROR);
            break;
        }
        if state.stream().avail_in == 0 {
            if !state.again() {
                gz_error(state, ReturnCode::BUF_ERROR, Some(UNEXPECTED_EOF));
            }
            break;
        }

        match inflate_step(state, &mut target) {
            Ok(code) => ret = code,
            Err(code) => {
                gz_error(state, code, Some(STATE_CORRUPT));
                ret = code;
                break;
            }
        }

        // L201-L203: any decompressed data marks this as a real gzip stream.
        if state.stream().avail_out < had {
            state.set_junk(0);
        }

        if ret == ReturnCode::STREAM_ERROR || ret == ReturnCode::NEED_DICT {
            gz_error(state, ReturnCode::STREAM_ERROR, Some(INFLATE_CORRUPT));
            break;
        }
        if ret == ReturnCode::MEM_ERROR {
            gz_error(state, ReturnCode::MEM_ERROR, Some(OUT_OF_MEMORY));
            break;
        }
        if ret == ReturnCode::DATA_ERROR {
            if state.junk() == 1 {
                // L214-L219: trailing garbage is ok.
                state.stream_mut().avail_in = 0;
                state.set_eof(true);
                state.set_how(LOOK);
                ret = ReturnCode::OK;
                break;
            }
            let text = state.stream().msg;
            let message = text.map_or(COMPRESSED_DATA_ERROR, str::as_bytes);
            gz_error(state, ReturnCode::DATA_ERROR, Some(message));
            break;
        }

        if state.stream().avail_out == 0 || ret == ReturnCode::STREAM_END {
            break;
        }
    }

    let produced = had.saturating_sub(state.stream().avail_out);
    if matches!(target, DecompTarget::Internal) {
        let cursor = state.stream().next_out;
        let start = cursor.saturating_sub(widen(produced));
        if state.set_output_window(start, produced).is_err() {
            gz_error(state, ReturnCode::STREAM_ERROR, Some(STATE_CORRUPT));
            return Progress {
                count: 0,
                result: Err(ReturnCode::STREAM_ERROR),
            };
        }
    }

    if ret == ReturnCode::STREAM_END {
        state.set_junk(0);
        state.set_how(LOOK);
        return Progress {
            count: produced,
            result: Ok(()),
        };
    }

    let result = if ret == ReturnCode::OK {
        Ok(())
    } else {
        Err(ret)
    };
    Progress {
        count: produced,
        result,
    }
}

/// Fills the output buffer, looking for a header first if one is due.
///
/// Implements `gz_fetch` (`gzread.c` L242-L277). `x.have` is zero on entry, and on return it is
/// either non-zero or the input is exhausted in both the file and the buffer, which is the loop
/// condition at L275.
///
/// C's `switch (state->how)` becomes an exhaustive `match` over [`GzHow`], so a `how` value that is
/// added later cannot be silently ignored -- AAP §0.3.3.1's reason for modelling these closed sets as
/// enums. C's `default:` arm survives as the [`None`] case, which is reached only for a `how` that is
/// not one of the three: `"state corrupt"` (L271-L273).
///
/// # Errors
///
/// * Whatever `gz_look`, [`gz_load`] or `gz_decomp` reports.
/// * [`ReturnCode::STREAM_ERROR`] for an unrecognised `how`, or for a size that cannot be doubled
///   into a `c_uint`.
///
/// # A behaviour divergence in the [`COPY`] arm -- divergence 9 of the `gz/open.rs` inventory
///
/// C assigns the byte count straight into `x.have` through `gz_load`'s out-parameter but sets
/// `x.next = state->out` only after checking for failure (L260-L263), so a read that fails *after*
/// delivering bytes leaves a count paired with a stale cursor -- and `gz_read`, which proceeds
/// whenever `x.have != 0` (L359-L361), then hands the application bytes from the wrong place. This
/// port publishes cursor and count together in both cases, so the delivered bytes are the ones that
/// were just read.
///
/// That is **not** presented as an improvement: it is an observable difference on the failing path,
/// and behaviour preservation is this port's governing constraint. It is recorded as **forced and
/// unresolved**, because reproducing C exactly would mean handing the application bytes from a
/// stale cursor -- a read of memory the count does not describe. See the inventory in
/// `gz/open.rs` for the full list and for what "forced, unresolved" means there.
pub(crate) fn gz_fetch<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
) -> Result<(), ReturnCode> {
    loop {
        match GzHow::from_raw(state.how()) {
            Some(GzHow::Look) => {
                gz_look(state)?;
                if state.how() == LOOK {
                    return Ok(());
                }
            }
            Some(GzHow::Copy) => {
                let len = widen(state.size()).saturating_mul(2);
                let progress = gz_load(state, LoadTarget::Output { len });
                state.set_output_window(0, progress.count)?;
                progress.result?;
                return Ok(());
            }
            Some(GzHow::Gzip) => {
                let avail_out = narrow(widen(state.size()).saturating_mul(2))
                    .ok_or(ReturnCode::STREAM_ERROR)?;
                let stream = state.stream_mut();
                stream.avail_out = avail_out;
                stream.next_out = 0;
                gz_decomp(state, DecompTarget::Internal).result?;
            }
            None => {
                gz_error(state, ReturnCode::STREAM_ERROR, Some(STATE_CORRUPT));
                return Err(ReturnCode::STREAM_ERROR);
            }
        }

        if state.have() != 0 || (state.eof() && state.stream().avail_in == 0) {
            return Ok(());
        }
    }
}

/// Discards `skip` bytes of uncompressed output, or stops at end of file.
///
/// Implements `gz_skip` (`gzread.c` L279-L309), which is how a deferred `gzseek` request is
/// eventually carried out: whatever is already in the output buffer is thrown away first, and the
/// rest is produced and thrown away, because a gzip stream cannot be positioned any other way
/// (`zlib.h` L1671-L1675 calls the emulation "extremely slow" for exactly this reason).
///
/// The clamp at L288-L290 is [`clamp_to_have`], and the position moves forward by what was discarded
/// even though the bytes are never delivered -- `x.pos` counts stream position, not bytes handed out.
///
/// # Errors
///
/// Whatever [`gz_fetch`] reports, or [`ReturnCode::STREAM_ERROR`] if the output cursor cannot
/// advance by the clamped amount, which the clamp itself makes unreachable.
pub(crate) fn gz_skip<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
) -> Result<(), ReturnCode> {
    // L284-L307: skip over `skip` bytes or reach end of file, whichever comes first.
    loop {
        if state.have() != 0 {
            let n = clamp_to_have(state.have(), state.skip());
            state.advance_out(widen(n))?;
            state.add_pos(ZOff64::from(n));
            state.set_skip(state.skip().saturating_sub(ZOff64::from(n)));
        } else if state.eof() && state.stream().avail_in == 0 {
            // L297-L299: output buffer empty and the input is finished.
            break;
        } else {
            gz_fetch(state)?;
        }

        if state.skip() == 0 {
            break;
        }
    }
    Ok(())
}

/// Reads up to `buf.len()` uncompressed bytes into `buf`, and returns how many arrived.
///
/// Implements `gz_read` (`gzread.c` L311-L393), the engine behind every public read entry point.
///
/// # The deferred-error contract
///
/// C's own comment (L311-L316) is the contract, and it is what `zlib.h` L1485-L1489 promises
/// callers: a zero return means *either* end of file *or* an error, and the recorded error
/// distinguishes them. If bytes were produced before an error, the count is returned and the error
/// stays recorded for the next call to report. The latch at L346-L348 is what implements the second
/// half -- after copying out of the output buffer the loop checks whether a previous [`gz_fetch`]
/// left an error behind, and stops without discarding the bytes it just delivered.
///
/// # Why small reads go through the output buffer
///
/// The `how == LOOK || n < (size << 1)` test (L357) is not an optimisation detail: it decides when
/// `x.have` and `x.next` are populated, and therefore whether the caller's `gzgetc` **macro** can
/// take its fast path at all. It also guarantees, as C's comment at L363-L364 says, that the copy
/// above leaves space in the output buffer so at least one `gzungetc` can succeed. Changing the test
/// changes behaviour in code this implementation cannot recompile.
///
/// A request at least as large as the whole output buffer skips the double-buffering and reads or
/// decompresses straight into `buf` (L367-L378).
pub(crate) fn gz_read<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
    dest: &mut OutputRegion<'_>,
) -> usize {
    let mut len = dest.len();
    if len == 0 {
        return 0;
    }

    // L326-L328: a pending seek is carried out before anything is delivered.
    if state.skip() != 0 && gz_skip(state).is_err() {
        return 0;
    }

    let mut got = 0_usize;
    let mut failed = false;
    let mut offset = 0_usize;

    loop {
        // L334-L337: the largest part of `len` that fits in an `unsigned`.
        let mut n = len.min(widen(c_uint::MAX));
        // C's `continue` at L362 jumps to the loop condition, skipping the progress update; this
        // flag is that jump.
        let mut refilled = false;

        if state.have() != 0 {
            // L339-L349: first just try copying data from the output buffer.
            n = n.min(widen(state.have()));
            let end = offset.saturating_add(n);
            let copied = match state.available_out().get(..n) {
                Some(source) => dest.write_slice_at(offset, source),
                None => false,
            };
            let _ = end;
            if !copied || state.advance_out(n).is_err() {
                // Unreachable: `n` is clamped to both `have` and the space left in `buf`.
                break;
            }
            // L346-L348: caught deferred error from `gz_fetch`.
            if state.err() != ReturnCode::OK.as_i32() {
                failed = true;
            }
        } else if state.eof() && state.stream().avail_in == 0 {
            // L351-L353: output buffer empty and the input is finished.
            break;
        } else if state.how() == LOOK || n < widen(state.size()).saturating_mul(2) {
            // L355-L365: for a small request or a new stream, fill our own output buffer so that
            // `gzgetc` stays fast and a `gzungetc` still has room.
            if gz_fetch(state).is_err() && state.have() == 0 {
                // L359-L361: with `x.have != 0` the error is caught after the copy instead.
                failed = true;
            }
            refilled = true;
        } else if state.how() == COPY {
            // L367-L369: large request, transparent stream -- read straight into the user buffer.
            let window = dest.reborrow(offset, n);
            if window.len() != n {
                break;
            }
            let progress = gz_load(state, LoadTarget::User(window));
            n = widen(progress.count);
            if progress.result.is_err() {
                failed = true;
            }
        } else {
            // L371-L378: large request, gzip stream -- decompress straight into the user buffer.
            let Some(avail_out) = narrow(n) else {
                break;
            };
            let stream = state.stream_mut();
            stream.avail_out = avail_out;
            stream.next_out = 0;
            let window = dest.reborrow(offset, n);
            if window.len() != n {
                break;
            }
            let progress = gz_decomp(state, DecompTarget::User(window));
            // L376-L377: `n = state->x.have; state->x.have = 0;`. The count comes back as a value
            // because nothing was published into the layer's own buffer; clearing the pair keeps the
            // exposed prefix in the state C leaves it in.
            n = widen(progress.count);
            if progress.result.is_err() {
                failed = true;
            }
            state.clear_have();
        }

        if !refilled {
            // L380-L384: update progress.
            len = len.saturating_sub(n);
            offset = offset.saturating_add(n);
            got = got.saturating_add(n);
            state.add_pos(to_off(n));
        }

        if len == 0 || failed {
            break;
        }
    }

    // L387-L389: note read past end of file.
    if len != 0 && state.eof() {
        state.set_past(true);
    }

    got
}

/// The entry guard every public read driver applies: C's mode and error checks.
///
/// Three of C's four checks; the fourth, `file == NULL`, is the facade's, because a `&mut GzState`
/// is proof that the structure exists. What remains is:
///
/// 1. `state->mode != GZ_READ` -- the integrity check `gzguts.h` L158 describes, which also rejects a
///    write stream handed to a read function (`gzread.c` L403-L404);
/// 2. [`GzState::resync_from_exposed`], which has no C counterpart and recovers the output cursor the
///    caller's `gzgetc` macro has been advancing;
/// 3. `state->err != Z_OK && state->err != Z_BUF_ERROR && !state->again` (L407-L408), followed by
///    `gz_error(state, Z_OK, NULL)` to clear the error for this call (L409).
///
/// Returns `false` when the caller must fail immediately, in which case nothing has been changed.
fn enter_read<'a, A: Allocator<'a>>(state: &mut GzState<'a, A>) -> bool {
    if state.mode() != GZ_READ {
        return false;
    }
    if state.resync_from_exposed().is_err() {
        return false;
    }
    if !err_permits_reading(state.err()) && !state.again() {
        return false;
    }
    gz_error(state, ReturnCode::OK, None);
    true
}

/// Reads and decompresses up to `buf.len()` bytes, returning the count or -1.
///
/// Implements `gzread` (`gzread.c` L395-L436), declared at `zlib.h` L1456. `buf` is the caller's
/// `(buf, len)` pair; the facade rebuilds it as a slice, and its length is C's `unsigned len`.
///
/// # The two failure reports
///
/// * **A length that cannot be returned.** C tests `(int)len < 0`, i.e. a request beyond `INT_MAX`,
///   and refuses it with `Z_STREAM_ERROR` "request does not fit in an int" (L411-L416) -- "this
///   avoids a flaw in the interface", since the count is returned in an `int`.
/// * **A stall that is not end of file.** With nothing produced and `again` set, C reports
///   `Z_ERRNO` and -1 so the application can tell a stalled non-blocking device from a finished file
///   (L425-L431); `zlib.h` L1480-L1483 documents that `errno` is then `EAGAIN` or `EWOULDBLOCK`.
///
/// # What is deliberately *not* reported here
///
/// An incomplete gzip stream. `zlib.h` L1474-L1478 is explicit: `gzread` does not return -1 for it.
/// The `Z_BUF_ERROR` `gz_decomp` recorded stays recorded, `gzerror` can be consulted for it, and
/// `gzclose` is what finally returns it.
pub fn gzread<'a, A: Allocator<'a> + Copy>(state: &mut GzState<'a, A>, buf: &mut [u8]) -> c_int {
    gzread_into(state, &mut OutputRegion::init(buf))
}

/// [`gzread`] over a destination that may be write-only storage.
///
/// The entry point `crates/libz-rs-sys` uses, because a C caller's `buf` is guaranteed
/// writable and nothing more -- `zlib.h` L1456-L1463 asks for `len` bytes of room and says
/// nothing about their contents. Identical in behaviour to [`gzread`], which is a one-line
/// forwarder to it over an [`OutputRegion::init`].
pub fn gzread_into<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
    dest: &mut OutputRegion<'_>,
) -> c_int {
    if !enter_read(state) {
        return -1;
    }

    if !request_fits_in_int(dest.len()) {
        gz_error(state, ReturnCode::STREAM_ERROR, Some(REQUEST_EXCEEDS_INT));
        state.refresh_exposed();
        return -1;
    }

    let got = gz_read(state, dest);

    if got == 0 {
        if !err_permits_reading(state.err()) {
            state.refresh_exposed();
            return -1;
        }
        if state.again() {
            let message = errno_message(None);
            gz_error(state, ReturnCode::ERRNO, Some(message.as_bytes()));
            state.refresh_exposed();
            return -1;
        }
    }

    state.refresh_exposed();
    // L434-L435. The length check above proves the count fits.
    c_int::try_from(got).unwrap_or(-1)
}

/// Reads and decompresses up to `nitems` items of `size` bytes, returning the count of full items.
///
/// Implements `gzfread` (`gzread.c` L438-L465), declared at `zlib.h` L1492. It duplicates
/// `fread`'s interface, so the product `size * nitems` is the byte count and the return value is
/// whole items.
///
/// # The overflow check lives here
///
/// C computes `len = nitems * size` and rejects the request when `size && len / size != nitems`
/// (L456-L461), which is precisely a checked multiplication. The check must be *this* function's,
/// because recording the error needs `gz_error`, which is crate-internal: the facade cannot report it.
/// So the facade passes the `size` and `nitems` it was given, together with the largest buffer it
/// could build for them -- `size.checked_mul(nitems)` bytes, or an empty slice when that product
/// overflows, in which case this function rejects the request before looking at the buffer at all.
///
/// # And so does the buffer-length check
///
/// `buf` must be at least `size * nitems` bytes. A shorter slice is a `Z_STREAM_ERROR` and zero
/// items, recorded through `gz_error` and leaving the stream position untouched -- **not** a read of
/// whatever was offered. C has no way to detect the condition, so there is no C behaviour to match
/// here; the choice is between refusing and silently answering a smaller question. Refusing is the
/// only one a caller can act on, because `gzfread` reports whole items and a short count is exactly
/// what end of file looks like. `gzfwrite` refuses the mirror-image request the same way
/// (`crate::gz::write`), so a facade sees one rule in both directions.
///
/// # The partial final item
///
/// `zlib.h` L1508-L1516 documents that a trailing partial item is still copied into the buffer and
/// the end-of-file flag set, matching common `fread` implementations, and that the count returned
/// does not include it. That falls out of the integer division at L464 rather than being coded
/// separately; the partial length can be recovered with `gztell`.
pub fn gzfread<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
    buf: &mut [u8],
    size: usize,
    nitems: usize,
) -> usize {
    gzfread_into(state, &mut OutputRegion::init(buf), size, nitems)
}

/// [`gzfread`] over a destination that may be write-only storage.
///
/// The entry point `crates/libz-rs-sys` uses; see [`gzread_into`] for why the shape differs
/// from the slice form, which forwards to this one unchanged.
pub fn gzfread_into<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
    dest: &mut OutputRegion<'_>,
    size: usize,
    nitems: usize,
) -> usize {
    if !enter_read(state) {
        return 0;
    }

    let Some(len) = nitems.checked_mul(size) else {
        gz_error(
            state,
            ReturnCode::STREAM_ERROR,
            Some(REQUEST_EXCEEDS_SIZE_T),
        );
        state.refresh_exposed();
        return 0;
    };

    if len == 0 || size == 0 {
        state.refresh_exposed();
        return 0;
    }
    // The buffer must hold the whole product. C cannot check this -- it has a bare pointer -- and
    // this port can, so it does, *before* reading anything. Substituting the shorter slice was the
    // alternative and it is worse than refusing: the read would succeed, the stream would advance,
    // and the caller would receive an item count that is indistinguishable from end of file while
    // the bytes it asked for were never delivered.
    let mut window = dest.reborrow(0, len);
    if window.len() != len {
        gz_error(state, ReturnCode::STREAM_ERROR, Some(REQUEST_PAST_BUFFER));
        state.refresh_exposed();
        return 0;
    }
    let got = gz_read(state, &mut window);
    state.refresh_exposed();
    got / size
}

/// Reads and decompresses one byte, returning it or -1.
///
/// Implements `gzgetc` (`gzread.c` L467-L498), declared at `zlib.h` L1613.
///
/// Callers do not normally arrive here. `gzgetc` is also a macro (`zlib.h` L1967-L1968) whose fast
/// path serves the byte out of the exposed prefix in the caller's own object code, and the function
/// is reached only when `have` is zero -- or when the caller took the address of `gzgetc`, or
/// `#undef`ined the macro. The fast path below is therefore the *same* fast path, reproduced for the
/// function form: read the byte at the cursor, then decrement `have`, increment `pos` and advance the
/// cursor (L490-L494).
pub fn gzgetc<'a, A: Allocator<'a> + Copy>(state: &mut GzState<'a, A>) -> c_int {
    if !enter_read(state) {
        return -1;
    }

    // L489-L494: try the output buffer. No need to check for a skip request -- `x.have != 0` can
    // only happen after one has been carried out.
    if state.have() != 0 {
        let byte = state.available_out().first().copied();
        let Some(byte) = byte else {
            // Unreachable: `have != 0` means the window is non-empty.
            state.refresh_exposed();
            return -1;
        };
        if state.advance_out(1).is_err() {
            state.refresh_exposed();
            return -1;
        }
        state.add_pos(1);
        state.refresh_exposed();
        return c_int::from(byte);
    }

    // L496-L497: nothing there -- try `gz_read`.
    let mut buf = [0_u8; 1];
    let got = gz_read(state, &mut OutputRegion::init(&mut buf));
    state.refresh_exposed();
    match buf.first() {
        Some(&byte) if got >= 1 => c_int::from(byte),
        _ => -1,
    }
}

/// The plain function form of [`gzgetc`].
///
/// Implements `gzgetc_` (`gzread.c` L500-L502). It exists because `gzgetc` is a macro: `zlib.h`
/// L1961 declares this name so that a library built before the macro existed still links, and the
/// macro's own fallback arm calls `(gzgetc)(g)`, which resolves to the exported function. It must
/// remain reachable and must do nothing but delegate.
pub fn gzgetc_<'a, A: Allocator<'a> + Copy>(state: &mut GzState<'a, A>) -> c_int {
    gzgetc(state)
}

/// Pushes one byte back so that it is read first next time, returning it or -1.
///
/// Implements `gzungetc` (`gzread.c` L504-L563), declared at `zlib.h` L1630. At least one push is
/// always allowed, and immediately after opening the whole output buffer is available for pushing
/// (`zlib.h` L1633-L1637), because the buffer is `2 * size` bytes and starts empty.
///
/// # Why the pending seek is carried out before `c` is rejected
///
/// `zlib.h` L1641-L1644 documents `gzungetc(-1, file)` as the way to **force a pending seek to
/// execute**, so that `gztell` reports the resulting position even when the seek reached end of file
/// -- which is how a program measures a gzip file's uncompressed length without buffering it. That
/// only works because C processes the skip request at L524-L526 *before* rejecting a negative `c` at
/// L528-L530. The order is part of the contract, not an accident.
///
/// # The five cases
///
/// 1. Just opened with nothing buffered: `gz_look` runs first so the buffers exist (L515-L517).
/// 2. Empty buffer: the byte is parked at `2 * size - 1`, the very last position, which "allows more
///    pushing" (L532-L540).
/// 3. Completely full buffer: `Z_DATA_ERROR` "out of room to push characters" (L542-L546).
/// 4. Cursor at the front: the buffered data slides to the end of the buffer to make room in front of
///    it (L548-L556).
/// 5. Room in front: the byte goes immediately before the existing data (L557-L562).
///
/// In every accepting case `pos` moves back by one and `past` is cleared, so a push after end of file
/// makes the stream readable again.
pub fn gzungetc<'a, A: Allocator<'a> + Copy>(state: &mut GzState<'a, A>, c: c_int) -> c_int {
    if state.mode() != GZ_READ {
        return -1;
    }
    if state.resync_from_exposed().is_err() {
        return -1;
    }

    // L515-L517: in case this was just opened, set up the input buffer. C discards the result, and
    // so does this: a failure is picked up by the error check immediately below.
    if state.how() == LOOK && state.have() == 0 {
        let _ = gz_look(state);
    }

    if !err_permits_reading(state.err()) && !state.again() {
        state.refresh_exposed();
        return -1;
    }
    gz_error(state, ReturnCode::OK, None);

    if state.skip() != 0 && gz_skip(state).is_err() {
        state.refresh_exposed();
        return -1;
    }

    // L528-L530: can't push end of file.
    if c < 0 {
        state.refresh_exposed();
        return -1;
    }
    // C's `(unsigned char)c`: the low byte is stored, while the value returned is the argument as
    // given (L536 and L562).
    let byte = u8::try_from(c & 0xFF).unwrap_or(0);

    // L532-L540: if the output buffer is empty, put the byte at the end.
    if state.have() == 0 {
        let Some(index) = state.ungetc_park_index() else {
            // No buffers: unreachable, because `gz_look` above either allocated them or recorded an
            // error the check above returned on.
            state.refresh_exposed();
            return -1;
        };
        if state.set_output_window(index, 1).is_err() {
            state.refresh_exposed();
            return -1;
        }
        let Some(slot) = state.available_out_mut().first_mut() else {
            state.refresh_exposed();
            return -1;
        };
        *slot = byte;
        state.add_pos(-1);
        state.set_past(false);
        state.refresh_exposed();
        return c;
    }

    // L542-L546: no room, which means a push has already been made and not read.
    let doubled = widen(state.size()).saturating_mul(2);
    if widen(state.have()) == doubled {
        gz_error(state, ReturnCode::DATA_ERROR, Some(NO_ROOM_TO_PUSH));
        state.refresh_exposed();
        return -1;
    }

    // L548-L556: slide the buffered data to the end of the buffer if it is against the front.
    if state.out_pos() == 0 {
        let have = widen(state.have());
        let Some(start) = doubled.checked_sub(have) else {
            state.refresh_exposed();
            return -1;
        };
        {
            let buffer = state.out_slice_mut();
            if start.saturating_add(have) > buffer.len() {
                state.refresh_exposed();
                return -1;
            }
            // C copies backwards, byte by byte, because source and destination overlap;
            // `copy_within` is the same move with the overlap handled for us.
            buffer.copy_within(0..have, start);
        }
        if state.set_output_window(start, state.have()).is_err() {
            state.refresh_exposed();
            return -1;
        }
    }

    // L557-L561: insert the byte before the existing data.
    let Some(position) = state.out_pos().checked_sub(1) else {
        state.refresh_exposed();
        return -1;
    };
    if state
        .set_output_window(position, state.have().saturating_add(1))
        .is_err()
    {
        state.refresh_exposed();
        return -1;
    }
    let Some(slot) = state.available_out_mut().first_mut() else {
        state.refresh_exposed();
        return -1;
    };
    *slot = byte;
    state.add_pos(-1);
    state.set_past(false);
    state.refresh_exposed();
    c
}

/// Reads one line into `buf`, stopping at a newline, at `buf.len() - 1` bytes, or at end of file.
///
/// Implements `gzgets` (`gzread.c` L565-L624), declared at `zlib.h` L1588. `buf` is the caller's
/// `(buf, len)` pair, so `buf.len()` is C's `len` and the terminating zero is written inside it.
///
/// Returns the number of data bytes copied, with a zero byte written just past them, or [`None`] when
/// nothing at all was copied -- C's `NULL`, which means end of file or an error. As with [`gzread`],
/// data already read is delivered before an error is reported: the next call returns [`None`] for it
/// (`zlib.h` L1597-L1600).
///
/// # Two details worth stating
///
/// * **The contents are not checked for a zero byte.** C says so explicitly at L617-L619: an embedded
///   zero is copied like any other byte, and a caller treating the result as a C string will see a
///   short line. The `"hello, hello!\0"` fixture in `test/example.c` relies on this.
/// * **`len == 1` returns [`None`].** C computes `left = len - 1`, skips its loop when `left` is
///   zero, and then returns `NULL` because nothing was written (L592-L621) -- so the terminating zero
///   is *not* written either. `zlib.h` L1592-L1593 reads as though it were; the implementation is the
///   oracle, and this implementation matches the implementation.
pub fn gzgets<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
    buf: &mut [u8],
) -> Option<usize> {
    gzgets_into(state, &mut OutputRegion::init(buf))
}

/// [`gzgets`] over a destination that may be write-only storage.
///
/// The entry point `crates/libz-rs-sys` uses; see [`gzread_into`] for why the shape differs
/// from the slice form, which forwards to this one unchanged. Nothing here reads the
/// destination back: the newline is looked for in the layer's own output buffer, exactly as
/// `gzread.c` L602-L606 does, so the caller's bytes are only ever written.
pub fn gzgets_into<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
    dest: &mut OutputRegion<'_>,
) -> Option<usize> {
    // L572-L578: C's `buf == NULL || len < 1`. An empty region is both.
    if dest.is_empty() {
        return None;
    }
    if !enter_read(state) {
        return None;
    }

    if state.skip() != 0 && gz_skip(state).is_err() {
        state.refresh_exposed();
        return None;
    }

    let mut written = 0_usize;
    let mut left = dest.len().saturating_sub(1);

    while left != 0 {
        if state.have() == 0 && gz_fetch(state).is_err() {
            break;
        }
        // L597-L600: end of file -- return what we have.
        if state.have() == 0 {
            state.set_past(true);
            break;
        }

        // L602-L606: look for the end of a line in the delivered window.
        let limit = widen(state.have()).min(left);
        let eol = state
            .available_out()
            .get(..limit)
            .and_then(|window| window.iter().position(|&byte| byte == b'\n'));
        let n = match eol {
            Some(index) => index.saturating_add(1),
            None => limit,
        };

        // L608-L614: copy through the end of the line, or the whole window if there was none.
        let end = written.saturating_add(n);
        let copied = match state.available_out().get(..n) {
            Some(source) => dest.write_slice_at(written, source),
            None => false,
        };
        if !copied || state.advance_out(n).is_err() {
            // Unreachable: `n` is at most `left`, which is what remains of `buf`, and at most
            // `have`, which is what remains of the window.
            break;
        }
        state.add_pos(to_off(n));
        left = left.saturating_sub(n);
        written = end;

        // L615: the loop stops at a newline.
        if eol.is_some() {
            break;
        }
    }

    if written == 0 {
        state.refresh_exposed();
        return None;
    }
    // L620: `buf[written] = 0;` -- the terminator, which is why `left` was `len - 1`.
    dest.write_byte_at(written, 0);
    state.refresh_exposed();
    Some(written)
}

/// Closes a read stream, releasing everything it owns.
///
/// Implements `gzclose_r` (`gzread.c` L644-L667), declared at `zlib.h` L1763. The teardown order is
/// C's and is observable: `test/infcover.c`'s tracking allocator reports a release that is not
/// last-in-first-out (L200-L234), so the engine goes first, then the output buffer, then the input
/// buffer.
///
/// C's final `free(state)` is not performed here, because the structure was allocated by whoever owns
/// it -- `crates/libz-rs-sys` -- and is released by dropping it after this returns. Everything else is
/// done, and [`GzState`]'s own `Drop` finds nothing left, so the two cannot double-release.
///
/// # What the return value means
///
/// * [`ReturnCode::STREAM_ERROR`] for a stream that is not open for reading.
/// * [`ReturnCode::ERRNO`] when closing the file failed.
/// * [`ReturnCode::BUF_ERROR`] when the last read ended in the middle of a gzip stream. This is the
///   deferred report `zlib.h` L1474-L1478 and L1758-L1760 promise: `gzread` does not signal a
///   truncated stream, and `gzclose` is where it finally surfaces.
/// * [`ReturnCode::OK`] otherwise.
pub fn gzclose_r<'a, A: Allocator<'a> + Copy>(state: &mut GzState<'a, A>) -> ReturnCode {
    if state.mode() != GZ_READ {
        return ReturnCode::STREAM_ERROR;
    }

    // L656-L661: free memory. `size != 0` is exactly "the buffers and the engine exist".
    if state.size() != 0 {
        if let GzEngine::Inflate(boxed) = state.take_engine() {
            if let Some(mut engine) = boxed.into_inner() {
                // `inflateEnd` cannot fail for an owned state; C ignores its result too.
                let _ = inflate_end(&mut engine);
            }
        }
        state.release_buffers();
    }

    // L662: the deferred truncation report.
    let err = if state.err() == ReturnCode::BUF_ERROR.as_i32() {
        ReturnCode::BUF_ERROR
    } else {
        ReturnCode::OK
    };
    gz_error(state, ReturnCode::OK, None);
    state.clear_path();

    // L665: `ret = close(state->fd);`. `GzFileSlot::close` reports the underlying handle's
    // result unchanged, so a facade handle backed by a real `close(2)` answers `Z_ERRNO` exactly
    // when C does. An empty slot reports success; C cannot reach that, since `gzclose` may not be
    // called twice on one stream.
    let closed = state.take_handle().close().is_ok();

    if closed {
        err
    } else {
        ReturnCode::ERRNO
    }
}

/// Rewinds a read stream to where its gzip data started.
///
/// Implements `gzrewind` (`gzlib.c` L345-L364), declared at `zlib.h` L1683, which documents it as
/// equivalent to `(int)gzseek(file, 0L, SEEK_SET)`.
///
/// Reading only, and only while the recorded error still permits it. The file is repositioned to
/// `state->start` -- which `gz_open` recorded so that a stream opened on a descriptor already part
/// way into a file rewinds to the right place (`gzlib.c` L273-L279) -- and then `gz_reset` puts every
/// read-path flag back to its just-opened value, including `junk = -1`, so the next read looks for a
/// header again.
///
/// Returns 0, or -1 if the mode, the error state or the seek forbids it.
pub fn gzrewind<'a, A: Allocator<'a>>(state: &mut GzState<'a, A>) -> c_int {
    if state.mode() != GZ_READ || !err_permits_reading(state.err()) {
        return -1;
    }
    if state.resync_from_exposed().is_err() {
        return -1;
    }

    let start = state.start();
    let sought = match state.handle_mut() {
        Some(mut handle) => handle.seek(start, GzSeekFrom::Start).is_ok(),
        None => false,
    };
    if !sought {
        state.refresh_exposed();
        return -1;
    }

    // L362-L363. `gz_reset` re-derives the exposed prefix itself; the call below is the uniform
    // contract of this module and costs one recomputation.
    gz_reset(state);
    state.refresh_exposed();
    0
}

/// Sets the position for the next read, in uncompressed bytes.
///
/// Implements `gzseek64` (`gzlib.c` L366-L435), the 64-bit form behind both `gzseek` and `gzseek64`
/// (`zlib.h` L1663 and L2018). `whence` is the raw C argument: only `SEEK_SET` and `SEEK_CUR` are
/// accepted, because the uncompressed length of a gzip stream is unknown without decompressing it, so
/// `SEEK_END` "is not supported" (`zlib.h` L1668-L1669).
///
/// Returns the resulting position, or -1 on error. Actual seeking is **deferred**: the request is
/// recorded in `skip` and carried out by `gz_skip` at the next read (`zlib.h` L1674-L1675), which is
/// why the returned value is `pos + offset` rather than something read back from the file.
///
/// # The four paths, in C's order
///
/// 1. **Normalisation** (L387-L393). `SEEK_SET` becomes `offset -= pos`. `SEEK_CUR` adds any pending
///    seek that has not yet run -- unless `past` is set, in which case the pending skip has already
///    been clipped by end of file -- and then clears it. Both arithmetic steps are **checked**, and
///    the absolute landing position is established as representable, before any field is written: a
///    request that cannot be normalised is answered with -1 and leaves the stream byte for byte as
///    it was, pending skip included. C wraps instead, which turns a `SEEK_SET` of `i64::MIN` into an
///    eight-exabyte forward skip that `gz_skip` then attempts.
/// 2. **The raw-area fast path** (L395-L409), taken only while reading a transparent stream. Here a
///    real seek is possible, so the file moves by `offset - have`, the buffered output and the flags
///    are discarded, and the new position is returned immediately.
/// 3. **Backwards** (L411-L420). Only a read stream can go back: the offset is re-expressed from the
///    start of the stream, rejected if it lands before it, and the stream is rewound so the skip can
///    run forwards.
/// 4. **Consume what is buffered** (L422-L430), so that the pending skip is as small as possible and,
///    as C's comment says, so there is "one less `gzgetc()` check".
///
/// `test/example.c` asserts `gzseek(file, -8L, SEEK_CUR) == 6` and then `gztell(file) == pos` on a
/// 14-byte stream that has been read to the end -- a concrete, non-negotiable check of the
/// normalisation and of the `skip` accounting, since the first call rewinds and defers a 6-byte skip
/// which the second must include.
pub fn gzseek64<'a, A: Allocator<'a>>(
    state: &mut GzState<'a, A>,
    offset: ZOff64,
    whence: c_int,
) -> ZOff64 {
    let mut offset = offset;

    if state.mode() != GZ_READ && state.mode() != GZ_WRITE {
        return -1;
    }
    if state.resync_from_exposed().is_err() {
        return -1;
    }
    if !err_permits_reading(state.err()) {
        return -1;
    }
    // L383-L393, normalisation. Every arithmetic step below is checked, and **all of it happens
    // before a single field is written**, because the contract this function offers on failure is
    // that the stream is left exactly as it was: a rejected `gzseek` must not have consumed the
    // pending skip, discarded the output window, or moved the file.
    //
    // C's `offset -= state->x.pos` / `offset += state->skip` are signed additions on a
    // caller-supplied `z_off64_t` and overflow them for extreme inputs -- undefined behaviour, which
    // in practice wraps. Wrapping is the worst possible answer here, not merely an imprecise one: at
    // `pos == 1`, `SEEK_SET` with `i64::MIN` wraps to `i64::MAX`, so "seek to the most negative
    // position imaginable" becomes "skip forward eight exabytes", and `gz_skip` then sets about
    // actually doing it, decompressing the whole stream to end of file. A request whose normalised
    // form is not representable cannot be honoured at all, so it is refused with -1.
    //
    // L383-L385: `SEEK_END` and anything unrecognised are refused.
    let (normalised, consumes_pending_skip) = match GzSeekFrom::from_raw(whence) {
        // L387-L389: `offset -= state->x.pos`.
        Some(GzSeekFrom::Start) => (offset.checked_sub(state.pos()), false),
        // L390-L393: `offset += state->past ? 0 : state->skip`, and then `state->skip = 0`. The
        // clearing is deferred to after every check, which is why the two statements are separated
        // here: a caller whose request is refused keeps the deferred seek it already had.
        Some(GzSeekFrom::Current) => {
            let pending = if state.past() { 0 } else { state.skip() };
            (offset.checked_add(pending), true)
        }
        Some(GzSeekFrom::End) | None => return -1,
    };
    let Some(normalised) = normalised else {
        return -1;
    };
    offset = normalised;

    // The absolute position this request will land on, computed once, here, while the stream is
    // still untouched. `pos + offset` is **invariant** across everything that follows -- the raw
    // fast path adds `offset` to `pos`, the backwards branch moves `pos` into `offset` and rewinds
    // `pos` to zero, and the buffered-skip loop adds `n` to `pos` while subtracting it from
    // `offset` -- so this single value is simultaneously C's `state->x.pos + offset` guard at L397,
    // its `offset < 0` "before start of file" test at L416, and its return value at L434. Computing
    // it up front is what makes an unrepresentable target refusable before any mutation, instead of
    // discovered after the output window has already been thrown away.
    let Some(landing) = state.pos().checked_add(offset) else {
        return -1;
    };

    // The last of the normalisation, now that nothing can refuse the request on arithmetic grounds.
    if consumes_pending_skip {
        state.set_skip(0);
    }

    // L395-L409: if within the raw area while reading, just go there. C's `state->x.pos + offset >= 0`
    // is exactly `landing >= 0`.
    if state.mode() == GZ_READ && state.how() == COPY && landing >= 0 {
        // L398: `LSEEK(state->fd, offset - (z_off64_t)state->x.have, SEEK_CUR)`. `have` is a
        // `c_uint` widened to 64 bits, so this can only underflow when `offset` is already within
        // `u32::MAX` of `i64::MIN`, which `landing >= 0` all but excludes. It is checked rather than
        // assumed so that no input can produce a wrapped displacement, and the refusal happens
        // before the window and the flags are discarded, so the stream is untouched.
        let Some(delta) = offset.checked_sub(ZOff64::from(state.have())) else {
            state.refresh_exposed();
            return -1;
        };
        let sought = match state.handle_mut() {
            Some(mut handle) => handle.seek(delta, GzSeekFrom::Current).is_ok(),
            None => false,
        };
        if !sought {
            state.refresh_exposed();
            return -1;
        }
        // L401-L407. `clear_have` is C's `state->x.have = 0` with the cursor nulled alongside, which
        // is what `crate::gz::state` requires of a discarded window.
        state.clear_have();
        state.set_eof(false);
        state.set_past(false);
        state.set_skip(0);
        gz_error(state, ReturnCode::OK, None);
        state.stream_mut().avail_in = 0;
        state.add_pos(offset);
        state.refresh_exposed();
        return state.pos();
    }

    // L411-L420: rewind if this is a backwards seek while reading.
    if offset < 0 {
        // L413-L414: writing cannot go backwards.
        if state.mode() != GZ_READ {
            state.refresh_exposed();
            return -1;
        }
        // L415: `offset += state->x.pos`, re-expressing the request from the start of the stream --
        // which is precisely `landing`, already computed and already known to be representable.
        offset = landing;
        // L416-L417: before the start of the file.
        if offset < 0 {
            state.refresh_exposed();
            return -1;
        }
        if gzrewind(state) == -1 {
            state.refresh_exposed();
            return -1;
        }
    }

    // L422-L430: while reading, skip what is already in the output buffer.
    if state.mode() == GZ_READ {
        let n = clamp_to_have(state.have(), offset);
        if state.advance_out(widen(n)).is_err() {
            state.refresh_exposed();
            return -1;
        }
        state.add_pos(ZOff64::from(n));
        // L429: `offset -= n`. `clamp_to_have` never returns more than `offset` itself when `offset`
        // is non-negative, and `offset >= 0` holds on every path that reaches here -- a negative one
        // was either rejected above or turned into the non-negative `landing` -- so the subtraction
        // cannot underflow. It is checked rather than wrapped so the guarantee is enforced instead of
        // relied upon, and the invariant `pos + offset == landing` is preserved because the same `n`
        // was just added to `pos`.
        let Some(remaining) = offset.checked_sub(ZOff64::from(n)) else {
            state.refresh_exposed();
            return -1;
        };
        offset = remaining;
    }

    // L432-L434: request the skip and report where it will land. C computes `state->x.pos + offset`
    // here; `landing` is that value, established as representable before anything was modified.
    state.set_skip(offset);
    state.refresh_exposed();
    landing
}

/// The position the next read or write will start at, in uncompressed bytes.
///
/// Implements `gztell64` (`gzlib.c` L445-L458), the 64-bit form behind both `gztell` and `gztell64`
/// (`zlib.h` L1691 and L2019). Any deferred seek is included, which is what makes the
/// `gzungetc(-1, file)` idiom work: force the seek to run, then ask where it ended up
/// (`zlib.h` L1641-L1644). A seek that has already been clipped by end of file sets `past`, and its
/// leftover skip is then *not* counted.
///
/// Returns the position, or -1 for a structure that is not an open stream.
///
/// # Why this one does not resynchronise the exposed prefix
///
/// It reads `pos`, `past` and `skip`, and the first of those *is* a field of the exposed prefix: the
/// caller's `gzgetc` macro increments `x.pos` in place, so this function sees every byte the macro
/// consumed without any conversion. Only `x.next` is derived state, and nothing here touches it --
/// which is also why an immutable borrow suffices.
#[must_use]
pub fn gztell64<'a, A: Allocator<'a>>(state: &GzState<'a, A>) -> ZOff64 {
    if state.mode() != GZ_READ && state.mode() != GZ_WRITE {
        return -1;
    }
    let pending = if state.past() { 0 } else { state.skip() };
    state.pos().saturating_add(pending)
}

/// The current offset in the compressed file, in actual bytes.
///
/// Implements `gzoffset64` (`gzlib.c` L468-L487), the 64-bit form behind both `gzoffset` and
/// `gzoffset64` (`zlib.h` L1702 and L2020). It includes any bytes that precede the gzip data, which
/// is what makes it useful as a progress indicator when appending or when reading from a descriptor
/// positioned part way into a file (`zlib.h` L1704-L1708).
///
/// While reading, buffered but unconsumed input is subtracted, because those bytes have been taken
/// from the file but not yet accounted for in the stream.
///
/// Returns the offset, or -1 if the structure is not an open stream or the file is not seekable.
///
/// Like [`gztell64`], this reads no derived state -- only `avail_in` and the handle's own position --
/// so it neither resynchronises nor refreshes the exposed prefix. It cannot: an offset in the
/// compressed file has nothing to do with the output cursor.
pub fn gzoffset64<'a, A: Allocator<'a>>(state: &mut GzState<'a, A>) -> ZOff64 {
    if state.mode() != GZ_READ && state.mode() != GZ_WRITE {
        return -1;
    }

    let offset = match state.handle_mut() {
        Some(mut handle) => handle.seek(0, GzSeekFrom::Current),
        None => return -1,
    };
    let Ok(offset) = offset else {
        return -1;
    };

    // L484-L486: don't count buffered input.
    if state.mode() == GZ_READ {
        let buffered = ZOff64::from(state.stream().avail_in);
        return offset.saturating_sub(buffered);
    }
    offset
}

/// The round-trip narrowing behind C's `ret == (z_off_t)ret ? (z_off_t)ret : -1`.
///
/// `gzseek`, `gztell` and `gzoffset` are thin wrappers that call their `*64` forms and narrow the
/// result, refusing a value the narrower type cannot hold (`gzlib.c` L438-L443, L461-L466,
/// L490-L495). Both families are exported, because `zconf.h` redirects the unsuffixed names according
/// to the **caller's** `_FILE_OFFSET_BITS` and `_LARGEFILE64_SOURCE` (`zlib.h` L1976-L2018), so which
/// one a given program references is decided when that program is compiled, not when this library is.
///
/// The narrowing itself belongs to `crates/libz-rs-sys`, because only the facade knows how wide
/// `z_off_t` is on the target -- `zconf.h` L494-L531 resolves it to `off_t`, `off64_t`, `__int64`,
/// `offset_t` or `long long` depending on the platform, and no fixed width may be assumed for it
/// anywhere. This helper is the check, so that the facade's wrapper is
/// `narrow_offset::<z_off_t>(gzseek64(..)).unwrap_or(-1)` and the `-1` convention stays in one place.
#[must_use]
pub fn narrow_offset<T: TryFrom<ZOff64>>(value: ZOff64) -> Option<T> {
    T::try_from(value).ok()
}

/// Whether a byte count can be returned in a C `int`, i.e. C's `(int)len >= 0`.
///
/// Split out of [`gzread`] so that the refusal at `gzread.c` L413-L416 can be exercised without
/// allocating the two gibibytes a slice of that length would need.
fn request_fits_in_int(len: usize) -> bool {
    c_int::try_from(len).is_ok()
}

#[cfg(test)]
mod tests {
    // The crate denies the panic-prone lints in library code, which is right there and wrong here:
    // an assertion that fails panics, and a test fixture that cannot index is unreadable. Scoped to
    // this module, which ships in no build.
    #![allow(
        clippy::unwrap_used,
        clippy::indexing_slicing,
        clippy::panic,
        clippy::too_many_lines
    )]

    use super::{
        clamp_to_have, gz_fetch, gz_look, gzclose_r, gzgetc, gzgetc_, gzgets, gzread, gzrewind,
        gzseek64, gztell64, gzungetc, narrow_offset, request_fits_in_int, MAX_READ_CHUNK,
    };
    use crate::allocate::GlobalAllocator;
    use crate::error::ReturnCode;
    use crate::gz::gzclearerr;
    use crate::gz::state::{
        GzFileSlot, GzHandle, GzIoError, GzSeekFrom, GzState, ZOff64, COPY, GZIP, GZ_READ,
        GZ_WRITE, LOOK,
    };
    use alloc::boxed::Box;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::Cell;
    use core::ffi::c_int;

    /// `"hello, hello!\0"` as a gzip member, produced by zlib itself at the default level.
    ///
    /// The exact payload `test/example.c`'s `test_gzio` writes: `strlen(hello) + 1` bytes, the
    /// trailing zero included (L95 and L165).
    const HELLO_GZ: [u8; 31] = [
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0xcb, 0x48, 0xcd, 0xc9, 0xc9,
        0xd7, 0x51, 0xc8, 0x00, 0x51, 0x8a, 0x0c, 0x00, 0x9d, 0x3f, 0x6c, 0xb5, 0x0e, 0x00, 0x00,
        0x00,
    ];

    /// The payload [`HELLO_GZ`] decompresses to.
    const HELLO: &[u8] = b"hello, hello!\0";

    /// `"first half\n"` as a gzip member.
    const MEMBER_A: [u8; 31] = [
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x4b, 0xcb, 0x2c, 0x2a, 0x2e,
        0x51, 0xc8, 0x48, 0xcc, 0x49, 0xe3, 0x02, 0x00, 0x92, 0xf5, 0x72, 0xa5, 0x0b, 0x00, 0x00,
        0x00,
    ];

    /// `"second half\n"` as a gzip member.
    const MEMBER_B: [u8; 32] = [
        0x1f, 0x8b, 0x08, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x03, 0x2b, 0x4e, 0x4d, 0xce, 0xcf,
        0x4b, 0x51, 0xc8, 0x48, 0xcc, 0x49, 0xe3, 0x02, 0x00, 0xcf, 0x3a, 0xdb, 0xb5, 0x0c, 0x00,
        0x00, 0x00,
    ];

    /// `"hello, hello!"` compressed by the system `gzip(1)` rather than by zlib.
    ///
    /// `FLG` is 0x08, so the member carries an `FNAME` field (`named.txt\0`) and a real `MTIME`,
    /// neither of which [`HELLO_GZ`] has. Reading it proves the interoperability direction
    /// "produced elsewhere, consumed here" on a member whose optional header fields are populated.
    const GZIP_CLI_NAMED: [u8; 40] = [
        0x1f, 0x8b, 0x08, 0x08, 0x13, 0x7f, 0x74, 0x6a, 0x00, 0x03, 0x6e, 0x61, 0x6d, 0x65, 0x64,
        0x2e, 0x74, 0x78, 0x74, 0x00, 0xcb, 0x48, 0xcd, 0xc9, 0xc9, 0xd7, 0x51, 0xc8, 0x00, 0x51,
        0x8a, 0x00, 0x9b, 0xdc, 0x9a, 0xb3, 0x0d, 0x00, 0x00, 0x00,
    ];

    /// What one [`MemoryFile`] read should do.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    enum Behaviour {
        /// Deliver as much as was asked for and is available.
        Whole,
        /// Deliver at most one byte per call, which is what a pipe or a socket may do and what
        /// `gz_load`'s loop exists for (`gzread.c` L10-L12).
        DribbleOneByte,
        /// Stall with `EAGAIN` on every read, delivering nothing.
        AlwaysStall,
        /// Deliver `first` bytes on the first read and stall on every one after that.
        StallAfter(usize),
    }

    /// An in-memory stand-in for the file a `gzFile` reads: this module's [`GzHandle`] fixture.
    ///
    /// Deliberately free of any real I/O so that every test here runs under Miri.
    struct MemoryFile<'c> {
        data: Vec<u8>,
        position: usize,
        behaviour: Behaviour,
        reads: Cell<usize>,
        closes: &'c Cell<usize>,
    }

    impl<'c> MemoryFile<'c> {
        fn new(data: &[u8], behaviour: Behaviour, closes: &'c Cell<usize>) -> Self {
            Self {
                data: data.to_vec(),
                position: 0,
                behaviour,
                reads: Cell::new(0),
                closes,
            }
        }
    }

    /// The `EAGAIN` a stalled read reports. The number is Linux's; only `would_block` is acted on.
    const EAGAIN: i32 = 11;

    impl GzHandle for MemoryFile<'_> {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, GzIoError> {
            let sequence = self.reads.get();
            self.reads.set(sequence + 1);
            let allowance = match self.behaviour {
                Behaviour::Whole => buf.len(),
                Behaviour::DribbleOneByte => buf.len().min(1),
                Behaviour::AlwaysStall => return Err(GzIoError::new(EAGAIN, true)),
                Behaviour::StallAfter(first) => {
                    if sequence == 0 {
                        buf.len().min(first)
                    } else {
                        return Err(GzIoError::new(EAGAIN, true));
                    }
                }
            };
            let available = self.data.len().saturating_sub(self.position);
            let count = allowance.min(available);
            buf[..count].copy_from_slice(&self.data[self.position..self.position + count]);
            self.position += count;
            Ok(count)
        }

        fn write(&mut self, _buf: &[u8]) -> Result<usize, GzIoError> {
            Err(GzIoError::new(0, false))
        }

        fn seek(&mut self, offset: ZOff64, whence: GzSeekFrom) -> Result<ZOff64, GzIoError> {
            let base = match whence {
                GzSeekFrom::Start => 0,
                GzSeekFrom::Current => ZOff64::try_from(self.position).unwrap(),
                GzSeekFrom::End => ZOff64::try_from(self.data.len()).unwrap(),
            };
            let target = base + offset;
            if target < 0 {
                return Err(GzIoError::new(0, false));
            }
            self.position = usize::try_from(target).unwrap();
            Ok(target)
        }

        fn set_nonblocking(&mut self, _nonblocking: bool) -> Result<(), GzIoError> {
            Ok(())
        }

        fn close(&mut self) -> Result<(), GzIoError> {
            self.closes.set(self.closes.get() + 1);
            Ok(())
        }
    }

    /// A read stream over `data`, in the condition `gz_open(path, "rb")` leaves one in.
    ///
    /// `mode` is [`GZ_READ`] and `direct` is 1, which is `gz_open`'s "start with a transparent
    /// assumption in case of an empty file" (`gzlib.c` L186-L189).
    fn reader<'c>(
        data: &[u8],
        want: u32,
        behaviour: Behaviour,
        closes: &'c Cell<usize>,
    ) -> GzState<'c, GlobalAllocator> {
        let mut state = GzState::new(GlobalAllocator);
        state.set_mode(GZ_READ);
        state.set_direct(1);
        state.set_want(want);
        state.try_set_path(b"memory").unwrap();
        let previous = state.set_handle(GzFileSlot::Boxed(Box::new(MemoryFile::new(
            data, behaviour, closes,
        ))));
        assert!(!previous.is_installed(), "the slot was empty before");
        state
    }

    /// The default `GZBUFSIZE`-sized reader, which is what every `gzopen` produces.
    fn default_reader<'c>(data: &[u8], closes: &'c Cell<usize>) -> GzState<'c, GlobalAllocator> {
        reader(data, 8192, Behaviour::Whole, closes)
    }

    /// `gzread`'s `int` count as an index, which every assertion below needs.
    fn count(got: c_int) -> usize {
        usize::try_from(got).unwrap()
    }

    #[test]
    fn max_read_chunk_is_the_c_expression() {
        assert_eq!(MAX_READ_CHUNK, (u32::MAX >> 2) + 1);
        assert_eq!(MAX_READ_CHUNK, 0x4000_0000);
    }

    #[test]
    fn the_clamp_is_a_minimum_on_this_target() {
        // `gzread.c` L288-L290 with `GT_OFF` false: `min(have, amount)`.
        assert_eq!(clamp_to_have(10, 4), 4);
        assert_eq!(clamp_to_have(4, 10), 4);
        assert_eq!(clamp_to_have(0, 10), 0);
        assert_eq!(clamp_to_have(7, 7), 7);
    }

    #[test]
    fn reads_one_gzip_member() {
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        let mut buf = [0_u8; 64];
        assert_eq!(gzread(&mut state, &mut buf), 14);
        assert_eq!(&buf[..14], HELLO);
        // A decompressed member is not a transparent read.
        assert_eq!(state.direct(), 0);
        assert_eq!(state.err(), ReturnCode::OK.as_i32());
        assert_eq!(state.pos(), 14);
        assert_eq!(gzclose_r(&mut state), ReturnCode::OK);
        assert_eq!(closes.get(), 1);
    }

    #[test]
    fn reads_a_member_produced_by_the_system_gzip() {
        let closes = Cell::new(0);
        let mut state = default_reader(&GZIP_CLI_NAMED, &closes);
        let mut buf = [0_u8; 64];
        assert_eq!(gzread(&mut state, &mut buf), 13);
        assert_eq!(&buf[..13], b"hello, hello!");
        assert_eq!(state.err(), ReturnCode::OK.as_i32());
    }

    #[test]
    fn reads_two_concatenated_members_as_one_stream() {
        // `zlib.h` L1462-L1464: any number of members may be concatenated.
        let mut data = MEMBER_A.to_vec();
        data.extend_from_slice(&MEMBER_B);
        let closes = Cell::new(0);
        let mut state = default_reader(&data, &closes);
        let mut buf = [0_u8; 64];
        assert_eq!(gzread(&mut state, &mut buf), 23);
        assert_eq!(&buf[..23], b"first half\nsecond half\n");
        assert_eq!(state.err(), ReturnCode::OK.as_i32());
        assert_eq!(gzclose_r(&mut state), ReturnCode::OK);
    }

    #[test]
    fn ignores_trailing_garbage_after_a_member() {
        // `zlib.h` L1465-L1466: trailing garbage is ignored and no error is returned. This is the
        // `junk == 1` arm of `gz_decomp` (`gzread.c` L213-L220).
        let mut data = HELLO_GZ.to_vec();
        data.extend_from_slice(b"this is not a gzip member");
        let closes = Cell::new(0);
        let mut state = default_reader(&data, &closes);
        let mut buf = [0_u8; 64];
        assert_eq!(gzread(&mut state, &mut buf), 14);
        assert_eq!(&buf[..14], HELLO);
        assert_eq!(state.err(), ReturnCode::OK.as_i32());
        assert!(state.eof());
        assert_eq!(state.how(), LOOK);
        // A second read finds nothing more and still reports no error.
        assert_eq!(gzread(&mut state, &mut buf), 0);
        assert_eq!(state.err(), ReturnCode::OK.as_i32());
        assert_eq!(gzclose_r(&mut state), ReturnCode::OK);
    }

    #[test]
    fn a_truncated_member_defers_buf_error_to_close() {
        // `zlib.h` L1474-L1478: `gzread` does not return -1 for an incomplete stream; `gzclose`
        // returns `Z_BUF_ERROR`.
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ[..27], &closes);
        let mut buf = [0_u8; 64];
        let got = gzread(&mut state, &mut buf);
        assert_eq!(
            got, 14,
            "the payload itself is complete; the trailer is not"
        );
        assert_eq!(state.err(), ReturnCode::BUF_ERROR.as_i32());
        assert_eq!(gzclose_r(&mut state), ReturnCode::BUF_ERROR);
    }

    #[test]
    fn a_header_with_nothing_decodable_behind_it_is_taken_for_junk() {
        // A valid gzip header followed by nonsense. `junk` is 1 from the moment the header is
        // detected (`gzread.c` L156) and only becomes 0 once a byte has actually been decompressed
        // (L203), so a `Z_DATA_ERROR` here takes the trailing-garbage arm: no error is reported and
        // the file is simply at its end. The assertions below are that outcome.
        let mut data = HELLO_GZ.to_vec();
        for byte in data.iter_mut().skip(10) {
            *byte = 0xff;
        }
        let closes = Cell::new(0);
        let mut state = default_reader(&data, &closes);
        let mut buf = [0_u8; 64];
        assert_eq!(gzread(&mut state, &mut buf), 0);
        assert_eq!(state.err(), ReturnCode::OK.as_i32());
        assert!(state.eof());
        assert!(state.past());
        assert_eq!(state.direct(), 0);
        assert_eq!(gzclose_r(&mut state), ReturnCode::OK);
    }

    #[test]
    fn transparent_read_of_non_gzip_input() {
        let closes = Cell::new(0);
        let mut state = default_reader(b"plain, uncompressed text", &closes);
        let mut buf = [0_u8; 64];
        assert_eq!(gzread(&mut state, &mut buf), 24);
        assert_eq!(&buf[..24], b"plain, uncompressed text");
        // What `gzdirect` reports: transparent.
        assert_eq!(state.direct(), 1);
        assert_eq!(state.how(), COPY);
        assert_eq!(state.err(), ReturnCode::OK.as_i32());
    }

    #[test]
    fn an_empty_file_reads_as_empty_and_stays_transparent() {
        let closes = Cell::new(0);
        let mut state = default_reader(b"", &closes);
        let mut buf = [0_u8; 8];
        assert_eq!(gzread(&mut state, &mut buf), 0);
        // `gz_look` returns without deciding, so the transparent assumption `gz_open` installed
        // survives -- `gzlib.c` L186-L189 chose it for exactly this case.
        assert_eq!(state.direct(), 1);
        assert_eq!(state.how(), LOOK);
        assert_eq!(state.err(), ReturnCode::OK.as_i32());
        assert!(state.eof());
        assert!(state.past(), "the read asked for more than the file had");
    }

    #[test]
    fn a_gzip_only_stream_never_reads_transparently() {
        // `direct == -1` is the `G` mode letter (`gzlib.c` L157): detection is skipped and the
        // engine is used whatever the data looks like (`gzread.c` L127-L133).
        // So non-gzip input is a `Z_DATA_ERROR` with "incorrect header check", not a transparent
        // read, and `direct` is cleared on the way.
        let closes = Cell::new(0);
        let mut state = reader(b"not gzip at all", 8192, Behaviour::Whole, &closes);
        state.set_direct(-1);
        let mut buf = [0_u8; 32];
        assert_eq!(gzread(&mut state, &mut buf), -1);
        assert_eq!(state.err(), ReturnCode::DATA_ERROR.as_i32());
        assert_eq!(state.direct(), 0);
        let message = state.msg().unwrap_or_default();
        assert_eq!(message, b"memory: incorrect header check");
        assert_eq!(gzclose_r(&mut state), ReturnCode::OK);
    }

    #[test]
    fn a_dribbling_handle_is_read_in_a_loop() {
        // `gz_load` must loop, because one read need not return everything asked for
        // (`gzread.c` L10-L12).
        let closes = Cell::new(0);
        let mut state = reader(&HELLO_GZ, 8192, Behaviour::DribbleOneByte, &closes);
        let mut buf = [0_u8; 64];
        assert_eq!(gzread(&mut state, &mut buf), 14);
        assert_eq!(&buf[..14], HELLO);
    }

    #[test]
    fn gz_look_waits_for_four_bytes_on_a_stalled_device() {
        // `gzread.c` L143-L146, and the `gzdirect` prose at `zlib.h` L1734-L1743.
        let closes = Cell::new(0);
        let mut state = reader(&HELLO_GZ, 8192, Behaviour::StallAfter(2), &closes);
        gz_look(&mut state).unwrap();
        assert_eq!(state.how(), LOOK, "no decision on fewer than four bytes");
        assert!(state.again());
        assert_eq!(
            state.direct(),
            1,
            "still transparent as far as anyone can see"
        );
        assert_eq!(state.err(), ReturnCode::OK.as_i32());
    }

    #[test]
    #[cfg_attr(miri, ignore = "renders a message through the platform's strerror")]
    fn a_stall_with_nothing_to_show_is_reported_as_errno() {
        // `gzread.c` L425-L431: the application must be able to tell a stall from end of file.
        let closes = Cell::new(0);
        let mut state = reader(&HELLO_GZ, 8192, Behaviour::AlwaysStall, &closes);
        let mut buf = [0_u8; 64];
        assert_eq!(gzread(&mut state, &mut buf), -1);
        assert_eq!(state.err(), ReturnCode::ERRNO.as_i32());
        assert!(state.again());
        assert!(!state.eof(), "a stall is not end of file");
    }

    #[test]
    #[cfg_attr(miri, ignore = "renders a message through the platform's strerror")]
    fn a_device_that_stalls_before_four_bytes_terminates_rather_than_spinning() {
        // The liveness half of the non-blocking protocol. `gz_look` returns without deciding while
        // fewer than four bytes have arrived (`gzread.c` L143-L146), so `gz_read` goes round again
        // (L362's `continue`). That is only bounded because the *next* stall delivers nothing and is
        // therefore reported as `Z_ERRNO` (L38-L42) rather than being another silent retry.
        let closes = Cell::new(0);
        let mut state = reader(&HELLO_GZ, 8192, Behaviour::StallAfter(2), &closes);
        let mut buf = [0_u8; 64];
        assert_eq!(gzread(&mut state, &mut buf), -1);
        assert_eq!(state.err(), ReturnCode::ERRNO.as_i32());
    }

    #[test]
    fn arbitrary_input_never_panics_and_always_terminates() {
        // The property the `fuzz_gz_roundtrip` target asserts, in miniature: this module is the
        // library's front door for untrusted data, so every one of these must return rather than
        // panic, hang or read out of range.
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        let mut next = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for case in 0..200 {
            let len = usize::try_from(next() % 200).unwrap();
            let mut data: Vec<u8> = (0..len)
                .map(|_| u8::try_from(next() & 0xff).unwrap())
                .collect();
            // Half the cases start with a plausible gzip header, so the engine is entered.
            if case % 2 == 0 {
                let mut framed = HELLO_GZ[..HELLO_GZ.len().min(1 + case % 30)].to_vec();
                framed.append(&mut data);
                data = framed;
            }
            let closes = Cell::new(0);
            let mut state = reader(&data, 32, Behaviour::Whole, &closes);
            let mut out = [0_u8; 40];
            let mut guard = 0;
            loop {
                let got = gzread(&mut state, &mut out);
                guard += 1;
                assert!(guard < 10_000, "case {case} did not terminate");
                if got <= 0 {
                    break;
                }
            }
            // Whatever happened, the stream can still be interrogated and closed.
            let _ = gzgetc(&mut state);
            let _ = gzgets(&mut state, &mut out);
            let _ = gzungetc(&mut state, c_int::from(b'x'));
            let _ = gzseek64(&mut state, 3, GzSeekFrom::Start.as_raw());
            let _ = gztell64(&state);
            let _ = gzrewind(&mut state);
            let close = gzclose_r(&mut state);
            assert!(
                close == ReturnCode::OK
                    || close == ReturnCode::BUF_ERROR
                    || close == ReturnCode::ERRNO,
                "case {case} closed with {close:?}"
            );
        }
    }

    #[test]
    fn gzgetc_serves_bytes_one_at_a_time() {
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        for expected in HELLO {
            assert_eq!(gzgetc(&mut state), c_int::from(*expected));
        }
        assert_eq!(gzgetc(&mut state), -1);
        assert_eq!(state.pos(), 14);
    }

    #[test]
    fn gzgetc_underscore_delegates() {
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        assert_eq!(gzgetc_(&mut state), c_int::from(b'h'));
        assert_eq!(gzgetc_(&mut state), c_int::from(b'e'));
    }

    #[test]
    fn the_exposed_prefix_survives_a_macro_style_mutation() {
        // The `gzgetc` macro consumes a byte in the caller's own object code
        // (`zlib.h` L1967-L1968): `have--`, `pos++`, `*next++`. Reproduce it by hand and check the
        // next entry point picks up where the macro left off.
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        assert_eq!(gzgetc(&mut state), c_int::from(b'h'));

        let before = state.have();
        let byte = unsafe_free_macro_step(&mut state);
        assert_eq!(byte, b'e');
        assert_eq!(state.have(), before - 1);
        assert_eq!(state.pos(), 2);

        // The library was not told, so this call must resynchronise from the pointer.
        assert_eq!(gzgetc(&mut state), c_int::from(b'l'));
        assert_eq!(state.pos(), 3);

        let mut rest = [0_u8; 32];
        let got = count(gzread(&mut state, &mut rest));
        assert_eq!(&rest[..got], &HELLO[3..]);
    }

    /// Performs exactly what the `gzgetc` macro performs, without any `unsafe`.
    ///
    /// The macro reads through `x.next`, which cannot be done here; the byte is taken from the
    /// output window instead, and then the three exposed fields are mutated the way the macro
    /// mutates them -- `next` by the same pointer arithmetic, which is a safe operation.
    fn unsafe_free_macro_step(state: &mut GzState<'_, GlobalAllocator>) -> u8 {
        let byte = state.available_out()[0];
        state.x.have -= 1;
        state.x.pos += 1;
        state.x.next = state.x.next.wrapping_add(1);
        byte
    }

    #[test]
    fn gzungetc_parks_a_byte_at_the_end_of_an_empty_buffer() {
        // `gzread.c` L532-L540, and `zlib.h` L1633-L1637: at least one push is always allowed, and
        // immediately after opening the whole buffer is available.
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        assert_eq!(gzungetc(&mut state, c_int::from(b'X')), c_int::from(b'X'));
        assert_eq!(state.have(), 1);
        assert_eq!(state.out_pos(), 2 * 8192 - 1);
        assert_eq!(state.pos(), -1);
        assert_eq!(gzgetc(&mut state), c_int::from(b'X'));
        assert_eq!(state.pos(), 0);
        // And the stream itself is undisturbed.
        let mut buf = [0_u8; 32];
        let got = count(gzread(&mut state, &mut buf));
        assert_eq!(&buf[..got], HELLO);
    }

    #[test]
    fn gzungetc_slides_the_window_when_the_cursor_is_at_the_front() {
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        gz_fetch(&mut state).unwrap();
        assert_eq!(state.out_pos(), 0);
        let have = state.have();
        assert_eq!(have, 14);

        assert_eq!(gzungetc(&mut state, c_int::from(b'>')), c_int::from(b'>'));
        assert_eq!(state.have(), have + 1);
        // The data moved to the end of the buffer and the pushed byte sits just before it.
        assert_eq!(
            state.out_pos(),
            2 * 8192 - usize::try_from(have).unwrap() - 1
        );
        assert_eq!(state.pos(), -1);

        let mut buf = [0_u8; 32];
        let got = gzread(&mut state, &mut buf);
        assert_eq!(got, 15);
        assert_eq!(buf[0], b'>');
        assert_eq!(&buf[1..15], HELLO);
    }

    #[test]
    fn gzungetc_reports_a_completely_full_buffer() {
        // `gzread.c` L542-L546. With `want` at its floor the output buffer is 16 bytes, so one
        // transparent fetch fills it exactly.
        let closes = Cell::new(0);
        let data = vec![b'z'; 64];
        let mut state = reader(&data, 8, Behaviour::Whole, &closes);
        // The first fetch decides on the transparent path and delivers what `gz_look` copied.
        gz_fetch(&mut state).unwrap();
        assert_eq!(state.how(), COPY);
        let mut drain = [0_u8; 8];
        assert_eq!(gzread(&mut state, &mut drain), 8);
        // The next fetch loads the whole doubled buffer.
        gz_fetch(&mut state).unwrap();
        assert_eq!(state.have(), 16);
        assert_eq!(state.out_pos(), 0);

        assert_eq!(gzungetc(&mut state, c_int::from(b'!')), -1);
        assert_eq!(state.err(), ReturnCode::DATA_ERROR.as_i32());
    }

    #[test]
    fn gzungetc_refuses_end_of_file_but_still_runs_a_pending_seek() {
        // `zlib.h` L1641-L1644: `gzungetc(-1, file)` forces a pending seek to execute so that
        // `gztell` reports the position. That works only because `gz_skip` runs before `c < 0` is
        // rejected (`gzread.c` L524-L530).
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        assert_eq!(gzseek64(&mut state, 6, GzSeekFrom::Start.as_raw()), 6);
        assert_eq!(state.skip(), 6, "deferred, not yet performed");

        assert_eq!(gzungetc(&mut state, -1), -1);
        assert_eq!(state.skip(), 0, "the seek ran");
        assert_eq!(state.pos(), 6);
        assert_eq!(gztell64(&state), 6);
    }

    #[test]
    fn gzgets_stops_at_a_newline() {
        let mut data = MEMBER_A.to_vec();
        data.extend_from_slice(&MEMBER_B);
        let closes = Cell::new(0);
        let mut state = default_reader(&data, &closes);

        let mut line = [0_u8; 32];
        assert_eq!(gzgets(&mut state, &mut line), Some(11));
        assert_eq!(&line[..11], b"first half\n");
        assert_eq!(line[11], 0, "terminated just past the data");

        assert_eq!(gzgets(&mut state, &mut line), Some(12));
        assert_eq!(&line[..12], b"second half\n");

        // Nothing left: end of file is `None`, and `past` is latched.
        assert_eq!(gzgets(&mut state, &mut line), None);
        assert!(state.past());
    }

    #[test]
    fn gzgets_honours_the_length_limit() {
        // At most `len - 1` bytes are copied, and the remainder is delivered by the next call.
        let closes = Cell::new(0);
        let mut state = default_reader(&MEMBER_A, &closes);
        let mut line = [0_u8; 6];
        assert_eq!(gzgets(&mut state, &mut line), Some(5));
        assert_eq!(&line[..5], b"first");
        assert_eq!(line[5], 0);
        assert_eq!(gzgets(&mut state, &mut line), Some(5));
        assert_eq!(&line[..5], b" half");
    }

    #[test]
    fn gzgets_returns_nothing_for_a_one_byte_buffer() {
        // `gzread.c` L592-L621: `left` is zero, the loop never runs, and nothing was written, so
        // the result is `NULL` and the buffer is left untouched.
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        let mut line = [0xaa_u8; 1];
        assert_eq!(gzgets(&mut state, &mut line), None);
        assert_eq!(line[0], 0xaa);
    }

    #[test]
    fn gzread_would_refuse_a_length_beyond_int_max() {
        // The refusal at `gzread.c` L413-L416, exercised through the predicate so that no test has
        // to allocate two gibibytes.
        assert!(request_fits_in_int(0));
        assert!(request_fits_in_int(usize::try_from(i32::MAX).unwrap()));
        assert!(!request_fits_in_int(usize::try_from(i32::MAX).unwrap() + 1));
    }

    #[test]
    fn gzfread_rejects_an_overflowing_product() {
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        let mut buf = [0_u8; 8];
        assert_eq!(
            super::gzfread(&mut state, &mut buf, usize::MAX, 2),
            0,
            "the product does not fit in a size_t"
        );
        assert_eq!(state.err(), ReturnCode::STREAM_ERROR.as_i32());
    }

    /// F13: a buffer too short for `size * nitems` is refused before anything is read.
    ///
    /// C cannot detect this -- it has a bare pointer -- so there is no C behaviour to reproduce; the
    /// choice is between refusing and silently reading less. Refusing is the only answer a caller can
    /// act on, because `gzfread` reports whole items and a short count is exactly what end of file
    /// looks like.
    #[test]
    fn gzfread_refuses_a_buffer_too_short_for_the_product() {
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        let mut buf = [0_u8; 8];
        // 4 items of 4 bytes is 16, and only 8 were offered.
        assert_eq!(super::gzfread(&mut state, &mut buf, 4, 4), 0);
        assert_eq!(state.err(), ReturnCode::STREAM_ERROR.as_i32());
        assert_eq!(
            gztell64(&state),
            0,
            "the stream position must not have moved"
        );
        assert_eq!(buf, [0_u8; 8], "and nothing may have been written");
        // The refusal is recoverable: clearing it and asking for what fits works.
        gzclearerr(Some(&mut state));
        assert_eq!(super::gzfread(&mut state, &mut buf, 4, 2), 2);
        assert_eq!(&buf[..8], &HELLO[..8]);
    }

    /// F13: exactly the product is accepted; the boundary is `<`, not `<=`.
    #[test]
    fn gzfread_accepts_a_buffer_exactly_the_size_of_the_product() {
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        let mut buf = [0_u8; 12];
        assert_eq!(super::gzfread(&mut state, &mut buf, 4, 3), 3);
        assert_eq!(state.err(), ReturnCode::OK.as_i32());
        assert_eq!(&buf[..12], &HELLO[..12]);
    }

    /// A [`GzHandle`] that claims to have read more than it was offered.
    ///
    /// Not reachable through any real file; it exists because the trait's contract has to be
    /// enforceable and enforcement has to be tested. See `HANDLE_OVER_REPORTED`.
    struct OverReportingReader {
        /// How much to add to the honest count.
        excess: usize,
    }

    impl GzHandle for OverReportingReader {
        fn read(&mut self, buf: &mut [u8]) -> Result<usize, GzIoError> {
            // Fill honestly, then lie about how much was filled.
            buf.fill(b'Z');
            Ok(buf.len().saturating_add(self.excess))
        }

        fn write(&mut self, _buf: &[u8]) -> Result<usize, GzIoError> {
            Err(GzIoError::new(0, false))
        }

        fn seek(&mut self, _offset: ZOff64, _whence: GzSeekFrom) -> Result<ZOff64, GzIoError> {
            Ok(0)
        }

        fn set_nonblocking(&mut self, _nonblocking: bool) -> Result<(), GzIoError> {
            Ok(())
        }

        fn close(&mut self) -> Result<(), GzIoError> {
            Ok(())
        }
    }

    /// F14: a read count larger than the window offered is refused, not clamped.
    ///
    /// Clamping was the previous behaviour and it is the unsafe one in the meaningful sense: the layer
    /// would go on treating buffer bytes the handle never wrote as data it had supplied.
    #[test]
    fn a_handle_that_over_reports_a_read_is_refused() {
        let mut state: GzState<'static, GlobalAllocator> = GzState::new(GlobalAllocator);
        state.set_mode(GZ_READ);
        state.set_direct(1);
        state.set_want(32);
        let previous = state.set_handle(GzFileSlot::Boxed(Box::new(OverReportingReader {
            excess: 1,
        })));
        assert!(!previous.is_installed());

        let mut buf = [0_u8; 16];
        assert_eq!(
            gzread(&mut state, &mut buf),
            -1,
            "an over-reporting handle must not produce data"
        );
        assert_eq!(
            state.err(),
            ReturnCode::STREAM_ERROR.as_i32(),
            "and it is a stream error, not an errno"
        );
        assert_eq!(state.have(), 0, "no cursor may have moved");
        assert_eq!(gztell64(&state), 0);
    }

    /// F14 boundary: reporting exactly the window is honest and is accepted.
    #[test]
    fn a_handle_that_reports_exactly_the_window_is_accepted() {
        let mut state: GzState<'static, GlobalAllocator> = GzState::new(GlobalAllocator);
        state.set_mode(GZ_READ);
        state.set_direct(1);
        state.set_want(32);
        let previous = state.set_handle(GzFileSlot::Boxed(Box::new(OverReportingReader {
            excess: 0,
        })));
        assert!(!previous.is_installed());

        let mut buf = [0_u8; 16];
        assert_eq!(gzread(&mut state, &mut buf), 16);
        assert_eq!(state.err(), ReturnCode::OK.as_i32());
        assert_eq!(buf, [b'Z'; 16]);
    }

    #[test]
    fn gzfread_returns_whole_items_and_keeps_the_partial_one() {
        // `zlib.h` L1508-L1516: the trailing partial item is copied but not counted.
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        let mut buf = [0_u8; 16];
        // 14 bytes of payload in items of 4: three whole items, two bytes over.
        assert_eq!(super::gzfread(&mut state, &mut buf, 4, 4), 3);
        assert_eq!(&buf[..14], HELLO);
        assert_eq!(gztell64(&state), 14, "the partial item was still delivered");
    }

    #[test]
    fn a_large_request_decompresses_straight_into_the_caller_buffer() {
        // `gzread.c` L371-L378, reached because the request is at least `size << 1`.
        let closes = Cell::new(0);
        let mut state = reader(&HELLO_GZ, 8, Behaviour::Whole, &closes);
        let mut buf = [0_u8; 64];
        assert_eq!(gzread(&mut state, &mut buf), 14);
        assert_eq!(&buf[..14], HELLO);
        assert_eq!(
            state.have(),
            0,
            "nothing was left in the layer's own buffer"
        );
        assert_eq!(state.pos(), 14);
    }

    #[test]
    fn a_large_request_reads_transparently_into_the_caller_buffer() {
        let closes = Cell::new(0);
        let data = vec![b'q'; 200];
        let mut state = reader(&data, 8, Behaviour::Whole, &closes);
        let mut buf = [0_u8; 200];
        assert_eq!(gzread(&mut state, &mut buf), 200);
        assert!(buf.iter().all(|&byte| byte == b'q'));
        assert_eq!(state.how(), COPY);
    }

    #[test]
    fn gzrewind_starts_the_stream_over() {
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        let mut buf = [0_u8; 64];
        assert_eq!(gzread(&mut state, &mut buf), 14);

        assert_eq!(gzrewind(&mut state), 0);
        assert_eq!(state.pos(), 0);
        assert_eq!(state.how(), LOOK);
        assert_eq!(
            state.junk(),
            -1,
            "the first member is to be looked for again"
        );
        assert!(!state.eof());

        let mut again = [0_u8; 64];
        assert_eq!(gzread(&mut state, &mut again), 14);
        assert_eq!(&again[..14], HELLO);
    }

    #[test]
    fn gzrewind_refuses_a_write_stream() {
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        state.set_mode(GZ_WRITE);
        assert_eq!(gzrewind(&mut state), -1);
    }

    #[test]
    fn gzseek64_within_the_raw_area_moves_the_file() {
        // `gzlib.c` L395-L409: the fast path, available only for a transparent read.
        let closes = Cell::new(0);
        let data: Vec<u8> = (0..=255_u8).collect();
        let mut state = reader(&data, 64, Behaviour::Whole, &closes);
        let mut buf = [0_u8; 4];
        assert_eq!(gzread(&mut state, &mut buf), 4);
        assert_eq!(state.how(), COPY);

        assert_eq!(gzseek64(&mut state, 100, GzSeekFrom::Start.as_raw()), 100);
        assert_eq!(state.skip(), 0, "the fast path seeks for real");
        assert_eq!(state.have(), 0);
        assert_eq!(gzread(&mut state, &mut buf), 4);
        assert_eq!(buf, [100, 101, 102, 103]);
        assert_eq!(gztell64(&state), 104);
    }

    #[test]
    fn gzseek64_defers_a_forward_seek_in_a_gzip_stream() {
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        assert_eq!(gzseek64(&mut state, 7, GzSeekFrom::Start.as_raw()), 7);
        assert_eq!(gztell64(&state), 7, "the pending skip counts");
        let mut buf = [0_u8; 16];
        let got = count(gzread(&mut state, &mut buf));
        assert_eq!(&buf[..got], &HELLO[7..]);
    }

    #[test]
    fn gzseek64_rejects_seek_end_and_nonsense() {
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        // `zlib.h` L1668-L1669: `SEEK_END` is not supported.
        assert_eq!(gzseek64(&mut state, 0, GzSeekFrom::End.as_raw()), -1);
        assert_eq!(gzseek64(&mut state, 0, 99), -1);
    }

    #[test]
    fn gzseek64_refuses_to_go_before_the_start() {
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        assert_eq!(gzseek64(&mut state, -1, GzSeekFrom::Current.as_raw()), -1);
    }

    #[test]
    fn gzseek64_refuses_an_offset_that_cannot_be_normalised() {
        // The condition C's `offset -= state->x.pos` overflows on. At `pos == 1`, wrapping turns
        // `i64::MIN` into `i64::MAX` and schedules an eight-exabyte forward skip; the checked form
        // refuses the request and leaves the stream untouched.
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        let mut one = [0_u8; 1];
        assert_eq!(gzread(&mut state, &mut one), 1);
        assert_eq!(gztell64(&state), 1);

        assert_eq!(
            gzseek64(&mut state, ZOff64::MIN, GzSeekFrom::Start.as_raw()),
            -1
        );
        // Nothing moved, and in particular no enormous skip was scheduled.
        assert_eq!(state.skip(), 0);
        assert_eq!(gztell64(&state), 1);
        // The stream still reads correctly from where it was.
        let mut rest = [0_u8; 32];
        let got = count(gzread(&mut state, &mut rest));
        assert_eq!(&rest[..got], &HELLO[1..]);
    }

    #[test]
    fn gzseek64_overflow_leaves_a_pending_skip_intact() {
        // `SEEK_CUR` normalisation is `offset += skip; skip = 0`. When the addition cannot be
        // represented the request is refused *before* `skip` is cleared, so a deferred seek the
        // caller already asked for is not silently discarded.
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        assert_eq!(gzseek64(&mut state, 7, GzSeekFrom::Start.as_raw()), 7);
        assert_eq!(state.skip(), 7, "the forward seek is deferred");

        assert_eq!(
            gzseek64(&mut state, ZOff64::MAX, GzSeekFrom::Current.as_raw()),
            -1
        );
        assert_eq!(state.skip(), 7, "the pending skip survives the refusal");
        assert_eq!(gztell64(&state), 7);
        let mut buf = [0_u8; 16];
        let got = count(gzread(&mut state, &mut buf));
        assert_eq!(&buf[..got], &HELLO[7..], "the deferred seek still runs");
    }

    #[test]
    fn gzseek64_refuses_an_unrepresentable_landing_position() {
        // `pos + offset` is what C returns; an unrepresentable sum is refused rather than wrapped
        // into a negative "position". Exercised on the raw path, where `landing >= 0` is also the
        // fast-path guard, and on the deferred path.
        let closes = Cell::new(0);
        let data: Vec<u8> = (0..=255_u8).collect();
        let mut state = reader(&data, 64, Behaviour::Whole, &closes);
        let mut buf = [0_u8; 4];
        assert_eq!(gzread(&mut state, &mut buf), 4);
        assert_eq!(state.how(), COPY);

        assert_eq!(
            gzseek64(&mut state, ZOff64::MAX, GzSeekFrom::Current.as_raw()),
            -1
        );
        assert_eq!(state.skip(), 0);
        assert_eq!(gztell64(&state), 4, "position unchanged");
    }

    #[test]
    fn gzoffset64_does_not_count_buffered_input() {
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        let mut buf = [0_u8; 64];
        assert_eq!(gzread(&mut state, &mut buf), 14);
        // The whole 31-byte member was read from the file and all of it consumed.
        let offset = super::gzoffset64(&mut state);
        assert_eq!(offset, 31);
    }

    #[test]
    fn narrow_offset_matches_the_c_round_trip() {
        // C's `ret == (z_off_t)ret ? (z_off_t)ret : -1` (`gzlib.c` L442).
        assert_eq!(narrow_offset::<i32>(1234), Some(1234));
        assert_eq!(narrow_offset::<i32>(ZOff64::from(i32::MAX) + 1), None);
        assert_eq!(narrow_offset::<i64>(ZOff64::MAX), Some(ZOff64::MAX));
    }

    #[test]
    fn every_entry_point_refuses_a_write_stream() {
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        state.set_mode(GZ_WRITE);
        let mut buf = [0_u8; 8];
        assert_eq!(gzread(&mut state, &mut buf), -1);
        assert_eq!(super::gzfread(&mut state, &mut buf, 1, 8), 0);
        assert_eq!(gzgetc(&mut state), -1);
        assert_eq!(gzungetc(&mut state, c_int::from(b'x')), -1);
        assert_eq!(gzgets(&mut state, &mut buf), None);
        assert_eq!(gzclose_r(&mut state), ReturnCode::STREAM_ERROR);
        // Put the mode back so the fixture's `Drop` is the ordinary one.
        state.set_mode(GZ_READ);
    }

    #[test]
    fn the_example_c_test_gzio_read_sequence() {
        // `test/example.c` L114-L163, on the exact stream that function writes: `"hello, hello!"`
        // plus one zero byte. Every constant below is asserted by the C test itself.
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);

        let mut uncompr = [b'?'; 64];
        assert_eq!(gzread(&mut state, &mut uncompr), 14);
        assert_eq!(&uncompr[..14], HELLO);

        // `pos = gzseek(file, -8L, SEEK_CUR); if (pos != 6 || gztell(file) != pos) ...`
        let pos = gzseek64(&mut state, -8, GzSeekFrom::Current.as_raw());
        assert_eq!(pos, 6);
        assert_eq!(gztell64(&state), pos);

        // `if (gzgetc(file) != ' ') ...`
        assert_eq!(gzgetc(&mut state), c_int::from(b' '));

        // `if (gzungetc(' ', file) != ' ') ...`
        assert_eq!(gzungetc(&mut state, c_int::from(b' ')), c_int::from(b' '));

        // `gzgets(file, uncompr, uncomprLen); strlen(uncompr) == 7` -- the seven bytes of
        // `" hello!"`, terminated by the payload's own zero byte, which `gzgets` does not check for.
        let mut line = [b'?'; 64];
        let written = gzgets(&mut state, &mut line).unwrap();
        assert_eq!(written, 8, "\" hello!\" plus the payload's zero byte");
        assert_eq!(&line[..8], b" hello!\0");
        assert_eq!(&line[..7], &HELLO[6..13]);

        assert_eq!(gzclose_r(&mut state), ReturnCode::OK);
        assert_eq!(closes.get(), 1);
    }

    #[test]
    fn reading_after_the_stream_is_closed_is_refused() {
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        assert_eq!(gzclose_r(&mut state), ReturnCode::OK);
        // The handle is gone, so a further read cannot invent data. It reports rather than panics.
        let mut buf = [0_u8; 8];
        assert_eq!(gzread(&mut state, &mut buf), -1);
        assert_eq!(state.err(), ReturnCode::STREAM_ERROR.as_i32());
    }

    #[test]
    fn a_gzip_stream_reports_its_container_choice_before_any_data() {
        // `gz_look` on a real member decides `GZIP` and clears `direct` without consuming output.
        let closes = Cell::new(0);
        let mut state = default_reader(&HELLO_GZ, &closes);
        gz_look(&mut state).unwrap();
        assert_eq!(state.how(), GZIP);
        assert_eq!(state.junk(), 1, "a candidate until output appears");
        assert_eq!(state.direct(), 0);
        assert_eq!(state.have(), 0);
    }
}
