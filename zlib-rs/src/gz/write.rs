//! Compression engine for the buffered gzip write path (`gz*` write API).
//!
//! This module is the idiomatic-Rust port of the three private compression
//! helpers in C `gzwrite.c` — `gz_init`, `gz_comp`, and `gz_zero` — expressed as
//! methods on [`GzState`]. Together they own the write-side lifecycle: lazily
//! allocating the I/O buffers and the deflate engine, driving `deflate()` and
//! draining its output to the file, and synthesizing runs of zero bytes to
//! satisfy a forward `gzseek`.
//!
//! These helpers are the ones the `open` module's [`gzsetparams`] and the future
//! `read`/`write`/`close` modules call; see the "Teardown ownership contract" in
//! [`crate::gz::state`] for how `gz_comp(Z_FINISH)` ties into `gzclose_w`.
//!
//! # Availability
//!
//! Compiled only under the `gz-io` Cargo feature (which implies `std` +
//! `gzip`). The feature gate is applied at the `pub mod gz;` declaration site in
//! the crate root, so no inner `#![cfg(...)]` attribute is needed here.
//!
//! # Safety and ownership model
//!
//! The whole crate carries `#![forbid(unsafe_code)]`, so this module contains
//! **zero `unsafe`**. The C raw-pointer cursors are re-expressed as plain `usize`
//! offsets into the owned [`GzState::in_buf`] / [`GzState::out_buf`] buffers:
//!
//! | C `z_stream` field                | Safe-core equivalent (`GzState`)              |
//! |-----------------------------------|-----------------------------------------------|
//! | `next_in - in` / `avail_in`       | [`in_next`](GzState::in_next) / [`in_have`](GzState::in_have) |
//! | `next_out - out` / `avail_out`    | [`out_pos`](GzState::out_pos) / `size - out_pos`             |
//! | `x.next - out` (file-write mark)  | [`out_written`](GzState::out_written)         |
//!
//! Because the safe-core [`crate::stream::ZStream`] does not retain the
//! input/output pointers across calls — every `deflate` step takes a borrowed
//! slice and returns the bytes consumed/produced — this layer threads those
//! slices itself, advancing the offsets above by the returned counts.

use std::io::Write;

use crate::constants::{DEF_MEM_LEVEL, Flush, MAX_WBITS, Strategy, Z_DEFLATED};
use crate::error::{ReturnCode, ZlibError};
use crate::gz::state::GzState;

// Raw zlib status codes used by the gz error slot (`state->err`), recovered from
// the authoritative `ZlibError` `const fn` accessors so the exact integers can
// never drift from the public `Z_*` `#define`s (matches the pattern used in
// `gz/state.rs` and `gz/open.rs`).
const Z_ERRNO: i32 = ZlibError::ErrNo.as_i32();
const Z_STREAM_ERROR: i32 = ZlibError::StreamError.as_i32();
const Z_MEM_ERROR: i32 = ZlibError::MemError.as_i32();

