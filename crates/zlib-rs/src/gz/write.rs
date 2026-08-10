//! The write half of the `gzFile` layer: the Rust counterpart of `gzwrite.c` (700 lines).
//!
//! This module drives the compression engine on behalf of a `gzFile` opened for writing. It is
//! where the gzip bytes this implementation produces are actually shaped -- not because it emits any of them
//! itself, but because it chooses the parameters that do. `gz_init` asks for `MAX_WBITS + 16`, and
//! that `+ 16` is what makes `crate::deflate` emit an RFC 1952 member rather than an RFC 1950
//! stream (`gzwrite.c` L36-L37). Change `windowBits`, `memLevel` or a flush decision here and the
//! contents of every file this layer writes change with it.
//!
//! # What lives here and what does not
//!
//! | C function | `gzwrite.c` | Here |
//! |---|---|---|
//! | `gz_init` | L11-L57 | `gz_init` |
//! | `gz_comp` | L65-L148 | `gz_comp` |
//! | `gz_zero` | L154-L182 | `gz_zero` |
//! | `gz_write` | L188-L252 | `gz_write` |
//! | `gzwrite` | L255-L277 | [`gzwrite`] |
//! | `gzfwrite` | L280-L304 | [`gzfwrite`] |
//! | `gzputc` | L307-L347 | [`gzputc`] |
//! | `gzputs` | L350-L372 | [`gzputs`] |
//! | `gz_vacate` | L382-L396 | `gz_vacate` |
//! | `gzvprintf`, `gzprintf` | L403-L495 | `printf.rs`, which calls `gz_init` and `gz_vacate` |
//! | `gzflush` | L603-L627 | [`gzflush`] |
//! | `gzsetparams` | L630-L664 | `mod.rs`, which calls `gz_comp` |
//! | `gzclose_w` | L667-L700 | [`gzclose_w`] |
//!
//! No gzip header, trailer or CRC-32 is assembled anywhere below. That is the engine's business,
//! selected by the `+ 16`, and `gz/header.rs` documents the same division of labour from the other
//! side: all four `gz*.c` translation units between them reference exactly one gzip header byte,
//! and it is on the *read* path (`gzread.c` L151-L153).
//!
//! # The two buffers, and why their sizes are the reverse of the read path's
//!
//! `gz_init` allocates `in` with `want << 1` and `out` with `want` (`gzwrite.c` L16 and L25). The
//! doubling is on the **input** side here, and C's comment at L15 says exactly why: "double size
//! for `gzprintf`". `gzprintf` formats straight into the input buffer after whatever is already
//! buffered, so it needs `size` bytes of headroom past the current contents
//! (`gzwrite.c` L435-L437). `gz_vacate` is what maintains that guarantee, which is why it is
//! `pub(crate)` rather than private: `printf.rs` calls it before and after formatting.
//!
//! The read path doubles the *output* buffer instead (`gzread.c` L99-L100), because that is where
//! `gzungetc` parks pushed bytes. Nothing in this module may assume the read path's shape.
//!
//! Neither buffer arrives zeroed. C obtains both from plain `malloc`, and the engine's own blocks
//! come from `zcalloc`, which is also `malloc` whenever `sizeof(uInt) > 2` (`zutil.c` L299-L303) --
//! every target this port supports. `test/infcover.c` L84-L87 fills each block it hands out with
//! `0xa5` precisely to catch code that assumes otherwise. `gz_zero` is the only routine below
//! that writes zeros, and it writes them deliberately.
//!
//! # The exposed prefix on a write stream
//!
//! `crate::gz::state` documents the pointer/index split in full; the part specific to this module
//! is that the prefix means something different when writing:
//!
//! * `x.next` is the **flush cursor**, not a delivery cursor. `gz_init` seeds it with
//!   `state->x.next = strm->next_out` (`gzwrite.c` L54) and `gz_comp` drains the staged output with
//!   `while (strm->next_out > state->x.next)`, advancing it by each `write` and resetting it to
//!   `state->out` when the buffer is recycled (L110-L127). This implementation holds it as the index
//!   `GzState::out_pos`, so the comparison is integer arithmetic over an owned slice and every
//!   access through it is bounds checked (AAP §0.3.3.7).
//! * `x.have` **stays zero for the entire life of a write stream**. No line of `gzwrite.c` assigns
//!   it. That is what keeps a caller's `gzgetc` macro from ever taking its fast path on a write
//!   handle: with `have` at zero the macro always calls the `gzgetc` function, which rejects a
//!   write stream. Nothing below may "helpfully" populate `have`; doing so would make `gzgetc`
//!   hand the caller bytes out of the compressor's staging buffer.
//! * `x.pos` is the position in the **uncompressed** stream and is advanced by `gz_write`,
//!   `gz_zero`, [`gzputc`] and `gzprintf`.
//!
//! Every public entry point below therefore calls `GzState::resync_from_exposed` on entry and
//! `GzState::refresh_exposed` before returning, which is the contract `crate::gz::state` states.
//! The one exception is a bail-out that has changed nothing: those return without refreshing,
//! because C likewise returns without touching the structure, and refreshing a state whose
//! `resync` failed would publish a pointer derived from an index that was never validated.
//!
//! # The contract this module expects from `crate::gz`
//!
//! Two crate-private helpers are used from `mod.rs`, which owns the shared plumbing of `gzlib.c`.
//!
//! `gz_error` implements `gzlib.c` L554-L589 and owns the whole error policy: releasing the
//! previous message, clearing `x.have` when the error is fatal and the stream is not merely
//! stalled, skipping the allocation for `Z_MEM_ERROR`, and prefixing the message with the path.
//! `gt_off` implements the `GT_OFF` macro (`gzguts.h` L216), which is false on every target
//! where `sizeof(int) != sizeof(z_off64_t)` and exists only so that an `unsigned` may be compared
//! against a signed `z_off64_t` without either one being misinterpreted.
//!
//! # Feature gating
//!
//! The crate root reaches this file through `#[cfg(feature = "std")] mod gz;`, so no per-item gate
//! appears below and `--no-default-features` compiles the whole subtree out. Nothing here needs
//! `std` directly: the file itself arrives injected as a `GzHandle`, and the non-blocking condition
//! that C reads out of `errno` arrives as `GzIoError::would_block`.

// Every item in a module named `write` that implements a C function named `gz_write`, `gzwrite` or
// `gz_vacate` necessarily repeats the module's name. The C names are the ones a maintainer
// comparing this file against `gzwrite.c` will search for, so they are kept verbatim.
#![allow(clippy::module_name_repetitions)]

use core::ffi::c_uint;

use crate::allocate::{Allocator, Buffer};
use crate::config::{DeflateConfig, Method, DEF_MEM_LEVEL, MAX_WBITS, Z_FINISH, Z_NO_FLUSH};
use crate::deflate::{
    deflate, deflate_end, deflate_init2, deflate_reset, DeflateReset, DeflateStream, Flush,
};
use crate::error::ReturnCode;
use crate::gz::state::{
    split_buffers, GzEngine, GzHandle, GzIoError, GzState, GzStream, ZOff64, GZ_NONE, GZ_WRITE,
};
use crate::gz::{errno_message, gt_off, gz_error};

/// `gz_error(state, Z_MEM_ERROR, "out of memory")` (`gzwrite.c` L18, L28 and L41).
const OUT_OF_MEMORY: &[u8] = b"out of memory";

/// `gz_error(state, Z_STREAM_ERROR, ...)` when `deflate` reports `Z_STREAM_ERROR`
/// (`gzwrite.c` L135-L136).
const DEFLATE_CORRUPT: &[u8] = b"internal error: deflate stream corrupt";

/// `gzwrite`'s rejection of a length that does not fit in an `int` (`gzwrite.c` L271).
const LENGTH_NOT_INT: &[u8] = b"requested length does not fit in int";

/// `gzfwrite`'s rejection of an overflowing `size * nitems` product (`gzwrite.c` L298).
const REQUEST_NOT_SIZE_T: &[u8] = b"request does not fit in a size_t";

/// `gzputs`'s rejection of a string length that does not fit in an `int` (`gzwrite.c` L367).
const STRING_NOT_INT: &[u8] = b"string length does not fit in int";

/// Reports a failed file operation the way `gz_comp` does: `gz_error(state, Z_ERRNO, zstrerror())`
/// (`gzwrite.c` L83, L118).
///
/// `zstrerror()` is `strerror(errno)` (`gzguts.h` L131-L133), so the message a caller reads back from
/// `gzerror` has to be the platform's own text for the number the failing call reported -- not a
/// fixed string. `crate::gz::errno_message` produces exactly those bytes, from the `errno` the
/// [`GzIoError`] carries when there is one and from the thread's current `errno` otherwise, which is
/// the same distinction C makes between its two call sites.
///
/// Written as one helper rather than repeated at each of the six call sites so that the read path and
/// the write path cannot drift apart in what they report; `crate::gz::read` renders through the same
/// function.
fn report_io_failure<'a, A: Allocator<'a>>(state: &mut GzState<'a, A>, error: Option<GzIoError>) {
    let message = errno_message(error);
    gz_error(state, ReturnCode::ERRNO, Some(message.as_bytes()));
}

/// The guard that replaces C's unchecked read past a caller's buffer in `gzfwrite`.
///
/// C computes `len = nitems * size` and hands `len` bytes at `buf` to `gz_write` without any way to
/// know how long the caller's buffer really is. This implementation receives a slice, so a request longer
/// than the slice is detectable, and is reported rather than read.
const REQUEST_PAST_BUFFER: &[u8] = b"request does not fit in the supplied buffer";

/// The message for a [`crate::gz::state::GzHandle`] that claims to have written more than it was
/// offered.
///
/// C cannot detect this: `write(2)` is trusted to honour its `count` argument and `gz_comp` advances
/// `state->x.next` by the return value unchecked (`gzwrite.c` L122). This port hands the handle a
/// *slice*, so a count larger than that slice is a broken implementation of the trait -- the bytes it
/// claims to have written were never given to it. Clamping the number was the alternative and it
/// hides the defect while leaving the stream's accounting wrong; refusing reports it as a
/// `Z_STREAM_ERROR`, which is the right code because no operating-system error occurred.
///
/// `crate::gz::read` refuses the mirror-image over-report with the same reasoning, so a `GzHandle`
/// implementation faces one rule in both directions -- stated on
/// [`crate::gz::state::GzHandle::read`] and [`crate::gz::state::GzHandle::write`] themselves.
const HANDLE_OVER_REPORTED: &[u8] = b"file handle reported more bytes than were requested";

/// Widens a C `unsigned` count into a `usize` index.
///
/// Every target this implementation supports has `usize` at least as wide as `c_uint`, so the conversion is
/// exact. Saturating rather than panicking on a hypothetical narrower target is the conservative
/// choice: a saturated value can only make a bounds check stricter, never looser.
fn to_index(value: c_uint) -> usize {
    usize::try_from(value).unwrap_or(usize::MAX)
}

/// Narrows a `usize` count into a C `unsigned`.
///
/// The counterpart of [`to_index`]. C performs this narrowing with a bare cast -- `(unsigned)len`
/// at `gzwrite.c` L216 and L239 -- guarded by a preceding range test. Every call site below has
/// already clamped the value to `state->size` or to `(unsigned)-1`, so the saturation is
/// unreachable; it exists so that the conversion cannot panic on any input.
fn to_count(value: usize) -> c_uint {
    c_uint::try_from(value).unwrap_or(c_uint::MAX)
}

/// Converts a byte count into the signed file-offset type that `x.pos` and `skip` use.
///
/// C writes `state->x.pos += n` with `n` an `unsigned` and `pos` a `z_off64_t` (`gzwrite.c` L176,
/// L219 and L243), relying on the implicit widening. Saturation stands in for that widening and is
/// unreachable for the same reason [`to_count`]'s is.
fn to_offset(value: usize) -> ZOff64 {
    ZOff64::try_from(value).unwrap_or(ZOff64::MAX)
}

/// `max = ((unsigned)-1 >> 2) + 1` (`gzwrite.c` L67): the largest single `write` request.
///
/// One quarter of the `unsigned` range plus one, which is 1 GiB on a 32-bit `unsigned`. C clamps
/// every `write` to it so that the `int` the system call returns can always be compared against the
/// request without the comparison itself overflowing.
fn max_write_chunk() -> usize {
    to_index(c_uint::MAX >> 2).saturating_add(1)
}

/// Copies the five `z_stream` scalars a reset produces into the layer's stream view.
///
/// `deflateReset` assigns `total_in`, `total_out`, `msg`, `data_type` and `adler` on the *caller's*
/// `z_stream` (`deflate.c` L651-L671). C's `gz_state` embeds that `z_stream` outright
/// (`gzguts.h` L202), so the assignments land in the state with nothing further to do. This implementation
/// splits the compression state from the caller-visible scalars, so [`crate::deflate::deflate_reset`]
/// hands them back as a [`DeflateReset`] and they are stored here instead.
///
/// `adler` is the one that matters: for a gzip stream it seeds the running CRC-32 that ends up in
/// the member trailer (`deflate.c` L667-L671). Dropping it would corrupt the trailer of every
/// member that takes more than one `deflate` call to write.
fn apply_reset<'a, A: Allocator<'a>>(strm: &mut GzStream<'a, A>, reset: DeflateReset) {
    strm.total_in = reset.total_in;
    strm.total_out = reset.total_out;
    strm.msg = reset.msg;
    strm.data_type = reset.data_type;
    strm.adler = reset.adler;
}

