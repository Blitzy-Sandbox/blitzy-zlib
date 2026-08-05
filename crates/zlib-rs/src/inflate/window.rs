//! The sliding-window update: the safe port of `updatewindow`
//! (`inflate.c` L252-L296).
//!
//! # What the reference says
//!
//! The design note above the C function (`inflate.c` L238-L251) is the clearest
//! statement of when this operation runs and why the window is shaped the way it
//! is, so it is reproduced here rather than paraphrased:
//!
//! > Update the window with the last `wsize` (normally 32K) bytes written before
//! > returning. If window does not exist yet, create it. This is only called when
//! > a window is already in use, or when output has been written during this
//! > `inflate` call, but the end of the deflate stream has not been reached yet.
//! > It is also called to create a window for dictionary data when a dictionary is
//! > loaded.
//! >
//! > Providing output buffers larger than 32K to `inflate()` should provide a
//! > speed advantage, since only the last 32K of output is copied to the sliding
//! > window upon return from `inflate()`, and since all distances after the first
//! > 32K of output will fall in the output data, making match copies simpler and
//! > faster. The advantage may be dependent on the size of the processor's data
//! > caches.
//!
//! # Four behaviours, none of them optional
//!
//! `update_window` is short, and every part of it is load-bearing. The C body
//! decomposes into exactly four steps, and this module keeps them in the same
//! order so the two files stay diffable:
//!
//! | Step | `inflate.c` | Behaviour |
//! |---|---|---|
//! | 1 | L256-L263 | **Lazy allocation.** `ZALLOC(strm, 1U << wbits, 1)` on first use; `return 1` if the allocator declines |
//! | 2 | L266-L271 | **Deferred initialisation.** `wsize == 0` means "window allocated but not in use": set `wsize = 1 << wbits`, `wnext = 0`, `whave = 0` |
//! | 3 | L274-L278 | **Whole-window replacement** when `copy >= wsize`: keep only the last `wsize` bytes, `wnext = 0`, `whave = wsize` |
//! | 4 | L279-L294 | **Two-part circular copy** otherwise: fill to the end of the buffer, then wrap what is left to the front |
//!
//! Steps 1 and 2 are independent of each other, which is easy to miss and easy to
//! get wrong. `inflateReset` zeroes `wsize`, `whave` and `wnext` but does **not**
//! free the window (`inflate.c` L130-L132); only `inflateReset2` releases it, and
//! only when `wbits` actually changes (L162-L165). A reset stream therefore
//! re-enters step 2 with a live buffer, and must not reallocate. Getting that
//! wrong produces either a leak or a spurious second allocation, both of which
//! `test/infcover.c`'s `mem_done` (its L200-L234) reports.
//!
//! # C's `end` pointer becomes the tail of a slice
//!
//! The C signature takes a pointer *past the end* of the region to copy from and
//! walks backwards from it:
//!
//! ```c
//! local int updatewindow(z_streamp strm, const Bytef *end, unsigned copy);
//! ```
//!
//! Pointer subtraction on a caller-supplied pointer is not expressible in safe
//! Rust -- and would not be desirable if it were -- so the region arrives as a
//! slice whose **tail** is the interesting part:
//!
//! | C expression | This module |
//! |---|---|
//! | `end` | one past the last byte of `written` -- the far end of the region |
//! | `end - copy` | `written[written.len() - copy ..]`, i.e. the last `copy` bytes |
//! | `end - wsize` | `written[written.len() - wsize ..]`, i.e. the last `wsize` bytes |
//! | `end - copy` recomputed after `copy -= dist` (L285) | the tail of that same region, `source[dist..]` |
//!
//! The last row is worth spelling out: because both regions end at `end`, C's
//! recomputed `end - copy` after `copy -= dist` addresses exactly `source + dist`.
//! The two formulations are the same bytes, and this module uses the second
//! because it needs no arithmetic at all.
//!
//! The parameter is a plain `&[u8]` rather than anything tied to the decoder's
//! output buffer, and that is a requirement rather than a convenience:
//! `inflateSetDictionary` calls straight into this function with the *caller's
//! dictionary* (`inflate.c` L1209), deliberately, so that a dictionary amends
//! whatever window content already exists -- the comment at L1207-L1208 says as
//! much.
//!
//! # Every length is checked, and nothing panics
//!
//! `copy` is derived from output the decoder produced from attacker-controlled
//! input, so the C code's implicit precondition -- that at least `copy` bytes
//! precede `end` -- is verified here instead of assumed. Reading past the front of
//! the region would be undefined behaviour in C; in Rust it would be a panic,
//! which for a library whose exported entry points are `extern "C"` means aborting
//! the caller's process. Neither is acceptable, so a region that is too short is
//! reported as [`ReturnCode::STREAM_ERROR`] and nothing is touched.
//!
//! Consequently the library code here contains no `[]` indexing, no `split_at`,
//! and no unchecked `copy_from_slice`: every access goes through `get`/`get_mut`
//! and every copy is preceded by a length equality check. (The test module opts
//! back into indexing deliberately, which is what the `#[allow]` on it records: a
//! test *should* fail loudly on an out-of-range expectation.) The crate root asserts
//! `#![forbid(unsafe_code)]`, so the bounds checks cannot be circumvented, and the
//! workspace lint table denies `clippy::indexing_slicing`,
//! `clippy::panic_in_result_fn` and the `unwrap`/`expect` family, so the absence
//! of panicking operations is compiler-enforced rather than merely intended.
//!
//! # `whave` is a security boundary, not an optimisation
//!
//! `whave` (how many window bytes are valid) and `wnext` (where the next byte
//! goes) are the *only* things that bound valid window content. The window itself
//! arrives uninitialised in C -- `zcalloc` selects `malloc` over `calloc` whenever
//! `sizeof(uInt) > 2` (`zutil.c` L299-L303), which is every target this port
//! supports -- and `test/infcover.c` fills every block it hands out with `0xa5`
//! (its L87) precisely so that code which assumes zeros produces wrong answers
//! instead of passing by luck.
//!
//! So the accounting here is not bookkeeping; it decides which streams are
//! accepted. The `MATCH` state rejects a distance larger than `whave` as "invalid
//! distance too far back" (`inflate.c` L1025-L1031), and the repository's HEAD
//! commit `09a1572` -- "Fix `inflateBack()` bug that would fail to detect a too far
//! back" -- exists because two lines in `infback.c` widened `whave` beyond what had
//! actually been written:
//!
//! ```c
//! if (state->whave < state->wsize)
//!     state->whave = state->wsize - left;   /* deleted by 09a1572 */
//! ```
//!
//! Its commit message records the consequence: the bug "would pass off an invalid
//! deflate stream as good, and copy uninitialized memory contents to the output".
//! Widening `whave` anywhere -- including here -- reintroduces that class of
//! defect. That is why the `if (whave < wsize) whave += dist` guard at
//! `inflate.c` L292 is reproduced exactly and must never be "simplified".
//!
//! Safe Rust cannot hand out uninitialised memory, so a window allocated through
//! this crate's allocator does arrive filled. That is strictly safer than C and
//! unobservable to callers, and it must never become a correctness dependency: if
//! the `whave` gating here were looser than C's, this library would accept streams
//! the reference rejects and bidirectional interoperability (AAP §0.8.1 directive
//! 4) would be broken in the worst possible way -- silently, and only for
//! malformed input.
//!
//! # Owned windows only
//!
//! `inflate()` owns a lazily allocated window; `inflateBack()` borrows a
//! caller-supplied `2**windowBits` buffer and maintains `whave` itself
//! (`infback.c` L25-L64, L61, L156 and L217). `inflateBack()` never calls
//! `updatewindow`, so a borrowed window reaching `update_window` is a
//! programming error rather than a malformed-input condition. It is still refused
//! rather than asserted on -- [`ReturnCode::STREAM_ERROR`], before anything is
//! read or written -- because a library must not abort its caller's process to
//! report a bug in itself.
//!
//! # What deliberately is not here
//!
//! Only the window *mechanics* of `updatewindow` live in this module. The
//! surrounding pieces have owners already, and duplicating them here would create
//! two places to fix one bug:
//!
//! * the allocation and cursor primitives -- [`InflateState::ensure_window`],
//!   `InflateState::discard_window`, `InflateState::valid_window_byte` and the
//!   `InflateWindow` accessors -- belong to `crates/zlib-rs/src/inflate/state.rs`;
//! * the public entry points that call in here -- `inflate()`'s `inf_leave`
//!   (`inflate.c` L1133-L1138) and `inflateSetDictionary` (L1207-L1213), together
//!   with `inflateGetDictionary`'s two-part read-back (L1176-L1183) -- belong to
//!   `crates/zlib-rs/src/inflate/mod.rs`.
//!
//! # Visibility
//!
//! Nothing here is exported to C. `updatewindow` is a `local` (file-static)
//! function in the reference: it appears in no header and in no block of
//! `zlib.map`, so its port is `pub(crate)`, never `pub`, and carries no
//! `#[no_mangle]` and no `extern "C"`.