impl GzState {
    /// Lazily allocate the I/O buffers and initialize the deflate engine for
    /// gzip compression — an exact port of the C `gz_init` (`gzwrite.c`).
    ///
    /// Allocates the input buffer at twice [`want`](GzState::want) (the extra
    /// room is for `gzprintf`), and — unless this is a transparent
    /// ([`direct`](GzState::direct) `!= 0`) stream — the output buffer at
    /// [`want`](GzState::want) and a gzip-configured deflate engine via
    /// `deflateInit2(level, Z_DEFLATED, MAX_WBITS + 16, DEF_MEM_LEVEL,
    /// strategy)`. The `+ 16` selects gzip wrapping (RFC 1952). On success the
    /// buffer-allocated sentinel [`size`](GzState::size) is set to
    /// [`want`](GzState::want) and the output cursors are reset so `deflate`
    /// writes into `out_buf[0..size]`.
    ///
    /// Returns `0` on success, or `-1` after recording `Z_MEM_ERROR` in the
    /// error slot if the deflate engine could not be initialized (the buffers
    /// are released again before returning, mirroring the C `free` on the error
    /// path). Unlike C `malloc`, the Rust [`alloc::vec!`] allocations are
    /// zero-filled, which additionally makes the [`gz_zero`](Self::gz_zero)
    /// zero-run path robust even in its first iteration.
    pub(crate) fn gz_init(&mut self) -> i32 {
        // Allocate the input buffer (double size for gzprintf). C:
        // `state->in = malloc(state->want << 1)`.
        self.in_buf = alloc::vec![0u8; self.want << 1];

        // Only need an output buffer and a deflate engine when compressing.
        if self.direct == 0 {
            // Allocate the output buffer. C: `state->out = malloc(state->want)`.
            self.out_buf = alloc::vec![0u8; self.want];

            // Allocate deflate memory, set up for gzip compression. C:
            // `deflateInit2(strm, level, Z_DEFLATED, MAX_WBITS + 16,
            //  DEF_MEM_LEVEL, strategy)`. The strategy `i32` (kept for parsing
            // fidelity) is converted to the strongly-typed `Strategy` here, at
            // the single engine entry point.
            let strategy = Strategy::try_from_i32(self.strategy).unwrap_or(Strategy::Default);
            let ret = crate::deflate::deflate_init2(
                &mut self.strm,
                self.level,
                Z_DEFLATED,
                MAX_WBITS + 16,
                DEF_MEM_LEVEL,
                strategy,
            );
            if ret.is_err() {
                // Release the buffers and report out-of-memory, as C does.
                self.in_buf = alloc::vec::Vec::new();
                self.out_buf = alloc::vec::Vec::new();
                self.set_error(Z_MEM_ERROR, Some("out of memory"));
                return -1;
            }
            // C `strm->next_in = NULL` ("no pending input yet") maps to a zero
            // input count; the cursor is meaningless while `in_have == 0`.
            self.in_have = 0;
            self.in_next = 0;
        }

        // Mark the state as initialized (buffers allocated). C:
        // `state->size = state->want`.
        self.size = self.want;

        // Initialize the write-buffer cursors if compressing. C:
        // `avail_out = size; next_out = out; x.next = next_out`, i.e. the whole
        // output buffer is free and nothing has been written to the file yet.
        if self.direct == 0 {
            self.out_pos = 0;
            self.out_written = 0;
        }
        0
    }