/// Allocates the working buffers and the compression engine, and marks the state initialised.
///
/// The port of `gz_init` (`gzwrite.c` L11-L57), which is `local` in C and is called lazily from
/// [`gz_comp`], `gz_write` and `gzprintf` the first time a write stream needs a buffer. C's
/// header comment states the contract: "Mark initialization by setting `state->size` to non-zero."
///
/// C's steps, in order:
///
/// | C | `gzwrite.c` | Here |
/// |---|---|---|
/// | `state->in = malloc(state->want << 1)` | L16 | `GzState::allocate_write_buffers` |
/// | `state->out = malloc(state->want)`, only when not `direct` | L23-L25 | the same call |
/// | `free(state->in)` when the second allocation fails | L27 | the same call, in `deflateEnd` order |
/// | `strm->zalloc = zfree = opaque = Z_NULL` | L33-L35 | not reproduced; see below |
/// | `deflateInit2(strm, level, Z_DEFLATED, MAX_WBITS + 16, DEF_MEM_LEVEL, strategy)` | L36-L37 | [`deflate_init2`] |
/// | any `deflateInit2` failure becomes `Z_MEM_ERROR` | L38-L43 | preserved verbatim |
/// | `strm->next_in = NULL` | L44 | the input cursor goes back to index zero |
/// | `state->size = state->want` | L48 | `GzState::set_size` |
/// | `strm->avail_out = size; strm->next_out = out; x.next = next_out` | L51-L55 | `GzState::set_output_window` |
///
/// ★ **`MAX_WBITS + 16` and `DEF_MEM_LEVEL` are not adjustable.** The `+ 16` is zlib's request for
/// the gzip wrapper rather than the zlib one, and together with `memLevel` 8 it is what makes the
/// bytes this layer writes byte-identical to the C library's and readable by the system `gzip`.
/// `level` and `strategy` come from the state, because `gzopen`'s mode string and `gzsetparams` set
/// them (`gzlib.c` L110-L111 and `gzwrite.c` L661-L662).
///
/// The three allocator fields C nulls at L33-L35 have no analogue: `crate::gz::state` omits them
/// from its stream view precisely because the `gzFile` layer always nulls them, and the engine
/// instead takes the [`Allocator`] the state was built with. Nulling them in C selects `zcalloc`,
/// which is `malloc`; injecting the state's allocator is the same decision made explicit.
///
/// The buffers arrive with unspecified contents and are deliberately left that way, for the reason
/// this module's documentation gives.
///
/// # Errors
///
/// [`ReturnCode::MEM_ERROR`], recorded on the state through `gz_error` before returning, for any of
/// the three failures C maps to `Z_MEM_ERROR`: either buffer, the engine, or -- a case C cannot have
/// -- a stored `strategy` that names none of the five documented strategies. C reaches the last one
/// through `deflateInit2` rejecting the argument, and maps *any* `deflateInit2` failure to
/// `Z_MEM_ERROR` regardless of the real code, which is reproduced here.
pub(crate) fn gz_init<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
) -> Result<(), ReturnCode> {
    // L15-L30. The helper reproduces both allocations, the `!state->direct` guard on the output
    // buffer, and the release of the input buffer when the output one fails.
    if state.allocate_write_buffers().is_err() {
        gz_error(state, ReturnCode::MEM_ERROR, Some(OUT_OF_MEMORY));
        return Err(ReturnCode::MEM_ERROR);
    }

    // `if (!state->direct)` (L23): a transparent stream stages nothing and compresses nothing, so
    // it gets neither an output buffer nor an engine.
    if state.direct() == 0 {
        // L36-L37, with the two fixed parameters that decide the container and the memory budget.
        let Some(strategy) = state.strategy_typed() else {
            state.release_buffers();
            gz_error(state, ReturnCode::MEM_ERROR, Some(OUT_OF_MEMORY));
            return Err(ReturnCode::MEM_ERROR);
        };
        let config = DeflateConfig {
            level: state.level(),
            method: Method::Deflated,
            window_bits: MAX_WBITS + 16,
            mem_level: DEF_MEM_LEVEL,
            strategy,
        };
        // `Copy` is what lets the allocator be handed to the engine by value while the state keeps
        // its own copy; the engine's blocks must come from the same allocator as the rest.
        let allocator = *state.allocator();

        // The real code is discarded on purpose -- C reports `Z_MEM_ERROR` for a `Z_STREAM_ERROR`
        // from `deflateInit2` too, and a caller of `gzerror` sees that.
        let Ok(engine) = deflate_init2(config, allocator) else {
            state.release_buffers();
            gz_error(state, ReturnCode::MEM_ERROR, Some(OUT_OF_MEMORY));
            return Err(ReturnCode::MEM_ERROR);
        };
        if state.install_deflate(engine).is_err() {
            state.release_buffers();
            gz_error(state, ReturnCode::MEM_ERROR, Some(OUT_OF_MEMORY));
            return Err(ReturnCode::MEM_ERROR);
        }

        // `deflateInit2_` finishes with `return deflateReset(strm)` (`deflate.c` L532), whose effect
        // on the caller's stream is part of its contract. `deflate_init2` cannot reach this layer's
        // scalars, so the reset is applied here; it is idempotent on the state.
        let reset = match &mut state.strm.engine {
            GzEngine::Deflate(engine) => engine.get_mut().map(deflate_reset),
            GzEngine::None | GzEngine::Inflate(_) => None,
        };
        let Some(reset) = reset else {
            state.release_buffers();
            gz_error(state, ReturnCode::MEM_ERROR, Some(OUT_OF_MEMORY));
            return Err(ReturnCode::MEM_ERROR);
        };
        apply_reset(&mut state.strm, reset);

        // `strm->next_in = NULL;` (L44). The cursor is an index here, so index zero is the
        // equivalent; `avail_in` is already zero, set by `gz_reset` (`gzlib.c` L83).
        state.strm.next_in = 0;
    }

    // `state->size = state->want;` (L48) -- the "initialised" marker every other routine tests.
    state.set_size(state.want());

    // L51-L55: prime the staging cursors. `avail_out` is the whole output buffer, the engine writes
    // from its start, and the flush cursor starts there too, which is the invariant `gz_comp`'s
    // `strm->next_out > state->x.next` comparison rests on.
    if state.direct() == 0 {
        state.strm.avail_out = state.size();
        state.strm.next_out = 0;
        if state.set_output_window(0, 0).is_err() {
            state.release_buffers();
            state.set_size(0);
            gz_error(state, ReturnCode::MEM_ERROR, Some(OUT_OF_MEMORY));
            return Err(ReturnCode::MEM_ERROR);
        }
    }
    Ok(())
}

/// Drains the staged output buffer to the file: the inner loop of `gz_comp` (`gzwrite.c` L110-L123).
///
/// The pointer comparison becomes an index comparison between `strm.next_out`, where the engine has
/// written to, and `GzState::out_pos`, how far the file has been given. A short write is normal
/// rather than exceptional -- the loop exists for it -- so the cursor advances by exactly the count
/// the handle reports, **once that count has been proved to be a count of bytes the handle was
/// actually given**. C needs no such proof because `write(2)` cannot return more than `count`; here
/// the file is an injected trait object and the count is untrusted input, so it is bounded by the
/// length of the offered slice before any cursor moves. See `HANDLE_OVER_REPORTED`.
///
/// # Errors
///
/// [`ReturnCode::ERRNO`] for a failed write, with `again` set first when the failure was the
/// non-blocking `EAGAIN`/`EWOULDBLOCK` condition (L117-L118), so that `gz_error` leaves `x.have`
/// alone for a stream that is merely stalled. Also [`ReturnCode::ERRNO`] when there is no file to
/// write to or the staged range cannot be addressed, which C would reach only by dereferencing a
/// null pointer; when the handle reports zero progress on a non-empty request; and when it reports
/// having written **more** than it was offered. [`ReturnCode::STREAM_ERROR`] if the flush cursor
/// could not be advanced.
fn flush_out<'a, A: Allocator<'a>>(state: &mut GzState<'a, A>) -> Result<(), ReturnCode> {
    let max = max_write_chunk();
    while state.strm.next_out > state.out_pos() {
        // `errno = 0; state->again = 0;` (L111-L112): cleared before every attempt, so that `again`
        // describes this write and no earlier one.
        state.set_again(false);

        let begin = state.out_pos();
        let put = state.strm.next_out.saturating_sub(begin).min(max);
        let end = begin.saturating_add(put);

        // The staged bytes live in `state.output` and the file in `state.handle`. Borrowing the two
        // fields directly rather than through a method keeps the borrows disjoint, which is what
        // lets both be held across the call. `offered` is the exact length that went to the file,
        // read back from the slice itself rather than recomputed, so the bound the reported count is
        // checked against cannot disagree with the bytes that were actually presented.
        let mut offered = 0_usize;
        let attempt = {
            let GzState {
                buffer_slot,
                handle,
                ..
            } = state;
            let (_, staged_out) = split_buffers(buffer_slot);
            let staged = staged_out
                .map(|buffer| buffer.as_slice())
                .and_then(|slice| slice.get(begin..end));
            let file = handle.handle_mut();
            match (staged, file) {
                (Some(chunk), Some(mut file)) => {
                    offered = chunk.len();
                    Some(file.write(chunk))
                }
                _ => None,
            }
        };

        let Some(result) = attempt else {
            // No error object: nothing was attempted, because the buffer or the handle was gone.
            // C's bare `zstrerror()` reads the thread's current `errno`, and so does this.
            report_io_failure(state, None);
            return Err(ReturnCode::ERRNO);
        };

        match result {
            // A count larger than the chunk offered is impossible from a correct handle, so it is
            // refused rather than clamped: nothing moves and the condition is reported. See
            // [`HANDLE_OVER_REPORTED`]. The bound is `offered`, read back from the slice that
            // actually reached the file, not the separately computed `put`.
            Ok(written) if written > offered => {
                gz_error(state, ReturnCode::STREAM_ERROR, Some(HANDLE_OVER_REPORTED));
                return Err(ReturnCode::STREAM_ERROR);
            }
            // `state->x.next += writ;` (L122)
            Ok(written) if written != 0 => {
                let moved = begin.saturating_add(written);
                if state.set_output_window(moved, 0).is_err() {
                    gz_error(state, ReturnCode::STREAM_ERROR, Some(DEFLATE_CORRUPT));
                    return Err(ReturnCode::STREAM_ERROR);
                }
            }
            // A handle that reports no progress on a non-empty request. `put` is at least one byte
            // here, so C's loop would spin forever; reporting the condition is the only way to keep
            // the promise that this layer terminates. `GzHandle::write` documents a short write as
            // legal and zero as a contract violation.
            Ok(_) => {
                report_io_failure(state, None);
                return Err(ReturnCode::ERRNO);
            }
            // L116-L121, including the `EAGAIN`/`EWOULDBLOCK` test that `GzIoError::would_block`
            // carries in place of `errno`.
            Err(error) => {
                if error.would_block {
                    state.set_again(true);
                }
                report_io_failure(state, Some(error));
                return Err(ReturnCode::ERRNO);
            }
        }
    }
    Ok(())
}

/// Writes the pending input straight to the file, uncompressed: `gz_comp`'s transparent path
/// (`gzwrite.c` L75-L91): while input remains, hand the handle at most `max` bytes of it and
/// advance by exactly the count reported, then return without touching the engine.
///
/// `flush` is **ignored** on this path, exactly as C's header comment says: "If `gz->direct` is
/// true, then simply write to the output file without compressing, and ignore flush." There is no
/// deflate stream to flush and no member to finish, so a `Z_FINISH` here is indistinguishable from a
/// `Z_NO_FLUSH`.
///
/// `source` selects where `strm.next_in` indexes: `None` is the layer's own input buffer, and
/// `Some(buffer)` is a caller's buffer that `gz_write` has pointed the stream at, which is C's
/// `state->strm.next_in = (z_const Bytef *)buf` (L234).
///
/// # Errors
///
/// [`ReturnCode::ERRNO`] on the same conditions [`flush_out`] reports it for, including a handle
/// that reports zero progress and one that reports having written more bytes than it was offered.
/// The over-report check matters more here than there, because on the `Some(buffer)` path the bytes
/// belong to the caller and the count feeds straight back into `gzwrite`'s return value.
fn write_through<'a, A: Allocator<'a>>(
    state: &mut GzState<'a, A>,
    source: Option<&[u8]>,
) -> Result<(), ReturnCode> {
    let max = max_write_chunk();
    while state.strm.avail_in != 0 {
        state.set_again(false);

        let begin = state.strm.next_in;
        let put = to_index(state.strm.avail_in).min(max);
        let end = begin.saturating_add(put);

        // `offered` is read back from the slice that actually went to the file, for the reason
        // `flush_out` records: the reported count is untrusted data and must be checked against the
        // exact bytes presented, not against a separately recomputed length.
        let mut offered = 0_usize;
        let attempt = {
            let GzState {
                buffer_slot,
                handle,
                ..
            } = state;
            let (staged_in, _) = split_buffers(buffer_slot);
            let pending = match source {
                Some(buffer) => buffer.get(begin..end),
                None => staged_in
                    .map(|buffer| buffer.as_slice())
                    .and_then(|slice| slice.get(begin..end)),
            };
            let file = handle.handle_mut();
            match (pending, file) {
                (Some(chunk), Some(mut file)) => {
                    offered = chunk.len();
                    Some(file.write(chunk))
                }
                _ => None,
            }
        };

        let Some(result) = attempt else {
            // No error object: nothing was attempted, because the buffer or the handle was gone.
            // C's bare `zstrerror()` reads the thread's current `errno`, and so does this.
            report_io_failure(state, None);
            return Err(ReturnCode::ERRNO);
        };

        match result {
            // As in `flush_out`: an over-report is refused, not clamped, and checked against
            // `offered` before the progress arm so that neither cursor moves.
            Ok(written) if written > offered => {
                gz_error(state, ReturnCode::STREAM_ERROR, Some(HANDLE_OVER_REPORTED));
                return Err(ReturnCode::STREAM_ERROR);
            }
            // L87-L88
            Ok(written) if written != 0 => {
                state.strm.avail_in = state.strm.avail_in.saturating_sub(to_count(written));
                state.strm.next_in = state.strm.next_in.saturating_add(written);
            }
            // See the matching arm of `flush_out` for why zero progress is an error here.
            Ok(_) => {
                report_io_failure(state, None);
                return Err(ReturnCode::ERRNO);
            }
            Err(error) => {
                if error.would_block {
                    state.set_again(true);
                }
                report_io_failure(state, Some(error));
                return Err(ReturnCode::ERRNO);
            }
        }
    }
    Ok(())
}