use crate::allocate::Allocator;
use crate::error::ReturnCode;
use crate::inflate::state::InflateState;

/// Copies the last `copy` bytes of `written` into the sliding window, creating
/// the window if it does not exist yet.
///
/// This is the port of `updatewindow` (`inflate.c` L252-L296); see the [module
/// documentation](self) for the reference's own description of when it runs, for
/// the correspondence between C's `end` pointer and the tail of `written`, and for
/// why the `whave` arithmetic may not be rearranged.
///
/// # Parameters
///
/// * `state` -- the stream state whose window, `wsize`, `whave` and `wnext` are
///   updated. Only an absent or state-owned window is accepted; see the errors
///   below.
/// * `written` -- a region whose **last `copy` bytes** are the ones to record.
///   `written` may be longer than `copy`; the leading part is ignored, exactly as
///   C ignores everything before `end - copy`. The two call sites pass the output
///   buffer written during this `inflate()` call (`inflate.c` L1135) and the
///   caller's whole dictionary (L1209) respectively.
/// * `copy` -- how many bytes at the end of `written` to record. C declares this
///   `unsigned`; here it is a `usize` because it is a byte count into a slice and
///   because every comparison it takes part in is then performed in `usize`, so no
///   conversion can truncate it.
///
/// # Errors
///
/// The C function reports failure as a bare `return 1`, which both call sites map
/// to `state->mode = MEM; return Z_MEM_ERROR` (`inflate.c` L1136-L1138 and
/// L1210-L1213). Callers of this port must treat **any** `Err` the same way --
/// `InflateState::latch_memory_error` performs exactly that step -- and the
/// distinction below exists for diagnosis, not for control flow:
///
/// * [`ReturnCode::MEM_ERROR`] -- the lazy window allocation was declined. This is
///   C's `return 1` and the only failure a well-formed caller can actually
///   provoke; `test/infcover.c` L421-L423 forces it with `mem_limit(&strm, 1)` and
///   asserts `Z_MEM_ERROR` twice in a row, because `Mode::Mem` is sticky.
/// * [`ReturnCode::STREAM_ERROR`] -- a precondition C cannot express was violated:
///   `written` holds fewer than `copy` bytes, the window is borrowed by
///   `inflateBack()`, or the stored cursors do not describe the allocated window.
///   Each is a defect in the calling code, and each is rejected **before** any
///   state is modified so that the previous state survives intact.
pub(crate) fn update_window<'a, A>(
    state: &mut InflateState<'a, A>,
    written: &[u8],
    copy: usize,
) -> Result<(), ReturnCode>
where
    A: Allocator<'a> + Copy,
{
    // Refuse a borrowed window up front. `inflateBack()` installs the caller's
    // buffer and maintains `whave` itself (`infback.c` L25-L64), and never calls
    // `updatewindow`, so arriving here with one is a bug in the caller rather than
    // anything a stream can cause.
    if state.window.is_borrowed() {
        return Err(ReturnCode::STREAM_ERROR);
    }

    // C's `end - copy` requires at least `copy` bytes to precede `end`; reading
    // past the front of the region would be undefined behaviour there and a panic
    // here. Checked first, so that a rejected call has no side effects at all --
    // not even the allocation below. C has no equivalent step, so its position is
    // a free choice, and "validate before mutate" is the useful one.
    let source = split_tail(written, copy)?;

    // Steps 1 and 2 of the C body (`inflate.c` L256-L271): allocate `1 << wbits`
    // bytes through the injected allocator if the window is absent, and initialise
    // the cursors if the window is not in use yet. Both are the state's own
    // primitives, so a reset stream reuses its live buffer instead of reallocating.
    let window_len = state.ensure_window()?.len();

    // The cursors, in C's `unsigned` domain, widened once so that every comparison
    // and every slice bound below is computed in `usize`.
    let wsize = usize::try_from(state.wsize).map_err(|_| ReturnCode::STREAM_ERROR)?;
    let wnext = usize::try_from(state.wnext).map_err(|_| ReturnCode::STREAM_ERROR)?;
    // `wsize` is `1 << wbits` and `wnext` is a write index within it: the reference
    // resets `wnext` to zero the moment it reaches `wsize` (L291), so `wnext > wsize`
    // describes no state `inflate.c` can produce. A cursor pair that does not fit
    // the allocated block is refused rather than clamped, because clamping would
    // silently relocate window history.
    if wsize == 0 || wsize > window_len || wnext > wsize {
        return Err(ReturnCode::STREAM_ERROR);
    }

    if copy >= wsize {
        // Step 3, `inflate.c` L274-L278. Everything older than the last `wsize`
        // bytes is unreachable by any legal distance, so the window is simply
        // replaced by that tail and the circular layout collapses to a linear one.
        let tail = split_tail(written, wsize)?;
        write_window(state, 0, tail)?;
        state.wnext = 0;
        state.whave = state.wsize;
    } else {
        // Step 4, `inflate.c` L279-L294. `dist` is the room left before the end of
        // the buffer, clamped to what there is to write:
        //
        //     dist = state->wsize - state->wnext;
        //     if (dist > copy) dist = copy;
        //
        // The subtraction cannot underflow: `wnext <= wsize` was established above.
        let dist = wsize.saturating_sub(wnext).min(copy);
        let first = source.get(..dist).ok_or(ReturnCode::STREAM_ERROR)?;
        let rest = source.get(dist..).ok_or(ReturnCode::STREAM_ERROR)?;
        write_window(state, wnext, first)?;

        // `rest` is C's `copy` after `copy -= dist` (L284), so `rest.is_empty()`
        // is C's `if (copy)` inverted. The arms are ordered the other way round
        // from the C source purely to keep the condition positive; each is
        // labelled with the lines it reproduces.
        if rest.is_empty() {
            // L289-L293: the write fitted before the end of the buffer.
            let mut next = wnext.saturating_add(dist);
            if next == wsize {
                next = 0;
            }
            state.wnext = u32::try_from(next).map_err(|_| ReturnCode::STREAM_ERROR)?;
            // L292, reproduced exactly. The guard saturates `whave` at `wsize` and
            // is what keeps it a truthful bound on valid content -- see the module
            // documentation on commit `09a1572`. `saturating_add` cannot actually
            // saturate here: while `whave < wsize` the window has only ever been
            // filled linearly, so `whave == wnext`, and `dist <= wsize - wnext`.
            if state.whave < state.wsize {
                let advance = u32::try_from(dist).map_err(|_| ReturnCode::STREAM_ERROR)?;
                state.whave = state.whave.saturating_add(advance);
            }
        } else {
            // L285-L288: the write wrapped, so the remainder lands at the front and
            // the whole buffer now holds valid history.
            write_window(state, 0, rest)?;
            state.wnext = u32::try_from(rest.len()).map_err(|_| ReturnCode::STREAM_ERROR)?;
            state.whave = state.wsize;
        }
    }

    Ok(())
}