    /// Drive `deflate()` on the staged input, draining its output to the file —
    /// an exact port of the C `gz_comp` (`gzwrite.c`).
    ///
    /// On the first call it allocates via [`gz_init`](Self::gz_init). For a
    /// transparent ([`direct`](GzState::direct) `!= 0`) stream it writes the
    /// staged input straight through. Otherwise it runs the C `do { drain;
    /// deflate } while (produced)` loop: it flushes the compressed output buffer
    /// to the file when full, or when `flush` requests it (but, for
    /// [`Flush::Finish`], only once `deflate` has reported
    /// [`ReturnCode::StreamEnd`]), then compresses the next chunk of staged
    /// input into the free tail of the output buffer.
    ///
    /// `flush` is forwarded verbatim to `deflate`. After a [`Flush::Finish`] the
    /// [`reset`](GzState::reset) flag is set so the next write transparently
    /// begins a fresh gzip member.
    ///
    /// Returns `0` on success or `-1` on error (a write failure records
    /// `Z_ERRNO`; a corrupt deflate stream records `Z_STREAM_ERROR`). It is
    /// contracted to succeed as a no-op when there is no pending input and
    /// `flush` is [`Flush::NoFlush`], which is the property `gzsetparams` relies
    /// on to call it unconditionally.
    pub(crate) fn gz_comp(&mut self, flush: Flush) -> i32 {
        // Allocate memory if this is the first time through.
        if self.size == 0 && self.gz_init() == -1 {
            return -1;
        }

        // Write directly (transparent passthrough) if requested. The staged
        // input bytes are written to the file as-is, with no compression.
        if self.direct != 0 {
            if self.in_have > 0 {
                self.again = false;
                let start = self.in_next;
                let end = self.in_next + self.in_have;
                // `write_all` loops over partial writes internally, which on a
                // blocking `File` subsumes the C `while (avail_in)` write loop.
                if let Err(e) = self.file.write_all(&self.in_buf[start..end]) {
                    self.again = e.kind() == std::io::ErrorKind::WouldBlock;
                    let msg = e.to_string();
                    self.set_error(Z_ERRNO, Some(&msg));
                    return -1;
                }
                self.in_next += self.in_have;
                self.in_have = 0;
            }
            return 0;
        }

        // Check for a pending reset (left set after a previous Z_FINISH). Don't
        // start a new gzip member unless there is data to write and we're not
        // merely flushing.
        if self.reset {
            if self.in_have == 0 && flush == Flush::NoFlush {
                return 0;
            }
            if crate::deflate::deflate_reset(&mut self.strm).is_err() {
                self.set_error(
                    Z_STREAM_ERROR,
                    Some("internal error: deflate stream corrupt"),
                );
                return -1;
            }
            self.reset = false;
        }

        // Run deflate() on the provided input until it produces no more output.
        // `last_was_stream_end` tracks the previous iteration's return value, as
        // C tests `ret == Z_STREAM_END` at the top of each loop turn.
        let mut last_was_stream_end = false;
        loop {
            // Write out the current buffer contents if full, or if flushing —
            // but when doing Z_FINISH don't write until deflate reaches
            // Z_STREAM_END (so the gzip trailer is emitted in the same flush).
            let avail_out_zero = self.out_pos == self.size;
            let should_drain = avail_out_zero
                || (flush != Flush::NoFlush && (flush != Flush::Finish || last_was_stream_end));
            if should_drain {
                // Drain the compressed-but-unwritten region
                // `out_buf[out_written..out_pos]` to the file. C loops with
                // `write()`; `write_all` is the blocking-`File` equivalent.
                if self.out_pos > self.out_written {
                    self.again = false;
                    let start = self.out_written;
                    let end = self.out_pos;
                    if let Err(e) = self.file.write_all(&self.out_buf[start..end]) {
                        self.again = e.kind() == std::io::ErrorKind::WouldBlock;
                        let msg = e.to_string();
                        self.set_error(Z_ERRNO, Some(&msg));
                        return -1;
                    }
                    self.out_written = self.out_pos;
                }
                // Once the buffer is fully consumed, rewind both cursors so the
                // next deflate writes from the start. C: `avail_out = size;
                // next_out = out; x.next = out`.
                if avail_out_zero {
                    self.out_pos = 0;
                    self.out_written = 0;
                }
            }

            // Compress: deflate the staged input into the free output tail.
            // The three field borrows (`in_buf`, `out_buf`, `strm`) are
            // disjoint, so this is a sound safe-Rust call. The returned counts
            // are owned values, so all borrows end before the cursor updates and
            // any `set_error` call below.
            let in_start = self.in_next;
            let in_end = self.in_next + self.in_have;
            let out_start = self.out_pos;
            let out_end = self.size;
            let (result, consumed, produced) = crate::deflate::deflate(
                &mut self.strm,
                &self.in_buf[in_start..in_end],
                &mut self.out_buf[out_start..out_end],
                flush,
            );
            if matches!(result, Err(ZlibError::StreamError)) {
                self.set_error(
                    Z_STREAM_ERROR,
                    Some("internal error: deflate stream corrupt"),
                );
                return -1;
            }
            last_was_stream_end = matches!(result, Ok(ReturnCode::StreamEnd));
            self.in_next += consumed;
            self.in_have -= consumed;
            self.out_pos += produced;

            // C `have -= strm->avail_out; } while (have)` — loop while this turn
            // produced output. A zero-output turn means deflate is drained.
            if produced == 0 {
                break;
            }
        }

        // If that completed a deflate stream, allow another to start.
        if flush == Flush::Finish {
            self.reset = true;
        }
        0
    }