/// One `deflate()` call, with the `z_stream` view rebuilt around the layer's own cursors.
///
/// Implements `ret = deflate(strm, flush);` (`gzwrite.c` L133). C hands the compressor the address
/// of the `z_stream` embedded in `gz_state`, so the cursors and the five scalars are already where
/// `deflate` expects them. This implementation keeps the compression state and the caller-visible scalars
/// apart, so the view is assembled here, handed over, and read back.
///
/// Both windows are the **whole** buffer truncated to the end of the valid region, never re-sliced
/// from the cursor: [`DeflateStream`] documents that `deflate_stored` reads *backwards* from
/// `next_in` (`deflate.c` L1766 and L1780) and that `flush_pending` writes at `next_out`, so the
/// already-consumed prefix has to stay addressable. That is exactly what C's advancing pointers
/// leave in place.
///
/// `source` selects the input base, for the reason [`write_through`] gives.
///
/// Returns whatever `deflate` returned, or [`ReturnCode::STREAM_ERROR`] if the view could not be
/// assembled -- no engine, no output buffer, or a cursor outside its buffer. C cannot reach those
/// states without having already corrupted memory; [`gz_comp`] maps the code onto the same
/// "internal error: deflate stream corrupt" report C produces at L134-L137.
fn deflate_once<'a, A: Allocator<'a>>(
    state: &mut GzState<'a, A>,
    flush: Flush,
    source: Option<&[u8]>,
) -> ReturnCode {
    let next_in = state.strm.next_in;
    let next_out = state.strm.next_out;
    let in_end = next_in.saturating_add(to_index(state.strm.avail_in));
    let out_end = next_out.saturating_add(to_index(state.strm.avail_out));
    let total_in = state.strm.total_in;
    let total_out = state.strm.total_out;
    let msg = state.strm.msg;
    let adler = state.strm.adler;
    let data_type = state.strm.data_type;

    // Three simultaneous views into one `&mut GzState`, each reached by naming its field directly:
    // `input` read-only, `output` mutably, and `strm.engine` mutably. The paths are disjoint, so the
    // borrow checker admits all three at once -- which is the whole reason the buffers live in
    // separate fields rather than behind one accessor.
    let (ret, new_next_in, new_avail_in, new_next_out, new_avail_out, scalars) = {
        let GzState {
            buffer_slot, strm, ..
        } = state;
        let (staged_in, staged_out) = split_buffers(buffer_slot);
        let in_base: &[u8] = match source {
            Some(buffer) => buffer,
            None => staged_in.map_or(&[][..], |buffer| buffer.as_slice()),
        };
        let Some(input) = in_base.get(..in_end) else {
            return ReturnCode::STREAM_ERROR;
        };
        let Some(output) = staged_out
            .map(Buffer::as_mut_slice)
            .and_then(|slice| slice.get_mut(..out_end))
        else {
            return ReturnCode::STREAM_ERROR;
        };
        let GzEngine::Deflate(engine) = &mut strm.engine else {
            return ReturnCode::STREAM_ERROR;
        };
        let Some(compressor) = engine.get_mut() else {
            return ReturnCode::STREAM_ERROR;
        };

        let mut stream = DeflateStream::new(input, output);
        stream.next_in = next_in;
        stream.next_out = next_out;
        stream.total_in = total_in;
        stream.total_out = total_out;
        stream.msg = msg;
        stream.adler = adler;
        stream.data_type = data_type;

        // `deflate` takes the C `int` rather than the typed mode, because it is the driver a C
        // caller reaches directly and therefore owns the `flush > Z_BLOCK` rejection at
        // `deflate.c` L985. `gzflush` has already applied the narrower `gzFile` rule.
        let ret = deflate(compressor, &mut stream, flush.as_raw());

        (
            ret,
            stream.next_in,
            stream.avail_in(),
            stream.next_out,
            stream.avail_out(),
            (
                stream.total_in,
                stream.total_out,
                stream.msg,
                stream.adler,
                stream.data_type,
            ),
        )
    };

    state.strm.next_in = new_next_in;
    state.strm.avail_in = to_count(new_avail_in);
    state.strm.next_out = new_next_out;
    state.strm.avail_out = to_count(new_avail_out);
    let (total_in, total_out, msg, adler, data_type) = scalars;
    state.strm.total_in = total_in;
    state.strm.total_out = total_out;
    state.strm.msg = msg;
    state.strm.adler = adler;
    state.strm.data_type = data_type;
    ret
}

/// `deflateReset(strm)` on the installed compressor, plus the scalar half of the reset.
///
/// Implements `gzwrite.c` L99, split out only so that the engine borrow ends before the scalars are
/// written back.
///
/// # Errors
///
/// [`ReturnCode::STREAM_ERROR`] when no compressor is installed, which C would reach only by
/// calling `deflateReset` on a null state.
fn reset_engine<'a, A: Allocator<'a>>(state: &mut GzState<'a, A>) -> Result<(), ReturnCode> {
    let reset = match &mut state.strm.engine {
        GzEngine::Deflate(engine) => engine.get_mut().map(deflate_reset),
        GzEngine::None | GzEngine::Inflate(_) => None,
    };
    let Some(reset) = reset else {
        return Err(ReturnCode::STREAM_ERROR);
    };
    apply_reset(&mut state.strm, reset);
    Ok(())
}

/// Compresses whatever is at the input cursor and writes it to the file.
///
/// Implements `gz_comp` (`gzwrite.c` L65-L148), `local` in C. `source` selects where
/// `strm.next_in` indexes; [`gz_comp`] is the buffered form and is the one every other module calls.
///
/// # Errors
///
/// See [`gz_comp`].
fn gz_comp_source<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
    flush: Flush,
    source: Option<&[u8]>,
) -> Result<(), ReturnCode> {
    // `if (state->size == 0 && gz_init(state) == -1) return -1;` (L71-L72)
    if state.size() == 0 {
        gz_init(state)?;
    }

    // `if (state->direct) { ... }` (L75-L91)
    if state.direct() != 0 {
        return write_through(state, source);
    }

    // L94-L101: a `Z_FINISH` completed the previous member and armed a reset, but the reset is
    // deferred until there is genuinely something to put in a new member. C's condition is precise
    // and worth reading carefully -- `avail_in == 0 && flush == Z_NO_FLUSH` -- so what the guard
    // suppresses is an *unflushed* call with nothing buffered, which is what `gz_write`'s drain
    // (L230), `gz_zero` (L160) and `gz_vacate` (L388) all perform routinely. A *flushing* call
    // still starts a new member even with nothing to put in it, which is why `gzclose_w`'s
    // `Z_FINISH` after a caller's own `gzflush(Z_FINISH)` appends an empty 20-byte member; the
    // reference does exactly the same, and `gzread` reads the pair back as one empty-tailed stream.
    if state.reset_pending() {
        if state.strm.avail_in == 0 && flush == Flush::NoFlush {
            return Ok(());
        }
        if reset_engine(state).is_err() {
            gz_error(state, ReturnCode::STREAM_ERROR, Some(DEFLATE_CORRUPT));
            return Err(ReturnCode::STREAM_ERROR);
        }
        state.set_reset_pending(false);
    }

    // `ret = Z_OK; do { ... } while (have);` (L104-L140)
    let mut ret = ReturnCode::OK;
    loop {
        // L106-L109, reproduced verbatim:
        //
        //     if (strm->avail_out == 0 || (flush != Z_NO_FLUSH &&
        //         (flush != Z_FINISH || ret == Z_STREAM_END)))
        //
        // ★ The `Z_FINISH` clause is load-bearing rather than an optimisation. It withholds the
        // write until `deflate` has reported `Z_STREAM_END`, so the whole member -- payload, CRC-32
        // and length -- reaches the file in one pass instead of being dribbled out around the
        // trailer.
        if state.strm.avail_out == 0
            || (flush != Flush::NoFlush
                && (flush != Flush::Finish || ret == ReturnCode::STREAM_END))
        {
            flush_out(state)?;

            // L124-L128: the buffer is full and has just been drained, so recycle it. All three
            // cursors go back to the start together, which is what keeps `next_out > out_pos`
            // meaningful.
            if state.strm.avail_out == 0 {
                state.strm.avail_out = state.size();
                state.strm.next_out = 0;
                if state.set_output_window(0, 0).is_err() {
                    gz_error(state, ReturnCode::STREAM_ERROR, Some(DEFLATE_CORRUPT));
                    return Err(ReturnCode::STREAM_ERROR);
                }
            }
        }

        // `have = strm->avail_out; ret = deflate(strm, flush); ... have -= strm->avail_out;`
        // (L132-L139)
        let before = state.strm.avail_out;
        ret = deflate_once(state, flush, source);
        if ret == ReturnCode::STREAM_ERROR {
            gz_error(state, ReturnCode::STREAM_ERROR, Some(DEFLATE_CORRUPT));
            return Err(ReturnCode::STREAM_ERROR);
        }

        // `} while (have);` (L140). The loop is driven by whether `deflate` produced anything, not
        // by whether the input ran out: a call that consumed input into the window without emitting
        // a byte ends the loop, and the bytes come out on a later call or at the next flush.
        if before == state.strm.avail_out {
            break;
        }
    }

    // `if (flush == Z_FINISH) state->reset = 1;` (L143-L144) -- "if that completed a deflate
    // stream, allow another to start".
    if flush == Flush::Finish {
        state.set_reset_pending(true);
    }
    Ok(())
}

/// Compresses whatever is buffered at the input cursor and writes it to the file.
///
/// The port of `gz_comp` (`gzwrite.c` L65-L148) over the layer's own input buffer, which is every
/// caller except `gz_write`'s large-input path. C's header comment is the specification:
///
/// > Compress whatever is at `avail_in` and `next_in` and write to the output file. Return -1 if
/// > there is an error writing to the output file or if `gz_init()` fails to allocate memory,
/// > otherwise 0. `flush` is assumed to be a valid `deflate()` flush value. If `flush` is
/// > `Z_FINISH`, then the `deflate()` state is reset to start a new gzip stream. If `gz->direct` is
/// > true, then simply write to the output file without compressing, and ignore flush.
///
/// `flush` is [`crate::deflate::Flush`], the workspace's single flush vocabulary; this module
/// defines no flush type of its own. The value is assumed valid, as C assumes it: [`gzflush`] is
/// where a caller's flush argument is screened.
///
/// # Errors
///
/// * [`ReturnCode::MEM_ERROR`] if the first call has to allocate and cannot (`gz_init`).
/// * [`ReturnCode::ERRNO`] if a write to the file failed. `GzState::again` distinguishes a
///   non-blocking stall, which callers use to decide whether a partial count may be reported.
/// * [`ReturnCode::STREAM_ERROR`] if the compressor reported `Z_STREAM_ERROR`.
///
/// The code is also recorded on the state through `gz_error`, so a caller that only needs the C
/// return convention can ignore it and read `GzState::err` -- which is what `gzflush` and
/// `gzclose_w` do.
pub(crate) fn gz_comp<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
    flush: Flush,
) -> Result<(), ReturnCode> {
    gz_comp_source(state, flush, None)
}

/// Compresses `GzState::skip` zero bytes, satisfying a forward seek on a write stream.
///
/// Implements `gz_zero` (`gzwrite.c` L154-L182), `local` in C.
///
/// A forward `gzseek` on a write stream does not move a file pointer; it records the distance in
/// `skip` (`gzlib.c` L432) and the next write operation pays for it by compressing that many zero
/// bytes. `test/example.c` L113 exercises exactly one byte of it with
/// `gzseek(file, 1L, SEEK_CUR)`, and the file it produces must contain a single trailing NUL.
///
/// The accounting is deliberately performed **before** the error is propagated (C's `if (ret == -1)`
/// sits after the three updates at L175-L177), so a stalled non-blocking destination leaves `skip`
/// holding only what is still owed and the operation can be retried.
///
/// # Two behaviour divergences -- divergence 7 of the `gz/open.rs` inventory
///
/// Both are FORCED and remain UNRESOLVED with respect to exact behaviour preservation. Neither is
/// claimed as an improvement, even though each avoids a defect in the reference implementation:
/// matching C exactly here would mean compressing uninitialised heap memory.
///
/// 1. **The buffers are allocated up front.** C allocates them lazily from inside the loop's first
///    `gz_comp` call. That is reachable: `gzseek` arms `skip` without allocating anything
///    (`gzlib.c` L432), so `gzputc` can call this with `state->size == 0`
///    (`gzwrite.c` L325 precedes the `if (state->size)` test at L330). On that path C's first
///    iteration computes a chunk of zero, and -- this is the defect -- **consumes its `first` flag on
///    it**, so the `memset` at L169 covers zero bytes and no later iteration zeroes anything: the
///    stream is then filled from a buffer that `malloc` never initialised. Allocating before the
///    loop removes the wasted pass. It is output-neutral, because that pass compresses no input and
///    only moves the gzip header from the compressor's pending buffer into the staging buffer, which
///    happens in the same order either way.
/// 2. **The zeroing is tied to the first non-empty chunk**, not to the first iteration. Chunks never
///    grow -- each is `min(skip_remaining, size)` and `skip` only decreases -- so zeroing the
///    largest one first is sufficient, which is the property C's `first` flag relies on and which
///    the defect above breaks.
///
/// Neither divergence changes a byte for any stream C already handled correctly: when `size` is
/// already non-zero, the first iteration's chunk is `min(skip, size)`, which is non-zero because
/// `skip` is, and the two implementations agree step for step. The residue -- the case where C
/// compresses uninitialised memory -- is where the outputs differ, and no claim of equivalence is
/// made for it.
///
/// # Errors
///
/// * [`ReturnCode::MEM_ERROR`] if the buffers cannot be allocated (`gz_init`).
/// * [`ReturnCode::ERRNO`] if a write failed; `GzState::again` reports a non-blocking stall.
/// * [`ReturnCode::STREAM_ERROR`] if the compressor reported `Z_STREAM_ERROR`, or accepted a
///   non-empty input and then reported success without consuming any of it. The latter cannot
///   happen -- `deflate` always drains available input when it has output room -- and is rejected
///   rather than retried so that this loop cannot spin.
pub(crate) fn gz_zero<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
) -> Result<(), ReturnCode> {
    // Divergence 1; see this function's documentation.
    if state.size() == 0 {
        gz_init(state)?;
    }

    // `if (strm->avail_in && gz_comp(state, Z_NO_FLUSH) == -1) return -1;` (L160-L161): whatever the
    // caller had buffered belongs in the stream before the zeros do.
    if state.strm.avail_in != 0 {
        gz_comp(state, Flush::NoFlush)?;
    }

    let mut first = true;
    loop {
        // L166-L167. `gt_off` guards the comparison of an `unsigned` against a signed `z_off64_t`;
        // it is false wherever `sizeof(int) != sizeof(z_off64_t)`, which is every supported target.
        let size = state.size();
        let skip = state.skip();
        let chunk = if gt_off(size) || ZOff64::from(size) > skip {
            // `skip < size` here, so it fits; `size` is the clamp for the `gt_off` case, which no
            // supported target takes.
            c_uint::try_from(skip).unwrap_or(size)
        } else {
            size
        };

        // L168-L171, with divergence 2 applied: the guard is "first non-empty chunk", not "first
        // iteration".
        if first && chunk != 0 {
            let count = to_index(chunk);
            if let Some(zeros) = state.in_slice_mut().get_mut(..count) {
                zeros.fill(0);
            }
            first = false;
        }

        state.strm.avail_in = chunk;
        state.strm.next_in = 0;
        let outcome = gz_comp(state, Flush::NoFlush);

        // L175-L177: the accounting happens whether or not the write succeeded.
        let consumed = to_index(chunk).saturating_sub(to_index(state.strm.avail_in));
        state.add_pos(to_offset(consumed));
        state.set_skip(state.skip().saturating_sub(to_offset(consumed)));

        outcome?;

        // The termination guarantee C leaves implicit. A non-empty chunk that the compressor
        // reported success on must have been consumed, or `skip` would never reach zero.
        if chunk != 0 && consumed == 0 {
            gz_error(state, ReturnCode::STREAM_ERROR, Some(DEFLATE_CORRUPT));
            return Err(ReturnCode::STREAM_ERROR);
        }

        // `} while (state->skip);` (L180)
        if state.skip() == 0 {
            return Ok(());
        }
    }
}