/// Borrows the last `len` bytes of `region`, or fails if it is shorter than that.
///
/// This is the safe form of C's backwards pointer arithmetic from `end`: `end -
/// copy` becomes `split_tail(written, copy)` and `end - wsize` becomes
/// `split_tail(written, wsize)`. The returned slice is exactly `len` bytes long,
/// which is what makes the copies below length-exact by construction.
///
/// # Errors
///
/// [`ReturnCode::STREAM_ERROR`] if `region` holds fewer than `len` bytes. C would
/// read in front of the buffer instead; see the module documentation on why that
/// precondition is verified rather than assumed.
fn split_tail(region: &[u8], len: usize) -> Result<&[u8], ReturnCode> {
    let start = region
        .len()
        .checked_sub(len)
        .ok_or(ReturnCode::STREAM_ERROR)?;
    region.get(start..).ok_or(ReturnCode::STREAM_ERROR)
}

/// Writes `source` into the window at `at`, the checked replacement for the three
/// `zmemcpy` calls at `inflate.c` L275, L282 and L285.
///
/// # Errors
///
/// [`ReturnCode::STREAM_ERROR`] if the window is absent, or if `at .. at +
/// source.len()` is not entirely inside it. Both are unreachable from
/// [`update_window`], which establishes the bounds before calling; the checks are
/// what make that structural instead of argued.
fn write_window<'a, A>(
    state: &mut InflateState<'a, A>,
    at: usize,
    source: &[u8],
) -> Result<(), ReturnCode>
where
    A: Allocator<'a>,
{
    let window = state
        .window
        .as_mut_slice()
        .ok_or(ReturnCode::STREAM_ERROR)?;
    let end = at
        .checked_add(source.len())
        .ok_or(ReturnCode::STREAM_ERROR)?;
    let destination = window.get_mut(at..end).ok_or(ReturnCode::STREAM_ERROR)?;
    // `get_mut(at..end)` yields exactly `end - at == source.len()` bytes, so
    // `copy_from_slice`'s length precondition already holds. Asserting it as a
    // returned error rather than trusting the arithmetic is what keeps the sole
    // panicking operation in this module unreachable.
    if destination.len() != source.len() {
        return Err(ReturnCode::STREAM_ERROR);
    }
    destination.copy_from_slice(source);
    Ok(())
}