    /// Compress a run of [`skip`](GzState::skip) zero bytes to satisfy a pending
    /// forward `gzseek` — an exact port of the C `gz_zero` (`gzwrite.c`).
    ///
    /// First flushes any input still staged in the buffer, then repeatedly fills
    /// the input buffer with zeros and compresses it (via
    /// [`gz_comp`](Self::gz_comp) with [`Flush::NoFlush`]) until
    /// [`skip`](GzState::skip) reaches zero, advancing [`pos`](GzState::pos) by
    /// the number of zero bytes actually consumed each round.
    ///
    /// Returns `0` on success or `-1` if a `gz_comp` call fails. The input buffer
    /// is zero-filled only on the first iteration (matching C's `first` flag);
    /// because [`gz_init`](Self::gz_init) allocates the buffer zero-filled, the
    /// remaining iterations correctly see zeros without re-clearing.
    pub(crate) fn gz_zero(&mut self) -> i32 {
        // Consume whatever's left in the input buffer first.
        if self.in_have > 0 && self.gz_comp(Flush::NoFlush) == -1 {
            return -1;
        }

        // Compress `skip` zero bytes. This is a `do { } while (skip)` in C: it
        // runs at least once.
        let mut first = true;
        loop {
            // n = min(size, skip). The C `GT_OFF` guard against `size`
            // overflowing a signed offset is vacuous for the small `usize`
            // buffer sizes used here, so this collapses to a plain minimum.
            let n = if (self.size as i64) > self.skip {
                self.skip as usize
            } else {
                self.size
            };
            // Zero the input region on the first pass only (C's `first` flag).
            if first {
                for byte in &mut self.in_buf[..n] {
                    *byte = 0;
                }
                first = false;
            }
            // Stage the zeros as pending input and compress them.
            self.in_have = n;
            self.in_next = 0;
            let ret = self.gz_comp(Flush::NoFlush);
            // C `n -= strm->avail_in`: the amount actually consumed this round
            // (the remainder, if any, is still staged in `in_have`).
            let consumed = n - self.in_have;
            self.pos += consumed as i64;
            self.skip -= consumed as i64;
            if ret == -1 {
                return -1;
            }
            if self.skip == 0 {
                break;
            }
        }
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gz::state::{GzMode, GzState};
    use std::fs::{self, File, OpenOptions};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Build a unique temporary path so parallel test threads never collide.
    fn unique_temp_path(tag: &str) -> PathBuf {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        std::env::temp_dir().join(format!("zlibrs_gz_write_test_{tag}_{pid}_{n}_{nanos}.gz"))
    }

    /// RAII wrapper that deletes its backing file on drop.
    struct TempPath(PathBuf);
    impl Drop for TempPath {
        fn drop(&mut self) {
            let _ = fs::remove_file(&self.0);
        }
    }

    /// Open a fresh read+write file for a write-mode `GzState`.
    fn writable_state(path: &PathBuf) -> GzState {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .expect("create temp gz file");
        GzState::new(file, path.to_string_lossy().into_owned(), GzMode::Write)
    }

    /// Decompress a gzip stream with the safe-core inflate engine and return the
    /// recovered bytes. Used to prove `gz_init`/`gz_comp` emit a valid gzip
    /// stream.
    fn gunzip(compressed: &[u8]) -> alloc::vec::Vec<u8> {
        use crate::inflate::InflateState;
        // windowBits 15 + 16 = 31 selects gzip decoding.
        let mut st = InflateState::new(15 + 16).expect("init gzip inflate");
        let mut out = alloc::vec![0u8; 1 << 16];
        let r = st.inflate(compressed, &mut out, Flush::Finish);
        assert_eq!(
            r.status,
            Ok(ReturnCode::StreamEnd),
            "inflate did not finish"
        );
        out.truncate(r.produced);
        out
    }

    #[test]
    fn gz_init_allocates_and_sets_size() {
        let tp = TempPath(unique_temp_path("init"));
        let mut state = writable_state(&tp.0);
        state.level = 6;
        assert_eq!(state.size, 0, "buffers start unallocated");
        assert_eq!(state.gz_init(), 0);
        assert_eq!(state.size, state.want, "size marks buffers allocated");
        assert_eq!(state.in_buf.len(), state.want << 1);
        assert_eq!(state.out_buf.len(), state.want);
        assert_eq!(state.out_pos, 0);
        assert_eq!(state.out_written, 0);
    }

    #[test]
    fn gz_comp_finish_writes_valid_gzip() {
        let tp = TempPath(unique_temp_path("finish"));
        let payload: alloc::vec::Vec<u8> = b"hello, gzip world! "
            .iter()
            .cycle()
            .take(5000)
            .copied()
            .collect();
        {
            let mut state = writable_state(&tp.0);
            state.level = 6;
            // First gz_comp allocates the buffers (no input staged yet).
            assert_eq!(state.gz_comp(Flush::NoFlush), 0);
            // Stage the payload and finish the stream.
            state.in_buf[..payload.len()].copy_from_slice(&payload);
            state.in_have = payload.len();
            state.in_next = 0;
            assert_eq!(state.gz_comp(Flush::Finish), 0);
            assert!(
                state.reset,
                "Finish sets the reset flag for the next member"
            );
            assert_eq!(state.in_have, 0, "all input consumed");
        }
        // Read the file back and decompress it.
        let bytes = fs::read(&tp.0).expect("read back compressed file");
        assert!(!bytes.is_empty(), "gzip stream was written");
        assert_eq!(gunzip(&bytes), payload, "round-trip is byte-identical");
    }

    #[test]
    fn gz_zero_emits_compressed_zero_run() {
        let tp = TempPath(unique_temp_path("zero"));
        let skip = 20_000usize; // spans several buffer fills (GZBUFSIZE = 8192)
        {
            let mut state = writable_state(&tp.0);
            state.level = 6;
            assert_eq!(state.gz_comp(Flush::NoFlush), 0); // allocate buffers
            state.skip = skip as i64;
            assert_eq!(state.gz_zero(), 0);
            assert_eq!(state.skip, 0, "all skip bytes consumed");
            assert_eq!(state.pos, skip as i64, "pos advanced by the zero run");
            assert_eq!(state.gz_comp(Flush::Finish), 0);
        }
        let bytes = fs::read(&tp.0).expect("read back compressed file");
        let out = gunzip(&bytes);
        assert_eq!(out.len(), skip, "decompresses to exactly `skip` bytes");
        assert!(
            out.iter().all(|&b| b == 0),
            "all decompressed bytes are zero"
        );
    }

    #[test]
    fn gzsetparams_midstream_level_change_and_skip_round_trips() {
        // End-to-end integration of `gzsetparams` (open.rs) with the write-layer
        // helpers it depends on (finding #10): the `size != 0` branch drives
        // `gz_comp(Flush::Block)` + `deflate_params`, and a pending `skip` drives
        // `gz_zero`. We compress chunk1 at level 6, then `gzsetparams` to level 1
        // with a pending skip (inserting a zero run), then compress chunk2 at
        // level 1, finish, and verify the stream decompresses to
        // `chunk1 + zeros + chunk2`.
        use crate::gz::open::gzsetparams;

        let tp = TempPath(unique_temp_path("setparams"));
        let chunk1: alloc::vec::Vec<u8> = b"first chunk at level six "
            .iter()
            .cycle()
            .take(3000)
            .copied()
            .collect();
        let chunk2: alloc::vec::Vec<u8> = b"second chunk at level one "
            .iter()
            .cycle()
            .take(3000)
            .copied()
            .collect();
        let skip = 4096usize; // forces multiple gz_zero buffer fills

        {
            let mut state = writable_state(&tp.0);
            state.level = 6;

            // Allocate buffers and compress chunk1 at the initial level.
            assert_eq!(state.gz_comp(Flush::NoFlush), 0);
            state.in_buf[..chunk1.len()].copy_from_slice(&chunk1);
            state.in_have = chunk1.len();
            state.in_next = 0;
            assert_eq!(state.gz_comp(Flush::NoFlush), 0);

            // Stage a pending forward seek so gzsetparams runs gz_zero, then
            // change the level (level 6 -> 1) so it also runs
            // gz_comp(Block) + deflate_params.
            state.skip = skip as i64;
            assert_eq!(
                gzsetparams(&mut state, 1, Strategy::Default.as_i32()),
                ReturnCode::Ok.as_i32(),
            );
            assert_eq!(state.level, 1, "level updated");
            assert_eq!(state.skip, 0, "pending skip consumed by gz_zero");

            // Compress chunk2 at the new level and finish the stream.
            state.in_buf[..chunk2.len()].copy_from_slice(&chunk2);
            state.in_have = chunk2.len();
            state.in_next = 0;
            assert_eq!(state.gz_comp(Flush::Finish), 0);
        }

        let bytes = fs::read(&tp.0).expect("read back compressed file");
        let mut expected = chunk1.clone();
        expected.extend(core::iter::repeat_n(0u8, skip));
        expected.extend_from_slice(&chunk2);
        assert_eq!(
            gunzip(&bytes),
            expected,
            "chunk1 + zero-run + chunk2 round-trips byte-identically across a level change",
        );
    }

    #[test]
    fn gz_comp_noflush_without_input_is_noop() {
        // The property gzsetparams relies on: gz_comp(NoFlush) with nothing
        // staged returns 0 and writes nothing.
        let tp = TempPath(unique_temp_path("noop"));
        let mut state = writable_state(&tp.0);
        state.level = 6;
        assert_eq!(state.gz_comp(Flush::NoFlush), 0); // allocates, writes nothing
        assert_eq!(state.gz_comp(Flush::NoFlush), 0); // genuine no-op
        let _ = File::open(&tp.0); // ensure path exists / no panic
    }
}