/// Compresses `buf` and writes it to the file, returning how many of its bytes were accepted.
///
/// Implements `gz_write` (`gzwrite.c` L188-L252), `local` in C. Its contract is the one every
/// public entry point below inherits: the count returned is how many of `buf`'s bytes were accepted,
/// a short count means an error, and a short *non-zero* count means a non-blocking stall.
///
/// So a short return is **never** a partial success in the ordinary sense: it means an error
/// occurred, and a *non-zero* short return additionally means the error was a stall and that many
/// bytes did make it into the stream.
///
/// # The two paths
///
/// | Condition | `gzwrite.c` | Behaviour |
/// |---|---|---|
/// | `len < state->size` | L205-L227 | copy into the input buffer, compressing whenever it fills |
/// | otherwise | L228-L248 | drain the buffer, then point the compressor straight at `buf` |
///
/// The second path is where C writes `state->strm.next_in = (z_const Bytef *)buf` (L234), moving the
/// stream's input cursor out of the layer's own buffer and into the caller's. This implementation expresses
/// that by keeping the cursor as an index and changing which base it indexes, which is what
/// `gz_comp_source`'s `source` argument selects.
///
/// # A behaviour divergence -- divergence 8 of the `gz/open.rs` inventory
///
/// FORCED, and UNRESOLVED with respect to exact behaviour preservation. It is not an improvement:
/// it is a difference this port cannot avoid, because matching C would mean reproducing an
/// out-of-bounds write.
///
/// C leaves `strm->next_in` pointing into the caller's buffer when the direct path ends. On the
/// ordinary path `avail_in` is zero by then and the next call resets the cursor
/// (`if (state->strm.avail_in == 0) state->strm.next_in = state->in;`, L210-L211), so the dangling
/// pointer is never read. On a **non-blocking stall** it is not zero, and the next `gzwrite` with a
/// small `len` computes `have = (next_in + avail_in) - state->in` from a pointer into unrelated
/// memory, derives `copy = state->size - have` from it -- an unsigned subtraction that underflows --
/// and `memcpy`s into `state->in + have`. That is an out-of-bounds write, so it cannot be
/// reproduced. This implementation instead clears both cursors when the direct path ends: nothing is lost,
/// because the count returned already tells the caller exactly how many bytes were accepted, and
/// the caller re-supplies the rest.
///
/// Returns 0 for an empty `buf`, matching C's `if (len == 0) return 0;` (L193-L194).
pub(crate) fn gz_write<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
    buf: &[u8],
) -> usize {
    let put = buf.len();

    if put == 0 {
        return 0;
    }

    if state.size() == 0 && gz_init(state).is_err() {
        return 0;
    }

    // L200-L202: a forward seek recorded by `gzseek` is paid for before anything else is written.
    if state.skip() != 0 && gz_zero(state).is_err() {
        return 0;
    }

    let size = to_index(state.size());
    let mut len = put;

    if len < size {
        // L205-L227: the buffered path.
        let mut taken = 0_usize;
        loop {
            if state.strm.avail_in == 0 {
                state.strm.next_in = 0;
            }

            // L212-L216. `have` is how much of `in` is occupied counting from its start, which is
            // where the copy has to land; `copy` is the room left in the first `size` bytes.
            // `saturating_sub` stands in for C's unsigned subtraction, which underflows when
            // `gzprintf` has pushed the contents past `size` -- see the divergence note above.
            let have = state
                .strm
                .next_in
                .saturating_add(to_index(state.strm.avail_in));
            let copy = size.saturating_sub(have).min(len);
            let target_end = have.saturating_add(copy);
            let source_end = taken.saturating_add(copy);

            // L217: `memcpy(state->in + have, buf, copy)`, bounds checked at both ends.
            let copied = {
                let source = buf.get(taken..source_end);
                let target = state.in_slice_mut().get_mut(have..target_end);
                match (source, target) {
                    (Some(source), Some(target)) => {
                        target.copy_from_slice(source);
                        true
                    }
                    _ => false,
                }
            };
            if !copied {
                gz_error(state, ReturnCode::STREAM_ERROR, Some(DEFLATE_CORRUPT));
                return 0;
            }

            state.strm.avail_in = state.strm.avail_in.saturating_add(to_count(copy));
            state.add_pos(to_offset(copy));
            taken = source_end;
            len = len.saturating_sub(copy);

            if len == 0 {
                break;
            }

            if gz_comp(state, Flush::NoFlush).is_err() {
                return if state.again() {
                    put.saturating_sub(len)
                } else {
                    0
                };
            }
        }
    } else {
        // L229-L231: whatever is buffered goes out first, so that the compressor can be pointed at
        // the caller's buffer without two input sources being live at once.
        if state.strm.avail_in != 0 && gz_comp(state, Flush::NoFlush).is_err() {
            return 0;
        }

        // L234: `state->strm.next_in = (z_const Bytef *)buf;` -- the cursor now indexes `buf`.
        state.strm.next_in = 0;

        let completed = loop {
            // L236-L240: one `unsigned`'s worth at a time, because `avail_in` is an `unsigned`.
            let chunk = to_index(c_uint::MAX).min(len);
            state.strm.avail_in = to_count(chunk);
            let failed = gz_comp_source(state, Flush::NoFlush, Some(buf)).is_err();

            // L242-L244: the accounting, again ahead of the error test.
            let consumed = chunk.saturating_sub(to_index(state.strm.avail_in));
            state.add_pos(to_offset(consumed));
            len = len.saturating_sub(consumed);

            if failed {
                break false;
            }
            if len == 0 {
                break true;
            }
            // The same termination guarantee `gz_zero` states: a successful call over a non-empty
            // chunk must have consumed something.
            if consumed == 0 {
                gz_error(state, ReturnCode::STREAM_ERROR, Some(DEFLATE_CORRUPT));
                break false;
            }
        };

        // The divergence: never leave the cursors describing the caller's buffer.
        state.strm.avail_in = 0;
        state.strm.next_in = 0;

        if !completed {
            return if state.again() {
                put.saturating_sub(len)
            } else {
                0
            };
        }
    }

    // L250-L251: "input was all buffered or compressed".
    put
}

/// Makes the second half of the input buffer available again, for `gzprintf`'s benefit.
///
/// Implements `gz_vacate` (`gzwrite.c` L382-L396), `local` in C and wrapped there in the
/// `NO_vsnprintf`/`NO_snprintf` preprocessor condition at L374-L376 because `gzprintf` is its only
/// caller and `gzprintf` itself disappears under those macros. This implementation has no such condition --
/// `printf.rs` is always compiled -- so the function is unconditionally available and `pub(crate)`
/// so that `printf.rs` can reach it.
///
/// The return value is therefore an occupancy report, not a status: `false` means the second half is
/// free. `gz_comp`'s own result is deliberately discarded, exactly as C's `(void)gz_comp(...)`
/// discards it, because the caller inspects `GzState::err` and `GzState::again` instead -- `gzprintf`
/// needs to tell a stall apart from a hard failure so it can report `Z_BUF_ERROR` and be retried
/// (`gzwrite.c` L439-L449).
///
/// # Post-conditions `printf.rs` relies on
///
/// When this returns `false`, both of the following hold, which together are what let `gzprintf`
/// format `size` bytes at `in + next_in + avail_in` without a bounds check of its own:
///
/// * the buffered input starts at the beginning of `in`, i.e. `strm.next_in` is zero, **or** it was
///   already wholly inside the first half and was left where it was;
/// * at least `size` bytes are free between the end of the buffered input and the end of `in`, which
///   is `2 * size` bytes long.
///
/// When it returns `true` the second half is still partly occupied, which happens only after a
/// non-blocking stall left more than `size` bytes unwritten.
pub(crate) fn gz_vacate<'a, A: Allocator<'a> + Copy>(state: &mut GzState<'a, A>) -> bool {
    let size = to_index(state.size());

    let occupied = state
        .strm
        .next_in
        .saturating_add(to_index(state.strm.avail_in));
    if occupied <= size {
        return false;
    }

    // L388: the result is the caller's to read off `state.err`.
    let _ = gz_comp(state, Flush::NoFlush);

    if state.strm.avail_in == 0 {
        state.strm.next_in = 0;
        return false;
    }

    // L393-L394: `memmove(state->in, strm->next_in, strm->avail_in)`, as an in-place slice move.
    let start = state.strm.next_in;
    let count = to_index(state.strm.avail_in);
    let end = start.saturating_add(count);
    let buffer = state.in_slice_mut();
    if end <= buffer.len() {
        buffer.copy_within(start..end, 0);
    }
    state.strm.next_in = 0;

    to_index(state.strm.avail_in) > size
}

/// The entry guard every public write entry point shares (`gzwrite.c` L263-L266).
///
/// ```c
/// if (state->mode != GZ_WRITE || (state->err != Z_OK && !state->again))
///     return <failure>;
/// gz_error(state, Z_OK, NULL);
/// ```
///
/// ★ The error half is **deliberately narrower than the read side's**. `gzread` and its siblings
/// tolerate a lingering `Z_BUF_ERROR` (`gzread.c` L407, `state->err != Z_OK && state->err !=
/// Z_BUF_ERROR`); the write side does not. A `Z_BUF_ERROR` on a write stream means `gzprintf`
/// stalled and must be retried (`gzwrite.c` L445), so continuing to write past it would interleave
/// the retried text with whatever came next. The asymmetry is real and is observable through
/// `gzerror`; it is reproduced here rather than tidied away.
///
/// `state->again` is the escape hatch: a stream that stalled on a non-blocking destination is not in
/// error, it is waiting, so the caller may keep going.
///
/// Also performs the entry half of the exposed-prefix contract. Returns `false` when the caller must
/// bail out, in which case nothing has been modified and the prefix must **not** be refreshed.
fn enter_write<'a, A: Allocator<'a>>(state: &mut GzState<'a, A>) -> bool {
    // `GzState::resync_from_exposed` recovers the flush cursor from the pointer the caller's
    // `gzgetc` macro can advance. On a write stream `x.have` is always zero so the macro cannot have
    // touched it, but the contract is unconditional and a state that fails the check is one this
    // library did not produce.
    if state.resync_from_exposed().is_err() {
        return false;
    }
    // The two remaining tests -- `state->mode != GZ_WRITE || state->err != Z_OK` -- live in
    // `GzState::accepts_writes` rather than here, because the C ABI facade has to ask the
    // same question one step earlier: C measures `gzputs`'s string only after them, so a
    // facade that scanned the string first would read a pointer C never touches. One
    // definition, two callers, no drift.
    if !state.accepts_writes() {
        return false;
    }
    gz_error(state, ReturnCode::OK, None);
    true
}

/// Compresses `buf` and writes it to the file.
///
/// Implements `gzwrite` (`gzwrite.c` L255-L277), declared at `zlib.h` L1519.
///
/// ★ The length rejection records **`Z_DATA_ERROR`** (L271), not the `Z_STREAM_ERROR` that `gzread`
/// records for the same condition on its side (`gzread.c` L414). Both codes are visible to a caller
/// through `gzerror`, so the asymmetry is part of the observable contract and is preserved.
///
/// `buf.len()` stands in for C's `unsigned len`, and "does not fit in an `int`" becomes "does not fit
/// in an `i32`", which is the same set of lengths: C's test `(int)len < 0` on an `unsigned` is true
/// exactly when `len > INT_MAX`.
///
/// Returns the number of bytes written as C's `int`, so the facade forwards it unchanged.
pub fn gzwrite<'a, A: Allocator<'a> + Copy>(state: &mut GzState<'a, A>, buf: &[u8]) -> i32 {
    if !enter_write(state) {
        return 0;
    }

    if i32::try_from(buf.len()).is_err() {
        gz_error(state, ReturnCode::DATA_ERROR, Some(LENGTH_NOT_INT));
        state.refresh_exposed();
        return 0;
    }

    let written = gz_write(state, buf);
    state.refresh_exposed();

    // `written <= buf.len()`, which the test above proved fits in an `i32`.
    i32::try_from(written).unwrap_or(i32::MAX)
}