// -----------------------------------------------------------------------------
//  Tests
// -----------------------------------------------------------------------------

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::panic,
    clippy::too_many_lines,
    clippy::unwrap_used
)]
mod tests {
    use super::{split_tail, update_window, write_window};
    use crate::allocate::{Allocator, AllocatorId, Buffer, GlobalAllocator, Opaque, SENTINEL_FILL};
    // `InflateConfig` arrives through the dependency's own contract rather than as
    // a new dependency of this module: `InflateState::new` takes one, so there is
    // no way to build an ordinary inflate state without naming it. Nothing outside
    // this test module refers to it.
    use crate::config::InflateConfig;
    use crate::error::ReturnCode;
    use crate::inflate::state::InflateState;
    use core::cell::Cell;
    use core::mem::size_of;

    /// Window size of every state built here: `1 << 8`, from `windowBits` of `-8`.
    ///
    /// The same 256-byte window `test/infcover.c` L415 uses, which keeps the test
    /// data small enough to reason about byte by byte while still exercising both
    /// halves of the circular copy.
    const WSIZE: usize = 256;

    /// An allocator that can be made to decline, in the shape of
    /// `test/infcover.c`'s tracking allocator.
    ///
    /// `mem_alloc` refuses a request when `zone->limit && zone->total + len >
    /// zone->limit` (its L79-L80) and fills what it does hand out with `0xa5`
    /// (L87); both behaviours are reproduced so the allocation-failure path and the
    /// "nothing depends on zeroed memory" property can be tested here rather than
    /// only in the relinked C driver.
    struct Tracker {
        /// Byte budget, or zero for "no limit", mirroring `mem_limit`.
        limit: Cell<usize>,
        /// Bytes currently outstanding.
        total: Cell<usize>,
        /// Successful allocations, so a reused window can be told from a new one.
        allocations: Cell<usize>,
        /// Releases, so leaks and double frees are visible.
        frees: Cell<usize>,
    }

    impl Tracker {
        fn new() -> Self {
            Self {
                limit: Cell::new(0),
                total: Cell::new(0),
                allocations: Cell::new(0),
                frees: Cell::new(0),
            }
        }

        fn set_limit(&self, limit: usize) {
            self.limit.set(limit);
        }

        fn allocations(&self) -> usize {
            self.allocations.get()
        }

        fn frees(&self) -> usize {
            self.frees.get()
        }

        fn refuses(&self, len: usize) -> bool {
            let limit = self.limit.get();
            limit != 0 && self.total.get().saturating_add(len) > limit
        }
    }

    impl<'a> Allocator<'a> for Tracker {
        fn id(&self) -> AllocatorId {
            // Storage comes from `Buffer::try_global`, so this really is the global
            // allocator wearing a counter -- the same arrangement `state.rs`'s own
            // tests use.
            AllocatorId::GLOBAL
        }

        fn opaque(&self) -> Opaque {
            Opaque::NULL
        }

        fn allocate_bytes(&self, items: usize, size: usize) -> Option<Buffer<'a, u8>> {
            let len = items.checked_mul(size)?;
            if self.refuses(len) {
                return None;
            }
            let buffer = Buffer::try_global(len, SENTINEL_FILL)?;
            self.total.set(self.total.get().saturating_add(len));
            self.allocations
                .set(self.allocations.get().saturating_add(1));
            Some(buffer)
        }