/// Compresses `nitems` items of `size` bytes each and writes them to the file.
///
/// Implements `gzfwrite` (`gzwrite.c` L280-L304), declared at `zlib.h` L1529. It duplicates `fwrite`'s interface, so the count it returns is in **items**, not
/// bytes: `gz_write(...) / size`, and zero when the product is zero (L303).
///
/// The overflow test is C's, `size && len / size != nitems` (L297), expressed as a checked
/// multiplication; the recorded code is `Z_STREAM_ERROR` (L298), and `zlib.h` L1540-L1541 documents
/// that outcome explicitly: "If the multiplication of `size` and `nitems` overflows ... nothing is
/// written, zero is returned, and the error state is set to `Z_STREAM_ERROR`."
///
/// `buf` must be at least `size * nitems` bytes long. C has no way to check that -- it receives a
/// bare pointer -- so a shorter slice is a facade defect; it is reported as `Z_STREAM_ERROR` rather
/// than read past, which is the one thing C would do here that a safe implementation must not.
///
/// `zlib.h` L1543-L1545 warns that a partial item can be written on a non-blocking or concurrently
/// read file with `size != 1`, with no way to learn how much of it was lost. That is a property of
/// the interface, not of this implementation, and it is unchanged.
pub fn gzfwrite<'a, A: Allocator<'a> + Copy>(
    state: &mut GzState<'a, A>,
    buf: &[u8],
    size: usize,
    nitems: usize,
) -> usize {
    if !enter_write(state) {
        return 0;
    }

    let Some(len) = nitems.checked_mul(size) else {
        gz_error(state, ReturnCode::STREAM_ERROR, Some(REQUEST_NOT_SIZE_T));
        state.refresh_exposed();
        return 0;
    };

    // L302-L303: `return len ? gz_write(state, buf, len) / size : 0;`. A zero product needs no
    // buffer and cannot be divided by `size`, which is itself zero whenever the product is.
    if len == 0 {
        state.refresh_exposed();
        return 0;
    }

    let Some(payload) = buf.get(..len) else {
        gz_error(state, ReturnCode::STREAM_ERROR, Some(REQUEST_PAST_BUFFER));
        state.refresh_exposed();
        return 0;
    };

    let written = gz_write(state, payload);
    state.refresh_exposed();

    // `size != 0` here, because `len != 0` and `len == nitems * size`.
    written.checked_div(size).unwrap_or(0)
}

/// Compresses one byte and writes it to the file.
///
/// Implements `gzputc` (`gzwrite.c` L307-L347), declared at `zlib.h` L1607: "Compress and write
/// `c`, converted to an `unsigned char`, into file. `gzputc` returns the value that was written, or
/// -1 in case of error."
///
/// The fast path (L328-L340) writes straight into the input buffer without touching the compressor,
/// which is what makes a byte-at-a-time writer usable. It applies only when the buffer exists --
/// `state->size` is zero until `gz_init` has run -- and only while the first `size` bytes have room;
/// otherwise the byte goes through a one-byte `gz_write`, which allocates and compresses as needed.
///
/// ★ The `c & 0xff` masking appears on **both** return paths (L338 and L346) and is reproduced on
/// both: `gzputc(file, -1)` returns 255, not -1, because -1 is a perfectly good byte and -1 is also
/// the error code. Returning `c` unmasked would make a written `0xff` indistinguishable from a
/// failure.
pub fn gzputc<'a, A: Allocator<'a> + Copy>(state: &mut GzState<'a, A>, c: i32) -> i32 {
    if !enter_write(state) {
        return -1;
    }

    // L324-L326: the pending seek is paid for before the byte, and before the buffer is even known
    // to exist -- `gz_zero` allocates if it has to.
    if state.skip() != 0 && gz_zero(state).is_err() {
        state.refresh_exposed();
        return -1;
    }

    // C's `(unsigned char)c`. The mask puts the value in `0 ..= 255` first, so the conversion is
    // exact and the fallback is unreachable; masking also gives the same result C's cast does for a
    // negative `c`.
    let masked = c & 0xff;
    let byte = u8::try_from(masked).unwrap_or(0);

    if state.size() != 0 {
        if state.strm.avail_in == 0 {
            state.strm.next_in = 0;
        }
        let have = state
            .strm
            .next_in
            .saturating_add(to_index(state.strm.avail_in));
        if have < to_index(state.size()) {
            let stored = state.in_slice_mut().get_mut(have);
            if let Some(slot) = stored {
                *slot = byte;
                state.strm.avail_in = state.strm.avail_in.saturating_add(1);
                state.add_pos(1);
                state.refresh_exposed();
                return masked;
            }
        }
    }

    // L342-L346: no room, or no buffer yet.
    let written = gz_write(state, &[byte]);
    state.refresh_exposed();
    if written == 1 {
        masked
    } else {
        -1
    }
}

/// Compresses a string and writes it to the file, excluding any terminating NUL.
///
/// Implements `gzputs` (`gzwrite.c` L350-L372), declared at `zlib.h` L1575 and documented at
/// L1577-L1584: it returns the number of characters written, or -1 on error, and the count "may be
/// less than the length of the string if the write destination is non-blocking".
///
/// C computes the length with `strlen`, so the terminating NUL is not part of the string; `s` here is
/// already that run of bytes, and the facade is where the NUL is found. `test/example.c` L105 asserts
/// `gzputs(file, "ello") == 4`.
///
/// The length rejection records `Z_STREAM_ERROR` (L367). C tests both `(int)len < 0` and
/// `(unsigned)len != len`, which are the `int` and `unsigned` halves of "fits in the return type and
/// in what `gz_write` can be asked for"; both are reproduced.
///
/// A non-empty string that produced nothing is an error, so `-1` is returned rather than `0`
/// (L371) -- otherwise a caller could not tell a failure from a legitimately empty write.
pub fn gzputs<'a, A: Allocator<'a> + Copy>(state: &mut GzState<'a, A>, s: &[u8]) -> i32 {
    if !enter_write(state) {
        return -1;
    }

    let len = s.len();
    if i32::try_from(len).is_err() || c_uint::try_from(len).is_err() {
        gz_error(state, ReturnCode::STREAM_ERROR, Some(STRING_NOT_INT));
        state.refresh_exposed();
        return -1;
    }

    let put = gz_write(state, s);
    state.refresh_exposed();
    if len != 0 && put == 0 {
        return -1;
    }

    // `put <= len`, which the test above proved fits in an `i32`.
    i32::try_from(put).unwrap_or(i32::MAX)
}

/// Flushes all pending output to the file with the requested `deflate` flush mode.
///
/// Implements `gzflush` (`gzwrite.c` L603-L627), declared at `zlib.h` L1647.
///
/// ★ The accepted range is `Z_NO_FLUSH ..= Z_FINISH` (L617), which is **narrower than `deflate`'s**:
/// `Z_BLOCK` and `Z_TREES` are rejected with `Z_STREAM_ERROR` even though `deflate` itself accepts
/// `Z_BLOCK`. The `gzFile` layer uses `Z_BLOCK` internally -- `gzsetparams` passes it before changing
/// parameters (`gzwrite.c` L657) -- but will not take it from a caller.
///
/// The `Z_FINISH` behaviour the header promises is not implemented here: it falls out of `gz_comp`
/// arming `GzState::reset`, which defers the `deflateReset` until the next call has either something
/// buffered or a flush to perform (`gzwrite.c` L94-L101 and L143-L144). Note what that does *not*
/// promise: a `gzflush(Z_FINISH)` immediately before a `gzclose` still leaves an empty 20-byte member
/// behind, because `gzclose_w`'s own `Z_FINISH` is a flushing call and passes the guard. Verified
/// against the reference build, which grows the same file from 27 bytes to 47.
///
/// Returns `GzState::err` -- the recorded error number, exactly as C does at L626, so
/// `gz_comp`'s own result is discarded.
pub fn gzflush<'a, A: Allocator<'a> + Copy>(state: &mut GzState<'a, A>, flush: i32) -> i32 {
    if !enter_write(state) {
        return ReturnCode::STREAM_ERROR.as_i32();
    }

    // L616-L618: `if (flush < 0 || flush > Z_FINISH) return Z_STREAM_ERROR;`. Written as one
    // inclusive-range test rather than two comparisons because `clippy::manual_range_contains`
    // requires it; `Z_NO_FLUSH` is zero, so the two spellings accept exactly the same integers.
    if !(Z_NO_FLUSH..=Z_FINISH).contains(&flush) {
        state.refresh_exposed();
        return ReturnCode::STREAM_ERROR.as_i32();
    }
    let Some(mode) = Flush::from_raw(flush) else {
        // Unreachable: every value in `Z_NO_FLUSH ..= Z_FINISH` names a mode. Reported rather than
        // asserted so that this function stays panic-free.
        state.refresh_exposed();
        return ReturnCode::STREAM_ERROR.as_i32();
    };

    if state.skip() != 0 && gz_zero(state).is_err() {
        state.refresh_exposed();
        return state.err();
    }

    let _ = gz_comp(state, mode);
    state.refresh_exposed();
    state.err()
}

/// Finishes the gzip stream, releases everything the state owns and closes the file.
///
/// Implements `gzclose_w` (`gzwrite.c` L667-L700), declared at `zlib.h` L1759 and specified through
/// `gzclose` at L1751-L1757: it flushes, closes, and deallocates, returning `Z_STREAM_ERROR` for an
/// invalid file, `Z_ERRNO` for a file-operation error, or `Z_OK`.
///
/// ★ **The first error does not skip the teardown.** C latches each failure into `ret` and carries on
/// (L680-L697), so a stream that could not be finished still has its engine ended, its buffers freed,
/// its path freed and its file closed. Returning early would leak, and `test/infcover.c`'s `mem_done`
/// reports leaks as defects. The order is C's exactly:
///
/// 1. pay for a pending seek, latching `state->err` on failure (L680-L682);
/// 2. `gz_comp(state, Z_FINISH)`, latching `state->err` on failure (L684-L686);
/// 3. when the buffers exist: end the compressor and free `out`, then free `in` (L687-L693);
/// 4. clear the error and the message (L694);
/// 5. free the path (L695);
/// 6. close the file, mapping a failure to `Z_ERRNO` (L696-L697);
/// 7. free the structure (L698) -- **the facade's step**, because the facade is what allocated it.
///
/// `deflateEnd`'s status is discarded, as C's `(void)deflateEnd(...)` discards it: a `Z_DATA_ERROR`
/// there would only be reporting that the stream was still busy, which the `Z_FINISH` above has
/// already dealt with or already reported.
///
/// # Two divergences, both making a use-after-close deterministic
///
/// C frees the structure at L698, so touching the `gzFile` again is undefined behaviour; `zlib.h`
/// L1754-L1755 says as much ("`gzclose` must not be called more than once on the same file, just as
/// `free` must not be called more than once on the same allocation"). This implementation cannot free the
/// structure -- the facade owns it -- so it leaves it in a state that refuses further work instead:
///
/// * `size` goes back to zero, which it must: the buffers are gone, and `size` is the marker that
///   says whether they exist. Leaving it set would make the next `gz_comp` skip `gz_init` and then
///   fail to find a buffer.
/// * `mode` goes to [`GZ_NONE`], so a subsequent [`gzwrite`] returns 0 and a subsequent [`gzflush`]
///   or `gzclose_w` returns `Z_STREAM_ERROR` rather than quietly re-running the teardown.
pub fn gzclose_w<'a, A: Allocator<'a> + Copy>(state: &mut GzState<'a, A>) -> i32 {
    let mut ret = ReturnCode::OK.as_i32();

    // The entry half of the exposed-prefix contract. `gzclose_w` does not share the guard the other
    // entry points use: C checks only the mode here (L676-L678), because a stream in error still has
    // to be closed.
    if state.resync_from_exposed().is_err() {
        return ReturnCode::STREAM_ERROR.as_i32();
    }
    if state.mode() != GZ_WRITE {
        return ReturnCode::STREAM_ERROR.as_i32();
    }

    if state.skip() != 0 && gz_zero(state).is_err() {
        ret = state.err();
    }

    if gz_comp(state, Flush::Finish).is_err() {
        ret = state.err();
    }

    // L687-L693. `size == 0` means neither buffer nor engine was ever created, which is why C guards
    // the whole block; `release_buffers` frees the output buffer before the input one, the
    // last-in-first-out order `test/infcover.c`'s `mem_done` requires.
    if state.size() != 0 {
        if state.direct() == 0 {
            if let GzEngine::Deflate(engine) = state.take_engine() {
                if let Some(mut compressor) = engine.into_inner() {
                    let _ = deflate_end(&mut compressor);
                }
            }
        }
        state.release_buffers();
    }

    gz_error(state, ReturnCode::OK, None);

    state.clear_path();

    // L696-L697. `GzFileSlot::close` reports the handle's own result, so a facade handle backed
    // by a real `close(2)` answers `Z_ERRNO` exactly when C does.
    if state.take_handle().close().is_err() {
        ret = ReturnCode::ERRNO.as_i32();
    }

    // The divergences; see this function's documentation.
    state.set_size(0);
    state.set_mode(GZ_NONE);
    state.refresh_exposed();
    ret
}

#[cfg(test)]
mod tests {
    // The crate denies the panic-prone lints in library code, which is the right policy there and
    // the wrong one here: a test asserts, and an assertion that fails panics. clippy.toml already
    // relaxes them inside tests; stating it again keeps the intent local and visible.
    #![allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]

    use super::{
        gz_comp, gz_init, gz_vacate, gz_write, gz_zero, gzclose_w, gzflush, gzfwrite, gzputc,
        gzputs, gzwrite, to_index,
    };
    use crate::allocate::GlobalAllocator;
    use crate::config::{
        InflateConfig, Z_BLOCK, Z_DEFAULT_COMPRESSION, Z_DEFAULT_STRATEGY, Z_FINISH, Z_NO_FLUSH,
        Z_SYNC_FLUSH, Z_TREES,
    };
    use crate::deflate::Flush;
    use crate::error::ReturnCode;
    use crate::gz::state::{
        GzFileSlot, GzHandle, GzIoError, GzSeekFrom, GzState, ZOff64, GZBUFSIZE, GZ_NONE, GZ_READ,
        GZ_WRITE,
    };
    use crate::inflate::{inflate, inflate_init2, inflate_reset, InflateStream};
    use alloc::boxed::Box;
    use alloc::rc::Rc;
    use alloc::vec;
    use alloc::vec::Vec;
    use core::cell::{Cell, RefCell};
    use core::ffi::c_uint;

    /// The gzip magic and method bytes an RFC 1952 member opens with
    /// (`doc/rfc1952.txt` L242-L244, and the same test `gz_look` applies at `gzread.c` L151-L153).
    const GZIP_PREFIX: [u8; 3] = [0x1f, 0x8b, 0x08];

    /// A file that keeps everything written to it in a shared buffer.
    ///
    /// The stand-in for C's file descriptor. Nothing here touches the real filesystem, which keeps
    /// every test below runnable under Miri and makes the produced bytes directly inspectable --
    /// and the bytes are the whole point, because this module chooses the parameters that shape
    /// them.
    struct MemoryFile {
        /// Everything written so far, shared with the test so it can be read back after the state
        /// has taken the handle away for closing.
        sink: Rc<RefCell<Vec<u8>>>,
        /// The most bytes a single `write` will accept, or [`None`] for all of them. A small value
        /// forces `gz_comp`'s short-write loop (`gzwrite.c` L110-L123) to iterate.
        accept: Option<usize>,
        /// How many times the handle was closed, so the teardown order can be asserted.
        closes: Rc<Cell<usize>>,
    }

    impl GzHandle for MemoryFile {
        fn read(&mut self, _buf: &mut [u8]) -> Result<usize, GzIoError> {
            Ok(0)
        }

        fn write(&mut self, buf: &[u8]) -> Result<usize, GzIoError> {
            let take = self.accept.map_or(buf.len(), |limit| limit.min(buf.len()));
            self.sink.borrow_mut().extend_from_slice(&buf[..take]);
            Ok(take)
        }

        fn seek(&mut self, offset: ZOff64, _whence: GzSeekFrom) -> Result<ZOff64, GzIoError> {
            Ok(offset)
        }

        fn set_nonblocking(&mut self, _nonblocking: bool) -> Result<(), GzIoError> {
            Ok(())
        }

        fn close(&mut self) -> Result<(), GzIoError> {
            self.closes.set(self.closes.get() + 1);
            Ok(())
        }
    }

    /// A file that refuses every write with the non-blocking stall condition.
    ///
    /// `GzIoError::would_block` is this implementation's spelling of `errno == EAGAIN || errno == EWOULDBLOCK`
    /// (`gzwrite.c` L82 and L117), so this drives the `state->again` paths.
    struct StalledFile;

    impl GzHandle for StalledFile {
        fn read(&mut self, _buf: &mut [u8]) -> Result<usize, GzIoError> {
            Ok(0)
        }

        fn write(&mut self, _buf: &[u8]) -> Result<usize, GzIoError> {
            Err(GzIoError::new(11, true))
        }

        fn seek(&mut self, offset: ZOff64, _whence: GzSeekFrom) -> Result<ZOff64, GzIoError> {
            Ok(offset)
        }

        fn set_nonblocking(&mut self, _nonblocking: bool) -> Result<(), GzIoError> {
            Ok(())
        }

        fn close(&mut self) -> Result<(), GzIoError> {
            Ok(())
        }
    }

    /// A [`GzHandle`] that claims to have written more bytes than it was given.
    ///
    /// `write(2)` cannot behave this way, so C never has to defend against it; an injected
    /// trait object can, and believing it would advance the layer's cursor past bytes the file
    /// never received. Not reachable through any real file; it exists because the trait's
    /// contract has to be enforceable and enforcement has to be tested. See
    /// `HANDLE_OVER_REPORTED`.
    struct OverReportingFile {
        /// Everything it was genuinely given, so a test can prove the bytes were not lost.
        sink: Rc<RefCell<Vec<u8>>>,
        /// How much to add to the honest count. `1` is the minimal violation.
        excess: usize,
    }

    impl GzHandle for OverReportingFile {
        fn read(&mut self, _buf: &mut [u8]) -> Result<usize, GzIoError> {
            Ok(0)
        }

        fn write(&mut self, buf: &[u8]) -> Result<usize, GzIoError> {
            self.sink.borrow_mut().extend_from_slice(buf);
            Ok(buf.len().saturating_add(self.excess))
        }

        fn seek(&mut self, offset: ZOff64, _whence: GzSeekFrom) -> Result<ZOff64, GzIoError> {
            Ok(offset)
        }

        fn set_nonblocking(&mut self, _nonblocking: bool) -> Result<(), GzIoError> {
            Ok(())
        }

        fn close(&mut self) -> Result<(), GzIoError> {
            Ok(())
        }
    }

    /// The three shared pieces a test needs: the state, the bytes written, and the close counter.
    struct Fixture {
        state: GzState<'static, GlobalAllocator>,
        sink: Rc<RefCell<Vec<u8>>>,
        closes: Rc<Cell<usize>>,
    }

    /// A write-mode state, in exactly the condition `gz_open` leaves one in for `"wb"`.
    ///
    /// `gz_open` sets `mode`, `level`, `strategy`, `direct` and the handle and then leaves `size` at
    /// zero so that `gz_init` runs lazily (`gzlib.c` L103-L112 and L248). `open.rs` is where that
    /// happens for real; assembling it here keeps these tests independent of it.
    fn fixture(want: c_uint, level: i32, direct: i32, accept: Option<usize>) -> Fixture {
        let sink = Rc::new(RefCell::new(Vec::new()));
        let closes = Rc::new(Cell::new(0));
        let mut state = GzState::new(GlobalAllocator);
        state.set_mode(GZ_WRITE);
        state.set_want(want);
        state.set_level(level);
        state.set_strategy(Z_DEFAULT_STRATEGY);
        state.set_direct(direct);
        let previous = state.set_handle(GzFileSlot::Boxed(Box::new(MemoryFile {
            sink: Rc::clone(&sink),
            accept,
            closes: Rc::clone(&closes),
        })));
        assert!(!previous.is_installed(), "the slot was empty before");
        Fixture {
            state,
            sink,
            closes,
        }
    }

    /// The default fixture: a compressing stream with the reference buffer size and level.
    fn plain() -> Fixture {
        fixture(GZBUFSIZE, Z_DEFAULT_COMPRESSION, 0, None)
    }

    /// Decompresses a concatenation of gzip members, the way `gzread` does.
    ///
    /// `gz/read.rs` is the module that does this for real, and it is deliberately not used here:
    /// driving the decoder directly is the stronger check, because it proves the bytes are a
    /// well-formed RFC 1952 stream rather than merely something this implementation's own reader
    /// happens to accept. Each member
    /// ends with `Z_STREAM_END`, after which `inflate_reset` starts the next one -- which is exactly
    /// what `zlib.h` L1654-L1657 promises about a `gzflush(Z_FINISH)` followed by more writes.
    fn gunzip(bytes: &[u8]) -> Vec<u8> {
        if bytes.is_empty() {
            return Vec::new();
        }
        // 31 is `MAX_WBITS + 16`: the largest window, with the gzip wrapper selected.
        let mut decoder = inflate_init2(InflateConfig::new(31), GlobalAllocator).unwrap();
        let mut out = vec![0_u8; 1 << 16];
        let mut produced = 0_usize;
        let mut consumed = 0_usize;
        loop {
            // Grow rather than fail: `inflate` reports `Z_BUF_ERROR` when it has no room, and a
            // test that ran out of buffer would look like a defect in the writer.
            if produced == out.len() {
                out.resize(out.len().saturating_mul(2), 0);
            }
            let mut stream = InflateStream::new(bytes, &mut out);
            stream.next_in = consumed;
            stream.next_out = produced;
            let code = inflate(&mut decoder, &mut stream, Z_NO_FLUSH);
            consumed = stream.next_in;
            produced = stream.next_out;
            assert!(
                code == ReturnCode::OK || code == ReturnCode::STREAM_END,
                "inflate reported {code:?} after {produced} bytes"
            );
            if code == ReturnCode::STREAM_END {
                if consumed == bytes.len() {
                    break;
                }
                // A further member follows; `gzread` restarts the decoder the same way.
                let _reset = inflate_reset(&mut decoder);
                continue;
            }
            assert!(
                consumed < bytes.len(),
                "inflate wants more input than the stream contains"
            );
        }
        out.truncate(produced);
        out
    }

    #[test]
    fn gz_init_allocates_the_write_shape_and_primes_the_cursors() {
        let mut fixture = plain();
        let state = &mut fixture.state;
        assert_eq!(state.size(), 0, "the marker starts clear");

        gz_init(state).unwrap();

        // `gzwrite.c` L16 and L25: the input buffer is the doubled one on this path.
        assert_eq!(state.size(), GZBUFSIZE);
        assert_eq!(state.in_slice().len(), 2 * to_index(GZBUFSIZE));
        assert_eq!(state.out_slice().len(), to_index(GZBUFSIZE));

        // L51-L55: the staging cursors and the flush cursor all start together.
        assert_eq!(state.stream().avail_out, GZBUFSIZE);
        assert_eq!(state.stream().next_out, 0);
        assert_eq!(state.out_pos(), 0);

        // The invariant this module rests on: nothing is ever available for delivery on a write
        // stream, so a caller's `gzgetc` macro can never take its fast path.
        assert_eq!(state.have(), 0);
    }

    #[test]
    fn a_transparent_stream_allocates_no_output_buffer_and_no_engine() {
        // `gzwrite.c` L23: `if (!state->direct)` guards both.
        let mut fixture = fixture(64, Z_DEFAULT_COMPRESSION, 1, None);
        gz_init(&mut fixture.state).unwrap();
        assert_eq!(fixture.state.in_slice().len(), 128);
        assert!(fixture.state.out_slice().is_empty());
        assert!(fixture.state.stream().engine.is_none());
    }

    #[test]
    fn the_first_bytes_written_are_a_gzip_member() {
        // The whole point of `MAX_WBITS + 16` at `gzwrite.c` L37.
        let mut fixture = plain();
        assert_eq!(gzwrite(&mut fixture.state, b"hello, hello!"), 13);
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        let written = fixture.sink.borrow().clone();
        assert_eq!(&written[..3], &GZIP_PREFIX, "not an RFC 1952 member");
        assert_eq!(gunzip(&written), b"hello, hello!");
    }

    #[test]
    fn the_buffered_path_round_trips() {
        // `len < state->size`, so `gzwrite.c` L205-L227 copies into the input buffer.
        let mut fixture = plain();
        let payload = b"the small-len path buffers instead of compressing directly";
        assert!(payload.len() < to_index(GZBUFSIZE));
        let expected = i32::try_from(payload.len()).unwrap();
        assert_eq!(gzwrite(&mut fixture.state, payload), expected);
        // Nothing has reached the file yet: `Z_NO_FLUSH` with room left writes nothing.
        assert!(fixture.sink.borrow().is_empty());
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        assert_eq!(gunzip(&fixture.sink.borrow()), payload);
    }

    #[test]
    fn the_direct_path_round_trips_more_than_one_buffer() {
        // `len >= state->size`, so `gzwrite.c` L228-L248 points the compressor at the caller's
        // buffer. A repetitive payload also exercises long matches across the window.
        let mut fixture = fixture(256, 6, 0, None);
        let mut payload = Vec::new();
        for index in 0..5000_u32 {
            payload.extend_from_slice(&index.to_le_bytes());
            payload.extend_from_slice(b"direct path ");
        }
        assert!(payload.len() > 256);
        assert_eq!(gz_write(&mut fixture.state, &payload), payload.len());
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        assert_eq!(gunzip(&fixture.sink.borrow()), payload);

        // The divergence this module documents: the cursors never keep describing the caller's
        // buffer once the direct path is done with it.
        assert_eq!(fixture.state.stream().next_in, 0);
        assert_eq!(fixture.state.stream().avail_in, 0);
    }

    #[test]
    fn a_short_writing_file_still_receives_everything() {
        // `gz_comp`'s inner loop exists for exactly this (`gzwrite.c` L110-L123).
        let mut fixture = fixture(64, 9, 0, Some(3));
        let payload: Vec<u8> = (0..4096_u32)
            .map(|value| u8::try_from(value % 251).unwrap())
            .collect();
        assert_eq!(gz_write(&mut fixture.state, &payload), payload.len());
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        assert_eq!(gunzip(&fixture.sink.borrow()), payload);
    }

    #[test]
    fn gzputc_fills_the_buffer_and_then_spills_into_gz_write() {
        // `gzwrite.c` L330-L340 is the in-buffer fast path; L342-L346 is the fallback. With
        // `size` at 8 the switch happens on the ninth byte.
        let mut fixture = fixture(8, 1, 0, None);
        for index in 0..8_u8 {
            assert_eq!(
                gzputc(&mut fixture.state, i32::from(b'a' + index)),
                97 + i32::from(index)
            );
        }
        // The first `size` bytes are now occupied, so the next byte cannot take the fast path.
        assert_eq!(fixture.state.stream().avail_in, 8);
        assert_eq!(gzputc(&mut fixture.state, i32::from(b'i')), i32::from(b'i'));
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        assert_eq!(gunzip(&fixture.sink.borrow()), b"abcdefghi");
    }