        fn allocate_u16s(&self, items: usize) -> Option<Buffer<'a, u16>> {
            let len = items.checked_mul(size_of::<u16>())?;
            if self.refuses(len) {
                return None;
            }
            let fill = u16::from_ne_bytes([SENTINEL_FILL, SENTINEL_FILL]);
            let buffer = Buffer::try_global(items, fill)?;
            self.total.set(self.total.get().saturating_add(len));
            self.allocations
                .set(self.allocations.get().saturating_add(1));
            Some(buffer)
        }

        fn deallocate_bytes(&self, buffer: Buffer<'a, u8>) {
            self.total
                .set(self.total.get().saturating_sub(buffer.len()));
            self.frees.set(self.frees.get().saturating_add(1));
            GlobalAllocator.deallocate_bytes(buffer);
        }

        fn deallocate_u16s(&self, buffer: Buffer<'a, u16>) {
            let len = buffer.len().saturating_mul(size_of::<u16>());
            self.total.set(self.total.get().saturating_sub(len));
            self.frees.set(self.frees.get().saturating_add(1));
            GlobalAllocator.deallocate_u16s(buffer);
        }
    }

    /// A raw-mode state with a 256-byte window and no window allocated yet.
    fn new_state(tracker: &Tracker) -> InflateState<'_, &Tracker> {
        let state = InflateState::new(InflateConfig::new(-8), tracker).unwrap();
        assert!(state.window.is_absent());
        assert_eq!(state.wsize, 0);
        state
    }

    /// The byte this port must find at absolute output position `index`.
    ///
    /// The period is 253 rather than 256 so that a byte which is one whole window
    /// out of date does *not* coincidentally carry the value expected at its slot;
    /// a stale window byte is therefore detected instead of hidden.
    fn pattern(index: usize) -> u8 {
        u8::try_from(index % 253).unwrap()
    }

    /// Appends `len` pattern bytes to `history` and returns the new total.
    ///
    /// The slice `&history[..total]` is then the exact analogue of what `inf_leave`
    /// passes as `end`: a region whose far end is the current output cursor.
    fn extend(history: &mut [u8], total: usize, len: usize) -> usize {
        for offset in 0..len {
            history[total + offset] = pattern(total + offset);
        }
        total + len
    }

    fn cursors(state: &InflateState<'_, &Tracker>) -> (usize, usize, usize) {
        (
            usize::try_from(state.wsize).unwrap(),
            usize::try_from(state.whave).unwrap(),
            usize::try_from(state.wnext).unwrap(),
        )
    }

    #[test]
    fn lazy_allocation_failure_is_the_c_return_1_indication() {
        let tracker = Tracker::new();
        let mut state = new_state(&tracker);
        let written = [1_u8, 2, 3, 4];

        // `mem_limit(&strm, 1)` -- `test/infcover.c` L421. The 256-byte window
        // cannot be granted, so C's `return 1` (`inflate.c` L262) is reported.
        tracker.set_limit(1);
        assert_eq!(
            update_window(&mut state, &written, 4),
            Err(ReturnCode::MEM_ERROR)
        );
        assert_eq!(tracker.allocations(), 0);
        assert!(state.window.is_absent());
        assert_eq!(cursors(&state), (0, 0, 0));

        // The caller's half of the contract: `state->mode = MEM; return
        // Z_MEM_ERROR` (`inflate.c` L1136-L1138), which is non-recoverable, so
        // `test/infcover.c` L422-L423 sees `Z_MEM_ERROR` twice in a row.
        assert_eq!(state.latch_memory_error(), ReturnCode::MEM_ERROR);
        assert_eq!(state.latched_error(), Some(ReturnCode::MEM_ERROR));
        assert_eq!(state.latched_error(), Some(ReturnCode::MEM_ERROR));

        // `mem_limit(&strm, 0)` -- L424. The very next call succeeds, so the
        // failure left nothing half-built behind.
        tracker.set_limit(0);
        assert_eq!(update_window(&mut state, &written, 4), Ok(()));
        assert_eq!(tracker.allocations(), 1);
        assert_eq!(cursors(&state), (WSIZE, 4, 4));
    }

    #[test]
    fn deferred_initialisation_survives_a_reset_without_reallocating() {
        let tracker = Tracker::new();
        let mut state = new_state(&tracker);
        let written = [11_u8, 12, 13, 14];

        assert_eq!(update_window(&mut state, &written, 4), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, 4, 4));
        assert_eq!(tracker.allocations(), 1);
        assert_eq!(
            state.window.as_slice().unwrap().get(..4),
            Some(&written[..])
        );

        // `inflateReset` zeroes the three cursors and keeps the window
        // (`inflate.c` L130-L132), so the next call re-enters the deferred
        // initialisation branch with a live buffer.
        let _ = state.reset();
        assert_eq!(cursors(&state), (0, 0, 0));
        assert!(state.window.is_owned());

        assert_eq!(update_window(&mut state, &written, 2), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, 2, 2));
        assert_eq!(tracker.allocations(), 1, "the window must be reused");
        assert_eq!(tracker.frees(), 0);

        let window = state.window.as_slice().unwrap();
        assert_eq!(window.get(..2), Some(&[13_u8, 14][..]));
        // Byte 2 still holds the stale 13 from the first call, and `whave` of 2 is
        // the only thing that makes it unreadable. Nothing was cleared.
        assert_eq!(window.get(2), Some(&13));
        assert_eq!(state.valid_window_byte(1), Some(14));
        assert_eq!(state.valid_window_byte(2), None);
    }

    #[test]
    fn whole_window_path_keeps_only_the_last_wsize_bytes() {
        let mut history = [0_u8; 512];
        let total = extend(&mut history, 0, 300);

        // `copy > wsize`: the reference copies `end - wsize .. end`
        // (`inflate.c` L275), discarding the first 44 bytes outright.
        let tracker = Tracker::new();
        let mut state = new_state(&tracker);
        assert_eq!(update_window(&mut state, &history[..total], 300), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, WSIZE, 0));
        assert_eq!(
            state.window.as_slice().unwrap(),
            &history[total - WSIZE..total]
        );

        // The `>=` boundary itself: `copy == wsize` takes the same branch.
        let boundary_tracker = Tracker::new();
        let mut boundary = new_state(&boundary_tracker);
        assert_eq!(
            update_window(&mut boundary, &history[..total], WSIZE),
            Ok(())
        );
        assert_eq!(cursors(&boundary), (WSIZE, WSIZE, 0));
        assert_eq!(
            boundary.window.as_slice().unwrap(),
            &history[total - WSIZE..total]
        );

        // One byte less is *not* the whole-window branch: it is the two-part copy
        // with `dist == copy`, which writes at `wnext == 0` and leaves `wnext` just
        // short of the end.
        let below_tracker = Tracker::new();
        let mut below = new_state(&below_tracker);
        assert_eq!(
            update_window(&mut below, &history[..total], WSIZE - 1),
            Ok(())
        );
        assert_eq!(cursors(&below), (WSIZE, WSIZE - 1, WSIZE - 1));
        assert_eq!(
            below.window.as_slice().unwrap().get(..WSIZE - 1),
            Some(&history[total - (WSIZE - 1)..total])
        );
        assert_eq!(
            below.window.as_slice().unwrap().get(WSIZE - 1),
            Some(&SENTINEL_FILL)
        );
    }

    #[test]
    fn no_wrap_path_fills_the_prefix_and_touches_nothing_else() {
        let tracker = Tracker::new();
        let mut state = new_state(&tracker);
        // Values chosen below `0xa5` so the allocator's fill stays distinguishable.
        let written = [1_u8, 2, 3, 4, 5, 6, 7, 8, 9, 10];

        // Only the last six bytes are recorded, exactly as C ignores everything
        // before `end - copy`.
        assert_eq!(update_window(&mut state, &written, 6), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, 6, 6));

        let window = state.window.as_slice().unwrap();
        assert_eq!(window.get(..6), Some(&written[4..]));
        assert!(
            window.get(6..).unwrap().iter().all(|&b| b == SENTINEL_FILL),
            "no byte past `wnext` may be written"
        );
        assert_eq!(state.valid_window_byte(5), Some(10));
        assert_eq!(state.valid_window_byte(6), None);
    }

    #[test]
    fn wrap_path_splits_at_the_window_end() {
        let tracker = Tracker::new();
        let mut state = new_state(&tracker);
        let mut history = [0_u8; 512];

        // Drive `wnext` to 250 through the ordinary path, so the state under test is
        // reached the way `inflate()` reaches it rather than by poking fields.
        let total = extend(&mut history, 0, 250);
        assert_eq!(update_window(&mut state, &history[..total], 250), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, 250, 250));

        // Now a copy that straddles the end: `dist = wsize - wnext = 6`, clamped
        // against `copy = 10`, leaves 4 bytes for the front (`inflate.c` L280-L288).
        let total = extend(&mut history, total, 10);
        assert_eq!(update_window(&mut state, &history[..total], 10), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, WSIZE, 4));

        let window = state.window.as_slice().unwrap();
        let source = &history[total - 10..total];
        assert_eq!(window.get(250..), Some(&source[..6]), "first part");
        assert_eq!(window.get(..4), Some(&source[6..]), "wrapped remainder");
        // Bytes 4..250 are untouched history from the first call.
        assert_eq!(window.get(4..250), Some(&history[4..250]));
    }

    #[test]
    fn wnext_resets_to_zero_without_taking_the_remainder_branch() {
        let tracker = Tracker::new();
        let mut state = new_state(&tracker);
        let mut history = [0_u8; 512];

        let total = extend(&mut history, 0, 250);
        assert_eq!(update_window(&mut state, &history[..total], 250), Ok(()));

        // `dist` is chosen so that `wnext + dist == wsize` exactly: C takes the
        // `else` arm (nothing wrapped), then `if (state->wnext == state->wsize)
        // state->wnext = 0` at L291 folds the cursor back to the front. `whave`
        // reaches `wsize` through the `+= dist` guard at L292, not by assignment.
        let total = extend(&mut history, total, 6);
        assert_eq!(update_window(&mut state, &history[..total], 6), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, WSIZE, 0));

        let window = state.window.as_slice().unwrap();
        assert_eq!(window.get(250..), Some(&history[250..256]));
        assert_eq!(window, &history[..WSIZE]);
    }

    #[test]
    fn whave_saturates_at_wsize_and_never_exceeds_it() {
        let tracker = Tracker::new();
        let mut state = new_state(&tracker);
        let mut history = [0_u8; 1024];
        let mut total = 0;

        // Twenty 40-byte calls cross the window end twice, which exercises the
        // `+= dist` accumulation, the `= wsize` assignment on wrap, and the guard
        // that stops the accumulation once the window is full.
        for _ in 0..20 {
            total = extend(&mut history, total, 40);
            assert_eq!(update_window(&mut state, &history[..total], 40), Ok(()));
            let (wsize, whave, wnext) = cursors(&state);
            assert_eq!(wsize, WSIZE);
            assert!(whave <= wsize, "`whave` must never exceed `wsize`");
            assert_eq!(whave, total.min(WSIZE));
            assert_eq!(wnext, total % WSIZE);
        }
        assert_eq!(cursors(&state).1, WSIZE);
    }

    #[test]
    fn copy_of_zero_allocates_and_initialises_but_records_nothing() {
        let tracker = Tracker::new();
        let mut state = new_state(&tracker);

        // C reaches its `else` branch with `dist == 0`: two no-op `zmemcpy` calls,
        // `wnext += 0`, `whave += 0`. The lazy allocation and the deferred
        // initialisation still happen, which is what makes this call meaningful.
        assert_eq!(update_window(&mut state, &[], 0), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, 0, 0));
        assert_eq!(tracker.allocations(), 1);
        assert!(state
            .window
            .as_slice()
            .unwrap()
            .iter()
            .all(|&b| b == SENTINEL_FILL));
        assert_eq!(state.valid_window_byte(0), None);

        // On an established window it is a pure no-op.
        let written = [21_u8, 22, 23];
        assert_eq!(update_window(&mut state, &written, 3), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, 3, 3));
        assert_eq!(update_window(&mut state, &written, 0), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, 3, 3));
        assert_eq!(update_window(&mut state, &[], 0), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, 3, 3));
        assert_eq!(tracker.allocations(), 1);
    }

    #[test]
    fn circular_layout_is_exactly_what_the_window_readers_expect() {
        // Every scenario is a sequence of `inflate()` calls, each recording that
        // call's output. The assertions below are written against the *ring-buffer
        // definition* -- most recent byte at `(wnext - 1) mod wsize`, `whave` bytes
        // valid -- and against the read arithmetic of the `MATCH` state
        // (`inflate.c` L1046-L1051), which `inffast.rs` mirrors in its
        // `wnext == 0`, wrap-around and contiguous cases. Nothing here re-derives
        // the code under test.
        const SCENARIOS: [&[usize]; 13] = [
            &[1],
            &[4],
            &[255],
            &[256],
            &[257],
            &[300],
            &[250, 6],
            &[250, 10],
            &[128, 128, 128],
            &[40, 40, 40, 40, 40, 40, 40, 40],
            &[1, 1, 1, 1, 1, 1, 1, 1, 1, 1],
            &[100, 200, 50, 300],
            &[0, 5, 0, 260, 0],
        ];

        for chunks in SCENARIOS {
            let tracker = Tracker::new();
            let mut state = new_state(&tracker);
            let mut history = [0_u8; 1024];
            let mut total = 0;
            // The ring-buffer definition of the write cursor, stated independently:
            // a call that covers the whole window leaves the most recent byte at the
            // very end of the buffer, and any shorter call advances the cursor
            // modulo the window size.
            let mut expected_wnext = 0;
            for &chunk in chunks {
                total = extend(&mut history, total, chunk);
                assert_eq!(
                    update_window(&mut state, &history[..total], chunk),
                    Ok(()),
                    "chunks {chunks:?}"
                );
                expected_wnext = if chunk >= WSIZE {
                    0
                } else {
                    (expected_wnext + chunk) % WSIZE
                };
            }

            let (wsize, whave, wnext) = cursors(&state);
            assert_eq!(wsize, WSIZE, "chunks {chunks:?}");
            assert_eq!(whave, total.min(WSIZE), "chunks {chunks:?}");
            assert_eq!(wnext, expected_wnext, "chunks {chunks:?}");
            assert_eq!(tracker.allocations(), 1, "chunks {chunks:?}");

            let window = state.window.as_slice().unwrap();
            for distance in 1..=whave {
                // `inflate.c` L1046-L1051, with `copy` renamed to `distance`:
                //     if (copy > state->wnext) from = window + (wsize - (copy - wnext));
                //     else                     from = window + (wnext - copy);
                let index = if distance > wnext {
                    wsize - (distance - wnext)
                } else {
                    wnext - distance
                };
                assert_eq!(
                    window[index],
                    history[total - distance],
                    "chunks {chunks:?}, distance {distance}"
                );
            }
            // A distance one past `whave` is what the `MATCH` state rejects as
            // "invalid distance too far back" (`inflate.c` L1025-L1031), so nothing
            // is asserted about that byte -- and with a fresh allocator fill it is
            // still the sentinel whenever the window has not been filled once.
            if whave < wsize {
                assert!(window[whave..].iter().all(|&b| b == SENTINEL_FILL));
            }
        }
    }

    #[test]
    fn whole_window_replacement_restarts_the_circular_write_at_the_front() {
        let tracker = Tracker::new();
        let mut state = new_state(&tracker);
        let mut history = [0_u8; 512];

        let total = extend(&mut history, 0, 300);
        assert_eq!(update_window(&mut state, &history[..total], 300), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, WSIZE, 0));

        // `wnext == 0` with a full window: the next call writes at the front and
        // `whave` stays put, because the L292 guard sees `whave == wsize`.
        let total = extend(&mut history, total, 10);
        assert_eq!(update_window(&mut state, &history[..total], 10), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, WSIZE, 10));

        let window = state.window.as_slice().unwrap();
        assert_eq!(window.get(..10), Some(&history[total - 10..total]));
        // Everything from 10 on is still the tail of the whole-window copy, which
        // began at absolute position 44.
        assert_eq!(window.get(10..), Some(&history[54..300]));
    }

    #[test]
    fn borrowed_windows_are_refused_and_left_untouched() {
        let tracker = Tracker::new();
        let mut caller_window = [7_u8; WSIZE];
        // `inflateBackInit_` installs the caller's buffer and manages `whave`
        // itself (`infback.c` L25-L64); it never calls `updatewindow`.
        let mut state =
            InflateState::with_borrowed_window(8, &mut caller_window, &tracker).unwrap();
        assert!(state.window.is_borrowed());

        let written = [1_u8, 2, 3, 4];
        assert_eq!(
            update_window(&mut state, &written, 4),
            Err(ReturnCode::STREAM_ERROR)
        );
        assert!(state.window.is_borrowed());
        assert_eq!(cursors(&state), (WSIZE, 0, 0));
        assert!(state.window.as_slice().unwrap().iter().all(|&b| b == 7));
        assert_eq!(tracker.allocations(), 0);
        assert_eq!(tracker.frees(), 0);
    }

    #[test]
    fn a_region_shorter_than_copy_is_refused_rather_than_panicking() {
        let tracker = Tracker::new();
        let mut state = new_state(&tracker);
        let written = [1_u8, 2, 3, 4];

        for copy in [5, 6, WSIZE, usize::MAX] {
            assert_eq!(
                update_window(&mut state, &written, copy),
                Err(ReturnCode::STREAM_ERROR),
                "copy {copy}"
            );
        }
        assert_eq!(
            update_window(&mut state, &[], 1),
            Err(ReturnCode::STREAM_ERROR)
        );

        // Rejected before anything was allocated or initialised.
        assert_eq!(tracker.allocations(), 0);
        assert!(state.window.is_absent());
        assert_eq!(cursors(&state), (0, 0, 0));

        // The empty region with `copy == 0` is legal, and the state is still usable.
        assert_eq!(update_window(&mut state, &[], 0), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, 0, 0));
        assert_eq!(update_window(&mut state, &written, 4), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, 4, 4));
    }

    #[test]
    fn inconsistent_cursors_are_refused_rather_than_relocating_history() {
        let tracker = Tracker::new();
        let mut state = new_state(&tracker);
        let written = [1_u8, 2, 3, 4];
        assert_eq!(update_window(&mut state, &written, 4), Ok(()));

        // No state `inflate.c` can produce has `wnext > wsize` (L291 folds it back
        // the moment it reaches `wsize`), so it is reported instead of clamped.
        state.wnext = state.wsize + 1;
        assert_eq!(
            update_window(&mut state, &written, 2),
            Err(ReturnCode::STREAM_ERROR)
        );

        // `wnext == wsize` is likewise unreachable, but it *is* recoverable -- C
        // would compute `dist == 0` and fold the cursor back -- so it is honoured.
        state.wnext = state.wsize;
        assert_eq!(update_window(&mut state, &written, 0), Ok(()));
        assert_eq!(cursors(&state), (WSIZE, 4, 0));

        // A window smaller than `wsize` cannot hold the history `wsize` claims.
        state.wsize = u32::try_from(WSIZE + 1).unwrap();
        assert_eq!(
            update_window(&mut state, &written, 2),
            Err(ReturnCode::STREAM_ERROR)
        );
    }

    #[test]
    fn tail_and_window_helpers_report_instead_of_panicking() {
        let region = [1_u8, 2, 3, 4];
        assert_eq!(split_tail(&region, 0), Ok(&[][..]));
        assert_eq!(split_tail(&region, 1), Ok(&region[3..]));
        assert_eq!(split_tail(&region, 4), Ok(&region[..]));
        assert_eq!(split_tail(&region, 5), Err(ReturnCode::STREAM_ERROR));
        assert_eq!(
            split_tail(&region, usize::MAX),
            Err(ReturnCode::STREAM_ERROR)
        );
        assert_eq!(split_tail(&[], 0), Ok(&[][..]));

        let tracker = Tracker::new();
        let mut state = new_state(&tracker);
        // An absent window is `window == Z_NULL`: reported, never dereferenced.
        assert_eq!(
            write_window(&mut state, 0, &region),
            Err(ReturnCode::STREAM_ERROR)
        );

        assert_eq!(update_window(&mut state, &region, 4), Ok(()));
        assert_eq!(write_window(&mut state, WSIZE - 4, &region), Ok(()));
        assert_eq!(
            write_window(&mut state, WSIZE - 3, &region),
            Err(ReturnCode::STREAM_ERROR)
        );
        assert_eq!(
            write_window(&mut state, usize::MAX, &region),
            Err(ReturnCode::STREAM_ERROR)
        );
        assert_eq!(
            state.window.as_slice().unwrap().get(WSIZE - 4..),
            Some(&region[..])
        );
    }

    #[test]
    fn the_window_is_released_to_the_allocator_that_produced_it() {
        let tracker = Tracker::new();
        {
            let mut state = new_state(&tracker);
            let written = [1_u8, 2, 3, 4];
            assert_eq!(update_window(&mut state, &written, 4), Ok(()));
            assert_eq!(tracker.allocations(), 1);
            assert_eq!(tracker.frees(), 0);
            state.release();
        }
        // `test/infcover.c`'s `mem_done` (its L200-L234) reports a leak, a non-LIFO
        // free or a rogue free; this is the same accounting in miniature.
        assert_eq!(tracker.allocations(), 1);
        assert_eq!(tracker.frees(), 1);
        assert_eq!(tracker.total.get(), 0);
    }
}