    #[test]
    fn gzputc_masks_its_return_value_on_both_paths() {
        // `c & 0xff` at `gzwrite.c` L338 and L346. -1 is a legal byte and also the error code, so
        // the mask is what keeps them apart.
        let mut fixture = fixture(4, 1, 0, None);
        assert_eq!(gzputc(&mut fixture.state, -1), 0xff, "fast path");
        for _ in 0..4 {
            assert!(gzputc(&mut fixture.state, 0x1ff) >= 0);
        }
        assert_eq!(gzputc(&mut fixture.state, -1), 0xff, "gz_write fallback");
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        let plain = gunzip(&fixture.sink.borrow());
        assert_eq!(plain, [0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
    }

    #[test]
    fn gzputs_reports_the_character_count() {
        // `test/example.c` L105 asserts exactly this.
        let mut fixture = plain();
        assert_eq!(gzputs(&mut fixture.state, b"ello"), 4);
        assert_eq!(
            gzputs(&mut fixture.state, b""),
            0,
            "an empty string is not an error"
        );
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        assert_eq!(gunzip(&fixture.sink.borrow()), b"ello");
    }

    #[test]
    fn gzfwrite_counts_whole_items_and_rejects_an_overflowing_product() {
        let mut fixture = plain();
        // `gzwrite.c` L303: the count is in items.
        assert_eq!(gzfwrite(&mut fixture.state, b"abcdef", 2, 3), 3);
        // L297-L300: `nitems * size` overflows, so nothing is written.
        assert_eq!(gzfwrite(&mut fixture.state, b"abcdef", usize::MAX, 2), 0);
        assert_eq!(fixture.state.err(), ReturnCode::STREAM_ERROR.as_i32());
        // The recorded error is not `Z_BUF_ERROR`, so the stream now refuses further writes --
        // which is the write side's narrower entry guard (L264) rather than the read side's.
        assert_eq!(gzwrite(&mut fixture.state, b"more"), 0);
    }

    #[test]
    fn gzfwrite_with_a_zero_size_writes_nothing() {
        // `len == 0`, so L303 takes its `: 0` branch without dividing by zero.
        let mut fixture = plain();
        assert_eq!(gzfwrite(&mut fixture.state, b"abcdef", 0, 3), 0);
        assert_eq!(gzfwrite(&mut fixture.state, b"abcdef", 2, 0), 0);
        assert_eq!(fixture.state.err(), ReturnCode::OK.as_i32());
    }

    #[test]
    fn gzfwrite_refuses_a_request_longer_than_the_buffer() {
        // The guard that replaces C's unchecked read past the caller's pointer.
        let mut fixture = plain();
        assert_eq!(gzfwrite(&mut fixture.state, b"abc", 4, 2), 0);
        assert_eq!(fixture.state.err(), ReturnCode::STREAM_ERROR.as_i32());
    }

    #[test]
    fn gzflush_finish_starts_a_new_member_only_when_there_is_more_data() {
        // `zlib.h` L1654-L1657 and the deferred reset at `gzwrite.c` L94-L101.
        let mut fixture = plain();
        assert_eq!(gzwrite(&mut fixture.state, b"first member"), 12);
        assert_eq!(
            gzflush(&mut fixture.state, Z_FINISH),
            ReturnCode::OK.as_i32()
        );
        let after_first = fixture.sink.borrow().len();
        assert!(
            after_first > 0,
            "Z_FINISH must have written the whole member"
        );
        assert!(fixture.state.reset_pending(), "a reset is armed");

        // The deferral proper: an *unflushed* call with nothing buffered must not start a member.
        // This is the shape `gz_write`'s drain, `gz_zero` and `gz_vacate` all use.
        gz_comp(&mut fixture.state, Flush::NoFlush).unwrap();
        assert_eq!(fixture.sink.borrow().len(), after_first, "no empty member");
        assert!(fixture.state.reset_pending(), "still armed");

        // A buffered write does not reach `gz_comp` at all (`gzwrite.c` L222-L223 breaks out before
        // L224), so the reset is still armed afterwards -- it is disarmed by whichever call finally
        // compresses, which here is `gzclose_w`'s `Z_FINISH`.
        assert_eq!(gzwrite(&mut fixture.state, b"second member"), 13);
        assert!(
            fixture.state.reset_pending(),
            "still armed until something compresses"
        );
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        // Armed again rather than clear: `gz_comp` disarms the flag on entry and then re-arms it on
        // the way out for every `Z_FINISH` (`gzwrite.c` L143-L144), so a finished stream always ends
        // with a reset pending.
        assert!(fixture.state.reset_pending());

        let written = fixture.sink.borrow().clone();
        assert_eq!(&written[..3], &GZIP_PREFIX);
        assert_eq!(
            &written[after_first..after_first + 3],
            &GZIP_PREFIX,
            "the second member must start where the first one ended"
        );
        assert_eq!(gunzip(&written), b"first membersecond member");
    }

    #[test]
    fn a_finish_before_close_leaves_the_reference_empty_trailing_member() {
        // The exact numbers come from running this sequence against the reference build: the file
        // is 27 bytes after `gzflush(Z_FINISH)` and 47 after `gzclose`, because `gzclose_w`'s own
        // `Z_FINISH` is a flushing call and therefore passes the deferred-reset guard, emitting a
        // 20-byte empty member. Reproducing that -- rather than "improving" it away -- is what
        // keeps this layer byte-compatible.
        let mut fixture = plain();
        assert_eq!(gzwrite(&mut fixture.state, b"payload"), 7);
        assert_eq!(
            gzflush(&mut fixture.state, Z_FINISH),
            ReturnCode::OK.as_i32()
        );
        let after_flush = fixture.sink.borrow().len();
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        let total = fixture.sink.borrow().len();
        assert_eq!(
            total - after_flush,
            20,
            "an empty gzip member is exactly the ten-byte header plus a three-bit final \
             empty block padded to a byte plus the eight-byte trailer"
        );
        assert_eq!(
            &fixture.sink.borrow()[after_flush..after_flush + 3],
            &GZIP_PREFIX
        );
        // Both members together still decode to just the payload.
        assert_eq!(gunzip(&fixture.sink.borrow()), b"payload");
    }

    #[test]
    fn gzflush_rejects_the_decompression_only_modes() {
        // `gzwrite.c` L617: the accepted range stops at `Z_FINISH`, so `Z_BLOCK` and `Z_TREES` are
        // refused even though `deflate` itself accepts `Z_BLOCK`.
        let mut fixture = plain();
        assert_eq!(gzwrite(&mut fixture.state, b"data"), 4);
        assert_eq!(
            gzflush(&mut fixture.state, Z_BLOCK),
            ReturnCode::STREAM_ERROR.as_i32()
        );
        assert_eq!(
            gzflush(&mut fixture.state, Z_TREES),
            ReturnCode::STREAM_ERROR.as_i32()
        );
        assert_eq!(
            gzflush(&mut fixture.state, -1),
            ReturnCode::STREAM_ERROR.as_i32()
        );
        // A rejected flush is not recorded as an error, so the stream is still usable.
        assert_eq!(fixture.state.err(), ReturnCode::OK.as_i32());
        assert_eq!(
            gzflush(&mut fixture.state, Z_SYNC_FLUSH),
            ReturnCode::OK.as_i32()
        );
        assert!(
            !fixture.sink.borrow().is_empty(),
            "Z_SYNC_FLUSH must reach the file"
        );
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        assert_eq!(gunzip(&fixture.sink.borrow()), b"data");
    }

    #[test]
    fn a_forward_seek_emits_exactly_that_many_zeros() {
        // `gzseek` on a write stream records the distance in `skip` (`gzlib.c` L432) and the next
        // write pays for it in zeros. `test/example.c` L113 does this with one byte.
        let mut fixture = plain();
        assert_eq!(gzputs(&mut fixture.state, b"before"), 6);
        fixture.state.set_skip(5);
        gz_zero(&mut fixture.state).unwrap();
        assert_eq!(fixture.state.skip(), 0, "the whole skip was paid for");
        assert_eq!(fixture.state.pos(), 11, "six bytes plus five zeros");
        assert_eq!(gzputs(&mut fixture.state, b"after"), 5);
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        assert_eq!(gunzip(&fixture.sink.borrow()), b"before\0\0\0\0\0after");
    }

    #[test]
    fn a_seek_longer_than_the_buffer_still_emits_only_zeros() {
        // Forces `gz_zero`'s loop to run several times, which is where C's `first` flag matters:
        // every chunk after the first reuses the zeros the first one wrote.
        let mut fixture = fixture(16, 9, 0, None);
        fixture.state.set_skip(200);
        gz_zero(&mut fixture.state).unwrap();
        assert_eq!(fixture.state.skip(), 0);
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        assert_eq!(gunzip(&fixture.sink.borrow()), vec![0_u8; 200]);
    }

    #[test]
    fn a_seek_armed_before_any_buffer_exists_still_emits_zeros() {
        // The path that exposes C's defect: `gzseek` then `gzputc` reaches `gz_zero` with
        // `state->size == 0`, and C consumes its `first` flag on a zero-length pass, so the zeros
        // are never written and uninitialised `malloc` memory is compressed instead. This implementation
        // allocates first, so the bytes are genuinely zero.
        let mut fixture = fixture(32, 6, 0, None);
        assert_eq!(fixture.state.size(), 0, "no buffer yet");
        fixture.state.set_skip(40);
        assert_eq!(gzputc(&mut fixture.state, i32::from(b'z')), i32::from(b'z'));
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        let mut expected = vec![0_u8; 40];
        expected.push(b'z');
        assert_eq!(gunzip(&fixture.sink.borrow()), expected);
    }

    #[test]
    fn gzclose_w_pays_for_a_pending_skip() {
        let mut fixture = plain();
        assert_eq!(gzwrite(&mut fixture.state, b"tail"), 4);
        fixture.state.set_skip(3);
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        assert_eq!(gunzip(&fixture.sink.borrow()), b"tail\0\0\0");
        assert_eq!(fixture.closes.get(), 1, "the file was closed exactly once");
    }

    #[test]
    fn gzclose_w_releases_everything_and_refuses_a_second_call() {
        let mut fixture = plain();
        assert_eq!(gzwrite(&mut fixture.state, b"payload"), 7);
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());

        // Everything the state owned is gone, and the markers say so.
        assert_eq!(fixture.state.size(), 0);
        assert!(fixture.state.in_slice().is_empty());
        assert!(fixture.state.out_slice().is_empty());
        assert!(fixture.state.stream().engine.is_none());
        assert!(fixture.state.path().is_empty());
        assert!(!fixture.state.has_handle());
        assert_eq!(fixture.state.mode(), GZ_NONE);
        assert_eq!(fixture.state.have(), 0);

        // The divergence that makes a use-after-close deterministic rather than undefined.
        assert_eq!(
            gzclose_w(&mut fixture.state),
            ReturnCode::STREAM_ERROR.as_i32()
        );
        assert_eq!(gzwrite(&mut fixture.state, b"more"), 0);
        assert_eq!(fixture.closes.get(), 1);
    }

    #[test]
    fn a_read_stream_is_refused_by_every_write_entry_point() {
        let mut fixture = plain();
        fixture.state.set_mode(GZ_READ);
        assert_eq!(gzwrite(&mut fixture.state, b"x"), 0);
        assert_eq!(gzfwrite(&mut fixture.state, b"x", 1, 1), 0);
        assert_eq!(gzputc(&mut fixture.state, 1), -1);
        assert_eq!(gzputs(&mut fixture.state, b"x"), -1);
        assert_eq!(
            gzflush(&mut fixture.state, Z_FINISH),
            ReturnCode::STREAM_ERROR.as_i32()
        );
        assert_eq!(
            gzclose_w(&mut fixture.state),
            ReturnCode::STREAM_ERROR.as_i32()
        );
        assert!(fixture.sink.borrow().is_empty());
    }

    #[test]
    fn a_transparent_stream_rejects_an_over_reported_write_count() {
        // `write_through` (`gzwrite.c` L75-L91) consumes input by the count the file reports. An
        // over-reported count would consume bytes the file never received, so it is refused rather
        // than clamped and reported as `Z_STREAM_ERROR`: no operating-system error occurred, and the
        // read side refuses its mirror image with the same code.
        //
        // The payload is longer than the buffer on purpose: that takes `gz_write`'s large-request
        // shortcut (`gzwrite.c` L228-L240), which points the stream straight at the caller's buffer,
        // so the over-reported count would be handed back to the caller as bytes written.
        let mut fixture = fixture(16, 6, 1, None);
        let sink = Rc::new(RefCell::new(Vec::new()));
        let previous = fixture
            .state
            .set_handle(GzFileSlot::Boxed(Box::new(OverReportingFile {
                sink: Rc::clone(&sink),
                excess: 1,
            })));
        assert!(previous.is_installed());

        let payload = b"a payload comfortably longer than the sixteen-byte buffer";
        assert_eq!(gz_write(&mut fixture.state, payload), 0);
        assert_eq!(fixture.state.err(), ReturnCode::STREAM_ERROR.as_i32());
        assert!(!fixture.state.again(), "this is not a stall");
        // The honest bytes did reach the file; what was refused is the claim about how many. The
        // first chunk is `max_write_chunk()`-bounded, so at least something was offered and kept.
        assert!(!sink.borrow().is_empty());
        assert!(payload.starts_with(sink.borrow().as_slice()));
    }

    #[test]
    fn a_compressing_stream_rejects_an_over_reported_write_count() {
        // The same defence on `flush_out`'s side of `gz_comp` (`gzwrite.c` L110-L123), where the
        // over-reported count would advance the flush cursor past staged deflate output and silently
        // truncate the member.
        let mut fixture = plain();
        let sink = Rc::new(RefCell::new(Vec::new()));
        let previous = fixture
            .state
            .set_handle(GzFileSlot::Boxed(Box::new(OverReportingFile {
                sink: Rc::clone(&sink),
                excess: 4,
            })));
        assert!(previous.is_installed());

        assert_eq!(gz_write(&mut fixture.state, b"hello, hello!"), 13);
        // Nothing has reached the file yet: the buffer is nowhere near full.
        assert!(sink.borrow().is_empty());
        // The flush is what drives `flush_out`, and it must report the violation.
        assert_eq!(
            gzflush(&mut fixture.state, Z_FINISH),
            ReturnCode::STREAM_ERROR.as_i32()
        );
        assert_eq!(fixture.state.err(), ReturnCode::STREAM_ERROR.as_i32());
        assert!(!fixture.state.again());
    }

    #[test]
    fn an_honest_short_write_is_still_accepted() {
        // The complement of the two tests above: `written < offered` is legal and normal, and the
        // guard must not have turned it into an error. `accept: Some(1)` forces `gz_comp`'s
        // short-write loop to iterate once per byte.
        let mut fixture = fixture(16, 6, 1, Some(1));
        let payload = b"one byte at a time";
        assert_eq!(gz_write(&mut fixture.state, payload), payload.len());
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        assert_eq!(fixture.sink.borrow().as_slice(), payload);
    }

    #[test]
    fn a_transparent_stream_writes_the_payload_with_no_gzip_header() {
        // `"wT"` in the mode string sets `direct` (`gzlib.c` L237), and `gz_comp` then copies
        // straight through (`gzwrite.c` L75-L91).
        let mut fixture = fixture(16, 6, 1, None);
        let payload = b"transparent writing adds no container at all";
        assert_eq!(gz_write(&mut fixture.state, payload), payload.len());
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        assert_eq!(fixture.sink.borrow().as_slice(), payload);
    }

    #[test]
    fn a_transparent_stream_ignores_the_flush_mode() {
        // C's header comment for `gz_comp`: "ignore flush".
        let mut fixture = fixture(16, 6, 1, None);
        assert_eq!(gzwrite(&mut fixture.state, b"abc"), 3);
        assert_eq!(
            gzflush(&mut fixture.state, Z_FINISH),
            ReturnCode::OK.as_i32()
        );
        assert_eq!(gzwrite(&mut fixture.state, b"def"), 3);
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        assert_eq!(fixture.sink.borrow().as_slice(), b"abcdef");
    }

    #[test]
    fn a_stalled_write_reports_the_bytes_it_accepted() {
        // The non-blocking contract at `zlib.h` L1523-L1525: a non-zero short return names the
        // number of bytes consumed.
        let mut fixture = plain();
        let previous = fixture
            .state
            .set_handle(GzFileSlot::Boxed(Box::new(StalledFile)));
        assert!(previous.is_installed(), "a handle was replaced");
        // Small enough to be buffered, so nothing is written and nothing stalls yet.
        assert_eq!(gzwrite(&mut fixture.state, b"buffered"), 8);
        assert_eq!(fixture.state.err(), ReturnCode::OK.as_i32());
        // A Z_FINISH has to reach the file, so this is where the stall surfaces.
        assert_eq!(
            gzflush(&mut fixture.state, Z_FINISH),
            ReturnCode::ERRNO.as_i32()
        );
        assert!(fixture.state.again(), "the stall must be recorded");
        // `again` is the escape hatch in the entry guard, so the stream is still writable.
        assert_eq!(gzwrite(&mut fixture.state, b"more"), 4);
    }

    #[test]
    fn gz_vacate_reports_the_second_half_free_when_it_already_is() {
        // `gzwrite.c` L386-L387: nothing past `in + size` is occupied, so there is nothing to do.
        let mut fixture = fixture(64, 6, 0, None);
        gz_init(&mut fixture.state).unwrap();
        assert!(!gz_vacate(&mut fixture.state));
        assert!(fixture.sink.borrow().is_empty(), "nothing needed writing");
    }

    #[test]
    fn gz_vacate_frees_the_second_half_and_leaves_the_input_at_the_start() {
        // The post-condition `printf.rs` relies on: after a successful vacate there are `size`
        // bytes free at the end of the buffered input, and it starts at index zero.
        let mut fixture = fixture(64, 6, 0, None);
        gz_init(&mut fixture.state).unwrap();

        // Occupy past `in + size`, which is what `gzprintf` does when it formats into the second
        // half (`gzwrite.c` L450-L453).
        let payload = vec![b'q'; 100];
        let target = fixture.state.in_slice_mut();
        target[..payload.len()].copy_from_slice(&payload);
        fixture.state.stream_mut().next_in = 0;
        fixture.state.stream_mut().avail_in = 100;

        assert!(
            !gz_vacate(&mut fixture.state),
            "the second half is free again"
        );
        assert_eq!(fixture.state.stream().avail_in, 0);
        assert_eq!(fixture.state.stream().next_in, 0);
        assert_eq!(fixture.state.err(), ReturnCode::OK.as_i32());

        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        assert_eq!(gunzip(&fixture.sink.borrow()), payload.as_slice());
    }

    #[test]
    fn gz_vacate_compacts_what_a_stall_left_behind() {
        // `gzwrite.c` L393-L395: the `memmove` path, reached only when `gz_comp` came back with
        // input still pending. A transparent stream is what makes that reachable in a small test --
        // `write_through` fails on the first stalled write and touches nothing, whereas a
        // compressing stream's `fill_window` would have drained `avail_in` into the window before
        // any write was attempted. `gzprintf` works on a transparent stream too, so this is a real
        // path and not a contrivance.
        let mut fixture = fixture(64, 6, 1, None);
        gz_init(&mut fixture.state).unwrap();
        let previous = fixture
            .state
            .set_handle(GzFileSlot::Boxed(Box::new(StalledFile)));
        assert!(previous.is_installed(), "a handle was replaced");

        let target = fixture.state.in_slice_mut();
        for (index, slot) in target.iter_mut().enumerate().take(100) {
            *slot = u8::try_from(index % 251).unwrap();
        }
        fixture.state.stream_mut().next_in = 10;
        fixture.state.stream_mut().avail_in = 90;

        // 90 bytes remain and `size` is 64, so the second half is still partly occupied and the
        // report is `true` -- which is exactly what tells `gzprintf` to give up and be retried.
        let still_occupied = gz_vacate(&mut fixture.state);
        assert!(
            still_occupied,
            "90 > size, so the second half is still in use"
        );
        assert_eq!(
            fixture.state.stream().next_in,
            0,
            "input moved to the start"
        );
        assert_eq!(fixture.state.stream().avail_in, 90, "nothing was written");
        assert!(fixture.state.again(), "the stall was recorded");
        assert_eq!(fixture.state.err(), ReturnCode::ERRNO.as_i32());
        let moved = fixture.state.in_slice()[0];
        assert_eq!(moved, 10, "the byte at index 10 is now at index 0");
        assert_eq!(
            fixture.state.in_slice()[89],
            99,
            "and the last one came with it"
        );
    }

    #[test]
    fn the_example_c_gzio_write_sequence_produces_the_expected_file() {
        // `test/example.c` L99-L114, with `gzprintf(file, ", %s!", "hello")` -- which lives in
        // `printf.rs` -- replaced by the eight bytes it formats. The file must read back as
        // "hello, hello!\0", all fourteen bytes of `strlen(hello) + 1`.
        let mut fixture = plain();
        assert_eq!(gzputc(&mut fixture.state, i32::from(b'h')), i32::from(b'h'));
        assert_eq!(gzputs(&mut fixture.state, b"ello"), 4);
        assert_eq!(gzputs(&mut fixture.state, b", hello!"), 8);
        // `gzseek(file, 1L, SEEK_CUR)` -- "add one zero byte".
        fixture.state.set_skip(1);
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());

        let plain = gunzip(&fixture.sink.borrow());
        assert_eq!(plain.as_slice(), b"hello, hello!\0");
        assert_eq!(plain.len(), 14);
    }

    #[test]
    fn every_level_and_the_reference_defaults_round_trip() {
        // The parameters `gz_init` passes are fixed except for `level` and `strategy`, and a wrong
        // `windowBits` or `memLevel` would show up as a stream `inflate` cannot read.
        let payload: Vec<u8> = (0..3000_u32)
            .map(|value| u8::try_from(value.wrapping_mul(2_654_435_761) >> 24).unwrap())
            .collect();
        for level in -1..=9 {
            let mut fixture = fixture(GZBUFSIZE, level, 0, None);
            assert_eq!(gz_write(&mut fixture.state, &payload), payload.len());
            assert_eq!(
                gzclose_w(&mut fixture.state),
                ReturnCode::OK.as_i32(),
                "level {level}"
            );
            let written = fixture.sink.borrow().clone();
            assert_eq!(&written[..3], &GZIP_PREFIX, "level {level}");
            assert_eq!(gunzip(&written), payload, "level {level}");
        }
    }

    #[test]
    fn an_empty_write_changes_nothing() {
        // `gzwrite.c` L192-L194 and `zlib.h` L1522-L1523: zero is returned for a zero length, and
        // it is not an error.
        let mut fixture = plain();
        assert_eq!(gzwrite(&mut fixture.state, b""), 0);
        assert_eq!(gz_write(&mut fixture.state, b""), 0);
        assert_eq!(fixture.state.err(), ReturnCode::OK.as_i32());
        assert_eq!(fixture.state.pos(), 0);
        assert_eq!(fixture.state.size(), 0, "nothing was allocated either");
    }

    #[test]
    fn closing_a_stream_that_wrote_nothing_still_produces_an_empty_member() {
        // C's `gzclose_w` runs `gz_comp(state, Z_FINISH)` unconditionally (L685), so an opened and
        // immediately closed write stream leaves a valid, empty gzip member behind -- which is what
        // `gzip` itself produces for empty input.
        let mut fixture = plain();
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        let written = fixture.sink.borrow().clone();
        assert_eq!(&written[..3], &GZIP_PREFIX);
        assert!(gunzip(&written).is_empty());
    }

    #[test]
    fn the_uncompressed_position_tracks_every_writer() {
        // `x.pos` is the position in the *uncompressed* stream, advanced by `gz_write`, `gz_zero`
        // and `gzputc` (`gzwrite.c` L176, L219, L243 and L337).
        let mut fixture = fixture(16, 6, 0, None);
        assert_eq!(gzputc(&mut fixture.state, i32::from(b'a')), i32::from(b'a'));
        assert_eq!(fixture.state.pos(), 1);
        assert_eq!(gzputs(&mut fixture.state, b"bcd"), 3);
        assert_eq!(fixture.state.pos(), 4);
        // Longer than `size`, so this takes the direct path and accounts for itself there.
        assert_eq!(gz_write(&mut fixture.state, &[b'e'; 40]), 40);
        assert_eq!(fixture.state.pos(), 44);
        fixture.state.set_skip(6);
        gz_zero(&mut fixture.state).unwrap();
        assert_eq!(fixture.state.pos(), 50);
        assert_eq!(gzclose_w(&mut fixture.state), ReturnCode::OK.as_i32());
        assert_eq!(gunzip(&fixture.sink.borrow()).len(), 50);
    }

    // Two guards cannot be exercised from here, and the reason is worth recording rather than
    // leaving as an apparent gap:
    //
    // * `gzwrite`'s `(int)len < 0` rejection (`gzwrite.c` L270) and `gzputs`'s
    //   `(int)len < 0 || (unsigned)len != len` rejection (L366) both require a slice longer than
    //   `i32::MAX`. Building one means allocating two gibibytes, which no test suite should do.
    //   Both tests are `i32::try_from(len).is_err()`, which is exactly C's predicate for an
    //   `unsigned` argument, and `crates/libz-rs-sys` is where a length that large can actually
    //   arrive.
    // * The `Ok(0)` arms of `flush_out` and `write_through` require a `GzHandle` that reports no
    //   progress on a non-empty buffer, which the trait documents as a contract violation. They
    //   exist so that a broken implementation cannot make this layer spin, not to be reachable.
    /// F14: a write count larger than the chunk offered is refused, not clamped.
    ///
    /// Clamping was the previous behaviour: `flush_out` did `.min(state.strm.next_out)` and the
    /// transparent path simply trusted the number. Either way the stream's accounting ended up wrong
    /// and the broken handle went unreported.
    #[test]
    fn a_handle_that_over_reports_a_write_is_refused() {
        let mut fixture = plain();
        let previous = fixture
            .state
            .set_handle(GzFileSlot::Boxed(Box::new(OverReportingFile {
                sink: Rc::new(RefCell::new(Vec::new())),
                excess: 1,
            })));
        assert!(previous.is_installed(), "a handle was replaced");

        // Buffered, so nothing reaches the file yet and nothing can go wrong yet.
        assert_eq!(gzwrite(&mut fixture.state, b"buffered"), 8);
        assert_eq!(fixture.state.err(), ReturnCode::OK.as_i32());

        // A Z_FINISH has to reach the file, so this is where the over-report surfaces.
        assert_eq!(
            gzflush(&mut fixture.state, Z_FINISH),
            ReturnCode::STREAM_ERROR.as_i32(),
            "an over-reporting handle is a stream error, not an errno"
        );
        assert_eq!(
            fixture.state.err(),
            ReturnCode::STREAM_ERROR.as_i32(),
            "and the code is latched for gzerror"
        );
    }

    /// F14 on the transparent path, which has its own write call and its own cursors.
    #[test]
    fn a_transparent_stream_refuses_an_over_reported_write() {
        let mut fixture = fixture(16, 6, 1, None);
        let previous = fixture
            .state
            .set_handle(GzFileSlot::Boxed(Box::new(OverReportingFile {
                sink: Rc::new(RefCell::new(Vec::new())),
                excess: 4,
            })));
        assert!(previous.is_installed(), "a handle was replaced");

        // Longer than the 16-byte buffer, so `gz_write` goes straight to the file rather than
        // staging the bytes -- which is the only way to reach the handle from here, since a payload
        // that fits is simply buffered and reported as accepted, exactly as C reports it.
        let payload = b"a payload that does not fit in the sixteen byte buffer";
        assert_eq!(
            gz_write(&mut fixture.state, payload),
            0,
            "no bytes may be reported as written"
        );
        assert_eq!(fixture.state.err(), ReturnCode::STREAM_ERROR.as_i32());
        assert_eq!(
            fixture.state.stream().avail_in,
            0,
            "and the input cursor must not have advanced past what was staged"
        );
    }

    /// F14 boundary: reporting exactly the chunk offered is honest and is accepted.
    #[test]
    fn a_handle_that_reports_exactly_the_chunk_is_accepted() {
        let mut fixture = fixture(16, 6, 1, None);
        let previous = fixture
            .state
            .set_handle(GzFileSlot::Boxed(Box::new(OverReportingFile {
                sink: Rc::new(RefCell::new(Vec::new())),
                excess: 0,
            })));
        assert!(previous.is_installed(), "a handle was replaced");

        let payload = b"transparent writing adds no container at all";
        assert_eq!(gz_write(&mut fixture.state, payload), payload.len());
        assert_eq!(fixture.state.err(), ReturnCode::OK.as_i32());
    }
}
